//! tmux's vocabulary, with no IO: ids, notifications, the `moa:` mailbox, quoted
//! commands with an allow-list, and the focus topology.

pub mod cmd;
pub mod ids;
pub mod mailbox;
pub mod notify;
pub mod topology;
