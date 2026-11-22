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
/// Every pane again, with what runs in it: its process, its terminal mark (`@moa_term`), the
/// program in front, its folder, then its `@moa_label`. Tab-separated with the label last,
/// since a folder or a label can hold spaces and a label tabs too: a list of its own, as
/// `LIST_PANES` already ends in free text.
pub const LIST_TERMS: &str = "T\t#{pane_id}\t#{pane_pid}\t#{@moa_term}\t#{pane_current_command}\t#{pane_current_path}\t#{@moa_label}";

/// The four queries that rebuild a `Topology`: the three `from_lists` takes, in its order,
/// then the panes' programs and terminals, for `terms_listed`.
pub fn refresh_cmds() -> Vec<Cmd> {
    vec![
        Cmd::new("list-sessions").arg("-F").arg(LIST_SESSIONS),
        Cmd::new("list-panes").arg("-a").arg("-F").arg(LIST_PANES),
        Cmd::new("list-clients").arg("-F").arg(LIST_CLIENTS),
        Cmd::new("list-panes").arg("-a").arg("-F").arg(LIST_TERMS),
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

/// What `LIST_TERMS` says runs in a pane: its process (`#{pane_pid}`), the program in front
/// (`#{pane_current_command}`, as of the listing: tmux tells nothing when it changes), and
/// that program's folder.
#[derive(Clone, Debug)]
struct Running {
    pid: u32,
    command: String,
    cwd: String,
}

#[derive(Clone, Default, Debug)]
pub struct Topology {
    sessions: BTreeMap<TmuxSessionId, String>,
    current: BTreeMap<TmuxSessionId, WindowId>,
    windows: BTreeMap<WindowId, Window>,
    clients: BTreeMap<ClientName, TmuxSessionId>,
    moa_sessions: BTreeMap<PaneId, SessionId>,
    cockpits: BTreeSet<WindowId>,
    /// Each window's sessions, from the pane lines: a rename is judged by them. A window is
    /// in more than one when it is linked into another (a sheet, S9).
    window_sessions: BTreeMap<WindowId, BTreeSet<TmuxSessionId>>,
    /// `@moa_view`, the panel a pane shows (or `agent` for the agent spot).
    views: BTreeMap<PaneId, String>,
    /// The panes whose program has exited and that tmux keeps (`remain-on-exit`).
    dead: BTreeSet<PaneId>,
    /// The terminals moa opened (`@moa_term`), each with its `@moa_label` as moa meant it
    /// (tmux keeps it with every `#` doubled).
    terms: BTreeMap<PaneId, String>,
    /// What runs in each pane, from `LIST_TERMS`.
    running: BTreeMap<PaneId, Running>,
    /// Whether `LIST_TERMS` came back with this listing: without it, no pane has a program
    /// or folder, and the only terminals known are those moa tagged since.
    panes_listed: bool,
}

/// The session the user's terminal shows, which holds the cockpit (the window of the same
/// name) and the agents' tabs.
pub(crate) const MOA: &str = "moa";

/// The hidden session where panels wait, parked.
pub(crate) const HIDDEN: &str = "_moa";

/// What [`Topology::sessions_of`] gives a window the lists don't hold.
static NO_SESSIONS: BTreeSet<TmuxSessionId> = BTreeSet::new();

/// What a terminal with no label is called: an agent that ran in it, in the spot, took its
/// label (`● <title>`) and left with it. The cockpit names a shell it sends home the same
/// (`⌨ shell`).
pub(crate) const UNNAMED: &str = "shell";

/// What the label of the agent in the spot starts with: `● <title>`.
pub const AGENT_MARK: &str = "● ";

/// A terminal's name: its label without the `⌨ ` moa puts first, or [`UNNAMED`] when that
/// leaves nothing. A label set by hand is the name as it is.
fn term_name(label: &str) -> &str {
    let name = label.strip_prefix("⌨ ").unwrap_or(label);
    if name.trim().is_empty() {
        UNNAMED
    } else {
        name
    }
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

    /// The panes' programs and terminals (`LIST_TERMS`) replace those known; it is read
    /// after the panes' list. A pane is a terminal when its `@moa_term` is set, as tmux's
    /// `-f` reads a flag (neither empty nor `0`). Lines that don't parse are skipped; an
    /// unset label prints as an empty one, and a label keeps any tabs it has. A label or a
    /// folder holding a newline could print a line of its own, so only a pane the panes'
    /// list holds counts, and a pane listed twice (a window linked into two sessions, or a
    /// forged line) keeps neither its program nor its terminal mark.
    pub fn terms_listed(&mut self, lines: &[String]) {
        self.terms.clear();
        self.running.clear();
        let mut twice = BTreeSet::new();
        for line in lines {
            let mut f = line.splitn(7, '\t');
            let (Some("T"), Some(pane), Some(pid), Some(term), Some(command), Some(cwd)) =
                (f.next(), f.next(), f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            let (Ok(pane), Ok(pid)) = (pane.parse(), pid.parse()) else {
                continue;
            };
            if !self.has_pane(pane) {
                continue;
            }
            let running = Running {
                pid,
                command: command.to_string(),
                cwd: cwd.to_string(),
            };
            if self.running.insert(pane, running).is_some() {
                twice.insert(pane);
            }
            if !term.is_empty() && term != "0" {
                // Set through `literal_format`, which doubles every `#`.
                let label = f.next().unwrap_or_default().replace("##", "#");
                self.terms.insert(pane, label);
            }
        }
        self.terms.retain(|p, _| !twice.contains(p));
        self.running.retain(|p, _| !twice.contains(p));
        self.panes_listed = true;
    }

    /// Whether this listing's `LIST_TERMS` came back: when it failed, no pane has a program
    /// or folder, so nothing can be told of what runs where.
    pub fn panes_listed(&self) -> bool {
        self.panes_listed
    }

    /// The process tmux started in pane `p` (`#{pane_pid}`): a terminal's shell.
    pub fn pid_of(&self, p: &PaneId) -> Option<u32> {
        self.running.get(p).map(|r| r.pid)
    }

    /// The program in front in pane `p` (`#{pane_current_command}`) when it was listed.
    pub fn command_of(&self, p: &PaneId) -> Option<&str> {
        self.running.get(p).map(|r| r.command.as_str())
    }

    /// The folder of pane `p`'s program in front (`#{pane_current_path}`) when it was listed.
    pub fn cwd_of(&self, p: &PaneId) -> Option<&str> {
        self.running.get(p).map(|r| r.cwd.as_str())
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
        self.window_sessions
            .entry(window)
            .or_default()
            .insert(session);
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
        let Some(sessions) = self.window_sessions.get(&w) else {
            return Applied::Refresh;
        };
        let cockpit = name == MOA
            && sessions
                .iter()
                .any(|s| self.sessions.get(s).is_some_and(|n| n == MOA));
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
        if name == MOA {
            return Applied::Refresh;
        }
        let in_session = &self.window_sessions;
        self.cockpits
            .retain(|w| !in_session.get(w).is_some_and(|ss| ss.contains(&s)));
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

    /// The name of the session that holds pane `p`. A window in more than one is `_moa`'s
    /// when that is one of them: a sheet linked into a user's session lives there (S9).
    /// Otherwise the name that sorts last, as when the last line `list-panes -a` printed for
    /// it won (tmux lists sessions by name).
    pub fn session_name_of(&self, p: PaneId) -> Option<&str> {
        let names = self
            .sessions_of(self.window_of(p)?)
            .iter()
            .filter_map(|s| self.session_name(*s));
        names.clone().find(|n| *n == HIDDEN).or_else(|| names.max())
    }

    /// The sessions window `w` is in: none for a window the lists don't hold.
    pub fn sessions_of(&self, w: WindowId) -> &BTreeSet<TmuxSessionId> {
        self.window_sessions.get(&w).unwrap_or(&NO_SESSIONS)
    }

    /// Session `s`'s name, if the lists hold it.
    pub fn session_name(&self, s: TmuxSessionId) -> Option<&str> {
        self.sessions.get(&s).map(String::as_str)
    }

    /// The tab session `s` shows, if the lists say.
    pub fn current_of(&self, s: TmuxSessionId) -> Option<WindowId> {
        self.current.get(&s).copied()
    }

    /// The session user's terminal `c` shows, if it is attached.
    pub fn client_session(&self, c: &ClientName) -> Option<TmuxSessionId> {
        self.clients.get(c).copied()
    }

    /// Whether a user's terminal shows session `s`.
    pub fn attached(&self, s: TmuxSessionId) -> bool {
        self.clients.values().any(|c| *c == s)
    }

    /// Records that moa linked window `w` into session `s` (`link-window`), as tmux just
    /// answered it. A window the lists don't hold changes nothing.
    pub fn linked(&mut self, w: WindowId, s: TmuxSessionId) {
        if let Some(sessions) = self.window_sessions.get_mut(&w) {
            sessions.insert(s);
        }
    }

    /// Records that moa unlinked window `w` from session `s` (`unlink-window`), as tmux just
    /// answered it: the window lives on in its other sessions.
    pub fn unlinked(&mut self, w: WindowId, s: TmuxSessionId) {
        if let Some(sessions) = self.window_sessions.get_mut(&w) {
            sessions.remove(&s);
        }
        if self.current.get(&s) == Some(&w) {
            self.current.remove(&s);
        }
    }

    /// A pane's `@moa_view` tag, as tmux printed it: a view's name, or `agent`.
    pub fn view_of(&self, p: PaneId) -> Option<&str> {
        self.views.get(&p).map(String::as_str)
    }

    /// Whether the pane's program has exited and tmux keeps the pane (`#{pane_dead}`).
    pub fn is_dead(&self, p: PaneId) -> bool {
        self.dead.contains(&p)
    }

    /// The label (`@moa_label`) of `p`, when it carries the terminal mark (`@moa_term`),
    /// whatever runs in it.
    pub fn term_label(&self, p: PaneId) -> Option<&str> {
        self.terms.get(&p).map(String::as_str)
    }

    /// Terminal `p`'s name: its label without the `⌨ ` moa puts first. None when `p` isn't
    /// a terminal, runs a session's agent (it is tagged with the session: it is that session
    /// then, and its label is the agent's), or is dead (its shell exited; tmux kept it).
    pub fn term_name(&self, p: PaneId) -> Option<&str> {
        if !self.is_live_term(p) {
            return None;
        }
        self.term_label(p).map(term_name)
    }

    /// Every terminal, by pane, with its name; none that runs a session's agent or is dead.
    pub fn terms(&self) -> impl Iterator<Item = (PaneId, &str)> + '_ {
        self.terms
            .iter()
            .filter(|(p, _)| self.is_live_term(**p))
            .map(|(p, label)| (*p, term_name(label)))
    }

    /// Whether `p` may be listed as a terminal: no session's agent runs in it, and its
    /// program hasn't exited.
    fn is_live_term(&self, p: PaneId) -> bool {
        !self.moa_sessions.contains_key(&p) && !self.dead.contains(&p)
    }

    /// Records that `p` is a terminal labelled `label`, as moa just tagged it.
    pub fn set_term(&mut self, p: PaneId, label: &str) {
        self.terms.insert(p, label.to_string());
    }

    /// Records that `p` has no terminal mark any more, as moa just unset it.
    pub fn unset_term(&mut self, p: PaneId) {
        self.terms.remove(&p);
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

    /// Records that moa swapped panes `a` and `b` (`swap-pane -d`): each takes the other's
    /// place, and a window's active place stays where it was. Panes of one window, or one
    /// the lists don't hold, change nothing.
    pub fn swapped(&mut self, a: PaneId, b: PaneId) {
        let (Some(wa), Some(wb)) = (self.window_of(a), self.window_of(b)) else {
            return;
        };
        if wa == wb {
            return;
        }
        for (w, out, into) in [(wa, a, b), (wb, b, a)] {
            if let Some(w) = self.windows.get_mut(&w) {
                w.panes.remove(&out);
                w.panes.insert(into);
                if w.active == Some(out) {
                    w.active = Some(into);
                }
            }
        }
    }

    /// Records that moa broke pane `p` out of its window into a new one (`break-pane -d`):
    /// it leaves its window, and is in none until a listing shows its new one.
    pub fn lifted(&mut self, p: PaneId) {
        let Some(id) = self.window_of(p) else {
            return;
        };
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        w.panes.remove(&p);
        if w.active == Some(p) {
            w.active = None;
        }
        if w.panes.is_empty() {
            self.windows.remove(&id);
            self.window_sessions.remove(&id);
            self.cockpits.remove(&id);
            self.current.retain(|_, current| *current != id);
        }
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

    /// A line that looks like `LIST_TERMS`'s: tab-separated words of ids, digits and text.
    fn tab_line() -> impl Strategy<Value = String> {
        "T?(\t{1,2}[%0-9a-z /#-]{0,12}){0,9}"
    }

    proptest! {
        /// Whatever tmux prints, the lists build a topology (R27). A panic here would reload
        /// the core, and the reload's refresh would read the same lines again.
        #[test]
        fn from_lists_never_panics(
            sessions in prop::collection::vec(line(), 0..8),
            panes in prop::collection::vec(line(), 0..8),
            clients in prop::collection::vec(line(), 0..8),
            terms in prop::collection::vec(prop_oneof![line(), tab_line()], 0..8),
        ) {
            let mut t = Topology::from_lists(&sessions, &panes, &clients);
            t.terms_listed(&terms);
            let _ = t.visible();
            let _ = t.terms().count();
        }
    }

    /// A swap trades the panes' places, the active place staying put; a pane broken out
    /// leaves its window, and a window left empty goes with it.
    #[test]
    fn swaps_and_breaks_are_recorded() {
        let s = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let mut t = Topology::from_lists(
            &s(&["S $0 moa", "S $1 _moa"]),
            &s(&[
                "P %1 @1 $0 1 1 0 1  0 tree",
                "P %5 @1 $0 0 1 0 1  0 empty-slot",
                "P %9 @9 $0 1 0 0 0  0 ",
            ]),
            &s(&["C 0 $0 /dev/ttys1"]),
        );
        t.swapped(PaneId(9), PaneId(5));
        assert_eq!(t.window_of(PaneId(9)), Some(WindowId(1)));
        assert_eq!(t.window_of(PaneId(5)), Some(WindowId(9)));
        assert_eq!(t.windows[&WindowId(9)].active, Some(PaneId(5)));
        // Two panes of one window, or one not listed: nothing.
        t.swapped(PaneId(1), PaneId(9));
        t.swapped(PaneId(9), PaneId(7));
        assert_eq!(t.window_of(PaneId(9)), Some(WindowId(1)));
        assert_eq!(t.window_of(PaneId(1)), Some(WindowId(1)));

        t.lifted(PaneId(5));
        assert_eq!(t.window_of(PaneId(5)), None);
        assert!(!t.has_window(WindowId(9)));
        assert_eq!(t.view_of(PaneId(5)), Some("empty-slot"), "its tags stay");
        t.lifted(PaneId(9));
        assert_eq!(t.cockpit(), Some(WindowId(1)), "the cockpit keeps the tree");
        assert_eq!(t.panes_in(WindowId(1)).collect::<Vec<_>>(), [PaneId(1)]);
    }

    /// S9: a sheet linked into a user's session is listed once per session, `_moa` first.
    /// The window is known in both, and it lives in `_moa`, whichever line came last.
    #[test]
    fn a_window_in_two_sessions_is_known_in_both() {
        let s = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let mut t = Topology::from_lists(
            &s(&["S $0 _moa", "S $1 moa"]),
            &s(&[
                "P %3 @3 $0 1 1 0 0  0 picker",
                "P %3 @3 $1 1 1 0 0  0 picker",
            ]),
            &s(&["C 0 $1 /dev/ttys1"]),
        );
        let both = BTreeSet::from([TmuxSessionId(0), TmuxSessionId(1)]);
        assert_eq!(t.sessions_of(WindowId(3)), &both);
        assert_eq!(t.session_name_of(PaneId(3)), Some("_moa"));
        assert!(t.sessions_of(WindowId(9)).is_empty());
        assert!(t.attached(TmuxSessionId(1)) && !t.attached(TmuxSessionId(0)));

        // An unlink moa just made is known at once; the window stays in `_moa`.
        t.unlinked(WindowId(3), TmuxSessionId(1));
        assert_eq!(
            t.sessions_of(WindowId(3)),
            &BTreeSet::from([TmuxSessionId(0)])
        );
        t.linked(WindowId(3), TmuxSessionId(1));
        assert_eq!(t.sessions_of(WindowId(3)), &both);
        assert_eq!(t.session_name_of(PaneId(3)), Some("_moa"));
    }

    /// A window that isn't `_moa`'s keeps the last line's session, as before S9: tmux lists
    /// sessions by name.
    #[test]
    fn a_window_in_two_other_sessions_reads_as_the_last_listed() {
        let s = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let t = Topology::from_lists(
            &s(&["S $4 work", "S $0 moa"]),
            &s(&["P %3 @3 $0 1 1 0 0  0 ", "P %3 @3 $4 1 1 0 0  0 "]),
            &s(&[]),
        );
        assert_eq!(t.session_name_of(PaneId(3)), Some("work"));
    }

    /// A terminal whose shell exited, kept by tmux, is listed as no terminal: there is
    /// nothing in it to bring to the screen.
    #[test]
    fn a_dead_terminal_is_no_terminal() {
        let s = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let mut t = Topology::from_lists(
            &s(&["S $0 moa"]),
            &s(&["P %3 @3 $0 1 0 0 0  0 ", "P %4 @4 $0 1 0 0 0  1 "]),
            &s(&[]),
        );
        t.terms_listed(&s(&[
            "T\t%3\t30\t1\tsh\t/r\t⌨ api",
            "T\t%4\t40\t1\tsh\t/r\t⌨ web",
        ]));
        assert_eq!(t.terms().collect::<Vec<_>>(), [(PaneId(3), "api")]);
        assert_eq!(t.term_name(PaneId(4)), None);
        assert_eq!(
            t.term_label(PaneId(4)),
            Some("⌨ web"),
            "its mark stays known"
        );
    }
}
