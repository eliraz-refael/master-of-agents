"""THROWAWAY PROTOTYPE: cockpit layouts. A layout is a tree of splits over panels;
each panel is a program in its own tmux pane in the `moa` window, tagged with the pane
option @moa_panel. Applying a layout builds it in a fresh window and moves the live
console and agent panes into it (they keep running), then retires the old window.
Not product code."""
import json
import os
import re
import shlex
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
CONFIG = os.path.expanduser("~/.config/moa/layouts.json")
SESSION, WINDOW = "moa", "moa:moa"


def _py(script, *args, log):
    return f"python3 {shlex.quote(os.path.join(HERE, script))} {' '.join(args)} 2>>{shlex.quote(os.path.join(HERE, log))}"


PANELS = {
    "console": _py("console.py", log="console.err"),
    "tree": _py("picture.py", "--panel", "tree", log="picture.err"),
    "today": _py("picture.py", "--panel", "today", log="picture.err"),
    "activity": _py("picture.py", "--panel", "activity", log="picture.err"),
    "projects": _py("picture.py", "--panel", "projects", log="picture.err"),
    "plan": _py("picture.py", "--panel", "plan", log="picture.err"),
    "picture": _py("picture.py", log="picture.err"),
    "agent": None,  # a live agent session's own pane, moved in
}
EMPTY_SLOT = ("sh -c 'printf \"\\n  no agent here yet\\n\\n  press Enter on a session in the tree\\n  to bring it into this spot\\n\"; "
              "exec sleep 2147483647'")
PLACEHOLDER = "sleep 2147483647"

DEFAULTS = {
    "current": "cockpit",
    "layouts": {
        "cockpit": {"split": "h", "children": [
            {"panel": "tree", "size": "26%"},
            {"panel": "console"},
            {"panel": "plan", "size": "30%"}]},
        "focus": {"split": "h", "children": [
            {"split": "v", "size": "24%", "children": [
                {"panel": "tree"},
                {"panel": "plan", "size": "45%"}]},
            {"split": "v", "children": [
                {"panel": "agent"},
                {"panel": "console", "size": "30%"}]}]},
        "classic": {"split": "h", "children": [
            {"panel": "console"},
            {"split": "v", "size": "42%", "children": [
                {"panel": "plan"},
                {"panel": "tree", "size": "40%"}]}]},
    },
}


def tmux(*args):
    return subprocess.run(["tmux", "-L", os.environ.get("MOA_TMUX", "moa"), *args], capture_output=True, text=True)


def out(*args):
    return tmux(*args).stdout.strip()


# config
def load():
    if not os.path.exists(CONFIG):
        save(DEFAULTS)
    with open(CONFIG) as f:
        return json.load(f)


def save(cfg):
    os.makedirs(os.path.dirname(CONFIG), exist_ok=True)
    with open(CONFIG, "w") as f:
        json.dump(cfg, f, indent=2)


def validate(node, path="layout"):
    if not isinstance(node, dict):
        raise ValueError(f"{path}: must be an object")
    size = node.get("size")
    if size is not None and not ((type(size) is int and size > 0) or (isinstance(size, str) and re.fullmatch(r"\d{1,2}%", size))):
        raise ValueError(f"{path}: size must be a number of cells or like \"30%\"")
    if "panel" in node:
        if not isinstance(node["panel"], str) or node["panel"] not in PANELS:
            raise ValueError(f"{path}: unknown panel {node['panel']!r} (panels: {', '.join(PANELS)})")
        return
    if node.get("split") not in ("h", "v"):
        raise ValueError(f"{path}: needs \"panel\", or \"split\": \"h\" (side by side) / \"v\" (stacked)")
    kids = node.get("children")
    if not isinstance(kids, list) or len(kids) < 2:
        raise ValueError(f"{path}: a split needs at least 2 children")
    for i, k in enumerate(kids):
        validate(k, f"{path}.children[{i}]")
    panels = leaves(node)
    if panels.count("console") > 1 or panels.count("agent") > 1:
        raise ValueError(f"{path}: at most one console and one agent panel")


def leaves(node):
    if "panel" in node:
        return [node["panel"]]
    return [p for k in node["children"] for p in leaves(k)]


# panes
def panes(target="-a"):
    args = ["list-panes", "-a"] if target == "-a" else ["list-panes", "-t", target]
    rows = out(*args, "-F", "#{pane_id}\t#{@moa_panel}\t#{window_id}\t#{window_name}").splitlines()
    return [dict(zip(("pane", "panel", "window", "wname"), r.split("\t"))) for r in rows if r]


def alive(pane):
    return bool(pane) and tmux("display-message", "-p", "-t", pane, "#{pane_id}").returncode == 0


def panel_of(pane):
    """The pane's @moa_panel tag ("" for an untagged pane such as a plain agent window)."""
    return out("display-message", "-p", "-t", pane, "#{@moa_panel}")


FRAMED = {"console", "tree", "plan", "today", "activity", "projects"}  # panels that draw their own frame
FRAME_IDLE, FRAME_ACTIVE = "fg=#4c566a", "fg=#88c0d0"  # a framed panel's default colour: its frame
LABELS = {"console": "◆ moa", "tree": "▤ sessions", "plan": "◷ big picture", "today": "☀ today",
          "activity": "≡ activity", "projects": "▦ projects", "picture": "◫ picture", "agent-slot": "○ agent spot"}


def tag(pane, panel, title=None):
    """Mark what a pane is (@moa_panel) and the title its top border shows (@moa_label)."""
    tmux("set", "-p", "-t", pane, "@moa_panel", panel)
    label(pane, f"● {title or 'agent'}" if panel == "agent" else LABELS.get(panel))
    # moa's own panels draw a rounded frame with the title (chrome.py) in the default colour;
    # tmux leaves them a blank gutter and colours the frame by focus
    framed = panel in FRAMED
    args = []
    for opt, val in (("@moa_framed", "1"), ("window-style", FRAME_IDLE), ("window-active-style", FRAME_ACTIVE)):
        args += ["set", "-p" if framed else "-pu", "-t", pane, opt, *([val] if framed else []), ";"]
    tmux(*args[:-1])  # one tmux call for all three


def label(pane, text):
    if text:
        tmux("set", "-p", "-t", pane, "@moa_label", text)
    else:  # an agent in its own window: the border shows the window's name
        tmux("set", "-pu", "-t", pane, "@moa_label")


def slot():
    """The cockpit pane that holds an agent (or the empty agent spot), if the layout has one."""
    for p in panes(WINDOW) if window_exists() else []:
        if p["panel"] in ("agent", "agent-slot"):
            return p
    return None


def window_exists():
    return tmux("has-session", "-t", WINDOW).returncode == 0 and \
        "moa" in out("list-windows", "-t", SESSION, "-F", "#{window_name}").splitlines()


def apply(name=None, session_title=lambda pane: None):
    """Build layout `name` (default: the current one) in a fresh window, move the live
    console and agent panes into it, and retire the old cockpit window."""
    cfg = load()
    name = name or cfg.get("current", "cockpit")
    if name not in cfg["layouts"]:
        raise ValueError(f"no layout {name!r} (have: {', '.join(cfg['layouts'])})")
    node = cfg["layouts"][name]
    validate(node)
    every = panes()
    old_win = next((p["window"] for p in every if p["wname"] == "moa"), None)
    console = next((p["pane"] for p in every if p["panel"] == "console"), None)
    current = next((p for p in every if p["panel"] == "agent" and p["window"] == old_win), None)
    agent = current["pane"] if current else None
    new_win, root = out("new-window", "-d", "-t", f"{SESSION}:", "-n", "moa-building", "-P", "-F",
                        "#{window_id} #{pane_id}", PLACEHOLDER).split()
    stray = []  # placeholders that ended up in other windows after a swap

    def place(pane, panel):
        nonlocal console, agent
        if panel == "console" and alive(console):
            tmux("swap-pane", "-d", "-s", console, "-t", pane)
            stray.append(pane)
            pane = console
        elif panel == "agent" and alive(agent):
            tmux("swap-pane", "-d", "-s", agent, "-t", pane)
            stray.append(pane)
            tag(agent, "agent", session_title(agent))
            return agent
        elif panel == "agent":
            tmux("respawn-pane", "-k", "-t", pane, EMPTY_SLOT)
            tag(pane, "agent-slot")
            return pane
        else:
            tmux("respawn-pane", "-k", "-t", pane, PANELS[panel])
        tag(pane, panel)
        return pane

    def build(n, pane):
        if "panel" in n:
            return place(pane, n["panel"])
        cells = [pane]
        for _ in n["children"][1:]:
            cells.append(out("split-window", "-d", "-" + n["split"], "-t", cells[-1], "-P", "-F", "#{pane_id}", PLACEHOLDER))
        firsts = [build(k, c) for k, c in zip(n["children"], cells)]
        for k, first in zip(n["children"], firsts):
            if k.get("size") is not None:
                tmux("resize-pane", "-t", first, "-x" if n["split"] == "h" else "-y", str(k["size"]))
        return firsts[0]

    build(node, root)
    used_agent = "agent" in leaves(node) and alive(agent)
    if old_win:
        # Agents (and the console, if this layout has no spot for it) still in the old
        # cockpit go back to windows of their own before it closes.
        for p in panes(old_win):
            if p["panel"] == "agent" and not (used_agent and p["pane"] == agent):
                tmux("break-pane", "-d", "-s", p["pane"], "-n", (session_title(p["pane"]) or "agent")[:24])
                label(p["pane"], None)
            elif p["panel"] == "console":
                tmux("break-pane", "-d", "-s", p["pane"], "-n", "console")
        tmux("kill-window", "-t", old_win)
    for p in stray:
        if alive(p):
            tmux("kill-pane", "-t", p)
    tmux("rename-window", "-t", new_win, "moa")
    tmux("move-window", "-s", new_win, "-t", f"{SESSION}:0")
    tmux("select-window", "-t", WINDOW)
    # the console in this layout (a respawned one if the old one died; none if the layout has no spot for it)
    console = next((p["pane"] for p in panes(new_win) if p["panel"] == "console"), None)
    if console:
        tmux("select-pane", "-t", console)
    if cfg.get("current") != name:
        cfg["current"] = name
        save(cfg)
    return name


def focus(target, title_of=lambda pane: None):
    """Show the agent in `target` pane: in the cockpit's agent spot if the layout has one,
    otherwise by switching to its window."""
    s = slot()
    if not s:
        tmux("select-window", "-t", target)
        tmux("select-pane", "-t", target)
        return "switched to its window"
    if s["pane"] != target:
        home = out("display-message", "-p", "-t", target, "#{window_id}")
        tmux("swap-pane", "-d", "-s", target, "-t", s["pane"])
        tag(target, "agent", title_of(target))
        if s["panel"] == "agent-slot":
            tmux("kill-pane", "-t", s["pane"])
        else:
            tmux("rename-window", "-t", home, (title_of(s["pane"]) or "agent")[:24])
            label(s["pane"], None)
    tmux("select-window", "-t", WINDOW)
    tmux("select-pane", "-t", target)
    return "brought it into the cockpit"
