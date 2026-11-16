//! The tmux config moa generates: two files (`tmux.conf` and `moa.tmux`) and the hooks the
//! daemon sets when it connects.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use moa_core::tmux::cmd::{Cmd, quote};
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::mailbox::KeyAction;

const TEMPLATE: &str = include_str!("moa.tmux.tpl");

/// The hook index moa owns; a user's hooks at other indexes are never touched.
const HOOK_INDEX: u32 = 90;

/// What a key says when moa isn't running.
const NOT_RUNNING: &str = "moa isn't running: run moa";

/// The hooks moa sets, by tmux's name.
const HOOK_NAMES: [&str; 5] = [
    "pane-exited",
    "pane-died",
    "window-layout-changed",
    "client-attached",
    "client-detached",
];

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
            "if -F '#{{==:#{{window_name}},moa}}' 'display \"split an agent or terminal window; the cockpit is a layout\"' 'split-window -{flag} -c \"##{{pane_current_path}}\" ; set -p @moa_label \"⌨ shell\"'"
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
        Some(plain("Zoom this pane", "z", "resize-pane -Z")),
        None,
        Some(daemon(KeyAction::Layout(1))),
        Some(daemon(KeyAction::Layout(2))),
        Some(daemon(KeyAction::Layout(3))),
        Some(daemon(KeyAction::Layout(4))),
        Some(daemon(KeyAction::Repair)),
        None,
        // A greyed note, not an item.
        Some(("-Ctrl-a ←↑↓→  move between panes", "", String::new())),
        Some(plain("Previous window", "[", "previous-window")),
        Some(plain("Next window", "]", "next-window")),
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

/// moa's options, status line, key table and menu for `brain`.
pub fn moa_tmux(brain: &str) -> String {
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
    // Each use of the menu is its own binding, so its continuation lines are shared text.
    TEMPLATE
        .replace("{{TITLE}}", &title)
        .replace("{{DAEMON_KEYS}}", keys.trim_end())
        .replace("{{MENU}}", &menu)
        .replace("{{REPAIR}}", &guarded(KeyAction::Repair, "-"))
}

/// The `-f` file: the user's own config first, then moa's.
pub fn tmux_conf(moa_tmux: &Path) -> String {
    format!(
        "source-file -q ~/.tmux.conf\nsource-file {}\n",
        quote(&moa_tmux.to_string_lossy())
    )
}

/// Writes `text` to `path` through a temp file and a rename, mode 0600. `false` when the file
/// already holds exactly `text`.
pub fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(false);
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| true)
}

fn hook_name(name: &str) -> String {
    format!("{name}[{HOOK_INDEX}]")
}

/// The hooks that tell the daemon (through its control client `ctl`) what happened, and the
/// one that forgets `ctl` when it goes. Set again on every connect.
pub fn hooks(ctl: &ClientName) -> Vec<Cmd> {
    let ctl = quote(&ctl.0);
    let tell = |name: &str, what: &str, arg: &str| {
        let body = format!("display-message -c {ctl} 'moa:hook {what} #{{{arg}}}'");
        Cmd::new("set-hook").args(["-g".to_string(), hook_name(name), body])
    };
    vec![
        tell("pane-exited", "pane-exited", "hook_pane"),
        tell("pane-died", "pane-died", "hook_pane"),
        tell("window-layout-changed", "layout", "hook_window"),
        tell("client-attached", "client-attached", "hook_client"),
        Cmd::new("set-hook").args([
            "-g".to_string(),
            hook_name("client-detached"),
            "if -F '#{==:#{hook_client},#{@moa_ctl}}' { set-option -gu @moa_ctl }".to_string(),
        ]),
    ]
}

/// Removes exactly the hooks `hooks` sets; a user's hooks at other indexes stay.
pub fn unhooks() -> Vec<Cmd> {
    HOOK_NAMES
        .iter()
        .map(|n| Cmd::new("set-hook").args(["-gu".to_string(), hook_name(n)]))
        .collect()
}
