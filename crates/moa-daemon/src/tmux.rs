//! The terminal link: the actor that owns the daemon's tmux control client. It finds the
//! server (it never starts one), connects and bootstraps the client, carries commands and
//! events between the core and the server, reconnects when the client drops, and takes
//! moa's hooks down on a clean stop.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moa_core::effect::{Down, TmuxIn, TmuxToken};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::notify::Notification;
use moa_platform::{TmuxSocket, which};
use moa_tmux::client::{
    CONNECT_WAIT, CmdError, ConnectError, ConnectOptions, ControlClient, Event, Events, ServerInfo,
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

/// How long each command of a connect's bootstrap, or of a stop's clean-up, may take. A
/// server that hangs mid-way is treated as gone.
const STEP_WAIT: Duration = Duration::from_secs(2);

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

/// The server's socket as a pane binding names it: the `-L` name, or the `-S` path.
fn socket_text(socket: &TmuxSocket) -> String {
    match socket {
        TmuxSocket::Name(n) => n.clone(),
        TmuxSocket::Path(p) => p.display().to_string(),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// What the link was doing, kept across a restart so the next run knows whether to
/// reconnect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Doing {
    Idle,
    Connecting,
    Up,
    /// Waiting to try this backoff step.
    BackingOff(usize),
}

/// What a restarted run does first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AfterRestart {
    /// After a give-up or no server the link was idle, and stays so until asked.
    StayIdle,
    Connect,
    /// Waits for this backoff step again and tries it: a restart doesn't start the backoff
    /// over.
    BackOff(usize),
}

/// A panic during a backoff step's own connect left `Connecting`, so that restart connects
/// afresh and counts from step 0; only a restart during the wait resumes its step.
fn after_restart(doing: Doing) -> AfterRestart {
    match doing {
        Doing::Idle => AfterRestart::StayIdle,
        Doing::Connecting | Doing::Up => AfterRestart::Connect,
        Doing::BackingOff(next) => AfterRestart::BackOff(next),
    }
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
    /// Tests only: how many times the actor has started, restarts included.
    #[cfg(test)]
    runs: Arc<std::sync::atomic::AtomicUsize>,
    /// Tests only: the backoff's waits, `BACKOFF` unless a test sets its own.
    #[cfg(test)]
    backoff: [Duration; BACKOFF.len()],
    /// Tests only: how long a connect waits for the server, `CONNECT_WAIT` unless a test
    /// sets its own.
    #[cfg(test)]
    connect_wait: Duration,
    /// Tests only: each backoff step as it is scheduled, with the run that scheduled it.
    #[cfg(test)]
    scheduled: Mutex<Vec<(usize, usize)>>,
    /// Tests only: the next probe panics.
    #[cfg(test)]
    panic_probe: std::sync::atomic::AtomicBool,
}

impl Shared {
    /// What a new link shares across its restarts, and the sender of its inbox.
    fn new(
        link: TmuxLink,
        to_core: Box<dyn Fn(TmuxIn) + Send + Sync>,
    ) -> (mpsc::UnboundedSender<TmuxCmd>, Shared) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Shared {
            link,
            to_core,
            inbox: Arc::new(tokio::sync::Mutex::new(rx)),
            conns: AtomicU64::new(0),
            tmux: Mutex::new(None),
            doing: Mutex::new(Doing::Idle),
            #[cfg(test)]
            runs: Arc::default(),
            #[cfg(test)]
            backoff: BACKOFF,
            #[cfg(test)]
            connect_wait: CONNECT_WAIT,
            #[cfg(test)]
            scheduled: Mutex::default(),
            #[cfg(test)]
            panic_probe: std::sync::atomic::AtomicBool::new(false),
        };
        (tx, shared)
    }

    /// Tests only: what the link is doing.
    #[cfg(test)]
    fn doing(&self) -> Doing {
        *lock(&self.doing)
    }
}

/// Starts the link under `supervise` as "the terminal link". It does nothing until it is
/// asked to connect; `to_core` hears everything it has to say.
pub(crate) fn start(
    link: TmuxLink,
    to_core: impl Fn(TmuxIn) + Send + Sync + 'static,
    failed: impl Fn(&'static str, String) + Send + 'static,
) -> mpsc::UnboundedSender<TmuxCmd> {
    let (tx, shared) = Shared::new(link, Box::new(to_core));
    supervised(Arc::new(shared), failed);
    tx
}

/// Runs the actor on `shared` under `supervise`. A panic while a client is up is told to the
/// core as a loss at once, before `failed` hears of it; the restart reconnects.
fn supervised(shared: Arc<Shared>, failed: impl Fn(&'static str, String) + Send + 'static) {
    let sh = shared.clone();
    let failed = move |name, why| {
        let was_up = {
            let mut doing = lock(&sh.doing);
            let was_up = *doing == Doing::Up;
            if was_up {
                *doing = Doing::Connecting;
            }
            was_up
        };
        if was_up {
            (sh.to_core)(TmuxIn::Down(Down::Lost));
        }
        failed(name, why);
    };
    let mut started = false;
    tokio::spawn(supervise(
        "the terminal link",
        move || {
            let restarted = std::mem::replace(&mut started, true);
            let shared = shared.clone();
            #[cfg(test)]
            shared.runs.fetch_add(1, Ordering::SeqCst);
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
    /// Each token travels to the control client as a tag, and back: the tokens of this
    /// client's sends still waiting for their answers, by tag.
    tokens: BTreeMap<u64, TmuxToken>,
    last_tag: u64,
}

impl Up {
    fn connected(&self, socket: &TmuxSocket) -> TmuxIn {
        TmuxIn::Connected {
            conn: self.conn,
            generation: self.info.generation,
            version: self.info.version.clone(),
            ctl: self.info.client.clone(),
            socket: socket_text(socket),
        }
    }

    fn tag(&mut self, token: TmuxToken) -> u64 {
        self.last_tag += 1;
        self.tokens.insert(self.last_tag, token);
        self.last_tag
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
            match after_restart(doing) {
                AfterRestart::StayIdle => {}
                AfterRestart::Connect => self.connect(None).await,
                AfterRestart::BackOff(next) => self.schedule(next),
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
                    let steady = self.steady();
                    let after = match &self.state {
                        State::Up(up) if up.since.elapsed() < steady => up.tried,
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
            State::Backoff { next, .. } => Doing::BackingOff(next),
        });
        std::mem::replace(&mut self.state, state)
    }

    fn down(&mut self, why: Down) {
        self.set(State::Idle);
        self.tell(TmuxIn::Down(why));
    }

    /// The backoff's waits.
    #[cfg(not(test))]
    fn waits(&self) -> [Duration; BACKOFF.len()] {
        BACKOFF
    }

    /// The backoff's waits: a test can set its own.
    #[cfg(test)]
    fn waits(&self) -> [Duration; BACKOFF.len()] {
        self.sh.backoff
    }

    /// How long a connection must stay up before a loss starts the backoff afresh: the
    /// longest wait. A client dropped sooner continues the backoff, so a server that keeps
    /// dropping it still costs at most 1 + 6 launches before the link gives up.
    fn steady(&self) -> Duration {
        self.waits()[BACKOFF.len() - 1]
    }

    /// Waits for backoff step `next`, then tries it.
    fn schedule(&mut self, next: usize) {
        #[cfg(test)]
        lock(&self.sh.scheduled).push((self.sh.runs.load(Ordering::SeqCst), next));
        let at = Instant::now() + self.waits()[next];
        self.set(State::Backoff { next, at });
    }

    /// Waits for the step after `tried` (the first, after a first try), or gives up when
    /// the steps have run out.
    fn back_off(&mut self, tried: Option<usize>) {
        let next = tried.map_or(0, |k| k + 1);
        match BACKOFF.get(next) {
            Some(_) => self.schedule(next),
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
            State::Up(up) => self.tell(up.connected(&self.sh.link.socket)),
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
        #[cfg(test)]
        if self.sh.panic_probe.swap(false, Ordering::SeqCst) {
            panic!("a test crashed the terminal link's probe");
        }
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
        #[cfg(not(test))]
        let wait = CONNECT_WAIT;
        #[cfg(test)]
        let wait = self.sh.connect_wait;
        let opts = ConnectOptions {
            tmux,
            socket: self.sh.link.socket.args().to_vec(),
            env: self.sh.link.env.clone(),
            wait,
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
            // A slow server is tried again, as one that turned the link away is.
            Err(ConnectError::TimedOut) => {
                tracing::info!("the terminal server didn't answer the link in time");
                self.back_off(tried);
            }
            Err(e @ ConnectError::Protocol(_)) => {
                tracing::warn!("the terminal link couldn't connect: {e}");
                self.down(Down::CantLink);
            }
        }
    }

    /// Sources moa's config, stops a stuck client from before, names the client in
    /// `@moa_ctl` and sets the hooks, then tells the core it is up. `tried` is the backoff
    /// step that connected it, if one did. A step that takes longer than `STEP_WAIT` counts
    /// as a loss.
    async fn bootstrap(
        &mut self,
        client: ControlClient,
        events: Events,
        info: ServerInfo,
        tried: Option<usize>,
    ) {
        let conn = self.sh.conns.fetch_add(1, Ordering::SeqCst) + 1;
        let moa_tmux = self.sh.link.moa_tmux.to_string_lossy().into_owned();
        let source = Cmd::new("source-file").arg(moa_tmux);
        let name = Cmd::new("set-option").args(["-g", "@moa_ctl", info.client.0.as_str()]);
        let linked = async {
            linking(&client, &source).await?;
            reap_stale(&client, &info.client).await?;
            linking(&client, &name).await
        };
        // Only a loss matters here; a refusal was logged.
        if linked.await.is_err() {
            client.close().await;
            return self.lost(tried).await;
        }
        client.send(hooks(&info.client), None);
        let up = Up {
            conn,
            client,
            events,
            info,
            since: Instant::now(),
            tried,
            tokens: BTreeMap::new(),
            last_tag: 0,
        };
        self.tell(up.connected(&self.sh.link.socket));
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
        // A panic in the probe still reconnects: the core is waiting for one.
        self.mark(Doing::Connecting);
        if !self.probe().await {
            return self.down(Down::NoServer);
        }
        self.back_off(after);
    }

    async fn retry(&mut self, next: usize) {
        self.state = State::Idle;
        self.connect(Some(next)).await;
    }

    fn send(&mut self, conn: u64, cmds: Vec<Cmd>, token: Option<TmuxToken>) {
        match &mut self.state {
            State::Up(up) if up.conn == conn => {
                let tag = token.map(|t| up.tag(t));
                up.client.send(cmds, tag);
            }
            // The commands stay out of the log.
            _ => tracing::debug!(conn, cmds = cmds.len(), "not on that connection: dropped"),
        }
    }

    fn event(&mut self, ev: Event) {
        let State::Up(up) = &mut self.state else {
            return;
        };
        let conn = up.conn;
        match ev {
            // The end of the events says it.
            Event::Notification(Notification::Exit) => {}
            Event::Notification(n) => self.tell(TmuxIn::Notification(n)),
            Event::Replies { tag, results } => match up.tokens.remove(&tag) {
                Some(token) => self.tell(TmuxIn::Replies {
                    conn,
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
    /// client. A command that takes longer than `STEP_WAIT` ends it there.
    async fn clean_up(&mut self) {
        let State::Up(up) = self.set(State::Idle) else {
            return;
        };
        let unset = Cmd::new("set-option").args(["-gu", "@moa_ctl"]);
        for cmd in unhooks().iter().chain([&unset]) {
            match run_bounded(&up.client, cmd).await {
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

/// Runs `cmd`, taking a reply slower than `STEP_WAIT` as a gone client.
async fn run_bounded(client: &ControlClient, cmd: &Cmd) -> Result<Vec<String>, CmdError> {
    tokio::time::timeout(STEP_WAIT, client.run(cmd))
        .await
        .unwrap_or(Err(CmdError::Gone))
}

/// One step of a connect's bootstrap: its output, or `None` when tmux refused it, which is
/// logged and the link goes on. `Err` when the client has gone, or the step took longer
/// than `STEP_WAIT`.
async fn linking(client: &ControlClient, cmd: &Cmd) -> Result<Option<Vec<String>>, CmdError> {
    match run_bounded(client, cmd).await {
        Ok(lines) => Ok(Some(lines)),
        Err(CmdError::Gone) => Err(CmdError::Gone),
        Err(e) => {
            tracing::warn!(
                "{} failed while linking: {}",
                cmd.name(),
                first_line(&e.to_string())
            );
            Ok(None)
        }
    }
}

/// Stops the control client that `@moa_ctl` still names, when it isn't `ours` and is still
/// attached: an earlier daemon's link that tmux kept (tmux 3.4 keeps a control client whose
/// output it couldn't write, and 3.8 one with many unread replies). Left attached, it keeps
/// the keys from saying moa isn't running, and `kill-server` waits on it. Its commands are
/// bootstrap steps (`linking`): a refusal is logged and leaves nothing to reap, and a
/// timeout or a gone client is `Err`, a loss.
async fn reap_stale(client: &ControlClient, ours: &ClientName) -> Result<(), CmdError> {
    let show = Cmd::new("show-options").args(["-gqv", "@moa_ctl"]);
    let shown = linking(client, &show).await?.unwrap_or_default();
    let old = shown.first().map(String::as_str).unwrap_or("");
    if old.is_empty() || old == ours.0 {
        return Ok(());
    }
    let format = "#{client_control_mode} #{client_pid} #{client_name}";
    let list = Cmd::new("list-clients").args(["-F", format]);
    let clients = linking(client, &list).await?.unwrap_or_default();
    for line in &clients {
        let mut f = line.splitn(3, ' ');
        let (Some("1"), Some(pid), Some(name)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if name == old
            && let Ok(pid) = pid.parse::<u32>()
        {
            moa_platform::kill_term(pid);
            tracing::info!(pid, "a stuck terminal link from before was stopped");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tests;
