//! The watchdog (K6): a turn of moa's agent with no news for `watchdog_seconds` is cancelled,
//! and one still under way `watchdog_kill_seconds` after that is cut short: moa's agent is
//! restarted, and today's thread reopened with no prompt. A `card` row in the turn's thread
//! says what happened, and is updated in place at each step (K7, K11).
//!
//! Until the cancel it watches only while no request of moa's agent waits for you: you may
//! take as long as you like, and the turn gets a fresh span once nothing waits. After the
//! cancel the turn should stop, so a request then doesn't hold the restart off.
//!
//! The deadlines are wall-clock, as every deadline of the core is (there is no monotonic
//! clock): a timer that fires more than a minute late (the Mac slept) gives the turn a fresh
//! span instead, so a sleep never cuts a turn short.

use super::feed::{card_body, kind};
use super::wire::AcpCmd;
use super::{Open, Phase, answer, card, launch, new_run, said, turn, warn};
use crate::core::{Tx, fold_in};
use crate::effect::{Effect, TimerKey, rearm};
use crate::model::{FeedEntry, MoaThread};
use crate::op::OpError;
use crate::state::State;
use crate::time::words;
use crate::{Now, UtcMs};

/// The `what` of the watchdog's card.
pub const WHAT: &str = "watchdog";

/// What whoever waits for a turn the watchdog cut short hears.
const HUNG: &str = "moa's turn hung, so moa's agent was restarted";

/// How late a timer may fire and still be on time.
const LATE_MS: i64 = 60_000;

/// How far the watchdog has gone with the turn under way.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Stage {
    /// No news cancels it.
    #[default]
    Watching,
    /// It was cancelled, and card `card` says so: if it doesn't stop, moa's agent is
    /// restarted.
    Cancelled { card: i64 },
}

/// The watchdog of moa's turn under way. Kept in memory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Watchdog {
    pub stage: Stage,
    /// When the stage's span began: the turn's newest news while it is watched, its cancel
    /// once cancelled.
    pub since: UtcMs,
    /// A request waits for your answer: the watch waits too.
    pub paused: bool,
    /// The deadline the `Watchdog` timer is armed at, if any, as `State::sleep_at` is.
    pub armed: Option<UtcMs>,
}

impl Default for Watchdog {
    fn default() -> Watchdog {
        Watchdog {
            stage: Stage::Watching,
            since: UtcMs(0),
            paused: false,
            armed: None,
        }
    }
}

impl Watchdog {
    /// A prompt was sent at `at`: its turn is watched from then.
    pub(super) fn watch(&mut self, at: UtcMs) {
        self.stage = Stage::Watching;
        self.since = at;
    }

    /// The run under way said something at `at`: a watched turn isn't stuck.
    pub(super) fn news(&mut self, at: UtcMs) {
        if self.stage == Stage::Watching {
            self.since = at;
        }
    }
}

/// The watchdog's check at the end of every input: a turn past its deadline is cancelled, or
/// cut short when it was cancelled already, and the timer is armed at the next deadline, or
/// cancelled when no turn is watched.
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    let moa = &mut state.central;
    let dog = &mut moa.watchdog;
    let waits = dog.stage == Stage::Watching && !moa.asking.is_empty();
    if waits {
        dog.paused = true;
    } else if std::mem::take(&mut dog.paused) {
        dog.since = now.wall;
    }
    let mut next = due(state);
    if let Some(at) = next.filter(|at| *at <= now.wall) {
        let more = match now.wall.0.saturating_sub(at.0) > LATE_MS {
            true => {
                state.central.watchdog.since = now.wall;
                Vec::new()
            }
            false => match state.central.watchdog.stage {
                Stage::Watching => cancel(state, now),
                Stage::Cancelled { card } => restart(state, now, card),
            },
        };
        fold_in(fx, more);
        fx.extend(said(state));
        next = due(state);
    }
    let dog = &mut state.central.watchdog;
    fx.extend(rearm(&mut dog.armed, TimerKey::Watchdog, next, fired));
}

/// When the turn under way is next due: `watchdog_ms` after its newest news, or
/// `watchdog_kill_ms` after its cancel. `None` when no turn is under way, or while a request
/// waits for your answer before the cancel (after it, the turn should have stopped).
fn due(state: &State) -> Option<UtcMs> {
    let moa = &state.central;
    let live =
        moa.phase == Phase::Thinking && moa.turn.as_ref().is_some_and(|t| t.exited.is_none());
    if !live || moa.watchdog.paused {
        return None;
    }
    let settings = &state.config().moa;
    let span = match moa.watchdog.stage {
        Stage::Watching => settings.watchdog_ms,
        Stage::Cancelled { .. } => settings.watchdog_kill_ms,
    };
    Some(UtcMs(moa.watchdog.since.0.saturating_add(span)))
}

/// No news: the turn is cancelled, and a card in its thread says so.
fn cancel(state: &mut State, now: &Now) -> Vec<Effect> {
    let moa = &state.central;
    let Some(thread) = moa.turn.as_ref().map(|t| t.thread.clone()) else {
        return Vec::new();
    };
    let cmd = AcpCmd::Cancel {
        generation: moa.generation,
        thread: thread.clone(),
    };
    let mut tx = Tx::new(now);
    let text = cancelled_text(state);
    let row = card(state, &mut tx, &thread, WHAT, &text);
    let dog = &mut state.central.watchdog;
    dog.stage = Stage::Cancelled { card: row.id };
    dog.since = now.wall;
    let mut fx = vec![warn("moa's turn had no news; it is cancelled")];
    fx.extend(turn::record(state, tx, vec![row]));
    fx.push(Effect::Acp(cmd));
    fx
}

/// Cancelled, the turn didn't stop: it is cut short (its running tools and the requests that
/// wait cancelled, whoever waits told), and moa's agent is started again, the old one
/// stopped first. Once it has started it reopens today's thread ([`reopened`] ends that).
fn restart(state: &mut State, now: &Now, card_id: i64) -> Vec<Effect> {
    let start = launch::spec(state);
    let moa = &mut state.central;
    let Some(turn) = moa.turn.take() else {
        return Vec::new();
    };
    let mut fx = vec![warn("moa's turn didn't stop; moa's agent is restarted")];
    fx.extend(turn::unsaved(moa));
    let mut rows = turn::cancelled_tools(moa);
    rows.extend(turn::forsaken(moa, false).0);
    moa.answered.clear();
    let mut tx = Tx::new(now);
    let killed = card_row(
        state,
        &mut tx,
        &turn.thread,
        card_id,
        &killed_text(state, None),
    );
    rows.push(killed.clone());
    fx.extend(turn::write(state, tx, rows).unwrap_or_default());
    let moa = &mut state.central;
    moa.written.clear();
    fx.extend(answer(turn.waiter, Err(OpError(HUNG.into()))));
    match start {
        Ok((account, spec)) => {
            fx.push(new_run(moa, account, spec));
            moa.restarting = Some(killed);
        }
        // moa's agent can't be started any more: the stuck one is stopped, and the card
        // says no thread is open.
        Err(OpError(why)) => {
            fx.push(Effect::Acp(AcpCmd::Stop {
                generation: moa.generation,
            }));
            moa.phase = Phase::Failed(why);
            moa.thread = None;
            moa.restarting = Some(killed);
            fx.extend(reopened(state, now, None));
        }
    }
    fx
}

/// The restart is over: today's thread is open again (`thread`), or it couldn't be (or moa's
/// agent couldn't start), and the next prompt opens a new one. The card says which.
pub(super) fn reopened(state: &mut State, now: &Now, thread: Option<MoaThread>) -> Vec<Effect> {
    let Some(card) = state.central.restarting.take() else {
        return Vec::new();
    };
    let text = killed_text(state, Some(thread.is_some()));
    let moa = &mut state.central;
    if moa.phase == Phase::Starting {
        moa.phase = Phase::Ready;
    }
    moa.reopen_failed = thread.is_none();
    moa.thread = thread.map(|row| Open {
        row,
        brief: None,
        reopen_failed: false,
    });
    let row = FeedEntry {
        body: card_body(WHAT, &text),
        ..card
    };
    let fx = turn::record(state, Tx::new(now), vec![row]);
    state.central.written.clear();
    fx
}

/// The card `id` the turn wrote, with `text`; a new card in `thread` when its row was lost.
fn card_row(state: &State, tx: &mut Tx, thread: &str, id: i64, text: &str) -> FeedEntry {
    match state.central.written.get(&id) {
        Some(row) if row.kind == kind::CARD => FeedEntry {
            body: card_body(WHAT, text),
            ..row.clone()
        },
        _ => card(state, tx, thread, WHAT, text),
    }
}

/// K11: `moa's turn had no news for <5 min>, so moa cancelled it.`
fn cancelled_text(state: &State) -> String {
    let span = words(state.config().moa.watchdog_ms);
    format!("moa's turn had no news for {span}, so moa cancelled it.")
}

/// K11: the cancel's text, then the restart's: whether today's thread was reopened, once
/// known.
fn killed_text(state: &State, reopened: Option<bool>) -> String {
    let kill = words(state.config().moa.watchdog_kill_ms);
    let end = match reopened {
        None => ".",
        Some(true) => " and today's thread reopened.",
        Some(false) => ". Today's thread couldn't be reopened; your next prompt starts a new one.",
    };
    format!(
        "{} It still hadn't stopped {kill} later, so moa's agent was restarted{end}",
        cancelled_text(state)
    )
}
