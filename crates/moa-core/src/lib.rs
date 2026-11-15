//! Plain types and pure logic shared by every moa crate. No IO.

pub mod config;
pub mod due;
pub mod model;
pub mod route;
pub mod time;

pub use time::{Now, TimeError, UtcMs};
