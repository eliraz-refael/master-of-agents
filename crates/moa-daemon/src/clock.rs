//! The daemon's two clocks: wall time (it can step) and a monotonic reading.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moa_core::UtcMs;

pub trait Clock: Send + Sync + 'static {
    fn wall(&self) -> UtcMs;
    fn mono(&self) -> Duration;
}

/// The real clocks. `mono` counts from when this was made.
#[derive(Debug)]
pub struct SystemClock {
    started: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            started: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall(&self) -> UtcMs {
        let ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
        };
        UtcMs(ms)
    }

    fn mono(&self) -> Duration {
        self.started.elapsed()
    }
}

/// A [`UtcMs`] as a `SystemTime`, for arming a `WallTimer`.
pub fn system_time(at: UtcMs) -> SystemTime {
    let ms = at.0.unsigned_abs();
    if at.0 >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms)
    }
}
