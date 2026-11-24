//! Idle sessions go to sleep on their own (R18–R20): their tab is closed, and they stay
//! resumable. One deadline, the earliest a session could be due, and a check at the end of
//! every input, since what kept a due session awake (being on screen, say) can end with any
//! of them.

use serde_json::json;

use crate::change::Change;
use crate::cockpit::run::through_queue;
use crate::config::{SleepAfter, hours_ms};
use crate::console::SLEEP_AFTER_USAGE;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, TimerKey, rearm};
use crate::hooks::asleep;
use crate::live::{DisplayState, display_state, is_asleep};
use crate::model::{ActivityWho, AskStatus, AskTarget, Lifecycle, Location, Session, SessionId};
use crate::op::{Done, OpError};
use crate::ops::launch::title_of;
use crate::state::State;
use crate::time::HOUR;
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Link, log};
use crate::{Now, UtcMs};

/// The `meta` key `/sleep` sets: the wait in milliseconds, `0` for never. It wins over
/// moa.toml's `sleep_after_hours` until `/sleep on` removes it.
pub const SLEEP_KEY: &str = "sleep.after_ms";

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
    if !topology.has_pane(pane) {
        return Some("not listed");
    }
    if topology.moa_session(pane) != Some(id) {
        return Some("not tagged");
    }
    // An agent moa launched but never heard from shows idle while it may be working.
    if !s.last_seen_at.is_some_and(|t| t > b.bound_at) {
        return Some("never heard from");
    }
    // The cockpit's spot sleeps too, off screen: the empty slot takes its place (L11).
    if topology.visible().contains(&pane) {
        return Some("on screen");
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
        // `/sleep off` cancels the deadline it leaves; a fired one is forgotten already.
        fx.extend(rearm(&mut state.sleep_at, TimerKey::Sleep, None, false));
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
    fx.extend(rearm(&mut state.sleep_at, TimerKey::Sleep, next, fired));
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
        panes.extend(bound_pane(state, id).map(|pane| (id, pane)));
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
    for (id, pane) in panes {
        kill_tab(state, &mut tx, id, pane);
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

/// Kills session `id`'s tab `pane` once `tx` is saved (R15), and remembers it as the tab
/// moa killed for the session, on the server the link is up on: its agent's late events
/// are history only ([`State::killed`]). While a cockpit exists (or its queue is busy), the
/// kill is the queue's vacate, which plans from a listing of its own: in the agent spot, the
/// empty slot takes the agent's place (L11, L17). Otherwise it is a plain kill, which
/// doesn't go out with no link up.
pub(crate) fn kill_tab(state: &mut State, tx: &mut Tx, id: SessionId, pane: PaneId) {
    if let Link::Up { generation, .. } = &state.tmux().link {
        let generation = *generation;
        state.killed.insert(id, (generation, pane));
    }
    if through_queue(state) {
        tx.vacate(pane, id);
    } else {
        tx.tmux(kill_pane(pane));
    }
}

/// This brain's wait before an idle session sleeps, or `None` for never: `/sleep`'s, else
/// moa.toml's.
fn threshold(state: &State) -> Option<i64> {
    let chosen = state
        .meta(SLEEP_KEY)
        .and_then(|v| v.parse().ok())
        .filter(|ms| *ms >= 0)
        .map(SleepAfter);
    chosen
        .unwrap_or_else(|| {
            state
                .config()
                .brains
                .get(state.brain())
                .map_or_else(SleepAfter::default, |b| b.sleep_after)
        })
        .ms()
}

/// What `/sleep WORD` stores under [`SLEEP_KEY`]: nothing for `on` (moa.toml's wait), `0` for
/// `off`, or a number of hours, 0 or more, in milliseconds ([`hours_ms`]). Anything else is
/// refused.
pub fn stored(word: &str) -> Result<Option<String>, String> {
    match word {
        "on" => Ok(None),
        "off" => Ok(Some("0".into())),
        hours => hours
            .parse()
            .ok()
            .and_then(|h| hours_ms(&toml::Value::Float(h)))
            .map(|ms| Some(ms.to_string()))
            .ok_or_else(|| SLEEP_AFTER_USAGE.to_string()),
    }
}

/// `console.sleep_after` (`/sleep`): the wait set as `value` says ([`stored`]), or only said
/// when there is none. It takes effect at once: [`run`] moves the deadline at the end of
/// this input.
pub(crate) fn set_after(
    state: &mut State,
    tx: &mut Tx,
    value: Option<&str>,
) -> Result<Done, OpError> {
    if let Some(word) = value {
        let value = stored(word).map_err(OpError)?;
        if state.meta(SLEEP_KEY) != value.as_deref() {
            let key = SLEEP_KEY.to_string();
            tx.commit(state, vec![Change::Meta { key, value }])?;
        }
    }
    let after = threshold(state);
    let asleep = state.sessions().filter(|s| is_asleep(s)).count();
    let text = match after {
        Some(ms) => {
            // Hours to two places, as typed: `2`, `1.5`.
            let hours = (ms as f64 / HOUR as f64 * 100.0).round() / 100.0;
            format!("idle sessions go to sleep after {hours} h · {asleep} asleep now")
        }
        None => "idle sessions stay up (/sleep on)".into(),
    };
    Ok(Done {
        text,
        data: json!({ "after_ms": after, "asleep": asleep }),
    })
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
