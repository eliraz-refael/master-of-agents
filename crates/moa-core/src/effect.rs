//! What goes into the core and what comes out. The core decides; an executor does.

use std::sync::Arc;

use jiff::civil::Date;

use crate::Now;
use crate::UtcMs;
use crate::change::Change;
use crate::op::{ConnId, Done, Op, OpError, ReqId};
use crate::publish::{CoreEvent, Model};
use crate::state::{CallerInfo, Snapshot, State};

/// The store can't be written to until `until`; the next write after that probes it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RefuseWrites {
    pub until: UtcMs,
    pub reason: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum TimerKey {
    Backup,
}

// `Client` carries a whole `Op`; one is built per request, so boxing it buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Input {
    Startup(Box<Snapshot>),
    Reload {
        snapshot: Box<Snapshot>,
        refuse_writes: Option<RefuseWrites>,
    },
    Client {
        conn: ConnId,
        req: ReqId,
        caller: CallerInfo,
        op: Op,
    },
    Timer(TimerKey),
    ZoneChanged,
    ActorFailed {
        actor: &'static str,
        message: String,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub enum Publish {
    Model(Arc<Model>),
    Event(CoreEvent),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// A log line. It never holds record contents (titles, text, names, paths): only ids,
/// tables and fixed sentences.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Effect {
    Persist(Vec<Change>),
    SetTimer {
        key: TimerKey,
        at: UtcMs,
    },
    CancelTimer(TimerKey),
    Publish(Publish),
    Reply {
        conn: ConnId,
        req: ReqId,
        result: Result<Done, OpError>,
    },
    Backup {
        day: Date,
    },
    Log(LogLine),
}

impl Effect {
    /// Timers and logs don't wait for a commit; everything else does.
    pub fn runs_at_once(&self) -> bool {
        matches!(
            self,
            Effect::SetTimer { .. } | Effect::CancelTimer(_) | Effect::Log(_)
        )
    }
}

/// Every `Persist` comes before any `Publish` or `Reply`: nobody hears of a change that
/// isn't saved.
pub fn check_order(effects: &[Effect]) -> Result<(), &'static str> {
    let mut heard = false;
    for e in effects {
        match e {
            Effect::Persist(_) if heard => {
                return Err("a Persist came after a Publish or Reply");
            }
            Effect::Publish(_) | Effect::Reply { .. } => heard = true,
            _ => {}
        }
    }
    Ok(())
}

/// [`crate::handle`], then a panic if the effects break [`check_order`]. For tests and debug
/// builds of the executor.
pub fn handle_checked(state: &mut State, input: Input, now: Now) -> Vec<Effect> {
    let effects = crate::core::handle(state, input, now);
    if let Err(why) = check_order(&effects) {
        // No effect contents: a panic message is a log line (S15).
        panic!("{why} (in {} effects)", effects.len());
    }
    effects
}

/// The reply to the client, if there is one.
pub fn reply_of(effects: &[Effect]) -> Option<&Result<Done, OpError>> {
    effects.iter().find_map(|e| match e {
        Effect::Reply { result, .. } => Some(result),
        _ => None,
    })
}
