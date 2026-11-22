//! A child process's end and its stderr: a reaper that reports the exit while the pid is
//! still the child's, signals that never reach a reused pid, and stderr read line by line.

use std::io;
use std::process::Child;

use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::{oneshot, watch};

use crate::sys::wait_exited;

/// A child's exit, seen by a thread of its own: a flag that goes up once the child has
/// exited, raised while the pid is still the child's (a zombie), and only then the reap. So
/// [`Reaper::kill_unless_exited`] never signals a pid that has been given to another process.
#[derive(Clone, Debug)]
pub struct Reaper {
    pid: u32,
    exited: watch::Receiver<bool>,
}

impl Reaper {
    /// Watches `child` until it exits and reaps it. `on_exit` runs once it has exited, just
    /// before the flag goes up: while the pid (and its process group) can't have been reused,
    /// except after a wait that failed (see [`after_wait`]).
    pub fn watch(mut child: Child, on_exit: impl FnOnce() + Send + 'static) -> Reaper {
        let pid = child.id();
        let (exit_tx, exited) = watch::channel(false);
        std::thread::spawn(move || {
            after_wait(
                wait_exited(pid),
                || {
                    on_exit();
                    let _ = exit_tx.send(true);
                },
                || drop(child.wait()),
            );
        });
        Reaper { pid, exited }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Waits for the exit to be reported.
    pub async fn exited(&self) {
        let mut exited = self.exited.clone();
        // An error means the reaper thread is gone, and so is the child.
        let _ = exited.wait_for(|gone| *gone).await;
    }

    /// Runs `kill` with the pid only while the exit has not been reported, so a reused pid
    /// is never signalled: `kill_term`, or `kill_group` for the child's process group.
    pub fn kill_unless_exited(&self, kill: impl FnOnce(u32)) {
        // The read guard is held across the kill (R28): the reaper's `send(true)` waits for it,
        // and so does the reap after that, so the pid can't be reaped and reused between the
        // look and the signal.
        let seen = self.exited.borrow();
        if !*seen {
            kill(self.pid);
        }
        drop(seen);
    }
}

/// The reaper's steps once `wait_exited` has returned (it retries an interrupted wait
/// itself): `flag` raises the exited flag, `reap` reaps the child.
/// - Seen exited: the flag goes up while the pid is still the child's (a zombie), then the
///   child is reaped.
/// - The wait failed: nothing says the child has exited. It is reaped first (a blocking
///   wait), and only then is the flag raised, so the flag never says "exited" of a child
///   that still runs. A pid reused in between would be the one gap, as before `WNOWAIT`.
pub fn after_wait(waited: io::Result<()>, flag: impl FnOnce(), reap: impl FnOnce()) {
    match waited {
        Ok(()) => {
            flag();
            reap();
        }
        Err(e) => {
            tracing::warn!("a child's reaper couldn't watch for its exit: {}", e.kind());
            reap();
            flag();
        }
    }
}

/// Reads stderr to its end, handing back the first line and discarding the rest.
pub fn first_stderr_line(stderr: ChildStderr) -> oneshot::Receiver<String> {
    let (tx, rx) = oneshot::channel();
    let mut tx = Some(tx);
    each_stderr_line(stderr, move |line| {
        if let Some(tx) = tx.take() {
            let _ = tx.send(line);
        }
    });
    rx
}

/// The most of one stderr line held at once, in bytes: a longer line is handed on in pieces
/// of this size, so a child that never writes a newline can't grow the buffer without end.
const STDERR_LINE_MAX: u64 = 64 * 1024;

/// Reads stderr to its end, handing `each` every line, without its line ending (a line longer
/// than [`STDERR_LINE_MAX`] comes in pieces). Bytes that aren't UTF-8 become U+FFFD, so a bad
/// byte never stops the reading (and so never leaves the child blocked on a full pipe).
pub fn each_stderr_line(stderr: ChildStderr, mut each: impl FnMut(String) + Send + 'static) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut piece = (&mut reader).take(STDERR_LINE_MAX);
            match piece.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            each(String::from_utf8_lossy(line).into_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order of the reaper's steps after a wait that came back with `waited`.
    fn reaper_steps(waited: io::Result<()>) -> Vec<&'static str> {
        let steps = std::cell::RefCell::new(Vec::new());
        after_wait(
            waited,
            || steps.borrow_mut().push("flag"),
            || steps.borrow_mut().push("reap"),
        );
        steps.into_inner()
    }

    #[test]
    fn a_seen_exit_is_flagged_before_the_reap() {
        assert_eq!(reaper_steps(Ok(())), ["flag", "reap"]);
    }

    /// Code review (b1): a wait that failed says nothing about the child, so the flag waits
    /// for the reap: never a false "exited" for a child that still runs.
    #[test]
    fn a_failed_wait_reaps_before_it_flags() {
        let failed = io::Error::from(io::ErrorKind::InvalidInput);
        assert_eq!(reaper_steps(Err(failed)), ["reap", "flag"]);
    }

    /// Every line, without its ending (`\n` or `\r\n`), the last one too; a byte that isn't
    /// UTF-8 is replaced and the reading goes on.
    #[tokio::test]
    async fn every_stderr_line_is_read_whatever_its_bytes() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", r"printf 'one\r\nt\377o\nlast' >&2"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = ChildStderr::from_std(child.stderr.take().unwrap()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        each_stderr_line(stderr, move |line| drop(tx.send(line)));
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        child.wait().unwrap();
        assert_eq!(lines, ["one", "t\u{FFFD}o", "last"]);
    }

    /// A line with no end in sight comes in bounded pieces, nothing lost.
    #[tokio::test]
    async fn a_long_stderr_line_comes_in_pieces() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "head -c 100000 /dev/zero | tr '\\0' x >&2"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = ChildStderr::from_std(child.stderr.take().unwrap()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        each_stderr_line(stderr, move |line| {
            let _ = tx.send(line.len());
        });
        let mut lens = Vec::new();
        while let Some(len) = rx.recv().await {
            lens.push(len);
        }
        child.wait().unwrap();
        assert_eq!(lens, [65536, 100000 - 65536]);
    }
}
