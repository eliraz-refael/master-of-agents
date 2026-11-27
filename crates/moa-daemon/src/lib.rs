//! The moa daemon: the core task (group commit), the wall-clock scheduler, the supervisor
//! that restarts a panicking actor, the terminal link, the link to moa's agent, and the
//! process around them (lock, socket, sessions).

mod acp;
pub mod clock;
mod config_watch;
pub mod core_task;
mod follow;
pub mod inspect;
pub mod logging;
mod run;
pub mod scheduler;
mod server;
mod session;
mod settled;
pub mod supervise;
pub mod testing;
mod tmux;

pub use core_task::{CoreHandle, CoreOptions, DRAIN_CAP, DaemonError, Stats};
pub use moa_core::state::StatusData;
pub use run::{Exec, ProbeWait, Ready, RunOptions, daemon_exe, load_config, run};
pub use session::{Program, peer_allowed, serve};
pub use tmux::{TmuxLink, tmux_env};
