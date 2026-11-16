//! tmux's vocabulary, with no IO: ids, notifications, the `moa:` mailbox, quoted
//! commands with an allow-list, the focus topology, and the link's state.

pub mod cmd;
pub mod ids;
pub mod link;
pub mod mailbox;
pub mod notify;
pub mod topology;
