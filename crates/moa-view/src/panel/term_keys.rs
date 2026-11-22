//! The terminals view's own keys (5d): close, name, note and timeout the selected terminal.
//! They name it by its tab id, which every `term.*` op takes; the core decides what is true.

use moa_core::op::Op;

use super::ask::Ask;
use super::{Out, Panel};
use crate::data::Data;
use crate::keys::Action;
use crate::line::Item;
use crate::views::picker::term_title;

/// What the timeout prompt starts with.
const TIMEOUT_PREFILL: &str = "2h";

impl Panel {
    /// `x`, `r`, `n` and `t` on the selected terminal. `x` always asks first, whether or not
    /// something is running in it; the core still decides how it closes on a fresh look
    /// (T8). `None` with no terminal selected, so the key means nothing.
    pub(super) fn term_key(&mut self, action: Action, data: &Data) -> Option<Vec<Out>> {
        let Some(Item::Term { pane }) = self.selected().cloned() else {
            return None;
        };
        let row = data.terms.get(&pane)?;
        let name = term_title(&pane, row);
        Some(match action {
            Action::Close => {
                let question = if row.running {
                    format!("close {name}, running {}? y/N", row.command)
                } else {
                    format!("close {name}? y/N")
                };
                self.confirm(question, Op::TermClose { target: pane })
            }
            Action::Rename => {
                let ask = Ask::TermName { target: pane };
                self.ask("name:".to_string(), row.name.clone(), ask)
            }
            Action::Note => {
                let ask = Ask::TermNote { target: pane };
                self.ask(
                    "note:".to_string(),
                    row.note.clone().unwrap_or_default(),
                    ask,
                )
            }
            Action::Timeout => {
                let ask = Ask::TermTimeout { target: pane };
                self.ask(
                    "close in (3h, 90m, off):".to_string(),
                    TIMEOUT_PREFILL.to_string(),
                    ask,
                )
            }
            _ => return None,
        })
    }
}
