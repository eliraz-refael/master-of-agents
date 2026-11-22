//! The only module in moa that may use `unsafe`: thin, safe wrappers over libc.

/// The calling process's real user id.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::getuid() }
}

/// The user id of the process at the other end of a Unix stream socket. It still answers
/// once that process has closed its end and gone, as a hook does right after its line.
pub fn peer_uid(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<u32> {
    peer_uid_of(std::os::fd::AsRawFd::as_raw_fd(&fd))
}

/// A pid `kill` may be given. Zero and negative pids address whole process groups, so a
/// `u32` that wraps there is refused rather than passed on.
fn signalable(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|&p| p > 0)
}

/// Whether a process with this pid exists (`kill(pid, 0)`). `EPERM` means it exists but
/// isn't ours, which counts as alive. A zombie counts as alive until its parent reaps it.
pub fn pid_alive(pid: u32) -> bool {
    let Some(pid) = signalable(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that `pid` can be signalled; nothing is delivered.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Sends SIGTERM to `pid`. Errors are ignored: the process may be gone already.
pub fn kill_term(pid: u32) {
    if let Some(pid) = signalable(pid) {
        // SAFETY: `kill` takes two integers and touches no memory.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Sends `signal` to every process in the process group `pgid`. Errors are ignored: the group
/// may be gone already.
pub fn kill_group(pgid: u32, signal: libc::c_int) {
    if let Some(pgid) = signalable(pgid) {
        // SAFETY: `killpg` takes two integers and touches no memory.
        unsafe { libc::killpg(pgid, signal) };
    }
}

/// Blocks until this process's child `pid` has exited, and leaves it unreaped: a zombie
/// whose pid can't be given to another process until something reaps it (`WNOWAIT`).
pub fn wait_exited(pid: u32) -> std::io::Result<()> {
    loop {
        // SAFETY: a zeroed `siginfo_t` is a valid one (plain integers and a union of them),
        // and `waitid` only writes into it for the length of the call.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, writable `siginfo_t` for the whole call.
        let ret =
            unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        match check(ret) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Points file descriptor 1 at `/dev/null`, so nothing the daemon prints later reaches the
/// pipe its starter read `ready` from.
pub fn stdout_to_null() -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: `dup2` of two descriptors that are open here: descriptor 1 is replaced, and
    // `null` closes its own copy when it drops.
    check(unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) })?;
    Ok(())
}

/// Whether the terminal on `fd` has gone (its other end closed): `poll`, which doesn't
/// wait, says it hung up or failed. Unread input alone isn't that.
pub fn terminal_gone(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    let mut p = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(&fd),
        // macOS reports a hang-up only to a poll for input.
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live `pollfd` for the whole call, and a zero timeout.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    n > 0 && p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Whether `e`, from writing to or asking a terminal, is how only a terminal that has gone
/// fails: EIO (a tty whose other end closed), ENXIO (no such device any more) or EPIPE.
pub fn terminal_gone_error(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EIO | libc::ENXIO | libc::EPIPE)
    )
}

/// A copy of `fd` that a child process inherits: `dup` leaves close-on-exec off on the copy.
pub(crate) fn dup_inheritable(
    fd: std::os::fd::BorrowedFd<'_>,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: `dup` of a descriptor the borrow keeps open.
    let raw = check(unsafe { libc::dup(fd.as_raw_fd()) })?;
    // SAFETY: `raw` is a new open descriptor that nothing else owns, so the `OwnedFd`
    // closes it exactly once.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("moa-platform supports only macOS and Linux");

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;

use std::io;
#[cfg(target_os = "macos")]
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Time since the Unix epoch, clamped to zero for instants before it.
pub(crate) fn since_epoch(at: SystemTime) -> Duration {
    at.duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// Turn a negative libc return into the last OS error.
pub(crate) fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Mark `fd` close-on-exec.
#[cfg(target_os = "macos")]
pub(crate) fn set_cloexec(fd: &impl AsRawFd) -> io::Result<()> {
    let raw: RawFd = fd.as_raw_fd();
    // SAFETY: `fcntl` with F_GETFD on a descriptor the caller holds open.
    let fdflags = check(unsafe { libc::fcntl(raw, libc::F_GETFD) })?;
    // SAFETY: `fcntl` with F_SETFD and a flag int, on the same open descriptor.
    check(unsafe { libc::fcntl(raw, libc::F_SETFD, fdflags | libc::FD_CLOEXEC) })?;
    Ok(())
}

/// Mark `fd` close-on-exec and non-blocking.
#[cfg(target_os = "macos")]
pub(crate) fn set_cloexec_nonblock(fd: &impl AsRawFd) -> io::Result<()> {
    set_cloexec(fd)?;
    let raw: RawFd = fd.as_raw_fd();
    // SAFETY: `fcntl` with F_GETFL on the same open descriptor.
    let flflags = check(unsafe { libc::fcntl(raw, libc::F_GETFL) })?;
    // SAFETY: `fcntl` with F_SETFL and a flag int, on the same open descriptor.
    check(unsafe { libc::fcntl(raw, libc::F_SETFL, flflags | libc::O_NONBLOCK) })?;
    Ok(())
}

/// Register `inner` with the tokio reactor for read readiness only. A kqueue has no
/// write filter, so the default read-write interest would fail on one.
/// Must be called inside a tokio runtime.
pub(crate) fn register_readable<T: std::os::fd::AsRawFd>(
    inner: T,
) -> io::Result<tokio::io::unix::AsyncFd<T>> {
    // SAFETY: every caller passes an owner of an open descriptor that keeps it open and
    // unchanged until it is dropped (an `OwnedFd`, or a notify registration that closes
    // its descriptor only in `Drop`).
    unsafe {
        tokio::io::unix::AsyncFd::register_with_interest(inner, tokio::io::Interest::READABLE)
    }
    .map_err(|e| e.into_parts().1)
}

#[cfg(test)]
mod tests {
    use super::signalable;

    #[test]
    fn only_positive_pids_that_fit_are_signalable() {
        assert_eq!(signalable(0), None);
        assert_eq!(signalable(u32::MAX), None);
        assert_eq!(signalable(i32::MAX as u32 + 1), None);
        assert_eq!(signalable(1), Some(1));
        assert_eq!(signalable(i32::MAX as u32), Some(i32::MAX));
    }
}
