//! An event newer than the history fence from a session that isn't live and has no tab (the
//! human's option C). Its agent may still run in a tab moa lost track of: a sleep or close
//! whose kill went with the link, or a wrong 💤. Until that is proven the event is history
//! only (M7: an asleep or ended session shows as it was left). One listing of the tabs is
//! asked; a tab that runs the session's agent, found as reconcile finds one (its tag, then
//! `MOA_SESSION`, then its conversation, R6, R31), is bound, the session wakes, and the
//! event is applied then.

use std::collections::BTreeSet;

use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::hook::HookEnvelope;
use crate::hooks::apply_held;
use crate::model::{Lifecycle, Location, Session, SessionId};
use crate::reconcile::{TabMatch, match_tab, new_binding, tag_pane};
use crate::state::State;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Asked, Link, log, ready};
use crate::tmux::panes::{PaneInfo, kept_bindings, list_panes};

/// How many events wait with one look; later ones stay history only.
const HELD: usize = 16;

/// Session `id`, which isn't live and has no tab, sent `env`: one listing of the tabs is
/// asked, unless one is already out for it, which `env` then waits with. Nothing is asked
/// while the tabs aren't reconciled (the link is down, or a reconcile on its way will bind
/// the tab itself), nor after a look found no tab running its agent, until it starts or
/// tmux shows a pane that look didn't list: `env` stays history.
pub(crate) fn look(state: &mut State, id: SessionId, env: &HookEnvelope) -> Option<Effect> {
    ready(state).ok()?;
    if let Some(events) = state.tmux.looking(id) {
        if events.len() < HELD {
            events.push(env.clone());
        }
        return None;
    }
    let Link::Up { generation, .. } = state.tmux().link else {
        return None;
    };
    if let Some((on, seen)) = state.not_found.get(&id) {
        if *on == generation && state.tmux().topology.panes().all(|p| seen.contains(&p)) {
            return None;
        }
        state.not_found.remove(&id);
    }
    let asked = Asked::Look {
        id,
        events: vec![env.clone()],
    };
    state.tmux.ask(asked, list_panes())
}

/// A look's listing came back, or failed (tmux refused it, or the link went before it
/// answered): then `events` stay history. A tab no kept binding holds that runs session
/// `id`'s agent is bound to it, in one commit with the session woken (live and here, no
/// longer asleep) and then each of `events` applied; its tags go on after. The session must
/// still be one that isn't live, has no tab and isn't opening, and the tabs still
/// reconciled.
pub(crate) fn looked(
    state: &mut State,
    now: &Now,
    id: SessionId,
    events: &[HookEnvelope],
    panes: Result<Vec<PaneInfo>, ()>,
) -> Vec<Effect> {
    let Ok(panes) = panes else {
        return Vec::new();
    };
    if ready(state).is_err() {
        return Vec::new();
    }
    let Link::Up {
        generation, socket, ..
    } = &state.tmux().link
    else {
        return Vec::new();
    };
    let (generation, socket) = (*generation, socket.clone());
    let Some(s) = state.session(id).cloned() else {
        return Vec::new();
    };
    if s.lifecycle == Lifecycle::Live || state.binding(id).is_some() || state.tmux().opening(id) {
        return Vec::new();
    }
    let held: BTreeSet<PaneId> = kept_bindings(state, generation, &panes)
        .iter()
        .filter_map(|b| b.pane_id.parse().ok())
        .collect();
    let found = panes
        .iter()
        .filter(|p| !p.dead && !held.contains(&p.pane))
        .find_map(
            |info| match match_tab(state, now.wall, info, &socket, |s| s == id) {
                TabMatch::Bind {
                    tracked,
                    tag,
                    current,
                    ..
                } => Some((info, tracked, tag, current)),
                TabMatch::Nobody | TabMatch::Stale | TabMatch::Unread(_) => None,
            },
        );
    let Some((info, tracked, tag, current)) = found else {
        // Its later events don't ask again for these same tabs ([`look`]).
        let seen = panes.iter().map(|p| p.pane).collect();
        state.not_found.insert(id, (generation, seen));
        return Vec::new();
    };
    let woken = Session {
        lifecycle: Lifecycle::Live,
        asleep_at: None,
        location: Location::Here,
        ..s
    };
    let mut changes = vec![
        Change::Session(woken),
        Change::PaneBinding(new_binding(
            id, generation, &socket, info, now.wall, tracked,
        )),
    ];
    changes.extend(current.map(Change::Conversation));
    let mut tx = Tx::new(now);
    let saved = tx.commit(state, changes).is_ok()
        && events
            .iter()
            .all(|env| apply_held(state, &mut tx, id, env).is_ok());
    if tx.changes.is_empty() {
        return vec![log(
            LogLevel::Warn,
            format!("session {id} runs in a tab but wasn't woken: nothing was saved"),
        )];
    }
    if let Some(adopted) = tag {
        tag_pane(state, &mut tx, info.pane, id, adopted);
    }
    let mut fx = vec![log(
        LogLevel::Info,
        format!("session {id} was heard from while not live, and runs in a tab: woken"),
    )];
    if !saved {
        fx.push(log(
            LogLevel::Warn,
            format!("session {id} was woken, but what it sent meanwhile wasn't saved"),
        ));
    }
    fx.extend(post_commit(state, tx));
    fx
}
