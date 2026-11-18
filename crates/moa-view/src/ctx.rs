//! The moment a frame is built for.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::UtcMs;

/// The clock and surroundings a view reads: the wall time, its local day, the animation tick,
/// the home folder and a check for folders.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub now: UtcMs,
    /// The date of `now` in `tz`.
    pub today: Date,
    pub tz: TimeZone,
    /// Monotonic milliseconds / 100: the animation frame.
    pub tick: u64,
    pub home: PathBuf,
    pub is_dir: fn(&Path) -> bool,
}

impl Ctx {
    /// The context at `now`, with `today` taken in `tz`.
    pub fn at(
        now: UtcMs,
        tz: TimeZone,
        tick: u64,
        home: PathBuf,
        is_dir: fn(&Path) -> bool,
    ) -> Ctx {
        let today = moa_core::Now::at(now, Duration::ZERO, tz.clone()).local_day;
        Ctx {
            now,
            today,
            tz,
            tick,
            home,
            is_dir,
        }
    }
}
