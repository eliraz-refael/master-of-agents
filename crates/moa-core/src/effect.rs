//! What goes into the core and what comes out. The core decides; an executor does.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::civil::Date;

use crate::Now;
use crate::UtcMs;
use crate::central::wire::{AcpCmd, AcpIn};
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
    /// The earliest moment an idle session could go to sleep (R19).
    Sleep,
    /// The earliest moment a terminal is due: a close's kill, or a timeout (5d, T8).
    Terminals,
    /// When moa's agent is next woken with the session reports (`central::channel`).
    Wake,
    /// When moa's turn under way has gone too long without news, or without stopping once
    /// cancelled (`central::watchdog`).
    Watchdog,
}

/// Timer `key`, armed at `armed`, at its next deadline `next`: set, or cancelled when there
/// is none, but only when that changed or when this input is the timer firing (`fired`: the
/// scheduler has forgotten it). `armed` becomes `next`. Sleep's, the terminals', the
/// wake-up's and the watchdog's timers are kept this way.
pub(crate) fn rearm(
    armed: &mut Option<UtcMs>,
    key: TimerKey,
    next: Option<UtcMs>,
    fired: bool,
) -> Option<Effect> {
    if next == *armed && !fired {
        return None;
    }
    *armed = next;
    Some(match next {
        Some(at) => Effect::SetTimer { key, at },
        None => Effect::CancelTimer(key),
    })
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
    /// The daemon's start drain of the hook spool has run, whether its commit landed or its
    /// `.draining` file was kept for later (or there was no spool). Sent once per start.
    StartDrained,
    /// What moa's agent, or its host, said.
    Acp(AcpIn),
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
    /// tmux answered the control client with something it couldn't follow. A server that
    /// doesn't answer in time isn't this: the link backs off and tries it again.
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
    /// A question about the server's panes (`TmuxState::ask`), by number.
    Query(u64),
    /// A no-op that proves the server still answers, before `tmux.ensure` says it runs, by
    /// number. The link answers it only when every command succeeded; otherwise, or when
    /// tmux takes too long, it drops the client and reports `Lost`.
    Ping(u64),
    /// Tags on panes moa just started (L7). They save nothing, so they go out at once, never
    /// held behind a commit; their answer is only checked, and a refusal logged.
    Tags,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Publish {
    Model(Arc<Model>),
    Event(CoreEvent),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Debug,
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
    /// [`TmuxIn::Replies`]: a `Refresh`, a `Query`, a `Ping` or `Tags` is the link's own
    /// plumbing, and runs at once; an `Open` starts an agent that reports on its session, and waits for
    /// the commit that saves it.
    Tmux {
        conn: u64,
        cmds: Vec<Cmd>,
        token: Option<TmuxToken>,
    },
    /// Connect the link, with this tmux program if one is known.
    TmuxConnect {
        tmux: Option<PathBuf>,
    },
    /// Writes `text` to `path` (`layouts.json`), through a temporary file in the same folder
    /// and a rename, before the effects after it. When it fails, the op's `Publish` and
    /// `Reply` after it are skipped, the file is read again (`layout::reread`), and `req` is
    /// refused: `couldn't save <file>: <error>`.
    WriteFile {
        path: PathBuf,
        text: String,
        req: Option<(ConnId, ReqId)>,
    },
    /// A command for moa's agent. A `Prompt` is sent once its `you` row is saved, and your
    /// answer to a permission request once its row says so ([`AcpCmd::held`]): each waits
    /// for the commit and is dropped with a failed one. Every other command runs at once.
    Acp(AcpCmd),
}

impl Effect {
    /// Timers, logs, the link's plumbing and moa's agent's commands but a prompt don't wait
    /// for a commit; everything else does. The plumbing reveals nothing about saved state,
    /// and held behind a failed commit it would be lost while the link waits for its answer.
    /// An `open`'s new window is not plumbing: the agent it starts sends events for a
    /// session that must be saved first. Held behind a failed commit it is dropped, and the
    /// reload answers the open. So is a prompt to moa, or your answer to its agent: each goes
    /// once its row is saved.
    pub fn runs_at_once(&self) -> bool {
        matches!(
            self,
            Effect::SetTimer { .. }
                | Effect::CancelTimer(_)
                | Effect::Log(_)
                | Effect::TmuxConnect { .. }
                | Effect::Tmux {
                    token: Some(
                        TmuxToken::Refresh
                            | TmuxToken::Query(_)
                            | TmuxToken::Ping(_)
                            | TmuxToken::Tags
                    ),
                    ..
                }
        ) || matches!(self, Effect::Acp(c) if !c.held())
    }

    /// Whether this tells the world about saved state, or starts something that will: a
    /// `Publish`, a `Reply`, a view (an untokened `Tmux`), an `open`'s new window, a prompt
    /// to moa (its answer lands in the feed after its row), or your answer to its agent.
    pub fn is_heard(&self) -> bool {
        matches!(
            self,
            Effect::Publish(_)
                | Effect::Reply { .. }
                | Effect::Tmux {
                    token: None | Some(TmuxToken::Open(_)),
                    ..
                }
        ) || matches!(self, Effect::Acp(c) if c.held())
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
