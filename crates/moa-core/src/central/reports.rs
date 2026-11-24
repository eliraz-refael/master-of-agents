//! The session reports queue: what a session tells moa's agent, kept in the database until a
//! wake-up carries it. A hook event that is applied (not history only, not a duplicate) may
//! queue one report row; its rules are here, and the change goes in the hook's own commit, so
//! a report is never queued for an event that wasn't saved.

use std::path::Path;

use crate::change::Change;
use crate::core::Tx;
use crate::hook::{HookEnvelope, HookKind};
use crate::model::{Report, ReportKind, Session};
use crate::state::{State, Table};
use crate::text::cut;

/// How many turns a session's `turn_ended` row keeps: the newest.
pub const TURNS_KEPT: usize = 6;

/// The most characters of one turn's message that a report keeps for moa's wake-up to render:
/// the earlier turns are cut shorter there ([`EARLIER_MAX`]).
pub const LAST_MAX: usize = 2600;

/// The most characters of an earlier turn that a wake-up shows.
pub const EARLIER_MAX: usize = 500;

/// The report `env` adds for `session` (the session as the event leaves it), or `None` when
/// it adds none: the event says nothing to moa (an empty message, a start, a failure, a
/// need cleared), or the session is moa's own agent's, whose folder is `Deps.agent_dir`.
///
/// A session has one row of each kind. A turn's message joins the row's turns, the newest
/// [`TURNS_KEPT`] kept; what a session needs replaces the last. The row keeps its `queued_at`,
/// its place in line, and `last_at` moves.
pub(crate) fn queued(
    state: &State,
    tx: &mut Tx,
    session: &Session,
    env: &HookEnvelope,
) -> Option<Change> {
    let (kind, text) = match &env.kind {
        HookKind::TurnEnded { last_message } => (ReportKind::TurnEnded, last_message.clone()),
        HookKind::NeedsPermission { tool } => (ReportKind::NeedsYou, format!("permission: {tool}")),
        HookKind::NeedsAnswer { what } => (ReportKind::NeedsYou, what.clone()),
        _ => return None,
    };
    let text = cut(text.trim(), LAST_MAX);
    if text.is_empty() || is_moas_own(state, session) {
        return None;
    }
    let row = match state
        .reports()
        .find(|r| r.session_id == session.id && r.kind == kind)
    {
        Some(old) => {
            let mut turns = old.turns.clone();
            match kind {
                ReportKind::TurnEnded => {
                    turns.push(text);
                    turns.drain(..turns.len().saturating_sub(TURNS_KEPT));
                }
                ReportKind::NeedsYou => turns = vec![text],
            }
            Report {
                last_at: env.at,
                turns,
                ..old.clone()
            }
        }
        None => Report {
            id: tx.next_id(state, Table::Report),
            session_id: session.id,
            kind,
            queued_at: env.at,
            last_at: env.at,
            turns: vec![text],
        },
    };
    Some(Change::Report(row))
}

/// Whether `session` works in moa's own agent's folder, or under it. Both paths are
/// canonical, so `/tmp` and `/private/tmp` agree.
fn is_moas_own(state: &State, session: &Session) -> bool {
    let deps = state.deps();
    session.cwd.as_deref().is_some_and(|cwd| {
        (deps.canonical)(Path::new(cwd)).starts_with((deps.canonical)(&deps.agent_dir))
    })
}
