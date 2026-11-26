//! moa's platform layer: paths, the build id, the counting spawner, a child's reaper, the
//! wall-clock alarm, the time-zone and path watches, process reads, and handing the daemon's
//! socket and lock to its next image.

pub mod alarm;
pub mod build_id;
pub mod child;
pub mod exe;
pub mod fs;
pub mod inherit;
pub mod net;
pub mod paths;
pub mod proc;
pub mod service;
pub mod spawn;
pub mod spool;
#[allow(unsafe_code)]
mod sys;
pub mod watch;
pub mod zone;

use std::sync::{Mutex, MutexGuard};

pub use alarm::{WallAlarm, WallTimer};
pub use build_id::{build_id, build_id_of, canonical, fnv1a64};
pub use exe::{exe_chain, invoked_exe};
pub use inherit::{Inherited, keep_open, reexec, take_inherited};
pub use paths::{Paths, PathsError, TmuxSocket, ensure_private_dir, sandbox_root, which};
pub use spawn::Spawner;
pub use sys::{
    current_uid, kill_group, kill_term, peer_uid, pid_alive, stdout_to_null, terminal_gone,
    terminal_gone_error, wait_exited, wait_hangup,
};
pub use watch::PathWatch;
pub use zone::ZoneWatch;

/// Locks a mutex, ignoring poisoning: the data is still there, and a panic elsewhere must not
/// take every later caller down with it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
