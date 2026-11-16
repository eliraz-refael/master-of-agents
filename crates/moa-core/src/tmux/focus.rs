//! Which sessions a user is looking at: trusted pane bindings against the topology's
//! visible panes, and `seen` for what they show.

use std::collections::BTreeMap;

use super::ids::PaneId;
use super::link::Link;
use crate::Now;
use crate::change::Change;
use crate::core::Tx;
use crate::model::SessionId;
use crate::state::State;

/// The panes whose binding can be believed: bound on the server the link is connected to,
/// and still there. Nothing is trusted while the link isn't up.
pub(crate) fn trusted_panes(state: &State) -> BTreeMap<PaneId, SessionId> {
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
            tmux.topology.has_pane(pane).then_some((pane, b.session_id))
        })
        .collect()
}

/// Sets `seen_at` on every session with an unseen turn end whose trusted pane a user can
/// see. A session that can't be written now (a row this build doesn't know, or writes
/// refused) is skipped, and tried again at the next focus change.
pub(crate) fn mark_seen(state: &mut State, tx: &mut Tx, now: &Now) {
    let visible = state.tmux().topology.visible();
    let due: Vec<SessionId> = trusted_panes(state)
        .into_iter()
        .filter(|(pane, _)| visible.contains(pane))
        .map(|(_, session)| session)
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
