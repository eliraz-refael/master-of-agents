//! The tmux config moa generates: two files (`tmux.conf` and `moa.tmux`) and the hooks the
//! daemon sets when it connects.

use std::path::Path;

use moa_core::cockpit::panel_env_of;
use moa_core::config::{Form, UiConfig};
use moa_core::keymap::{Does, ctrl_a};
use moa_core::panel::View;
use moa_core::state::PanelLaunch;
use moa_core::tmux::cmd::{Cmd, literal_format, quote};
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::mailbox::KeyAction;

const TEMPLATE: &str = include_str!("moa.tmux.tpl");

/// The status line's height: a blank line, then the bar. The menu fits above it.
const STATUS_LINES: usize = 2;

/// The hook index moa owns; a user's hooks at other indexes are never touched.
const HOOK_INDEX: u32 = 90;

/// Help's popup width: the spike's help, whose widest line it fits (S10).
const HELP_WIDTH: u16 = 84;

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

/// What a popup runs: moa's own program, as panels get it, for the brain, at home.
#[derive(Clone, Copy)]
struct Popups<'a> {
    brain: &'a str,
    launch: &'a PanelLaunch,
    home: &'a Path,
}

impl Popups<'_> {
    /// `display-popup` running `moa panel <view> --once` (S8), at home and with the
    /// panels' environment (`panel_env_of`). `-EE` closes it when moa ends well; one that
    /// fails (a broken `moa.toml`, a brain that isn't there) stays open, so what it said is
    /// seen, until a key closes it. The program and its arguments go as separate
    /// arguments, which tmux neither expands nor hands a shell; it expands `-d`, so the
    /// folder is kept literal. `menu`: for a menu item, whose command tmux expands once
    /// before it runs, every `#` in the folder, the environment and the program doubled
    /// once more. The title holds nothing an expansion changes.
    ///
    /// Help's popup is [`HELP_WIDTH`] columns wide, or the whole width on a narrower
    /// terminal: tmux refuses a popup wider than the terminal, and `-w` takes no format, so
    /// a check picks between the two (S10).
    fn command(self, view: View, menu: bool) -> String {
        let popup = |size: &str, title: &str| self.popup(view, size, title, menu);
        let body = match view {
            View::Picker => popup("-w 80% -h 70%", "⌕ find a session"),
            View::Help => format!(
                "if -F '#{{e|<:#{{client_width}},{HELP_WIDTH}}}' {{ {} }} {{ {} }}",
                popup("-w 100% -h 90%", "keys and help"),
                popup(&format!("-w {HELP_WIDTH} -h 90%"), "keys and help"),
            ),
            other => unreachable!("no popup is written for the {other} view"),
        };
        format!(
            "if -F '#{{@moa_ctl}}' {{ {body} }} {{ display-message -d 4000 \"{NOT_RUNNING}\" }}"
        )
    }

    /// One `display-popup` of `size` (its `-w` and `-h`) titled `title`, running `view`.
    fn popup(self, view: View, size: &str, title: &str, menu: bool) -> String {
        let lit = |s: &str| match menu {
            true => literal_format(s),
            false => s.to_string(),
        };
        let mut words = vec![
            format!("display-popup -EE {size} -d"),
            quote(&lit(&literal_format(&self.home.to_string_lossy()))),
            format!("-T \"#[fg=#88c0d0 bold] {title} \""),
        ];
        let program = self.launch.exe.to_string_lossy();
        let args = [program.as_ref(), "panel", view.word(), "--once", "--brain"];
        words.extend(
            panel_env_of(self.brain, self.launch)
                .iter()
                .map(String::as_str)
                .chain(args)
                .chain([self.brain])
                .map(|w| quote(&lit(w))),
        );
        words.join(" ")
    }
}

/// Where `|` and `-` don't split, in the menu and the template's own bindings alike: the
/// cockpit (its panes are the layout's), and the picker's or help's full tab, which waits
/// parked for its next open (a shell split into it would come back with it).
const SPLIT_REFUSED: &str =
    "#{||:#{==:#{window_name},moa},#{||:#{==:#{@moa_view},picker},#{==:#{@moa_view},help}}}";
/// Why, in the refusal: the cockpit's, or a sheet's.
const SPLIT_WHY: &str =
    "#{?#{==:#{window_name},moa},the cockpit is a layout,the picker and help stay whole}";

/// The menu command for what a key does.
fn menu_command(does: Does, popups: Popups) -> String {
    let split = |flag: &str| {
        format!(
            "if -F '{SPLIT_REFUSED}' 'display \"split an agent or terminal tab; {SPLIT_WHY}\"' 'split-window -{flag} -c \"##{{pane_current_path}}\" ; set -p @moa_label \"⌨ shell\"'"
        )
    };
    match does {
        Does::Moa(a) => guarded(a, "-"),
        Does::Popup(view) => popups.command(view, true),
        Does::SplitBeside => split("h"),
        Does::SplitBelow => split("v"),
        Does::BackToMoa => "select-window -t moa:moa".into(),
        Does::Zoom => "resize-pane -Z".into(),
        Does::PrevTab => "previous-window".into(),
        Does::NextTab => "next-window".into(),
        Does::SendCtrlA => "send-keys C-a".into(),
        Does::Move | Does::Menu => String::new(),
    }
}

/// The menu's lines, written from the key table, without the menu's own key. `tall`: a
/// separator between groups, and the greyed note on moving between splits; otherwise
/// neither, for a short terminal.
fn menu_items(tall: bool, form: Form, popups: Popups) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for (i, group) in ctrl_a(form).into_iter().enumerate() {
        if tall && i > 0 {
            items.push("\"\"".into());
        }
        for b in group {
            let item = match b.does {
                // A leading `-` greys the note: tmux shows it but it isn't an item.
                Does::Move if tall => {
                    format!("\"-Ctrl-a {}  {}\" '' ''", b.key, b.label)
                }
                // The menu doesn't list itself.
                Does::Move | Does::Menu => continue,
                _ => format!(
                    "\"{}\" {} {}",
                    b.label,
                    quote(b.key),
                    quote(&menu_command(b.does, popups))
                ),
            };
            items.push(item);
        }
    }
    items
}

fn display_menu(items: &[String]) -> String {
    let mut out =
        String::from("display-menu -T \"#[align=centre fg=#88c0d0 bold] moa \" -x C -y C \\\n");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(" \\\n");
        }
        out.push_str("  ");
        out.push_str(item);
    }
    out
}

/// The menu. tmux 3.4 never opens a menu taller than the terminal, and later versions draw
/// the status line over its end, so a terminal shorter than the tall menu (its lines and
/// the border) and the status line gets the short one. The short one's items all show
/// down to 24 rows (on 3.8, its bottom border is under the status line's blank line).
fn menu(form: Form, popups: Popups) -> String {
    let tall = menu_items(true, form, popups);
    let rows = tall.len() + 2 + STATUS_LINES;
    let indent = |menu: String| menu.replace('\n', "\n  ");
    format!(
        "if -F '#{{e|<:#{{client_height}},{rows}}}' {{\n  {}\n}} {{\n  {}\n}}",
        indent(display_menu(&menu_items(false, form, popups))),
        indent(display_menu(&tall))
    )
}

/// The mouse line (V11): panels take clicks, motion and the wheel, which tmux forwards only
/// with the mouse on.
const MOUSE_ON: &str = "set -g  mouse on                # panels take clicks, hover and the wheel; [ui] mouse = false in moa.toml leaves it to you\n";

/// moa's options, status line, key table and menu for `brain`. With `[ui] mouse` false the
/// file says nothing about the mouse: `mouse off` would override a user who turns it on.
/// `[ui] picker` says which key opens the picker's popup, and whether `?` opens help's
/// (S8, S10); a popup runs `launch`'s program with the panels' environment, at `home`.
pub fn moa_tmux(brain: &str, ui: &UiConfig, launch: &PanelLaunch, home: &Path) -> String {
    let title = if brain == "default" {
        "moa · #W".to_string()
    } else {
        format!("moa {} · #W", in_double_quotes(brain))
    };
    let popups = Popups {
        brain,
        launch,
        home,
    };
    // The daemon's keys and the popups, in the table's order; the plain tmux ones are in
    // the template.
    let mut keys = String::new();
    for b in ctrl_a(ui.picker).into_iter().flatten() {
        let command = match b.does {
            Does::Moa(a) => guarded(a, "##{pane_id}"),
            Does::Popup(view) => popups.command(view, false),
            _ => continue,
        };
        keys.push_str(&format!("bind -T moa {:<7} {command}\n", quote(b.key)));
    }
    let menu = menu(ui.picker, popups);
    // The menu text is inlined into each of the three bindings.
    TEMPLATE
        .replace("{{TITLE}}", &title)
        .replace("{{STATUS_LINES}}", &STATUS_LINES.to_string())
        .replace("{{MOUSE}}\n", if ui.mouse { MOUSE_ON } else { "" })
        .replace("{{DAEMON_KEYS}}", keys.trim_end())
        .replace("{{MENU}}", &menu)
        .replace("{{REPAIR}}", &guarded(KeyAction::Repair, "-"))
        .replace("{{SPLIT_REFUSED}}", SPLIT_REFUSED)
        .replace("{{SPLIT_WHY}}", SPLIT_WHY)
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
