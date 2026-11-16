//! The wire between the daemon and its clients: newline-delimited JSON-RPC 2.0 messages.
//!
//! Pure: no IO, no async runtime. The daemon and the client both depend on this crate, so
//! a line means the same thing at both ends.

mod patch;
mod wire;

pub use moa_core::publish::{CoreEvent, Model, Topic};
pub use patch::{Patch, SnapshotMsg, snapshot_of};
pub use wire::*;
