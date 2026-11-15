//! Test support for moa: a throwaway tmux server in an isolated home and environment.

mod sandbox;

pub use sandbox::{Sandbox, wait_for};
