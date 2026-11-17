//! The moa daemon: the core task (group commit), the wall-clock scheduler, the supervisor
//! that restarts a panicking actor, the terminal link, and the process around them (lock,
//! socket, sessions).

pub mod clock;
pub mod core_task;
pub mod logging;
mod run;
pub mod scheduler;
mod server;
mod session;
pub mod supervise;
pub mod testing;
mod tmux;

pub use core_task::{CoreHandle, CoreOptions, DRAIN_CAP, DaemonError, Stats};
pub use moa_core::state::StatusData;
pub use run::{Ready, RunOptions, load_config, run};
pub use session::{peer_allowed, serve};
pub use tmux::{TmuxLink, tmux_env};
