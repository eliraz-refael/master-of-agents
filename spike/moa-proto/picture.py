#!/usr/bin/env python3
"""THROWAWAY PROTOTYPE: the picture pane. A live view of brain.db plus aoe-style
session control over the prototype's tmux server (-L moa). Not product code."""
import curses
import datetime
import locale
import os
import shlex
import sqlite3
import subprocess
import textwrap
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
DB = os.environ.get("MOA_DB", os.path.normpath(os.path.join(HERE, "..", "master-chat", "brain.db")))
SOCK = os.environ.get("MOA_TMUX", "moa")
VIEWS = ["Plan", "Today", "Projects", "Sessions", "Activity"]
ICON = {"working": "◐", "idle": "●", "waiting": "⚠", "ended": "○", "elsewhere": "◌", "archived": "·"}
SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def icon(state):
    t = time.monotonic()
    if state == "working":
        return SPIN[int(t * 10) % len(SPIN)]
    if state == "waiting":
        return "⚠" if int(t * 2) % 2 else "!"
    return ICON.get(state, "·")
LIVE = ("working", "idle", "waiting")
STRIP = 7
PANEL = None  # set by --panel: show one view only (a cockpit panel), no tabs or activity strip
PANEL_VIEW = {"tree": "Sessions", "today": "Today", "activity": "Activity", "projects": "Projects", "plan": "Plan"}

SETUP = """
CREATE TABLE IF NOT EXISTS groups (path TEXT PRIMARY KEY, created_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY, ts TEXT NOT NULL DEFAULT (datetime('now','localtime')), who TEXT, text TEXT);
CREATE TRIGGER IF NOT EXISTS a_proj_ins AFTER INSERT ON projects BEGIN
  INSERT INTO activity(who,text) VALUES('brain','new project ' || NEW.name || ' (' || NEW.status || ')'); END;
CREATE TRIGGER IF NOT EXISTS a_proj_status AFTER UPDATE OF status ON projects WHEN OLD.status IS NOT NEW.status BEGIN
  INSERT INTO activity(who,text) VALUES('brain','project ' || NEW.name || ': ' || OLD.status || ' → ' || NEW.status); END;
CREATE TRIGGER IF NOT EXISTS a_task_ins AFTER INSERT ON tasks BEGIN
  INSERT INTO activity(who,text) VALUES('brain','new task "' || NEW.title || '"' ||
    coalesce(' in ' || (SELECT name FROM projects WHERE id=NEW.project_id), '')); END;
CREATE TRIGGER IF NOT EXISTS a_task_status AFTER UPDATE OF status ON tasks WHEN OLD.status IS NOT NEW.status BEGIN
  INSERT INTO activity(who,text) VALUES('brain','task "' || NEW.title || '": ' || OLD.status || ' → ' || NEW.status); END;
CREATE TRIGGER IF NOT EXISTS a_task_today AFTER UPDATE OF today ON tasks
  WHEN NEW.today IS NOT NULL AND OLD.today IS NOT NEW.today BEGIN
  INSERT INTO activity(who,text) VALUES('brain','added to today: "' || NEW.title || '"'); END;
CREATE TRIGGER IF NOT EXISTS a_note_ins AFTER INSERT ON notes BEGIN
  INSERT INTO activity(who,text) VALUES('brain','noted (' || NEW.kind || '): ' || substr(NEW.text,1,100)); END;
CREATE TRIGGER IF NOT EXISTS a_sugg_ins AFTER INSERT ON suggestions BEGIN
  INSERT INTO activity(who,text) VALUES('brain','suggests #' || NEW.id || ': ' || NEW.text); END;
CREATE TRIGGER IF NOT EXISTS a_sess_link AFTER UPDATE OF project_id ON sessions
  WHEN NEW.project_id IS NOT NULL AND OLD.project_id IS NOT NEW.project_id BEGIN
  INSERT INTO activity(who,text) VALUES('brain','linked session ' || substr(NEW.session_id,1,8) || ' → ' ||
    (SELECT name FROM projects WHERE id=NEW.project_id)); END;
CREATE TRIGGER IF NOT EXISTS a_event AFTER INSERT ON events WHEN NEW.kind != 'start' BEGIN
  INSERT INTO activity(who,text) VALUES(CASE NEW.kind WHEN 'ui' THEN 'you' ELSE 'session' END,
    CASE NEW.kind WHEN 'ui' THEN NEW.text ELSE
      replace(NEW.cwd, rtrim(NEW.cwd, replace(NEW.cwd,'/','')), '') || ' ' ||
      CASE NEW.kind WHEN 'prompt' THEN '› ' || substr(replace(NEW.text, char(10), ' '),1,90)
                    WHEN 'stop' THEN '✓ turn done' WHEN 'waiting' THEN '⚠ needs you'
                    WHEN 'end' THEN '○ ended' ELSE NEW.kind END END); END;
"""


def connect():
    conn = sqlite3.connect(DB, timeout=2)
    conn.row_factory = sqlite3.Row
    for table, col in (("sessions", "pane"), ("sessions", "tmux"), ("sessions", "grp"),
                       ("tasks", "who"), ("tasks", "due")):  # tasks.who: you | agent | other; tasks.due: YYYY-MM-DD
        try:
            conn.execute(f"ALTER TABLE {table} ADD COLUMN {col} TEXT")
        except sqlite3.OperationalError:
            pass
    conn.executescript(SETUP)
    conn.commit()
    return conn


def tmux(*args):
    return subprocess.run(["tmux", "-L", SOCK, *args], capture_output=True, text=True)


def db_mtime():
    m = 0
    for suffix in ("", "-wal"):
        try:
            m = max(m, os.stat(DB + suffix).st_mtime_ns)
        except FileNotFoundError:
            pass
    return m


def fresh(ts, seconds=4):
    try:
        return (datetime.datetime.now() - datetime.datetime.fromisoformat(ts)).total_seconds() < seconds
    except (TypeError, ValueError):
        return False


def ago(ts):
    if not ts:
        return ""
    try:
        s = (datetime.datetime.now() - datetime.datetime.fromisoformat(ts)).total_seconds()
    except ValueError:
        return ts
    if s < 60:
        return "now"
    if s < 3600:
        return f"{int(s // 60)}m"
    if s < 86400:
        return f"{int(s // 3600)}h"
    return f"{int(s // 86400)}d"


class Row:
    def __init__(self, text, attr=0, kind=None, data=None, right=None):
        self.text, self.attr, self.kind, self.data = text, attr, kind, data
        self.right = right  # (text, attr) drawn against the right edge, like a task's due date


def short_path(path):
    path = (path or "").replace(os.path.expanduser("~"), "~")
    parts = path.split("/")
    return "/".join(parts[-2:]) if len(parts) > 3 else path


def tree_line(s, width):
    title = s["title"] or (s["last_prompt"] or "").replace("\n", " ")[:60] or s["session_id"][:8]
    acct = (s["account"] or "").replace("claude-", "")
    # Narrow panels drop the path first, then the account, so titles stay readable.
    cols = [(acct, 6)] if width >= 34 else []
    if width >= 76:
        cols.append((short_path(s["cwd"]), 23))
    tw = max(8, width - 7 - sum(n for _, n in cols))
    extra = "".join(f"{text[:n - 1]:<{n}}" for text, n in cols)
    return f"{icon(s['state'])} {title[:tw]:<{tw}} {extra}{ago(s['last_seen']):>4}"


def session_tree(sessions, groups, collapsed, width, C):
    root = {"children": {}, "sessions": []}
    for x in sessions:
        node = root
        for part in (x["grp"] or x["project"] or "Loose").split("/"):
            node = node["children"].setdefault(part, {"children": {}, "sessions": []})
        node["sessions"].append(x)
    for g in groups:
        node = root
        for part in g.split("/"):
            node = node["children"].setdefault(part, {"children": {}, "sessions": []})

    def count(node):
        ss = node["sessions"] + [y for c in node["children"].values() for y in count(c)]
        return ss

    rows = []

    def walk(node, prefix, key, depth):
        items = [("g", n, c) for n, c in node["children"].items()] + [("s", None, x) for x in node["sessions"]]
        for i, (kind, name, item) in enumerate(items):
            last = i == len(items) - 1
            branch, cont = ("", "") if depth == 0 else (("└─ ", "   ") if last else ("├─ ", "│  "))
            if kind == "g":
                gkey = f"{key}/{name}"
                members = count(item)
                live = sum(m["state"] in LIVE for m in members)
                busy = any(m["state"] == "working" for m in members)
                arrow = "▸" if gkey in collapsed else "▾"
                label = f"{arrow} {name}  {len(members)}" + (f" · {live} live" if live else "") + (f" {icon('working')}" if busy else "")
                rows.append(Row(prefix + branch + label, C["head"] if depth == 0 else C["group"], "group", gkey))
                if gkey not in collapsed:
                    walk(item, prefix + cont, gkey, depth + 1)
            else:
                st = item["state"]
                attr = C["warn"] | curses.A_BOLD if st == "waiting" else (C["ok"] if st in LIVE else C["dim"])
                rows.append(Row(prefix + branch + tree_line(item, width - len(prefix) - len(branch)), attr, "session", item))

    walk(root, "", "", 0)
    return rows


def session_line(s, indent=""):
    proj = s["title"] or s["project"] or os.path.basename(s["cwd"] or "?")
    prompt = (s["last_prompt"] or "").replace("\n", " ")
    if prompt.startswith("<"):
        prompt = ""
    acct = (s["account"] or "").replace("claude-", "")
    return f"{indent}{icon(s['state'])} {proj} [{acct}] {ago(s['last_seen']):>3}  {prompt}"


SESSIONS_SQL = """SELECT s.*, p.name AS project FROM sessions s LEFT JOIN projects p ON p.id=s.project_id"""


def days_left(day):
    """Days from today to a YYYY-MM-DD due date (negative = overdue), None if it isn't one."""
    try:
        return (datetime.date.fromisoformat(day) - datetime.date.today()).days
    except (TypeError, ValueError):
        return None


def when(day):
    """A due date in words: today, tomorrow, Fri, 3 Dec; overdue ones say so."""
    delta = days_left(day)
    if delta is None:
        return day or ""
    d = datetime.date.today() + datetime.timedelta(days=delta)
    if delta < 0:
        return f"{-delta}d overdue"
    return "today" if delta == 0 else "tomorrow" if delta == 1 else f"{d:%a}" if delta < 7 else f"{d:%-d %b}"


def plan(conn, width, C):
    """The big picture: each project's open tasks and whose move each one is, the projects
    that need you first. moa keeps the tasks (moactl task …) from what sessions report."""
    today = datetime.date.today().isoformat()
    done = conn.execute("SELECT count(*) FROM tasks WHERE status='done' AND date(updated_at)=?", (today,)).fetchone()[0]
    head = f"{datetime.date.today():%a %d %b}" + (f" · ✓ {done} done today" if done else "")
    rows = [Row(head, C["head"])]
    tasks = conn.execute("""SELECT t.*, p.name AS project, s.state AS sstate, s.title AS stitle FROM tasks t
        LEFT JOIN projects p ON p.id=t.project_id LEFT JOIN sessions s ON s.session_id=t.session_id
        WHERE t.status NOT IN ('done','dropped','idea')""").fetchall()
    waiting = conn.execute(SESSIONS_SQL + " WHERE s.state='waiting'").fetchall()
    by = {}
    for t in tasks:
        by.setdefault(t["project"] or "-", {"tasks": [], "waiting": []})["tasks"].append(t)
    for w in waiting:
        by.setdefault(w["project"] or "-", {"tasks": [], "waiting": []})["waiting"].append(w)

    def kind(t):
        if t["status"] in ("waiting", "blocked") or t["who"] == "other":
            return "wait"
        if t["who"] == "agent" or t["status"] == "active":
            return "agent"
        return "you"

    def rank(name):
        g = by[name]
        mine = [t for t in g["tasks"] if kind(t) == "you"]
        due = min((t["due"] for t in g["tasks"] if t["due"]), default="9999")
        return (not g["waiting"], not mine, due, name)

    def due_chip(day):
        """A task's due date as its own column: overdue and today stand out, the rest fade with distance."""
        words = when(day)
        if not words:
            return None
        delta = days_left(day)
        delta = 99 if delta is None else delta
        attr = (C["late"] if delta < 0 else C["today"] if delta == 0 else
                C["soon"] if delta < 7 else C["dim"])
        return (f" {words} " if delta <= 0 else words, attr)

    def item(mark, title, tail, attr, kind=None, data=None, due=None):
        """A task wraps under its mark; only its first row is selectable. Its due date sits on the right."""
        chip = due_chip(due)
        if chip and width - 6 - len(chip[0]) < 16:  # too narrow for a date column: the date goes in the text
            attr = C["warn"] if chip[0].startswith(" ") else attr  # overdue or today still stands out
            tail, chip = ", ".join(x for x in (tail, chip[0].strip()) if x), None
        text = f"{title}  ({tail})" if tail else title
        lines = textwrap.wrap(text, max(10, width - 5 - (len(chip[0]) + 1 if chip else 0))) or [""]
        rows.append(Row(f"  {mark} {lines[0]}", attr, kind, data, chip))
        rows.extend(Row(f"    {ln}", attr) for ln in lines[1:])

    if not by:
        rows.append(Row("  nothing tracked yet: moa fills this in as sessions report", C["dim"]))
    for name in sorted(by, key=rank):
        g = by[name]
        n_you = sum(kind(t) == "you" for t in g["tasks"]) + len(g["waiting"])
        n_agent = sum(kind(t) == "agent" for t in g["tasks"])
        n_wait = sum(kind(t) == "wait" for t in g["tasks"])
        counts = " · ".join(x for x in (n_you and f"{n_you} on you", n_agent and f"{n_agent} with agents",
                                        n_wait and f"{n_wait} waiting") if x)
        title = f"{name} "
        rows.append(Row(""))
        rows.append(Row(f"{title}{'─' * max(2, width - len(title) - len(counts) - 3)} {counts}",
                        C["group"] if n_you else C["head"]))
        for w in g["waiting"]:
            item(icon("waiting"), f"{w['title'] or w['project']}: the session needs you", "", C["warn"] | curses.A_BOLD, "session", w)
        for t in sorted(g["tasks"], key=lambda t: ({"you": 0, "agent": 1, "wait": 2}[kind(t)], t["due"] or "9999", t["id"])):
            k = kind(t)
            if k == "wait":
                on = t["blocked_on"] or ("someone else" if t["who"] == "other" else "")
                item("◷", t["title"], on, C["dim"], "task", t, t["due"])
            elif k == "agent":
                mark = icon("working") if t["sstate"] == "working" else "◐"
                tail = "agent working" if t["sstate"] == "working" else "agent"
                item(mark, t["title"], tail, C["ok"], "task", t, t["due"])
            else:
                item("▸", t["title"], "", 0, "task", t, t["due"])
    quiet = conn.execute("""SELECT name FROM projects p WHERE last_activity >= date('now','localtime','-7 days')
        AND NOT EXISTS (SELECT 1 FROM tasks t WHERE t.project_id=p.id AND t.status NOT IN ('done','dropped','idea'))
        ORDER BY last_activity DESC""").fetchall()
    quiet = [q["name"] for q in quiet if q["name"] not in by]
    if quiet:
        rows.append(Row(""))
        rows.extend(Row(ln, C["dim"]) for ln in textwrap.wrap("no open tasks: " + ", ".join(quiet), max(10, width - 1)))
    rows.append(Row(""))
    rows.append(Row("↵ jump to its session · d done · x drop", C["dim"]))
    return rows


def build(conn, view, expanded, collapsed, width, C):
    rows = []
    today = datetime.date.today().isoformat()
    if view == "Plan":
        return plan(conn, width, C)
    if view == "Today":
        rows.append(Row(f"TODAY · {datetime.date.today():%a %d %b}", C["head"]))
        tasks = conn.execute("""SELECT t.*, p.name AS project FROM tasks t LEFT JOIN projects p ON p.id=t.project_id
                                WHERE t.today=? ORDER BY t.status='done', t.id""", (today,)).fetchall()
        if not tasks:
            rows.append(Row("  (nothing yet: set today's goals with moa)", C["dim"]))
        for t in tasks:
            mark = "✓" if t["status"] == "done" else "▸"
            rows.append(Row(f"{mark} {t['project'] or '-'}: {t['title']}  [{t['status']}]",
                            C["dim"] if t["status"] == "done" else 0, "task", t))
        need = conn.execute(SESSIONS_SQL + """ WHERE s.state IN ('waiting','idle')
                              AND s.last_seen >= datetime('now','localtime','-1 day') ORDER BY s.last_seen DESC""").fetchall()
        rows.append(Row(""))
        rows.append(Row("WAITING ON YOU", C["head"]))
        if not need:
            rows.append(Row("  (nothing)", C["dim"]))
        for s in need:
            rows.append(Row(session_line(s), C["warn"] if s["state"] == "waiting" else 0, "session", s))
        working = conn.execute(SESSIONS_SQL + " WHERE s.state='working' ORDER BY s.last_seen DESC").fetchall()
        if working:
            rows.append(Row(""))
            rows.append(Row("WORKING", C["head"]))
            for s in working:
                rows.append(Row(session_line(s), C["ok"], "session", s))
        sugg = conn.execute("SELECT * FROM suggestions WHERE status IN ('open','later') ORDER BY id").fetchall()
        rows.append(Row(""))
        rows.append(Row("SUGGESTIONS  (y accept · n reject)", C["head"]))
        if not sugg:
            rows.append(Row("  (none)", C["dim"]))
        for s in sugg:
            rows.append(Row(f"? #{s['id']} {s['text']}", C["brain"], "suggestion", s))
    elif view == "Projects":
        projects = conn.execute("""SELECT p.*, CAST(julianday('now','localtime') - julianday(p.last_activity) AS INT) AS idle,
              (SELECT count(*) FROM tasks t WHERE t.project_id=p.id AND t.status NOT IN ('done','dropped')) AS open_tasks,
              (SELECT count(*) FROM sessions s WHERE s.project_id=p.id AND s.state IN ('working','waiting','idle')) AS live
              FROM projects p ORDER BY p.status='idea', p.last_activity DESC""").fetchall()
        for p in projects:
            arrow = "▾" if p["id"] in expanded else "▸"
            extra = []
            if p["live"]:
                extra.append(f"{p['live']} live")
            if p["open_tasks"]:
                extra.append(f"{p['open_tasks']} tasks")
            idle = "" if p["idle"] is None else f"{p['idle']}d"
            attr = C["brain"] if p["status"] == "idea" else (C["dim"] if (p["idle"] or 0) > 60 else 0)
            rows.append(Row(f"{arrow} {p['name']}  {p['status']} {idle}  {' · '.join(extra)}", attr, "project", p))
            if p["id"] in expanded:
                if p["summary"]:
                    rows.append(Row(f"    {p['summary']}", C["dim"]))
                for t in conn.execute("SELECT * FROM tasks WHERE project_id=? AND status NOT IN ('done','dropped') ORDER BY id",
                                      (p["id"],)):
                    blocked = f" ⧗ {t['blocked_on']}" if t["blocked_on"] else ""
                    rows.append(Row(f"    ▸ {t['title']}  [{t['status']}]{blocked}", 0, "task", t))
                for s in conn.execute(SESSIONS_SQL + " WHERE s.project_id=? ORDER BY s.last_seen DESC LIMIT 6", (p["id"],)):
                    rows.append(Row(session_line(s, "    "), C["dim"] if s["state"] == "ended" else 0, "session", s))
    elif view == "Sessions":
        sessions = conn.execute(SESSIONS_SQL + " WHERE coalesce(s.state,'') != 'archived' ORDER BY s.last_seen DESC").fetchall()
        nlive = sum(x["state"] in LIVE for x in sessions)
        nwork = sum(x["state"] == "working" for x in sessions)
        nwait = sum(x["state"] == "waiting" for x in sessions)
        head = f"{len(sessions)} sessions · {nlive} live"
        if nwork:
            head += f" · {icon('working')} {nwork} working"
        if nwait:
            head += f" · {nwait} waiting on you"
        rows.append(Row(head, C["dim"]))
        groups = [r[0] for r in conn.execute("SELECT path FROM groups ORDER BY path")]
        rows.extend(session_tree(sessions, groups, collapsed, width, C))
    else:
        for a in conn.execute("SELECT * FROM activity ORDER BY id DESC LIMIT 300"):
            rows.append(Row(f"{a['ts'][11:16]} {a['text']}", C[a["who"]] if a["who"] in C else 0))
    return rows


def account_cmd(account, resume):
    if account == "codex":
        return f"codex resume {resume}"
    if (account or "").startswith("/"):  # adopted under a CLAUDE_CONFIG_DIR moa has no name for
        return f"CLAUDE_CONFIG_DIR={shlex.quote(account)} claude --resume {resume}"
    return f"{account if account in ('claude', 'claude-lead', 'claude-neeo', 'claude-work') else 'claude-lead'} --resume {resume}"


def log_ui(conn, text):
    conn.execute("INSERT INTO events(kind,text) VALUES('ui',?)", (text,))
    conn.commit()


_pushed = None


def push_status(conn):
    """Show live counts in the tmux status line (@moa_status) and a per-window state
    icon (@moa_state). Launches one tmux process, only when something changed."""
    global _pushed
    live = conn.execute("SELECT state, pane FROM sessions WHERE state IN ('waiting','working','idle')").fetchall()
    n = {s: sum(r["state"] == s for r in live) for s in ("waiting", "working", "idle")}
    text = " · ".join(f"{n[s]} {w}" for s, w in (("waiting", "waiting on you"), ("working", "working"), ("idle", "idle")) if n[s])
    panes = {r["pane"]: r["state"] for r in live if r["pane"]}
    if _pushed == (text, panes):
        return
    args = ["set", "-g", "@moa_status", text]
    for pane, state in panes.items():
        args += [";", "set", "-q", "-w", "-t", pane, "@moa_state", state]
    for pane in (_pushed[1] if _pushed else {}).keys() - panes.keys():
        args += [";", "set", "-q", "-wu", "-t", pane, "@moa_state"]
    tmux(*args)
    _pushed = (text, panes)


def pane_alive(pane):
    return bool(pane) and tmux("display-message", "-p", "-t", pane, "#{pane_id}").returncode == 0


def act_session(conn, s):
    if pane_alive(s["pane"]):
        tmux("select-window", "-t", s["pane"])
        tmux("select-pane", "-t", s["pane"])
        return f"jumped to {s['project'] or s['session_id'][:8]}"
    if s["state"] == "elsewhere":
        return "running in herdr right now; close it there first"
    cwd = s["cwd"] if s["cwd"] and os.path.isdir(s["cwd"]) else None
    if not cwd:
        return f"can't resume: folder {s['cwd']} is gone"
    name = (s["title"] or s["project"] or os.path.basename(cwd))[:24]
    tmux("new-window", "-n", name, "-c", cwd,
         f"env MOA_TRACK=1 {account_cmd(s['account'], s['session_id'])}")
    log_ui(conn, f"resumed session {s['session_id'][:8]} in {name}")
    return f"resuming {s['session_id'][:8]} in a new window"


def new_session(conn, p, account):
    path = p["path"]
    if not path or not os.path.isdir(path):
        return f"no folder for {p['name']} ({path})"
    tmux("new-window", "-n", p["name"][:20], "-c", path, f"env MOA_TRACK=1 {account}")
    log_ui(conn, f"started a {account} session in {p['name']}")
    return f"started {account} in {p['name']}"


MOACTL = os.path.join(HERE, "moactl")


def ctl(*args):
    """Run a brain API command; return its last line of output (or error) for the footer."""
    r = subprocess.run([MOACTL, *args], capture_output=True, text=True)
    out = (r.stdout.strip() or r.stderr.strip())
    return out.splitlines()[-1] if out else "ok"


def ask(scr, label, default=""):
    """A one-line editor in the footer. Enter accepts, Esc cancels (returns None)."""
    curses.curs_set(1)
    scr.timeout(-1)
    buf = list(default)
    try:
        while True:
            h, w = scr.getmaxyx()
            y, x, n = (h - 2, 2, w - 4) if PANEL else (h - 1, 0, w - 1)  # the footer's cells (inside the frame)
            line = f"{label} {''.join(buf)}"
            scr.addnstr(y, x, " " * n, n)
            scr.addnstr(y, x, line[-(n - 1):], n - 1, curses.A_BOLD)
            scr.refresh()
            ch = scr.get_wch()
            if ch in ("\n", "\r", curses.KEY_ENTER):
                return "".join(buf).strip()
            if ch == "\x1b":
                return None
            if ch in (curses.KEY_BACKSPACE, "\x7f", "\b"):
                buf = buf[:-1]
            elif ch == "\x15":
                buf = []
            elif ch == "\x17":
                text = "".join(buf).rstrip()
                buf = list(text[:text.rfind(" ") + 1] if " " in text else "")
            elif isinstance(ch, str) and ch.isprintable():
                buf.append(ch)
    finally:
        curses.curs_set(0)


def tree_key(scr, conn, k, cur):
    """Session tree editing. Returns a footer message, or None if the key isn't ours."""
    group = cur.data[1:] if cur.kind == "group" else (cur.data["grp"] or "")
    if k == ord("n"):
        if cur.kind == "group":
            recent = conn.execute("SELECT cwd, account FROM sessions WHERE grp=? OR grp LIKE ? || '/%' ORDER BY last_seen DESC LIMIT 1",
                                  (group, group)).fetchone()
            path0, acct0 = (recent["cwd"], recent["account"]) if recent else ("~/private/", "claude-lead")
        else:
            path0, acct0 = cur.data["cwd"], cur.data["account"]
        path = ask(scr, f"new session in {group or 'top level'} · path:", (path0 or "").replace(os.path.expanduser("~"), "~"))
        if not path:
            return "cancelled"
        title = ask(scr, "title:")
        if title is None:
            return "cancelled"
        acct = ask(scr, "account (claude-lead / claude-neeo / claude-work / codex):",
                   acct0 if acct0 in ("claude-lead", "claude-neeo", "claude-work", "codex") else "claude-lead")
        if not acct:
            return "cancelled"
        args = ["open", path, "--account", acct] + (["--title", title] if title else []) + (["--group", group] if group else [])
        return ctl(*args)
    if k == ord("+"):
        name = ask(scr, "new group:", group + "/" if group else "Private/")
        return ctl("group", "add", name.strip("/")) if name else "cancelled"
    if k == ord("m") and cur.kind == "session":
        to = ask(scr, f"move {cur.data['title'] or cur.data['session_id'][:8]} to group:", group)
        return ctl("set", cur.data["session_id"], "--group", to.strip("/")) if to else "cancelled"
    if k == ord("r"):
        if cur.kind == "session":
            t = ask(scr, "rename session:", cur.data["title"] or "")
            return ctl("set", cur.data["session_id"], "--title", t) if t else "cancelled"
        parent, _, leaf = group.rpartition("/")
        new = ask(scr, "rename group:", leaf)
        if not new or new == leaf:
            return "cancelled"
        return ctl("group", "rename", group, f"{parent}/{new}" if parent else new)
    if k == ord("D") and cur.kind == "group":
        return ctl("group", "rm", group)
    return None


def main(scr):
    import chrome  # the shared palette and frame (imported here: moactl imports this module without curses)
    from layout import LABELS
    curses.curs_set(0)
    curses.use_default_colors()
    c = chrome.color
    C = {"head": c("blue") | curses.A_BOLD, "dim": c("dim"), "warn": c("yellow"), "ok": c("green"),
         "brain": c("magenta"), "session": c("dim"), "you": c("yellow"), "group": c("blue") | curses.A_BOLD,
         "late": c("dark", "red") | curses.A_BOLD, "today": c("dark", "yellow") | curses.A_BOLD, "soon": c("blue")}
    selected = c("bright", "sel")
    if PANEL:
        chrome.use_palette(scr)
    conn = connect()
    view, sel, top, expanded, collapsed, msg, confirm = 0, 0, 0, set(), set(), "", None
    if PANEL:
        view = VIEWS.index(PANEL_VIEW[PANEL])
    seen, rows, strip = -1, [], []
    me = os.path.abspath(__file__)
    code = os.stat(me).st_mtime_ns
    animate = False
    while True:
        if os.stat(me).st_mtime_ns != code:
            curses.endwin()
            os.execv(sys.executable, [sys.executable, me] + sys.argv[1:])
        scr.timeout(100 if animate else 300)
        m = db_mtime()
        h, w = scr.getmaxyx()
        # a cockpit panel draws a rounded frame and keeps a column of padding inside it
        oy, ox, ih, iw = (1, 2, max(1, h - 2), max(2, w - 3)) if PANEL else (0, 0, h, w)
        if m != seen or animate:
            seen = m
            rows = build(conn, VIEWS[view], expanded, collapsed, max(10, iw), C)
            strip = conn.execute("SELECT * FROM activity ORDER BY id DESC LIMIT ?", (STRIP,)).fetchall()
            animate = conn.execute("SELECT 1 FROM sessions WHERE state IN ('working','waiting') LIMIT 1").fetchone() is not None \
                or any(fresh(a["ts"]) for a in strip)
            push_status(conn)
        selectable = [i for i, r in enumerate(rows) if r.kind]
        if selectable and sel not in selectable:
            sel = min(selectable, key=lambda i: abs(i - sel))
        y_top = 0 if PANEL else 1
        body = max(1, ih - 2) if PANEL else max(1, h - STRIP - 3)
        if sel < top:
            top = sel
        elif sel >= top + body:
            top = sel - body + 1
        scr.erase()
        if PANEL:
            chrome.frame(scr, LABELS.get(PANEL, PANEL))
        if not PANEL:
            tabs = "  ".join((f"[{n}]" if i == view else f" {n} ") for i, n in enumerate(VIEWS))
            scr.addnstr(0, 0, tabs, w - 1, C["head"])
            brand = f"moa {icon('working') if animate else '●'}"
            if w > len(tabs) + len(brand) + 2:
                scr.addnstr(0, w - len(brand) - 1, brand, len(brand), C["brain"] | curses.A_BOLD)
        for y, r in enumerate(rows[top:top + body], start=y_top):
            on = top + y - y_top == sel and r.kind
            attr = (selected | (r.attr & curses.A_BOLD)) if on else r.attr
            text = r.text.ljust(iw - 1) if on else r.text  # the selection is a full-width bar
            scr.addnstr(oy + y, ox, text, iw - 1, attr)
            if r.right and iw - 1 - len(r.right[0]) > len(r.text.rstrip()):
                chip, cattr = r.right  # on the selection bar, a plain date takes the bar's colour; a padded (late/today) chip keeps its own
                cattr = selected if on and not chip.startswith(" ") else cattr
                scr.addnstr(oy + y, ox + iw - 1 - len(chip), chip, len(chip), cattr)
        y0 = h - STRIP - 2
        if not PANEL:
            scr.hline(y0, 0, curses.ACS_HLINE, w - 1)
            scr.addnstr(y0, 1, " activity ", w - 2, C["dim"])
        for i, a in enumerate([] if PANEL else reversed(strip)):
            new = fresh(a["ts"])
            line = f"{a['ts'][11:16]} {'✦' if new else ' '}{a['text']}"
            scr.addnstr(y0 + 1 + i, 0, line, w - 1, C.get(a["who"], 0) | (curses.A_BOLD | curses.A_STANDOUT if new and int(time.monotonic() * 4) % 2 else curses.A_BOLD if new else 0))
        keys = ("⏎ open · n new · + group · m move · r rename · D del group · a archive · x close"
                if VIEWS[view] == "Sessions" else
                "j/k · ⏎ open · y/n suggestion" if PANEL else
                "1-5/tab · j/k · ⏎ open · n/N new lead/neeo · a archive · x close · y/n sugg · q")
        foot = confirm[0] if confirm else (msg or keys)
        scr.addnstr(oy + ih - 1, ox, foot, iw - 1, C["warn"] if confirm or msg else C["dim"])
        scr.refresh()

        k = scr.getch()
        if k == -1:
            continue
        msg = ""
        cur = rows[sel] if rows and sel < len(rows) and rows[sel].kind else None
        if confirm:
            if k == ord("y"):
                msg = confirm[1]()
            confirm, seen = None, -1
            continue
        if k == ord("q") and not PANEL:
            return
        if PANEL and k in (ord("\t"), curses.KEY_RIGHT, ord("l"), curses.KEY_BTAB, curses.KEY_LEFT, ord("h")) + tuple(range(ord("1"), ord("6"))):
            continue
        if k in (ord("\t"), curses.KEY_RIGHT, ord("l")):
            view, sel, top = (view + 1) % len(VIEWS), 0, 0
        elif k in (curses.KEY_BTAB, curses.KEY_LEFT, ord("h")):
            view, sel, top = (view - 1) % len(VIEWS), 0, 0
        elif ord("1") <= k <= ord("5"):
            view, sel, top = k - ord("1"), 0, 0
        elif k in (ord("j"), curses.KEY_DOWN) and selectable:
            sel = next((i for i in selectable if i > sel), sel)
        elif k in (ord("k"), curses.KEY_UP) and selectable:
            sel = next((i for i in reversed(selectable) if i < sel), sel)
        elif k == ord("g") and selectable:
            sel = selectable[0]
        elif k == ord("G") and selectable:
            sel = selectable[-1]
        elif k in (10, 13, curses.KEY_ENTER, ord(" ")) and cur:
            if cur.kind == "project":
                expanded ^= {cur.data["id"]}
            elif cur.kind == "group":
                collapsed ^= {cur.data}
            elif cur.kind == "session":
                msg = ctl("focus", cur.data["session_id"]) if cur.data["state"] != "elsewhere" else act_session(conn, cur.data)
            elif cur.kind == "task" and cur.data["session_id"]:
                msg = ctl("focus", cur.data["session_id"])
            elif cur.kind == "task":
                msg = "no session linked to this task"
        elif k in (ord("d"), ord("x")) and cur and cur.kind == "task" and VIEWS[view] == "Plan":
            status = "done" if k == ord("d") else "dropped"
            conn.execute("UPDATE tasks SET status=?, updated_at=datetime('now','localtime') WHERE id=?", (status, cur.data["id"]))
            log_ui(conn, f"{status}: {cur.data['title']}")
            msg = f"marked {status}"
        elif VIEWS[view] == "Sessions" and cur and k in (ord("n"), ord("+"), ord("m"), ord("r"), ord("D")):
            msg = tree_key(scr, conn, k, cur) or ""
        elif k in (ord("n"), ord("N")) and cur and cur.kind not in ("suggestion", "group"):
            p = cur.data if cur.kind == "project" else conn.execute(
                "SELECT * FROM projects WHERE id=?", (cur.data["project_id"],)).fetchone()
            if p:
                msg = new_session(conn, p, "claude-neeo" if k == ord("N") else "claude-lead")
        elif k in (ord("y"), ord("n")) and cur and cur.kind == "suggestion":
            status = "accepted" if k == ord("y") else "rejected"
            conn.execute("UPDATE suggestions SET status=?, resolved_at=datetime('now','localtime') WHERE id=?",
                         (status, cur.data["id"]))
            log_ui(conn, f"{status} suggestion #{cur.data['id']}: {cur.data['text']}")
        elif k == ord("a") and cur and cur.kind == "session":
            conn.execute("UPDATE sessions SET state='archived' WHERE session_id=?", (cur.data["session_id"],))
            log_ui(conn, f"archived {cur.data['title'] or cur.data['session_id'][:8]}")
            msg = "archived (still resumable by ID)"
        elif k == ord("x") and cur and cur.kind == "session" and VIEWS[view] != "Plan":  # in Plan, x only drops tasks
            s = cur.data

            def close(s=s):
                if pane_alive(s["pane"]):
                    tmux("kill-pane", "-t", s["pane"])
                conn.execute("UPDATE sessions SET state='ended' WHERE session_id=?", (s["session_id"],))
                log_ui(conn, f"closed session {s['session_id'][:8]} ({s['project'] or s['cwd']})")
                return "closed"
            confirm = (f"close session {s['session_id'][:8]} in {s['project'] or s['cwd']}? y/N", close)
        seen = -1


if __name__ == "__main__":
    if "--panel" in sys.argv:
        PANEL = sys.argv[sys.argv.index("--panel") + 1]
    os.environ.setdefault("ESCDELAY", "25")
    locale.setlocale(locale.LC_ALL, "")
    curses.wrapper(main)
