//! A clock and an alarm tests move by hand. Small, so it is always compiled: other crates'
//! tests (the daemon's, the client's) use it too.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use moa_core::UtcMs;
use moa_platform::WallTimer;
use tokio::sync::Notify;

use crate::clock::{Clock, utc_ms};

struct Inner {
    wall: i64,
    mono: Duration,
    armed: Option<i64>,
}

/// Wall time that moves only when [`FakeTime::advance`] says so. Share it as the daemon's
/// `Clock`, and give the daemon [`FakeTime::alarm`] as its `WallTimer`.
pub struct FakeTime {
    inner: Mutex<Inner>,
    changed: Notify,
}

impl FakeTime {
    pub fn new(at: UtcMs) -> Arc<FakeTime> {
        Arc::new(FakeTime {
            inner: Mutex::new(Inner {
                wall: at.0,
                mono: Duration::ZERO,
                armed: None,
            }),
            changed: Notify::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves wall and monotonic time on; an armed alarm whose deadline is now past fires.
    pub fn advance(&self, by: Duration) {
        {
            let mut i = self.lock();
            i.wall = i
                .wall
                .saturating_add(i64::try_from(by.as_millis()).unwrap_or(i64::MAX));
            i.mono += by;
        }
        self.changed.notify_one();
    }

    pub fn alarm(self: &Arc<Self>) -> FakeAlarm {
        FakeAlarm { time: self.clone() }
    }

    /// The deadline the alarm is armed for, if any.
    pub fn armed(&self) -> Option<UtcMs> {
        self.lock().armed.map(UtcMs)
    }
}

impl Clock for FakeTime {
    fn wall(&self) -> UtcMs {
        UtcMs(self.lock().wall)
    }

    fn mono(&self) -> Duration {
        self.lock().mono
    }
}

/// The `WallTimer` of a [`FakeTime`].
pub struct FakeAlarm {
    time: Arc<FakeTime>,
}

impl WallTimer for FakeAlarm {
    fn arm(&mut self, at: SystemTime) -> io::Result<()> {
        self.time.lock().armed = Some(utc_ms(at).0);
        // A deadline already past fires at once.
        self.time.changed.notify_one();
        Ok(())
    }

    fn disarm(&mut self) -> io::Result<()> {
        self.time.lock().armed = None;
        Ok(())
    }

    fn fired(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async move {
            loop {
                {
                    let mut i = self.time.lock();
                    if i.armed.is_some_and(|at| i.wall >= at) {
                        i.armed = None;
                        return Ok(());
                    }
                }
                // `notify_one` stores a permit, so a change between the check and here
                // is not lost.
                self.time.changed.notified().await;
            }
        })
    }
}
