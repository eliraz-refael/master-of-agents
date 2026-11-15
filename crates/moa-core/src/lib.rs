//! Plain types and pure logic shared by every moa crate. No IO.

pub mod config;
pub mod model;
pub mod time;

pub use time::{TimeError, UtcMs};
