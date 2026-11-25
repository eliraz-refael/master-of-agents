//! A throwaway `MOA_ROOT` on a short `/tmp` path, for tests that run a real daemon. Dropping
//! it stops the daemons it started, then the terminal servers under it, and removes the root.
//! It holds each brain's start lock from before the daemons stop until the root is removed,
//! so no client starts a daemon there meanwhile.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use moa_agents::claude::find_transcript;
use moa_core::central::opening::BRIEFED_KEY;
use moa_core::change::Change;
use moa_platform::{Paths, ensure_private_dir, kill_term, pid_alive};
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::import::{IdSource, ImportEnv, import_spike};
use tempfile::TempDir;

use crate::brain::{TYPICAL_TZ, typical_now_ms, tz};
use crate::{Scenario, SpikeFixture, TMUX_NOT_FOUND, wait_for};

/// How long a daemon has to let go of its lock after `shutdown`.
const STOP_WAIT: Duration = Duration::from_secs(3);
/// How long a client may hold a start lock: its daemon has 10 s to say it is ready.
const START_WAIT: Duration = Duration::from_secs(10);

pub struct TempBrain {
    pub dir: TempDir,
    /// `MOA_ROOT`: the temp dir itself, or the sandbox root under it.
    root: PathBuf,
}

fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .expect("a temp dir")
}

impl TempBrain {
    /// A fresh root; `moa_toml`, when given, is written as `<root>/.config/moa/moa.toml`.
    pub fn new(moa_toml: Option<&str>) -> TempBrain {
        let dir = temp_dir();
        let root = dir.path().to_path_buf();
        let brain = TempBrain { dir, root };
        if let Some(text) = moa_toml {
            let paths = brain.paths();
            fs::create_dir_all(paths.config_dir()).expect("the config dir");
            fs::write(paths.config_file(), text).expect("moa.toml");
        }
        brain
    }

    /// A fresh home for `moa --sandbox`: the root is `sandbox_root(home)`, which isn't
    /// created, since `moa --sandbox` makes it. Dropping it cleans up there as usual.
    pub fn sandbox_home() -> TempBrain {
        let dir = temp_dir();
        let root = moa_platform::sandbox_root(dir.path());
        TempBrain { dir, root }
    }

    /// The temp dir: `HOME` for a `sandbox_home`, else the same as `root`.
    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn paths(&self) -> Paths {
        Paths::with_root(self.root())
    }

    /// moa opened `day` already in the default brain: `meta` `moa.briefed` is saved, so the
    /// user's presence that day doesn't open it again (D2). For tests whose prompts mean what
    /// they did before the opening.
    pub fn opened_on(&self, day: Date) {
        let paths = self.paths();
        let mut store =
            Store::open(&paths.db("default"), &paths.backups("default")).expect("the brain's db");
        let opened = Change::Meta {
            key: BRIEFED_KEY.to_string(),
            value: Some(day.to_string()),
        };
        store.apply(&[opened]).expect("moa.briefed is saved");
    }

    /// [`TempBrain::opened_on`] [`TempBrain::today`].
    pub fn opened_today(&self) {
        self.opened_on(Self::today());
    }

    /// Today, as a `moa` run with [`TempBrain::env`] tells the day.
    pub fn today() -> Date {
        Timestamp::now().to_zoned(tz()).date()
    }

    pub fn db(&self, brain: &str) -> PathBuf {
        self.paths().db(brain)
    }

    /// What to give a `moa` process run against this root. `SHELL=/bin/sh` for the same
    /// reason as in [`TempBrain::tmux`]: a `moa` that starts a terminal server passes it on.
    /// `TZDIR` passes through when set: a build sandbox with no system time zone database
    /// (nix on Linux) points it at one, as a real system has.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = vec![
            ("MOA_ROOT".into(), self.root().into()),
            ("HOME".into(), self.root().into()),
            ("TZ".into(), TYPICAL_TZ.into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("TERM".into(), "xterm-256color".into()),
            ("SHELL".into(), "/bin/sh".into()),
        ];
        env.extend(std::env::var_os("TZDIR").map(|d| ("TZDIR".into(), d)));
        env
    }

    /// Runs a tmux command against `brain`'s terminal server (`-S <tmux_socket(brain)>`),
    /// with the environment cleared, then `PATH`, `HOME` (the root), `SHELL`, `TERM` and
    /// `LANG`. Stdout with trailing newlines trimmed, or `Err` carrying stderr on a non-zero
    /// exit. The only way tests reach a TempBrain's server (T23).
    ///
    /// `SHELL=/bin/sh`, as in `Sandbox`: a server takes its `default-shell` from it, else
    /// from the passwd entry, and runs one-string pane commands through that shell. A build
    /// user's entry can be `/sbin/nologin` (nix on macOS), where every pane would die at
    /// once and the server with them.
    pub fn tmux(&self, brain: &str, args: &[&str]) -> io::Result<String> {
        let socket = self
            .paths()
            .tmux_socket(brain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        // A server started before any daemon still needs the socket's folder.
        if let Some(dir) = socket.path(None, 0).parent() {
            ensure_private_dir(dir).map_err(|e| io::Error::other(e.to_string()))?;
        }
        let out = Command::new("tmux")
            .arg("-u")
            .args(socket.args())
            .args(args)
            .env_clear()
            .envs([
                ("PATH", std::env::var_os("PATH").unwrap_or_default()),
                ("HOME", self.root().into()),
                ("SHELL", "/bin/sh".into()),
                ("TERM", "xterm-256color".into()),
                ("LANG", "C.UTF-8".into()),
            ])
            .output()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(io::ErrorKind::NotFound, TMUX_NOT_FOUND)
                } else {
                    e
                }
            })?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string())
    }

    /// Starts a control client (`tmux -C attach`) on `brain`'s terminal server, with
    /// [`TempBrain::env`] as its whole environment. Its stdin and stdout are piped; a caller
    /// that leaves stdout unread and stdin open has a client that stays attached, as a stuck
    /// link from an earlier daemon would.
    pub fn control_client(&self, brain: &str) -> io::Result<std::process::Child> {
        let socket = self
            .paths()
            .tmux_socket(brain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Command::new("tmux")
            .arg("-u")
            .args(socket.args())
            .args(["-C", "attach"])
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(io::ErrorKind::NotFound, TMUX_NOT_FOUND)
                } else {
                    e
                }
            })
    }

    /// The last `n` lines of `brain`'s daemon log and of its stderr log, for a failure
    /// message: the daemon's lines carry ids and counts only, never a record's text.
    pub fn log_tail(&self, brain: &str, n: usize) -> String {
        let paths = self.paths();
        [paths.log(brain), paths.stderr_log(brain)]
            .iter()
            .map(|path| {
                let text = fs::read_to_string(path).unwrap_or_else(|e| format!("({e})\n"));
                let lines: Vec<&str> = text.lines().collect();
                let tail = lines[lines.len().saturating_sub(n)..].join("\n");
                format!("--- {}:\n{tail}\n", path.display())
            })
            .collect()
    }

    /// Kills `brain`'s terminal server when its socket is there, and waits for it to exit.
    fn kill_tmux(&self, brain: &str) {
        let Ok(socket) = self.paths().tmux_socket(brain) else {
            return;
        };
        if !socket.path(None, 0).exists() {
            return;
        }
        let pid = self.tmux(brain, &["display-message", "-p", "#{pid}"]).ok();
        let _ = self.tmux(brain, &["kill-server"]);
        let Some(pid) = pid.and_then(|p| p.parse::<u32>().ok()) else {
            return;
        };
        if wait_for(STOP_WAIT, || (!pid_alive(pid)).then_some(())).is_none() {
            // Never leave a server running once its socket's folder is deleted.
            kill_term(pid);
        }
    }

    /// The Typical spike fixture under `<root>/spike-home`, imported into `db(brain)` with
    /// derived ids (so a test knows the ids it will see). The brain's folder is made
    /// private, as `moa adopt` makes it.
    pub fn adopt_typical(&self, brain: &str) {
        let home = self.root().join("spike-home");
        fs::create_dir_all(&home).expect("the spike home");
        let spike = self.root().join("spike.db");
        SpikeFixture::write(&spike, Scenario::Typical, &home).expect("the fixture writes");
        let db = self.db(brain);
        ensure_private_dir(db.parent().expect("a brain dir")).expect("the brain dir");
        let mut store = Store::open(&db, &self.paths().backups(brain)).expect("a new store");
        let env = ImportEnv {
            tz: tz(),
            tz_spec: Some(TYPICAL_TZ.into()),
            now: typical_now_ms(),
            brain: brain.into(),
            accounts: AccountMap::new(home),
            ids: IdSource::Derived,
            transcript: find_transcript,
        };
        import_spike(&spike, &mut store, &env).expect("the fixture imports");
    }
}

/// A std client's one-shot: says hello, then `shutdown`, and returns the daemon's pid from
/// the hello reply. `None` when nothing answered.
fn ask_to_stop(sock: &Path) -> Option<u32> {
    let mut stream = UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(STOP_WAIT)).ok()?;
    stream.set_write_timeout(Some(STOP_WAIT)).ok()?;
    // `shutdown` is allowed on a mismatched build, so the build here is made up.
    let hello = r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"temp-brain","build":"teardown","caller":{}}}"#;
    let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#;
    writeln!(stream, "{hello}").ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let pid = serde_json::from_str::<serde_json::Value>(&line)
        .ok()?
        .pointer("/result/pid")?
        .as_u64()
        .and_then(|p| u32::try_from(p).ok());
    let _ = writeln!(stream, "{shutdown}");
    pid
}

/// Whether nobody holds the brain's lock. This `try_lock` probe is for teardown only: it
/// must never be copied into client code, where taking the daemon's lock, even for a
/// moment, could make a daemon being started meanwhile say "already running" (D1: the
/// client never touches `lock(b)`).
fn lock_is_free(lock: &Path) -> bool {
    match fs::OpenOptions::new().read(true).write(true).open(lock) {
        Ok(f) => f.try_lock().is_ok(),
        // No lock file, no daemon.
        Err(_) => true,
    }
}

/// Takes `brain`'s start lock, the one a client holds while it starts a daemon, waiting as
/// long as a start may take. `None` when it can't be had: the teardown goes on without it.
fn hold_start_lock(paths: &Paths, brain: &str) -> Option<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.start_lock(brain))
        .ok()?;
    wait_for(START_WAIT, || file.try_lock().is_ok().then_some(()))?;
    Some(file)
}

impl Drop for TempBrain {
    /// Takes the start lock of every brain it finds (by its lock, its start lock or its
    /// `tmux-<b>` socket), so no client starts a daemon while the root is taken down; a start
    /// already under way is let finish first. Then stops every daemon: asks it to
    /// `shutdown`, waits for its lock to be free, and sends SIGTERM to the pid it reported
    /// if it still holds it. Then kills the terminal servers, once the daemons have taken
    /// their hooks down, and removes the root while the start locks are still held. Errors
    /// are ignored.
    ///
    /// L15: a client that starts a daemon here (a panel's first connect, late) would leave
    /// it running under a removed root. One that waited on a start lock finds the root gone.
    fn drop(&mut self) {
        let paths = self.paths();
        let Ok(entries) = fs::read_dir(paths.run_dir()) else {
            return;
        };
        let brains: BTreeSet<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let brain = name
                    .strip_prefix("tmux-")
                    .or_else(|| name.strip_suffix(".lock"))
                    .or_else(|| name.strip_suffix(".start"))?;
                Some(brain.to_string())
            })
            .collect();
        let _starts: Vec<fs::File> = brains
            .iter()
            .filter_map(|brain| hold_start_lock(&paths, brain))
            .collect();
        for brain in &brains {
            let lock = paths.lock(brain);
            let pid = paths.socket(brain).ok().and_then(|s| ask_to_stop(&s));
            let free = wait_for(STOP_WAIT, || lock_is_free(&lock).then_some(()));
            if free.is_none()
                && let Some(pid) = pid
            {
                kill_term(pid);
                let _ = wait_for(STOP_WAIT, || lock_is_free(&lock).then_some(()));
            }
        }
        for brain in &brains {
            self.kill_tmux(brain);
        }
        // `TempDir` would remove it only after the start locks are let go.
        let _ = fs::remove_dir_all(self.dir.path());
    }
}
