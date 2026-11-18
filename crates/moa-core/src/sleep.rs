//! Idle sessions go to sleep on their own (R18–R20): their tab is closed, and they stay
//! resumable. One deadline, the earliest a session could be due, and a check at the end of
//! every input, since what kept a due session awake (being on screen, say) can end with any
//! of them.

use crate::change::Change;
use crate::config::SleepAfter;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, TimerKey};
use crate::hooks::asleep;
use crate::live::{DisplayState, display_state};
use crate::model::{ActivityWho, AskStatus, AskTarget, Lifecycle, Location, Session, SessionId};
use crate::ops::launch::title_of;
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::PaneId;
use crate::tmux::link::log;
use crate::{Now, UtcMs};

/// Why session `id` can't go to sleep at `now` after `after_ms` idle, or `None` when it can
/// (R18). The reasons are fixed words, for tests and logs.
pub fn why_not(state: &State, id: SessionId, now: UtcMs, after_ms: i64) -> Option<&'static str> {
    let Some(s) = state.session(id) else {
        return Some("unknown");
    };
    if s.archived_at.is_some() {
        return Some("archived");
    }
    if s.lifecycle != Lifecycle::Live {
        return Some("not running");
    }
    if s.location == Location::Elsewhere {
        return Some("elsewhere");
    }
    // Not quiet, done, asking, waiting, failed or working: nothing the user would miss.
    if display_state(s, now).0 != DisplayState::Idle {
        return Some("not idle");
    }
    if in_an_ask(state, id) {
        return Some("in an ask");
    }
    if now < due_at(s, after_ms) {
        return Some("too soon");
    }
    // A launch waits for its tab, or a resume or restart of it is looking at the tabs.
    if state.tmux().opening(id) {
        return Some("opening");
    }
    // A binding of the server the link is up on.
    let (Some(pane), Some(b)) = (bound_pane(state, id), state.binding(id)) else {
        return Some("no tab here");
    };
    // An adopted agent sends no hooks: it always looks idle, while it may be working.
    if !b.tracked {
        return Some("not tracked");
    }
    if !state.tmux().reconciled() {
        return Some("not reconciled");
    }
    let topology = &state.tmux().topology;
    let Some(window) = topology.window_of(pane) else {
        return Some("not listed");
    };
    if topology.moa_session(pane) != Some(id) {
        return Some("not tagged");
    }
    // An agent moa launched but never heard from shows idle while it may be working.
    if !s.last_seen_at.is_some_and(|t| t > b.bound_at) {
        return Some("never heard from");
    }
    if topology.visible().contains(&pane) {
        return Some("on screen");
    }
    // Slice 5 parks the cockpit's spot first; until then only tabs of their own sleep.
    if topology.is_cockpit(window) {
        return Some("in the cockpit");
    }
    None
}

/// The earliest moment after `now` a session could be due (R19): among live, idle sessions
/// with a tracked tab on this server, `activity_at` (else `created_at`) plus the threshold.
/// `None` when there is none, or the brain never sleeps.
pub(crate) fn deadline(state: &State, now: UtcMs) -> Option<UtcMs> {
    let after = threshold(state)?;
    state
        .sessions()
        .filter(|s| {
            s.archived_at.is_none()
                && s.lifecycle == Lifecycle::Live
                && s.location == Location::Here
        })
        .filter(|s| {
            state.binding(s.id).is_some_and(|b| b.tracked) && bound_pane(state, s.id).is_some()
        })
        .filter(|s| display_state(s, now).0 == DisplayState::Idle)
        .map(|s| due_at(s, after))
        .filter(|at| *at > now)
        .min()
}

/// Puts every due, eligible session to sleep in one commit, with one line about it from the
/// brain, then kills their tabs (R15, R20). Then arms the timer at the next deadline, or
/// cancels it, when that changed or when this input is the timer firing (`fired`: the
/// scheduler has forgotten it). `handle` runs it at the end of every input.
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    if fired {
        state.sleep_at = None;
    }
    let Some(after) = threshold(state) else {
        return;
    };
    if !state.writes_refused(now.wall) {
        let due: Vec<SessionId> = state
            .sessions()
            .filter(|s| s.lifecycle == Lifecycle::Live && now.wall >= due_at(s, after))
            .map(|s| s.id)
            .filter(|id| why_not(state, *id, now.wall, after).is_none())
            .collect();
        if !due.is_empty() {
            sleep_due(state, now, &due, fx);
        }
    }
    let next = deadline(state, now.wall);
    if next != state.sleep_at || fired {
        state.sleep_at = next;
        fx.push(match next {
            Some(at) => Effect::SetTimer {
                key: TimerKey::Sleep,
                at,
            },
            None => Effect::CancelTimer(TimerKey::Sleep),
        });
    }
}

fn sleep_due(state: &mut State, now: &Now, due: &[SessionId], fx: &mut Vec<Effect>) {
    let mut tx = Tx::new(now);
    let mut changes = Vec::new();
    let mut panes = Vec::new();
    let mut names = Vec::new();
    for &id in due {
        let one = put_to_sleep(state, &tx, id);
        // A row this build doesn't know can't be written: that one stays, the rest sleep.
        if one
            .iter()
            .filter_map(Change::row)
            .any(|(table, key)| state.is_unknown(table, &key))
        {
            continue;
        }
        changes.extend(one);
        panes.extend(bound_pane(state, id));
        if let Some(s) = state.session(id) {
            names.push(title_of(state, s));
        }
    }
    if changes.is_empty() {
        return;
    }
    let (n, s) = (names.len(), if names.len() > 1 { "s" } else { "" });
    let text = format!(
        "💤 put {n} idle session{s} to sleep: {} · resume from the tree",
        names.join(", ")
    );
    if tx
        .ui_by(state, changes, vec![text], ActivityWho::Brain)
        .is_err()
    {
        // Unsaved: nothing is killed, and the next input tries again.
        fx.push(log(
            LogLevel::Warn,
            format!("putting {n} idle session{s} to sleep wasn't saved; moa tries again"),
        ));
        return;
    }
    for pane in panes {
        tx.tmux(kill_pane(pane));
    }
    let mut more = vec![log(
        LogLevel::Info,
        format!("{n} idle session{s} went to sleep"),
    )];
    more.extend(post_commit(state, tx));
    fold_in(fx, more);
}

/// Session `id` put to sleep (R5): asleep if it was live, and its binding deleted. The
/// caller commits, then queues the kill of its tab.
pub(crate) fn put_to_sleep(state: &State, tx: &Tx, id: SessionId) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Some(s) = state.session(id).filter(|s| s.lifecycle == Lifecycle::Live) {
        changes.push(Change::Session(asleep(s, tx.wall)));
    }
    if state.binding(id).is_some() {
        changes.push(Change::DeletePaneBinding(id));
    }
    changes
}

/// Kills tab `pane`, by its id (R14).
pub(crate) fn kill_pane(pane: PaneId) -> Cmd {
    Cmd::new("kill-pane").args(["-t".to_string(), pane.to_string()])
}

/// This brain's wait before an idle session sleeps, or `None` for never.
fn threshold(state: &State) -> Option<i64> {
    state
        .config()
        .brains
        .get(state.brain())
        .map_or_else(SleepAfter::default, |b| b.sleep_after)
        .ms()
}

/// When session `s` has been idle `after_ms`.
fn due_at(s: &Session, after_ms: i64) -> UtcMs {
    let since = s.activity_at.unwrap_or(s.created_at);
    UtcMs(since.0.saturating_add(after_ms))
}

/// Whether an ask from or to session `id` is still open.
fn in_an_ask(state: &State, id: SessionId) -> bool {
    let me = Some(AskTarget::Session(id));
    state.asks.values().any(|a| {
        matches!(
            a.status,
            AskStatus::Pending | AskStatus::Sent | AskStatus::Received
        ) && (a.from_session == me || a.to_session == me)
    })
}
