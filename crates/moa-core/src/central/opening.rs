//! moa opens the day (D2, D3, C19): the first time the user is here each local day, moa's
//! agent is sent one prompt that opens it ([`super::channel::Source::Brief`]), or the user's
//! own first prompt opens it instead. Only presence starts it ([`present`]), never midnight
//! itself: moa pays for no turn nobody reads. Which day was opened last is saved (`meta`
//! [`BRIEFED_KEY`]) by the commit of the prompt that carried the opening, so a restart the
//! same day doesn't open it again.

use crate::Now;
use crate::change::Change;
use crate::state::State;

/// The `meta` key holding the last day moa opened, as `YYYY-MM-DD`.
pub const BRIEFED_KEY: &str = "moa.briefed";

/// The opening block (D10): what moa's agent is told when the prompt opens the day.
pub const OPENING: &str = "(moa · the user is here for the first time today, not typed by the user)\n\
                           Open the day: brief the user as a thread's first message does (see \
                           \"How to talk\" in your instructions).";

/// The user is here (D2): when today isn't opened yet, the opening is due today
/// ([`super::Moa::brief_due`]). The callers are the user's own inputs only, never moa's.
pub fn present(state: &mut State, now: &Now) {
    if !opened(state, now) {
        state.central.brief_due = Some(now.local_day);
    }
}

/// Whether the opening is due now: the user was here today, and today isn't opened yet.
pub(crate) fn due_today(state: &State, now: &Now) -> bool {
    state.central.brief_due == Some(now.local_day)
}

/// An opening due on a day that has passed is dropped (R8): the user being here yesterday
/// doesn't open today, nor wake moa in the night.
pub(crate) fn expire(state: &mut State, now: &Now) {
    if !due_today(state, now) {
        state.central.brief_due = None;
    }
}

/// Whether today was opened: `meta` [`BRIEFED_KEY`] is today's date.
fn opened(state: &State, now: &Now) -> bool {
    state.meta(BRIEFED_KEY) == Some(now.local_day.to_string().as_str())
}

/// Today marked opened, for the commit of the prompt that carries the opening.
pub(crate) fn told(now: &Now) -> Change {
    Change::Meta {
        key: BRIEFED_KEY.to_string(),
        value: Some(now.local_day.to_string()),
    }
}
