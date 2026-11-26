//! Linux: a CLOCK_REALTIME timerfd and inotify watches on directories.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{check, since_epoch};

/// `SO_PEERCRED`: the credentials saved when the peer connected.
pub(super) fn peer_uid_of(fd: RawFd) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `getsockopt` writes at most `len` bytes into `cred`, which is owned here and
    // that big, on a descriptor the caller keeps open.
    check(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    })?;
    Ok(cred.uid)
}

/// A disarmed timerfd on the wall clock, non-blocking and close-on-exec.
pub fn alarm_create() -> io::Result<OwnedFd> {
    // SAFETY: `timerfd_create` takes only constants and returns a descriptor or -1.
    let raw = check(unsafe {
        libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC)
    })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Blocks until the terminal on `fd` hangs up (its other end closed). A `poll` that asks
/// for nothing still hears a hang-up, an error or a closed descriptor, and only those, so
/// input never wakes it and it never spins.
pub fn wait_hangup(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    loop {
        // SAFETY: one live `pollfd` for the whole call; no timeout.
        match check(unsafe { libc::poll(&mut p, 1, -1) }) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
            Ok(_) if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 => {
                return Ok(());
            }
            Ok(_) => {}
        }
    }
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

/// Read the expiry count to clear it: true if the timer had fired. An interrupted read is
/// retried: the caller clears readiness next, and an edge-triggered reactor would never
/// report the unread expiry again.
pub fn alarm_drain(fd: &OwnedFd) -> io::Result<bool> {
    let mut count = [0u8; 8];
    loop {
        // SAFETY: reads at most 8 bytes into an 8-byte buffer from an open descriptor.
        let r = unsafe { libc::read(fd.as_raw_fd(), count.as_mut_ptr().cast(), count.len()) };
        if r == count.len() as isize {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => return Ok(false),
            _ => return Err(e),
        }
    }
}

/// An empty inotify instance, non-blocking and close-on-exec.
pub fn inotify_new() -> io::Result<OwnedFd> {
    // SAFETY: `inotify_init1` takes only constants and returns a descriptor or -1.
    let raw = check(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Watch directory `dir` on `fd` for its entries being created, written, renamed in,
/// removed or changed in their attributes, and for itself being removed or moved. Answers
/// the watch descriptor its events carry; watching one directory twice answers the same
/// one.
pub fn inotify_watch(fd: &OwnedFd, dir: &Path) -> io::Result<i32> {
    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mask = libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_ATTRIB
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;
    // SAFETY: `path` is a valid C string and `fd` is an open inotify descriptor.
    check(unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) })
}

/// Read every pending event: true if `hit(watch descriptor, directory gone, entry name)`
/// says yes to any, or the kernel's queue overflowed (events were lost, so any of them
/// might have been one). "Directory gone" is the watched directory itself removed, moved
/// or no longer watched; such an event names no entry.
pub fn inotify_drain(
    fd: &OwnedFd,
    mut hit: impl FnMut(i32, bool, &OsStr) -> bool,
) -> io::Result<bool> {
    let mut any = false;
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: reads at most `buf.len()` bytes into `buf` from an open descriptor.
        let r = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return match e.kind() {
                io::ErrorKind::WouldBlock => Ok(any),
                io::ErrorKind::Interrupted => continue,
                _ => Err(e),
            };
        }
        if r == 0 {
            return Ok(any);
        }
        any |= any_record(&buf[..r as usize], &mut hit);
    }
}

/// Whether `hit` says yes to any inotify record in `buf`, given its watch descriptor,
/// whether it is about the watched directory itself, and its entry name. An overflow
/// record (watch descriptor -1) always counts.
fn any_record(buf: &[u8], hit: &mut impl FnMut(i32, bool, &OsStr) -> bool) -> bool {
    const DIR_GONE: u32 = libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED;
    const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
    let mut off = 0;
    while off + HEADER <= buf.len() {
        let field = |at: usize| buf[off + at..off + at + 4].try_into().expect("4 bytes");
        // The 4-byte fields: wd, mask, cookie, then the name's length.
        let wd = i32::from_ne_bytes(field(0));
        let mask = u32::from_ne_bytes(field(4));
        let len = u32::from_ne_bytes(field(12)) as usize;
        let start = off + HEADER;
        let end = (start + len).min(buf.len());
        let raw = &buf[start..end];
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if wd < 0 || hit(wd, mask & DIR_GONE != 0, OsStr::from_bytes(&raw[..nul])) {
            return true;
        }
        off = start + len;
    }
    false
}

/// A pid `/proc` may be asked about: zero and pids past `i32` are never read as a process.
fn proc_pid(pid: u32) -> Option<u32> {
    super::signalable(pid).map(|_| pid)
}

/// Appends the whole of `path` to `buf`, and answers how many bytes it added.
fn read_into(path: &str, buf: &mut Vec<u8>) -> Option<usize> {
    use std::io::Read as _;
    let before = buf.len();
    std::fs::File::open(path).ok()?.read_to_end(buf).ok()?;
    Some(buf.len() - before)
}

/// `pid`'s argv and kept environment from `/proc/<pid>/cmdline` and `/proc/<pid>/environ`,
/// both read into `buf`. `None` when either can't be read: the process is gone, or it is
/// another user's (its `environ` is closed to us); the caller tells which.
pub(crate) fn read_args(pid: u32, buf: &mut Vec<u8>) -> Option<crate::proc::ProcArgs> {
    let pid = proc_pid(pid)?;
    buf.clear();
    let argv = read_into(&format!("/proc/{pid}/cmdline"), buf)?;
    read_into(&format!("/proc/{pid}/environ"), buf)?;
    let (cmdline, environ) = buf.split_at(argv);
    Some(crate::proc::parse_proc_files(cmdline, environ))
}

/// `pid`'s direct children from each of its threads' `children` file. A kernel built
/// without those files gets a scan of every process's `stat` for the parent pid instead.
/// `None` when the process's threads, or `/proc` itself, can't be listed.
pub(crate) fn child_pids(pid: u32) -> Option<Vec<u32>> {
    let pid = proc_pid(pid)?;
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    let mut kids = Vec::new();
    let mut listed = false;
    for task in tasks.flatten() {
        if let Ok(text) = std::fs::read_to_string(task.path().join("children")) {
            listed = true;
            kids.extend(crate::proc::pid_list(&text));
        }
    }
    if !listed {
        kids = scan_children(pid)?;
    }
    kids.sort_unstable();
    kids.dedup();
    Some(kids)
}

/// Every process whose `/proc/<pid>/stat` names `parent` as its parent; `None` when `/proc`
/// can't be listed. A process that exits mid-scan is left out.
fn scan_children(parent: u32) -> Option<Vec<u32>> {
    let all = std::fs::read_dir("/proc").ok()?;
    let kids = all
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|kid| {
            std::fs::read_to_string(format!("/proc/{kid}/stat"))
                .ok()
                .and_then(|s| crate::proc::stat_ppid(&s))
                == Some(parent)
        })
        .collect();
    Some(kids)
}

/// `pid`'s name as tmux reads it (`osdep-linux.c`): its argv0, the first string of
/// `/proc/<pid>/cmdline`.
pub(crate) fn proc_name(pid: u32) -> Option<String> {
    let pid = proc_pid(pid)?;
    let mut buf = Vec::new();
    read_into(&format!("/proc/{pid}/cmdline"), &mut buf)?;
    crate::proc::strings(&buf).next()
}

/// `pid`'s real and effective user ids, from `/proc/<pid>/status`, which any user may read.
pub(crate) fn uids(pid: u32) -> Option<(u32, u32)> {
    let pid = proc_pid(pid)?;
    crate::proc::status_uids(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// Whether `/proc/<pid>/stat` says `pid` has exited (`Z`) or is dead (`X`); `false` when
/// it can't be read.
pub(crate) fn zombie(pid: u32) -> bool {
    proc_pid(pid)
        .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/stat")).ok())
        .is_some_and(|stat| matches!(crate::proc::stat_state(&stat), Some('Z' | 'X')))
}

/// When `pid` started: field 22 of `/proc/<pid>/stat`, in clock ticks since boot, plus the
/// boot time (`btime` in `/proc/stat`).
pub(crate) fn start_time(pid: u32) -> Option<SystemTime> {
    let pid = proc_pid(pid)?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = crate::proc::stat_start_ticks(&stat)?;
    let boot = crate::proc::boot_time(&std::fs::read_to_string("/proc/stat").ok()?)?;
    // SAFETY: `sysconf` takes one integer and touches no memory.
    let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|&hz| hz > 0)?;
    let since_boot =
        Duration::from_secs(ticks / hz) + Duration::from_nanos((ticks % hz) * 1_000_000_000 / hz);
    std::time::UNIX_EPOCH.checked_add(Duration::from_secs(boot) + since_boot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(wd: i32, mask: u32, name: &str, pad: usize) -> Vec<u8> {
        let mut v = wd.to_ne_bytes().to_vec();
        v.extend_from_slice(&mask.to_ne_bytes());
        v.extend_from_slice(&[0u8; 4]);
        let len = (name.len() + pad) as u32;
        v.extend_from_slice(&len.to_ne_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend(std::iter::repeat_n(0u8, pad));
        v
    }

    /// What the Linux reads answer for a pid that is no process and for this one; the
    /// gone-or-failed choice on top of them is `proc`'s, and CI runs this.
    #[test]
    fn proc_reads_fail_for_no_process_and_answer_for_this_one() {
        let mut buf = Vec::new();
        assert!(read_args(i32::MAX as u32, &mut buf).is_none());
        assert!(child_pids(i32::MAX as u32).is_none());
        assert!(child_pids(0).is_none());
        let me = std::process::id();
        assert!(read_args(me, &mut buf).is_some_and(|a| !a.argv.is_empty()));
        assert!(child_pids(me).is_some());
        assert!(scan_children(me).is_some());
        assert_eq!(
            uids(me),
            Some((super::super::current_uid(), super::super::current_uid()))
        );
        assert!(uids(i32::MAX as u32).is_none());
    }

    #[test]
    fn any_record_matches_only_the_named_entry_of_its_watch() {
        let mut ours =
            |wd: i32, dir_gone: bool, name: &OsStr| wd == 1 && (dir_gone || name == "localtime");
        let mut buf = record(1, libc::IN_CREATE, "other", 3);
        buf.extend(record(2, libc::IN_MOVED_TO, "localtime", 7));
        // Another watch's directory going is not ours.
        buf.extend(record(2, libc::IN_DELETE_SELF, "", 0));
        assert!(!any_record(&buf, &mut ours));
        buf.extend(record(1, libc::IN_MOVED_TO, "localtime", 7));
        assert!(any_record(&buf, &mut ours));
        assert!(
            any_record(&record(-1, libc::IN_Q_OVERFLOW, "", 0), &mut ours),
            "an overflow counts"
        );
        for gone in [libc::IN_DELETE_SELF, libc::IN_MOVE_SELF, libc::IN_IGNORED] {
            assert!(
                any_record(&record(1, gone, "", 0), &mut ours),
                "the watched directory going counts"
            );
        }
    }
}
