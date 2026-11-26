//! The client library behind `moactl` and the console: connect to a brain's daemon, start
//! it when it isn't running, and speak the line protocol.
//!
//! The client never touches `lock(b)` (the daemon's lock): it connects, and when nothing
//! answers it starts a daemon under the *start* lock and lets the daemon's own lock decide
//! who runs. A client's probe of `lock(b)` could make a daemon that launchd or another
//! client is starting meanwhile report "already running".

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use moa_core::config::BrainName;
use moa_core::op::{Done, Op};
use moa_core::state::CallerInfo;
use moa_platform::{Paths, PathsError, Spawner, ensure_private_dir, pid_alive};
use moa_proto::{
    Hello, HelloOk, Id, MAX_LINE, RpcError, ServerLine, SnapshotMsg, StatusOk, Subscribe, Topic,
    UpgradeOk, UpgradeParams, code, decode_server_line,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

mod live;
pub use live::*;

/// How long a started daemon has to say `ready`.
const READY_WAIT: Duration = Duration::from_secs(10);
/// How long a daemon told to stop has to exit.
const EXIT_WAIT: Duration = Duration::from_secs(10);
const EXIT_POLL: Duration = Duration::from_millis(50);
/// How long to retry connecting to a daemon another starter holds the brain for: it may not
/// have bound its socket yet.
const BIND_WAIT: Duration = Duration::from_secs(2);
/// How long to read to the end of a connection after `shutdown`.
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// How long a client waits for a daemon to upgrade in place: moa may be finishing a turn.
pub const UPGRADE_WAIT: Duration = Duration::from_secs(30);

/// What to do when the daemon is a different build from this client.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnMismatch {
    /// Follow the upgrade policy (L1): a daemon whose program is now this build upgrades in
    /// place; one run by another install is left running and refused; one from before live
    /// updates is stopped and a fresh one started, once (a daemon that still doesn't match
    /// is an error).
    Restart,
    /// Keep it; the caller sees `hello().matched == false`.
    Accept,
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub paths: Paths,
    pub brain: BrainName,
    /// Who is saying hello (`moactl`, `console`).
    pub client: &'static str,
    pub caller: CallerInfo,
    /// The `moa` executable to start when no daemon answers; `None` never starts one.
    /// Pass [`own_exe`]: the path that was run, so the daemon follows it (L2), and never a
    /// `moactl` link, which isn't a daemon.
    pub autostart: Option<PathBuf>,
    pub on_mismatch: OnMismatch,
    /// This client's build id (`moa_platform::build_id()`), compared with the daemon's.
    pub build: String,
    /// How long `Restart` waits for a daemon upgrading in place ([`UPGRADE_WAIT`]).
    pub upgrade_wait: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the {0} brain's moa isn't running")]
    NotRunning(BrainName),
    /// What the daemon said when it couldn't start, with where its log is.
    #[error("couldn't start moa: {0}")]
    Start(String),
    /// The root folder doesn't exist: starting a daemon would bring it back, with a daemon
    /// in it that no one stops (L15).
    #[error("moa's folder {} is gone", .0.display())]
    RootGone(PathBuf),
    #[error("{0}")]
    Paths(#[from] PathsError),
    #[error("{}", .0.message)]
    Rpc(RpcError),
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The connection ended while nothing was awaited.
    #[error("moa closed the connection")]
    Closed,
    /// The connection ended while a reply was awaited: the client can't know whether the
    /// operation ran. `wrote` is set when the request was a write: it may be saved already.
    #[error("{}", stopped_line(*.wrote))]
    Stopped { wrote: bool },
    /// The daemon's upgrade still waits for moa's turn to end (L4).
    #[error("moa is finishing a turn; it upgrades right after. Try again in a moment.")]
    StillInTurn,
    /// The daemon's upgrade didn't finish in the time given, though moa wasn't in a turn.
    #[error("moa didn't finish upgrading within {} s", .0.as_secs())]
    UpgradeUnfinished(Duration),
    /// The daemon stopped during its upgrade: its next build didn't start (L7).
    #[error("moa stopped while upgrading; run the command again to start it")]
    UpgradeStopped,
}

/// How a daemon came back from an upgrade ([`Client::await_upgrade`]), connected.
pub enum Upgraded {
    /// The same process, on the build its program has now.
    InPlace(Client),
    /// Another process answered: the daemon restarted rather than upgraded in place.
    Restarted(Client),
}

fn stopped_line(wrote: bool) -> &'static str {
    if wrote {
        "moa stopped before answering; it may have saved that, so check before trying again"
    } else {
        "moa stopped before answering; try again"
    }
}

/// The first line a daemon started with `--notify-ready` prints.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReadyLine {
    Ready,
    /// Another daemon holds the brain (or this one died silently): connect to that one.
    TryAgain,
    Failed(String),
}

/// `ready`; `already running` or no line at all; `error: <why>`.
pub fn read_ready(first_line: Option<&str>) -> ReadyLine {
    match first_line.map(str::trim_end) {
        Some("ready") => ReadyLine::Ready,
        Some("already running") | None => ReadyLine::TryAgain,
        Some(line) => ReadyLine::Failed(line.strip_prefix("error: ").unwrap_or(line).to_string()),
    }
}

/// The environment a started daemon gets (D17). `HOME` (and `MOA_ROOT` when sandboxed) come
/// from `paths`, and so does `TMPDIR`, so both sides compute the same fallback socket;
/// none is copied from the parent. Of the parent's, only what the daemon needs survives:
/// `PATH`, `USER`, `LANG`, `LC_*`, `MOA_LOG`, `TMUX_TMPDIR` (so both sides find the same
/// tmux socket), `TZDIR` (where the zone database is, when it isn't in the usual place, as
/// on NixOS); `TZ` only when sandboxed (a real home has the system zone);
/// `MOA_REAL_HOME` only when not sandboxed; `MOA_AGENT_HOME` (the parent's real `HOME`,
/// when it is a full path other than the sandbox) only when sandboxed, for moa's agent's login.
pub fn daemon_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
    paths: &Paths,
) -> Vec<(OsString, OsString)> {
    let sandboxed = paths.is_sandboxed();
    let mut env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), paths.home().into()),
        ("TMPDIR".into(), paths.tmpdir().into()),
    ];
    if sandboxed {
        env.push(("MOA_ROOT".into(), paths.home().into()));
    }
    let mut real_home = None;
    env.extend(parent.filter(|(k, v)| match k.to_str() {
        Some("PATH" | "USER" | "LANG" | "MOA_LOG" | "TMUX_TMPDIR" | "TZDIR") => true,
        Some("TZ") => sandboxed,
        Some("MOA_REAL_HOME") => !sandboxed,
        Some("HOME") => {
            let home = Path::new(v);
            if sandboxed && home.is_absolute() && home != paths.home() {
                real_home = Some(v.clone());
            }
            false
        }
        Some(k) => k.starts_with("LC_"),
        None => false,
    }));
    env.extend(real_home.map(|h| ("MOA_AGENT_HOME".into(), h)));
    env
}

/// This moa's program, for [`ClientOptions::autostart`] (L2): the path it was run as, links
/// kept (`moa_platform::invoked_exe`), so a daemon started through a profile link follows
/// that link across upgrades. Run as `moactl`, it is the `moa` beside it when that is this
/// same program (`moactl daemon` isn't the daemon), else this program's real path.
pub fn own_exe() -> io::Result<PathBuf> {
    let real = || -> io::Result<PathBuf> { std::fs::canonicalize(std::env::current_exe()?) };
    let ran = moa_platform::invoked_exe();
    if !ran.is_absolute() {
        return real();
    }
    if ran.file_name().is_some_and(|n| n == "moactl") {
        let moa = ran.with_file_name("moa");
        let real = real()?;
        return Ok(match std::fs::canonicalize(&moa) {
            Ok(m) if m == real => moa,
            _ => real,
        });
    }
    Ok(ran)
}

/// Whether an I/O error means the other end has gone.
fn is_gone(e: &io::Error) -> bool {
    use io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        e.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
    )
}

fn invalid(what: impl std::fmt::Display) -> ClientError {
    ClientError::Io(io::Error::new(io::ErrorKind::InvalidData, what.to_string()))
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ClientError> {
    serde_json::from_value(value)
        .map_err(|e| invalid(format!("moa sent a reply this client can't read: {e}")))
}

/// What a failed write means: a gone daemon never got the request.
fn write_error(e: LinesCodecError) -> ClientError {
    match e {
        LinesCodecError::Io(e) if is_gone(&e) => ClientError::Stopped { wrote: false },
        LinesCodecError::Io(e) => e.into(),
        e => invalid(e),
    }
}

/// A line read from the connection, or `None` when it has ended.
fn server_line(
    next: Option<Result<String, LinesCodecError>>,
) -> Result<Option<ServerLine>, ClientError> {
    match next {
        None => Ok(None),
        Some(Ok(line)) => decode_server_line(&line).map(Some).map_err(|e| {
            invalid(format!(
                "moa sent something that isn't a reply: {}",
                e.message
            ))
        }),
        Some(Err(LinesCodecError::Io(e))) if is_gone(&e) => Ok(None),
        Some(Err(LinesCodecError::Io(e))) => Err(e.into()),
        Some(Err(e)) => Err(invalid(e)),
    }
}

/// The line protocol over one connection.
struct Wire {
    framed: Framed<UnixStream, LinesCodec>,
    next: i64,
    /// Notices read while a reply was awaited, for `next_notice`.
    notices: VecDeque<(String, Value)>,
}

impl Wire {
    fn new(stream: UnixStream) -> Wire {
        Wire {
            framed: Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE)),
            next: 0,
            notices: VecDeque::new(),
        }
    }

    /// A request's line, with the next id.
    fn request_line(&mut self, method: &str, params: Value) -> (Id, String) {
        self.next += 1;
        let id = Id::Num(self.next);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        (id, line.to_string())
    }

    /// An op's request line, with the next id.
    fn op_line(&mut self, op: &Op) -> Result<(Id, String), ClientError> {
        let mut wire = serde_json::to_value(op).map_err(io::Error::other)?;
        let method = wire["method"].as_str().unwrap_or_default().to_string();
        let params = wire["params"].take();
        Ok(self.request_line(&method, params))
    }

    /// Writes a request and returns its id.
    async fn send(&mut self, method: &str, params: Value) -> Result<Id, ClientError> {
        let (id, line) = self.request_line(method, params);
        self.write(id, line).await
    }

    /// Writes an op as a request and returns its id.
    async fn send_op(&mut self, op: &Op) -> Result<Id, ClientError> {
        let (id, line) = self.op_line(op)?;
        self.write(id, line).await
    }

    /// Writes a request's line and hands back its id.
    async fn write(&mut self, id: Id, line: String) -> Result<Id, ClientError> {
        self.framed.send(line).await.map_err(write_error)?;
        Ok(id)
    }

    /// The next line, or `None` when the connection has ended.
    async fn read(&mut self) -> Result<Option<ServerLine>, ClientError> {
        server_line(self.framed.next().await)
    }

    /// Sends a request and waits for the reply that carries its id; notices that arrive
    /// meanwhile are kept for `next_notice`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.send(method, params).await?;
        self.reply(id).await
    }

    /// Waits for the reply that carries `id`, keeping the notices that arrive meanwhile.
    async fn reply(&mut self, id: Id) -> Result<Value, ClientError> {
        loop {
            match self.read().await? {
                None => return Err(ClientError::Stopped { wrote: false }),
                Some(ServerLine::Result { id: got, value }) if got == id => return Ok(value),
                Some(ServerLine::Error { id: got, error })
                    if got.as_ref().is_none_or(|g| *g == id) =>
                {
                    return Err(ClientError::Rpc(error));
                }
                Some(ServerLine::Notice { method, params }) => {
                    self.notices.push_back((method, params));
                }
                Some(_) => {}
            }
        }
    }

    /// Sends `shutdown` and reads to the end of the connection: the daemon closes it after
    /// replying, so an end of stream is the expected answer. An error line is the daemon
    /// refusing; a connection that stays open is an error too.
    async fn stop(&mut self) -> Result<(), ClientError> {
        match self.send("shutdown", json!({})).await {
            Ok(_) | Err(ClientError::Stopped { .. }) => {}
            Err(e) => return Err(e),
        }
        let mut refusal = None;
        let to_the_end = async {
            loop {
                match self.read().await {
                    Ok(Some(ServerLine::Error { error, .. })) => refusal = Some(error),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        };
        if timeout(CLOSE_WAIT, to_the_end).await.is_err() {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "moa didn't hang up within 5 s",
            )));
        }
        refusal.map_or(Ok(()), |e| Err(ClientError::Rpc(e)))
    }
}

/// Waits until `pid` has exited, for up to `limit`. Whether it has.
async fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while pid_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        sleep(EXIT_POLL).await;
    }
    true
}

pub struct Client {
    wire: Wire,
    hello: HelloOk,
    /// How it connected: what reaching the daemon again after an upgrade takes.
    opts: ClientOptions,
}

impl Client {
    /// Connects to the brain's daemon, starting it when `autostart` is set and nothing is
    /// running (D1, D2), and says hello.
    pub async fn connect(opts: ClientOptions) -> Result<Client, ClientError> {
        let mut restarted = false;
        loop {
            let stream = connect_or_start(&opts).await?;
            let client = Client::greet(stream, &opts).await?;
            if client.hello.matched || opts.on_mismatch == OnMismatch::Accept {
                return Ok(client);
            }
            // Only tests reach this: every caller that restarts passes `autostart`.
            if opts.autostart.is_none() {
                // Stopping a daemon this client can't start again would just take moa down.
                return Err(ClientError::Rpc(RpcError {
                    code: code::BUILD_MISMATCH,
                    message: "moa is running a different build; stop it with: moa shutdown".into(),
                }));
            }
            if restarted {
                return Err(ClientError::Rpc(RpcError {
                    code: code::BUILD_MISMATCH,
                    message: "moa was restarted and is still a different build".into(),
                }));
            }
            match client.follow_policy(&opts).await? {
                Some(client) => return Ok(client),
                // A daemon from before live updates stopped: start this build's, once.
                None => restarted = true,
            }
        }
    }

    /// L1, for a daemon on another build: asks it to upgrade if its program is no longer
    /// the build it runs, and waits for it to come back on this build. A daemon run by
    /// another install (it isn't stale, or came back on another build) is left running and
    /// refused. `None` once a daemon from before live updates has stopped for a restart.
    async fn follow_policy(mut self, opts: &ClientOptions) -> Result<Option<Client>, ClientError> {
        let Some(ok) = self.ask_upgrade(true).await? else {
            self.stop_for_restart(opts).await?;
            return Ok(None);
        };
        if !ok.upgrading {
            let exe = ok.exe.or_else(|| self.hello.exe.clone());
            return Err(another_install(opts, exe));
        }
        let (Upgraded::InPlace(back) | Upgraded::Restarted(back)) = self
            .await_upgrade(ok.waiting_for_moa, opts.upgrade_wait)
            .await?;
        if back.hello.build != opts.build {
            return Err(another_install(opts, back.hello.exe.clone()));
        }
        Ok(Some(back))
    }

    async fn greet(stream: UnixStream, opts: &ClientOptions) -> Result<Client, ClientError> {
        let mut wire = Wire::new(stream);
        let hello = Hello {
            client: opts.client.to_string(),
            build: opts.build.clone(),
            caller: opts.caller.clone(),
        };
        let params = serde_json::to_value(&hello).map_err(io::Error::other)?;
        let hello = decode(wire.request("hello", params).await?)?;
        Ok(Client {
            wire,
            hello,
            opts: opts.clone(),
        })
    }

    /// Asks a daemon from before live updates to stop for a restart and waits until its
    /// process is gone: a `shutdown`, which every daemon takes.
    async fn stop_for_restart(mut self, opts: &ClientOptions) -> Result<(), ClientError> {
        let pid = self.hello.pid;
        self.wire.stop().await?;
        drop(self);
        if !wait_for_exit(pid, EXIT_WAIT).await {
            return Err(start_error(
                &opts.paths,
                &opts.brain,
                "the old moa didn't stop within 10 s",
            ));
        }
        Ok(())
    }

    pub fn hello(&self) -> &HelloOk {
        &self.hello
    }

    /// Asks the daemon to upgrade in place (L1): always, or, with `if_stale`, only when its
    /// program is no longer the build it runs. `None` from a daemon from before live
    /// updates: a bare `upgrade` (`if_stale` false, sent as `{}`) is a stop to it, and it is
    /// stopping; one with `if_stale` it refuses (`INVALID_PARAMS`), and it still runs.
    pub async fn ask_upgrade(&mut self, if_stale: bool) -> Result<Option<UpgradeOk>, ClientError> {
        let params = serde_json::to_value(UpgradeParams { if_stale }).map_err(io::Error::other)?;
        match self.wire.request("upgrade", params).await {
            Ok(reply) => Ok(serde_json::from_value(reply).ok()),
            Err(ClientError::Rpc(e)) if e.code == code::INVALID_PARAMS => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// After the daemon said it is upgrading (`waiting_for_moa` as it said): waits, up to
    /// `limit` ([`UPGRADE_WAIT`]), for its next image, and says hello to it. The old image
    /// ends this connection once it has wound down, between moa's turns; the socket stays
    /// open across the upgrade, and a connection made after that waits in its backlog for
    /// the next image. Errors: [`ClientError::StillInTurn`] when moa's turn outlasts `limit`,
    /// [`ClientError::UpgradeUnfinished`] when the upgrade does without one, and
    /// [`ClientError::UpgradeStopped`] when no next image came.
    pub async fn await_upgrade(
        self,
        waiting_for_moa: bool,
        limit: Duration,
    ) -> Result<Upgraded, ClientError> {
        let deadline = Instant::now() + limit;
        let pid = self.hello.pid;
        let Client { mut wire, opts, .. } = self;
        let ended = timeout_at(deadline, async {
            while let Ok(Some(_)) = wire.read().await {}
        })
        .await;
        if ended.is_err() {
            return Err(match waiting_for_moa {
                true => ClientError::StillInTurn,
                false => ClientError::UpgradeUnfinished(limit),
            });
        }
        drop(wire);
        let sock = opts.paths.socket(opts.brain.as_str())?;
        let left = deadline.saturating_duration_since(Instant::now());
        // Nothing listening (for a moment, in case another starter raced in) is a daemon
        // that stopped.
        let stream = match connect_soon(&sock, left.min(BIND_WAIT)).await {
            Ok(stream) => stream,
            Err(e) if is_absent(&e) => return Err(ClientError::UpgradeStopped),
            Err(e) => return Err(e.into()),
        };
        let client = match timeout_at(deadline, Client::greet(stream, &opts)).await {
            Ok(Ok(client)) => client,
            // The process went with the connection waiting for it.
            Ok(Err(ClientError::Stopped { .. })) => return Err(ClientError::UpgradeStopped),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ClientError::UpgradeUnfinished(limit)),
        };
        Ok(if client.hello.pid == pid {
            Upgraded::InPlace(client)
        } else {
            Upgraded::Restarted(client)
        })
    }

    pub async fn call(&mut self, op: Op) -> Result<Done, ClientError> {
        // A send that failed never reached the daemon; only a missing reply leaves a write
        // in doubt.
        let id = self.wire.send_op(&op).await?;
        let reply = match self.wire.reply(id).await {
            Err(ClientError::Stopped { .. }) => {
                return Err(ClientError::Stopped { wrote: op.writes() });
            }
            reply => reply?,
        };
        decode(reply)
    }

    pub async fn status(&mut self) -> Result<StatusOk, ClientError> {
        decode(self.wire.request("status", json!({})).await?)
    }

    /// Asks the daemon to stop and returns once its process has exited. An error when the
    /// daemon refuses or is still running after 10 s.
    pub async fn shutdown(&mut self) -> Result<(), ClientError> {
        self.shutdown_within(EXIT_WAIT).await
    }

    async fn shutdown_within(&mut self, limit: Duration) -> Result<(), ClientError> {
        let pid = self.hello.pid;
        self.wire.stop().await?;
        if wait_for_exit(pid, limit).await {
            Ok(())
        } else {
            Err(ClientError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "moa didn't stop within 10 s",
            )))
        }
    }

    /// Follows `topics`: the full snapshot now, then `patch`, `snapshot` and `event` notices
    /// through [`Client::next_notice`].
    pub async fn subscribe(&mut self, topics: &[Topic]) -> Result<SnapshotMsg, ClientError> {
        let params = Subscribe {
            topics: topics.to_vec(),
            since: None,
        };
        let params = serde_json::to_value(&params).map_err(io::Error::other)?;
        decode(self.wire.request("subscribe", params).await?)
    }

    /// The next notice: its method (`patch`, `snapshot`, `event`) and params.
    pub async fn next_notice(&mut self) -> Result<(String, Value), ClientError> {
        if let Some(n) = self.wire.notices.pop_front() {
            return Ok(n);
        }
        loop {
            match self.wire.read().await? {
                None => return Err(ClientError::Closed),
                Some(ServerLine::Notice { method, params }) => return Ok((method, params)),
                Some(_) => {}
            }
        }
    }
}

/// L1's refusal: the brain's daemon runs `daemon_exe`, another install than this client's.
fn another_install(opts: &ClientOptions, daemon_exe: Option<String>) -> ClientError {
    let theirs = daemon_exe.unwrap_or_else(|| "another moa".into());
    let ours = opts
        .autostart
        .clone()
        .unwrap_or_else(moa_platform::invoked_exe);
    ClientError::Rpc(RpcError {
        code: code::BUILD_MISMATCH,
        message: format!(
            "moa: brain '{}' is run by {theirs}; this moa is {}. Use that one, or stop the \
             daemon first: moa shutdown",
            opts.brain.as_str(),
            ours.display()
        ),
    })
}

fn start_error(paths: &Paths, brain: &BrainName, line: &str) -> ClientError {
    let log = paths.stderr_log(brain.as_str());
    ClientError::Start(format!("{line} (log: {})", log.display()))
}

fn is_absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// A connection to the daemon, which is started first when `opts` allow and none answers.
async fn connect_or_start(opts: &ClientOptions) -> Result<UnixStream, ClientError> {
    let sock = opts.paths.socket(opts.brain.as_str())?;
    match UnixStream::connect(&sock).await {
        Ok(s) => return Ok(s),
        Err(e) if is_absent(&e) => {}
        Err(e) => return Err(e.into()),
    }
    let Some(exe) = &opts.autostart else {
        return Err(ClientError::NotRunning(opts.brain.clone()));
    };
    start(opts, exe, &sock).await
}

/// Starts the daemon under the start lock, so clients starting at once launch one.
async fn start(opts: &ClientOptions, exe: &Path, sock: &Path) -> Result<UnixStream, ClientError> {
    let (paths, brain) = (&opts.paths, &opts.brain);
    let b = brain.as_str();
    paths.check_v1_allowed()?;
    // A user's root always exists; one that is gone was a test's, and making the run
    // folder would bring it back with a daemon no one stops.
    let gone = || ClientError::RootGone(paths.home().to_path_buf());
    let root = root_id(paths.home()).ok_or_else(gone)?;
    ensure_private_dir(&paths.run_dir())?;
    ensure_private_dir(&paths.log_dir())?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.start_lock(b))?;
    // `lock` blocks while another client is starting the daemon.
    let _lock = tokio::task::spawn_blocking(move || lock.lock().map(|()| lock))
        .await
        .map_err(io::Error::other)??;
    // The root may have gone while we waited (a test's teardown holds the start lock while
    // it removes it), or gone and come back by our own `ensure_private_dir`: either way the
    // lock we hold is on a removed file, and a daemon started now would be no one's.
    if root_id(paths.home()) != Some(root) {
        return Err(gone());
    }

    // Whoever held the lock before us may have started it.
    match UnixStream::connect(sock).await {
        Ok(s) => return Ok(s),
        Err(e) if is_absent(&e) => {}
        Err(e) => return Err(e.into()),
    }

    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(paths.stderr_log(b))?;
    let mut cmd = Command::new(exe);
    // The daemon runs, watches and upgrades into the path it was started from (L2).
    cmd.args(["daemon", "--brain", b, "--notify-ready"])
        .env_clear()
        .envs(daemon_env(std::env::vars_os(), paths))
        .env("MOA_EXE", exe)
        .current_dir(paths.state_dir())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    let mut child = Spawner
        .spawn(&mut cmd)
        .map_err(|e| start_error(paths, brain, &format!("{}: {e}", exe.display())))?;
    let stdout = child
        .stdout
        .take()
        .map(tokio::process::ChildStdout::from_std);
    // The daemon outlives this call (and maybe this process); reaping it keeps a zombie
    // from looking alive to `pid_alive` here.
    std::thread::spawn(move || drop(child.wait()));

    let mut first = String::new();
    if let Some(stdout) = stdout.transpose()? {
        match timeout(READY_WAIT, BufReader::new(stdout).read_line(&mut first)).await {
            Ok(_) => {}
            Err(_) => {
                return Err(start_error(
                    paths,
                    brain,
                    "it didn't say it was ready within 10 s",
                ));
            }
        }
    }
    let first = (!first.is_empty()).then_some(first.as_str());
    match read_ready(first) {
        ReadyLine::Failed(line) => Err(start_error(paths, brain, &line)),
        ReadyLine::Ready => Ok(UnixStream::connect(sock).await?),
        // Another daemon holds the brain; it may still be binding its socket.
        ReadyLine::TryAgain => connect_soon(sock, BIND_WAIT)
            .await
            .map_err(|e| start_error(paths, brain, &format!("it didn't answer: {e}"))),
    }
}

/// The root folder's identity (device and inode), or `None` when it isn't a folder: a root
/// removed and made again has a new one.
fn root_id(root: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(root)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|m| (m.dev(), m.ino()))
}

/// Connects, retrying while nothing listens yet, for up to `limit`.
async fn connect_soon(sock: &Path, limit: Duration) -> io::Result<UnixStream> {
    let deadline = Instant::now() + limit;
    loop {
        match UnixStream::connect(sock).await {
            Err(e) if is_absent(&e) && Instant::now() < deadline => sleep(EXIT_POLL).await,
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use moa_platform::Paths;
    use moa_proto::{Incoming, encode_result};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn the_ready_line() {
        assert_eq!(read_ready(Some("ready")), ReadyLine::Ready);
        assert_eq!(read_ready(Some("already running")), ReadyLine::TryAgain);
        assert_eq!(read_ready(None), ReadyLine::TryAgain);
        assert_eq!(
            read_ready(Some("error: moa.toml: bad")),
            ReadyLine::Failed("moa.toml: bad".into())
        );
    }

    fn parent() -> Vec<(OsString, OsString)> {
        [
            ("MOA_SESSION", "7"),
            ("CLAUDE_CONFIG_DIR", "/x"),
            ("TMUX", "/t,1,0"),
            ("TMUX_PANE", "%1"),
            ("TZ", "UTC"),
            ("MOA_REAL_HOME", "1"),
            ("HOME", "/real"),
            ("MOA_ROOT", "/elsewhere"),
            ("TMPDIR", "/parent/tmp"),
            ("PATH", "/bin"),
            ("LC_ALL", "C"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    fn sorted(env: Vec<(OsString, OsString)>) -> Vec<String> {
        let mut v: Vec<String> = env
            .into_iter()
            .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn the_daemon_gets_a_clean_env() {
        let sandboxed = Paths::with_root(Path::new("/r"));
        assert_eq!(
            sorted(daemon_env(parent().into_iter(), &sandboxed)),
            [
                "HOME=/r",
                "LC_ALL=C",
                "MOA_AGENT_HOME=/real",
                "MOA_ROOT=/r",
                "PATH=/bin",
                "TMPDIR=/tmp",
                "TZ=UTC"
            ]
        );
        let real = Paths::from_vars(None, Some("/home/u".into()), None, 501, true).unwrap();
        assert_eq!(
            sorted(daemon_env(parent().into_iter(), &real)),
            [
                "HOME=/home/u",
                "LC_ALL=C",
                "MOA_REAL_HOME=1",
                "PATH=/bin",
                "TMPDIR=/tmp"
            ]
        );
    }

    /// A sandbox's agent gets the real home (when it is a full path other than the root);
    /// nothing else does.
    #[test]
    fn daemon_env_passes_the_real_home_only_to_a_sandbox() {
        let with_home = |home: &str| {
            let mut p = parent();
            p.retain(|(k, _)| k != "HOME");
            p.push(("HOME".into(), home.into()));
            p.into_iter()
        };
        let has = |env: &[String]| env.iter().any(|e| e.starts_with("MOA_AGENT_HOME="));
        let sandboxed = Paths::with_root(Path::new("/r"));
        let env = sorted(daemon_env(with_home("/real"), &sandboxed));
        assert!(env.contains(&"MOA_AGENT_HOME=/real".to_string()), "{env:?}");
        assert!(env.contains(&"HOME=/r".to_string()), "{env:?}");
        assert!(!has(&sorted(daemon_env(with_home("/r"), &sandboxed))));
        assert!(!has(&sorted(daemon_env(with_home("rel"), &sandboxed))));
        let real = Paths::from_vars(None, Some("/home/u".into()), None, 501, true).unwrap();
        assert!(!has(&sorted(daemon_env(with_home("/real"), &real))));
    }

    /// T20: with `-L` names, the daemon and the front door must find the same tmux socket.
    #[test]
    fn daemon_env_passes_tmux_tmpdir() {
        let parent = || {
            let mut p = parent();
            p.push(("TMUX_TMPDIR".into(), "/run/t".into()));
            p.into_iter()
        };
        for paths in [
            Paths::with_root(Path::new("/r")),
            Paths::from_vars(None, Some("/home/u".into()), None, 501, true).unwrap(),
        ] {
            let env = sorted(daemon_env(parent(), &paths));
            assert!(env.contains(&"TMUX_TMPDIR=/run/t".to_string()), "{env:?}");
        }
    }

    /// Where the zone database is (NixOS sets it), so the daemon finds zones by name.
    #[test]
    fn daemon_env_passes_tzdir() {
        let parent = || {
            let mut p = parent();
            p.push(("TZDIR".into(), "/etc/zoneinfo".into()));
            p.into_iter()
        };
        for paths in [
            Paths::with_root(Path::new("/r")),
            Paths::from_vars(None, Some("/home/u".into()), None, 501, true).unwrap(),
        ] {
            let env = sorted(daemon_env(parent(), &paths));
            assert!(env.contains(&"TZDIR=/etc/zoneinfo".to_string()), "{env:?}");
        }
    }

    /// A daemon that answers `hello` (with `pid`), reads `then_read` more lines, sends
    /// `reply` if given, and hangs up.
    struct Fake {
        _dir: tempfile::TempDir,
        opts: ClientOptions,
    }

    fn fake(then_read: usize) -> Fake {
        fake_with(then_read, dead_pid(), None)
    }

    /// The pid of a process that has exited (and been reaped).
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn fake_with(then_read: usize, pid: u32, reply: Option<String>) -> Fake {
        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = Paths::with_root(dir.path());
        ensure_private_dir(&paths.run_dir()).unwrap();
        let listener = UnixListener::bind(paths.socket("default").unwrap()).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(stream, LinesCodec::new());
            let line = framed.next().await.unwrap().unwrap();
            let Ok(Incoming::Request { id, .. }) = moa_proto::decode_incoming(&line) else {
                panic!("not a request: {line}");
            };
            let ok = HelloOk {
                brain: "default".into(),
                build: "b".into(),
                rev: 0,
                matched: true,
                pid,
                exe: None,
            };
            framed.send(encode_result(&id, &ok)).await.unwrap();
            for _ in 0..then_read {
                framed.next().await;
            }
            if let Some(reply) = reply {
                framed.send(reply).await.unwrap();
            }
        });
        let opts = ClientOptions {
            paths,
            brain: BrainName::default_brain(),
            client: "test",
            caller: CallerInfo::default(),
            autostart: None,
            on_mismatch: OnMismatch::Accept,
            build: "b".into(),
            upgrade_wait: UPGRADE_WAIT,
        };
        Fake { _dir: dir, opts }
    }

    #[tokio::test]
    async fn a_reply_that_never_comes_is_stopped() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.call(Op::SessionLs { all: false }).await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped { .. }), "{err}");
        assert_eq!(err.to_string(), "moa stopped before answering; try again");
    }

    #[tokio::test]
    async fn a_write_that_got_no_reply_may_have_been_saved() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let op = Op::NoteAdd {
            text: "n".into(),
            project: None,
            kind: None,
        };
        assert!(op.writes());
        let err = client.call(op).await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped { wrote: true }), "{err}");
        assert_eq!(
            err.to_string(),
            "moa stopped before answering; it may have saved that, so check before trying again"
        );
    }

    #[tokio::test]
    async fn status_and_subscribe_also_say_stopped() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.status().await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped { .. }), "{err}");
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.subscribe(&[Topic::Sessions]).await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped { .. }), "{err}");
    }

    #[tokio::test]
    async fn the_end_of_the_connection_with_nothing_awaited_is_closed() {
        let f = fake(0);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.next_notice().await.unwrap_err();
        assert!(matches!(err, ClientError::Closed), "{err}");
        assert_eq!(err.to_string(), "moa closed the connection");
    }

    #[tokio::test]
    async fn shutdown_ending_the_connection_with_the_process_gone_is_ok() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        client.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_fails_when_the_process_stays_alive() {
        // This test process is alive for as long as the test runs.
        let f = fake_with(1, std::process::id(), None);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client
            .shutdown_within(Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Io(_)), "{err}");
        assert_eq!(err.to_string(), "moa didn't stop within 10 s");
    }

    #[tokio::test]
    async fn shutdown_fails_when_the_daemon_answers_with_an_error() {
        let refusal = moa_proto::encode_error(
            Some(&Id::Num(2)),
            &RpcError {
                code: code::FAILED,
                message: "no, not now".into(),
            },
        );
        let f = fake_with(1, dead_pid(), Some(refusal));
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.shutdown().await.unwrap_err();
        assert!(matches!(err, ClientError::Rpc(_)), "{err}");
        assert_eq!(err.to_string(), "no, not now");
    }

    /// A root with a socket listening for the default brain, and options for it.
    fn listening() -> (tempfile::TempDir, ClientOptions, UnixListener) {
        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = Paths::with_root(dir.path());
        ensure_private_dir(&paths.run_dir()).unwrap();
        let listener = UnixListener::bind(paths.socket("default").unwrap()).unwrap();
        let opts = ClientOptions {
            paths,
            brain: BrainName::default_brain(),
            client: "test",
            caller: CallerInfo::default(),
            autostart: None,
            on_mismatch: OnMismatch::Accept,
            build: "b".into(),
            upgrade_wait: UPGRADE_WAIT,
        };
        (dir, opts, listener)
    }

    type Conn = Framed<tokio::net::UnixStream, LinesCodec>;

    /// The next connection, its `hello` answered as process `pid` on build `build`.
    async fn greeted(listener: &UnixListener, pid: u32, build: &str) -> Conn {
        let (stream, _) = listener.accept().await.unwrap();
        let mut conn = Framed::new(stream, LinesCodec::new());
        let (id, method, _) = request(&mut conn).await.expect("a hello");
        assert_eq!(method, "hello");
        let ok = HelloOk {
            brain: "default".into(),
            build: build.into(),
            rev: 0,
            matched: true,
            pid,
            exe: None,
        };
        conn.send(encode_result(&id, &ok)).await.unwrap();
        conn
    }

    /// The next request's id, method and params, read as a daemon of any build would.
    async fn request(conn: &mut Conn) -> Option<(Id, String, Value)> {
        let line = conn.next().await?.unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        let id = serde_json::from_value(v["id"].clone()).unwrap();
        let method = v["method"].as_str().unwrap().to_string();
        Some((id, method, v.get("params").cloned().unwrap_or(Value::Null)))
    }

    /// A daemon from before live updates: `upgrade`'s params are `NoParams`, which refuses a
    /// field it doesn't know (`INVALID_PARAMS`) and takes `{}` as a stop. Each upgrade's
    /// params and how they were taken go to `seen`.
    async fn old_daemon(listener: UnixListener, seen: mpsc::UnboundedSender<(Value, bool)>) {
        let mut conn = greeted(&listener, std::process::id(), "old").await;
        while let Some((id, method, params)) = request(&mut conn).await {
            assert_eq!(method, "upgrade");
            let taken = serde_json::from_value::<moa_core::op::NoParams>(params.clone()).is_ok();
            seen.send((params, taken)).unwrap();
            if !taken {
                let e = RpcError {
                    code: code::INVALID_PARAMS,
                    message: "the parameters for upgrade don't fit".into(),
                };
                conn.send(moa_proto::encode_error(Some(&id), &e))
                    .await
                    .unwrap();
                continue;
            }
            conn.send(encode_result(&id, &json!({}))).await.unwrap();
            return;
        }
    }

    /// Important 1: an old daemon refuses `if_stale` and still runs; it takes the bare
    /// `upgrade` (`{}`) as a stop. Both read as `None`.
    #[tokio::test]
    async fn an_old_daemon_reads_as_none_both_ways() {
        let (_dir, opts, listener) = listening();
        let (seen_tx, mut seen) = mpsc::unbounded_channel();
        tokio::spawn(old_daemon(listener, seen_tx));
        let mut client = Client::connect(opts).await.unwrap();

        assert!(client.ask_upgrade(true).await.unwrap().is_none());
        assert_eq!(
            seen.recv().await.unwrap(),
            (json!({"if_stale": true}), false),
            "refused: it still runs"
        );
        assert!(client.ask_upgrade(false).await.unwrap().is_none());
        assert_eq!(
            seen.recv().await.unwrap(),
            (json!({}), true),
            "a bare upgrade: it stops"
        );
    }

    /// A daemon that answers `upgrade` with `reply`, then keeps the connection open: its
    /// upgrade never finishes.
    async fn stuck_daemon(listener: UnixListener, reply: Value) {
        let mut conn = greeted(&listener, std::process::id(), "b").await;
        let (id, _, _) = request(&mut conn).await.unwrap();
        conn.send(encode_result(&id, &reply)).await.unwrap();
        while request(&mut conn).await.is_some() {}
    }

    /// Only an upgrade that waits for moa's turn says moa is finishing a turn; one that
    /// doesn't finish otherwise says so plainly.
    #[tokio::test]
    async fn still_in_turn_only_while_moa_is_in_a_turn() {
        for waiting in [true, false] {
            let (_dir, opts, listener) = listening();
            let reply = json!({"upgrading": true, "waiting_for_moa": waiting});
            tokio::spawn(stuck_daemon(listener, reply));
            let mut client = Client::connect(opts).await.unwrap();
            let ok = client.ask_upgrade(false).await.unwrap().unwrap();
            assert_eq!(ok.waiting_for_moa, waiting);
            let limit = Duration::from_millis(200);
            let err = match client.await_upgrade(waiting, limit).await {
                Err(e) => e,
                Ok(_) => panic!("it never finished"),
            };
            match waiting {
                true => assert!(matches!(err, ClientError::StillInTurn), "{err}"),
                false => assert!(matches!(err, ClientError::UpgradeUnfinished(_)), "{err}"),
            }
        }
    }

    /// A daemon that answers `upgrade` as upgrading, hangs up, and then: with `next`, comes
    /// back as process `next` on build `new`; without, is gone (its next build didn't start).
    async fn upgrading_daemon(listener: UnixListener, pid: u32, next: Option<u32>) {
        let mut conn = greeted(&listener, pid, "old").await;
        let (id, _, _) = request(&mut conn).await.unwrap();
        let reply = json!({"upgrading": true, "waiting_for_moa": false});
        conn.send(encode_result(&id, &reply)).await.unwrap();
        drop(conn);
        match next {
            Some(next) => {
                let mut conn = greeted(&listener, next, "new").await;
                while request(&mut conn).await.is_some() {}
            }
            None => drop(listener),
        }
    }

    async fn upgraded(pid: u32, next: Option<u32>) -> Result<Upgraded, ClientError> {
        let (_dir, opts, listener) = listening();
        tokio::spawn(upgrading_daemon(listener, pid, next));
        let mut client = Client::connect(opts).await.unwrap();
        assert!(client.ask_upgrade(false).await.unwrap().unwrap().upgrading);
        client.await_upgrade(false, Duration::from_secs(1)).await
    }

    /// M2: the same pid on the new build is an upgrade in place; another pid is a restart;
    /// nobody answering is a daemon that stopped while upgrading.
    #[tokio::test]
    async fn the_next_image_is_told_apart() {
        let me = std::process::id();
        match upgraded(me, Some(me)).await {
            Ok(Upgraded::InPlace(c)) => assert_eq!(c.hello().build, "new"),
            Ok(Upgraded::Restarted(_)) => panic!("the same pid restarted"),
            Err(e) => panic!("{e}"),
        }
        match upgraded(me, Some(dead_pid())).await {
            Ok(Upgraded::Restarted(c)) => assert_eq!(c.hello().build, "new"),
            Ok(Upgraded::InPlace(_)) => panic!("another pid upgraded in place"),
            Err(e) => panic!("{e}"),
        }
        match upgraded(me, None).await {
            Err(e @ ClientError::UpgradeStopped) => assert_eq!(
                e.to_string(),
                "moa stopped while upgrading; run the command again to start it"
            ),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("nobody answered"),
        }
    }

    #[tokio::test]
    async fn waiting_for_an_exit_sees_a_gone_pid_and_a_live_one() {
        assert!(wait_for_exit(dead_pid(), Duration::from_millis(200)).await);
        assert!(!wait_for_exit(std::process::id(), Duration::from_millis(200)).await);
    }
}
