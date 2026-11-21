//! The one table of moa's `Ctrl-a` keys. moa's tmux file writes its menu and its daemon keys
//! from it, and the help view reads it, so help can't drift from what the keys do.

use crate::config::Form;
use crate::panel::View;
use crate::tmux::mailbox::KeyAction;

/// What a key does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Does {
    /// Tells moa, through the mailbox.
    Moa(KeyAction),
    /// Opens the view in a popup over the tab you're on: a `moa panel <view> --once` that
    /// tmux starts itself (S8).
    Popup(View),
    /// A shell beside the split you're on.
    SplitBeside,
    /// A shell below the split you're on.
    SplitBelow,
    BackToMoa,
    Zoom,
    PrevTab,
    NextTab,
    /// A real `Ctrl-a` for the app.
    SendCtrlA,
    /// Moving between splits with the arrows: a greyed note in the menu, not an item.
    Move,
    /// The menu itself: help lists it, but the menu doesn't, and the template binds it (with
    /// a second `Ctrl-a`).
    Menu,
}

/// A key after `Ctrl-a`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
    /// The key, as tmux names it. `Move`'s is the four arrows as they're shown, `←↑↓→`: it
    /// isn't one key. No binding is written from `Move` or `Menu`.
    pub key: &'static str,
    /// The menu's text.
    pub label: &'static str,
    /// The help view's text.
    pub help: &'static str,
    pub does: Does,
}

/// A key moa acts on: its key and menu text are the action's own.
fn moa(action: KeyAction, help: &'static str) -> Binding {
    Binding {
        key: action.key(),
        label: action.label(),
        help,
        does: Does::Moa(action),
    }
}

fn plain(key: &'static str, label: &'static str, help: &'static str, does: Does) -> Binding {
    Binding {
        key,
        label,
        help,
        does,
    }
}

/// The picker's two keys: `/` opens the form `form` names, `s` the other (S8). The popup
/// is a binding of its own; the full tab is the daemon's `find`.
fn find_keys(form: Form) -> [Binding; 2] {
    let (popup, tab) = (Does::Popup(View::Picker), Does::Moa(KeyAction::Find));
    let (main, other, label, help) = match form {
        Form::Popup => (
            popup,
            tab,
            "Find a session, full tab…",
            "find a session in a full tab: type part of its name, Enter jumps",
        ),
        Form::Tab => (
            tab,
            popup,
            "Find a session, popup…",
            "find a session in a popup: type part of its name, Enter jumps",
        ),
    };
    [
        plain(
            "/",
            "Find a session…",
            "find a session: type part of its name, Enter jumps",
            main,
        ),
        plain("s", label, help, other),
    ]
}

/// Help's key, `?`, in the form `form` names, as `/` is (S10). The popup is a binding of its
/// own; the full tab is the daemon's `help`.
fn help_key(form: Form) -> Binding {
    let does = match form {
        Form::Popup => Does::Popup(View::Help),
        Form::Tab => Does::Moa(KeyAction::Help),
    };
    plain(
        KeyAction::Help.key(),
        KeyAction::Help.label(),
        "this help: every key moa has",
        does,
    )
}

/// moa's `Ctrl-a` keys, in the menu's groups and order, with the picker's and help's keys
/// as `form` has them; a separator goes between groups.
pub fn ctrl_a(form: Form) -> Vec<Vec<Binding>> {
    let [find, find_other] = find_keys(form);
    vec![
        vec![
            moa(KeyAction::Console, "go to the console panel"),
            moa(
                KeyAction::Tree,
                "the sessions tree: every session, by group",
            ),
            moa(
                KeyAction::Plan,
                "the big picture: what every session is doing",
            ),
            moa(
                KeyAction::Agent,
                "the agent spot: the session open in the cockpit",
            ),
            find,
            find_other,
            moa(KeyAction::New, "start a new session"),
            moa(
                KeyAction::Terminal,
                "a plain terminal in a new tab (for a dev server…), in the folder of the \
                 agent or session you're on; `exit` closes it",
            ),
            plain(
                "|",
                "Split: shell beside",
                "split the agent or terminal tab you're on: a shell beside it, in the same \
                 folder; `exit` closes it",
                Does::SplitBeside,
            ),
            plain(
                "-",
                "Split: shell below",
                "split the agent or terminal tab you're on: a shell below it, in the same \
                 folder; `exit` closes it",
                Does::SplitBelow,
            ),
            plain(
                "m",
                "Back to moa",
                "back to moa's cockpit, from an agent's own tab",
                Does::BackToMoa,
            ),
            plain(
                "z",
                "Zoom in/out",
                "zoom the panel or split you're on to the whole tab (again to unzoom)",
                Does::Zoom,
            ),
        ],
        vec![
            // Only the layout's name: the user can change what a layout holds, so help
            // lists that from moa's own layouts.
            moa(KeyAction::Layout(1), "switch to the cockpit layout"),
            moa(KeyAction::Layout(2), "switch to the focus layout"),
            moa(KeyAction::Layout(3), "switch to the classic layout"),
            moa(KeyAction::Layout(4), "switch to the triage layout"),
            moa(
                KeyAction::Repair,
                "restart any of moa's panels that stopped",
            ),
        ],
        vec![
            plain(
                "Space",
                "The full menu",
                "the full menu (or Ctrl-a twice): every key here by name, with new session, \
                 layouts and repair",
                Does::Menu,
            ),
            plain(
                "←↑↓→",
                "move between splits",
                "move to the panel or split that way (or h j k l); tap more arrows right \
                 after to keep moving",
                Does::Move,
            ),
            plain("[", "Previous tab", "the previous tab", Does::PrevTab),
            plain("]", "Next tab", "the next tab", Does::NextTab),
            help_key(form),
            plain(
                "a",
                "Send Ctrl-a to the app",
                "a real Ctrl-a for the app (start of line)",
                Does::SendCtrlA,
            ),
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` says "pane" or "window" (or their plurals) as a whole word; "panel" is fine.
    fn says_tmux_words(text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.split(|c: char| !c.is_alphanumeric()).any(|word| {
            let one = word.strip_suffix('s').unwrap_or(word);
            one == "pane" || one == "window"
        })
    }

    const FORMS: [Form; 2] = [Form::Popup, Form::Tab];

    #[test]
    fn every_binding_has_help() {
        for b in FORMS.into_iter().flat_map(ctrl_a).flatten() {
            assert!(!b.help.trim().is_empty(), "{b:?} has no help");
            assert!(!says_tmux_words(b.help), "{b:?}: its help");
            assert!(!says_tmux_words(b.label), "{b:?}: its label");
        }
    }

    fn binding(form: Form, key: &str) -> Binding {
        ctrl_a(form)
            .into_iter()
            .flatten()
            .find(|b| b.key == key)
            .unwrap_or_else(|| panic!("{key} isn't in the table"))
    }

    /// S8: `/` opens the form `[ui] picker` names and `s` the other; the full tab is the
    /// daemon's `find`.
    #[test]
    fn the_picker_form_picks_the_keys() {
        let popup = Does::Popup(View::Picker);
        let tab = Does::Moa(KeyAction::Find);
        assert_eq!(binding(Form::Popup, "/").does, popup);
        assert_eq!(binding(Form::Popup, "s").does, tab);
        assert_eq!(binding(Form::Tab, "/").does, tab);
        assert_eq!(binding(Form::Tab, "s").does, popup);
        assert_eq!(binding(Form::Popup, "/").label, "Find a session…");
        assert_eq!(binding(Form::Tab, "/").label, "Find a session…");
        assert_eq!(binding(Form::Popup, "s").label, "Find a session, full tab…");
        assert_eq!(binding(Form::Tab, "s").label, "Find a session, popup…");
    }

    #[test]
    fn the_word_check_catches_tmux_words_only() {
        assert!(says_tmux_words("split the Pane"));
        assert!(says_tmux_words("two windows."));
        assert!(!says_tmux_words("restart moa's panels"));
    }

    /// S10: `?` opens help in the form `[ui] picker` names: the popup, or the daemon's
    /// `help`, which opens the full tab.
    #[test]
    fn the_picker_form_picks_helps_key() {
        assert_eq!(binding(Form::Popup, "?").does, Does::Popup(View::Help));
        assert_eq!(binding(Form::Tab, "?").does, Does::Moa(KeyAction::Help));
        for form in FORMS {
            assert_eq!(binding(form, "?").label, "Keys and help");
        }
    }

    /// The daemon's keys are written from the table, so every action must be in it, in
    /// either form: once as the daemon's key, or as its view's popup (S8, S10). Help is in
    /// the table once, and find twice: `/` and `s`.
    #[test]
    fn every_key_action_is_in_the_table() {
        for form in FORMS {
            let table = ctrl_a(form);
            for a in KeyAction::ALL {
                let popup = match a {
                    KeyAction::Find => Some(Does::Popup(View::Picker)),
                    KeyAction::Help => Some(Does::Popup(View::Help)),
                    _ => None,
                };
                let daemon = table
                    .iter()
                    .flatten()
                    .filter(|b| b.does == Does::Moa(a))
                    .count();
                let popups = table
                    .iter()
                    .flatten()
                    .filter(|b| Some(b.does) == popup)
                    .count();
                let want = match a {
                    KeyAction::Find => (1, 1),
                    KeyAction::Help if form == Form::Popup => (0, 1),
                    _ => (1, 0),
                };
                assert_eq!((daemon, popups), want, "{form:?}: {a:?}");
            }
        }
    }

    #[test]
    fn no_key_is_in_the_table_twice() {
        for form in FORMS {
            let keys: Vec<&str> = ctrl_a(form).into_iter().flatten().map(|b| b.key).collect();
            for k in &keys {
                assert_eq!(keys.iter().filter(|x| *x == k).count(), 1, "{form:?}: {k}");
            }
        }
    }
}
