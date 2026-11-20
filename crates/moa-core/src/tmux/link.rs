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
use crate::hook::HookEnvelope;
use crate::model::SessionId;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::ops::adopt::AdoptArgs;
use crate::ops::launch::ResumeAsk;
use crate::ops::term::Tell;
use crate::panel::View;
use crate::state::State;

/// An op that needs a tab, while moa's terminal isn't linked.
pub(crate) const NO_TERMINAL: &str = "moa isn't open in a terminal yet; run moa first";
/// An op that acts on bindings, before this connection's reconcile (R10).
pub const NOT_READY: &str = "moa is still looking at its tabs; try again in a moment";
/// A prompt's answer (`moa:prompt`) whose token waits for none.
pub(crate) const NOTHING_WAITS: &str = "a prompt answer arrived that nothing waits for";
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
    /// `tmux.ensure` callers waiting for the link: to connect, or, while it is up, for the
    /// next ping.
    waiters: Vec<(ConnId, ReqId)>,
    /// The `tmux.ensure` callers the ping that is out answers.
    pinged: Vec<(ConnId, ReqId)>,
    /// The number of the ping that is out on this connection, if one is: only its answer
    /// counts.
    pinging: Option<u64>,
    next_ping: u64,
    refresh: Refresh,
    /// The last server generation the link connected to.
    generation: Option<Generation>,
    /// The socket of the last server the link connected to: the one a `NoServer` means
    /// (one per daemon). Spike panes are compared with it.
    socket: Option<String>,
    /// The tmux program the front door found.
    tmux: Option<PathBuf>,
    /// The status options last written to the server, by name. Forgotten whenever the
    /// server may not have them.
    pub(super) written: Written,
    /// When the status clock's timer fires, while it is armed.
    pub(super) clock_at: Option<UtcMs>,
    /// The launches waiting for tmux to say which pane their tab has.
    opens: BTreeMap<SessionId, Launching>,
    /// Panes of this server that exited with no session bound to them, oldest first, at
    /// most [`EXITED_KEPT`]: a launch's answer naming one may still be on its way.
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
    /// This connection's reconciles that started and never finished (one that panicked):
    /// kept across a reload, so a reconcile that always panics is tried a bounded number of
    /// times, not again after every reload.
    pub(crate) unfinished_reconciles: u32,
    /// This connection's reconciles were given up on, and the log said so.
    pub(crate) reconcile_given_up: bool,
    /// The `@moa_session` tags moa sent after a saved commit that no listing has shown yet,
    /// by pane. The daemon sends a listing at once but holds the tag until the commit is
    /// released, so a listing can predate the tag: these stay in the topology until one
    /// shows it (R18 reads the tag).
    sent_tags: BTreeMap<PaneId, SessionId>,
    /// The `@moa_view` tags moa set on panels it just started that no listing has shown
    /// yet, by pane: a listing the new window's own notification asked for can predate them
    /// (L7). Kept as `sent_tags` are.
    sent_views: BTreeMap<PaneId, &'static str>,
    /// The terminals moa tagged (`@moa_term`, with their `@moa_label`) that no listing has
    /// shown yet, by pane: the listing the new window's notification asked for can predate
    /// the tags. Kept as `sent_tags` are.
    sent_terms: BTreeMap<PaneId, String>,
}

/// What a pane query was asked for: who reads its answer.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Asked {
    Reconcile,
    /// A resume whose checks passed, looking for a tab that already runs its conversation
    /// (R13 step 6).
    Resume(ResumeAsk),
    /// An adopt, looking at the tabs before it takes one on (R21).
    Adopt {
        conn: ConnId,
        req: ReqId,
        args: AdoptArgs,
    },
    /// An `ls` with the tabs reconciled, looking at them for the agents moa doesn't track
    /// and the adopted ones that left (R23).
    Ls {
        conn: ConnId,
        req: ReqId,
        all: bool,
    },
    /// A terminal's new window (R16), named `name` and opened in `path`: its tab is tagged
    /// and brought to the screen once tmux says which pane it has. `tell` is whom to tell:
    /// nobody, once the caller's connection closed, though the tab still gets its tags (R26).
    Term {
        tell: Option<Tell>,
        name: String,
        path: String,
    },
    /// `Ctrl-a $` from `client` in a pane that is neither a panel's nor an agent's: the
    /// folder tmux says it is in, where the terminal opens (V6).
    Here {
        client: ClientName,
    },
    /// Session `id`, which isn't live and has no tab, was heard from: does a tab run its
    /// agent? `events` are what it sent meanwhile, applied once it is found (the human's
    /// option C).
    Look {
        id: SessionId,
        events: Vec<HookEnvelope>,
    },
    /// Panels started parked, one `new-window` per view, for the cockpit operation `seq`
    /// (L7, L17): each started pane is tagged once tmux says which it is.
    Spawn {
        views: Vec<View>,
        seq: u64,
    },
    /// The fresh listing a cockpit operation starts from (L17): it rebuilds the topology
    /// as a refresh does.
    Scene {
        seq: u64,
    },
    /// A cockpit operation's commands: its chain, or Repair's respawns, one answer each.
    Chain {
        seq: u64,
    },
}

/// A pane query's answer: each command's lines, or `Err` when any command failed or the
/// link went before tmux answered.
pub(crate) type Answer = Result<Vec<Vec<String>>, ()>;

/// A query's answer per command (L18): each command's lines, or tmux's error text. A chain
/// is one command, so one entry.
pub(crate) type Answers = Vec<Result<Vec<String>, String>>;

/// A query's answers as one [`Answer`]: `None` (the link went before tmux answered) or any
/// command that failed fails it.
pub(crate) fn all_lines(answers: Option<Answers>) -> Answer {
    answers
        .ok_or(())?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())
}

/// How many unbound pane exits are remembered.
const EXITED_KEPT: usize = 64;

/// How a launch's new tab turned out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum OpenEnd {
    /// The pane it runs in, and the pane's process.
    Bound { pane: PaneId, pid: i64 },
    /// tmux refused it (its first error line), or answered with something moa can't read.
    Failed(Option<String>),
    /// The link went away before tmux answered.
    Gone,
}

/// What started an agent in a new tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LaunchKind {
    Open,
    Resume,
    /// `moactl restart`: an adopted session's conversation, resumed in a tab moa tracks.
    Restart,
}

/// A launch waiting for tmux to say which pane its tab has.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Launching {
    /// Whom to answer: the caller by its reply, or a key's client (`Ctrl-a n`) by a notice;
    /// nobody, once the caller's connection closed.
    pub reply: Option<Tell>,
    pub kind: LaunchKind,
    /// The tab is brought to the screen once it is bound.
    pub focus: bool,
    /// The `focus` callers waiting for the tab.
    pub waiters: Vec<(ConnId, ReqId)>,
    /// When the launch was committed: the binding's `bound_at`.
    pub launched_at: UtcMs,
}

/// A launch that has its answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Settled {
    pub id: SessionId,
    pub launching: Launching,
    pub end: OpenEnd,
}

impl Default for TmuxState {
    fn default() -> Self {
        TmuxState {
            link: Link::Down,
            topology: Topology::default(),
            restarts: 0,
            waiters: Vec::new(),
            pinged: Vec::new(),
            pinging: None,
            next_ping: 0,
            refresh: Refresh::Idle,
            generation: None,
            socket: None,
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
            unfinished_reconciles: 0,
            reconcile_given_up: false,
            sent_tags: BTreeMap::new(),
            sent_views: BTreeMap::new(),
            sent_terms: BTreeMap::new(),
        }
    }
}

/// What a launch's `new-window -P` prints: the new pane and its process.
pub(crate) const OPENED_FORMAT: &str = "P #{pane_id} #{pane_pid}";

/// What tmux said to a launch's new window, printed with [`OPENED_FORMAT`].
fn open_end(results: &[Result<Vec<String>, String>]) -> OpenEnd {
    let line = match results {
        [Ok(lines)] => match lines.as_slice() {
            [line] => line,
            _ => return OpenEnd::Failed(None),
        },
        [Err(e)] => return OpenEnd::Failed(Some(e.lines().next().unwrap_or("").to_string())),
        _ => return OpenEnd::Failed(None),
    };
    match opened_pane(line) {
        Some((pane, pid)) => OpenEnd::Bound { pane, pid },
        None => OpenEnd::Failed(None),
    }
}

/// The pane and its process in a line printed with [`OPENED_FORMAT`], if it is one.
pub(crate) fn opened_pane(line: &str) -> Option<(PaneId, i64)> {
    let mut words = line.split(' ');
    let (Some("P"), Some(pane), Some(pid), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return None;
    };
    // `i64::from_str` would accept a leading '+'.
    let pid = Some(pid)
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<i64>().ok())
        .filter(|p| *p > 0)?;
    Some((pane.parse::<PaneId>().ok()?, pid))
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

/// The no-op a `Ping` sends: tmux prints the fixed text back.
pub fn ping() -> Cmd {
    Cmd::new("display-message").args(["-p", "moa:ping"])
}

/// A four-second notice on one client. `-l` keeps the text literal.
pub fn notice(client: &ClientName, text: &str) -> Cmd {
    Cmd::new("display-message")
        .arg("-c")
        .arg(client.0.as_str())
        .args(["-d", "4000", "-l"])
        .arg(format!("moa: {text}"))
}

/// Unsets one of moa's options (`@moa_session`, `@moa_adopted`) on one pane.
pub(crate) fn unset_pane_option(pane: PaneId, name: &str) -> Cmd {
    Cmd::new("set-option").args([
        "-p".to_string(),
        "-u".to_string(),
        "-t".to_string(),
        pane.to_string(),
        name.to_string(),
    ])
}

/// Sets one of moa's options (`@moa_session`, `@moa_adopted`) on one pane.
pub(crate) fn set_pane_option(pane: PaneId, name: &str, value: &str) -> Cmd {
    Cmd::new("set-option").args([
        "-p".to_string(),
        "-t".to_string(),
        pane.to_string(),
        name.to_string(),
        value.to_string(),
    ])
}

/// The link's connection, for an op that acts on bindings: up, and this connection's panes
/// reconciled.
pub fn ready(state: &State) -> Result<u64, OpError> {
    let tmux = state.tmux();
    match tmux.link {
        Link::Up { conn, .. } if tmux.reconciled => Ok(conn),
        Link::Up { .. } => Err(OpError(NOT_READY.into())),
        Link::Down | Link::Connecting => Err(OpError(NO_TERMINAL.into())),
    }
}

pub(crate) fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log(LogLine { level, message })
}

/// Moa's tags that a listing doesn't show yet, kept in `topology` (`put`) while it lists
/// their pane and shows no tag of that kind there (`shown`). A tag is sent only once tmux has
/// answered with its pane, so a listing without the pane was asked after it was gone: its
/// tag goes too. One the listing shows, or another one it shows instead, is the listing's.
fn keep_until_listed<T>(
    sent: &mut BTreeMap<PaneId, T>,
    topology: &mut Topology,
    shown: impl Fn(&Topology, PaneId) -> bool,
    put: impl Fn(&mut Topology, PaneId, &T),
) {
    sent.retain(|pane, tag| {
        let keep = !shown(topology, *pane) && topology.has_pane(*pane);
        if keep {
            put(topology, *pane, tag);
        }
        keep
    });
}

/// tmux answered the tags on panes moa just started: only a refusal says anything.
fn tags_answered(results: &[Result<Vec<String>, String>]) -> Vec<Effect> {
    results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .map(|e| {
            let why = e.lines().map(str::trim).find(|l| !l.is_empty());
            let why = why.unwrap_or("tmux gave no reason");
            log(
                LogLevel::Warn,
                format!("tmux didn't tag a panel moa started: {why}"),
            )
        })
        .collect()
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
    /// session is the front door's to make, in the user's environment (T1). While linked,
    /// the answer waits for a ping sent after it: a server killed a moment ago still looks
    /// linked until the link sees its client drop, and the front door would attach to it.
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
            Link::Up { .. } => {
                self.waiters.push((conn, req));
                self.ping().into_iter().collect()
            }
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
                    TmuxToken::Ping(n) => self.pinged(n, &results),
                    TmuxToken::Tags => tags_answered(&results),
                }
            }
        }
    }

    /// After a reload: the link and what it knows stay; its bookkeeping starts over, since a
    /// failed commit or a panic may have left it half-changed. A reconcile's changes may be
    /// among those the reload dropped, so its flag goes too.
    pub(crate) fn reload(&mut self) -> Vec<Effect> {
        self.refresh = Refresh::Idle;
        // A failed commit's held tags were never sent; the refresh below shows the rest.
        self.sent_tags.clear();
        self.sent_views.clear();
        self.sent_terms.clear();
        self.written.reset();
        self.reconciled = false;
        // The ping out may have been lost with the handling that sent it: its callers wait
        // for a new one, and its answer, if it comes, is dropped.
        self.unping();
        match self.link {
            Link::Up { .. } => {
                let mut fx = self.refresh();
                fx.extend(self.ping());
                fx
            }
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

    /// Holds a launch's answers until tmux says how its tab turned out.
    pub(crate) fn wait_open(&mut self, id: SessionId, launching: Launching) {
        self.opens.insert(id, launching);
    }

    /// The launch of `id` waiting for its tab, if there is one.
    pub(crate) fn launching(&mut self, id: SessionId) -> Option<&mut Launching> {
        self.opens.get_mut(&id)
    }

    /// Whether `id` is on its way to a tab: its launch waits for its pane, or a resume of it
    /// is still looking at the tabs (and will launch it unless they show it running).
    pub(crate) fn opening(&self, id: SessionId) -> bool {
        self.opens.contains_key(&id)
            || self
                .queries
                .values()
                .any(|a| matches!(a, Asked::Resume(r) if r.checked.id == id))
    }

    /// `conn` closed: its ensures, resumes, adopts and `ls` still looking at the tabs and
    /// focus waiters are forgotten, and its launches and terminals go on with nobody to
    /// answer.
    pub(crate) fn client_gone(&mut self, conn: ConnId) {
        self.waiters.retain(|(c, _)| *c != conn);
        self.pinged.retain(|(c, _)| *c != conn);
        self.queries.retain(|_, a| match a {
            Asked::Resume(r) => r.conn != conn,
            Asked::Adopt { conn: c, .. } | Asked::Ls { conn: c, .. } => *c != conn,
            Asked::Reconcile
            | Asked::Term { .. }
            | Asked::Here { .. }
            | Asked::Look { .. }
            | Asked::Spawn { .. }
            | Asked::Scene { .. }
            | Asked::Chain { .. } => true,
        });
        for asked in self.queries.values_mut() {
            if let Asked::Term { tell, .. } = asked
                && matches!(tell, Some(Tell::Reply(c, _)) if *c == conn)
            {
                *tell = None;
            }
        }
        for launching in self.opens.values_mut() {
            if matches!(launching.reply, Some(Tell::Reply(c, _)) if c == conn) {
                launching.reply = None;
            }
            launching.waiters.retain(|(c, _)| *c != conn);
        }
    }

    /// The sessions whose launch is waiting.
    pub(crate) fn waiting_opens(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.opens.keys().copied()
    }

    /// The launches waiting, each with its session.
    pub(crate) fn launches(&self) -> impl Iterator<Item = (SessionId, &Launching)> + '_ {
        self.opens.iter().map(|(id, l)| (*id, l))
    }

    /// Stops waiting for `id`'s tab, and gives back its launch.
    pub(crate) fn forget_open(&mut self, id: SessionId) -> Option<Launching> {
        self.opens.remove(&id)
    }

    /// The launches this input answers: the one whose tab tmux reports on, or every one
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
                .map(|launching| Settled {
                    id: *id,
                    launching,
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

    /// The pane queries this input answers: the one tmux replied to, with each command's
    /// answer, or every one, with `None`, when the link goes down or a new connection
    /// replaces the one they were asked on. Run before [`TmuxState::input`], like
    /// [`TmuxState::settle`].
    pub(crate) fn answered(&mut self, input: &TmuxIn) -> Vec<(Asked, Option<Answers>)> {
        match input {
            TmuxIn::Replies {
                conn,
                token: TmuxToken::Query(n),
                results,
            } if self.conn() == Some(*conn) => self
                .queries
                .remove(n)
                .map(|asked| (asked, Some(results.clone())))
                .into_iter()
                .collect(),
            TmuxIn::Down(_) => self.all_failed(),
            TmuxIn::Connected { conn, .. } if self.conn().is_some_and(|c| c != *conn) => {
                self.all_failed()
            }
            _ => Vec::new(),
        }
    }

    fn all_failed(&mut self) -> Vec<(Asked, Option<Answers>)> {
        std::mem::take(&mut self.queries)
            .into_values()
            .map(|asked| (asked, None))
            .collect()
    }

    /// The pane queries waiting for their answer, by number.
    pub fn queries(&self) -> impl Iterator<Item = u64> + '_ {
        self.queries.keys().copied()
    }

    /// The events waiting with the look at the tabs asked for session `id`, while one is.
    pub(crate) fn looking(&mut self, id: SessionId) -> Option<&mut Vec<HookEnvelope>> {
        self.queries.values_mut().find_map(|a| match a {
            Asked::Look { id: i, events } if *i == id => Some(events),
            _ => None,
        })
    }

    /// Whether a query for `asked` is waiting for its answer.
    pub(crate) fn asking(&self, asked: &Asked) -> bool {
        self.queries.values().any(|a| a == asked)
    }

    /// The last server generation the link connected to, kept while it is down.
    pub(crate) fn generation(&self) -> Option<Generation> {
        self.generation
    }

    /// Whether this connection's panes have been reconciled.
    pub fn reconciled(&self) -> bool {
        self.reconciled
    }

    /// This connection's panes are reconciled: the bindings can be trusted until the next
    /// connection or reload.
    pub(crate) fn set_reconciled(&mut self) {
        self.reconciled = self.conn().is_some();
    }

    /// The socket of the last server the link connected to, if it ever connected.
    pub(crate) fn last_socket(&self) -> Option<&str> {
        self.socket.as_deref()
    }

    fn all_gone(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.opens)
            .into_iter()
            .map(|(id, launching)| Settled {
                id,
                launching,
                end: OpenEnd::Gone,
            })
            .collect()
    }

    /// Whether `input` connects to another server than the one last connected to: its pane
    /// ids name other panes.
    pub(crate) fn another_server(&self, input: &TmuxIn) -> bool {
        matches!(input, TmuxIn::Connected { generation, .. }
            if self.generation.is_some_and(|g| g != *generation))
    }

    /// The link's connection, while it is up.
    pub(crate) fn conn(&self) -> Option<u64> {
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

    /// Sends the ping for the ensures that wait, unless one is out (its answer may predate
    /// them: they wait for the next) or none waits.
    fn ping(&mut self) -> Option<Effect> {
        let conn = self.conn()?;
        if self.pinging.is_some() || self.waiters.is_empty() {
            return None;
        }
        self.next_ping += 1;
        self.pinging = Some(self.next_ping);
        self.pinged = std::mem::take(&mut self.waiters);
        Some(Effect::Tmux {
            conn,
            cmds: vec![ping()],
            token: Some(TmuxToken::Ping(self.next_ping)),
        })
    }

    /// No ping is out any more: its callers wait again, ahead of those who came after.
    fn unping(&mut self) {
        self.pinging = None;
        let later = std::mem::take(&mut self.waiters);
        self.waiters = std::mem::take(&mut self.pinged);
        self.waiters.extend(later);
    }

    /// Ping `n` came back. Answered, the server runs; failed, the link drops the client
    /// and says `Lost`, and its callers wait for what the link finds next.
    fn pinged(&mut self, n: u64, results: &[Result<Vec<String>, String>]) -> Vec<Effect> {
        if self.pinging != Some(n) || results.iter().any(Result::is_err) {
            return Vec::new();
        }
        self.pinging = None;
        let mut fx: Vec<Effect> = std::mem::take(&mut self.pinged)
            .into_iter()
            .map(|(conn, req)| Effect::Reply {
                conn,
                req,
                result: running(true),
            })
            .collect();
        fx.extend(self.ping());
        fx
    }

    /// Moa tagged `pane` with session `id`, after a saved commit: the topology knows it at
    /// once, and keeps it through listings that don't show it yet.
    pub(crate) fn tag_sent(&mut self, pane: PaneId, id: SessionId) {
        self.sent_tags.insert(pane, id);
        self.topology.tag(pane, id);
    }

    /// Moa tagged the panel it just started in `pane` with `view` (L7): the topology knows
    /// it at once, and keeps it through listings that don't show it yet.
    pub(crate) fn view_sent(&mut self, pane: PaneId, view: &'static str) {
        self.sent_views.insert(pane, view);
        self.topology.set_view(pane, view);
    }

    /// Moa tagged the terminal it just opened in `pane` (`@moa_term`), labelled `label`:
    /// the topology knows it at once, and keeps it through listings that don't show it yet.
    pub(crate) fn term_sent(&mut self, pane: PaneId, label: &str) {
        self.sent_terms.insert(pane, label.to_string());
        self.topology.set_term(pane, label);
    }

    /// A cockpit chain that tmux ran set these `@moa_view`s (`None`: unset), as L17 records
    /// them: the topology knows them at once, and a view a listing hasn't shown yet (a
    /// panel just started) is kept as the chain left it, not as it was started.
    pub(crate) fn views_set(&mut self, views: &[(PaneId, Option<&'static str>)]) {
        for (pane, view) in views {
            match view {
                Some(view) => {
                    if let Some(sent) = self.sent_views.get_mut(pane) {
                        *sent = view;
                    }
                    self.topology.set_view(*pane, view);
                }
                None => {
                    self.sent_views.remove(pane);
                    self.topology.unset_view(*pane);
                }
            }
        }
    }

    /// A cockpit operation's own listing came back (L17): the topology is rebuilt from it
    /// as from a refresh's. `false` when it couldn't be read; what was known stays.
    pub(crate) fn scene_listed(&mut self, results: &[Result<Vec<String>, String>]) -> bool {
        self.rebuild(results)
    }

    /// Moa took the session tag off `pane`, after a saved commit: neither the topology nor
    /// the tags waiting for a listing keep it.
    pub(crate) fn untag_sent(&mut self, pane: PaneId) {
        self.sent_tags.remove(&pane);
        self.topology.untag(pane);
    }

    /// Reads the lists again, at most one in flight and one more after it.
    pub(crate) fn refresh(&mut self) -> Vec<Effect> {
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
        let fresh = self.conn() != Some(conn);
        if fresh {
            // A new connection: nothing asked on another one will be answered, and its
            // panes are yet to be reconciled, with a fresh count of tries.
            self.refresh = Refresh::Idle;
            self.unping();
            self.reconciled = false;
            self.sent_tags.clear();
            self.sent_views.clear();
            self.sent_terms.clear();
            self.unfinished_reconciles = 0;
            self.reconcile_given_up = false;
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
        self.socket = Some(socket.clone());
        self.link = Link::Up {
            conn,
            generation,
            version,
            ctl,
            socket,
        };
        self.written.reset();
        if fresh {
            // The connection was just made: the server answered it.
            fx.extend(self.answer(&running(true)));
        } else {
            // The same connection said again (a connect asked again by a reload) proves
            // nothing new: the ensures waiting get a ping, or share the one out.
            fx.extend(self.ping());
        }
        fx.extend(self.refresh());
        fx
    }

    fn down(&mut self, d: Down) -> Vec<Effect> {
        self.last_down = Some(d.clone());
        self.reconciled = false;
        // A ping's callers wait for what the link finds, as those who came while it
        // connected do.
        self.unping();
        self.link = match d {
            // The link is reconnecting.
            Down::Lost => Link::Connecting,
            _ => Link::Down,
        };
        self.topology = Topology::default();
        self.sent_tags.clear();
        self.sent_views.clear();
        self.sent_terms.clear();
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
                // The keys the core acts on never get here (`keys::run`).
                Ok(Mailbox::Key { action, client, .. }) => action
                    .not_yet()
                    .map(|text| Effect::Tmux {
                        conn,
                        cmds: vec![notice(&client, text)],
                        token: None,
                    })
                    .into_iter()
                    .collect(),
                // The answers that prompts wait for never get here (`prompt::answered`).
                Ok(Mailbox::Prompt { .. }) => vec![log(LogLevel::Info, NOTHING_WAITS.into())],
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
        if !self.rebuild(results) {
            fx.push(log(
                LogLevel::Warn,
                "listing the terminal server failed".into(),
            ));
        }
        if again {
            fx.extend(self.refresh());
        }
        fx
    }

    /// The topology from the lists `refresh_cmds` asks for, keeping the tags sent that they
    /// don't show yet. `false`, with what was known kept, when they aren't those lists.
    fn rebuild(&mut self, results: &[Result<Vec<String>, String>]) -> bool {
        let (sessions, panes, clients, terms) = match results {
            [Ok(sessions), Ok(panes), Ok(clients), Ok(terms)] => {
                (sessions, panes, clients, terms.as_slice())
            }
            // Three lists, as before terminals were listed: none is.
            [Ok(sessions), Ok(panes), Ok(clients)] => (sessions, panes, clients, &[][..]),
            // No information: what was known stays.
            _ => return false,
        };
        self.topology = Topology::from_lists(sessions, panes, clients);
        self.topology.terms_listed(terms);
        // A tag the listing doesn't show yet may have been sent after it was asked.
        let topology = &mut self.topology;
        keep_until_listed(
            &mut self.sent_tags,
            topology,
            |t, p| t.moa_session(p).is_some(),
            |t, p, id| t.tag(p, *id),
        );
        keep_until_listed(
            &mut self.sent_views,
            topology,
            |t, p| t.view_of(p).is_some(),
            |t, p, view| t.set_view(p, view),
        );
        keep_until_listed(
            &mut self.sent_terms,
            topology,
            |t, p| t.term_label(p).is_some(),
            |t, p, label| t.set_term(p, label),
        );
        true
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
