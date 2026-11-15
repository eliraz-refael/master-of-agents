//! Linux: a CLOCK_REALTIME timerfd and an inotify watch on a directory.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{check, since_epoch};

/// A disarmed timerfd on the wall clock, non-blocking and close-on-exec.
pub fn alarm_create() -> io::Result<OwnedFd> {
    // SAFETY: `timerfd_create` takes only constants and returns a descriptor or -1.
    let raw = check(unsafe {
        libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC)
    })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn settime(fd: &OwnedFd, value: Duration, flags: libc::c_int) -> io::Result<()> {
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: value.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
            tv_nsec: value.subsec_nanos() as _,
        },
    };
    // SAFETY: `spec` is a valid itimerspec; the old-value pointer may be null.
    let r = unsafe { libc::timerfd_settime(fd.as_raw_fd(), flags, &spec, std::ptr::null_mut()) };
    check(r).map(|_| ())
}

/// Replace the deadline with the wall-clock instant `at`. A zero value would disarm the
/// timer, so an instant at or before the epoch becomes one nanosecond, which is long past.
pub fn alarm_arm(fd: &OwnedFd, at: SystemTime) -> io::Result<()> {
    let d = since_epoch(at).max(Duration::from_nanos(1));
    settime(fd, d, libc::TFD_TIMER_ABSTIME)
}

/// Stop the timer and forget any expiry not yet read.
pub fn alarm_disarm(fd: &OwnedFd) -> io::Result<()> {
    settime(fd, Duration::ZERO, 0)
}

/// Read the expiry count to clear it: true if the timer had fired.
pub fn alarm_drain(fd: &OwnedFd) -> io::Result<bool> {
    let mut count = [0u8; 8];
    // SAFETY: reads at most 8 bytes into an 8-byte buffer from an open descriptor.
    let r = unsafe { libc::read(fd.as_raw_fd(), count.as_mut_ptr().cast(), count.len()) };
    if r == count.len() as isize {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(false),
        _ => Err(e),
    }
}

/// An inotify instance watching one directory for changes to its entries.
pub fn notify_open(dir: &Path) -> io::Result<OwnedFd> {
    // SAFETY: `inotify_init1` takes only constants and returns a descriptor or -1.
    let raw = check(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mask = libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_ATTRIB;
    // SAFETY: `path` is a valid C string and `fd` is an open inotify descriptor.
    check(unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) })?;
    Ok(fd)
}

/// Read every pending event: true if any of them names `name`.
pub fn notify_drain(fd: &OwnedFd, name: &OsStr) -> io::Result<bool> {
    let mut hit = false;
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: reads at most `buf.len()` bytes into `buf` from an open descriptor.
        let r = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return match e.kind() {
                io::ErrorKind::WouldBlock => Ok(hit),
                io::ErrorKind::Interrupted => continue,
                _ => Err(e),
            };
        }
        if r == 0 {
            return Ok(hit);
        }
        hit |= names_in(&buf[..r as usize], name);
    }
}

/// Whether any inotify record in `buf` carries the entry name `name`.
fn names_in(buf: &[u8], name: &OsStr) -> bool {
    const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
    let mut off = 0;
    while off + HEADER <= buf.len() {
        // `len` is the 4-byte field after wd, mask and cookie.
        let len = u32::from_ne_bytes(buf[off + 12..off + 16].try_into().expect("4 bytes")) as usize;
        let start = off + HEADER;
        let end = (start + len).min(buf.len());
        let raw = &buf[start..end];
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if OsStr::from_bytes(&raw[..nul]) == name {
            return true;
        }
        off = start + len;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, pad: usize) -> Vec<u8> {
        let mut v = vec![0u8; 12];
        let len = (name.len() + pad) as u32;
        v.extend_from_slice(&len.to_ne_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend(std::iter::repeat_n(0u8, pad));
        v
    }

    #[test]
    fn names_in_matches_only_the_named_entry() {
        let mut buf = record("other", 3);
        assert!(!names_in(&buf, OsStr::new("localtime")));
        buf.extend(record("localtime", 7));
        assert!(names_in(&buf, OsStr::new("localtime")));
    }
}
