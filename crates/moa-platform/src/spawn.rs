//! Every product launch goes through [`Spawner`], which counts launches.

use std::io;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};

static LAUNCHES: AtomicU64 = AtomicU64::new(0);

/// Spawns child processes and counts every attempt.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spawner;

impl Spawner {
    /// Adds 1 to the process-wide counter *before* spawning, so a failed launch counts too.
    pub fn spawn(&self, cmd: &mut Command) -> io::Result<Child> {
        LAUNCHES.fetch_add(1, Ordering::SeqCst);
        cmd.spawn()
    }
}

/// How many launches this process has attempted.
pub fn launches() -> u64 {
    LAUNCHES.load(Ordering::SeqCst)
}
