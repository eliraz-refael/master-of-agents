//! The tmux layer: the control-mode byte parser and the control client built on it.

pub mod client;
pub mod conf;
mod parse;

pub use moa_core::tmux::cmd::Cmd;
pub use moa_core::tmux::mailbox::Mailbox;
pub use moa_core::tmux::notify::Notification;
pub use parse::{Frame, MAX_BLOCK, MAX_LINE, ParseError, Parser, notification};
