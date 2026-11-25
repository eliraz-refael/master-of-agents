//! What a prompt to moa's agent carries besides your text (D1), and how its commit marks it
//! delivered. The session reports' blocks are [`super::reports`]'; your yeses' block is
//! here (D5, D10).

use super::reports::cut_marked;
use crate::change::Change;
use crate::model::{Suggestion, YES_TOLD};
use crate::text::one_line;

/// The most characters of a suggestion's text in the yes block; a longer one ends in `…`, so
/// one yes can't push a prompt past [`super::PROMPT_MAX`].
pub const YES_MAX: usize = 500;

/// The yes block (D10): your yes to each of `yeses`, suggestions with nothing to run, one
/// line each (at most [`YES_MAX`] characters of its text), and what moa does with them.
pub fn yes_text(yeses: &[&Suggestion]) -> String {
    let lines: Vec<String> = yeses
        .iter()
        .map(|s| format!("- #{}: {}", s.id, cut_marked(&one_line(&s.text), YES_MAX)))
        .collect();
    format!(
        "(moa · the user said yes to your suggestion(s), not typed by the user)\n{}\n\
         Do what each one proposed.",
        lines.join("\n")
    )
}

/// Each of `yeses` marked told ([`YES_TOLD`]), for the commit of the prompt that carries
/// them.
pub(crate) fn told(yeses: &[&Suggestion]) -> Vec<Change> {
    yeses
        .iter()
        .map(|s| {
            let mut row = (*s).clone();
            row.result = Some(YES_TOLD.to_string());
            Change::Suggestion(row)
        })
        .collect()
}
