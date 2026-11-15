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

fn kevent_zeroed() -> libc::kevent {
    // SAFETY: `kevent` is a plain C struct of integers and a pointer; all-zero is valid.
    unsafe { std::mem::zeroed() }
}

/// A kqueue holding no timer yet, close-on-exec. (A kqueue rejects O_NONBLOCK, and
/// every `kevent` on it uses a zero timeout, so it never blocks anyway.)
pub fn alarm_create() -> io::Result<OwnedFd> {
    // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
    let raw = check(unsafe { libc::kqueue() })?;
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    set_cloexec(&fd)?;
    Ok(fd)
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
