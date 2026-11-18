//! What goes into the core and what comes out. The core decides; an executor does.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::civil::Date;

use crate::Now;
use crate::UtcMs;
use crate::change::Change;
use crate::hook::HookEnvelope;
use crate::model::SessionId;
use crate::op::{ConnId, Done, Op, OpError, ReqId};
use crate::publish::{CoreEvent, Model};
use crate::state::{CallerInfo, Snapshot, State};
use crate::tmux::cmd::Cmd;
use crate::tmux::ids::{ClientName, Generation};
use crate::tmux::notify::Notification;

/// The store can't be written to until `until`; the next write after that probes it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RefuseWrites {
    pub until: UtcMs,
    pub reason: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum TimerKey {
    Backup,
    /// The status line's minute clock.
    Clock,
    /// The earliest moment a working session turns quiet.
    Quiet,
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
    /// A client's connection closed: nothing is answered on it any more.
    ClientGone(ConnId),
    Timer(TimerKey),
    ZoneChanged,
    ActorFailed {
        actor: &'static str,
        message: String,
    },
    Tmux(TmuxIn),
    /// One hook event from an agent. With `reply`, the sender waits for an answer, sent
    /// after the commit.
    Hook {
        reply: Option<(ConnId, ReqId)>,
        envelope: HookEnvelope,
    },
}

/// What the tmux link reports. `conn` numbers the link's connections; anything from an
/// older one is dropped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TmuxIn {
    Connected {
        conn: u64,
        generation: Generation,
        version: String,
        ctl: ClientName,
        /// The server's socket as moa names it (`-L` name or `-S` path), for pane bindings.
        socket: String,
    },
    Down(Down),
    Notification(Notification),
    /// The answers to one tokened [`Effect::Tmux`], one per command: its output lines, or
    /// tmux's error text.
    Replies {
        conn: u64,
        token: TmuxToken,
        results: Vec<Result<Vec<String>, String>>,
    },
}

/// Why the link is not up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Down {
    /// The control client dropped while the server is still there; the link reconnects.
    Lost,
    /// No server is running.
    NoServer,
    /// The reconnects ran out.
    GaveUp,
    /// tmux answered the control client with something it couldn't follow, or not at all.
    CantLink,
    /// The server's tmux is older than moa needs; its version.
    TooOld(String),
    /// tmux couldn't be started.
    NoTmux,
    /// This daemon has no tmux link.
    Disabled,
}

/// Which plumbing request a reply answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TmuxToken {
    /// The three lists that rebuild the topology.
    Refresh,
    /// The new window an `open` asked for, and the session in it.
    Open(SessionId),
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
    /// Commands for the link's connection `conn`. Untokened, they show saved state (the
    /// status line, notices) and wait for the commit. Tokened, their answers come back as
    /// [`TmuxIn::Replies`]: a `Refresh` is the link's own plumbing, and runs at once; an
    /// `Open` starts an agent that reports on its session, and waits for the commit that
    /// saves it.
    Tmux {
        conn: u64,
        cmds: Vec<Cmd>,
        token: Option<TmuxToken>,
    },
    /// Connect the link, with this tmux program if one is known.
    TmuxConnect {
        tmux: Option<PathBuf>,
    },
}

impl Effect {
    /// Timers, logs and the link's plumbing don't wait for a commit; everything else does.
    /// The plumbing reveals nothing about saved state, and held behind a failed commit it
    /// would be lost while the link waits for its answer. An `open`'s new window is not
    /// plumbing: the agent it starts sends events for a session that must be saved first.
    /// Held behind a failed commit it is dropped, and the reload answers the open.
    pub fn runs_at_once(&self) -> bool {
        matches!(
            self,
            Effect::SetTimer { .. }
                | Effect::CancelTimer(_)
                | Effect::Log(_)
                | Effect::TmuxConnect { .. }
                | Effect::Tmux {
                    token: Some(TmuxToken::Refresh),
                    ..
                }
        )
    }

    /// Whether this tells the world about saved state, or starts something that will: a
    /// `Publish`, a `Reply`, a view (an untokened `Tmux`), or an `open`'s new window.
    pub fn is_heard(&self) -> bool {
        matches!(
            self,
            Effect::Publish(_)
                | Effect::Reply { .. }
                | Effect::Tmux {
                    token: None | Some(TmuxToken::Open(_)),
                    ..
                }
        )
    }
}

/// Every `Persist` comes before anything heard ([`Effect::is_heard`]): nobody hears of a
/// change that isn't saved.
pub fn check_order(effects: &[Effect]) -> Result<(), &'static str> {
    let mut heard = false;
    for e in effects {
        match e {
            Effect::Persist(_) if heard => {
                return Err("a Persist came after a Publish, Reply or view");
            }
            e if e.is_heard() => heard = true,
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
