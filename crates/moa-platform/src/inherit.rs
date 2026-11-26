//! Handing the daemon's listening socket and brain lock to the next image of itself, so a
//! restart into a new build keeps the same pid, refuses no connection and never lets the
//! lock go.
//!
//! The old image clears close-on-exec on both and execs with their numbers in the
//! environment ([`reexec`]); the new one takes them back before it starts any thread
//! ([`take_inherited`]).

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use crate::sys;

/// The listening socket's descriptor number.
const LISTEN_FDS: &str = "MOA_LISTEN_FDS";
/// The brain lock's descriptor number.
const LOCK_FD: &str = "MOA_LOCK_FD";
/// The build of the image that handed them over.
const REEXEC_FROM: &str = "MOA_REEXEC_FROM";

/// What the previous image of this process handed over across its re-exec.
#[derive(Debug)]
pub struct Inherited {
    /// The daemon's listening socket, still bound and listening: connections made during
    /// the handoff wait in its backlog.
    pub listener: UnixListener,
    /// The brain lock's file, still locked: it is the same open file.
    pub lock: File,
    /// The previous image's build id.
    pub from_build: String,
}

/// Takes what a [`reexec`] handed over, and removes its variables so nothing this process
/// starts sees them. `None` when this process wasn't started that way; an error when the
/// variables are incomplete or don't name open descriptors. Both descriptors are
/// close-on-exec again.
///
/// Call it first thing in `main`, before any thread starts (the tokio runtime included):
/// it changes the environment, and it takes ownership of descriptors by number.
pub fn take_inherited() -> io::Result<Option<Inherited>> {
    let (listener, lock, from) = (
        sys::take_env(LISTEN_FDS),
        sys::take_env(LOCK_FD),
        sys::take_env(REEXEC_FROM),
    );
    if listener.is_none() && lock.is_none() && from.is_none() {
        return Ok(None);
    }
    let listener = sys::adopt_inherited(number(LISTEN_FDS, listener)?)?;
    let lock = sys::adopt_inherited(number(LOCK_FD, lock)?)?;
    let from_build = from
        .and_then(|f| f.into_string().ok())
        .ok_or_else(|| handoff_error(REEXEC_FROM))?;
    Ok(Some(Inherited {
        listener: listener.into(),
        lock: lock.into(),
        from_build,
    }))
}

/// The descriptor number `var` held.
fn number(var: &str, value: Option<OsString>) -> io::Result<RawFd> {
    value
        .and_then(|v| v.to_str()?.parse().ok())
        .ok_or_else(|| handoff_error(var))
}

fn handoff_error(var: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the restart handed over no usable {var}"),
    )
}

/// Keeps `fd` open across this process's next exec (clears its close-on-exec flag).
pub fn keep_open(fd: BorrowedFd<'_>) -> io::Result<()> {
    sys::set_cloexec(&fd, false)
}

/// Replaces this process with `exe args`, handing it `listener` and `lock` (kept open and
/// named in the environment) and `from_build`, for [`take_inherited`] to take back. Returns
/// only if it failed.
pub fn reexec(
    exe: &Path,
    args: &[&str],
    listener: BorrowedFd<'_>,
    lock: BorrowedFd<'_>,
    from_build: &str,
) -> io::Error {
    if let Err(e) = keep_open(listener).and_then(|()| keep_open(lock)) {
        return e;
    }
    Command::new(exe)
        .args(args)
        .env(LISTEN_FDS, listener.as_raw_fd().to_string())
        .env(LOCK_FD, lock.as_raw_fd().to_string())
        .env(REEXEC_FROM, from_build)
        .exec()
}

#[cfg(test)]
mod tests {
    use std::fs::TryLockError;
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use super::*;

    /// Set only in the round trip's child, to the directory it works in.
    const CHILD_DIR: &str = "MOA_TEST_HANDOFF_DIR";
    const CHILD: &str = "inherit::tests::handoff_child";
    const CHILD_ARGS: [&str; 4] = ["--exact", CHILD, "--nocapture", "--test-threads=1"];

    #[test]
    fn nothing_is_inherited_without_the_variables() {
        assert!(take_inherited().unwrap().is_none());
    }

    /// Runs `handoff_child` as a child process (never this one), which re-execs itself
    /// holding a listener and a lock.
    #[test]
    fn the_listener_and_the_lock_survive_a_reexec() {
        let tmp = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(CHILD_ARGS)
            .env(CHILD_DIR, tmp.path())
            .env_remove(LISTEN_FDS)
            .env_remove(LOCK_FD)
            .env_remove(REEXEC_FROM)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{text}\n{err}");
        let pids: Vec<&str> = text
            .lines()
            .filter_map(|l| Some(l.split_once("handoff pid ")?.1))
            .collect();
        assert_eq!(pids.len(), 2, "both images ran: {text}");
        assert_eq!(pids[0], pids[1], "one process: {text}");
        assert!(text.contains("handoff ok"), "{text}\n{err}");
    }

    /// The round trip's child, in two images around a re-exec. Without `CHILD_DIR` (any
    /// ordinary run) it does nothing.
    #[test]
    fn handoff_child() {
        let Some(dir) = std::env::var_os(CHILD_DIR).map(PathBuf::from) else {
            return;
        };
        println!("handoff pid {}", std::process::id());
        std::io::stdout().flush().unwrap();
        match take_inherited().unwrap() {
            None => {
                let listener = UnixListener::bind(dir.join("sock")).unwrap();
                let lock = File::create(dir.join("lock")).unwrap();
                lock.try_lock().unwrap();
                // Connected before the handoff; accepted only after it.
                let mut early = UnixStream::connect(dir.join("sock")).unwrap();
                early.write_all(b"before").unwrap();
                let exe = std::env::current_exe().unwrap();
                let e = reexec(
                    &exe,
                    &CHILD_ARGS,
                    listener.as_fd(),
                    lock.as_fd(),
                    "the-old-build",
                );
                panic!("the re-exec failed: {e}");
            }
            Some(got) => {
                assert_eq!(got.from_build, "the-old-build");
                let (mut conn, _) = got.listener.accept().unwrap();
                let mut said = String::new();
                conn.read_to_string(&mut said).unwrap();
                assert_eq!(said, "before");
                let other = File::open(dir.join("lock")).unwrap();
                assert!(
                    matches!(other.try_lock(), Err(TryLockError::WouldBlock)),
                    "the lock is still held"
                );
                println!("handoff ok");
            }
        }
    }
}
