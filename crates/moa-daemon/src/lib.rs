//! The moa daemon's engine: the core task (group commit), the wall-clock scheduler and the
//! supervisor that restarts a panicking actor.

pub mod clock;
pub mod core_task;
pub mod scheduler;
pub mod supervise;
pub mod testing;

pub use core_task::{CoreHandle, CoreOptions, DRAIN_CAP, DaemonError, Stats};
pub use moa_core::state::StatusData;
