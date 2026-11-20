//! The picker's own keys (S7): the query's editing, moving through the matches, and a pick.

use moa_core::op::Op;

use super::{Out, Panel};
use crate::data::Data;
use crate::editor::Editor;
use crate::keys::Key;
use crate::line::Item;
use crate::list::ListState;
use crate::views::picker;

impl Panel {
    /// The picker's keys (S7). They come before every view's moving keys, so `j`, `k`, `q`
    /// and a space are typed into the query; every edit selects the first row again, and
    /// the selection stops at the ends.
    pub(super) fn pick_key(&mut self, k: Key, data: &Data, body: usize) -> Vec<Out> {
        match k {
            Key::Enter => return self.pick(data),
            Key::Esc | Key::Ctrl('g') => return vec![Out::Close],
            Key::Up | Key::Ctrl('p') | Key::BackTab => self.list.step(&self.lines, false),
            Key::Down | Key::Ctrl('n') | Key::Tab => self.list.step(&self.lines, true),
            Key::PageUp | Key::PageDown => {
                self.list.page(&self.lines, body, k == Key::PageDown);
            }
            Key::Backspace | Key::Ctrl('u') | Key::Ctrl('w') => {
                self.edit_query(|e| {
                    e.key(k);
                });
            }
            Key::Char(c) if !c.is_control() => {
                self.edit_query(|e| {
                    e.key(k);
                });
            }
            _ => {}
        }
        Vec::new()
    }

    /// Edits the picker's query as the footer's editor edits its text, and selects the
    /// first row again: the next build lists the new matches from the top.
    pub(super) fn edit_query(&mut self, edit: impl FnOnce(&mut Editor)) {
        let mut editor = Editor::new("", std::mem::take(&mut self.ui.query));
        edit(&mut editor);
        self.ui.query = editor.text;
        self.list = ListState::default();
        self.hover = None;
    }

    /// Enter or a click in the picker: the selected session is focused, a terminal shown,
    /// and the footer says what is opening; its reply decides whether the picker closes.
    /// With no row listed, the picker closes, as the spike's did.
    pub(super) fn pick(&mut self, data: &Data) -> Vec<Out> {
        let Some(item) = self.selected().cloned() else {
            return vec![Out::Close];
        };
        let op = match &item {
            Item::Session(id) => Op::SessionFocus { q: id.to_string() },
            Item::Term { pane } => Op::TermShow { pane: pane.clone() },
            Item::Group(_) | Item::Task(_) | Item::Project(_) | Item::Suggestion(_) => {
                return Vec::new();
            }
        };
        let title = picker::title(data, &item).unwrap_or_default();
        self.say(&format!("opening {title}…"));
        self.picking = true;
        vec![Out::Send(op)]
    }
}
