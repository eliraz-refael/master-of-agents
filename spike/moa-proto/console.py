#!/usr/bin/env python3
"""THROWAWAY PROTOTYPE: the console. A feed of what moa says and what tracked
sessions do, plus one input line: /commands run instantly through the brain API
(moactl); anything else goes to moa, the central agent, over ACP (claude-agent-acp on
the claude-lead account). moa's replies render as Markdown; commands show up as
buttons (click, or Tab to one and press Enter). Not product code."""
import curses
import datetime
import json
import locale
import os
import queue
import re
import shlex
import subprocess
import sys
import threading
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from picture import ICON, ago, connect, db_mtime, icon  # noqa: E402
import layout  # noqa: E402

MASTER_DIR = os.path.normpath(os.path.join(HERE, "..", "master-chat", "master"))
MOACTL = os.path.join(HERE, "moactl")
ACCOUNT = os.path.expanduser("~/.claude-lead")
OPENING = ("(The console just opened a new thread for today.) Give me your opening brief: "
           "at most 6 short lines, what's hot, what's waiting on me, what's going stale, "
           "then propose 1-3 goals for today and ask which.")
WAKE = "(Automatic wake-up from the console"
SETTLE, GAP = (int(os.environ.get(v, d)) for v, d in (("MOA_WAKE_SETTLE", 45), ("MOA_WAKE_GAP", 180)))  # wake moa 45 s after the last report, at most every 3 min
COMMANDS = [  # name, arguments, what it does
    ("attention", "", "what needs you, ranked (instant, no LLM)"),
    ("brief", "", "moa's brief"),
    ("ls", "", "live sessions"),
    ("open", "PATH [title…]", "start an agent there and switch to it"),
    ("resume", "Q", "resume or jump to a session (title, project or ID)"),
    ("focus", "Q", "bring a session into the cockpit's agent spot"),
    ("layout", "[NAME]", "list layouts, or switch to one"),
    ("close", "Q", "close a session's agent"),
    ("archive", "Q", "archive a session"),
    ("new", "", "start a fresh moa thread"),
    ("auto", "[on|off]", "moa reads session reports on its own (default on)"),
    ("help", "", "commands and keys"),
    ("quit", "", "close the console"),
]
NEEDS_ARG = {n for n, a, _ in COMMANDS if a and not a.startswith("[")}
BAR = [("attention", "/attention"), ("brief", "/brief"), ("ls", "/ls"), ("open…", "!fill /open "),
       ("resume…", "!fill /resume "), ("layout", "/layout"), ("new", "/new"), ("help", "/help")]
KEYS = """↵ send · Esc cancels moa's turn · PgUp/PgDn or the wheel scroll
Tab hops between buttons, ↵ presses one; clicking works too
/ opens the command list: ↑↓ pick, Tab completes, ↵ runs
Ctrl-a anywhere: the moa menu (Ctrl-a a: a real Ctrl-a)
when moa asks permission: y once · a always · n no
hold Shift to select text with the mouse"""

CMD_RE = re.compile(r"/(" + "|".join(n for n, _, _ in COMMANDS) + r")(\s|$)")
MOACTL_RE = re.compile(r"(?:moactl )(resume|focus|open|close|archive) (\S.*)")
INLINE = re.compile(r"(\*\*[^*\n]+\*\*|`[^`\n]+`|⟦[^⟧|]+\|[^⟧]+⟧|\[[^\]\n]+\]\([^)\n]+\)|[●⚠▸◐✓✗○◌])")
# moactl prints "→ moactl resume ID"; the console shows a button instead
ARROW_RE = re.compile(r"\s*→ moactl (resume|focus) (\S+)")


def seg_len(segs):
    return sum(len(s[0]) for s in segs)


SESSION_NAMES = {}  # session ID → the name a button shows (the console keeps it fresh from the brain)
NAMES_GEN = [0]     # bumped when SESSION_NAMES changes, so cached feed rows re-render with the new names


def button_label(cmd):
    """What a command button says: a session by its name, not its ID ("↩ Lantern · Rust scaffold")."""
    verb, _, arg = cmd[1:].partition(" ")
    arg = arg.strip().lower()
    if verb in ("resume", "focus", "close", "archive") and len(arg) >= 6 and re.fullmatch(r"[0-9a-f-]+", arg):
        names = [n for sid, n in SESSION_NAMES.items() if sid.startswith(arg)]
        if len(names) == 1:  # an ambiguous prefix keeps its ID, as moactl would refuse it
            name = names[0] if len(names[0]) <= 40 else names[0][:39] + "…"
            return f"↩ {name}" if verb == "resume" else f"{verb} {name}"
    return cmd


def as_command(code):
    """A console command for an inline `code` span, if it is one (`/resume x`, `moactl focus y`)."""
    if CMD_RE.match(code + " "):
        return code
    m = MOACTL_RE.fullmatch(code)
    return f"/{m.group(1)} {m.group(2)}" if m and " --" not in f" {m.group(2)}" else None


class Style:
    def __init__(self):
        grey = 8 if curses.COLORS > 8 else curses.COLOR_WHITE
        pairs = [(curses.COLOR_CYAN, -1), (grey, -1), (curses.COLOR_YELLOW, -1), (curses.COLOR_GREEN, -1),
                 (curses.COLOR_BLUE, -1), (curses.COLOR_RED, -1), (curses.COLOR_MAGENTA, -1),
                 (curses.COLOR_BLACK, curses.COLOR_CYAN), (curses.COLOR_BLACK, curses.COLOR_YELLOW),
                 (curses.COLOR_BLACK, curses.COLOR_MAGENTA), (curses.COLOR_BLACK, curses.COLOR_WHITE)]
        for i, (fg, bg) in enumerate(pairs, start=1):
            curses.init_pair(i, fg, bg)
        P = curses.color_pair
        B, I = curses.A_BOLD, getattr(curses, "A_ITALIC", 0)
        self.head, self.dim, self.warn, self.ok = P(1) | B, P(2), P(3), P(4)
        self.you, self.err, self.brand, self.code = P(5) | B, P(6), P(7), P(1)
        self.chip, self.chipwarn, self.pill, self.focus = P(8), P(9), P(10) | B, P(11) | B
        self.italic = I
        self.glyph = {"●": self.ok, "⚠": self.warn | B, "▸": self.brand, "◐": self.head, "✓": self.ok,
                      "✗": self.err, "○": self.dim, "◌": self.dim}


# text → rows of styled segments: (text, attr) or (text, attr, command) for a button
def inline(text, attr, S):
    out = []
    for part in INLINE.split(text):
        if not part:
            continue
        if len(part) > 4 and part.startswith("**") and part.endswith("**"):
            out.append((part[2:-2], attr | curses.A_BOLD))
        elif len(part) > 2 and part[0] == "`" and part[-1] == "`":
            cmd = as_command(part[1:-1])
            out.append((f" {button_label(cmd)} ", S.chip, cmd) if cmd else (part[1:-1], S.code))
        elif part[0] == "⟦":
            label, cmd = part[1:-1].split("|", 1)
            out.append((f" ↩ {label} ", S.chip, cmd))
        elif part[0] == "[" and "](" in part:
            out.append((part[1:part.index("](")], attr | curses.A_UNDERLINE))
        elif part in S.glyph:
            out.append((part, S.glyph[part]))
        else:
            out.append((part, attr))
    return out


def wrap(spans, width, lead=(), rest=()):
    """Word-wrap styled spans; `lead` and `rest` are the prefixes of the first and later rows."""
    rows, row, used, fresh = [], list(lead), seg_len(lead), True

    def newline():
        nonlocal row, used, fresh
        rows.append(row)
        row, used, fresh = list(rest), seg_len(rest), True

    for seg in spans:
        if len(seg) > 2:  # a button never splits
            if used + len(seg[0]) > width and not fresh:
                newline()
            row.append(seg)
            used, fresh = used + len(seg[0]), False
            continue
        text, attr = seg
        for word in re.findall(r"\s*\S+|\s+", text):
            if fresh and rows:
                word = word.lstrip()
            if used + len(word) > width and not fresh:
                newline()
                word = word.lstrip()
            while word and used + len(word) > width:  # longer than a whole row: hard cut
                cut = max(1, width - used)
                row.append((word[:cut], attr))
                newline()
                word = word[cut:]
            if word:
                row.append((word, attr))
                used, fresh = used + len(word), False
    if not fresh or not rows:
        rows.append(row)
    return rows


def fit(segs, width):
    """Cut a row of segments to `width` cells, ending in … if anything was cut."""
    out, used = [], 0
    for s in segs:
        if used + len(s[0]) <= width:
            out.append(s)
            used += len(s[0])
        else:
            if width - used > 1:
                out.append((s[0][:width - used - 1] + "…", s[1]))
            break
    return out


def plain(md):
    return re.sub(r"\*\*|`", "", md)


def table(lines, width, S):
    cells = [[c.strip() for c in ln.strip().strip("|").split("|")] for ln in lines]
    cells = [r for r in cells if not all(re.fullmatch(r":?-{2,}:?", c) for c in r if c)]
    if not cells:
        return []
    n = max(len(r) for r in cells)
    cells = [[plain(c) for c in r] + [""] * (n - len(r)) for r in cells]
    widths = [max(len(r[j]) for r in cells) for j in range(n)]
    if sum(widths) + 3 * (n - 1) <= width:
        out = []
        for k, r in enumerate(cells):
            row = []
            for j, c in enumerate(r):
                if j:
                    row.append((" │ ", S.dim))
                row.append((c.ljust(widths[j]), curses.A_BOLD if k == 0 else 0))
            out.append(row)
            if k == 0 and len(cells) > 1:
                out.append([("─┼─".join("─" * w for w in widths), S.dim)])
        return out
    out = []  # too wide for the pane: one bullet per row
    for r in cells[1:] or cells:
        out += wrap([(r[0], curses.A_BOLD)] + [(f" · {c}", 0) for c in r[1:] if c], width, [("• ", S.brand)], [("  ", 0)])
    return out


def markdown(text, width, S, base=0):
    rows, lines, i, in_code = [], text.split("\n"), 0, False

    def gap():
        if rows and rows[-1]:
            rows.append([])

    while i < len(lines):
        ln = lines[i]
        s = ln.strip()
        i += 1
        if s.startswith("```"):
            in_code = not in_code
            continue
        if in_code:
            rows.append([("▏ ", S.dim), (ln[:max(1, width - 2)], S.code)])
            continue
        if not s:
            gap()
            continue
        if s.startswith("|") and s.count("|") >= 2:
            tbl = [s]
            while i < len(lines) and lines[i].strip().startswith("|"):
                tbl.append(lines[i].strip())
                i += 1
            rows += table(tbl, width, S)
            continue
        m = re.match(r"(#{1,6})\s+(.*)", s)
        if m:
            gap()
            if len(m.group(1)) <= 2:
                rows += wrap(inline(m.group(2), S.head, S), width, [("▍", S.brand)], [(" ", 0)])
            else:
                rows += wrap(inline(m.group(2), curses.A_BOLD, S), width)
            continue
        if re.fullmatch(r"(-\s*){3,}|(\*\s*){3,}|(_\s*){3,}", s):
            rows.append([("─" * min(width, 40), S.dim)])
            continue
        m = re.match(r"(\s*)([-*+]|\d+[.)])\s+(.*)", ln)
        if m:
            pad = "  " * min(len(m.group(1).expandtabs(4)) // 2, 4)
            mark, body = m.group(2), m.group(3)
            bullet, battr = (f"{mark} ", S.brand) if mark[0].isdigit() else ("•◦▪"[min(len(pad) // 2, 2)] + " ", S.brand)
            check = re.match(r"\[([ xX])\]\s+(.*)", body)
            if check:
                bullet, battr, body = ("☑ ", S.ok) if check.group(1) != " " else ("☐ ", S.dim), check.group(2)
            elif body[:1] in S.glyph:  # "- ● Website …": the status glyph is the bullet
                bullet, battr, body = body[0] + " ", S.glyph[body[0]], body[1:].lstrip()
            rows += wrap(inline(body, base, S), width, [(pad, 0), (bullet, battr)], [(pad + " " * len(bullet), 0)])
            continue
        if s.startswith(">"):
            q = S.dim | S.italic
            rows += wrap(inline(s[1:].strip(), q, S), width, [("▎ ", S.dim)], [("▎ ", S.dim)])
            continue
        rows += wrap(inline(s, base, S), width)
    while rows and not rows[-1]:
        rows.pop()
    return rows


class Acp:
    def __init__(self, inbox):
        env = dict(os.environ, CLAUDE_CONFIG_DIR=ACCOUNT)
        self.p = subprocess.Popen(["claude-agent-acp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, bufsize=1, cwd=MASTER_DIR, env=env)
        self.inbox, self.ids = inbox, 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            try:
                self.inbox.put(json.loads(line))
            except ValueError:
                pass
        self.inbox.put({"_exit": True})

    def send(self, obj):
        try:
            self.p.stdin.write(json.dumps(obj) + "\n")
            self.p.stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def request(self, method, params):
        self.ids += 1
        self.send({"jsonrpc": "2.0", "id": self.ids, "method": method, "params": params})
        return self.ids

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def respond(self, rid, result=None, error=None):
        msg = {"jsonrpc": "2.0", "id": rid}
        msg.update({"error": error} if error else {"result": result})
        self.send(msg)


class Console:
    def __init__(self, scr, S):
        self.scr, self.S, self.conn = scr, S, connect()
        self.load_names()
        self.feed, self.tools = [], {}
        self.inbox = queue.Queue()
        self.acp = Acp(self.inbox)
        self.pending = {}          # request id -> what it was
        self.sid = None
        self.busy = False
        self.perms = []            # pending session/request_permission requests and their feed entries, oldest first
        self.buf, self.cur = [], 0
        self.history, self.hpos = [], 0
        self.scroll = 0
        self.streaming = None      # the agent entry being streamed into
        self.replaying = False     # session/load is replaying today's thread
        self.turn = 0              # moa's replies group by turn
        self.auto_turns = set()    # turns moa took on its own (woken by session reports)
        self.hide_user = False     # replay: skipping a hidden prompt's chunks
        self.reports, self.report_at, self.last_wake, self.auto = [], 0.0, -GAP, True
        self.hits = []             # buttons on screen: (y, x0, x1, command), in Tab order
        self.focus = None          # the focused hit (y, x0, x1, command), while Tab-hopping
        self.pal, self.pal_key, self.pal_items, self.pal_hits = 0, None, [], []
        self.pal_moved = False     # ↑↓ picked a row, so ↵ runs it even with nothing typed after the command
        self.cursor = self.conn.execute("SELECT coalesce(max(id),0) FROM events").fetchone()[0]
        self.seen = db_mtime()
        self._last_err = (None, 0.0)  # the last error line shown, and when
        self.pending[self.acp.request("initialize", {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False}})] = "init"
        self.add("sys", "connecting to moa…")
        self.run_ctl("attention", title="needs you right now")

    # feed
    def add(self, kind, text, **kw):
        if kind == "you":
            self.turn += 1
        # replayed history has no real time (ACP doesn't send one), so it's marked "earlier"
        ts = "earlier" if self.replaying else datetime.datetime.now().strftime("%H:%M")
        e = {"kind": kind, "text": text, "ts": ts, "turn": self.turn, **kw}
        self.feed.append(e)
        del self.feed[:-500]
        self.streaming = None if kind != "agent" else e
        return e

    def error(self, exc):
        """Log the traceback; show an error line, but the same one at most every 30 s."""
        traceback.print_exc()
        text, now = f"console error: {exc!r} (details in console.err)", time.monotonic()
        if self._last_err[0] != text or now - self._last_err[1] > 30:
            self.add("err", text)
            self._last_err = (text, now)

    def run_ctl(self, *args, title=None):
        title = title or "/" + " ".join(args)
        if args == ("ls",):
            r = subprocess.run([MOACTL, "ls", "--json"], capture_output=True, text=True)
            try:
                self.add("ctl", "", title=title, sessions=json.loads(r.stdout))
                return
            except ValueError:
                pass
        r = subprocess.run([MOACTL, *args], capture_output=True, text=True)
        self.add("ctl", (r.stdout + r.stderr).strip() or "ok", title=title)

    # moa (ACP)
    def thread_meta(self, value=None):
        if value is None:
            row = self.conn.execute("SELECT value FROM meta WHERE key='console_thread'").fetchone()
            return json.loads(row[0]) if row else {}
        self.conn.execute("INSERT INTO meta(key,value) VALUES('console_thread',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                          (json.dumps(value),))
        self.conn.commit()

    def new_thread(self):
        self.pending[self.acp.request("session/new", {"cwd": MASTER_DIR, "mcpServers": []})] = "new"

    def prompt(self, text, show=True, auto=False):
        if not self.sid:
            self.add("sys", "moa isn't connected yet")
            return
        if show:
            self.add("you", text)
        else:
            self.turn += 1
        if auto:
            self.auto_turns.add(self.turn)
        self.busy = True
        self.pending[self.acp.request("session/prompt", {"sessionId": self.sid, "prompt": [{"type": "text", "text": text}]})] = "prompt"

    def handle(self, m):
        if m.get("_exit"):
            self.add("err", "moa's agent process exited; reopen the console from the menu (Ctrl-a)")
            self.busy = False
            return
        if "method" in m and "id" in m:                       # a request from the agent
            if m["method"] == "session/request_permission":
                tc = m["params"].get("toolCall", {})
                self.perms.append((m, self.add("perm", tc.get("title") or "this", rid=m["id"])))
                self.focus = None
            else:
                self.acp.respond(m["id"], error={"code": -32601, "message": "not supported by the console"})
            return
        if "id" in m:                                          # a response to us
            what = self.pending.pop(m["id"], None)
            if what == "load":
                self.replaying = False
            if "error" in m:
                self.add("err", f"{what}: {m['error'].get('message')}")
                if what == "load":
                    self.new_thread()
                self.busy = False
                return
            r = m.get("result") or {}
            if what == "init":
                t = self.thread_meta()
                if t.get("date") == datetime.date.today().isoformat() and r.get("agentCapabilities", {}).get("loadSession"):
                    self.sid = t["sid"]
                    self.pending[self.acp.request("session/load", {"sessionId": t["sid"], "cwd": MASTER_DIR, "mcpServers": []})] = "load"
                    self.replaying = True
                else:
                    self.new_thread()
            elif what == "new":
                self.sid = r["sessionId"]
                self.thread_meta({"sid": self.sid, "date": datetime.date.today().isoformat()})
                self.add("sys", f"new thread {self.sid[:8]} for today")
                self.prompt(OPENING, show=False)
            elif what == "load":
                self.hide_user = False
                self.add("sys", f"resumed today's thread {self.sid[:8]}: everything above is from earlier")
            elif what == "prompt":
                self.busy = False
                if r.get("stopReason") not in (None, "end_turn"):
                    self.add("sys", f"(turn ended: {r['stopReason']})")
            return
        if m.get("method") == "session/update":
            u = m["params"]["update"]
            kind = u.get("sessionUpdate")
            if kind in ("agent_message_chunk", "user_message_chunk"):
                text = (u.get("content") or {}).get("text", "")
                who = "agent" if kind == "agent_message_chunk" else "you"
                if who == "you" and (text.startswith(OPENING[:30]) or text.startswith(WAKE)):
                    self.turn += 1  # a hidden prompt being replayed: skip it and the rest of its chunks
                    if text.startswith(WAKE):
                        self.auto_turns.add(self.turn)
                    self.hide_user = True
                    return
                if who == "you" and self.hide_user:
                    return
                self.hide_user = False
                if self.feed and self.feed[-1]["kind"] == who and (who == "you" or self.streaming is self.feed[-1]):
                    self.feed[-1]["text"] += text
                else:
                    e = self.add(who, text)
                    if who == "agent":
                        self.streaming = e
            elif kind == "tool_call":
                self.hide_user = False
                self.tools[u["toolCallId"]] = self.add("tool", u.get("title") or "tool", status=u.get("status"), raw=u.get("rawInput") or {})
            elif kind == "tool_call_update" and u.get("toolCallId") in self.tools:
                e = self.tools[u["toolCallId"]]
                e.pop("_key", None)
                if u.get("title"):
                    e["text"] = u["title"]
                if u.get("status"):
                    e["status"] = u["status"]
                if u.get("rawInput"):
                    e["raw"] = u["rawInput"]

    def answer(self, ch, rid=None):
        """Answer the permission request `rid` (the oldest pending one if None)."""
        found = next((p for p in self.perms if rid is None or p[0]["id"] == rid), None)
        if not found:
            return
        msg, entry = found
        want = {"y": "allow_once", "a": "allow_always", "n": "reject_once"}[ch]
        opts = msg["params"]["options"]
        opt = next((o for o in opts if o["kind"] == want), opts[0])
        self.acp.respond(msg["id"], {"outcome": {"outcome": "selected", "optionId": opt["optionId"]}})
        entry["answered"] = opt["name"]
        self.perms.remove(found)

    def load_names(self):
        """Session names for buttons: the title, with the project in front when the title doesn't say it."""
        names = {}
        for r in self.conn.execute("""SELECT s.session_id, s.title, s.grp, s.cwd, p.name AS project FROM sessions s
                                      LEFT JOIN projects p ON p.id=s.project_id"""):
            leaf = (r["grp"] or "").rsplit("/", 1)[-1] or r["project"] or ""
            title = r["title"] or os.path.basename(r["cwd"] or "") or r["session_id"][:8]
            same = not leaf or leaf.lower() in title.lower() or title.lower() in leaf.lower()
            names[r["session_id"]] = title if same else f"{leaf} · {title}"
        if names != SESSION_NAMES:
            SESSION_NAMES.clear()
            SESSION_NAMES.update(names)
            NAMES_GEN[0] += 1

    # sessions reporting in
    def watch_events(self):
        m = db_mtime()
        if m == self.seen:
            return
        self.load_names()
        rows = self.conn.execute("""SELECT e.*, coalesce(s.title, p.name) AS name, p.name AS project FROM events e
            LEFT JOIN sessions s ON s.session_id=e.session_id LEFT JOIN projects p ON p.id=s.project_id
            WHERE e.id > ? ORDER BY e.id""", (self.cursor,)).fetchall()
        for e in rows:
            name = e["name"] or os.path.basename(e["cwd"] or "") or "?"
            sid = (e["session_id"] or "")[:8]
            text = " ".join((e["text"] or "").split())
            if e["kind"] in ("stop", "waiting", "start", "end"):
                self.add("event", text, what=e["kind"], name=name, sid=sid)
            if e["kind"] in ("stop", "waiting") and e["cwd"] != MASTER_DIR and (e["text"] or "").strip():
                old = [r for r in self.reports if r["sid"] == sid and r["kind"] == e["kind"]]
                self.reports = [r for r in self.reports if r not in old]
                self.reports.append({"sid": sid, "name": name, "project": e["project"], "kind": e["kind"],
                                     "ts": (e["ts"] or "")[11:16], "text": (e["text"] or "").strip()[:2600],
                                     "at": old[0]["at"] if old else time.monotonic()})  # a newer report keeps its place in line
                self.report_at = time.monotonic()
            self.cursor = e["id"]  # after it was shown, so a failing row is retried, not skipped
        self.seen = m  # only after every row was shown, so a failure retries instead of dropping events

    def tick(self):
        """Wake moa with the session reports once they settle, so it keeps the big picture."""
        now = time.monotonic()
        if not (self.auto and self.reports and self.sid and not self.busy and not self.perms and not self.replaying):
            return
        settling = now - self.report_at < SETTLE and now - min(r["at"] for r in self.reports) < GAP  # a steady stream still wakes moa
        if settling or now - self.last_wake < GAP:
            return
        batch, self.reports, self.last_wake = self.reports[:8], self.reports[8:], now  # the rest go in the next wake
        words = {"stop": "finished a turn", "waiting": "needs Eliraz"}
        parts = [f"### {r['name']} · project {r['project'] or '(none)'} · session {r['sid']} · {words[r['kind']]} at {r['ts']}\n{r['text']}"
                 for r in batch]
        text = (f"{WAKE}, not typed by Eliraz.) {len(batch)} session report(s) since your last look:\n\n" + "\n\n".join(parts) +
                "\n\nUpdate the big picture (see \"Wake-ups\" in your instructions): `moactl task ls`, then add, update or "
                "close tasks for these projects. Reply in ONE short line saying what changed. Only if something needs "
                "Eliraz now, say it in at most 3 lines and end with the button.")
        self.add("sys", f"moa is reading {len(batch)} session report{'s' if len(batch) > 1 else ''}…")
        self.prompt(text, show=False, auto=True)

    # input
    def submit(self, line):
        if not line.strip():
            return
        self.history.append(line)
        self.hpos = len(self.history)
        self.scroll = 0
        if not line.startswith("/"):
            self.prompt(line)
            return
        try:
            parts = shlex.split(line[1:])
        except ValueError:  # an unbalanced quote, e.g. /resume Eliraz's idea
            parts = line[1:].split()
        if not parts:
            return
        cmd, *args = parts
        if cmd == "help":
            self.add("help", "", title="commands")
        elif cmd == "quit":
            raise SystemExit
        elif cmd == "new":
            self.add("sys", "starting a fresh thread…")
            self.new_thread()
        elif cmd == "auto":
            self.auto = args[0] != "off" if args else not self.auto
            self.add("sys", "moa reads session reports on its own" if self.auto else
                     "moa won't read session reports until /auto on")
        elif cmd in ("attention", "brief", "ls"):
            self.run_ctl(cmd)
        elif cmd == "open" and args:
            self.run_ctl("open", args[0], *(["--title", " ".join(args[1:])] if args[1:] else []))
        elif cmd == "layout":
            self.run_ctl("layout", *(["apply", args[0]] if args else ["list"]))
        elif cmd in ("resume", "close", "archive", "focus") and args:
            self.run_ctl(cmd, " ".join(args))
        elif cmd in NEEDS_ARG:
            self.fill(f"/{cmd} ")
        else:
            self.add("sys", f"unknown command /{cmd} (try /help)")

    def fill(self, text):
        self.buf, self.cur, self.focus, self.pal, self.pal_moved = list(text), len(text), None, 0, False

    def press(self, cmd):
        """A button was clicked or Entered."""
        self.focus = None
        if cmd.startswith("!perm "):
            _, rid, ch = cmd.split(" ")
            self.answer(ch, json.loads(rid))
        elif cmd.startswith("!fill "):
            self.fill(cmd[6:])
        else:
            self.submit(cmd)

    def palette(self):
        """The command list under what's being typed: (label, detail, insert) rows."""
        text = "".join(self.buf)
        if text == self.pal_key:
            return self.pal_items
        items = []
        if text.startswith("/") and " " not in text:
            items = [(f"/{n}", f"{a}  {d}" if a else d, f"/{n} " if a else f"/{n}")
                     for n, a, d in COMMANDS if n.startswith(text[1:])]
        elif text.startswith("/"):
            cmd, _, q = text[1:].partition(" ")
            q = q.strip().lower()
            if cmd in ("resume", "focus", "close", "archive"):
                rows = self.conn.execute("SELECT session_id, title, grp, state, last_seen FROM sessions "
                                         "WHERE state != 'archived' ORDER BY last_seen DESC").fetchall()
                items = [(f"{ICON.get(r['state'], '·')} {r['title'] or r['session_id'][:8]}",
                          f"{r['grp'] or ''}  {ago(r['last_seen'])}", f"/{cmd} {r['session_id'][:8]}")
                         for r in rows if q in f"{r['title']} {r['grp']} {r['session_id']}".lower()]
            elif cmd == "layout":
                try:
                    cfg = layout.load()
                    items = [(n, "current" if n == cfg.get("current") else " · ".join(layout.leaves(node)), f"/layout {n}")
                             for n, node in cfg["layouts"].items() if n.startswith(q)]
                except (OSError, ValueError, KeyError):
                    items = []
        self.pal_key, self.pal_items = text, items[:max(8, len(COMMANDS))]
        self.pal = min(self.pal, max(0, len(self.pal_items) - 1))
        return self.pal_items

    def key(self, ch):
        if ch == curses.KEY_MOUSE:
            return self.mouse()
        if self.perms and not self.buf and ch in ("y", "a", "n"):
            return self.answer(ch)
        pal = self.palette()
        if ch in ("\t", curses.KEY_BTAB) and pal:
            self.fill(pal[self.pal][2])
        elif ch in ("\t", curses.KEY_BTAB) and not self.buf and self.hits:
            step = 1 if ch == "\t" else -1
            at = self.hits.index(self.focus) if self.focus in self.hits else None
            self.focus = self.hits[0 if at is None and step == 1 else ((at or 0) + step) % len(self.hits)]
        elif ch in ("\n", "\r", curses.KEY_ENTER):
            if self.focus is not None and not self.buf:
                return self.press(self.focus[3])
            if pal and "".join(self.buf).strip() != pal[self.pal][2].strip() and pal[self.pal][2].endswith(" "):
                return self.fill(pal[self.pal][2])  # a command that needs an argument: complete it first
            typed = "".join(self.buf)
            picked = pal and (self.pal_moved or " " not in typed or typed.split(" ", 1)[1].strip())
            line = pal[self.pal][2] if picked else typed
            self.buf, self.cur = [], 0
            self.submit(line)
        elif ch == "\x1b":
            if self.focus is not None:
                self.focus = None
            elif self.buf:
                self.fill("")
            elif self.busy and self.sid:
                self.acp.notify("session/cancel", {"sessionId": self.sid})
                self.add("sys", "cancelling…")
        elif ch in (curses.KEY_UP, curses.KEY_DOWN) and pal:
            self.pal = (self.pal + (1 if ch == curses.KEY_DOWN else -1)) % len(pal)
            self.pal_moved = True
        elif ch in (curses.KEY_BACKSPACE, "\x7f", "\b"):
            if self.cur:
                del self.buf[self.cur - 1]
                self.cur -= 1
        elif ch == curses.KEY_DC and self.cur < len(self.buf):
            del self.buf[self.cur]
        elif ch in (curses.KEY_LEFT, "\x02"):
            self.cur = max(0, self.cur - 1)
        elif ch in (curses.KEY_RIGHT, "\x06"):
            self.cur = min(len(self.buf), self.cur + 1)
        elif ch in (curses.KEY_HOME, "\x01"):
            self.cur = 0
        elif ch in (curses.KEY_END, "\x05"):
            self.cur = len(self.buf)
        elif ch == "\x15":
            self.buf, self.cur = self.buf[self.cur:], 0
        elif ch == "\x17":
            left = "".join(self.buf[:self.cur]).rstrip()
            cut = left.rfind(" ") + 1 if " " in left else 0
            self.buf, self.cur = list(left[:cut]) + self.buf[self.cur:], cut
        elif ch == curses.KEY_UP and self.history:
            self.hpos = max(0, self.hpos - 1)
            self.fill(self.history[self.hpos])
        elif ch == curses.KEY_DOWN and self.history:
            self.hpos = min(len(self.history), self.hpos + 1)
            self.fill(self.history[self.hpos] if self.hpos < len(self.history) else "")
        elif ch == curses.KEY_PPAGE:
            self.scroll += 10
        elif ch == curses.KEY_NPAGE:
            self.scroll = max(0, self.scroll - 10)
        elif isinstance(ch, str) and ch.isprintable():
            self.focus = None
            self.buf.insert(self.cur, ch)
            self.cur += 1
        if "".join(self.buf) != self.pal_key:
            self.pal, self.pal_moved = 0, False

    def mouse(self):
        try:
            _, x, y, _, bstate = curses.getmouse()
        except curses.error:
            return
        if bstate & getattr(curses, "BUTTON4_PRESSED", 0):
            self.scroll += 3
        elif bstate & getattr(curses, "BUTTON5_PRESSED", 0x200000):
            self.scroll = max(0, self.scroll - 3)
        elif bstate & (curses.BUTTON1_PRESSED | curses.BUTTON1_CLICKED):
            for hy, x0, x1, cmd in self.hits + self.pal_hits:
                if hy == y and x0 <= x < x1:
                    return self.press(cmd)

    # drawing: every entry becomes rows of segments
    def entry_rows(self, e, width):
        S, k = self.S, e["kind"]
        if k == "agent":
            return markdown(e["text"], width, S)
        if k == "you":
            return wrap(inline(e["text"], S.you, S), width, [("› ", S.you)], [("  ", 0)])
        if k == "tool":
            st = {"completed": ("✓", S.ok), "failed": ("✗", S.err), "in_progress": (icon("working"), S.brand)}.get(e.get("status"), ("·", S.dim))
            cmd = (e.get("raw") or {}).get("command")
            text = e["text"] if not cmd or cmd in e["text"] else f"{e['text']}: {cmd}"
            text = text.replace(MOACTL, "moactl").replace(f"cd {os.path.dirname(MASTER_DIR)} && ", "").replace(os.path.expanduser("~"), "~")
            return [fit([("⚙ ", S.dim), (text, S.dim)], width - 2) + [(" " + st[0], st[1])]]
        if k == "ctl":
            return self.card(e["title"], self.ctl_rows(e, width - 2), width)
        if k == "help":
            rows = []
            col = max(len(n) + len(a) for n, a, _ in COMMANDS) + 5
            for n, a, d in COMMANDS:
                rows.append(fit([(f" /{n} ", S.chip, f"!fill /{n} " if n in NEEDS_ARG else f"/{n}"),
                                 (f" {a}".ljust(col - len(n) - 3), S.dim), (d, 0)], width - 2))
            rows += [[]] + [[(ln, S.dim)] for ln in KEYS.split("\n")]
            return self.card("help", rows, width)
        if k == "event":
            what = e["what"]
            glyph, attr, verb = {"stop": ("●", S.ok, "finished"), "waiting": ("⚠", S.warn | curses.A_BOLD, "needs you"),
                                 "start": ("▶", S.dim, "started"), "end": ("○", S.dim, "ended")}[what]
            spans = [(f"{glyph} ", attr), (e["name"], curses.A_BOLD if what in ("stop", "waiting") else S.dim),
                     (f" {verb}", attr)]
            if what in ("stop", "waiting") and e["text"]:
                spans.append((": " + e["text"][:140], 0 if what == "stop" else S.warn))
            if what in ("stop", "waiting") and e["sid"]:
                spans += [(" ", 0), (" ↩ resume " if what == "stop" else " ↩ answer ", S.chip if what == "stop" else S.chipwarn,
                                     f"/resume {e['sid']}")]
            return wrap(spans, width, [(e["ts"] + " ", S.dim)], [("      ", 0)])
        if k == "perm":
            head = wrap([("moa wants to: ", S.warn | curses.A_BOLD), (e["text"], 0)], width - 2)
            if e.get("answered"):
                rows = head + [[(f"→ {e['answered']}", S.dim)]]
            else:
                rid = json.dumps(e["rid"])
                rows = head + [[(" y  once ", S.chipwarn, f"!perm {rid} y"), ("  ", 0), (" a  always ", S.chipwarn, f"!perm {rid} a"),
                                ("  ", 0), (" n  no ", S.chip, f"!perm {rid} n")]]
            return self.card("⚠ permission", rows, width, S.warn)
        if k == "err":
            return wrap([(e["text"], S.err)], width, [("✗ ", S.err)], [("  ", 0)])
        return wrap([(e["text"], S.dim | S.italic)], width, [("· ", S.dim)], [("  ", 0)])

    def ctl_rows(self, e, width):
        S = self.S
        if "sessions" in e:
            rows = []
            for s in e["sessions"]:
                if "untracked" in s:
                    rows.append(fit([("? ", S.warn), (f"untracked pane {s['untracked']} ", S.warn),
                                     (f"{s['account']} · {s['cwd'].replace(os.path.expanduser('~'), '~')}", S.dim)], width))
                    continue
                state = s["state"] if s["alive"] == "live" else s["alive"]
                btn = (" ↩ resume ", S.chip, f"/resume {s['id'][:8]}")
                left = fit([(f"{icon(state)} ", S.glyph.get(ICON.get(state), S.dim)), (s["title"] or s["id"][:8], curses.A_BOLD),
                            (f"  {s['group'] or ''}", S.dim), (f"  {ago(s['last'])}", S.dim)], width - len(btn[0]) - 1)
                rows.append(left + [(" " * (width - len(btn[0]) - seg_len(left)), 0), btn])
            return rows or [[("no sessions", S.dim)]]
        return markdown(ARROW_RE.sub(lambda m: f" ⟦{m.group(1)}|/{m.group(1)} {m.group(2)}⟧", e["text"]), width, S)

    def card(self, title, rows, width, attr=None):
        attr = attr if attr is not None else self.S.dim
        top = [("╭─ ", attr), (title, self.S.head if attr == self.S.dim else attr | curses.A_BOLD),
               (" " + "─" * max(0, width - len(title) - 4), attr)]
        return [top] + [[("│ ", attr)] + r for r in rows] + [[("╰" + "─" * (width - 1), attr)]]

    def cached_rows(self, e, width):
        if e["kind"] == "tool" and e.get("status") == "in_progress":
            return self.entry_rows(e, width)  # a spinner: no cache
        key = (width, len(e["text"]), e.get("status"), e.get("answered"), NAMES_GEN[0])
        if e.get("_key") != key:
            e["_key"], e["_rows"] = key, self.entry_rows(e, width)
        return e["_rows"]

    def lines(self, width):
        """The whole feed as rows: moa's turns grouped under one header, a gap between blocks."""
        S, out, block, i = self.S, [], None, 0
        feed = self.feed
        while i < len(feed):
            e = feed[i]
            k = e["kind"]
            b = ("moa", e["turn"]) if k in ("agent", "tool") else ("events",) if k == "event" else ("one", i)
            if b != block:
                if out:
                    out.append([])
                if b[0] == "moa":
                    out.append([("◆ moa", S.brand | curses.A_BOLD), (f"  {e['ts']}", S.dim)] +
                               ([("  · on its own", S.dim | S.italic)] if e["turn"] in self.auto_turns else []))
                block = b
            if k == "tool":  # a run of tool calls: show the last two, count the rest
                j = i
                while j < len(feed) and feed[j]["kind"] == "tool" and feed[j]["turn"] == e["turn"]:
                    j += 1
                run = feed[i:j]
                if len(run) > 3:
                    done = sum(1 for t in run[:-2] if t.get("status") == "completed")
                    out.append([("▏ ", S.brand), (f"⚙ {len(run) - 2} earlier steps", S.dim), (f"  {done} ✓", S.ok)])
                    run = run[-2:]
                out += [[("▏ ", S.brand)] + r for t in run for r in self.cached_rows(t, width - 2)]
                i = j
                continue
            rows = self.cached_rows(e, width - (2 if b[0] == "moa" else 0))
            out += [[("▏ ", S.brand)] + r for r in rows] if b[0] == "moa" else rows
            i += 1
        if self.busy:
            out += [[], [(f"{icon('working')} moa is thinking… ", S.brand), ("Esc cancels", S.dim)]]
        return out

    def draw(self):
        S, scr = self.S, self.scr
        h, w = scr.getmaxyx()
        W = w - 1
        scr.erase()
        screen = []  # (y, segments)
        if self.perms:
            state = [("⚠ answer y / a / n", S.warn | curses.A_BOLD)]
        elif not self.sid:
            state = [("connecting…", S.dim)]
        elif self.busy:
            state = [(f"{icon('working')} thinking", S.brand)]
        else:
            state = [("● ready", S.ok)]
        if self.reports and self.auto and not self.perms:
            state = [(f"◷ {len(self.reports)} for moa  ", S.dim)] + state
        head = [(f" thread {self.sid[:8]}" if self.sid else "", S.dim)]  # the pane's border already says "◆ moa"
        screen.append((0, head + [(" " * max(1, W - seg_len(head) - seg_len(state) - 1), 0)] + state))
        body = max(1, h - 4)
        lines = self.lines(W)
        self.scroll = min(self.scroll, max(0, len(lines) - body))
        end = len(lines) - self.scroll
        for y, segs in enumerate(lines[max(0, end - body):end], start=1):
            screen.append((y, segs))
        pal, self.pal_hits = self.palette(), []
        room = max(1, h - 6)  # a short pane shows a window of the list around the pick
        first = min(max(0, self.pal - room + 1), max(0, len(pal) - room))
        shown = pal[first:first + room]
        if shown:  # the command list floats over the bottom of the feed
            top = h - 3 - len(shown) - 1
            screen.append((top, [("╭─ ", S.dim), ("↑↓ pick · Tab complete · ↵ run", S.dim), (" " + "─" * W, S.dim)]))
            col = max(len(p[0]) for p in pal) + 2
            for n, (label, detail, ins) in enumerate(shown):
                attr = S.focus if first + n == self.pal else curses.A_BOLD
                row = fit([("│ ", S.dim), (f" {label} ".ljust(col), attr), (f"  {detail}", S.dim)], W)
                screen.append((top + 1 + n, row + [(" " * (W - seg_len(row)), 0)]))
                self.pal_hits.append((top + 1 + n, 0, W, ins if not ins.endswith(" ") else "!fill " + ins))
        rule = [("─" * W, S.dim)]
        if self.scroll:
            rule = [("──", S.dim), (f" ↑ {self.scroll} more · PgDn ", S.dim), ("─" * W, S.dim)]
        screen.append((h - 3, rule))
        bar, used = [], 0
        for label, cmd in BAR:
            if used + len(label) + 3 > W:
                break
            bar += [(f" {label} ", S.chip, cmd), (" ", 0)]
            used += len(label) + 3
        screen.append((h - 1, bar))
        # buttons, in Tab order: the newest in the feed first, then the bar
        feed_hits, bar_hits = [], []
        for y, segs in screen:
            x = 0
            for s in segs:
                if len(s) > 2 and s[2]:
                    (bar_hits if y == h - 1 else feed_hits).append((y, x, x + len(s[0]), s[2]))
                x += len(s[0])
        if shown:
            feed_hits = [hit for hit in feed_hits if hit[0] < h - 4 - len(shown)]
        self.hits = sorted(feed_hits, key=lambda hit: (-hit[0], hit[1])) + bar_hits  # bottom row first, left to right
        if self.focus is not None and self.focus not in self.hits:  # the feed moved: follow the same button
            same = [hit for hit in self.hits if hit[3] == self.focus[3]]
            self.focus = min(same, key=lambda hit: abs(hit[0] - self.focus[0])) if same else None
        focused = self.focus[:2] if self.focus is not None else None
        for y, segs in screen:
            x = 0
            for s in segs:
                n = min(len(s[0]), W - x)
                if n > 0:
                    attr = S.focus if focused == (y, x) and len(s) > 2 else s[1]
                    try:
                        scr.addnstr(y, x, s[0], n, attr)
                    except curses.error:
                        pass
                x += len(s[0])
        text = "".join(self.buf)
        view_start = max(0, self.cur - (w - 4))
        scr.addnstr(h - 2, 0, "› " + text[view_start:view_start + w - 4], W, S.you if text.startswith("/") else curses.A_BOLD)
        if not text:
            hint = "Tab: buttons · ↵ press" if self.focus is not None else "ask moa, or / for commands"
            scr.addnstr(h - 2, 2, hint, w - 3, S.dim)
        scr.move(h - 2, min(w - 2, 2 + self.cur - view_start))
        scr.refresh()


def main(scr):
    curses.use_default_colors()
    S = Style()
    curses.curs_set(1)
    scr.keypad(True)
    curses.mousemask(curses.ALL_MOUSE_EVENTS)
    curses.mouseinterval(0)
    con = Console(scr, S)
    me = os.path.abspath(__file__)
    code = os.stat(me).st_mtime_ns
    try:
        while True:
            if os.stat(me).st_mtime_ns != code and not con.busy:
                con.acp.p.terminate()
                curses.endwin()
                os.execv(sys.executable, [sys.executable, me])
            try:
                while not con.inbox.empty():
                    con.handle(con.inbox.get())
                con.watch_events()
                con.tick()
            except Exception as exc:  # keep the console alive; the traceback goes to stderr (console.err)
                con.error(exc)
            try:
                con.draw()
            except Exception as exc:
                con.error(exc)
            scr.timeout(100)
            try:
                ch = scr.get_wch()
            except curses.error:
                continue
            try:
                con.key(ch)
            except SystemExit:
                raise
            except Exception as exc:
                con.error(exc)
    except (SystemExit, KeyboardInterrupt):
        con.acp.p.terminate()


if __name__ == "__main__":
    os.environ.setdefault("ESCDELAY", "25")
    locale.setlocale(locale.LC_ALL, "")
    curses.wrapper(main)
