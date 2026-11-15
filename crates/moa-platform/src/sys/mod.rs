//! The only module in moa that may use `unsafe`: thin, safe wrappers over libc.

/// The calling process's real user id.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::getuid() }
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
