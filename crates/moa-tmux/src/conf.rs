//! The tmux config moa generates: two files (`tmux.conf` and `moa.tmux`) and the hooks the
//! daemon sets when it connects.

use std::path::Path;

use moa_core::tmux::cmd::{Cmd, quote};
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::mailbox::KeyAction;

const TEMPLATE: &str = include_str!("moa.tmux.tpl");

/// The hook index moa owns; a user's hooks at other indexes are never touched.
const HOOK_INDEX: u32 = 90;

/// What a key says when moa isn't running.
const NOT_RUNNING: &str = "moa isn't running: run moa";

/// Inside a tmux double-quoted string: `\`, `"`, `$` and `#` are escaped.
fn in_double_quotes(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' | '"' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '#' => out.push_str("##"),
            _ => out.push(c),
        }
    }
    out
}

/// Tells the daemon about a key or menu item, or says moa isn't running. `pane` is
/// `##{pane_id}` for a key and `-` for a menu, which has no pane.
fn guarded(action: KeyAction, pane: &str) -> String {
    format!(
        "if -F '#{{@moa_ctl}}' {{ run -C \"display-message -c '#{{@moa_ctl}}' 'moa:key {} {pane} ##{{session_id}} ##{{client_name}}'\" }} {{ display-message -d 4000 \"{NOT_RUNNING}\" }}",
        action.word()
    )
}

/// The menu: the spike's items, with the daemon's going through `guarded`.
fn menu() -> String {
    let split = |flag: &str| {
        format!(
            "if -F '#{{==:#{{window_name}},moa}}' 'display \"split an agent or terminal tab; the cockpit is a layout\"' 'split-window -{flag} -c \"##{{pane_current_path}}\" ; set -p @moa_label \"⌨ shell\"'"
        )
    };
    let daemon = |a: KeyAction| (a.label(), a.key(), guarded(a, "-"));
    let plain = |label: &'static str, key: &'static str, cmd: &str| (label, key, cmd.to_string());
    let items: Vec<Option<(&str, &str, String)>> = vec![
        Some(daemon(KeyAction::Console)),
        Some(daemon(KeyAction::Tree)),
        Some(daemon(KeyAction::Plan)),
        Some(daemon(KeyAction::Agent)),
        Some(daemon(KeyAction::Find)),
        Some(daemon(KeyAction::New)),
        Some(daemon(KeyAction::Terminal)),
        Some(plain("Split: shell beside", "|", &split("h"))),
        Some(plain("Split: shell below", "-", &split("v"))),
        Some(plain("Back to moa", "m", "select-window -t moa:moa")),
        Some(plain("Zoom in/out", "z", "resize-pane -Z")),
        None,
        Some(daemon(KeyAction::Layout(1))),
        Some(daemon(KeyAction::Layout(2))),
        Some(daemon(KeyAction::Layout(3))),
        Some(daemon(KeyAction::Layout(4))),
        Some(daemon(KeyAction::Repair)),
        None,
        // A greyed note, not an item.
        Some(("-Ctrl-a ←↑↓→  move between splits", "", String::new())),
        Some(plain("Previous tab", "[", "previous-window")),
        Some(plain("Next tab", "]", "next-window")),
        Some(daemon(KeyAction::Help)),
        Some(plain("Send Ctrl-a to the app", "a", "send-keys C-a")),
    ];
    let mut out =
        String::from("display-menu -T \"#[align=centre fg=#88c0d0 bold] moa \" -x C -y C \\\n");
    let last = items.len() - 1;
    for (i, item) in items.iter().enumerate() {
        out.push_str("  ");
        match item {
            Some((label, key, cmd)) => {
                out.push_str(&format!("\"{label}\" {} {}", quote(key), quote(cmd)));
            }
            None => out.push_str("\"\""),
        }
        if i != last {
            out.push_str(" \\\n");
        }
    }
    out
}

/// The mouse line (V11): panels take clicks, motion and the wheel, which tmux forwards only
/// with the mouse on.
const MOUSE_ON: &str = "set -g  mouse on               # panels take clicks, hover and the wheel; [ui] mouse = false in moa.toml leaves it to you\n";

/// moa's options, status line, key table and menu for `brain`. With `mouse` false the file
/// says nothing about the mouse: `mouse off` would override a user who turns it on.
pub fn moa_tmux(brain: &str, mouse: bool) -> String {
    let title = if brain == "default" {
        "moa · #W".to_string()
    } else {
        format!("moa {} · #W", in_double_quotes(brain))
    };
    let mut keys = String::new();
    for a in KeyAction::ALL {
        keys.push_str(&format!(
            "bind -T moa {:<7} {}\n",
            quote(a.key()),
            guarded(a, "##{pane_id}")
        ));
    }
    let menu = menu();
    // The menu text is inlined into each of the three bindings.
    TEMPLATE
        .replace("{{TITLE}}", &title)
        .replace("{{MOUSE}}\n", if mouse { MOUSE_ON } else { "" })
        .replace("{{DAEMON_KEYS}}", keys.trim_end())
        .replace("{{MENU}}", &menu)
        .replace("{{REPAIR}}", &guarded(KeyAction::Repair, "-"))
}

/// The `-f` file: the user's own config first, then moa's. `-f` replaces tmux's own search,
/// so this loads what tmux would, in its order and each file quietly: `/etc/tmux.conf`,
/// `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf` and `~/.config/tmux/tmux.conf`. tmux
/// loads all that exist, and a file twice named (`$XDG_CONFIG_HOME` is `~/.config`) once, so
/// the XDG line is skipped when the variable is empty or is `~/.config`.
pub fn tmux_conf(moa_tmux: &Path) -> String {
    format!(
        "source-file -q /etc/tmux.conf\n\
         source-file -q ~/.tmux.conf\n\
         %if \"#{{&&:#{{!=:$XDG_CONFIG_HOME,}},#{{!=:$XDG_CONFIG_HOME,$HOME/.config}}}}\"\n\
         source-file -q \"$XDG_CONFIG_HOME/tmux/tmux.conf\"\n\
         %endif\n\
         source-file -q ~/.config/tmux/tmux.conf\n\
         source-file {}\n",
        quote(&moa_tmux.to_string_lossy())
    )
}

fn hook_name(name: &str) -> String {
    format!("{name}[{HOOK_INDEX}]")
}

/// The hooks moa sets, by tmux's name. Each tells the daemon what happened as
/// `moa:hook <word> #{<format>}`; `client-detached` is the one that forgets the control
/// client instead. `hooks` and `unhooks` both come from this table.
const HOOK_TABLE: [(&str, Option<(&str, &str)>); 5] = [
    ("pane-exited", Some(("pane-exited", "hook_pane"))),
    ("pane-died", Some(("pane-died", "hook_pane"))),
    ("window-layout-changed", Some(("layout", "hook_window"))),
    ("client-attached", Some(("client-attached", "hook_client"))),
    ("client-detached", None),
];

/// The hooks that tell the daemon (through its control client `ctl`) what happened, and the
/// one that forgets `ctl` when it goes. Set again on every connect.
pub fn hooks(ctl: &ClientName) -> Vec<Cmd> {
    let ctl = quote(&ctl.0);
    HOOK_TABLE
        .iter()
        .map(|(name, tell)| {
            let body = match tell {
                Some((what, arg)) => {
                    format!("display-message -c {ctl} 'moa:hook {what} #{{{arg}}}'")
                }
                None => {
                    "if -F '#{==:#{hook_client},#{@moa_ctl}}' { set-option -gu @moa_ctl }".into()
                }
            };
            Cmd::new("set-hook").args(["-g".to_string(), hook_name(name), body])
        })
        .collect()
}

/// Removes exactly the hooks `hooks` sets; a user's hooks at other indexes stay.
pub fn unhooks() -> Vec<Cmd> {
    HOOK_TABLE
        .iter()
        .map(|(name, _)| Cmd::new("set-hook").args(["-gu".to_string(), hook_name(name)]))
        .collect()
}
