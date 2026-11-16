//! What the tmux parser makes of a `%` line.

use super::ids::{ClientName, PaneId, TmuxSessionId, WindowId};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Notification {
    WindowPaneChanged {
        window: WindowId,
        pane: PaneId,
    },
    SessionWindowChanged {
        session: TmuxSessionId,
        window: WindowId,
    },
    ClientSessionChanged {
        client: ClientName,
        session: TmuxSessionId,
    },
    ClientDetached {
        client: ClientName,
    },
    /// The control client's own session.
    SessionChanged {
        session: TmuxSessionId,
    },
    SessionsChanged,
    /// `%window-add` and `%unlinked-window-add`.
    WindowAdd {
        window: WindowId,
    },
    /// `%window-close` and `%unlinked-window-close`.
    WindowClose {
        window: WindowId,
    },
    LayoutChange {
        window: WindowId,
    },
    /// `%message`, verbatim.
    Message(String),
    Exit,
    /// A known name moa doesn't use ("window-renamed", "output", ...).
    Other(&'static str),
    /// An unknown `%name`: the name only.
    Unknown(String),
}
