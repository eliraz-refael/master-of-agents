//! The idle terminals (D6): the open ones nobody has touched for `terminal_idle_ms`. A
//! wake-up carries those that went idle since the last one ([`super::prompt`]), so moa can
//! offer to close one that looks forgotten. Nothing is saved for them.

use crate::model::Terminal;
use crate::ops::term::with_tilde;
use crate::state::State;
use crate::terminals::{is_active, is_idle, open_records};
use crate::text::{cut, one_line};
use crate::time::{HOUR, words};
use crate::{Now, UtcMs};

/// The most terminals one prompt carries.
pub const IDLE_MAX: usize = 10;

/// The most characters of a terminal's name, or its command, in a line.
const FIELD_MAX: usize = 40;

/// When open record `t` was last touched: its open, when never.
fn last_touched(t: &Terminal) -> UtcMs {
    t.touched_at.unwrap_or(t.opened_at)
}

/// The open terminals idle now, and with `since`, only those that went idle after it (their
/// last touch plus `terminal_idle_ms`): oldest touched first, at most [`IDLE_MAX`]. None
/// when `terminal_idle_ms` is 0.
pub fn idle_terms<'a>(state: &'a State, now: &Now, since: Option<UtcMs>) -> Vec<&'a Terminal> {
    let idle_ms = state.config().ui.terminal_idle_ms;
    let mut idle: Vec<&Terminal> = open_records(state)
        .filter(|t| is_idle(last_touched(t), is_active(state, t), idle_ms, now.wall))
        .filter(|t| since.is_none_or(|s| last_touched(t).0.saturating_add(idle_ms) > s.0))
        .collect();
    idle.sort_by_key(|t| (last_touched(t), t.id));
    idle.truncate(IDLE_MAX);
    idle
}

/// The idle block (D10) of `terms`: a line each, with its folder, how many whole hours it
/// has been untouched, and what it runs.
pub fn block(state: &State, now: &Now, terms: &[&Terminal]) -> String {
    let deps = state.deps();
    let field = |s: &str| cut(&one_line(s), FIELD_MAX);
    let lines: Vec<String> = terms
        .iter()
        .map(|t| {
            let hours = now.wall.0.saturating_sub(last_touched(t).0) / HOUR;
            let runs = match t.command == t.shell {
                true => "at its prompt".to_string(),
                false => format!("running {}", field(&t.command)),
            };
            format!(
                "- #{} {} · {} · idle {hours} h · {runs}",
                t.id.0,
                field(&t.name),
                one_line(&with_tilde(&t.cwd, &deps.home, deps.canonical)),
            )
        })
        .collect();
    format!(
        "Idle terminals (untouched for {} or more; offer to close one that looks forgotten, \
         as a suggestion with --do \"term close ID\"):\n{}",
        words(state.config().ui.terminal_idle_ms),
        lines.join("\n")
    )
}
