//! The idle terminals (D6): the open ones nobody has touched for `terminal_idle_ms`. A
//! wake-up carries those not told yet ([`super::prompt`]), so moa can offer to close one
//! that looks forgotten. What was told is kept in memory ([`super::Moa::idle_told`]), not
//! saved.

use crate::model::{TermId, Terminal};
use crate::ops::term::with_tilde;
use crate::state::State;
use crate::terminals::{is_active, is_idle, open_records};
use crate::text::{cut, one_line};
use crate::time::{span, words};
use crate::{Now, UtcMs};

/// The most terminals one prompt carries.
pub const IDLE_MAX: usize = 10;

/// The most characters of a terminal's name, or its command, in a line.
const FIELD_MAX: usize = 40;

/// Where the idle terminals told so far end, in the order they are told: the last one's idle
/// start, and its id. Terminals often go idle at the same moment (found together, never
/// touched), so the id breaks the tie.
pub type IdleMark = (UtcMs, TermId);

/// When open record `t` was last touched: its open, when never.
fn last_touched(t: &Terminal) -> UtcMs {
    t.touched_at.unwrap_or(t.opened_at)
}

/// Where idle record `t` stands among the idle ones: when it went idle (its last touch plus
/// `terminal_idle_ms`), and its id.
pub fn mark(state: &State, t: &Terminal) -> IdleMark {
    let idle_ms = state.config().ui.terminal_idle_ms;
    (UtcMs(last_touched(t).0.saturating_add(idle_ms)), t.id)
}

/// The open terminals idle now, and with `since`, only those after it ([`mark`]): oldest
/// idle first, at most [`IDLE_MAX`], so the rest come next time. None when
/// `terminal_idle_ms` is 0.
pub fn idle_terms<'a>(state: &'a State, now: &Now, since: Option<IdleMark>) -> Vec<&'a Terminal> {
    let idle_ms = state.config().ui.terminal_idle_ms;
    let mut idle: Vec<&Terminal> = open_records(state)
        .filter(|t| is_idle(last_touched(t), is_active(state, t), idle_ms, now.wall))
        .filter(|t| since.is_none_or(|s| mark(state, t) > s))
        .collect();
    idle.sort_by_key(|t| mark(state, t));
    idle.truncate(IDLE_MAX);
    idle
}

/// The idle block (D10) of `terms`: a line each, with its folder, how long it has been
/// untouched ([`span`]: `terminal_idle_hours` may be under an hour), and what it runs.
pub fn block(state: &State, now: &Now, terms: &[&Terminal]) -> String {
    let deps = state.deps();
    let field = |s: &str| cut(&one_line(s), FIELD_MAX);
    let lines: Vec<String> = terms
        .iter()
        .map(|t| {
            let idle = span(now.wall.0.saturating_sub(last_touched(t).0));
            let runs = match t.command == t.shell {
                true => "at its prompt".to_string(),
                false => format!("running {}", field(&t.command)),
            };
            format!(
                "- #{} {} · {} · idle {idle} · {runs}",
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
