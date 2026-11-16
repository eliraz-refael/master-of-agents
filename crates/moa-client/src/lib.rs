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
    code, decode_server_line,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

/// How long a started daemon has to say `ready`.
const READY_WAIT: Duration = Duration::from_secs(10);
/// How long a daemon told to `upgrade` has to exit.
const EXIT_WAIT: Duration = Duration::from_secs(10);
const EXIT_POLL: Duration = Duration::from_millis(50);
/// How long to read to the end of a connection after `shutdown` or `upgrade`.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// What to do when the daemon is a different build from this client.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnMismatch {
    /// Ask it to `upgrade` (it exits), start a fresh one, and say hello again. Once: a
    /// daemon that still doesn't match is an error.
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
    /// Pass [`own_exe`], so a `moactl` symlink never becomes the daemon's `argv[0]`.
    pub autostart: Option<PathBuf>,
    pub on_mismatch: OnMismatch,
    /// This client's build id (`moa_platform::build_id()`), compared with the daemon's.
    pub build: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the {0} brain's moa isn't running")]
    NotRunning(BrainName),
    /// What the daemon said when it couldn't start, with where its log is.
    #[error("couldn't start moa: {0}")]
    Start(String),
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
    /// operation ran.
    #[error("moa stopped before answering; try again")]
    Stopped,
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
/// `PATH`, `USER`, `LANG`, `LC_*`, `MOA_LOG`; `TZ` only when sandboxed (a real home has
/// the system zone); `MOA_REAL_HOME` only when not sandboxed.
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
    env.extend(parent.filter(|(k, _)| match k.to_str() {
        Some("PATH" | "USER" | "LANG" | "MOA_LOG") => true,
        Some("TZ") => sandboxed,
        Some("MOA_REAL_HOME") => !sandboxed,
        Some(k) => k.starts_with("LC_"),
        None => false,
    }));
    env
}

/// This executable's real path, for [`ClientOptions::autostart`].
pub fn own_exe() -> io::Result<PathBuf> {
    std::fs::canonicalize(std::env::current_exe()?)
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

    /// Writes a request and returns its id.
    async fn send(&mut self, method: &str, params: Value) -> Result<Id, ClientError> {
        self.next += 1;
        let id = Id::Num(self.next);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match self.framed.send(line.to_string()).await {
            Ok(()) => Ok(id),
            Err(LinesCodecError::Io(e)) if is_gone(&e) => Err(ClientError::Stopped),
            Err(LinesCodecError::Io(e)) => Err(e.into()),
            Err(e) => Err(invalid(e)),
        }
    }

    /// The next line, or `None` when the connection has ended.
    async fn read(&mut self) -> Result<Option<ServerLine>, ClientError> {
        match self.framed.next().await {
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

    /// Sends a request and waits for the reply that carries its id; notices that arrive
    /// meanwhile are kept for `next_notice`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.send(method, params).await?;
        loop {
            match self.read().await? {
                None => return Err(ClientError::Stopped),
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

    /// Sends `shutdown` or `upgrade` and reads to the end of the connection: the daemon
    /// closes it after replying, so an end of stream is the expected answer.
    async fn stop(&mut self, method: &str) -> Result<(), ClientError> {
        match self.send(method, json!({})).await {
            Ok(_) | Err(ClientError::Stopped) => {}
            Err(e) => return Err(e),
        }
        let to_the_end = async { while let Ok(Some(_)) = self.read().await {} };
        let _ = timeout(CLOSE_WAIT, to_the_end).await;
        Ok(())
    }
}

pub struct Client {
    wire: Wire,
    hello: HelloOk,
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
            if restarted {
                return Err(ClientError::Rpc(RpcError {
                    code: code::BUILD_MISMATCH,
                    message: "moa was restarted and is still a different build".into(),
                }));
            }
            restarted = true;
            client.upgrade(&opts).await?;
        }
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
        Ok(Client { wire, hello })
    }

    /// Asks the daemon to stop for a restart and waits until its process is gone.
    async fn upgrade(mut self, opts: &ClientOptions) -> Result<(), ClientError> {
        let pid = self.hello.pid;
        self.wire.stop("upgrade").await?;
        drop(self);
        let deadline = Instant::now() + EXIT_WAIT;
        while pid_alive(pid) {
            if Instant::now() >= deadline {
                return Err(start_error(
                    &opts.paths,
                    &opts.brain,
                    "the old moa didn't stop within 10 s",
                ));
            }
            sleep(EXIT_POLL).await;
        }
        Ok(())
    }

    pub fn hello(&self) -> &HelloOk {
        &self.hello
    }

    pub async fn call(&mut self, op: Op) -> Result<Done, ClientError> {
        let mut wire = serde_json::to_value(&op).map_err(io::Error::other)?;
        let method = wire["method"].as_str().unwrap_or_default().to_string();
        let params = wire["params"].take();
        decode(self.wire.request(&method, params).await?)
    }

    pub async fn status(&mut self) -> Result<StatusOk, ClientError> {
        decode(self.wire.request("status", json!({})).await?)
    }

    /// Asks the daemon to stop. The connection ending is the answer.
    pub async fn shutdown(&mut self) -> Result<(), ClientError> {
        self.wire.stop("shutdown").await
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
    cmd.args(["daemon", "--brain", b, "--notify-ready"])
        .env_clear()
        .envs(daemon_env(std::env::vars_os(), paths))
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
        ReadyLine::TryAgain => UnixStream::connect(sock)
            .await
            .map_err(|e| start_error(paths, brain, &format!("it didn't answer: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use moa_platform::Paths;
    use moa_proto::{Incoming, encode_result};
    use tokio::net::UnixListener;

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

    /// A daemon that answers `hello`, reads `then_read` more lines, and hangs up.
    struct Fake {
        _dir: tempfile::TempDir,
        opts: ClientOptions,
    }

    fn fake(then_read: usize) -> Fake {
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
                pid: 1,
            };
            framed.send(encode_result(&id, &ok)).await.unwrap();
            for _ in 0..then_read {
                framed.next().await;
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
        };
        Fake { _dir: dir, opts }
    }

    #[tokio::test]
    async fn a_reply_that_never_comes_is_stopped() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.call(Op::SessionLs { all: false }).await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped), "{err}");
        assert_eq!(err.to_string(), "moa stopped before answering; try again");
    }

    #[tokio::test]
    async fn status_and_subscribe_also_say_stopped() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.status().await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped), "{err}");
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        let err = client.subscribe(&[Topic::Sessions]).await.unwrap_err();
        assert!(matches!(err, ClientError::Stopped), "{err}");
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
    async fn shutdown_ending_the_connection_is_not_an_error() {
        let f = fake(1);
        let mut client = Client::connect(f.opts.clone()).await.unwrap();
        client.shutdown().await.unwrap();
    }
}
