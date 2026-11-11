#!/usr/bin/env python3
"""THROWAWAY PROTOTYPE: the console. A feed of what moa says and what tracked
sessions do, plus one input line: /commands run instantly through the brain API
(moactl); anything else goes to moa, the central agent, over ACP (claude-agent-acp on
the claude-lead account). Not product code."""
import curses
import datetime
import json
import locale
import os
import queue
import shlex
import subprocess
import sys
import textwrap
import threading
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from picture import connect, db_mtime, icon  # noqa: E402

MASTER_DIR = os.path.normpath(os.path.join(HERE, "..", "master-chat", "master"))
MOACTL = os.path.join(HERE, "moactl")
ACCOUNT = os.path.expanduser("~/.claude-lead")
OPENING = ("(The console just opened a new thread for today.) Give me your opening brief: "
           "at most 6 short lines, what's hot, what's waiting on me, what's going stale, "
           "then propose 1-3 goals for today and ask which.")
HELP = """/attention       what needs you, ranked (instant, no LLM)
/brief           moa's brief
/ls              live sessions
/open PATH [title…]   start an agent there and switch to it
/resume Q        resume or jump to a session (title, project or ID)
/focus Q         bring a session into the cockpit's agent spot
/layout [NAME]   list layouts, or switch to one (cockpit, focus, classic, …)
/close Q · /archive Q
/new             start a fresh moa thread
/quit
anything else goes to moa · Esc cancels its turn · PgUp/PgDn scroll
Ctrl-] anywhere: the moa menu
when it asks permission: y once · a always · n no"""


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
    def __init__(self, scr):
        self.scr, self.conn = scr, connect()
        self.feed, self.tools = [], {}
        self.inbox = queue.Queue()
        self.acp = Acp(self.inbox)
        self.pending = {}          # request id -> what it was
        self.sid = None
        self.busy = False
        self.perm = None           # a pending session/request_permission
        self.buf, self.cur = [], 0
        self.history, self.hpos = [], 0
        self.scroll = 0
        self.streaming = None      # the agent entry being streamed into
        self.cursor = self.conn.execute("SELECT coalesce(max(id),0) FROM events").fetchone()[0]
        self.seen = db_mtime()
        self.pending[self.acp.request("initialize", {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False}})] = "init"
        self.add("sys", "moa · type /help, or Ctrl-] for the menu · connecting…")
        self.run_ctl("attention", header="needs you right now:")

    # feed
    def add(self, kind, text, **kw):
        e = {"kind": kind, "text": text, "ts": datetime.datetime.now().strftime("%H:%M"), **kw}
        self.feed.append(e)
        del self.feed[:-500]
        self.streaming = None if kind != "agent" else e
        return e

    def run_ctl(self, *args, header=None):
        r = subprocess.run([MOACTL, *args], capture_output=True, text=True)
        out = (r.stdout + r.stderr).strip()
        if header:
            self.add("sys", header)
        self.add("ctl", out or "ok")

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

    def prompt(self, text, show=True):
        if not self.sid:
            self.add("sys", "moa isn't connected yet")
            return
        if show:
            self.add("you", text)
        self.busy = True
        self.pending[self.acp.request("session/prompt", {"sessionId": self.sid, "prompt": [{"type": "text", "text": text}]})] = "prompt"

    def handle(self, m):
        if m.get("_exit"):
            self.add("err", "moa's agent process exited; reopen the console from the menu (Ctrl-])")
            self.busy = False
            return
        if "method" in m and "id" in m:                       # a request from the agent
            if m["method"] == "session/request_permission":
                self.perm = m
                tc = m["params"].get("toolCall", {})
                self.add("perm", f"allow {tc.get('title') or 'this'}?  [y] once · [a] always · [n] no")
            else:
                self.acp.respond(m["id"], error={"code": -32601, "message": "not supported by the console"})
            return
        if "id" in m:                                          # a response to us
            what = self.pending.pop(m["id"], None)
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
                else:
                    self.new_thread()
            elif what == "new":
                self.sid = r["sessionId"]
                self.thread_meta({"sid": self.sid, "date": datetime.date.today().isoformat()})
                self.add("sys", f"new thread {self.sid[:8]} for today")
                self.prompt(OPENING, show=False)
            elif what == "load":
                self.add("sys", f"resumed today's thread {self.sid[:8]}")
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
                if who == "you" and text.startswith(OPENING[:30]):
                    return
                if self.feed and self.feed[-1]["kind"] == who and (who == "you" or self.streaming is self.feed[-1]):
                    self.feed[-1]["text"] += text
                else:
                    e = self.add(who, text)
                    if who == "agent":
                        self.streaming = e
            elif kind == "tool_call":
                self.tools[u["toolCallId"]] = self.add("tool", u.get("title") or "tool", status=u.get("status"), raw=u.get("rawInput") or {})
            elif kind == "tool_call_update" and u.get("toolCallId") in self.tools:
                e = self.tools[u["toolCallId"]]
                if u.get("title"):
                    e["text"] = u["title"]
                if u.get("status"):
                    e["status"] = u["status"]
                if u.get("rawInput"):
                    e["raw"] = u["rawInput"]

    # sessions reporting in
    def watch_events(self):
        m = db_mtime()
        if m == self.seen:
            return
        self.seen = m
        rows = self.conn.execute("""SELECT e.*, coalesce(s.title, p.name) AS name FROM events e
            LEFT JOIN sessions s ON s.session_id=e.session_id LEFT JOIN projects p ON p.id=s.project_id
            WHERE e.id > ? ORDER BY e.id""", (self.cursor,)).fetchall()
        for e in rows:
            self.cursor = e["id"]
            name = e["name"] or os.path.basename(e["cwd"] or "") or "?"
            sid = (e["session_id"] or "")[:8]
            text = " ".join((e["text"] or "").split())
            if e["kind"] == "stop":
                self.add("done", f"● {name} finished: {text[:160]}", hint=f"/resume {sid}")
            elif e["kind"] == "waiting":
                self.add("wait", f"⚠ {name} needs you: {text[:120]}", hint=f"/resume {sid}")
            elif e["kind"] == "start":
                self.add("event", f"▶ {name} started")
            elif e["kind"] == "end":
                self.add("event", f"○ {name} ended")

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
        cmd, *args = shlex.split(line[1:]) if line.count('"') % 2 == 0 else line[1:].split()
        if cmd == "help":
            self.add("ctl", HELP)
        elif cmd == "quit":
            raise SystemExit
        elif cmd == "new":
            self.add("sys", "starting a fresh thread…")
            self.new_thread()
        elif cmd in ("attention", "brief", "ls"):
            self.run_ctl(cmd)
        elif cmd == "open" and args:
            self.run_ctl("open", args[0], *(["--title", " ".join(args[1:])] if args[1:] else []))
        elif cmd == "layout":
            self.run_ctl("layout", *(["apply", args[0]] if args else ["list"]))
        elif cmd in ("resume", "close", "archive", "focus") and args:
            self.run_ctl(cmd, " ".join(args))
        else:
            self.add("sys", f"unknown command /{cmd} (try /help)")

    def key(self, ch):
        if self.perm and not self.buf and ch in ("y", "a", "n"):
            want = {"y": "allow_once", "a": "allow_always", "n": "reject_once"}[ch]
            opts = self.perm["params"]["options"]
            opt = next((o for o in opts if o["kind"] == want), opts[0])
            self.acp.respond(self.perm["id"], {"outcome": {"outcome": "selected", "optionId": opt["optionId"]}})
            self.add("sys", f"→ {opt['name']}")
            self.perm = None
            return
        if ch in ("\n", "\r", curses.KEY_ENTER):
            line, self.buf, self.cur = "".join(self.buf), [], 0
            self.submit(line)
        elif ch == "\x1b":
            if self.busy and self.sid:
                self.acp.notify("session/cancel", {"sessionId": self.sid})
                self.add("sys", "cancelling…")
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
            self.buf = list(self.history[self.hpos])
            self.cur = len(self.buf)
        elif ch == curses.KEY_DOWN and self.history:
            self.hpos = min(len(self.history), self.hpos + 1)
            self.buf = list(self.history[self.hpos]) if self.hpos < len(self.history) else []
            self.cur = len(self.buf)
        elif ch == curses.KEY_PPAGE:
            self.scroll += 10
        elif ch == curses.KEY_NPAGE:
            self.scroll = max(0, self.scroll - 10)
        elif isinstance(ch, str) and ch.isprintable():
            self.buf.insert(self.cur, ch)
            self.cur += 1

    # drawing
    def lines(self, width, C):
        out = []
        for e in self.feed:
            k = e["kind"]
            if k == "you":
                first, rest, attr = "› ", "  ", C["you"]
            elif k == "agent":
                first, rest, attr = "  ", "  ", 0
            elif k == "tool":
                st = {"completed": "✓", "failed": "✗", "in_progress": icon("working")}.get(e.get("status"), "·")
                cmd = (e.get("raw") or {}).get("command")
                text = e["text"] if not cmd or cmd in e["text"] else f"{e['text']}: {cmd}"
                text = text.replace(MOACTL, "moactl").replace(f"cd {os.path.dirname(MASTER_DIR)} && ", "").replace(os.path.expanduser("~"), "~")
                out.append((f"    ⚙ {text}"[:width - 3] + f" {st}", C["dim"]))
                continue
            elif k in ("done", "wait", "event"):
                first, rest, attr = f"{e['ts']} ", "      ", {"done": C["ok"], "wait": C["warn"] | curses.A_BOLD, "event": C["dim"]}[k]
            elif k == "perm":
                first, rest, attr = "⚠ ", "  ", C["warn"] | curses.A_BOLD
            elif k == "err":
                first, rest, attr = "✗ ", "  ", C["err"]
            else:
                first, rest, attr = "", "", C["dim"]
            text = e["text"] + (f"   {e['hint']}" if e.get("hint") else "")
            lead = first
            for para in text.split("\n"):
                wrapped = textwrap.wrap(para, max(10, width - len(rest) - 1)) or [""]
                for w in wrapped:
                    out.append((lead + w, attr))
                    lead = rest
            if k in ("you", "agent"):
                out.append(("", 0))
        return out

    def draw(self, C):
        scr = self.scr
        h, w = scr.getmaxyx()
        scr.erase()
        state = (f"{icon('working')} thinking… (Esc cancels)" if self.busy else "ready") if self.sid else "connecting…"
        if self.perm:
            state = "⚠ waiting for your y / a / n"
        head = f" moa · thread {self.sid[:8] if self.sid else '…'} · {state}"
        scr.addnstr(0, 0, head.ljust(w - 1), w - 1, C["head"] | curses.A_REVERSE)
        body = h - 3
        lines = self.lines(w - 1, C)
        self.scroll = min(self.scroll, max(0, len(lines) - body))
        end = len(lines) - self.scroll
        for y, (text, attr) in enumerate(lines[max(0, end - body):end], start=1):
            scr.addnstr(y, 0, text, w - 1, attr)
        scr.hline(h - 2, 0, curses.ACS_HLINE, w - 1)
        if self.scroll:
            scr.addnstr(h - 2, 2, f" ↑ {self.scroll} lines · PgDn ", w - 4, C["dim"])
        text = "".join(self.buf)
        view_start = max(0, self.cur - (w - 4))
        scr.addnstr(h - 1, 0, "› " + text[view_start:view_start + w - 4], w - 1, curses.A_BOLD)
        if not text:
            scr.addnstr(h - 1, 2, "ask moa, or /help", w - 3, C["dim"])
        scr.move(h - 1, min(w - 2, 2 + self.cur - view_start))
        scr.refresh()


def main(scr):
    curses.use_default_colors()
    for i, c in enumerate([curses.COLOR_CYAN, 8 if curses.COLORS > 8 else curses.COLOR_WHITE, curses.COLOR_YELLOW,
                           curses.COLOR_GREEN, curses.COLOR_MAGENTA, curses.COLOR_RED], start=1):
        curses.init_pair(i, c, -1)
    C = {"head": curses.color_pair(1) | curses.A_BOLD, "dim": curses.color_pair(2), "warn": curses.color_pair(3),
         "ok": curses.color_pair(4), "you": curses.color_pair(5) | curses.A_BOLD, "err": curses.color_pair(6)}
    curses.curs_set(1)
    scr.keypad(True)
    con = Console(scr)
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
            except Exception as exc:  # keep the console alive; the traceback goes to stderr (console.err)
                traceback.print_exc()
                con.add("err", f"console error: {exc!r} (details in console.err)")
            con.draw(C)
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
                traceback.print_exc()
                con.add("err", f"console error: {exc!r} (details in console.err)")
    except (SystemExit, KeyboardInterrupt):
        con.acp.p.terminate()


if __name__ == "__main__":
    os.environ.setdefault("ESCDELAY", "25")
    locale.setlocale(locale.LC_ALL, "")
    curses.wrapper(main)
