//! Which panes a user can see, from tmux's lists and its notifications.

use std::collections::{BTreeMap, BTreeSet};

use super::cmd::Cmd;
use super::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use super::notify::Notification;
use crate::model::SessionId;

pub const LIST_SESSIONS: &str = "S #{session_id} #{session_name}";
pub const LIST_PANES: &str = "P #{pane_id} #{window_id} #{session_id} #{pane_active} #{window_active} #{window_zoomed_flag} #{@moa_session}";
pub const LIST_CLIENTS: &str = "C #{client_control_mode} #{session_id} #{client_name}";

/// The three queries that rebuild a `Topology`, in the order `from_lists` takes them.
pub fn refresh_cmds() -> Vec<Cmd> {
    vec![
        Cmd::new("list-sessions").arg("-F").arg(LIST_SESSIONS),
        Cmd::new("list-panes").arg("-a").arg("-F").arg(LIST_PANES),
        Cmd::new("list-clients").arg("-F").arg(LIST_CLIENTS),
    ]
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Window {
    pub panes: BTreeSet<PaneId>,
    pub active: Option<PaneId>,
    pub zoomed: bool,
}

/// What `Topology::apply` did with a notification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Applied {
    Changed,
    Same,
    /// The lists must be read again.
    Refresh,
}

#[derive(Clone, Default, Debug)]
pub struct Topology {
    sessions: BTreeMap<TmuxSessionId, String>,
    current: BTreeMap<TmuxSessionId, WindowId>,
    windows: BTreeMap<WindowId, Window>,
    clients: BTreeMap<ClientName, TmuxSessionId>,
    moa_sessions: BTreeMap<PaneId, SessionId>,
}

/// Replace a value, saying whether it differed.
fn put<K: Ord, V: PartialEq>(map: &mut BTreeMap<K, V>, k: K, v: V) -> Applied {
    if map.get(&k) == Some(&v) {
        return Applied::Same;
    }
    map.insert(k, v);
    Applied::Changed
}

impl Topology {
    /// Build from the three lists' lines. Lines that don't parse are skipped.
    pub fn from_lists(sessions: &[String], panes: &[String], clients: &[String]) -> Topology {
        let mut t = Topology::default();
        for line in sessions {
            let mut f = line.splitn(3, ' ');
            if let (Some("S"), Some(id), Some(name)) = (f.next(), f.next(), f.next())
                && let Ok(id) = id.parse()
            {
                t.sessions.insert(id, name.to_string());
            }
        }
        for line in panes {
            t.add_pane(line);
        }
        for line in clients {
            let mut f = line.splitn(4, ' ');
            if let (Some("C"), Some("0"), Some(session), Some(name)) =
                (f.next(), f.next(), f.next(), f.next())
                && let Ok(session) = session.parse()
            {
                t.clients.insert(ClientName(name.to_string()), session);
            }
        }
        t
    }

    fn add_pane(&mut self, line: &str) {
        let mut f = line.split(' ');
        let (Some("P"), Some(pane), Some(window), Some(session)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            return;
        };
        let (Some(active), Some(window_active), Some(zoomed)) = (f.next(), f.next(), f.next())
        else {
            return;
        };
        // An unset `@moa_session` prints as an empty last field.
        let moa = f.next().and_then(|s| s.parse::<SessionId>().ok());
        let (Ok(pane), Ok(window), Ok(session)) = (pane.parse(), window.parse(), session.parse())
        else {
            return;
        };
        let w = self.windows.entry(window).or_default();
        w.panes.insert(pane);
        w.zoomed = zoomed == "1";
        if active == "1" {
            w.active = Some(pane);
        }
        if window_active == "1" {
            self.current.insert(session, window);
        }
        if let Some(m) = moa {
            self.moa_sessions.insert(pane, m);
        }
    }

    pub fn apply(&mut self, n: &Notification) -> Applied {
        match n {
            Notification::WindowPaneChanged { window, pane } => {
                match self.windows.get_mut(window) {
                    Some(w) if !w.panes.contains(pane) => Applied::Refresh,
                    Some(w) if w.active == Some(*pane) => Applied::Same,
                    Some(w) => {
                        w.active = Some(*pane);
                        Applied::Changed
                    }
                    None => Applied::Refresh,
                }
            }
            Notification::SessionWindowChanged { session, window } => {
                put(&mut self.current, *session, *window)
            }
            Notification::ClientSessionChanged { client, session } => {
                if self.clients.contains_key(client) {
                    put(&mut self.clients, client.clone(), *session)
                } else {
                    Applied::Refresh
                }
            }
            Notification::ClientDetached { client } => {
                if self.clients.remove(client).is_some() {
                    Applied::Changed
                } else {
                    Applied::Same
                }
            }
            Notification::WindowAdd { .. }
            | Notification::WindowClose { .. }
            | Notification::SessionsChanged
            | Notification::LayoutChange { .. } => Applied::Refresh,
            _ => Applied::Same,
        }
    }

    /// The panes a user client can see: its session's current window, or only that
    /// window's active pane when it is zoomed.
    pub fn visible(&self) -> BTreeSet<PaneId> {
        let mut out = BTreeSet::new();
        for session in self.clients.values() {
            let Some(w) = self.current.get(session).and_then(|w| self.windows.get(w)) else {
                continue;
            };
            if w.zoomed {
                out.extend(w.active);
            } else {
                out.extend(&w.panes);
            }
        }
        out
    }

    pub fn users_attached(&self) -> bool {
        !self.clients.is_empty()
    }

    pub fn session_named(&self, name: &str) -> Option<TmuxSessionId> {
        self.sessions
            .iter()
            .find_map(|(id, n)| (n == name).then_some(*id))
    }

    pub fn has_pane(&self, p: PaneId) -> bool {
        self.window_of(p).is_some()
    }

    pub fn window_of(&self, p: PaneId) -> Option<WindowId> {
        self.windows
            .iter()
            .find_map(|(id, w)| w.panes.contains(&p).then_some(*id))
    }

    /// The moa session a pane was tagged with (`@moa_session`), if any.
    pub fn moa_session(&self, p: PaneId) -> Option<SessionId> {
        self.moa_sessions.get(&p).copied()
    }

    /// The active pane of the window a user client is looking at.
    pub fn client_pane(&self, c: &ClientName) -> Option<PaneId> {
        let session = self.clients.get(c)?;
        let w = self.current.get(session)?;
        self.windows.get(w)?.active
    }
}
