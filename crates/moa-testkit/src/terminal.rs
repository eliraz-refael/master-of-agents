use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use crate::wait_for;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A program running on a pseudo-terminal, as a user's terminal would run it. It is the
/// pty's master end: `press` types, `wait_for_text` reads what the program drew. Tests only.
pub struct Terminal {
    master: File,
    child: Child,
    tty: String,
    out: Arc<Mutex<Vec<u8>>>,
    mark: Mutex<usize>,
}

impl Terminal {
    /// Runs `program` with the pty as its stdin, stdout and stderr. The environment is
    /// cleared, then `env` is set, then `TERM=xterm-256color` and `LANG=C.UTF-8` are added
    /// when `env` lacks them.
    pub fn spawn(
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        cols: u16,
        rows: u16,
    ) -> io::Result<Terminal> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let tty = ptsname(&master, Vec::new())?;
        let tty = tty.to_string_lossy().into_owned();
        let slave: OwnedFd =
            rustix::fs::open(tty.as_str(), OFlags::RDWR | OFlags::NOCTTY, Mode::empty())?;
        tcsetwinsize(
            &slave,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;

        let mut cmd = Command::new(program);
        cmd.args(args).env_clear().envs(env.iter().cloned());
        for (k, v) in [("TERM", "xterm-256color"), ("LANG", "C.UTF-8")] {
            if !env.iter().any(|(name, _)| name == k) {
                cmd.env(k, v);
            }
        }
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        let child = cmd.spawn()?;
        // The command holds the only slave descriptors the parent has; dropping it means
        // the reader sees the end of output once the child is gone.
        drop(cmd);

        let master = File::from(master);
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone()?;
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // The end is a zero read, or EIO (what the pty gives once the child is gone).
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                lock(&sink).extend_from_slice(&buf[..n]);
            }
        });
        Ok(Terminal {
            master,
            child,
            tty,
            out,
            mark: Mutex::new(0),
        })
    }

    /// Types `bytes` as if the user did.
    pub fn press(&self, bytes: &[u8]) {
        let mut w = &self.master;
        let _ = w.write_all(bytes);
    }

    /// Later `wait_for_text` calls see only output after this.
    pub fn mark(&self) {
        *lock(&self.mark) = lock(&self.out).len();
    }

    /// Whether `text` shows up in the output since the last `mark`, within `limit`.
    pub fn wait_for_text(&self, text: &str, limit: Duration) -> bool {
        wait_for(limit, || {
            let from = *lock(&self.mark);
            let out = lock(&self.out);
            let seen = String::from_utf8_lossy(&out[from.min(out.len())..]);
            seen.contains(text).then_some(())
        })
        .is_some()
    }

    /// The pty's path: what tmux calls this client.
    pub fn tty(&self) -> String {
        self.tty.clone()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
