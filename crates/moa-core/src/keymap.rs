//! The one table of moa's `Ctrl-a` keys. moa's tmux file writes its menu and its daemon keys
//! from it, and the help view reads it, so help can't drift from what the keys do.

use crate::tmux::mailbox::KeyAction;

/// What a key does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Does {
    /// Tells moa, through the mailbox.
    Moa(KeyAction),
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
}

/// A key after `Ctrl-a`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
    /// The key, as tmux names it. `Move`'s is the four arrows as they're shown, `←↑↓→`: it
    /// isn't one key, and no binding is written from it.
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

/// moa's `Ctrl-a` keys, in the menu's groups and order; a separator goes between groups.
pub fn ctrl_a() -> Vec<Vec<Binding>> {
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
            moa(
                KeyAction::Find,
                "find a session: type part of its name, Enter jumps",
            ),
            moa(
                KeyAction::FindOther,
                "find a session in a full tab: type part of its name, Enter jumps",
            ),
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
                "←↑↓→",
                "move between splits",
                "move to the split that way (or h j k l); tap more arrows right after to \
                 keep moving",
                Does::Move,
            ),
            plain("[", "Previous tab", "the previous tab", Does::PrevTab),
            plain("]", "Next tab", "the next tab", Does::NextTab),
            moa(KeyAction::Help, "this help: every key moa has"),
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

    #[test]
    fn every_binding_has_help() {
        for b in ctrl_a().into_iter().flatten() {
            assert!(!b.help.trim().is_empty(), "{b:?} has no help");
            assert!(!says_tmux_words(b.help), "{b:?}: its help");
            assert!(!says_tmux_words(b.label), "{b:?}: its label");
        }
    }

    #[test]
    fn the_word_check_catches_tmux_words_only() {
        assert!(says_tmux_words("split the Pane"));
        assert!(says_tmux_words("two windows."));
        assert!(!says_tmux_words("restart moa's panels"));
    }

    /// The daemon's keys are written from the table, so every action must be in it, once.
    #[test]
    fn every_key_action_is_in_the_table_once() {
        let table = ctrl_a();
        for a in KeyAction::ALL {
            let n = table
                .iter()
                .flatten()
                .filter(|b| b.does == Does::Moa(a))
                .count();
            assert_eq!(n, 1, "{a:?}");
        }
    }

    #[test]
    fn no_key_is_in_the_table_twice() {
        let keys: Vec<&str> = ctrl_a().into_iter().flatten().map(|b| b.key).collect();
        for k in &keys {
            assert_eq!(keys.iter().filter(|x| *x == k).count(), 1, "{k}");
        }
    }
}
