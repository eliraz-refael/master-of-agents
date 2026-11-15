use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tempfile::TempDir;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Try `f` every 20 ms until it returns `Some` or `limit` passes. Tests only.
pub fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(20));
    }
}

/// A private tmux server: its own socket, home, config and environment.
pub struct Sandbox {
    name: String,
    dir: TempDir,
    home: PathBuf,
}

impl Sandbox {
    pub fn start() -> io::Result<Sandbox> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("moa-test-{}-{n}", std::process::id());
        // One short root for everything: unix socket paths are limited to 104 bytes on
        // macOS, and later slices put the daemon socket under `home`.
        let dir = tempfile::Builder::new().prefix("moa").tempdir_in("/tmp")?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home)?;
        std::fs::write(
            dir.path().join("tmux.conf"),
            "set -g default-shell /bin/sh\nset -g @moa-sandbox 1\n",
        )?;
        let sb = Sandbox { name, dir, home };
        let conf = sb.dir.path().join("tmux.conf");
        let home = sb.home.clone();
        sb.tmux(&[
            "-f",
            &conf.to_string_lossy(),
            "new-session",
            "-d",
            "-s",
            "sandbox",
            "-c",
            &home.to_string_lossy(),
            "-x",
            "200",
            "-y",
            "50",
        ])?;
        Ok(sb)
    }

    /// Run a tmux command against the sandbox server; stdout with trailing newlines
    /// trimmed, or `Err` carrying stderr on a non-zero exit.
    pub fn tmux(&self, args: &[&str]) -> io::Result<String> {
        let out = Command::new("tmux")
            .args(["-u", "-L", &self.name])
            .args(args)
            .env_clear()
            .envs(self.env())
            .output()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "tmux not found on PATH (moa needs tmux >= 3.4; or run inside `nix develop`)",
                    )
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

    pub fn socket_name(&self) -> &str {
        &self.name
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The exact environment the sandbox gives tmux.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), self.home.clone().into_os_string()),
            ("SHELL".into(), "/bin/sh".into()),
            ("TMUX_TMPDIR".into(), self.dir.path().as_os_str().to_owned()),
            ("TERM".into(), "xterm-256color".into()),
        ]
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let pid = self.tmux(&["display-message", "-p", "#{pid}"]).ok();
        let _ = self.tmux(&["kill-server"]);
        if let Some(pid) = pid {
            wait_for(Duration::from_secs(2), || {
                let alive = Command::new("kill")
                    .args(["-0", &pid])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(true); // can't run kill: assume still alive and keep waiting
                (!alive).then_some(())
            });
        }
    }
}
