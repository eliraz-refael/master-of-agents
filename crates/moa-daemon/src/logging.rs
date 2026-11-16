//! The daemon's log: `tracing` lines appended to `logs/<brain>.log`, no colour.
//!
//! The no-contents rule (2a): a line names ids, methods and counts, never a record's text.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use tracing_subscriber::filter::LevelFilter;

/// The level `MOA_LOG` names (`error`, `warn`, `info`, `debug`, `trace`, `off`); `info`
/// when it is unset or not one of those.
fn level_from_env() -> LevelFilter {
    std::env::var("MOA_LOG")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(LevelFilter::INFO)
}

/// Starts logging to `path` at `MOA_LOG`'s level.
pub fn init(path: &Path) -> io::Result<()> {
    init_at(path, level_from_env())
}

/// Starts logging to `path` at `level`. A process has one global subscriber: when one is
/// already set (a second `run` in a test process), this leaves it alone.
#[doc(hidden)]
pub fn init_at(path: &Path, level: LevelFilter) -> io::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(level)
        .with_writer(Mutex::new(file))
        .try_init();
    Ok(())
}
