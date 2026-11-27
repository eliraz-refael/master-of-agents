//! A running daemon on a short `/tmp` root and a raw line client for it.
#![allow(dead_code)]

pub mod hooks;
pub mod rig;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_daemon::testing::FakeTime;
use moa_daemon::{DaemonError, Ready, RunOptions, run};
use moa_platform::Paths;
use moa_testkit::TempBrain;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub type Done = JoinHandle<Result<(), DaemonError>>;

/// A root short enough for a unix socket path.
pub fn short_root() -> TempDir {
    tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .expect("a temp dir")
}

pub struct Daemon {
    pub root: TempDir,
    pub paths: Paths,
    pub sock: PathBuf,
    pub done: Done,
    pub brain: TypicalBrain,
    pub time: Arc<FakeTime>,
}

pub fn place_db(paths: &Paths, brain: &TypicalBrain) {
    moa_store::backup::vacuum_into(brain.store().conn(), &paths.db("default"))
        .expect("the brain is put in place");
}

pub fn options(
    paths: &Paths,
    brain: &TypicalBrain,
    time: &Arc<FakeTime>,
    ready: Ready,
) -> RunOptions {
    let mut config = brain.config();
    config.canonicalize_with(moa_platform::canonical);
    RunOptions {
        paths: paths.clone(),
        brain: BrainName::default_brain(),
        config,
        config_text: None,
        tz: TimeZone::posix(TYPICAL_TZ).expect("a zone"),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        zone: None,
        ready,
        events_capacity: 256,
        uid_override: None,
        exe: moa_daemon::daemon_exe().expect("the test's program"),
        build: moa_platform::build_id().to_string(),
        exec: moa_daemon::Exec::Test(tokio::sync::mpsc::channel(1).0),
        inherited: None,
        probe_wait: moa_daemon::ProbeWait::default(),
    }
}

/// A fresh root with the Typical brain in it, `setup` run on its `Paths`, `tweak` on the
/// options, and a daemon started and ready.
pub async fn boot_with(setup: impl FnOnce(&Paths), tweak: impl FnOnce(&mut RunOptions)) -> Daemon {
    let root = short_root();
    let paths = Paths::with_root(root.path());
    let brain = TypicalBrain::new();
    place_db(&paths, &brain);
    setup(&paths);
    let time = FakeTime::new(brain.now().wall);
    let (tx, rx) = oneshot::channel();
    let mut opts = options(&paths, &brain, &time, Ready::Channel(tx));
    tweak(&mut opts);
    let done = tokio::spawn(run(opts));
    let sock = match timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(sock)) => sock,
        Ok(Err(_)) => panic!("run ended before ready: {:?}", done.await),
        Err(_) => panic!("run was not ready after 10 s"),
    };
    Daemon {
        root,
        paths,
        sock,
        done,
        brain,
        time,
    }
}

pub async fn boot() -> Daemon {
    boot_with(|_| {}, |_| {}).await
}

/// Creates the default brain's terminal server exactly as the front door does (T1): `_moa`,
/// then the user's `moa` session at home. Its config is the daemon's, so a daemon has started
/// on `paths` first.
pub fn create_server(tb: &TempBrain, paths: &Paths) {
    let conf = paths.tmux_conf("default").to_string_lossy().into_owned();
    let home = paths.home().to_string_lossy().into_owned();
    let args = [
        "-f",
        &conf,
        "new-session",
        "-d",
        "-s",
        "_moa",
        "-x",
        "10",
        "-y",
        "3",
        "/bin/sh",
        ";",
        "new-session",
        "-d",
        "-s",
        "moa",
        "-n",
        "moa",
        "-c",
        &home,
    ];
    tb.tmux("default", &args)
        .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"));
}

pub fn request(id: i64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// A blocking client over a std socket. Reads give up after 10 s.
pub struct Client {
    r: BufReader<UnixStream>,
    next: i64,
}

impl Client {
    pub fn connect(sock: &Path) -> Client {
        let s = UnixStream::connect(sock).expect("the daemon's socket accepts");
        // macOS returns EINVAL from setsockopt once the peer has closed (the daemon
        // turns a rejected connection away at once); the next read then sees EOF.
        match s.set_read_timeout(Some(Duration::from_secs(10))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("set_read_timeout: {e}"),
        }
        Client {
            r: BufReader::new(s),
            next: 0,
        }
    }

    pub fn write(&mut self, line: &str) {
        let w = self.r.get_mut();
        w.write_all(line.as_bytes()).expect("write");
        w.write_all(b"\n").expect("write");
    }

    /// A write that may fail (the daemon may have closed the connection).
    pub fn try_write(&mut self, line: &str) {
        let w = self.r.get_mut();
        let _ = w.write_all(line.as_bytes());
        let _ = w.write_all(b"\n");
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.r.get_mut().write_all(bytes).expect("write");
    }

    /// The next line, or `None` when the daemon closed the connection.
    pub fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.r.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).expect("a JSON line")),
        }
    }

    pub fn id(&mut self) -> i64 {
        self.next += 1;
        self.next
    }

    /// Sends a request and returns the reply that carries its id, skipping notices.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.reply(id)
    }

    /// Sends a request and returns its id, for [`Client::reply`] later.
    pub fn send(&mut self, method: &str, params: Value) -> i64 {
        let id = self.id();
        self.write(&request(id, method, params));
        id
    }

    /// The reply that carries `id`, skipping notices and other replies.
    pub fn reply(&mut self, id: i64) -> Value {
        loop {
            let v = self.read().expect("a reply");
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }

    pub fn hello_as(&mut self, build: &str) -> Value {
        self.call(
            "hello",
            json!({"client": "test", "build": build, "caller": {}}),
        )
    }

    pub fn hello(&mut self) -> Value {
        self.hello_as(moa_platform::build_id())
    }

    /// The next notice with this method, skipping other notices.
    pub fn notice(&mut self, method: &str) -> Value {
        loop {
            let v = self.read().expect("a notice");
            if v.get("method") == Some(&json!(method)) {
                return v["params"].clone();
            }
        }
    }
}
