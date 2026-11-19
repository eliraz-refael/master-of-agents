//! Which panes a user can see, from tmux's lists and its notifications.

use std::collections::{BTreeMap, BTreeSet};

use super::cmd::Cmd;
use super::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use super::notify::Notification;
use crate::model::SessionId;

pub const LIST_SESSIONS: &str = "S #{session_id} #{session_name}";
/// The flag before `@moa_session` is the cockpit's: the window named `moa` in the session
/// named `moa`, as tmux sees it (its index depends on `base-index`). After the tag, whether
/// the pane is dead, and its `@moa_view` tag last, since it is free text.
pub const LIST_PANES: &str = "P #{pane_id} #{window_id} #{session_id} #{pane_active} #{window_active} #{window_zoomed_flag} #{&&:#{==:#{session_name},moa},#{==:#{window_name},moa}} #{@moa_session} #{pane_dead} #{@moa_view}";
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
    cockpits: BTreeSet<WindowId>,
    /// Each window's session, from the pane lines: a rename is judged by it.
    window_sessions: BTreeMap<WindowId, TmuxSessionId>,
    /// `@moa_view`, the panel a pane shows (or `agent` for the agent spot).
    views: BTreeMap<PaneId, String>,
    /// The panes whose program has exited and that tmux keeps (`remain-on-exit`).
    dead: BTreeSet<PaneId>,
}

/// The name of the session that holds the cockpit, and of the cockpit window in it.
const COCKPIT: &str = "moa";

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
        // Eleven fields, the last of which (`@moa_view`) keeps any spaces it has.
        let mut f = line.splitn(11, ' ');
        let (Some("P"), Some(pane), Some(window), Some(session)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            return;
        };
        let (Some(active), Some(window_active), Some(zoomed), Some(cockpit)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            return;
        };
        // An unset `@moa_session` or `@moa_view` prints as an empty field. Lines from before
        // the last two fields read as alive and untagged.
        let moa = f.next().and_then(|s| s.parse::<SessionId>().ok());
        let dead = f.next() == Some("1");
        let view = f.next().filter(|v| !v.is_empty());
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
        if cockpit == "1" {
            self.cockpits.insert(window);
        }
        if let Some(m) = moa {
            self.moa_sessions.insert(pane, m);
        }
        self.window_sessions.insert(window, session);
        if dead {
            self.dead.insert(pane);
        }
        if let Some(v) = view {
            self.views.insert(pane, v.to_string());
        }
    }

    /// The window is renamed: it is the cockpit when its new name and its session's are both
    /// `moa`. A window the lists don't hold yet is read again.
    fn window_renamed(&mut self, w: WindowId, name: &str) -> Applied {
        let Some(session) = self.window_sessions.get(&w) else {
            return Applied::Refresh;
        };
        let cockpit = name == COCKPIT && self.sessions.get(session).is_some_and(|n| n == COCKPIT);
        let changed = if cockpit {
            self.cockpits.insert(w)
        } else {
            self.cockpits.remove(&w)
        };
        if changed {
            Applied::Changed
        } else {
            Applied::Same
        }
    }

    /// The session is renamed. Renamed away from `moa`, none of its windows is the cockpit;
    /// renamed to `moa`, which of them is named `moa` takes a listing, since the lists carry
    /// no window names.
    fn session_renamed(&mut self, s: TmuxSessionId, name: &str) -> Applied {
        let Some(old) = self.sessions.get(&s) else {
            return Applied::Refresh;
        };
        if old == name {
            return Applied::Same;
        }
        self.sessions.insert(s, name.to_string());
        if name == COCKPIT {
            return Applied::Refresh;
        }
        let in_session = &self.window_sessions;
        self.cockpits.retain(|w| in_session.get(w) != Some(&s));
        Applied::Changed
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
            // A renamed session changes what `session_named` answers, and a renamed window
            // which window is the cockpit (by name: `moa` in the session `moa`). Both are
            // applied from the notification, with no listing (L16).
            Notification::WindowRenamed { window, name } => self.window_renamed(*window, name),
            Notification::SessionRenamed { session, name } => self.session_renamed(*session, name),
            Notification::WindowAdd { .. }
            | Notification::WindowClose { .. }
            | Notification::SessionsChanged
            | Notification::LayoutChange { .. }
            // A rename whose arguments didn't parse.
            | Notification::Other(
                "session-renamed" | "window-renamed" | "unlinked-window-renamed",
            ) => Applied::Refresh,
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

    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.keys().copied()
    }

    pub fn has_window(&self, w: WindowId) -> bool {
        self.windows.contains_key(&w)
    }

    /// Every pane in the lists, in any window.
    pub fn panes(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.windows.values().flat_map(|w| w.panes.iter().copied())
    }

    pub fn has_pane(&self, p: PaneId) -> bool {
        self.window_of(p).is_some()
    }

    pub fn window_of(&self, p: PaneId) -> Option<WindowId> {
        self.windows
            .iter()
            .find_map(|(id, w)| w.panes.contains(&p).then_some(*id))
    }

    /// Whether `w` is the cockpit: the window named `moa` in the session named `moa`.
    pub fn is_cockpit(&self, w: WindowId) -> bool {
        self.cockpits.contains(&w)
    }

    /// The cockpit window, if there is one (the first, should two be named `moa`).
    pub fn cockpit(&self) -> Option<WindowId> {
        self.cockpits.first().copied()
    }

    /// The panes in window `w`; none for a window the lists don't hold.
    pub fn panes_in(&self, w: WindowId) -> impl Iterator<Item = PaneId> + '_ {
        self.windows
            .get(&w)
            .into_iter()
            .flat_map(|w| w.panes.iter().copied())
    }

    /// The name of the session that holds pane `p`.
    pub fn session_name_of(&self, p: PaneId) -> Option<&str> {
        let session = self.window_sessions.get(&self.window_of(p)?)?;
        self.sessions.get(session).map(String::as_str)
    }

    /// A pane's `@moa_view` tag, as tmux printed it: a view's name, or `agent`.
    pub fn view_of(&self, p: PaneId) -> Option<&str> {
        self.views.get(&p).map(String::as_str)
    }

    /// Whether the pane's program has exited and tmux keeps the pane (`#{pane_dead}`).
    pub fn is_dead(&self, p: PaneId) -> bool {
        self.dead.contains(&p)
    }

    /// Records that `p` is tagged with session `id`, as moa just set it.
    pub fn tag(&mut self, p: PaneId, id: SessionId) {
        self.moa_sessions.insert(p, id);
    }

    /// Records that `p` has no session tag any more, as moa just unset it.
    pub fn untag(&mut self, p: PaneId) {
        self.moa_sessions.remove(&p);
    }

    /// Records that `p` shows `view` (`@moa_view`), as moa just set it.
    pub fn set_view(&mut self, p: PaneId, view: &str) {
        self.views.insert(p, view.to_string());
    }

    /// Records that `p` has no `@moa_view` any more, as moa just unset it.
    pub fn unset_view(&mut self, p: PaneId) {
        self.views.remove(&p);
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A line that looks like a list's: a kind letter, then words of ids, digits, flags and
    /// `@moa_session` values, with spaces doubled or missing; or any text at all.
    fn line() -> impl Strategy<Value = String> {
        prop_oneof![
            "[SPC]?( {0,2}[%@$]?[0-9a-f-]{0,40}){0,12} ?",
            "[ %@$SPC0-9]{0,30}",
            ".*",
        ]
    }

    proptest! {
        /// Whatever tmux prints, the lists build a topology (R27). A panic here would reload
        /// the core, and the reload's refresh would read the same lines again.
        #[test]
        fn from_lists_never_panics(
            sessions in prop::collection::vec(line(), 0..8),
            panes in prop::collection::vec(line(), 0..8),
            clients in prop::collection::vec(line(), 0..8),
        ) {
            let t = Topology::from_lists(&sessions, &panes, &clients);
            let _ = t.visible();
        }
    }
}
