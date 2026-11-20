//! moa's platform layer: paths, the build id, the counting spawner, the wall-clock alarm,
//! the time-zone watch and process reads.

pub mod alarm;
pub mod build_id;
pub mod fs;
pub mod net;
pub mod paths;
pub mod proc;
pub mod service;
pub mod spawn;
pub mod spool;
#[allow(unsafe_code)]
mod sys;
pub mod zone;

pub use alarm::{WallAlarm, WallTimer};
pub use build_id::{build_id, build_id_of, canonical, fnv1a64};
pub use paths::{Paths, PathsError, TmuxSocket, ensure_private_dir, sandbox_root, which};
pub use spawn::Spawner;
pub use sys::{
    current_uid, kill_term, peer_uid, pid_alive, stdout_to_null, terminal_gone,
    terminal_gone_error, wait_exited, wait_hangup,
};
pub use zone::ZoneWatch;
