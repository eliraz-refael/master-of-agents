"""THROWAWAY PROTOTYPE: find the Claude accounts on this machine and install, limit or
remove moa's tracking hook in each one's settings.json. A Claude account is a config
folder (~/.claude, or ~/.claude-* used with CLAUDE_CONFIG_DIR) that has a projects/
folder or a settings.json. The hook (master-chat/hooks/track.sh) reports only sessions
moa started (MOA_TRACK=1); "only" also limits it to some folders (MOA_ONLY_UNDER).
Not product code."""
import datetime
import glob
import json
import os
import shlex
import shutil

HERE = os.path.dirname(os.path.abspath(__file__))
TRACK = os.path.normpath(os.path.join(HERE, "..", "master-chat", "hooks", "track.sh"))
MARK = "# moa-spike"  # every hook command moa installs ends with this, so it can find and remove them
EVENTS = {"SessionStart": None, "UserPromptSubmit": None, "Stop": None, "SessionEnd": None,
          "Notification": "permission_prompt|elicitation_dialog"}


def find_accounts(names):
    """[{name, dir, state, only, last_used}] for every Claude config folder in $HOME.
    `names` maps account names (the command you run) to config folders, to label them."""
    home = os.path.expanduser("~")
    by_dir = {os.path.realpath(os.path.expanduser(d)): n for n, d in names.items()}
    found = []
    for d in sorted(glob.glob(os.path.join(home, ".claude")) + glob.glob(os.path.join(home, ".claude-*"))):
        if not os.path.isdir(d) or not (os.path.isdir(os.path.join(d, "projects")) or os.path.isfile(os.path.join(d, "settings.json"))):
            continue
        state, only = hook_state(d)
        projects = os.path.join(d, "projects")
        used = os.path.getmtime(projects) if os.path.isdir(projects) else None
        found.append({"name": by_dir.get(os.path.realpath(d), os.path.basename(d).lstrip(".")), "dir": d,
                      "state": state, "only": only, "last_used": used})
    return found


def _settings(d):
    p = os.path.join(d, "settings.json")
    if not os.path.exists(p):
        return p, {}
    with open(p) as f:
        return p, json.load(f)


def _moa_commands(s):
    return [h.get("command", "") for groups in (s.get("hooks") or {}).values() for g in groups
            for h in g.get("hooks", []) if MARK in h.get("command", "")]


def hook_state(d):
    """("installed" | "not installed" | "unreadable", [folders it's limited to])."""
    try:
        _, s = _settings(d)
    except (OSError, ValueError):
        return "unreadable", []
    cmds = _moa_commands(s)
    if not cmds:
        return "not installed", []
    only = []
    for c in cmds:
        for word in shlex.split(c.split(MARK)[0]):
            if word.startswith("MOA_ONLY_UNDER="):
                only = [os.path.expandvars(x) for x in word.split("=", 1)[1].split(":") if x]
    return "installed", only


def remove(d):
    """Take moa's hooks out of this account's settings (other hooks stay). Returns how many."""
    p, s = _settings(d)
    n = 0
    for ev, groups in list((s.get("hooks") or {}).items()):
        kept = []
        for g in groups:
            hooks = [h for h in g.get("hooks", []) if MARK not in h.get("command", "")]
            n += len(g.get("hooks", [])) - len(hooks)
            if hooks:
                kept.append({**g, "hooks": hooks})
        if kept:
            s["hooks"][ev] = kept
        else:
            del s["hooks"][ev]
    if n:
        _write(p, s)
    return n


def install(d, only=None):
    """(Re)install moa's hook for every event it uses; `only` limits it to those folders."""
    remove(d)
    p, s = _settings(d)
    env = ""
    if only:
        home = os.path.expanduser("~")
        roots = [os.path.abspath(os.path.expanduser(x)) for x in only]
        env = 'MOA_ONLY_UNDER="' + ":".join("$HOME" + r[len(home):] if r.startswith(home + "/") else r for r in roots) + '" '
    hook = {"type": "command", "command": f"{env}bash {shlex.quote(TRACK)} {MARK}", "timeout": 5}
    for ev, matcher in EVENTS.items():
        group = {"hooks": [hook]}
        if matcher:
            group["matcher"] = matcher
        s.setdefault("hooks", {}).setdefault(ev, []).append(group)
    _write(p, s)


def _write(p, s):
    backup = p + ".bak-moa"
    if os.path.exists(p) and not os.path.exists(backup):  # the settings as they were before moa first touched them
        shutil.copy(p, backup)
    tmp = p + ".moa-tmp"
    with open(tmp, "w") as f:
        json.dump(s, f, indent=2)
        f.write("\n")
    os.replace(tmp, p)


def short(path):
    home = os.path.expanduser("~")
    return "~" + path[len(home):] if path.startswith(home) else path


def ago(ts):
    if not ts:
        return "never used"
    days = (datetime.datetime.now().timestamp() - ts) / 86400
    return "used today" if days < 1 else f"used {int(days)}d ago"


def interactive(accounts, default_only, skipped):
    """Ask about each account that isn't tracked. Returns the names the person skipped."""
    print("moa follows your agent sessions through a small hook in each Claude account.")
    print("It only reports sessions moa starts, and you can limit it to some folders.\n")
    for a in accounts:
        extra = f" (only {', '.join(short(x) for x in a['only'])})" if a["only"] else ""
        print(f"  {'✓' if a['state'] == 'installed' else '·'} {a['name']:<16} {short(a['dir']):<22} {a['state']}{extra}  · {ago(a['last_used'])}")
    todo = [a for a in accounts if a["state"] == "not installed"]
    if not todo:
        print("\nEvery account is set up. (moactl setup remove NAME takes it out.)")
        return skipped
    folders = " ".join(short(x) for x in default_only)
    print(f"\nFor each account: y install · f only under {folders} · n not now")
    for a in todo:
        try:
            ans = input(f"  {a['name']} ({short(a['dir'])}, {ago(a['last_used'])})? [y/f/n] ").strip().lower()
        except EOFError:
            break
        if ans in ("y", "yes"):
            install(a["dir"])
            print("    installed")
        elif ans in ("f", "folders"):
            install(a["dir"], default_only)
            print(f"    installed, only under {folders}")
        else:
            skipped = sorted(set(skipped) | {a["name"]})
            print("    skipped")
    print("\nRunning sessions pick this up when they restart.")
    return skipped
