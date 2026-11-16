//! Test support for moa: a throwaway tmux server in an isolated home and environment.

pub mod brain;
mod sandbox;
pub mod spike_fixture;
pub mod spike_gen;
pub mod temp_brain;
mod terminal;

pub use sandbox::{Sandbox, TMUX_NOT_FOUND, tmux_path, wait_for};
pub use spike_fixture::{OLD_SCHEMA_SQL, Scenario, SpikeBrain, SpikeFixture, ids};
pub use temp_brain::TempBrain;
pub use terminal::Terminal;
