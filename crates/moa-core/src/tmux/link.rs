//! The link to moa's terminal server, as the core sees it: whether it is up, the focus
//! topology it keeps, the `tmux.ensure` callers waiting for it, and the server's
//! generations.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::cmd::Cmd;
use super::ids::{ClientName, Generation, PaneId, WindowId};
use super::mailbox::{HookMsg, Mailbox, MailboxError};
use super::notify::Notification;
use super::topology::{Applied, Topology, refresh_cmds};
use crate::UtcMs;
use crate::effect::{Down, Effect, LogLevel, LogLine, TimerKey, TmuxIn, TmuxToken};
use crate::model::SessionId;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::state::State;

/// An op that needs a tab, while moa's terminal isn't linked.
pub(crate) const NO_TERMINAL: &str = "moa isn't open in a terminal yet; run moa first";
/// An op that acts on bindings, before this connection's reconcile (R10).
pub(crate) const NOT_READY: &str = "moa is still looking at its tabs; try again in a moment";
const NOT_TMUX: &str = "that isn't a tmux program";
const GAVE_UP: &str = "moa lost its terminal server and couldn't reconnect; see the log";
const CANT_LINK: &str = "moa couldn't talk to its terminal; see moa's log";
const NO_TMUX: &str = "moa can't find tmux; install tmux 3.4 or newer";
const DISABLED: &str = "moa v1 isn't switched over yet; try moa --sandbox";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    Down,
    Connecting,
    Up {
        conn: u64,
        generation: Generation,
        version: String,
        ctl: ClientName,
        /// The server's socket, as pane bindings name it.
        socket: String,
    },
}

/// The refresh lists: none asked, one in flight, or one in flight and another asked since.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Refresh {
    #[default]
    Idle,
    InFlight,
    Again,
}

/// The status options last written to the server.
#[derive(Debug)]
pub(super) struct Written {
    pub status: Option<String>,
    pub clock: Option<String>,
    pub windows: BTreeMap<WindowId, &'static str>,
    /// Nothing is known of what the server holds (a start, a reload, a new connection):
    /// the next sync with windows to look at clears every one it has no state for.
    pub fresh: bool,
}

impl Default for Written {
    fn default() -> Self {
        Written {
            status: None,
            clock: None,
            windows: BTreeMap::new(),
            fresh: true,
        }
    }
}

impl Written {
    pub fn reset(&mut self) {
        *self = Written::default();
    }
}

/// The link's state. `State::load` never touches it: it describes the server, not the
/// brain.
#[derive(Debug)]
pub struct TmuxState {
    pub link: Link,
    pub topology: Topology,
    /// How many times the server was seen to restart (a new generation).
    pub restarts: u32,
    /// `tmux.ensure` callers waiting for the link.
    waiters: Vec<(ConnId, ReqId)>,
    refresh: Refresh,
    /// The last server generation the link connected to.
    generation: Option<Generation>,
    /// The tmux program the front door found.
    tmux: Option<PathBuf>,
    /// The status options last written to the server, by name. Forgotten whenever the
    /// server may not have them.
    pub(super) written: Written,
    /// When the status clock's timer fires, while it is armed.
    pub(super) clock_at: Option<UtcMs>,
    /// The `open`s waiting for tmux to say which pane their tab has, with whom to answer:
    /// nobody, once the caller's connection closed.
    opens: BTreeMap<SessionId, Option<(ConnId, ReqId)>>,
    /// Panes of this server that exited with no session bound to them, oldest first, at
    /// most [`EXITED_KEPT`]: an `open`'s answer naming one may still be on its way.
    exited: VecDeque<PaneId>,
    /// The pane queries waiting for their answer, by number, and what each was asked for.
    queries: BTreeMap<u64, Asked>,
    next_query: u64,
    /// The start drain of the hook spool has run (`Input::StartDrained`). Set once per
    /// daemon start, never reset: reconcile waits for it (R2).
    pub drained: bool,
    /// Why the link last went down, whatever happened since.
    pub last_down: Option<Down>,
    /// This connection's panes have been reconciled: the bindings can be trusted (R10).
    reconciled: bool,
}

/// What a pane query was asked for: who reads its answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code, reason = "reconcile asks it (4b)")]
pub(crate) enum Asked {
    Reconcile,
}

/// A pane query's answer: each command's lines, or `Err` when any command failed or the
/// link went before tmux answered.
pub(crate) type Answer = Result<Vec<Vec<String>>, ()>;

/// How many unbound pane exits are remembered.
const EXITED_KEPT: usize = 64;

/// How an `open`'s new tab turned out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum OpenEnd {
    /// The pane it runs in, and the pane's process.
    Bound { pane: PaneId, pid: i64 },
    /// tmux refused it (its first error line), or answered with something moa can't read.
    Failed(Option<String>),
    /// The link went away before tmux answered.
    Gone,
}

/// An `open` that has its answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Settled {
    pub id: SessionId,
    /// Whom to answer, unless the caller went.
    pub reply: Option<(ConnId, ReqId)>,
    pub end: OpenEnd,
}

impl Default for TmuxState {
    fn default() -> Self {
        TmuxState {
            link: Link::Down,
            topology: Topology::default(),
            restarts: 0,
            waiters: Vec::new(),
            refresh: Refresh::Idle,
            generation: None,
            tmux: None,
            written: Written::default(),
            clock_at: None,
            opens: BTreeMap::new(),
            exited: VecDeque::new(),
            queries: BTreeMap::new(),
            next_query: 0,
            drained: false,
            last_down: None,
            reconciled: false,
        }
    }
}

/// What an `open`'s `new-window -P` prints: the new pane and its process.
pub(crate) const OPENED_FORMAT: &str = "P #{pane_id} #{pane_pid}";

/// What tmux said to an `open`'s new window, printed with [`OPENED_FORMAT`].
fn open_end(results: &[Result<Vec<String>, String>]) -> OpenEnd {
    let line = match results {
        [Ok(lines)] => match lines.as_slice() {
            [line] => line,
            _ => return OpenEnd::Failed(None),
        },
        [Err(e)] => return OpenEnd::Failed(Some(e.lines().next().unwrap_or("").to_string())),
        _ => return OpenEnd::Failed(None),
    };
    let mut words = line.split(' ');
    let (Some("P"), Some(pane), Some(pid), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return OpenEnd::Failed(None);
    };
    // `i64::from_str` would accept a leading '+'.
    let pid = Some(pid)
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<i64>().ok())
        .filter(|p| *p > 0);
    match (pane.parse::<PaneId>(), pid) {
        (Ok(pane), Some(pid)) => OpenEnd::Bound { pane, pid },
        _ => OpenEnd::Failed(None),
    }
}

/// An absolute path whose last part is `tmux`.
fn is_tmux_program(p: &Path) -> bool {
    p.is_absolute() && p.as_os_str().as_encoded_bytes().ends_with(b"/tmux")
}

fn running(yes: bool) -> Result<Done, OpError> {
    Ok(Done {
        text: String::new(),
        data: json!({ "running": yes }),
    })
}

/// A four-second notice on one client. `-l` keeps the text literal.
pub fn notice(client: &ClientName, text: &str) -> Cmd {
    Cmd::new("display-message")
        .arg("-c")
        .arg(client.0.as_str())
        .args(["-d", "4000", "-l"])
        .arg(format!("moa: {text}"))
}

/// The link's connection, for an op that acts on bindings: up, and this connection's panes
/// reconciled.
#[allow(dead_code, reason = "the ops on bindings use it (4b)")]
pub(crate) fn ready(state: &State) -> Result<u64, OpError> {
    let tmux = state.tmux();
    match tmux.link {
        Link::Up { conn, .. } if tmux.reconciled => Ok(conn),
        Link::Up { .. } => Err(OpError(NOT_READY.into())),
        Link::Down | Link::Connecting => Err(OpError(NO_TERMINAL.into())),
    }
}

fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log(LogLine { level, message })
}

/// The pane a `moa:hook pane-exited` or `pane-died` message names, if `input` is one.
pub(crate) fn exited_pane(input: &TmuxIn) -> Option<PaneId> {
    let TmuxIn::Notification(Notification::Message(m)) = input else {
        return None;
    };
    match Mailbox::parse(m) {
        Ok(Mailbox::Hook(HookMsg::PaneExited(p) | HookMsg::PaneDied(p))) => Some(p),
        _ => None,
    }
}

impl TmuxState {
    /// The daemon has started: connect.
    pub(crate) fn startup(&mut self) -> Vec<Effect> {
        self.link = Link::Connecting;
        vec![self.connect()]
    }

    /// `tmux.ensure`: answered now, or once the link knows. Linked is running: the `moa`
    /// session is the front door's to make, in the user's environment (T1).
    pub(crate) fn ensure(
        &mut self,
        conn: ConnId,
        req: ReqId,
        tmux: Option<PathBuf>,
    ) -> Vec<Effect> {
        if let Some(t) = tmux {
            if !is_tmux_program(&t) {
                return vec![Effect::Reply {
                    conn,
                    req,
                    result: Err(OpError(NOT_TMUX.into())),
                }];
            }
            self.tmux = Some(t);
        }
        match self.link {
            Link::Up { .. } => vec![Effect::Reply {
                conn,
                req,
                result: running(true),
            }],
            Link::Connecting => {
                self.waiters.push((conn, req));
                Vec::new()
            }
            Link::Down => {
                self.waiters.push((conn, req));
                self.link = Link::Connecting;
                vec![self.connect()]
            }
        }
    }

    pub(crate) fn input(&mut self, input: TmuxIn) -> Vec<Effect> {
        match input {
            TmuxIn::Connected {
                conn,
                generation,
                version,
                ctl,
                socket,
            } => self.connected(conn, generation, version, ctl, socket),
            TmuxIn::Down(d) => self.down(d),
            TmuxIn::Notification(n) => self.notification(&n),
            TmuxIn::Replies {
                conn,
                token,
                results,
            } => {
                if self.conn() != Some(conn) {
                    return Vec::new();
                }
                match token {
                    TmuxToken::Refresh => self.listed(&results),
                    // `settle` took it.
                    TmuxToken::Open(_) => Vec::new(),
                    // `answered` took it.
                    TmuxToken::Query(_) => Vec::new(),
                }
            }
        }
    }

    /// After a reload: the link and what it knows stay; its bookkeeping starts over, since a
    /// failed commit or a panic may have left it half-changed. A reconcile's changes may be
    /// among those the reload dropped, so its flag goes too.
    pub(crate) fn reload(&mut self) -> Vec<Effect> {
        self.refresh = Refresh::Idle;
        self.written.reset();
        self.reconciled = false;
        match self.link {
            Link::Up { .. } => self.refresh(),
            Link::Connecting => vec![self.connect()],
            Link::Down => Vec::new(),
        }
    }

    /// The clock's timer fired: its deadline is spent, whatever the wall clock says.
    pub(crate) fn clock_fired(&mut self) {
        self.clock_at = None;
    }

    /// Remembers that `pane` exited with no session bound to it, while the link is up (the
    /// pane id means something only on this server).
    pub(crate) fn exited_unbound(&mut self, pane: PaneId) {
        if !matches!(self.link, Link::Up { .. }) {
            return;
        }
        if self.exited.len() == EXITED_KEPT {
            self.exited.pop_front();
        }
        self.exited.push_back(pane);
    }

    /// Whether `pane` exited before anything was bound to it, forgetting it.
    pub(crate) fn take_exited(&mut self, pane: PaneId) -> bool {
        match self.exited.iter().position(|p| *p == pane) {
            Some(i) => {
                self.exited.remove(i);
                true
            }
            None => false,
        }
    }

    /// Holds `open`'s reply until tmux says how its tab turned out.
    pub(crate) fn wait_open(&mut self, id: SessionId, conn: ConnId, req: ReqId) {
        self.opens.insert(id, Some((conn, req)));
    }

    /// `conn` closed: its ensures are forgotten, and its opens go on with nobody to answer.
    pub(crate) fn client_gone(&mut self, conn: ConnId) {
        self.waiters.retain(|(c, _)| *c != conn);
        for reply in self.opens.values_mut() {
            if matches!(reply, Some((c, _)) if *c == conn) {
                *reply = None;
            }
        }
    }

    /// The sessions whose `open` is waiting.
    pub(crate) fn waiting_opens(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.opens.keys().copied()
    }

    /// Stops waiting for `id`'s tab, and says whom to answer.
    pub(crate) fn forget_open(&mut self, id: SessionId) -> Option<(ConnId, ReqId)> {
        self.opens.remove(&id).flatten()
    }

    /// The `open`s this input answers: the one whose tab tmux reports on, or every one
    /// when the link goes down or a new connection replaces the one they were asked on.
    /// Run before [`TmuxState::input`], while the link is still as they knew it.
    pub(crate) fn settle(&mut self, input: &TmuxIn) -> Vec<Settled> {
        match input {
            TmuxIn::Replies {
                conn,
                token: TmuxToken::Open(id),
                results,
            } if self.conn() == Some(*conn) => self
                .opens
                .remove(id)
                .map(|reply| Settled {
                    id: *id,
                    reply,
                    end: open_end(results),
                })
                .into_iter()
                .collect(),
            TmuxIn::Down(_) => self.all_gone(),
            TmuxIn::Connected { conn, .. } if self.conn().is_some_and(|c| c != *conn) => {
                self.all_gone()
            }
            _ => Vec::new(),
        }
    }

    /// Asks tmux about the server's panes, on this connection: the tokened commands, whose
    /// answer [`TmuxState::answered`] hands back. `None` while the link isn't up.
    #[allow(dead_code, reason = "reconcile asks (4b)")]
    pub(crate) fn ask(&mut self, asked: Asked, cmds: Vec<Cmd>) -> Option<Effect> {
        let conn = self.conn()?;
        self.next_query += 1;
        let n = self.next_query;
        self.queries.insert(n, asked);
        Some(Effect::Tmux {
            conn,
            cmds,
            token: Some(TmuxToken::Query(n)),
        })
    }

    /// The pane queries this input answers: the one tmux replied to, or every one, failed,
    /// when the link goes down or a new connection replaces the one they were asked on. Run
    /// before [`TmuxState::input`], like [`TmuxState::settle`].
    #[allow(dead_code, reason = "reconcile reads the answers (4b)")]
    pub(crate) fn answered(&mut self, input: &TmuxIn) -> Vec<(Asked, Answer)> {
        match input {
            TmuxIn::Replies {
                conn,
                token: TmuxToken::Query(n),
                results,
            } if self.conn() == Some(*conn) => self
                .queries
                .remove(n)
                .map(|asked| {
                    let lines = results.iter().cloned().collect::<Result<Vec<_>, _>>();
                    (asked, lines.map_err(|_| ()))
                })
                .into_iter()
                .collect(),
            TmuxIn::Down(_) => self.all_failed(),
            TmuxIn::Connected { conn, .. } if self.conn().is_some_and(|c| c != *conn) => {
                self.all_failed()
            }
            _ => Vec::new(),
        }
    }

    fn all_failed(&mut self) -> Vec<(Asked, Answer)> {
        std::mem::take(&mut self.queries)
            .into_values()
            .map(|asked| (asked, Err(())))
            .collect()
    }

    /// The pane queries waiting for their answer, by number.
    pub fn queries(&self) -> impl Iterator<Item = u64> + '_ {
        self.queries.keys().copied()
    }

    /// Whether this connection's panes have been reconciled.
    pub fn reconciled(&self) -> bool {
        self.reconciled
    }

    fn all_gone(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.opens)
            .into_iter()
            .map(|(id, reply)| Settled {
                id,
                reply,
                end: OpenEnd::Gone,
            })
            .collect()
    }

    fn conn(&self) -> Option<u64> {
        match self.link {
            Link::Up { conn, .. } => Some(conn),
            _ => None,
        }
    }

    fn connect(&self) -> Effect {
        Effect::TmuxConnect {
            tmux: self.tmux.clone(),
        }
    }

    /// Reads the lists again, at most one in flight and one more after it.
    fn refresh(&mut self) -> Vec<Effect> {
        let Some(conn) = self.conn() else {
            return Vec::new();
        };
        match self.refresh {
            Refresh::Idle => {
                self.refresh = Refresh::InFlight;
                vec![Effect::Tmux {
                    conn,
                    cmds: refresh_cmds(),
                    token: Some(TmuxToken::Refresh),
                }]
            }
            Refresh::InFlight | Refresh::Again => {
                self.refresh = Refresh::Again;
                Vec::new()
            }
        }
    }

    fn connected(
        &mut self,
        conn: u64,
        generation: Generation,
        version: String,
        ctl: ClientName,
        socket: String,
    ) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.conn() != Some(conn) {
            // A new connection: nothing asked on another one will be answered, and its
            // panes are yet to be reconciled.
            self.refresh = Refresh::Idle;
            self.reconciled = false;
        }
        if self.generation.is_some_and(|g| g != generation) {
            self.restarts += 1;
            fx.push(log(LogLevel::Info, "the terminal server restarted".into()));
        }
        if self.generation != Some(generation) {
            // Another server's pane ids name other panes.
            self.exited.clear();
        }
        self.generation = Some(generation);
        self.link = Link::Up {
            conn,
            generation,
            version,
            ctl,
            socket,
        };
        self.written.reset();
        fx.extend(self.answer(&running(true)));
        fx.extend(self.refresh());
        fx
    }

    fn down(&mut self, d: Down) -> Vec<Effect> {
        self.last_down = Some(d.clone());
        self.reconciled = false;
        self.link = match d {
            // The link is reconnecting.
            Down::Lost => Link::Connecting,
            _ => Link::Down,
        };
        self.topology = Topology::default();
        self.written.reset();
        self.refresh = Refresh::Idle;
        // Nobody is known to be attached any more: the status clock stops.
        let mut fx = Vec::new();
        if self.clock_at.take().is_some() {
            fx.push(Effect::CancelTimer(TimerKey::Clock));
        }
        let result = match d {
            Down::Lost => return fx,
            Down::NoServer => running(false),
            Down::GaveUp => Err(OpError(GAVE_UP.into())),
            Down::CantLink => Err(OpError(CANT_LINK.into())),
            Down::TooOld(v) => Err(OpError(format!(
                "moa needs tmux 3.4 or newer; this one is {v}"
            ))),
            Down::NoTmux => {
                // It couldn't be started: the next ensure says which one to use.
                self.tmux = None;
                Err(OpError(NO_TMUX.into()))
            }
            Down::Disabled => Err(OpError(DISABLED.into())),
        };
        fx.extend(self.answer(&result));
        fx
    }

    fn notification(&mut self, n: &Notification) -> Vec<Effect> {
        let Some(conn) = self.conn() else {
            return Vec::new();
        };
        match n {
            // Hooks say the tree changed in a way no notification does.
            // The log lines never carry the message text.
            Notification::Message(m) => match Mailbox::parse(m) {
                Ok(Mailbox::Hook(_)) => self.refresh(),
                Ok(Mailbox::Key { action, client, .. }) => vec![Effect::Tmux {
                    conn,
                    cmds: vec![notice(&client, action.not_yet())],
                    token: None,
                }],
                Ok(Mailbox::Prompt { .. }) => vec![log(
                    LogLevel::Info,
                    "a prompt answer arrived that nothing waits for".into(),
                )],
                Err(MailboxError::NotMoa) => vec![log(
                    LogLevel::Warn,
                    "tmux sent a message moa didn't ask for".into(),
                )],
                Err(MailboxError::Bad) => {
                    vec![log(LogLevel::Warn, "a moa message didn't parse".into())]
                }
            },
            // The link reports its own end as `Down`.
            Notification::Exit => Vec::new(),
            n => match self.topology.apply(n) {
                Applied::Refresh => self.refresh(),
                Applied::Changed | Applied::Same => Vec::new(),
            },
        }
    }

    /// The refresh lists came back.
    fn listed(&mut self, results: &[Result<Vec<String>, String>]) -> Vec<Effect> {
        let again = self.refresh == Refresh::Again;
        self.refresh = Refresh::Idle;
        let mut fx = Vec::new();
        match results {
            [Ok(sessions), Ok(panes), Ok(clients)] => {
                self.topology = Topology::from_lists(sessions, panes, clients);
            }
            // No information: what was known stays.
            _ => fx.push(log(
                LogLevel::Warn,
                "listing the terminal server failed".into(),
            )),
        }
        if again {
            fx.extend(self.refresh());
        }
        fx
    }

    /// Answers every waiter with `result`.
    fn answer(&mut self, result: &Result<Done, OpError>) -> Vec<Effect> {
        std::mem::take(&mut self.waiters)
            .into_iter()
            .map(|(conn, req)| Effect::Reply {
                conn,
                req,
                result: result.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
