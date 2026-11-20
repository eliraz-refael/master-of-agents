//! Plain types and pure logic shared by every moa crate. No IO.

pub mod activity;
pub mod change;
pub mod cockpit;
pub mod config;
mod core;
pub mod due;
pub mod effect;
pub mod folders;
pub mod hook;
mod hooks;
pub mod keymap;
mod keys;
pub mod layout;
pub mod live;
pub mod model;
pub mod names;
pub mod op;
mod ops;
pub mod panel;
mod peers;
pub mod publish;
mod reconcile;
pub mod route;
pub mod sleep;
pub mod state;
pub mod time;
pub mod tmux;
mod wake;

pub use crate::core::handle;
pub use effect::{Effect, Input};
pub use state::State;
pub use time::{Now, TimeError, UtcMs};
