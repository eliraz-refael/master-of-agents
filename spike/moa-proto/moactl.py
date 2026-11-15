#!/usr/bin/env python3
"""THROWAWAY PROTOTYPE: the brain API. The one way agents (and the console) read and
change the brain and drive sessions on the prototype's tmux server (-L moa).
Not product code. A session ID can be any unique prefix, a title or a project name.

Read:     brief | attention | ls [--all] | find Q | project NAME
Sessions: open PATH [--account A] [--title T] [--group G] [--prompt P] [--bg]
          resume Q [--bg] | close Q | archive Q... | set Q [--title T] [--group G]
          sleep Q... | sleep --idle HOURS [--dry-run]   (close the window, keep it resumable: 💤 in the tree)
          term [PATH] [--title T]   (a plain shell in a window of its own, e.g. for a dev server)
Groups:   group add PATH | group rename OLD NEW | group rm PATH
Brain:    note TEXT [--project P] [--kind K] | suggest TEXT
          task add PROJECT TITLE [--today] | task set ID [--status S] [--today|--not-today] [--blocked-on X]
          adopt PANE [--title T] [--group G] [--id ID]   (an untracked agent pane in a private folder)
          focus Q                (show it: in the cockpit's agent spot if the layout has one)
          goto PANEL             (console, tree, today, agent…; reopens it if it was closed)
Layouts:  layout list | layout show NAME | layout apply [NAME] | layout set NAME JSON
          JSON: {"split":"h"|"v","children":[...]} or {"panel":NAME}, each optionally with
          "size": cells or "NN%". Panels: console, tree, today, activity, projects, picture, agent
Setup:    import-herdr
"""
import argparse
import datetime
import glob
import json
import os
import re
import shlex
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from picture import connect, tmux, pane_alive, session_alive, on_this_server, log_ui, SOCK, asleep, epoch, live  # noqa: E402
import layout  # noqa: E402

LIVE = ("working", "idle", "waiting")
LAUNCH = {"claude": "claude", "claude-lead": "claude-lead", "claude-neeo": "claude-neeo", "claude-work": "claude-work",
          "claude-personal": "claude-personal"}


def launch_cmd(account, resume=None, prompt=None):
    if account == "codex":
        return f"codex resume {resume}" if resume else "codex"
    if (account or "").startswith("/"):  # adopted under a CLAUDE_CONFIG_DIR moa has no name for
        cmd = f"CLAUDE_CONFIG_DIR={shlex.quote(account)} claude"
    else:
        cmd = LAUNCH.get(account or "", "claude-lead")
    if resume:
        cmd += f" --resume {resume}"
    if prompt:
        cmd += " " + shlex.quote(prompt)
    return cmd


SESS = "SELECT s.*, p.name AS project FROM sessions s LEFT JOIN projects p ON p.id=s.project_id "


def candidates(conn, q):
    rows = conn.execute(SESS + "WHERE s.session_id LIKE ?", (q + "%",)).fetchall()
    if rows:
        return rows
    like = f"%{q.lower()}%"
    rows = conn.execute(SESS + """WHERE coalesce(s.state,'') != 'archived' AND
        (lower(coalesce(s.title,'')) LIKE ? OR lower(coalesce(p.name,'')) LIKE ? OR lower(coalesce(s.grp,'')) LIKE ?)
        ORDER BY s.last_seen DESC""", (like, like, like)).fetchall()
    exact = [r for r in rows if (r["title"] or "").lower() == q.lower()]
    return exact or rows


def find(conn, q):
    rows = candidates(conn, q)
    if len(rows) == 1:
        return rows[0]
    if not rows:
        sys.exit(f"no session matches {q!r}")
    sys.exit(f"{len(rows)} sessions match {q!r}; be more specific:\n" + "\n".join(
        f"  {r['session_id'][:8]}  {r['title'] or '-'}  ({r['grp'] or r['project'] or '-'}, {r['state']})" for r in rows[:8]))


def window_name(text):
    return (text or "agent")[:24]


def open_session(conn, path, account, title, group, prompt, bg):
    path = os.path.abspath(os.path.expanduser(path))
    if not os.path.isdir(path):
        os.makedirs(path)
        print(f"created {path}")
    env = {"MOA_TRACK": "1"}
    if title:
        env["MOA_TITLE"] = title
    if group:
        env["MOA_GROUP"] = group
    envs = " ".join(f"{k}={shlex.quote(v)}" for k, v in env.items())
    args = ["new-window", "-d", "-P", "-F", "#{pane_id}", "-n", window_name(title or os.path.basename(path)), "-c", path]
    r = tmux(*args, f"env {envs} {launch_cmd(account, prompt=prompt)}")
    if r.returncode:
        sys.exit(f"tmux failed: {r.stderr.strip()} (is the moa tmux server running?)")
    pane = r.stdout.strip()
    log_ui(conn, f"opened a {account} session in {path}" + (f" ({title})" if title else ""))
    how = "in the background" if bg else layout.focus(pane, title_of(conn))
    print(f"opened {account} in {path} as pane {pane}; {how}")


def title_of(conn):
    def title(pane):
        row = conn.execute("SELECT title, session_id FROM sessions WHERE pane=? ORDER BY last_seen DESC LIMIT 1", (pane,)).fetchone()
        return (row["title"] or row["session_id"][:8]) if row else None
    return title


def resume(conn, s, bg):
    # A stale pane ID (tmux restarts numbering after a server restart) can point at a
    # cockpit panel; only an untagged pane or an agent pane is this session's.
    if not on_this_server(s) and s["state"] in LIVE and not asleep(s):
        sys.exit("it's running in another moa's tmux server; switch to it there (or close it there first)")
    if session_alive(s) and layout.panel_of(s["pane"]) in ("", "agent"):
        print(f"already running in pane {s['pane']}" + ("" if bg else "; " + layout.focus(s["pane"], title_of(conn))))
        return
    if s["state"] == "elsewhere":
        sys.exit("it's running in herdr right now; close it there first")
    if not s["cwd"] or not os.path.isdir(s["cwd"]):
        sys.exit(f"its folder {s['cwd']} is gone")
    args = ["new-window", "-d", "-P", "-F", "#{pane_id}", "-n", window_name(s["title"] or s["project"]), "-c", s["cwd"]]
    r = tmux(*args, f"env MOA_TRACK=1 {launch_cmd(s['account'], resume=s['session_id'])}")
    if r.returncode:
        sys.exit(f"tmux failed: {r.stderr.strip()} (is the moa tmux server running?)")
    pane = r.stdout.strip()
    # awake again (the hook's SessionStart clears asleep_at too; this covers an agent without the hook)
    conn.execute("UPDATE sessions SET pane=?, asleep_at=NULL WHERE session_id=?", (pane, s["session_id"]))
    conn.commit()
    log_ui(conn, f"resumed {s['title'] or s['session_id'][:8]}")
    how = "in the background" if bg else layout.focus(pane, title_of(conn))
    print(f"resumed {s['session_id']} in {s['cwd']}; {how}")


def here_dir(pane):
    """Where "here" is for a new terminal: the folder of the agent (or terminal) you're
    looking at; on a cockpit panel, the folder of the session selected in it (@moa_here,
    set by picture.py), else of the agent in the cockpit's agent spot; else ~.
    `pane` is the focused pane (the tmux key passes it); else this process's own pane, if
    it's on moa's server (TMUX_PANE from another tmux server names someone else's pane)."""
    if not pane and os.path.basename(os.environ.get("TMUX", "").split(",")[0]) == SOCK:
        pane = os.environ.get("TMUX_PANE")
    r = tmux("display-message", "-p", *(["-t", pane] if pane else []), "#{@moa_panel}\t#{pane_current_path}\t#{@moa_here}")
    panel, cwd, picked = (r.stdout.rstrip("\n").split("\t") + ["", ""])[:3]
    if r.returncode == 0 and panel in ("", "agent") and cwd:
        return cwd
    if r.returncode == 0 and picked:
        return picked
    s = layout.slot()
    if s and s["panel"] == "agent":
        return layout.out("display-message", "-p", "-t", s["pane"], "#{pane_current_path}") or "~"
    return "~"


def term(path, title, pane):
    """A plain shell in a window of its own (a dev server, a quick look around). It isn't
    an agent: the brain doesn't track it, and the status line and panels leave it out. Its
    pane is marked @moa_term so the session picker can list the open terminals."""
    path = os.path.expanduser(path or "")
    if not os.path.isabs(path):  # relative to "here" (`/term web`), not to wherever moactl runs
        path = os.path.join(os.path.expanduser(here_dir(pane)), path)
    path = os.path.abspath(path)
    if not os.path.isdir(path):
        sys.exit(f"no folder {path}")
    name = f"⌨ {title or ('~' if path == os.path.expanduser('~') else os.path.basename(path) or path)}"[:24]
    r = tmux("new-window", "-P", "-F", "#{pane_id}", "-n", name, "-c", path)  # no command: the user's shell
    if r.returncode:
        sys.exit(f"tmux failed: {r.stderr.strip()} (is the moa tmux server running?)")
    new = r.stdout.strip()
    tmux("set", "-p", "-t", new, "@moa_term", "1", ";", "set", "-p", "-t", new, "@moa_label", name)
    print(f"opened a terminal in {path.replace(os.path.expanduser('~'), '~')} (window {name})")


def close(conn, s):
    if session_alive(s):
        tmux("kill-pane", "-t", s["pane"])
    conn.execute("UPDATE sessions SET state='ended', asleep_at=NULL WHERE session_id=?", (s["session_id"],))
    log_ui(conn, f"closed {s['title'] or s['session_id'][:8]}")
    print("closed")


CONFIG_DIRS = {"claude": "~/.claude", "claude-lead": "~/.claude-lead", "claude-neeo": "~/.claude-neeo", "claude-work": "~/.claude-account1",
               "claude-personal": "~/.claude-account2"}


# Only these folders belong to this (private) brain; day-job panes are never listed or adopted.
PRIVATE_ROOTS = [os.path.expanduser("~/private"), os.path.expanduser("~/.config")]
ACCOUNTS = ("claude-lead", "claude-neeo", "claude-work", "claude-personal", "claude", "codex")


def private(path):
    return any(path == r or path.startswith(r + "/") for r in PRIVATE_ROOTS)


PANE_FMT = "#{pane_id}\t#{pane_current_path}\t#{pane_start_command}\t#{pane_pid}"


def pane_agent(pane, info=None):
    """What agent a pane runs: {cwd, account, sid, title, group}, or None if it isn't one.
    Reads the launch command when moa started the pane; otherwise (an agent typed into a
    plain shell) looks at the shell's child processes and their environment. `info` is the
    pane's PANE_FMT line when the caller already listed it."""
    if info is None:
        r = tmux("display-message", "-p", "-t", pane, PANE_FMT)
        if r.returncode:
            return None
        info = r.stdout.rstrip("\n")
    _, cwd, cmd, pid = (info.split("\t") + ["", "", ""])[:4]
    if "console.py" in cmd or "picture.py" in cmd:
        return None
    if len(cmd) >= 2 and cmd[0] == cmd[-1] == '"':
        cmd = cmd[1:-1].replace('\\"', '"')  # tmux shows the start command wrapped in quotes
    if not cmd.strip():
        if not pid:
            return None
        kids = subprocess.run(["pgrep", "-P", pid], capture_output=True, text=True).stdout.split()
        return next((a for a in (child_agent(cwd, k) for k in kids) if a), None)
    try:
        words = shlex.split(cmd)
    except ValueError:
        return None
    return agent_from(cwd, words, {})


def child_agent(cwd, pid):
    """A shell's child process as an agent. ps prints argv unquoted, so it's split on
    whitespace (not shlex); the environment, which `ps e` appends after argv, is read
    separately so its words never land in argv."""
    argv = subprocess.run(["ps", "-ww", "-o", "command=", "-p", pid], capture_output=True, text=True).stdout.split()
    ps = subprocess.run(["ps", "eww", "-o", "command=", "-p", pid], capture_output=True, text=True).stdout
    env = dict(m.groups() for m in re.finditer(r"\b(CLAUDE_CONFIG_DIR|MOA_TITLE|MOA_GROUP)=(\S+)", ps))
    return agent_from(cwd, argv, env)


def agent_from(cwd, words, env):
    args = []
    for w in words:
        if not args and re.fullmatch(r"[A-Z_][A-Z0-9_]*=.*", w):
            k, v = w.split("=", 1)
            env[k] = v
        elif not args and w == "env":
            continue
        else:
            args.append(w)
    if not args:
        return None
    prog = os.path.basename(args[0])
    if prog in ("node", "bun") and len(args) > 1:  # `claude` running under its interpreter
        prog, args = os.path.basename(args[1]), args[1:]
    if prog not in ACCOUNTS:
        return None
    account = prog
    if prog == "claude" and env.get("CLAUDE_CONFIG_DIR"):
        cfg = env["CLAUDE_CONFIG_DIR"].rstrip("/")
        account = next((a for a, d in CONFIG_DIRS.items() if os.path.expanduser(d) == cfg), cfg)
    sid = None
    if "--resume" in args[1:-1]:
        sid = args[args.index("--resume") + 1]
    elif prog == "codex" and len(args) > 2 and args[1] == "resume":
        sid = args[2]
    return {"cwd": cwd, "account": account, "sid": sid, "title": env.get("MOA_TITLE"), "group": env.get("MOA_GROUP")}


def related(a, b):
    """Same place: equal, or one inside the other (an agent may cd into a subfolder)."""
    a, b = (a or "").rstrip("/"), (b or "").rstrip("/")
    return bool(a and b) and (a == b or a.startswith(b + "/") or b.startswith(a + "/"))


def untracked_panes(conn):
    """Agent panes in private folders on the moa tmux server that the brain doesn't track.
    A row whose pane ID is reused by a pane in another folder (pane IDs restart with the
    tmux server) doesn't count as tracking it."""
    r = tmux("list-panes", "-a", "-F", PANE_FMT)
    held = {}
    for row in conn.execute("SELECT pane, cwd FROM sessions WHERE pane IS NOT NULL AND state != 'ended'"):
        held.setdefault(row["pane"], []).append(row["cwd"])
    out = []
    for line in r.stdout.splitlines():
        pane, cwd = (line.split("\t") + [""])[:2]
        if not private(cwd) or any(related(c, cwd) for c in held.get(pane, ())):
            continue
        agent = pane_agent(pane, line)
        if agent:
            out.append((pane, agent))
    return out


def adopt(conn, pane, title, group, sid=None):
    agent = pane_agent(pane)
    if not agent:
        sys.exit(f"pane {pane} isn't running an agent moa recognizes")
    cwd, account = agent["cwd"], agent["account"]
    if not private(cwd):
        sys.exit(f"{cwd} isn't a private folder; this brain doesn't track it")
    title, group = title or agent["title"], group or agent["group"]
    sid = sid or agent["sid"]
    if not sid:
        if account not in CONFIG_DIRS:
            sys.exit(f"can't look up {account} conversations; pass the conversation ID with --id")
        slot = os.path.join(os.path.expanduser(CONFIG_DIRS[account]), "projects", re.sub(r"[^A-Za-z0-9]", "-", cwd))
        files = sorted(glob.glob(os.path.join(slot, "*.jsonl")), key=os.path.getmtime)
        # Skip conversations another live pane already holds (two agents in one folder).
        taken = {row["session_id"] for row in conn.execute(
            "SELECT session_id, pane FROM sessions WHERE pane IS NOT NULL AND pane != ? AND state != 'ended'", (pane,))
            if pane_alive(row["pane"])}
        files = [f for f in files if os.path.basename(f)[:-len(".jsonl")] not in taken]
        if not files:
            sys.exit(f"no untracked conversation found for {cwd} in {account}; pass it with --id")
        sid = os.path.basename(files[-1])[:-len(".jsonl")]
    holder = conn.execute("SELECT pane, cwd FROM sessions WHERE session_id=?", (sid,)).fetchone()
    if holder and holder["pane"] and holder["pane"] != pane and pane_alive(holder["pane"]):
        sys.exit(f"session {sid[:8]} is already tracked in live pane {holder['pane']}")
    if group:
        conn.execute("INSERT OR IGNORE INTO groups(path) VALUES(?)", (group,))
    # A stale row still pointing at this (reused) pane ID lets go of it.
    conn.execute("UPDATE sessions SET pane=NULL WHERE pane=? AND session_id != ?", (pane, sid))
    conn.execute("""INSERT INTO sessions(session_id,account,cwd,project_id,title,grp,state,pane,first_seen,last_seen)
        VALUES(:sid,:acct,:cwd,(SELECT id FROM projects WHERE :cwd = path OR :cwd LIKE path || '/%' OR :cwd LIKE path || '-worktrees/%'
               ORDER BY length(path) DESC LIMIT 1),:title,:grp,'idle',:pane,datetime('now','localtime'),datetime('now','localtime'))
        ON CONFLICT(session_id) DO UPDATE SET pane=excluded.pane, state='idle', last_seen=excluded.last_seen,
          title=coalesce(sessions.title, excluded.title), grp=coalesce(sessions.grp, excluded.grp)""",
                 {"sid": sid, "acct": account, "cwd": cwd, "title": title, "grp": group, "pane": pane})
    conn.commit()
    log_ui(conn, f"adopted {title or sid[:8]} ({account}, {cwd})")
    print(f"adopted {sid} ({account}) in {cwd} as {title or '-'} [{group or '-'}]")


def ls(conn, show_all, as_json=False):
    where = "" if show_all else ("WHERE s.state IN ('working','idle','waiting','elsewhere') OR s.last_seen >= datetime('now','localtime','-2 days')"
                                 " OR (s.asleep_at IS NOT NULL AND s.state = 'ended')")  # asleep: one resume from running
    rows = []
    for s in conn.execute("SELECT s.*, p.name AS project FROM sessions s LEFT JOIN projects p ON p.id=s.project_id "
                          f"{where} ORDER BY s.last_seen DESC"):
        # asleep first: a session slept in the cockpit's agent spot leaves its pane ID alive (the empty spot)
        alive = "asleep" if asleep(s) else "live" if session_alive(s) else s["state"]
        if as_json:
            rows.append({"id": s["session_id"], "alive": alive, "state": s["state"], "account": s["account"],
                         "group": s["grp"] or s["project"], "title": s["title"] or (s["last_prompt"] or "")[:40],
                         "cwd": s["cwd"], "last": s["last_seen"]})
            continue
        print(f"{s['session_id'][:8]}  {alive:<9} {s['account'] or '':<12} {(s['grp'] or s['project'] or '-'):<22} "
              f"{(s['title'] or (s['last_prompt'] or '')[:40]):<34} {s['cwd']}  last {s['last_seen']}")
    for pane, agent in untracked_panes(conn):
        if as_json:
            rows.append({"untracked": pane, "account": agent["account"], "cwd": agent["cwd"]})
            continue
        how = f"moactl adopt {pane}" if agent["sid"] or agent["account"] in CONFIG_DIRS else f"moactl adopt {pane} --id ID"
        print(f"UNTRACKED pane {pane}: {agent['account']} in {agent['cwd']}  → {how}")
    if as_json:
        print(json.dumps(rows))


def due_date(text):
    """--due: YYYY-MM-DD, today, tomorrow, +N days, a weekday (the next one), or "" to clear."""
    if text is None or text == "":
        return text
    today = datetime.date.today()
    if text in ("today", "tomorrow"):
        return (today + datetime.timedelta(days=text == "tomorrow")).isoformat()
    if text.startswith("+") and text[1:].isdigit():
        return (today + datetime.timedelta(days=int(text[1:]))).isoformat()
    days = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]
    day = next((i for i, d in enumerate(days) if text.lower() in (d, d[:3])), None)
    if day is not None:
        ahead = (day - today.weekday()) % 7 or 7  # the next one, never today
        return (today + datetime.timedelta(days=ahead)).isoformat()
    try:
        return datetime.date.fromisoformat(text).isoformat()
    except ValueError:
        sys.exit(f"--due: {text!r} isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)")


def session_id(conn, q):
    """--session: a full session ID from a prefix, title or project (via find)."""
    return None if q is None else "" if q == "" else find(conn, q)["session_id"]


def task_ls(conn, project=None, show_all=False):
    """Open tasks by project, with IDs, for moa to update (moactl task set ID …)."""
    where = "" if show_all else "WHERE t.status NOT IN ('done','dropped')"
    if project:
        where += (" AND" if where else "WHERE") + " lower(p.name)=lower(?)"
    rows = conn.execute(f"""SELECT t.*, p.name AS project FROM tasks t LEFT JOIN projects p ON p.id=t.project_id {where}
                            ORDER BY p.name, t.id""", (project,) if project else ()).fetchall()
    last = None
    for t in rows:
        if t["project"] != last:
            print(f"## {t['project'] or '-'}")
            last = t["project"]
        extra = [x for x in (t["priority"] and t["priority"].upper(), t["today"] == datetime.date.today().isoformat() and "today",
                             t["who"] and f"who={t['who']}", t["due"] and f"due={t['due']}",
                             t["blocked_on"] and f"on={t['blocked_on']}", t["session_id"] and f"session={t['session_id'][:8]}") if x]
        print(f"- #{t['id']} [{t['status']}] {t['title']}" + (f"  ({', '.join(extra)})" if extra else ""))
    if not rows:
        print("no open tasks")


# --- suggestions: moa proposes, Eliraz answers with a button --------------------------------
DOABLE = {"task", "set", "archive", "close", "group", "note", "project"}  # what a suggestion's "yes" may run


def suggest(conn, text, actions):
    for act in actions:  # checked now, so a bad action fails for moa, not later for Eliraz
        try:
            words = shlex.split(act)
        except ValueError as e:
            sys.exit(f"--do {act!r}: {e}")
        if not words or words[0] not in DOABLE:
            sys.exit(f"--do {act!r}: a suggestion can run moactl {', '.join(sorted(DOABLE))}")
    dup = conn.execute("SELECT id FROM suggestions WHERE status='open' AND text=?", (text,)).fetchone()
    if dup:
        print(f"already suggested (#{dup[0]})")
        return
    cur = conn.execute("INSERT INTO suggestions(text, action) VALUES(?,?)", (text, json.dumps(actions) if actions else None))
    conn.commit()
    print(f"suggestion #{cur.lastrowid}: Eliraz sees it with yes / later / no buttons")


def suggestion(conn, op, sid):
    if op == "ls":
        for r in conn.execute("SELECT * FROM suggestions WHERE status IN ('open','later') ORDER BY id"):
            acts = json.loads(r["action"]) if r["action"] else []
            print(f"#{r['id']} [{r['status']}] {r['text']}" + (f"  (yes runs: {'; '.join(acts)})" if acts else ""))
        return
    r = conn.execute("SELECT * FROM suggestions WHERE id=?", (sid,)).fetchone() if sid else None
    if not r:
        sys.exit("moactl suggestion yes|no|later ID")
    if r["status"] not in ("open", "later"):
        sys.exit(f"suggestion #{sid} is already {r['status']}")
    failed = []
    if op == "yes":
        for act in json.loads(r["action"]) if r["action"] else []:
            out = subprocess.run([sys.executable, os.path.abspath(__file__), *shlex.split(act)], capture_output=True, text=True)
            if out.returncode:
                failed.append(f"{act}: {(out.stderr or out.stdout).strip()}")
    status = {"yes": "accepted", "no": "rejected", "later": "later"}[op]
    conn.execute("UPDATE suggestions SET status=?, resolved_at=datetime('now','localtime') WHERE id=?", (status, sid))
    log_ui(conn, f"{status} suggestion #{sid}: {r['text']}" + (f" (failed: {'; '.join(failed)})" if failed else ""))
    print(("done" if op == "yes" else status) + ("" if not failed else "; but " + "; ".join(failed)))


def age(ts):
    if not ts:
        return "?"
    try:
        d = (datetime.datetime.now() - datetime.datetime.fromisoformat(ts)).total_seconds()
    except ValueError:
        return ts
    return f"{int(d // 60)}m" if d < 3600 else f"{int(d // 3600)}h" if d < 86400 else f"{int(d // 86400)}d"


def label(s):
    return f"{s['title'] or (s['last_prompt'] or '')[:50] or s['session_id'][:8]} [{s['grp'] or s['project'] or '-'}]"


def brief(conn):
    today = datetime.date.today().isoformat()
    out = [f"# Brief ({datetime.datetime.now():%a %d %b %H:%M}). More: moactl attention | project NAME | ls | find Q"]
    tasks = conn.execute("SELECT t.*, p.name AS project FROM tasks t LEFT JOIN projects p ON p.id=t.project_id WHERE t.today=?",
                         (today,)).fetchall()
    out.append("## Today: " + ("; ".join(f"#{t['id']} {t['project'] or '-'}: {t['title']} [{t['status']}]" for t in tasks)
                               if tasks else "nothing set yet"))
    out.append("## Needs attention")
    out += attention_lines(conn, limit=8) or ["- nothing"]
    hot = conn.execute("SELECT name FROM projects WHERE last_activity >= date('now','localtime','-7 days') ORDER BY last_activity DESC").fetchall()
    out.append("## Active this week: " + (", ".join(r["name"] for r in hot) or "none"))
    out.append("## Live sessions")
    live = conn.execute(SESS + "WHERE s.state IN ('working','idle','waiting','elsewhere') ORDER BY s.last_seen DESC LIMIT 12").fetchall()
    out += [f"- {r['session_id'][:8]} {label(r)} {r['state']} {age(r['last_seen'])}" for r in live] or ["- none"]
    sleepy = conn.execute(SESS + "WHERE s.asleep_at IS NOT NULL AND s.state='ended' ORDER BY s.asleep_at DESC").fetchall()
    if sleepy:  # normal, not a problem: mention briefly
        out.append(f"## Asleep (idle, window closed to save memory; moactl resume ID): "
                   + ", ".join(f"{r['session_id'][:8]} {label(r)}" for r in sleepy[:12]) + (" …" if len(sleepy) > 12 else ""))
    out.append("## What you know about Eliraz (latest notes)")
    for n in conn.execute("SELECT kind, text FROM notes WHERE kind IN ('preference','routine','fact') ORDER BY id DESC LIMIT 10"):
        out.append(f"- ({n['kind']}) {n['text'][:200]}")
    last = conn.execute("SELECT text, created_at FROM notes WHERE kind='thread-summary' ORDER BY id DESC LIMIT 1").fetchone()
    if last:
        out.append(f"## Last thread ({last['created_at'][:10]}): {last['text'][:600]}")
    return "\n".join(out)


def attention_lines(conn, limit=20):
    today = datetime.date.today().isoformat()
    out = []
    for s in conn.execute(SESS + "WHERE s.state='waiting' ORDER BY s.last_seen"):
        out.append(f"- ⚠ {label(s)} is blocked on you (permission/question) for {age(s['last_seen'])}  → moactl resume {s['session_id'][:8]}")
    for s in conn.execute(SESS + "WHERE s.state='idle' AND s.last_seen >= datetime('now','localtime','-1 day') ORDER BY s.last_seen"):
        out.append(f"- ● {label(s)} finished its turn {age(s['last_seen'])} ago; your move  → moactl resume {s['session_id'][:8]}")
    for t in conn.execute("""SELECT t.*, p.name AS project FROM tasks t LEFT JOIN projects p ON p.id=t.project_id
                             WHERE t.today=? AND t.status NOT IN ('done','dropped')""", (today,)):
        out.append(f"- ▸ today: {t['project'] or '-'}: {t['title']} [{t['status']}]")
    for t in conn.execute("""SELECT t.*, p.name AS project FROM tasks t LEFT JOIN projects p ON p.id=t.project_id
                             WHERE t.status='blocked'"""):
        out.append(f"- ⧗ blocked: {t['project'] or '-'}: {t['title']} on {t['blocked_on'] or '?'}")
    for p in conn.execute("""SELECT p.name, p.last_activity, count(t.id) AS n FROM projects p JOIN tasks t ON t.project_id=p.id
                             AND t.status IN ('todo','active','waiting') WHERE p.last_activity < date('now','localtime','-7 days')
                             GROUP BY p.id ORDER BY p.last_activity"""):
        out.append(f"- ◷ {p['name']}: {p['n']} open task(s), untouched since {p['last_activity']}")
    for s in conn.execute("SELECT * FROM suggestions WHERE status='open' ORDER BY id"):
        out.append(f"- ? suggestion #{s['id']}: {s['text']}")
    return out[:limit]


def add_project(conn, name, path, summary):
    """A folder becomes a project: its tasks get their own group on the big picture, and the
    sessions already working in it are linked to it."""
    path = os.path.abspath(os.path.expanduser(path))
    if not private(path):
        sys.exit(f"{path} isn't a private folder; this brain doesn't track it")
    if conn.execute("SELECT 1 FROM projects WHERE lower(name)=lower(?) OR path=?", (name, path)).fetchone():
        sys.exit(f"there's already a project named {name!r} or for {path}")
    pid = conn.execute("INSERT INTO projects(name, path, summary, last_activity) VALUES(?,?,?,date('now','localtime'))",
                       (name, path, summary)).lastrowid
    n = conn.execute("UPDATE sessions SET project_id=? WHERE (cwd=? OR cwd LIKE ? || '/%') AND project_id IS NULL",
                     (pid, path, path)).rowcount
    conn.commit()
    log_ui(conn, f"new project {name} ({path})")
    print(f"project {name} added for {path}" + (f"; {n} session{'s' if n != 1 else ''} linked" if n else ""))


def project(conn, name):
    p = conn.execute("SELECT * FROM projects WHERE lower(name)=lower(?) OR lower(name) LIKE lower(?) ORDER BY lower(name)=lower(?) DESC LIMIT 1",
                     (name, f"%{name}%", name)).fetchone()
    if not p:
        sys.exit(f"no project matches {name!r}")
    print(f"{p['name']} [{p['status']}] {p['path']}  last activity {p['last_activity']}\n{p['summary'] or ''}")
    for t in conn.execute("SELECT * FROM tasks WHERE project_id=? AND status NOT IN ('done','dropped')", (p["id"],)):
        print(f"  task #{t['id']} {t['title']} [{t['status']}]" + (f" blocked on {t['blocked_on']}" if t["blocked_on"] else ""))
    for s in conn.execute(SESS + "WHERE s.project_id=? ORDER BY s.last_seen DESC LIMIT 10", (p["id"],)):
        print(f"  session {s['session_id'][:8]} {label(s)} {s['state']} {age(s['last_seen'])}  last prompt: {(s['last_prompt'] or '')[:80]}")
    for n in conn.execute("SELECT * FROM notes WHERE project_id=? ORDER BY id DESC LIMIT 10", (p["id"],)):
        print(f"  note ({n['kind']}) {n['text'][:200]}")
    return p


def set_session(conn, s, title, group):
    if title:
        conn.execute("UPDATE sessions SET title=? WHERE session_id=?", (title, s["session_id"]))
    if group:
        conn.execute("INSERT OR IGNORE INTO groups(path) VALUES(?)", (group,))
        conn.execute("UPDATE sessions SET grp=? WHERE session_id=?", (group, s["session_id"]))
    conn.commit()
    log_ui(conn, f"session {title or s['title'] or s['session_id'][:8]}" + (f" → {group}" if group else " renamed"))


def group_cmd(conn, op, args):
    if op == "add":
        conn.execute("INSERT OR IGNORE INTO groups(path) VALUES(?)", (args[0],))
        log_ui(conn, f"new group {args[0]}")
    elif op == "rename":
        old, new = args
        conn.execute("UPDATE sessions SET grp = ? || substr(grp, length(?) + 1) WHERE grp = ? OR grp LIKE ? || '/%'", (new, old, old, old))
        conn.execute("UPDATE groups SET path = ? || substr(path, length(?) + 1) WHERE path = ? OR path LIKE ? || '/%'", (new, old, old, old))
        conn.execute("INSERT OR IGNORE INTO groups(path) VALUES(?)", (new,))
        log_ui(conn, f"group {old} → {new}")
    elif op == "rm":
        n = conn.execute("SELECT count(*) FROM sessions WHERE (grp = ? OR grp LIKE ? || '/%') AND state != 'archived'",
                         (args[0], args[0])).fetchone()[0]
        if n:
            sys.exit(f"{args[0]} still has {n} session(s); move or archive them first")
        conn.execute("DELETE FROM groups WHERE path = ? OR path LIKE ? || '/%'", (args[0], args[0]))
        log_ui(conn, f"removed group {args[0]}")
    conn.commit()
    print("ok")


def local(ts):
    if not ts:
        return None
    t = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00")).astimezone()
    return t.strftime("%Y-%m-%d %H:%M:%S")


def import_herdr(conn):
    data = json.load(open(os.path.expanduser("~/.local/state/herdr-sessions/sessions.json")))
    n = skipped = 0
    for h in data["sessions"]:
        group = h.get("group") or ""
        if group.startswith("Work") or not h.get("conversation_id"):
            skipped += 1
            continue
        path = h["path"]
        conn.execute("""INSERT INTO sessions(session_id,account,cwd,project_id,title,grp,state,first_seen,last_seen)
            VALUES(:sid, :acct, :path,
                   (SELECT id FROM projects WHERE :path = path OR :path LIKE path || '/%' OR :path LIKE path || '-worktrees/%'
                    ORDER BY length(path) DESC LIMIT 1),
                   :title, :grp, :state, :first, :last)
            ON CONFLICT(session_id) DO UPDATE SET title=coalesce(sessions.title, excluded.title),
              grp=coalesce(sessions.grp, excluded.grp)""",
                     {"sid": h["conversation_id"], "acct": h["profile"], "path": path, "title": h["title"],
                      "grp": group or "Private", "state": "elsewhere" if h.get("live") else "ended",
                      "first": local(h.get("created_at")), "last": local(h.get("last_active_at"))})
        n += 1
    conn.commit()
    log_ui(conn, f"imported {n} sessions from herdr-sessions (skipped {skipped}: Work/* or no conversation ID)")
    print(f"imported {n}, skipped {skipped}")


def setup(conn, a):
    """moactl setup: find Claude accounts, show whether moa's hook is in each, install or remove it."""
    import setup_hooks
    row = conn.execute("SELECT value FROM meta WHERE key='setup_skipped'").fetchone()
    skipped = json.loads(row[0]) if row else []

    def save_skipped(names):
        conn.execute("INSERT INTO meta(key,value) VALUES('setup_skipped',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                     (json.dumps(sorted(set(names))),))
        conn.commit()

    accounts = setup_hooks.find_accounts(CONFIG_DIRS)
    if a.op is None and sys.stdin.isatty():
        save_skipped(setup_hooks.interactive(accounts, PRIVATE_ROOTS, skipped))
        return
    if a.op in (None, "list"):
        for x in accounts:
            x["skipped"] = x["name"] in skipped
        if a.json:
            print(json.dumps(accounts))
            return
        for x in accounts:
            plug = ("  + plugin" if x["plugin"] else f"  · no plugin yet (moactl setup {setup_hooks.again(x)})") if x["state"] == "installed" else ""
            print(f"{x['name']:<16} {setup_hooks.short(x['dir']):<22} {x['state']}{setup_hooks.limits(x)}{plug}{'  [skipped]' if x['skipped'] else ''}")
        return
    if not a.name:
        sys.exit(f"moactl setup {a.op} NAME (one of: {', '.join(x['name'] for x in accounts)})")
    acct = next((x for x in accounts if a.name in (x["name"], x["dir"], setup_hooks.short(x["dir"]))), None)
    if not acct:
        sys.exit(f"no Claude account {a.name!r} (moactl setup list)")
    if (a.brain or a.exceptions) and a.op != "install":
        sys.exit("--brain and --except go with `setup install`")
    if a.folders and a.op != "only":
        sys.exit(f"`setup {a.op}` takes no folders" + (" (--except FOLDERS with --brain BRAIN)" if a.op == "install" else ""))
    if a.exceptions and not a.brain:
        sys.exit("--except FOLDERS goes with --brain BRAIN: the account's sessions go to that brain, except in FOLDERS")
    if a.brain and (a.brain == "default" or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", a.brain)):
        sys.exit("a brain's name is lowercase letters, digits and dashes (and plain moa is the default brain)")
    if a.op == "install":
        err = setup_hooks.install(acct["dir"], brain=a.brain, exceptions=a.exceptions)
        where = ""
        if a.brain:
            where = (f"; its sessions go to the {a.brain} moa (start it with: moa {a.brain})"
                     + (f", except under {', '.join(setup_hooks.short(f) for f in a.exceptions)}, which stay with the moa that opens them (the default one, unless moa {a.brain} does)"
                        if a.exceptions else ""))
        print(f"tracking and the moa plugin installed for {acct['name']}{where}; its running sessions pick them up when they restart"
              + (f"\nbut: {err}" if err else ""))
    elif a.op == "only":
        folders = a.folders or PRIVATE_ROOTS
        err = setup_hooks.install(acct["dir"], folders)
        print(f"tracking and the moa plugin installed for {acct['name']}, only under {', '.join(setup_hooks.short(f) for f in folders)}"
              + (f"\nbut: {err}" if err else ""))
    elif a.op == "remove":
        n, err = setup_hooks.remove(acct["dir"])
        print((f"removed moa's hook and plugin from {acct['name']}" if n else f"{acct['name']} had no moa hook; its moa plugin is removed")
              + (f"\nbut: {err}" if err else ""))
    elif a.op == "skip":
        save_skipped(skipped + [acct["name"]])
        print(f"won't ask about {acct['name']} again (moactl setup install {acct['name']} if you change your mind)")
    if a.op in ("install", "only"):
        save_skipped([n for n in skipped if n != acct["name"]])


# --- partners: agents asking each other (and moa) through moa ------------------------------
ASK_WAIT = 540  # an agent's shell command times out at 10 minutes: answer before that
LANDED = 20     # seconds for a typed-in ask to show up as the peer's prompt before it counts as failed


def caller(conn):
    """(session ID, name) of whoever runs this: moa itself (the console marks its agent with
    MOA_SELF), the tracked session in this tmux pane, or (None, the folder's name) for an agent
    moa doesn't track."""
    if os.environ.get("MOA_SELF") == "moa":
        return "moa", "moa"
    pane, tsock = os.environ.get("TMUX_PANE"), os.environ.get("TMUX", "").split(",")[0]
    if pane:
        row = conn.execute(SESS + "WHERE s.pane=? AND coalesce(s.tmux,'') IN (?, '') AND s.state != 'ended' "
                           "ORDER BY s.last_seen DESC LIMIT 1", (pane, tsock)).fetchone()
        if row:
            return row["session_id"], row["title"] or label(row)
    master = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "master-chat", "master")
    if os.path.realpath(os.getcwd()) == os.path.realpath(master):  # moa started by hand (cd master && claude)
        return "moa", "moa"
    return None, os.path.basename(os.getcwd()) or "an agent outside moa"


def reachable(s):
    """A live session in a pane on moa's tmux server. pane_alive asks moa's server, so a pane
    ID recorded on another tmux server could name some other agent's pane here."""
    return s["state"] in LIVE and session_alive(s)


def peers(conn, as_json):
    """The agents you can ask: moa, and every live session in moa but you."""
    me = caller(conn)[0]
    rows = [r for r in conn.execute(SESS + "WHERE s.state IN ('working','idle','waiting') ORDER BY s.last_seen DESC")
            if r["session_id"] != me and reachable(r)]
    if as_json:
        print(json.dumps([{"name": "moa", "id": "moa", "state": "here", "about": "moa itself: the big picture"}] + [
            {"name": r["title"] or r["session_id"][:8], "id": r["session_id"][:8], "state": r["state"], "project": r["project"],
             "agent": "codex" if r["account"] == "codex" else "claude", "cwd": r["cwd"], "doing": (r["last_prompt"] or "")[:120]}
            for r in rows], indent=1))
        return
    print("moa       moa itself: knows the big picture (projects, tasks, who's working on what)")
    for r in rows:
        doing = " ".join((r["last_prompt"] or "").split())[:70]
        print(f"{r['session_id'][:8]}  {label(r)}  {r['state']}  {(r['cwd'] or '').replace(os.path.expanduser('~'), '~')}"
              + (f"\n          last asked: {doing}" if doing else ""))
    if not rows:
        print("(no other agent sessions are running in moa)")
    print('\nask one: moactl ask NAME-OR-ID "message"  (waits for the reply)')


def deliver(conn, a, s):
    """Type the message into the peer's pane (it's idle), framed so it knows who's asking and
    that its final message goes back. Bracketed paste keeps a multi-line message one prompt.
    False if another ask to that peer is already in (one at a time, so the peer's hook ties
    each answer to the right ask) or typing it failed."""
    # a 'sent' or 'received' ask nobody resolved (its asker was killed or gave up, or the peer
    # died mid-turn) would block this peer for good: it's idle now, so past any asker's wait
    # that ask isn't coming back. ts is restamped when an ask is typed in, so this counts from then.
    conn.execute("UPDATE asks SET status='failed', reply='it never got an answer from them' WHERE to_sid=? "
                 "AND status IN ('sent','received') AND ts < datetime('now','localtime',?)",
                 (a["to_sid"], f"-{2 * ASK_WAIT} seconds"))
    # claimed atomically, before typing: two askers polling the same idle peer can't both type in
    if not conn.execute("UPDATE asks SET status='sent', ts=datetime('now','localtime') WHERE id=? AND status='pending' AND NOT EXISTS "
                        "(SELECT 1 FROM asks WHERE to_sid=? AND status IN ('sent','received'))",
                        (a["id"], a["to_sid"])).rowcount:
        conn.commit()
        return False
    conn.commit()
    back = "They're waiting on your answer, so answer rather than asking them back."
    msg = (f"[moa · message from {a['from_name']} · ask #{a['id']}]\n{a['text']}\n\n(Your final message this turn goes back to "
           f"{a['from_name']} as the answer. {back})")
    ok = (tmux("load-buffer", "-b", "moa-ask", "-", input=msg).returncode == 0
          and tmux("paste-buffer", "-p", "-d", "-b", "moa-ask", "-t", s["pane"]).returncode == 0)
    if ok:
        time.sleep(0.5)  # let the paste land before Enter submits it
        ok = tmux("send-keys", "-t", s["pane"], "Enter").returncode == 0
    if not ok:
        fail(conn, a["id"], "couldn't type it into their pane")
    return ok


def fail(conn, ask_id, why, status="failed", was="sent"):
    n = conn.execute("UPDATE asks SET status=?, reply=? WHERE id=? AND status=?", (status, why, ask_id, was)).rowcount
    conn.commit()
    return n


def ask(conn, q, text, timeout):
    me_sid, me_name = caller(conn)
    if q.lower() == "moa":
        if me_sid == "moa":
            sys.exit("that's you")
        to = "moa"
    else:
        live = [r for r in candidates(conn, q) if r["state"] in LIVE]  # an ended session with the same title isn't a match
        s = live[0] if len(live) == 1 else find(conn, q)
        if s["session_id"] == me_sid:
            sys.exit("that's you")
        if not reachable(s):
            sys.exit(f"{label(s)} isn't running in moa ({s['state']}); `moactl resume {s['session_id'][:8]}` starts it")
        to = s["session_id"]
    if me_sid and conn.execute("SELECT 1 FROM asks WHERE from_sid=? AND to_sid=? AND status IN ('pending','sent','received')",
                               (to, me_sid)).fetchone():
        sys.exit("they're waiting on your answer to their own message, so they can't take one now: "
                 "put what you need from them in your final message instead")
    cur = conn.execute("INSERT INTO asks(from_sid,from_name,to_sid,text) VALUES(?,?,?,?)", (me_sid, me_name, to, text))
    conn.commit()
    wait_reply(conn, cur.lastrowid, timeout)


def wait_reply(conn, ask_id, timeout):
    """Deliver the ask once its peer is free (moa's own are delivered by the console), then
    print the reply. Gives up after `timeout` seconds; `moactl reply ID` waits again."""
    end, typed = time.monotonic() + timeout, None  # typed: when this process typed it in
    while True:
        a = conn.execute("SELECT * FROM asks WHERE id=?", (ask_id,)).fetchone()
        if not a:
            sys.exit(f"no ask #{ask_id}")
        if a["status"] == "answered":
            print(a["reply"] or "(they finished without a reply)")
            return
        if a["status"] in ("failed", "dropped"):
            sys.exit(f"ask #{ask_id} {a['status']}: {a['reply'] or 'no reason recorded'}. Ask again.")
        if a["to_sid"] != "moa":
            s = conn.execute(SESS + "WHERE s.session_id=?", (a["to_sid"],)).fetchone()
            if not s or not reachable(s):
                if a["status"] in ("pending", "sent", "received"):  # an unresolved one would block a resumed peer for good
                    fail(conn, ask_id, "they stopped running", status="dropped" if a["status"] == "pending" else "failed",
                         was=a["status"])
                sys.exit(f"{label(s) if s else a['to_sid'][:8]} stopped running; ask #{ask_id} "
                         + ("wasn't delivered" if a["status"] == "pending" else "got no reply"))
            if a["status"] == "pending" and s["state"] == "idle" and deliver(conn, a, s):
                typed = time.monotonic()
                continue
            if a["status"] == "sent" and typed and time.monotonic() - typed > LANDED:
                # the paste never became a prompt (a dialog or copy-mode had the pane): free the peer for others
                fail(conn, ask_id, "it never arrived as their prompt (something else had their pane)")
                continue
        if time.monotonic() > end:
            if a["status"] == "pending" and a["to_sid"] != "moa":  # nobody would type it in once we stop waiting
                if not fail(conn, ask_id, "they stayed busy until the wait ran out", status="dropped", was="pending"):
                    continue  # another `moactl reply` typed it in meanwhile
                sys.exit(f"ask #{ask_id} wasn't delivered: they stayed busy for {timeout} s. Ask again later.")
            how = "they're working on it" if a["status"] in ("sent", "received") else "moa's busy; it goes in when it's free"
            print(f"no reply yet to ask #{ask_id} ({how}). `moactl reply {ask_id}` waits for it again.")
            return
        time.sleep(1)


def asks_ls(conn):
    for a in conn.execute("SELECT a.*, coalesce(s.title, a.to_sid) AS to_name FROM asks a "
                          "LEFT JOIN sessions s ON s.session_id=a.to_sid ORDER BY a.id DESC LIMIT 20"):
        print(f"#{a['id']} {a['ts'][5:16]}  {a['from_name']} → {a['to_name']}  {a['status']}: "
              + " ".join(a["text"].split())[:80])


# --- sleep: idle sessions give their memory back, and stay one resume away ------------------
def put_to_sleep(conn, s):
    """Close a session's agent and mark it asleep, resumable as ever (`moactl resume`). The
    mark is a column of its own (asleep_at), not a state: killing the pane makes the
    SessionEnd hook write state='ended', which would race a state written here. Resuming
    clears it (here, and the hook's SessionStart). False if it isn't running in moa."""
    panel = layout.panel_of(s["pane"]) if reachable(s) else None
    if panel not in ("", "agent") or not runs_in_pane(s):  # not running here, or a stale pane ID (a panel, another agent)
        return False
    if panel == "agent":  # in the cockpit's agent spot: the spot stays, empty
        tmux("respawn-pane", "-k", "-t", s["pane"], layout.EMPTY_SLOT)
        layout.tag(s["pane"], "agent-slot")
    else:
        tmux("kill-pane", "-t", s["pane"])
    conn.execute("UPDATE sessions SET state='ended', asleep_at=datetime('now','localtime') WHERE session_id=?",
                 (s["session_id"],))
    conn.commit()
    return True


def drowsy(conn, hours):
    """The sessions that can go to sleep: idle for `hours` (no prompt, reply or ping since),
    in an agent window of their own on moa's server, and nothing you'd miss: not working or
    waiting on you, no reply you haven't seen (the status panel's "done") or question, not
    on screen (the window you're in, or the cockpit's agent spot), not in an open ask.
    The pane has to run this session's agent: pane IDs restart with the tmux server, and a
    session that died without its SessionEnd still says idle, holding an ID that now
    belongs to someone else's pane."""
    rows = live(conn)[0]
    newest = {}  # pane → the session that last reported from it
    for s in rows:
        if s["pane"] and (s["pane"] not in newest or (s["last_seen"] or "") > (newest[s["pane"]]["last_seen"] or "")):
            newest[s["pane"]] = s
    panes = {}
    for line in tmux("list-panes", "-a", "-F", "#{@moa_panel}\t#{@moa_term}\t#{&&:#{window_active},#{session_attached}}\t"
                     + PANE_FMT).stdout.splitlines():
        panel, term, shown, info = (line.split("\t", 3) + ["", "", ""])[:4]
        panes[info.split("\t")[0]] = (panel, term, shown, info)
    busy = {sid for r in conn.execute("SELECT from_sid, to_sid FROM asks WHERE status IN ('pending','sent','received')")
            for sid in r}
    cutoff = time.time() - hours * 3600
    out = []
    for s in rows:
        panel, term, shown, info = panes.get(s["pane"] or "", ("-", "", "", ""))
        if (s["show"] != "idle" or panel or term or shown == "1" or s["session_id"] in busy
                or newest.get(s["pane"]) is not s or max(epoch(s["last_seen"]), epoch(s["since"])) > cutoff
                or not reachable(s)):
            continue
        if runs_in_pane(s, info):
            out.append(s)
    return out


def runs_in_pane(s, info=None):
    """The session's pane runs its agent, not someone else's that inherited a reused pane ID."""
    agent = pane_agent(s["pane"], info)
    return bool(agent) and (agent["sid"] or s["session_id"]) == s["session_id"] and related(agent["cwd"], s["cwd"])


def sleep_cmd(conn, ids, hours, dry_run):
    if ids:
        rows = [find(conn, q) for q in ids]
    elif hours is not None:
        rows = drowsy(conn, hours)
    else:
        sys.exit("moactl sleep ID... | moactl sleep --idle HOURS [--dry-run]")
    if dry_run:
        print("\n".join(f"would sleep {s['session_id'][:8]}  {label(s)}" for s in rows) or "nothing to put to sleep")
        return
    slept = [s for s in rows if put_to_sleep(conn, s)]
    for s in rows:
        if s not in slept:
            print(f"{label(s)} isn't running in moa ({s['state']}); nothing to put to sleep")
    if slept:
        names = ", ".join(s["title"] or s["session_id"][:8] for s in slept)
        log_ui(conn, f"put {len(slept)} session{'s' if len(slept) > 1 else ''} to sleep: {names}")
        print(f"asleep: {names} (resumable: moactl resume ID)")
    elif not ids:
        print("nothing to put to sleep")


def main():
    ap = argparse.ArgumentParser(prog="moactl")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("ls"); p.add_argument("--all", action="store_true"); p.add_argument("--json", action="store_true")
    p = sub.add_parser("setup", help="install moa's tracking hook in your Claude accounts (asks; no args = interactive)")
    p.add_argument("op", nargs="?", choices=["list", "install", "only", "remove", "skip"])
    p.add_argument("name", nargs="?"); p.add_argument("folders", nargs="*"); p.add_argument("--json", action="store_true")
    p.add_argument("--brain", metavar="BRAIN", help="with install: the account's sessions go to this brain's moa (moa BRAIN)")
    p.add_argument("--except", dest="exceptions", nargs="+", metavar="FOLDER",
                   help="with --brain: sessions in these folders stay with the default moa")
    p = sub.add_parser("open"); p.add_argument("path"); p.add_argument("--account", default=os.environ.get("MOA_OPEN_ACCOUNT") or "claude-lead")
    p.add_argument("--title"); p.add_argument("--group"); p.add_argument("--prompt"); p.add_argument("--bg", action="store_true")
    p = sub.add_parser("resume"); p.add_argument("id"); p.add_argument("--bg", action="store_true")
    p = sub.add_parser("close"); p.add_argument("id")
    p = sub.add_parser("sleep", help="close sessions' agents to free memory; they stay resumable (moactl resume) and show as 💤")
    p.add_argument("ids", nargs="*"); p.add_argument("--dry-run", action="store_true")
    p.add_argument("--idle", type=float, metavar="HOURS", help="every session idle that long (what the console does on its own)")
    p = sub.add_parser("term", help="a plain shell in a new window (default folder: the agent you're looking at, else ~)")
    p.add_argument("path", nargs="?"); p.add_argument("--title"); p.add_argument("--pane", help="the focused pane (tmux passes it)")
    p = sub.add_parser("archive"); p.add_argument("ids", nargs="+")
    sub.add_parser("import-herdr")
    p = sub.add_parser("adopt"); p.add_argument("pane"); p.add_argument("--title"); p.add_argument("--group"); p.add_argument("--id")
    p = sub.add_parser("focus"); p.add_argument("q")
    p = sub.add_parser("goto"); p.add_argument("panel")
    p = sub.add_parser("layout"); p.add_argument("op", choices=["list", "show", "apply", "set"]); p.add_argument("args", nargs="*")
    sub.add_parser("brief"); sub.add_parser("attention")
    p = sub.add_parser("find"); p.add_argument("q")
    p = sub.add_parser("project", help="show a project; with --add PATH, make that folder a new project")
    p.add_argument("name"); p.add_argument("--add", metavar="PATH"); p.add_argument("--summary")
    p = sub.add_parser("set"); p.add_argument("id"); p.add_argument("--title"); p.add_argument("--group")
    p = sub.add_parser("group"); p.add_argument("op", choices=["add", "rename", "rm"]); p.add_argument("args", nargs="+")
    p = sub.add_parser("note"); p.add_argument("text"); p.add_argument("--project"); p.add_argument("--kind", default="fact")
    p = sub.add_parser("suggest", help="propose something to Eliraz; with --do, a button that does it")
    p.add_argument("text"); p.add_argument("--do", action="append", default=[], metavar="MOACTL-ARGS",
                                           help='what "yes" runs, as moactl arguments ("task set 12 --due fri"); repeatable')
    p = sub.add_parser("suggestion", help="answer a suggestion: yes runs its actions")
    p.add_argument("op", choices=["yes", "no", "later", "ls"]); p.add_argument("id", nargs="?", type=int)
    p = sub.add_parser("task"); p.add_argument("op", choices=["add", "set", "ls"]); p.add_argument("args", nargs="*")
    p.add_argument("--today", action="store_true"); p.add_argument("--not-today", action="store_true")
    p.add_argument("--status"); p.add_argument("--blocked-on"); p.add_argument("--title")
    p.add_argument("--who", choices=["you", "agent", "other"]); p.add_argument("--due", help="YYYY-MM-DD, today, tomorrow, +N (days) or a weekday (fri, friday)")
    p.add_argument("--session"); p.add_argument("--note"); p.add_argument("--all", action="store_true")
    p.add_argument("--priority", choices=["p0", "p1", "p2", ""], help="p0 urgent, p1 important, p2 normal; \"\" clears it")
    p = sub.add_parser("peers", help="the agents you can ask: moa and the other sessions running in moa")
    p.add_argument("--json", action="store_true")
    p = sub.add_parser("ask", help="send another agent (a session, or moa) a message and wait for its reply",
                       description="Send another agent a message and wait for its reply (its final message that turn). "
                                   "Are you an agent? `moactl peers` lists who you can ask. The message is typed into "
                                   "their session once they're free, marked as from you. Waits up to 9 minutes (give the shell command a 10-minute timeout); if it "
                                   "gives up, `moactl reply ID` waits again.")
    p.add_argument("to", help="moa, or a session's ID prefix, title or project"); p.add_argument("text")
    p.add_argument("--timeout", type=int, default=ASK_WAIT, help="seconds to wait (default 540)")
    p = sub.add_parser("reply", help="wait again for the reply to an ask"); p.add_argument("id", type=int)
    p.add_argument("--timeout", type=int, default=ASK_WAIT)
    sub.add_parser("asks", help="recent asks between agents")
    a = ap.parse_args()
    conn = connect()
    pid = lambda name: conn.execute("SELECT id FROM projects WHERE lower(name)=lower(?)", (name,)).fetchone()
    if a.cmd == "goto":
        def find_panel():
            return next((x["pane"] for x in layout.panes(layout.WINDOW) if x["panel"] in (a.panel, "agent-slot" if a.panel == "agent" else a.panel)),
                        None) if layout.window_exists() else None
        pane = find_panel()
        cfg = layout.load()
        node = cfg.get("layouts", {}).get(cfg.get("current", "cockpit"))
        if not pane and (node is None or a.panel in layout.leaves(node)):
            try:
                layout.apply(session_title=title_of(conn))
            except (ValueError, KeyError) as exc:
                sys.exit(f"layout: {exc}")
            pane = find_panel()
        if not pane:
            sys.exit(f"the current layout has no {a.panel} panel (moactl layout list)")
        tmux("select-window", "-t", layout.WINDOW)
        tmux("select-pane", "-t", pane)
        print(a.panel)
    elif a.cmd == "focus":
        resume(conn, find(conn, a.q), False)
    elif a.cmd == "layout":
        try:
            if a.op == "list":
                cfg = layout.load()
                for n, node in cfg["layouts"].items():
                    print(f"{'▸' if n == cfg.get('current') else ' '} {n}: {' · '.join(layout.leaves(node))}")
                print(f"(config: {layout.CONFIG})")
            elif a.op == "show":
                print(json.dumps(layout.load()["layouts"][a.args[0]], indent=2))
            elif a.op == "set":
                node = json.loads(" ".join(a.args[1:]))
                layout.validate(node)
                cfg = layout.load()
                cfg["layouts"][a.args[0]] = node
                layout.save(cfg)
                print(f"saved layout {a.args[0]}; apply it with: moactl layout apply {a.args[0]}")
            elif a.op == "apply":
                name = layout.apply(a.args[0] if a.args else None, session_title=title_of(conn))
                log_ui(conn, f"layout → {name}")
                print(f"layout {name} applied")
        except (ValueError, KeyError, IndexError, json.JSONDecodeError) as exc:
            sys.exit(f"layout: {exc}")
    elif a.cmd == "adopt":
        adopt(conn, a.pane, a.title, a.group, a.id)
    elif a.cmd == "brief":
        print(brief(conn))
    elif a.cmd == "attention":
        print("\n".join(attention_lines(conn)) or "nothing needs you right now")
    elif a.cmd == "peers":
        peers(conn, a.json)
    elif a.cmd == "ask":
        ask(conn, a.to, a.text, a.timeout)
    elif a.cmd == "reply":
        wait_reply(conn, a.id, a.timeout)
    elif a.cmd == "asks":
        asks_ls(conn)
    elif a.cmd == "find":
        for r in candidates(conn, a.q)[:15]:
            print(f"{r['session_id'][:8]}  {label(r)}  {r['state']}  {age(r['last_seen'])}  {r['cwd']}")
    elif a.cmd == "project":
        add_project(conn, a.name, a.add, a.summary) if a.add else project(conn, a.name)
    elif a.cmd == "set":
        set_session(conn, find(conn, a.id), a.title, a.group)
        print("ok")
    elif a.cmd == "group":
        group_cmd(conn, a.op, a.args)
    elif a.cmd == "note":
        p = pid(a.project) if a.project else None
        conn.execute("INSERT INTO notes(project_id,kind,text) VALUES(?,?,?)", (p[0] if p else None, a.kind, a.text))
        conn.commit(); print("noted")
    elif a.cmd == "suggest":
        suggest(conn, a.text, a.do)
    elif a.cmd == "suggestion":
        suggestion(conn, a.op, a.id)
    elif a.cmd == "task" and a.op == "ls":
        task_ls(conn, a.args[0] if a.args else None, a.all)
    elif a.cmd == "task" and a.op == "add":
        if len(a.args) < 2:
            sys.exit("moactl task add PROJECT TITLE… [--who you|agent|other] [--due DATE] [--session ID] [--status S]")
        p = pid(a.args[0])
        if not p:
            sys.exit(f"no project {a.args[0]!r} (moactl project NAME to check)")
        cur = conn.execute("INSERT INTO tasks(project_id,title,today,status,who,due,session_id,note,blocked_on,priority) "
                           "VALUES(?,?,?,?,?,?,?,?,?,?)",
                           (p[0], " ".join(a.args[1:]), datetime.date.today().isoformat() if a.today else None,
                            a.status or ("blocked" if a.blocked_on else "todo"), a.who or "you", due_date(a.due),
                            session_id(conn, a.session), a.note, a.blocked_on, a.priority or None))
        conn.commit(); print(f"task #{cur.lastrowid} added")
    elif a.cmd == "task" and a.op == "set":
        if not a.args:
            sys.exit("moactl task set ID [--status S] [--title T] [--who W] [--due D] [--priority P] [--session ID] [--note N]")
        tid = int(a.args[0])
        if not conn.execute("SELECT 1 FROM tasks WHERE id=?", (tid,)).fetchone():
            sys.exit(f"no task #{tid}")
        fields = {"status": a.status, "title": a.title, "who": a.who, "due": due_date(a.due),
                  "session_id": session_id(conn, a.session), "note": a.note, "priority": a.priority}
        if a.title == "":
            sys.exit("--title can't be empty")
        if a.blocked_on is not None:  # "" clears it
            fields["blocked_on"] = a.blocked_on
            if a.blocked_on:
                fields["status"] = a.status or "blocked"
        if a.today or a.not_today:
            fields["today"] = datetime.date.today().isoformat() if a.today else ""
        for col, val in fields.items():
            if val is not None:
                conn.execute(f"UPDATE tasks SET {col}=nullif(?,''), updated_at=datetime('now','localtime') WHERE id=?", (val, tid))
        conn.commit(); print("ok")
    elif a.cmd == "setup":
        setup(conn, a)
    elif a.cmd == "ls":
        ls(conn, a.all, a.json)
    elif a.cmd == "open":
        open_session(conn, a.path, a.account, a.title, a.group, a.prompt, a.bg)
    elif a.cmd == "resume":
        resume(conn, find(conn, a.id), a.bg)
    elif a.cmd == "close":
        close(conn, find(conn, a.id))
    elif a.cmd == "sleep":
        sleep_cmd(conn, a.ids, a.idle, a.dry_run)
    elif a.cmd == "term":
        term(a.path, a.title, a.pane)
    elif a.cmd == "archive":
        for i in a.ids:
            s = find(conn, i)
            conn.execute("UPDATE sessions SET state='archived' WHERE session_id=?", (s["session_id"],))
            log_ui(conn, f"archived {s['title'] or s['session_id'][:8]}")
        conn.commit()
        print(f"archived {len(a.ids)}")
    elif a.cmd == "import-herdr":
        import_herdr(conn)


if __name__ == "__main__":
    main()
