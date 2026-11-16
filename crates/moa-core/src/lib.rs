//! Plain types and pure logic shared by every moa crate. No IO.

pub mod activity;
pub mod change;
pub mod config;
mod core;
pub mod due;
pub mod effect;
pub mod live;
pub mod model;
pub mod names;
pub mod op;
mod ops;
pub mod publish;
pub mod route;
pub mod state;
pub mod time;

pub use crate::core::handle;
pub use effect::{Effect, Input};
pub use state::State;
pub use time::{Now, TimeError, UtcMs};
