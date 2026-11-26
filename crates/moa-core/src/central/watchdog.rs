//! The watchdog (K6): a turn of moa's agent with no news for `watchdog_seconds` is cancelled,
//! and one still under way `watchdog_kill_seconds` after that is cut short: moa's agent is
//! restarted, and today's thread reopened with no prompt (or the new thread `/new` asked for
//! opened, or a new one for today when today has none yet). A `card` row in the turn's thread
//! says what happened, and is updated in place at each step (K7, K11).
//!
//! Until the cancel it watches only while no request of moa's agent waits for you, and moa
//! itself waits on no ask (A10: its `moactl ask` or `reply` blocks its turn for up to its
//! wait; anyone else's wait on moa's ask holds nothing): you, or the peer, may take as long as you like, and the turn gets a fresh span once
//! nothing waits. After the cancel the turn should stop, so a request or an ask then doesn't
//! hold the restart off.
//!
//! The deadlines are wall-clock, as every deadline of the core is (there is no monotonic
//! clock): a timer that fires more than a minute late (the Mac slept) gives the turn a fresh
//! span instead, so a sleep never cuts a turn short.

use super::feed::{card_body, kind};
use super::wire::AcpCmd;
use super::{Open, Phase, answer, card, launch, new_run, said, turn, warn};
use crate::asks;
use crate::core::{Tx, fold_in};
use crate::effect::{Effect, TimerKey, rearm};
use crate::model::FeedEntry;
use crate::op::OpError;
use crate::state::State;
use crate::time::words;
use crate::{Now, UtcMs};

/// The `what` of the watchdog's card.
pub const WHAT: &str = "watchdog";

/// What whoever waits for a turn the watchdog cut short hears.
const HUNG: &str = "moa's turn hung, so moa's agent was restarted";

/// How the card and the waiter end when moa's agent couldn't start again.
const NOT_STARTED: &str = "so moa stopped moa's agent, and it couldn't start again; your next \
                           prompt starts it with a new thread.";

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

/// The watchdog's restart under way: its card, finished once the restart ends, and the brief
/// of the new thread `/new` asked for, which the thread's first prompt carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Restart {
    pub card: FeedEntry,
    pub brief: Option<String>,
}

/// How the restart ended.
pub(super) enum Restarted {
    /// Today's thread is open again.
    Reopened(Open),
    /// The new thread `/new` asked for is open.
    New(Open),
    /// Today had no thread to reopen (the turn began before midnight): a new one is open.
    NewToday(Open),
    /// moa's agent started again, but no thread could be opened.
    NoThread,
    /// moa's agent couldn't start again.
    NotStarted,
}

/// The watchdog of moa's turn under way. Kept in memory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Watchdog {
    pub stage: Stage,
    /// When the stage's span began: the turn's newest news while it is watched, its cancel
    /// once cancelled.
    pub since: UtcMs,
    /// A request waits for your answer, or moa for an ask's: the watch waits too.
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
    let asked = asks::moa_waits(state);
    let moa = &mut state.central;
    let dog = &mut moa.watchdog;
    let waits = dog.stage == Stage::Watching && (!moa.asking.is_empty() || asked);
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
/// waits for your answer, or moa for an ask's, before the cancel (after it, the turn should
/// have stopped).
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
    let Some(turn) = state.central.turn.take() else {
        return Vec::new();
    };
    let ended = start.is_err().then_some(&Restarted::NotStarted);
    let mut tx = Tx::new(now);
    let text = killed_text(state, ended);
    let killed = card_row(state, &mut tx, &turn.thread, card_id, &text);
    let mut fx = vec![warn("moa's turn didn't stop; moa's agent is stopped")];
    fx.extend(turn::cut_short(state, tx, killed.clone()));
    let moa = &mut state.central;
    // Only the card stays written: its id, saved or not, is held until the restart's end
    // updates it ([`reopened`]), so no new row (an account card, say) takes it meanwhile.
    moa.written.retain(|id, _| *id == killed.id);
    let told = match ended {
        None => HUNG.to_string(),
        Some(_) => format!("moa's turn hung, {NOT_STARTED}"),
    };
    fx.extend(answer(turn.waiter, Err(OpError(told))));
    match start {
        Ok((account, spec)) => {
            fx.push(new_run(moa, account, spec));
            moa.restarting = Some(Restart {
                card: killed,
                brief: None,
            });
        }
        // moa's agent can't be started any more: the stuck one is stopped, and the next prompt
        // starts it with a new thread.
        Err(OpError(why)) => {
            fx.push(Effect::Acp(AcpCmd::Stop {
                generation: moa.generation,
            }));
            moa.phase = Phase::Failed(why);
            moa.thread = None;
            moa.reopen_failed = true;
        }
    }
    fx
}

/// The restart is over, as `ended` says: a thread is open, or none is and the next prompt
/// opens a new one. The card says which.
pub(super) fn reopened(state: &mut State, now: &Now, ended: Restarted) -> Vec<Effect> {
    let Some(restart) = state.central.restarting.take() else {
        return Vec::new();
    };
    let text = killed_text(state, Some(&ended));
    let moa = &mut state.central;
    if moa.phase == Phase::Starting {
        moa.phase = Phase::Ready;
    }
    let (thread, failed) = match ended {
        Restarted::Reopened(open) | Restarted::New(open) | Restarted::NewToday(open) => {
            (Some(open), false)
        }
        Restarted::NoThread | Restarted::NotStarted => (None, true),
    };
    moa.thread = thread;
    moa.reopen_failed = failed;
    let row = FeedEntry {
        body: card_body(WHAT, &text),
        ..restart.card
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

/// K11: the cancel's text, then the restart's: how it ended, once it has (`ended`).
fn killed_text(state: &State, ended: Option<&Restarted>) -> String {
    let kill = words(state.config().moa.watchdog_kill_ms);
    let end = match ended {
        None => "so moa's agent was restarted.",
        Some(Restarted::Reopened(_)) => "so moa's agent was restarted and today's thread reopened.",
        Some(Restarted::New(_)) => "so moa's agent was restarted, with a new thread as you asked.",
        Some(Restarted::NewToday(_)) => {
            "so moa's agent was restarted, with a new thread for today."
        }
        Some(Restarted::NoThread) => {
            "so moa's agent was restarted. Today's thread couldn't be reopened; your next prompt \
             starts a new one."
        }
        Some(Restarted::NotStarted) => NOT_STARTED,
    };
    format!(
        "{} It still hadn't stopped {kill} later, {end}",
        cancelled_text(state)
    )
}
