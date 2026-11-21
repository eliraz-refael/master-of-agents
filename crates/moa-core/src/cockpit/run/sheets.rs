//! The sheets' events (S9): what opens and closes the picker's or help's tab in your
//! session, as the queue runs it: `ui.close`, a session leaving the tab, a terminal
//! detaching, and a listing that finds a sheet left open by a daemon that restarted.

use std::collections::BTreeSet;

use serde_json::json;

use super::{CockpitOp, queue};
use crate::effect::{Effect, TmuxIn};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::panel::View;
use crate::state::State;
use crate::tmux::ids::{PaneId, TmuxSessionId, WindowId};
use crate::tmux::link::NO_TERMINAL;
use crate::tmux::notify::Notification;
use crate::tmux::topology::{HIDDEN, Topology};

/// `ui.close` from a panel that isn't a sheet moa placed.
const ONLY_SHEETS: &str = "only the picker and help close this way";

/// The sheet `op` opens or closes, by its view (a sheet has one panel): a newer open or close
/// of it replaces a waiting one.
pub(super) fn sheet_of(state: &State, op: &CockpitOp) -> Option<View> {
    match op {
        CockpitOp::Sheet { view, .. } => Some(*view),
        CockpitOp::Unsheet { pane } => state
            .tmux
            .topology
            .view_of(*pane)?
            .parse::<View>()
            .ok()
            .filter(|v| v.is_sheet()),
        _ => None,
    }
}

/// `ui.close`: the sheet in the asking panel's tab is closed, in its turn. Answered at once,
/// as soon as the close is queued: closing has nothing more to say. Only a sheet moa placed
/// asks it, and only while moa has its terminal can it run.
pub(crate) fn ui_close(state: &mut State, conn: ConnId, req: ReqId) -> Vec<Effect> {
    let pane = state
        .panels
        .get(&conn)
        .filter(|reg| reg.view.is_sheet() && reg.managed)
        .and_then(|reg| reg.pane);
    let result = match pane {
        None => Err(OpError(ONLY_SHEETS.into())),
        Some(_) if state.tmux.conn().is_none() => Err(OpError(NO_TERMINAL.into())),
        Some(_) => Ok(Done {
            text: String::new(),
            data: json!({}),
        }),
    };
    let mut fx = match (pane, &result) {
        (Some(pane), Ok(_)) => queue(state, CockpitOp::Unsheet { pane }),
        _ => Vec::new(),
    };
    fx.push(Effect::Reply { conn, req, result });
    fx
}

/// The user's sessions window `w` is linked into: every one but `_moa`, while `_moa` holds
/// it too. A window `_moa` doesn't hold is no sheet of moa's to unlink (its only link can't
/// be unlinked).
pub(super) fn linked_out(topology: &Topology, w: WindowId) -> Vec<TmuxSessionId> {
    let hidden = |s: &TmuxSessionId| topology.session_name(*s) == Some(HIDDEN);
    let sessions = topology.sessions_of(w);
    if !sessions.iter().any(hidden) {
        return Vec::new();
    }
    sessions.iter().filter(|s| !hidden(s)).copied().collect()
}

/// What a notification may mean for the sheets open.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SheetNews {
    /// Session `session` shows its tab `window` now (`%session-window-changed`).
    Shows {
        session: TmuxSessionId,
        window: WindowId,
    },
    /// A user's terminal left session `session`: it detached (`%client-detached`), or
    /// switched to another session (`%client-session-changed`).
    Detached { session: TmuxSessionId },
}

/// The news in `t` for the sheets, read before the topology takes it in: a detached
/// terminal's session is known only until then.
pub(crate) fn sheet_news(state: &State, t: &TmuxIn) -> Option<SheetNews> {
    match t {
        TmuxIn::Notification(Notification::SessionWindowChanged { session, window }) => {
            Some(SheetNews::Shows {
                session: *session,
                window: *window,
            })
        }
        TmuxIn::Notification(Notification::ClientDetached { client }) => {
            let session = state.tmux.topology.client_session(client)?;
            Some(SheetNews::Detached { session })
        }
        TmuxIn::Notification(Notification::ClientSessionChanged {
            client,
            session: to,
        }) => {
            let session = state.tmux.topology.client_session(client)?;
            (session != *to).then_some(SheetNews::Detached { session })
        }
        _ => None,
    }
}

/// What `news` means for the sheets open, once the topology took it in: one whose tab a
/// user's session left for another of its tabs, or whose session the last user's terminal
/// detached from, is closed, so nobody comes back to a stale sheet. Not while an open or
/// close of that sheet is under way: a close queued now would replace an open you asked for
/// since, or unlink one just made.
pub(crate) fn sheets_notified(state: &mut State, news: SheetNews) -> Vec<Effect> {
    let busy = busy(state);
    let topology = &state.tmux.topology;
    let done: Vec<PaneId> = state
        .sheets
        .keys()
        .copied()
        .filter(|pane| {
            let view = topology.view_of(*pane).and_then(|v| v.parse::<View>().ok());
            if view.is_some_and(|v| busy.contains(&v)) {
                return false;
            }
            let Some(window) = topology.window_of(*pane) else {
                return false;
            };
            let (session, left) = match news {
                SheetNews::Shows { session, window: w } => (session, w != window),
                SheetNews::Detached { session } => (session, !topology.attached(session)),
            };
            left && topology.sessions_of(window).contains(&session)
                && topology.session_name(session) != Some(HIDDEN)
        })
        .collect();
    let mut fx = Vec::new();
    for pane in done {
        fx.extend(queue(state, CockpitOp::Unsheet { pane }));
    }
    fx
}

/// The sheets an open or close is under way for: running, or waiting its turn.
fn busy(state: &State) -> BTreeSet<View> {
    let run = &state.cockpit;
    run.in_flight
        .iter()
        .map(|(_, op)| op)
        .chain(run.waiting.iter())
        .filter_map(|op| sheet_of(state, op))
        .collect()
}

/// What a listing (an operation's, or the link's) shows of sheets left open with nothing
/// recording their open: a daemon that restarted forgot them, or a lost link dropped their
/// close, and they are closed. Not while an open or close of that sheet is under way: its
/// open isn't recorded until tmux answers its link.
pub(super) fn stale_sheets(state: &mut State) -> Vec<Effect> {
    let busy = busy(state);
    let topology = &state.tmux.topology;
    let stale: Vec<PaneId> = topology
        .panes()
        .filter(|p| {
            let view = topology.view_of(*p).and_then(|v| v.parse::<View>().ok());
            view.is_some_and(|v| v.is_sheet() && !busy.contains(&v))
                && !state.sheets.contains_key(p)
                && topology
                    .window_of(*p)
                    .is_some_and(|w| !linked_out(topology, w).is_empty())
        })
        .collect();
    let mut fx = Vec::new();
    for pane in stale {
        fx.extend(queue(state, CockpitOp::Unsheet { pane }));
    }
    fx
}
