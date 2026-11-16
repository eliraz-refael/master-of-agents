//! moa's platform layer: paths, the build id, the counting spawner, the wall-clock alarm
//! and the time-zone watch.

pub mod alarm;
pub mod build_id;
pub mod paths;
pub mod service;
pub mod spawn;
#[allow(unsafe_code)]
mod sys;
pub mod zone;

pub use alarm::{WallAlarm, WallTimer};
pub use build_id::{build_id, build_id_of, canonical, fnv1a64};
pub use paths::{Paths, PathsError, ensure_private_dir};
pub use spawn::Spawner;
pub use sys::{current_uid, kill_term, pid_alive, stdout_to_null};
pub use zone::ZoneWatch;
