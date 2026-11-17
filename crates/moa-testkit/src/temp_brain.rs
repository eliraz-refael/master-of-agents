//! A throwaway `MOA_ROOT` on a short `/tmp` path, for tests that run a real daemon. Dropping
//! it stops the daemons it started, then the terminal servers under it.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use moa_agents::claude::find_transcript;
use moa_platform::{Paths, ensure_private_dir, kill_term, pid_alive};
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::import::{IdSource, ImportEnv, import_spike};
use tempfile::TempDir;

use crate::brain::{TYPICAL_TZ, typical_now_ms, tz};
use crate::{Scenario, SpikeFixture, TMUX_NOT_FOUND, wait_for};

/// How long a daemon has to let go of its lock after `shutdown`.
const STOP_WAIT: Duration = Duration::from_secs(3);

pub struct TempBrain {
    pub dir: TempDir,
}

impl TempBrain {
    /// A fresh root; `moa_toml`, when given, is written as `<root>/.config/moa/moa.toml`.
    pub fn new(moa_toml: Option<&str>) -> TempBrain {
        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .expect("a temp dir");
        let brain = TempBrain { dir };
        if let Some(text) = moa_toml {
            let paths = brain.paths();
            fs::create_dir_all(paths.config_dir()).expect("the config dir");
            fs::write(paths.config_file(), text).expect("moa.toml");
        }
        brain
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn paths(&self) -> Paths {
        Paths::with_root(self.root())
    }

    pub fn db(&self, brain: &str) -> PathBuf {
        self.paths().db(brain)
    }

    /// What to give a `moa` process run against this root.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("MOA_ROOT".into(), self.root().into()),
            ("HOME".into(), self.root().into()),
            ("TZ".into(), TYPICAL_TZ.into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("TERM".into(), "xterm-256color".into()),
        ]
    }

    /// Runs a tmux command against `brain`'s terminal server (`-S <tmux_socket(brain)>`),
    /// with the environment cleared, then `PATH`, `HOME` (the root), `TERM` and `LANG`.
    /// Stdout with trailing newlines trimmed, or `Err` carrying stderr on a non-zero exit.
    /// The only way tests reach a TempBrain's server (T23).
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
    /// derived ids (so a test knows the ids it will see).
    pub fn adopt_typical(&self, brain: &str) {
        let home = self.root().join("spike-home");
        fs::create_dir_all(&home).expect("the spike home");
        let spike = self.root().join("spike.db");
        SpikeFixture::write(&spike, Scenario::Typical, &home).expect("the fixture writes");
        let db = self.db(brain);
        fs::create_dir_all(db.parent().expect("a brain dir")).expect("the brain dir");
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

impl Drop for TempBrain {
    /// Stops every daemon with a lock under this root: asks it to `shutdown`, waits for the
    /// lock to be free, and sends SIGTERM to the pid it reported if it still holds it. Then
    /// kills the terminal server of every brain it found (by its lock or its `tmux-<b>`
    /// socket), once the daemons have taken their hooks down. Errors are ignored.
    fn drop(&mut self) {
        let paths = self.paths();
        let Ok(entries) = fs::read_dir(paths.run_dir()) else {
            return;
        };
        let mut brains = BTreeSet::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(brain) = name.strip_prefix("tmux-") {
                brains.insert(brain.to_string());
            }
            let Some(brain) = name.strip_suffix(".lock") else {
                continue;
            };
            brains.insert(brain.to_string());
            let pid = paths.socket(brain).ok().and_then(|s| ask_to_stop(&s));
            let free = wait_for(STOP_WAIT, || lock_is_free(&entry.path()).then_some(()));
            if free.is_none()
                && let Some(pid) = pid
            {
                kill_term(pid);
                let _ = wait_for(STOP_WAIT, || lock_is_free(&entry.path()).then_some(()));
            }
        }
        for brain in brains {
            self.kill_tmux(&brain);
        }
    }
}
