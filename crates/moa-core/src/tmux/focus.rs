//! Which sessions a user is looking at: trusted pane bindings against the topology's
//! visible panes, and `seen` for what they show.

use std::collections::BTreeMap;

use super::ids::{PaneId, WindowId};
use super::link::Link;
use crate::Now;
use crate::change::Change;
use crate::core::Tx;
use crate::model::SessionId;
use crate::state::State;

/// The panes whose binding can be believed: bound on the server the link is connected to,
/// and still there, with the window each is in. Nothing is trusted while the link isn't up.
pub(crate) fn trusted_panes(state: &State) -> BTreeMap<PaneId, (WindowId, SessionId)> {
    let tmux = state.tmux();
    let Link::Up { generation, .. } = &tmux.link else {
        return BTreeMap::new();
    };
    // One session per pane: slice 4's reconcile keeps current-generation bindings unique.
    state
        .pane_bindings()
        .filter(|b| b.server_pid == generation.pid && b.server_start == generation.start)
        .filter_map(|b| {
            let pane: PaneId = b.pane_id.parse().ok()?;
            let window = tmux.topology.window_of(pane)?;
            Some((pane, (window, b.session_id)))
        })
        .collect()
}

/// The session bound to `pane` on the server the link is connected to. A pane's exit needs
/// no more than this: tmux never reuses a pane id within one server's life, so a binding of
/// this generation names this pane. Unlike [`trusted_panes`], the topology isn't asked: an
/// agent that dies at launch is gone before any refresh lists its pane.
pub(crate) fn bound_session(state: &State, pane: PaneId) -> Option<SessionId> {
    let Link::Up { generation, .. } = &state.tmux().link else {
        return None;
    };
    state
        .pane_bindings()
        .filter(|b| b.server_pid == generation.pid && b.server_start == generation.start)
        .find(|b| b.pane_id.parse() == Ok(pane))
        .map(|b| b.session_id)
}

/// The pane `id` is bound to on the server the link is connected to: it runs there.
pub(crate) fn bound_pane(state: &State, id: SessionId) -> Option<PaneId> {
    let Link::Up { generation, .. } = &state.tmux().link else {
        return None;
    };
    state
        .binding(id)
        .filter(|b| b.server_pid == generation.pid && b.server_start == generation.start)
        .and_then(|b| b.pane_id.parse().ok())
}

/// Sets `seen_at` on every session with an unseen turn end whose trusted pane a user can
/// see. A session that can't be written now (a row this build doesn't know, or writes
/// refused) is skipped, and tried again at the next focus change.
pub(crate) fn mark_seen(state: &mut State, tx: &mut Tx, now: &Now) {
    let visible = state.tmux().topology.visible();
    let due: Vec<SessionId> = trusted_panes(state)
        .into_iter()
        .filter(|(pane, _)| visible.contains(pane))
        .map(|(_, (_, session))| session)
        .collect();
    for id in due {
        let Some(s) = state.session(id) else {
            continue;
        };
        // `display_state`'s rule for `Done`.
        let (Some(end), Some(seen)) = (s.last_turn_end_at, s.seen_at) else {
            continue;
        };
        if end <= seen {
            continue;
        }
        let mut s = s.clone();
        // Never before the turn's end, or it would still show as unseen.
        s.seen_at = Some(now.wall.max(end));
        let _ = tx.commit(state, vec![Change::Session(s)]);
    }
}
