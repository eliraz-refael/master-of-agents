//! The only module in moa that may use `unsafe`: thin, safe wrappers over libc.

/// The calling process's real user id.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::getuid() }
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
