#!/usr/bin/env python3
"""THROWAWAY PROTOTYPE: the session picker (Ctrl-a /). Type to filter every session moa
knows, Enter jumps to it (or resumes it): the way around once there are more agents than
fit in the status line. Runs in a tmux popup (tmux.conf). Live sessions come first, most
urgent first (the status panel's order), then finished ones, most recent first.
fzf would do the filtering, but it isn't installed here, and a curses list in moa's own
palette can show the live state glyphs and animate them. Not product code."""
import curses
import locale
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chrome  # noqa: E402
from picture import (SESSIONS_SQL, SHOW_WORD, ago, connect, db_mtime, live, show_icon,  # noqa: E402
                     HERE)

GONE = {"ended": ("○", "ended"), "elsewhere": ("◌", "in herdr")}


def sessions(conn):
    """Live sessions (with their status) first, then the rest that aren't archived."""
    rows, _ = live(conn)
    seen = {s["session_id"] for s in rows}
    rest = conn.execute(SESSIONS_SQL + """ WHERE coalesce(s.state,'') NOT IN ('archived','working','idle','waiting')
        ORDER BY s.last_seen DESC LIMIT 300""").fetchall()
    for r in rest:
        if r["session_id"] not in seen:
            s = dict(r)
            s["show"], s["since"], s["why"] = r["state"] or "ended", r["last_seen"], ""
            rows.append(s)
    for s in rows:
        title = s["title"] or s["project"] or os.path.basename(s["cwd"] or "") or s["session_id"][:8]
        word = SHOW_WORD.get(s["show"]) or GONE.get(s["show"], ("", s["show"]))[1]
        s["cols"] = (title, s["grp"] or s["project"] or "", word)
        s["hay"] = " ".join((*s["cols"], os.path.basename(s["cwd"] or ""), s["session_id"][:8])).lower()
    return rows


def match(hay, terms):
    """fzf-ish: every space-separated term is a subsequence of the session's text.
    Returns a rank (contiguous matches first) or None."""
    rank = 0
    for t in terms:
        i = 0
        for ch in t:
            i = hay.find(ch, i) + 1
            if not i:
                return None
        rank += 0 if t in hay else 1
    return rank


def marks(line, terms):
    """Where the terms hit the rendered line, to highlight (substring first, else subsequence)."""
    low, hits = line.lower(), set()
    for t in terms:
        at = low.find(t)
        if at >= 0:
            hits.update(range(at, at + len(t)))
            continue
        i = 0
        for ch in t:
            i = low.find(ch, i) + 1
            if not i:
                break
            hits.add(i - 1)
    return hits


def main(scr):
    curses.curs_set(1)
    curses.use_default_colors()
    chrome.use_palette(scr)
    c = chrome.color
    attr = {"waiting": c("yellow") | curses.A_BOLD, "question": c("orange") | curses.A_BOLD, "done": c("green") | curses.A_BOLD,
            "working": c("blue"), "idle": c("text")}
    dim, sel_attr, hit, prompt = c("dim"), c("bright", "sel"), c("yellow") | curses.A_BOLD | curses.A_UNDERLINE, c("blue") | curses.A_BOLD
    conn = connect()
    query, sel, top, stamp, rows = "", 0, 0, None, []
    while True:
        if db_mtime() != stamp:
            stamp, rows = db_mtime(), sessions(conn)
        terms = query.lower().split()
        shown = [s for _, _, s in sorted((match(s["hay"], terms), i, s) for i, s in enumerate(rows)
                                         if match(s["hay"], terms) is not None)] if terms else rows
        h, w = scr.getmaxyx()
        body = max(1, h - 3)
        sel = max(0, min(sel, len(shown) - 1))
        top = min(max(top, sel - body + 1), sel)
        scr.erase()
        nlive = sum(s["show"] in SHOW_WORD for s in rows)
        count = f"{len(shown)}/{len(rows)} · {nlive} live "
        scr.addnstr(1, 0, "─" * (w - 1), w - 1, dim)
        tw = max(10, min(30, w // 4))
        gw = max(0, min(24, w // 6)) if w >= 70 else 0
        for y, s in enumerate(shown[top:top + body], start=2):
            title, grp, word = s["cols"]
            glyph = show_icon(s["show"]) if s["show"] in SHOW_WORD else GONE.get(s["show"], ("·",))[0]
            line = f" {glyph} {title[:tw - 1]:<{tw}}{grp[:gw - 1]:<{gw}}{word:<10}{ago(s['since']):>4}  {s['why'] or ''}"
            line = line.replace("\n", " ")[:w - 1]
            on = top + y - 2 == sel
            base = sel_attr if on else attr.get(s["show"], dim)
            scr.addnstr(y, 0, line.ljust(w - 1) if on else line, w - 1, base)
            for x in marks(line[3:3 + tw + gw], terms):  # highlight in the title and group only
                scr.addnstr(y, 3 + x, line[3 + x], 1, (sel_attr | curses.A_BOLD | curses.A_UNDERLINE) if on else hit)
        if not shown:
            scr.addnstr(2, 2, "no session matches", w - 3, dim)
        hint = "↵ jump · ↑↓ move · esc close"
        scr.addnstr(h - 1, 0, hint, w - 1, dim)
        scr.addnstr(0, max(0, w - 1 - len(count)), count, len(count), dim)
        scr.addnstr(0, 0, "› ", 2, prompt)
        scr.addnstr(0, 2, query[-(w - len(count) - 4):], max(1, w - len(count) - 4), c("bright") | curses.A_BOLD)
        scr.move(0, min(w - len(count) - 2, 2 + len(query)))
        scr.refresh()

        scr.timeout(150)  # the working spinners and the waiting blink keep moving
        try:
            k = scr.get_wch()
        except curses.error:
            continue
        if k in ("\x1b", "\x03", "\x07"):  # Esc, Ctrl-c, Ctrl-g
            return None
        if k in ("\n", "\r", curses.KEY_ENTER):
            return shown[sel]["session_id"] if shown else None
        if k in (curses.KEY_UP, "\x10", curses.KEY_BTAB):
            sel -= 1
        elif k in (curses.KEY_DOWN, "\x0e", "\t"):
            sel += 1
        elif k == curses.KEY_PPAGE:
            sel -= body
        elif k == curses.KEY_NPAGE:
            sel += body
        elif k in (curses.KEY_BACKSPACE, "\x7f", "\b"):
            query, sel = query[:-1], 0
        elif k == "\x15":  # Ctrl-u
            query, sel = "", 0
        elif k == "\x17":  # Ctrl-w
            query, sel = query.rstrip()[:query.rstrip().rfind(" ") + 1], 0
        elif isinstance(k, str) and k.isprintable():
            query, sel = query + k, 0
        sel = max(0, min(sel, len(shown) - 1))


if __name__ == "__main__":
    os.environ.setdefault("ESCDELAY", "25")
    locale.setlocale(locale.LC_ALL, "")
    picked = chrome.wrapper(main)
    if picked:
        # moactl focus: into the cockpit's agent spot if the layout has one, else its own
        # window; resumes it first if it isn't running. moamenu shows the result as a message.
        os.execv(os.path.join(HERE, "moamenu"), [os.path.join(HERE, "moamenu"), "focus", picked])
