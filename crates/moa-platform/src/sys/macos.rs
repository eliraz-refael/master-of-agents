//! macOS: a kqueue wall-clock timer and the timezone notification.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::SystemTime;

use super::{check, set_cloexec, set_cloexec_nonblock, since_epoch};

/// Not in `libc`: the timer's absolute deadline is on the continuous clock, which keeps
/// counting while the Mac sleeps.
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x80;
const TIMER_IDENT: libc::uintptr_t = 1;

/// `getpeereid`: the uid saved when the peer connected. (tokio's `peer_cred` also asks for
/// the peer's pid, `LOCAL_PEEREPID`, which fails once the peer has closed its end.)
pub(super) fn peer_uid_of(fd: RawFd) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `getpeereid` on a descriptor the caller keeps open, writing two integers
    // owned here.
    check(unsafe { libc::getpeereid(fd, &mut uid, &mut gid) })?;
    Ok(uid)
}

fn kevent_zeroed() -> libc::kevent {
    // SAFETY: `kevent` is a plain C struct of integers and a pointer; all-zero is valid.
    unsafe { std::mem::zeroed() }
}

/// A new, empty kqueue, close-on-exec. A kqueue rejects O_NONBLOCK: whether a `kevent`
/// on it waits is up to that call's timeout.
fn new_kqueue() -> io::Result<OwnedFd> {
    // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
    let raw = check(unsafe { libc::kqueue() })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    set_cloexec(&fd)?;
    Ok(fd)
}

/// A kqueue holding no timer yet, close-on-exec. Every `kevent` on the alarm's kqueue uses
/// a zero timeout, so the alarm never blocks.
pub fn alarm_create() -> io::Result<OwnedFd> {
    new_kqueue()
}

/// Blocks until the terminal on `fd` hangs up (its other end closed). A kqueue read
/// filter cleared on every delivery (`EV_CLEAR`) wakes once per new input and stays
/// asleep while input waits unread, so this never spins; the hang-up is its `EV_EOF`.
/// macOS's `poll` reports a hang-up only to a poll for input, which would spin there.
pub fn wait_hangup(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
    // Its own kqueue, which this call blocks on (unlike the alarm's).
    let kq = new_kqueue()?;
    let mut ev = kevent_zeroed();
    ev.ident = fd.as_raw_fd() as libc::uintptr_t;
    ev.filter = libc::EVFILT_READ;
    ev.flags = libc::EV_ADD | libc::EV_CLEAR;
    change(&kq, &ev)?;
    loop {
        let mut got = kevent_zeroed();
        // SAFETY: no change list, room for one event, no timeout: it waits for one.
        let r = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut got,
                1,
                std::ptr::null(),
            )
        };
        match check(r) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
            Ok(_) if got.flags & libc::EV_ERROR != 0 => {
                return Err(io::Error::from_raw_os_error(got.data as i32));
            }
            Ok(_) if got.flags & libc::EV_EOF != 0 => return Ok(()),
            Ok(_) => {}
        }
    }
}

fn change(fd: &OwnedFd, ev: &libc::kevent) -> io::Result<()> {
    // SAFETY: one valid change record, no event list, on an open kqueue.
    let r = unsafe {
        libc::kevent(
            fd.as_raw_fd(),
            ev,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    check(r).map(|_| ())
}

/// Replace the timer with one that fires once at the wall-clock instant `at`.
pub fn alarm_arm(fd: &OwnedFd, at: SystemTime) -> io::Result<()> {
    let mut ev = kevent_zeroed();
    ev.ident = TIMER_IDENT;
    ev.filter = libc::EVFILT_TIMER;
    ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
    ev.fflags = libc::NOTE_ABSOLUTE | NOTE_MACH_CONTINUOUS_TIME | libc::NOTE_USECONDS;
    // The kernel turns microseconds into nanoseconds and refuses a product past i64, so a
    // deadline past 2262 is clamped to the latest one it accepts.
    ev.data = since_epoch(at).as_micros().min((i64::MAX / 1000) as u128) as isize;
    change(fd, &ev)
}

/// Remove the timer; it is fine if it already fired and went away.
pub fn alarm_disarm(fd: &OwnedFd) -> io::Result<()> {
    let mut ev = kevent_zeroed();
    ev.ident = TIMER_IDENT;
    ev.filter = libc::EVFILT_TIMER;
    ev.flags = libc::EV_DELETE;
    match change(fd, &ev) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        other => other,
    }
}

/// Consume a pending expiry: true if the timer had fired.
pub fn alarm_drain(fd: &OwnedFd) -> io::Result<bool> {
    let mut ev = kevent_zeroed();
    let zero = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: no change list, room for one event, a zero timeout so it never blocks.
    let r = unsafe { libc::kevent(fd.as_raw_fd(), std::ptr::null(), 0, &mut ev, 1, &zero) };
    let n = check(r)?;
    Ok(n > 0 && ev.filter == libc::EVFILT_TIMER && ev.flags & libc::EV_ERROR == 0)
}

unsafe extern "C" {
    fn notify_register_file_descriptor(
        name: *const libc::c_char,
        notify_fd: *mut libc::c_int,
        flags: libc::c_int,
        out_token: *mut libc::c_int,
    ) -> u32;
    fn notify_cancel(token: libc::c_int) -> u32;
    fn notify_post(name: *const libc::c_char) -> u32;
}

const NOTIFY_STATUS_OK: u32 = 0;

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// A notify(3) registration delivering one 4-byte token per post on a pipe.
/// Dropping it cancels the registration, which closes the pipe.
#[derive(Debug)]
pub struct NotifyFd {
    fd: RawFd,
    token: libc::c_int,
}

impl AsRawFd for NotifyFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for NotifyFd {
    fn drop(&mut self) {
        // SAFETY: `token` came from a successful registration and is cancelled once;
        // cancelling closes the descriptor that notify created for it.
        unsafe { notify_cancel(self.token) };
    }
}

/// Register for the named notification.
pub fn notify_open(name: &str) -> io::Result<NotifyFd> {
    let name = c_name(name)?;
    let (mut fd, mut token) = (-1, 0);
    // SAFETY: `name` is a valid C string; both out-pointers point at live ints.
    let status = unsafe { notify_register_file_descriptor(name.as_ptr(), &mut fd, 0, &mut token) };
    if status != NOTIFY_STATUS_OK {
        return Err(io::Error::other(format!(
            "notify_register_file_descriptor failed: {status}"
        )));
    }
    // From here the guard cancels the registration on any early return.
    let n = NotifyFd { fd, token };
    set_cloexec_nonblock(&n)?;
    Ok(n)
}

/// Read every pending token: true if there was at least one.
pub fn notify_drain(n: &NotifyFd) -> io::Result<bool> {
    let mut any = false;
    loop {
        let mut buf = [0u8; 4];
        // SAFETY: reads at most 4 bytes into a 4-byte buffer from an open descriptor.
        let r = unsafe { libc::read(n.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if r > 0 {
            any = true;
        } else if r == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "notify pipe closed",
            ));
        } else {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::WouldBlock => return Ok(any),
                io::ErrorKind::Interrupted => {}
                _ => return Err(e),
            }
        }
    }
}

/// Post the named notification.
pub fn notify_post_name(name: &str) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: `name` is a valid C string.
    let status = unsafe { notify_post(name.as_ptr()) };
    if status == NOTIFY_STATUS_OK {
        Ok(())
    } else {
        Err(io::Error::other(format!("notify_post failed: {status}")))
    }
}

/// A pid libproc and `sysctl` may be asked about: zero (the kernel) and pids past `i32` are
/// never read as a process.
fn proc_pid(pid: u32) -> Option<libc::pid_t> {
    super::signalable(pid)
}

/// `KERN_ARGMAX`: the most bytes a process's arguments and environment can take, so the
/// size of a buffer `KERN_PROCARGS2` always fits in. 1 MiB if the kernel won't say.
fn arg_max() -> usize {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut max: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: a two-entry name, and `max` is a live `c_int` whose size is passed in `size`;
    // `sysctl` writes at most that many bytes. No new value is set.
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            (&raw mut max).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    match usize::try_from(max) {
        Ok(max) if ret == 0 && max > 0 => max,
        _ => 1 << 20,
    }
}

/// `pid`'s argv and kept environment through `sysctl([CTL_KERN, KERN_PROCARGS2, pid])`,
/// read into `buf` (sized from `KERN_ARGMAX` on first use). `None` when the read fails: the
/// process is gone, or the kernel refuses it (another user's, a zombie); the caller tells
/// which.
pub(crate) fn read_args(pid: u32, buf: &mut Vec<u8>) -> Option<crate::proc::ProcArgs> {
    let pid = proc_pid(pid)?;
    if buf.is_empty() {
        buf.resize(arg_max(), 0);
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = buf.len();
    // SAFETY: a three-entry name, and `buf` is live and writable for `size` bytes, which
    // `sysctl` never writes past. No new value is set.
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }
    crate::proc::parse_procargs2(&buf[..size.min(buf.len())])
}

/// `pid`'s direct children through `proc_listchildpids`, which answers a **count of pids**,
/// not bytes. A buffer that comes back full may have cut the list short, so it grows once.
/// libproc answers any pid by looking for processes with that parent, so a gone one has no
/// children rather than a failure; `None` when the pid can't be a process or libproc fails.
pub(crate) fn child_pids(pid: u32) -> Option<Vec<u32>> {
    const ROOM: usize = 64;
    let pid = proc_pid(pid)?;
    let mut pids = list_children(pid, ROOM)?;
    if pids.len() == ROOM {
        pids = list_children(pid, ROOM * 16)?;
    }
    Some(
        pids.into_iter()
            .filter_map(|p| u32::try_from(p).ok().filter(|&p| p > 0))
            .collect(),
    )
}

/// Up to `room` of `pid`'s children; `None` when libproc fails.
fn list_children(pid: libc::pid_t, room: usize) -> Option<Vec<libc::pid_t>> {
    let mut pids: Vec<libc::pid_t> = vec![0; room];
    let bytes =
        libc::c_int::try_from(room * std::mem::size_of::<libc::pid_t>()).expect("a small buffer");
    // SAFETY: `pids` is live and writable for `bytes` bytes, and libproc writes at most
    // that many.
    let n = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(n).ok()?.min(room));
    Some(pids)
}

/// When `pid` started: `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec` and
/// `pbi_start_tvusec`.
pub(crate) fn start_time(pid: u32) -> Option<SystemTime> {
    let info = bsd_info(pid)?;
    let at = std::time::Duration::from_secs(info.pbi_start_tvsec)
        + std::time::Duration::from_micros(info.pbi_start_tvusec);
    std::time::UNIX_EPOCH.checked_add(at)
}

/// `pid`'s name as tmux reads it (`osdep-darwin.c`): `proc_pidinfo(PROC_PIDTBSDINFO)`'s
/// `pbi_name`, or its `pbi_comm` when that is empty.
pub(crate) fn proc_name(pid: u32) -> Option<String> {
    let info = bsd_info(pid)?;
    let text = |chars: &[libc::c_char]| {
        let bytes: Vec<u8> = chars
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let name = text(&info.pbi_name);
    Some(if name.is_empty() {
        text(&info.pbi_comm)
    } else {
        name
    })
}

/// `pid`'s real and effective user ids, from `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)`, which
/// answers for any user's process, the system's own daemons among them (the full
/// `PROC_PIDTBSDINFO` is refused for many of those).
pub(crate) fn uids(pid: u32) -> Option<(u32, u32)> {
    let pid = proc_pid(pid)?;
    // SAFETY: `proc_bsdshortinfo` is a plain C struct of integers and byte arrays; all-zero
    // is valid.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: `info` is live and writable for `size` bytes, and libproc writes at most that
    // many.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    (n == size).then_some((info.pbsi_ruid, info.pbsi_uid))
}

/// `pid`'s `proc_bsdinfo`, or `None` when libproc doesn't answer it whole.
fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = proc_pid(pid)?;
    // SAFETY: `proc_bsdinfo` is a plain C struct of integers and byte arrays; all-zero is
    // valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is live and writable for `size` bytes, and libproc writes at most that
    // many.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(info)
}
