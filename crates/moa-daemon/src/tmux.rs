//! The terminal link: the actor that owns the daemon's tmux control client. It finds the
//! server (it never starts one), connects and bootstraps the client, carries commands and
//! events between the core and the server, reconnects when the client drops, and takes
//! moa's hooks down on a clean stop.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moa_core::effect::{Down, TmuxIn, TmuxToken};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::notify::Notification;
use moa_platform::{TmuxSocket, which};
use moa_tmux::client::{
    CmdError, ConnectError, ConnectOptions, ControlClient, Event, Events, ServerInfo,
};
use moa_tmux::conf::{hooks, unhooks};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};
use tokio::time::Instant;

use crate::core_task::lock;
use crate::supervise::supervise;

/// The waits before each reconnect, once the control client has dropped while the server
/// is still there (T4). Then the link gives up until it is asked again.
const BACKOFF: [Duration; 6] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
    Duration::from_millis(1600),
    Duration::from_millis(3200),
];

/// Which server the daemon links to, and how its control client is started.
#[derive(Clone, Debug)]
pub struct TmuxLink {
    pub socket: TmuxSocket,
    /// moa's own config, sourced on every connect.
    pub moa_tmux: PathBuf,
    /// The control client's whole environment (see [`tmux_env`]).
    pub env: Vec<(OsString, OsString)>,
}

/// What the control client keeps of the daemon's environment (T3): `HOME`, `USER`,
/// `LOGNAME`, `PATH`, `LANG`, `LC_*` and `TMUX_TMPDIR`.
pub fn tmux_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    parent
        .filter(|(k, _)| match k.to_str() {
            Some("HOME" | "USER" | "LOGNAME" | "PATH" | "LANG" | "TMUX_TMPDIR") => true,
            Some(k) => k.starts_with("LC_"),
            None => false,
        })
        .collect()
}

/// What the core asks of the link.
pub(crate) enum TmuxCmd {
    /// Connect, with this tmux program if one is known. While connected the link says so
    /// again; while it backs off it says nothing until it knows.
    Connect { tmux: Option<PathBuf> },
    /// Commands for connection `conn`. With a token, their answers come back as `Replies`.
    Send {
        conn: u64,
        cmds: Vec<Cmd>,
        token: Option<TmuxToken>,
    },
    /// Take moa's hooks and `@moa_ctl` down, close the client, then answer.
    Stop(oneshot::Sender<()>),
    /// Tests only: the actor panics.
    #[cfg(test)]
    Crash,
}

/// A token travels to the control client as a tag, and back.
fn tag_of(token: TmuxToken) -> u64 {
    match token {
        TmuxToken::Refresh => 1,
        TmuxToken::Cockpit => 2,
    }
}

fn token_of(tag: u64) -> Option<TmuxToken> {
    match tag {
        1 => Some(TmuxToken::Refresh),
        2 => Some(TmuxToken::Cockpit),
        _ => None,
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// How long a connection must stay up before a loss starts the backoff afresh: the longest
/// wait. A client dropped sooner continues the backoff, so a server that keeps dropping
/// it still costs at most 1 + 6 launches before the link gives up.
const STEADY: Duration = BACKOFF[BACKOFF.len() - 1];

/// What the link was doing, kept across a restart so the next run knows whether to
/// reconnect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Doing {
    Idle,
    Connecting,
    Up,
    BackingOff,
}

/// What outlives a restart: the inbox, the connection numbers, the tmux program the front
/// door named, and what the link was doing.
struct Shared {
    link: TmuxLink,
    to_core: Box<dyn Fn(TmuxIn) + Send + Sync>,
    inbox: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<TmuxCmd>>>,
    /// The last connection's number: every connect takes the next, restarts included.
    conns: AtomicU64,
    tmux: Mutex<Option<PathBuf>>,
    doing: Mutex<Doing>,
}

/// Starts the link under `supervise` as "the terminal link". It does nothing until it is
/// asked to connect; `to_core` hears everything it has to say.
pub(crate) fn start(
    link: TmuxLink,
    to_core: impl Fn(TmuxIn) + Send + Sync + 'static,
    failed: impl Fn(&'static str, String) + Send + 'static,
) -> mpsc::UnboundedSender<TmuxCmd> {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        link,
        to_core: Box::new(to_core),
        inbox: Arc::new(tokio::sync::Mutex::new(rx)),
        conns: AtomicU64::new(0),
        tmux: Mutex::new(None),
        doing: Mutex::new(Doing::Idle),
    });
    let mut started = false;
    tokio::spawn(supervise(
        "the terminal link",
        move || {
            let restarted = std::mem::replace(&mut started, true);
            let shared = shared.clone();
            async move {
                let inbox = shared.inbox.clone().lock_owned().await;
                Actor {
                    sh: shared,
                    state: State::Idle,
                }
                .run(inbox, restarted)
                .await;
            }
        },
        failed,
    ));
    tx
}

/// A linked control client.
struct Up {
    conn: u64,
    client: ControlClient,
    events: Events,
    info: ServerInfo,
    since: Instant,
    /// The backoff step that connected it, if one did; a loss before `STEADY` goes on from
    /// there.
    tried: Option<usize>,
}

impl Up {
    fn connected(&self) -> TmuxIn {
        TmuxIn::Connected {
            conn: self.conn,
            generation: self.info.generation,
            version: self.info.version.clone(),
            ctl: self.info.client.clone(),
        }
    }
}

enum State {
    Idle,
    Up(Up),
    /// Waiting to try `BACKOFF[next]`'s connect at `at`.
    Backoff {
        next: usize,
        at: Instant,
    },
}

enum Step {
    Cmd(Option<TmuxCmd>),
    Event(Option<Event>),
    Retry(usize),
}

struct Actor {
    sh: Arc<Shared>,
    state: State,
}

impl Actor {
    async fn run(
        mut self,
        mut inbox: OwnedMutexGuard<mpsc::UnboundedReceiver<TmuxCmd>>,
        restarted: bool,
    ) {
        let mut pending = VecDeque::new();
        if restarted {
            // A stop, or a core that has gone, comes before any reconnect.
            loop {
                match inbox.try_recv() {
                    Ok(TmuxCmd::Stop(done)) => {
                        let _ = done.send(());
                        return;
                    }
                    Ok(cmd) => {
                        // The reconnect below uses the program a queued connect names.
                        if let TmuxCmd::Connect { tmux: Some(t) } = &cmd {
                            *lock(&self.sh.tmux) = Some(t.clone());
                        }
                        pending.push_back(cmd);
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return,
                }
            }
            let doing = *lock(&self.sh.doing);
            if doing == Doing::Up {
                // The core still thinks a client is up: it hears of the loss first.
                self.tell(TmuxIn::Down(Down::Lost));
            }
            // After a give-up or no server the link was idle, and stays so until asked.
            if doing != Doing::Idle {
                self.connect(None).await;
            }
        }
        loop {
            let step = match pending.pop_front() {
                Some(cmd) => Step::Cmd(Some(cmd)),
                None => self.next(&mut inbox).await,
            };
            match step {
                // The core has gone without a stop.
                Step::Cmd(None) => {
                    self.clean_up().await;
                    return;
                }
                Step::Cmd(Some(TmuxCmd::Stop(done))) => {
                    self.clean_up().await;
                    let _ = done.send(());
                    return;
                }
                Step::Cmd(Some(TmuxCmd::Connect { tmux })) => self.connect_asked(tmux).await,
                Step::Cmd(Some(TmuxCmd::Send { conn, cmds, token })) => {
                    self.send(conn, cmds, token)
                }
                #[cfg(test)]
                Step::Cmd(Some(TmuxCmd::Crash)) => panic!("a test crashed the terminal link"),
                Step::Event(Some(ev)) => self.event(ev),
                Step::Event(None) => {
                    let after = match &self.state {
                        State::Up(up) if up.since.elapsed() < STEADY => up.tried,
                        _ => None,
                    };
                    self.lost(after).await;
                }
                Step::Retry(next) => self.retry(next).await,
            }
        }
    }

    async fn next(&mut self, inbox: &mut mpsc::UnboundedReceiver<TmuxCmd>) -> Step {
        match &mut self.state {
            State::Idle => Step::Cmd(inbox.recv().await),
            State::Up(up) => tokio::select! {
                cmd = inbox.recv() => Step::Cmd(cmd),
                ev = up.events.recv() => Step::Event(ev),
            },
            State::Backoff { next, at } => {
                let (next, at) = (*next, *at);
                tokio::select! {
                    cmd = inbox.recv() => Step::Cmd(cmd),
                    () = tokio::time::sleep_until(at) => Step::Retry(next),
                }
            }
        }
    }

    fn tell(&self, input: TmuxIn) {
        (self.sh.to_core)(input);
    }

    fn mark(&self, doing: Doing) {
        *lock(&self.sh.doing) = doing;
    }

    /// Moves to `state`, noting it for a restart; returns the old one.
    fn set(&mut self, state: State) -> State {
        self.mark(match state {
            State::Idle => Doing::Idle,
            State::Up(_) => Doing::Up,
            State::Backoff { .. } => Doing::BackingOff,
        });
        std::mem::replace(&mut self.state, state)
    }

    fn down(&mut self, why: Down) {
        self.set(State::Idle);
        self.tell(TmuxIn::Down(why));
    }

    /// Waits for the step after `tried` (the first, after a first try), or gives up when
    /// the steps have run out.
    fn back_off(&mut self, tried: Option<usize>) {
        let next = tried.map_or(0, |k| k + 1);
        match BACKOFF.get(next) {
            Some(wait) => {
                self.set(State::Backoff {
                    next,
                    at: Instant::now() + *wait,
                });
            }
            None => {
                tracing::warn!(
                    "the terminal link gave up after {} reconnects",
                    BACKOFF.len()
                );
                self.down(Down::GaveUp);
            }
        }
    }

    /// One client at most: connected, it says so again; backing off, it says nothing yet.
    async fn connect_asked(&mut self, tmux: Option<PathBuf>) {
        if let Some(t) = tmux {
            *lock(&self.sh.tmux) = Some(t);
        }
        match &self.state {
            State::Up(up) => self.tell(up.connected()),
            State::Backoff { .. } => {}
            State::Idle => self.connect(None).await,
        }
    }

    /// The program named by the front door, else the one on the daemon's `PATH`.
    fn find_tmux(&self) -> Option<PathBuf> {
        let named = lock(&self.sh.tmux).clone();
        named.or_else(|| which("tmux", self.env("PATH").as_deref()))
    }

    fn env(&self, key: &str) -> Option<OsString> {
        let env = &self.sh.link.env;
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// Whether a server answers on the socket: connecting is enough (T4). Only "not found"
    /// and "refused" mean there is none; anything else is left for tmux to explain.
    async fn probe(&self) -> bool {
        let tmpdir = self.env("TMUX_TMPDIR").filter(|v| !v.is_empty());
        let path = self.sh.link.socket.path(
            tmpdir.as_deref().map(std::path::Path::new),
            moa_platform::current_uid(),
        );
        match tokio::net::UnixStream::connect(&path).await {
            Ok(_) => true,
            Err(e) => !matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ),
        }
    }

    /// Finds the program and the server, then connects. `tried` is the backoff step this
    /// is, if it is one.
    async fn connect(&mut self, tried: Option<usize>) {
        self.mark(Doing::Connecting);
        let Some(tmux) = self.find_tmux() else {
            return self.down(Down::NoTmux);
        };
        if !self.probe().await {
            return self.down(Down::NoServer);
        }
        self.attempt(tmux, tried).await;
    }

    /// One connect. `tried` is the backoff step this is, if it is one.
    async fn attempt(&mut self, tmux: PathBuf, tried: Option<usize>) {
        let opts = ConnectOptions {
            tmux,
            socket: self.sh.link.socket.args().to_vec(),
            env: self.sh.link.env.clone(),
        };
        match ControlClient::connect(&opts).await {
            Ok((client, events, info)) => self.bootstrap(client, events, info, tried).await,
            Err(ConnectError::TooOld(v)) => self.down(Down::TooOld(v)),
            Err(ConnectError::Spawn(e)) => {
                tracing::warn!("tmux couldn't be started: {}", e.kind());
                // The next ensure names one again, or `PATH` has one.
                *lock(&self.sh.tmux) = None;
                self.down(Down::NoTmux);
            }
            Err(ConnectError::Exited(line)) => {
                tracing::info!(
                    "the terminal server turned the link away: {}",
                    first_line(&line)
                );
                self.back_off(tried);
            }
            Err(e @ ConnectError::Protocol(_)) => {
                tracing::warn!("the terminal link couldn't connect: {e}");
                self.down(Down::GaveUp);
            }
        }
    }

    /// Sources moa's config, names the client in `@moa_ctl` and sets the hooks, then tells
    /// the core it is up. `tried` is the backoff step that connected it, if one did.
    async fn bootstrap(
        &mut self,
        client: ControlClient,
        events: Events,
        info: ServerInfo,
        tried: Option<usize>,
    ) {
        let conn = self.sh.conns.fetch_add(1, Ordering::SeqCst) + 1;
        let moa_tmux = self.sh.link.moa_tmux.to_string_lossy().into_owned();
        let steps = [
            Cmd::new("source-file").arg(moa_tmux),
            Cmd::new("set-option").args(["-g", "@moa_ctl", info.client.0.as_str()]),
        ];
        for cmd in &steps {
            match client.run(cmd).await {
                Ok(_) => {}
                Err(CmdError::Gone) => {
                    client.close().await;
                    return self.lost(tried).await;
                }
                Err(e) => tracing::warn!(
                    "{} failed while linking: {}",
                    cmd.name(),
                    first_line(&e.to_string())
                ),
            }
        }
        client.send(hooks(&info.client), None);
        let up = Up {
            conn,
            client,
            events,
            info,
            since: Instant::now(),
            tried,
        };
        self.tell(up.connected());
        self.set(State::Up(up));
    }

    /// The client dropped. The core hears so before anything else, then the link finds out
    /// whether the server is still there and, if it is, reconnects after the wait for the
    /// step after `after` (the first step when the client had stayed up, or none was
    /// counted).
    async fn lost(&mut self, after: Option<usize>) {
        if let State::Up(up) = self.set(State::Idle) {
            up.client.close().await;
        }
        tracing::info!("the terminal link lost its client");
        self.tell(TmuxIn::Down(Down::Lost));
        if !self.probe().await {
            return self.down(Down::NoServer);
        }
        self.back_off(after);
    }

    async fn retry(&mut self, next: usize) {
        self.state = State::Idle;
        self.connect(Some(next)).await;
    }

    fn send(&self, conn: u64, cmds: Vec<Cmd>, token: Option<TmuxToken>) {
        match &self.state {
            State::Up(up) if up.conn == conn => up.client.send(cmds, token.map(tag_of)),
            // The commands stay out of the log.
            _ => tracing::debug!(conn, cmds = cmds.len(), "not on that connection: dropped"),
        }
    }

    fn event(&self, ev: Event) {
        let State::Up(up) = &self.state else {
            return;
        };
        match ev {
            // The end of the events says it.
            Event::Notification(Notification::Exit) => {}
            Event::Notification(n) => self.tell(TmuxIn::Notification(n)),
            Event::Replies { tag, results } => match token_of(tag) {
                Some(token) => self.tell(TmuxIn::Replies {
                    conn: up.conn,
                    token,
                    results: results
                        .into_iter()
                        .map(|r| r.map_err(|e| e.to_string()))
                        .collect(),
                }),
                None => tracing::warn!(tag, "an answer came back that nothing asked for"),
            },
        }
    }

    /// Takes down what the link put up, so keys say moa isn't running, and closes the
    /// client.
    async fn clean_up(&mut self) {
        let State::Up(up) = self.set(State::Idle) else {
            return;
        };
        let unset = Cmd::new("set-option").args(["-gu", "@moa_ctl"]);
        for cmd in unhooks().iter().chain([&unset]) {
            match up.client.run(cmd).await {
                Ok(_) => {}
                Err(CmdError::Gone) => break,
                Err(e) => tracing::warn!(
                    "{} failed while unlinking: {}",
                    cmd.name(),
                    first_line(&e.to_string())
                ),
            }
        }
        up.client.close().await;
    }
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tests;
