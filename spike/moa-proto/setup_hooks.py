"""THROWAWAY PROTOTYPE: find the Claude accounts on this machine and install, limit or
remove moa's tracking hook in each one's settings.json. A Claude account is a config
folder (~/.claude, or ~/.claude-* used with CLAUDE_CONFIG_DIR) that has a projects/
folder or a settings.json. The hook (master-chat/hooks/track.sh) reports only sessions
moa started (MOA_TRACK=1); "only" also limits it to some folders (MOA_ONLY_UNDER), and
"--else BRAIN" sends the account's sessions outside them to that brain's moa (MOA_ELSE).
Not product code."""
import datetime
import glob
import json
import os
import re
import shlex
import shutil
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
TRACK = os.path.normpath(os.path.join(HERE, "..", "master-chat", "hooks", "track.sh"))
# the moa plugin (a skill telling agents about moa, and moactl on their PATH), loaded in place
# from this folder as a local marketplace: edits apply at the next session start
MARKET, PLUGIN = "moa-local", "moa@moa-local"
MARKET_DIR = os.path.normpath(os.path.join(HERE, "..", "moa-plugin"))
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
        state, only, other = hook_state(d)
        projects = os.path.join(d, "projects")
        used = os.path.getmtime(projects) if os.path.isdir(projects) else None
        found.append({"name": by_dir.get(os.path.realpath(d), os.path.basename(d).lstrip(".")), "dir": d,
                      "state": state, "only": only, "else": other, "last_used": used, "plugin": has_plugin(d)})
    return found


def has_plugin(d):
    try:
        return bool((_settings(d)[1].get("enabledPlugins") or {}).get(PLUGIN))
    except (OSError, ValueError):
        return False


def _settings(d):
    p = os.path.join(d, "settings.json")
    if not os.path.exists(p):
        return p, {}
    with open(p) as f:
        return p, json.load(f)


def _is_moa(cmd):
    # the marker, or a track.sh hook installed by hand before there was one
    return MARK in cmd or "master-chat/hooks/track.sh" in cmd


def _moa_commands(s):
    return [h.get("command", "") for groups in (s.get("hooks") or {}).values() for g in groups
            for h in g.get("hooks", []) if _is_moa(h.get("command", ""))]


def hook_state(d):
    """("installed" | "not installed" | "unreadable", [folders it's limited to], the brain
    sessions outside them go to or None)."""
    try:
        _, s = _settings(d)
    except (OSError, ValueError):
        return "unreadable", [], None
    cmds = _moa_commands(s)
    if not cmds:
        return "not installed", [], None
    only, other = [], None
    for c in cmds:
        for word in shlex.split(c.split(MARK)[0]):
            if word.startswith("MOA_ONLY_UNDER="):
                only = [os.path.expandvars(x).replace("\\$", "$").replace("\\`", "`") for x in word.split("=", 1)[1].split(":") if x]
            elif word.startswith("MOA_ELSE="):
                other = word.split("=", 1)[1] or None
    return "installed", only, other


def _strip(s):
    """Take moa's hooks out of settings `s` in place (other hooks stay). Returns how many."""
    n = 0
    for ev, groups in list((s.get("hooks") or {}).items()):
        kept = []
        for g in groups:
            hooks = [h for h in g.get("hooks", []) if not _is_moa(h.get("command", ""))]
            n += len(g.get("hooks", [])) - len(hooks)
            if hooks or not g.get("hooks"):  # keep groups that never had hooks of moa's
                kept.append({**g, "hooks": hooks} if hooks else g)
        if kept:
            s["hooks"][ev] = kept
        else:
            del s["hooks"][ev]
    if "hooks" in s and not s["hooks"]:
        del s["hooks"]
    return n


def _unplug(s):
    """Take the plugin's folder limit out of settings `s` in place."""
    for k in ("MOA_ONLY_UNDER", "MOA_ELSE"):
        (s.get("env") or {}).pop(k, None)
    if "env" in s and not s["env"]:
        del s["env"]


def plugin(d, on):
    """Add or take out the moa plugin with Claude's own CLI (settings entries alone don't
    register a marketplace). Returns an error message, or None."""
    env = dict(os.environ, CLAUDE_CONFIG_DIR=d)
    run = lambda *a: subprocess.run(["claude", "plugin", *a], env=env, capture_output=True, text=True,  # noqa: E731
                                    stdin=subprocess.DEVNULL, timeout=60)
    try:
        if not on:
            r = run("uninstall", PLUGIN)
            run("marketplace", "remove", MARKET)
            # not installed is fine; anything else (e.g. enabled at another scope) is worth saying
            return None if r.returncode == 0 or not has_plugin(d) else _last_line(r, "the plugin uninstall failed")
        run("marketplace", "add", MARKET_DIR)  # fails harmlessly when it's already there
        r = run("install", PLUGIN)
    except FileNotFoundError:
        return f"the claude command isn't on PATH, so the moa plugin wasn't {'installed' if on else 'removed'}"
    except subprocess.TimeoutExpired:
        return f"the claude command hung, so the moa plugin wasn't {'installed' if on else 'removed'}"
    return None if r.returncode == 0 else _last_line(r, "the plugin install failed")


def _last_line(r, default):
    return ((r.stdout + r.stderr).strip().splitlines() or [default])[-1]


def remove(d):
    """Take moa's hooks and plugin out of this account's settings (others stay). Returns (how
    many hooks, an error message about the plugin or None)."""
    p, s = _settings(d)
    before = json.dumps(s, sort_keys=True)
    n = _strip(s)
    _unplug(s)
    if json.dumps(s, sort_keys=True) != before:
        _write(p, s)
    err = plugin(d, False)
    p, s = _settings(d)  # Claude's uninstall leaves the plugin's settings keys behind, empty
    if any(k in s and not s[k] for k in ("enabledPlugins", "extraKnownMarketplaces")):
        _write(p, {k: v for k, v in s.items() if v or k not in ("enabledPlugins", "extraKnownMarketplaces")})
    return n, err


def _dq(text):
    """Escape text for inside a double-quoted shell word."""
    return re.sub(r'([\\"$`])', r"\\\1", text)


def install(d, only=None, other=None):
    """(Re)install moa's hook for every event it uses, and the moa plugin; `only` limits both
    to those folders, and `other` (a brain's name) gets the sessions outside them."""
    p, s = _settings(d)
    _strip(s)
    _unplug(s)
    env = ""
    if only:
        home = os.path.expanduser("~")
        roots = [os.path.abspath(os.path.expanduser(x)) for x in only]
        env = 'MOA_ONLY_UNDER="' + ":".join("$HOME" + _dq(r[len(home):]) if r.startswith(home + "/") else _dq(r)
                                            for r in roots) + '" '
        # the same limit for the plugin's moactl: settings env reaches the agent's shell (no $HOME expansion there)
        s.setdefault("env", {})["MOA_ONLY_UNDER"] = ":".join(roots)
        if other:
            env += f"MOA_ELSE={shlex.quote(other)} "
            s["env"]["MOA_ELSE"] = other
    hook = {"type": "command", "command": f"{env}bash {shlex.quote(TRACK)} {MARK}", "timeout": 5}
    for ev, matcher in EVENTS.items():
        group = {"hooks": [hook]}
        if matcher:
            group["matcher"] = matcher
        s.setdefault("hooks", {}).setdefault(ev, []).append(group)
    _write(p, s)
    return plugin(d, True)  # an error message if the plugin couldn't be installed


def _write(p, s):
    p = os.path.realpath(p)  # a settings.json linked from a dotfiles repo stays a link
    backup = p + ".bak-moa"
    if os.path.exists(p) and not os.path.exists(backup):  # the settings as they were before moa first touched them
        shutil.copy(p, backup)
    tmp = p + ".moa-tmp"
    with open(tmp, "w") as f:
        json.dump(s, f, indent=2, ensure_ascii=False)
        f.write("\n")
    if os.path.exists(p):
        shutil.copymode(p, tmp)
    os.replace(tmp, p)


def short(path):
    home = os.path.expanduser("~")
    return "~" + path[len(home):] if path == home or path.startswith(home + "/") else path


def limits(a):
    """How an account's hook is limited, for listings: " (only ~/x; elsewhere: the work moa)"."""
    if not a["only"]:
        return ""
    return f" (only {', '.join(short(x) for x in a['only'])}" + (f"; elsewhere: the {a['else']} moa)" if a["else"] else ")")


def again(a):
    """The moactl setup arguments that reinstall an account as it is."""
    if not a["only"]:
        return f"install {a['name']}"
    return f"only {a['name']} {' '.join(shlex.quote(x) for x in a['only'])}" + (f" --else {a['else']}" if a["else"] else "")


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
        extra = limits(a)
        print(f"  {'✓' if a['state'] == 'installed' else '·'} {a['name']:<16} {short(a['dir']):<22} {a['state']}{extra}  · {ago(a['last_used'])}")
    todo = [a for a in accounts if a["state"] == "not installed"]
    for a in accounts:  # set up before there was a plugin: install again to add it, keeping its folders
        if a["state"] == "installed" and not a["plugin"]:
            print(f"    {a['name']} has no moa plugin yet: moactl setup {again(a)}")
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
        if ans in ("y", "yes", "f", "folders"):
            err = install(a["dir"], default_only if ans[0] == "f" else None)
            skipped = [n for n in skipped if n != a["name"]]
            print(f"    installed, only under {folders}" if ans[0] == "f" else "    installed")
            if err:
                print(f"    but: {err}")
        else:
            skipped = sorted(set(skipped) | {a["name"]})
            print("    skipped")
    print("\nRunning sessions pick this up when they restart.")
    return skipped
