//! A one-shot alarm on the wall clock that also counts time spent asleep.

use std::future::Future;
use std::io;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::time::SystemTime;

use tokio::io::unix::AsyncFd;

use crate::sys;

/// A re-armable single deadline on the wall clock.
pub trait WallTimer: Send {
    /// Replace any earlier deadline. A deadline in the past fires at once.
    fn arm(&mut self, at: SystemTime) -> io::Result<()>;
    /// Cancel the deadline, if any.
    fn disarm(&mut self) -> io::Result<()>;
    /// Resolves once per expiry. Cancel-safe: a dropped future loses nothing.
    fn fired(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>>;
}

/// The real alarm: a kqueue timer on macOS, a timerfd on Linux.
#[derive(Debug)]
pub struct WallAlarm {
    fd: AsyncFd<OwnedFd>,
}

impl WallAlarm {
    /// Must be called inside a tokio runtime.
    pub fn new() -> io::Result<WallAlarm> {
        Ok(WallAlarm {
            fd: sys::register_readable(sys::alarm_create()?)?,
        })
    }
}

impl WallTimer for WallAlarm {
    fn arm(&mut self, at: SystemTime) -> io::Result<()> {
        sys::alarm_arm(self.fd.get_ref(), at)
    }

    fn disarm(&mut self) -> io::Result<()> {
        sys::alarm_disarm(self.fd.get_ref())
    }

    fn fired(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let mut guard = self.fd.readable().await?;
                // The drain follows the await with no yield between, so dropping the
                // future either leaves the expiry unread or consumes it and returns.
                let fired = sys::alarm_drain(self.fd.get_ref());
                guard.clear_ready();
                if fired? {
                    return Ok(());
                }
            }
        })
    }
}
