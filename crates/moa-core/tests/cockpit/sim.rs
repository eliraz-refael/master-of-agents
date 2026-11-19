//! A tmux stand-in for the cockpit's chains: which pane is in which window, and which window
//! in which session, with each pane's options. No geometry: a window's panes are in order,
//! and a join puts its pane right after the target, as tmux does. It reads a `Cmd` only
//! through `parts()`, `name()` and `argv()`.

use std::collections::BTreeMap;

use moa_core::cockpit::{Place, Role, Scene};
use moa_core::model::SessionId;
use moa_core::panel::View;
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::PaneId;
use moa_core::tmux::topology::Topology;

/// What a pane was made as: the invariants follow strangers by what they were.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Agent,
    Panel,
    Stranger,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneSt {
    pub kind: Kind,
    /// The pane's options (`@moa_view`, `@moa_session`, …).
    pub tags: BTreeMap<String, String>,
    pub dead: bool,
    /// The session moa binds this pane to.
    pub bound: Option<SessionId>,
}

impl PaneSt {
    pub fn stranger() -> PaneSt {
        PaneSt {
            kind: Kind::Stranger,
            tags: BTreeMap::new(),
            dead: false,
            bound: None,
        }
    }

    pub fn panel(view: View) -> PaneSt {
        PaneSt {
            kind: Kind::Panel,
            ..PaneSt::stranger()
        }
        .tag("@moa_view", view.word())
    }

    /// A bound agent, with its `@moa_session` tag.
    pub fn agent(id: SessionId) -> PaneSt {
        PaneSt {
            kind: Kind::Agent,
            bound: Some(id),
            ..PaneSt::stranger()
        }
        .tag("@moa_session", &id.to_string())
    }

    pub fn tag(mut self, name: &str, value: &str) -> PaneSt {
        self.tags.insert(name.into(), value.into());
        self
    }

    pub fn dead(mut self) -> PaneSt {
        self.dead = true;
        self
    }

    pub fn unbound(mut self) -> PaneSt {
        self.bound = None;
        self
    }

    /// An agent now: bound, or tagged with its session.
    pub fn is_agent(&self) -> bool {
        self.bound.is_some() || self.tags.contains_key("@moa_session")
    }

    /// The panel view it carries, when it isn't an agent.
    pub fn panel_view(&self) -> Option<View> {
        if self.is_agent() {
            return None;
        }
        self.tags.get("@moa_view").and_then(|v| v.parse().ok())
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Win {
    pub id: u32,
    pub session: String,
    pub name: String,
    pub panes: Vec<PaneId>,
}

#[derive(Clone, Debug)]
pub struct World {
    /// Session names; a session's id is its index.
    pub sessions: Vec<String>,
    pub windows: Vec<Win>,
    pub panes: BTreeMap<PaneId, PaneSt>,
    pub killed: Vec<PaneId>,
    pub respawned: Vec<PaneId>,
    next_window: u32,
    next_pane: u32,
}

impl Default for World {
    fn default() -> World {
        World::new()
    }
}

impl World {
    /// Sessions `moa` and `_moa`, with no windows.
    pub fn new() -> World {
        World {
            sessions: vec!["moa".into(), "_moa".into()],
            windows: Vec::new(),
            panes: BTreeMap::new(),
            killed: Vec::new(),
            respawned: Vec::new(),
            next_window: 1,
            next_pane: 1,
        }
    }

    /// A new window in `session` (made if missing), named `name`.
    pub fn window(&mut self, session: &str, name: &str) -> u32 {
        if !self.sessions.iter().any(|s| s == session) {
            self.sessions.push(session.into());
        }
        let id = self.next_window;
        self.next_window += 1;
        self.windows.push(Win {
            id,
            session: session.into(),
            name: name.into(),
            panes: Vec::new(),
        });
        id
    }

    /// A new pane at the end of window `w`.
    pub fn pane(&mut self, w: u32, st: PaneSt) -> PaneId {
        let p = PaneId(self.next_pane);
        self.next_pane += 1;
        self.put(w, p, st);
        p
    }

    /// Pane `p`, at the end of window `w`.
    pub fn put(&mut self, w: u32, p: PaneId, st: PaneSt) {
        self.next_pane = self.next_pane.max(p.0 + 1);
        self.next_window = self.next_window.max(w + 1);
        self.win_mut(w).panes.push(p);
        self.panes.insert(p, st);
    }

    /// The cockpit window, made with the next window id.
    pub fn cockpit(&mut self) -> u32 {
        self.window("moa", "moa")
    }

    /// A panel started parked, as a spawn makes it: alone in its own window in `_moa`.
    pub fn park(&mut self, view: View) -> PaneId {
        let w = self.window("_moa", view.word());
        self.pane(w, PaneSt::panel(view))
    }

    fn win_mut(&mut self, id: u32) -> &mut Win {
        self.windows.iter_mut().find(|w| w.id == id).unwrap()
    }

    /// The window holding `p`, and the pane's index in it.
    pub fn where_is(&self, p: PaneId) -> Option<(usize, usize)> {
        self.windows
            .iter()
            .enumerate()
            .find_map(|(wi, w)| w.panes.iter().position(|x| *x == p).map(|pi| (wi, pi)))
    }

    pub fn session_of(&self, p: PaneId) -> Option<&str> {
        self.where_is(p)
            .map(|(wi, _)| self.windows[wi].session.as_str())
    }

    fn is_cockpit(w: &Win) -> bool {
        w.session == "moa" && w.name == "moa"
    }

    /// The panes of the cockpit (the first window named `moa` in session `moa`), in order.
    pub fn cockpit_leaves(&self) -> Vec<PaneId> {
        self.windows
            .iter()
            .filter(|w| World::is_cockpit(w))
            .min_by_key(|w| w.id)
            .map(|w| w.panes.clone())
            .unwrap_or_default()
    }

    pub fn in_cockpit(&self, p: PaneId) -> bool {
        self.cockpit_leaves().contains(&p)
    }

    /// What a fresh listing says, read as the engine reads it.
    pub fn scene(&self) -> Scene {
        let sessions: Vec<String> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| format!("S ${i} {s}"))
            .collect();
        let mut panes = Vec::new();
        let mut windows: Vec<&Win> = self.windows.iter().collect();
        windows.sort_by_key(|w| w.id);
        for w in windows {
            let session = self.sessions.iter().position(|s| *s == w.session).unwrap();
            let cockpit = u8::from(World::is_cockpit(w));
            for p in &w.panes {
                let st = &self.panes[p];
                let tag = |k: &str| st.tags.get(k).cloned().unwrap_or_default();
                panes.push(format!(
                    "P {p} @{} ${session} 0 0 0 {cockpit} {} {} {}",
                    w.id,
                    tag("@moa_session"),
                    u8::from(st.dead),
                    tag("@moa_view"),
                ));
            }
        }
        let topology = Topology::from_lists(&sessions, &panes, &[]);
        Scene::read(&topology, |p| self.panes.get(&p).and_then(|st| st.bound))
    }

    /// Runs a command or a chain as tmux would: in order, stopping at the first error.
    pub fn run(&mut self, cmd: &Cmd) -> Result<(), String> {
        self.run_first(cmd, usize::MAX)
    }

    /// Runs only a chain's first `n` parts, as when part `n` fails.
    pub fn run_first(&mut self, cmd: &Cmd, n: usize) -> Result<(), String> {
        cmd.check().map_err(|e| e.to_string())?;
        for part in cmd.parts().iter().take(n) {
            self.run_one(part)
                .map_err(|e| format!("{}: {e}", part.name()))?;
        }
        Ok(())
    }

    /// Takes `p` out of its window; a window left empty closes. Its session never does,
    /// though tmux ends a session whose last window closes. moa never needs that: `_moa`
    /// keeps the `/bin/sh -s` window the link attaches to (the front door makes it, and the
    /// engine never moves it), and `moa` always keeps the cockpit. Don't rely on an empty
    /// session surviving here.
    fn take_out(&mut self, p: PaneId) {
        let (wi, pi) = self.where_is(p).unwrap();
        self.windows[wi].panes.remove(pi);
        if self.windows[wi].panes.is_empty() {
            self.windows.remove(wi);
        }
    }

    fn session_target(&self, t: &str) -> Result<String, String> {
        let name = t
            .strip_prefix('=')
            .and_then(|t| t.strip_suffix(':'))
            .ok_or("a bad session target")?;
        if self.sessions.iter().any(|s| s == name) {
            Ok(name.into())
        } else {
            Err(format!("can't find session: {name}"))
        }
    }

    fn pane_arg(&self, s: &str) -> Result<PaneId, String> {
        let p: PaneId = s.parse().map_err(|_| format!("not a pane: {s}"))?;
        if self.where_is(p).is_some() {
            Ok(p)
        } else {
            Err(format!("can't find pane: {s}"))
        }
    }

    fn run_one(&mut self, c: &Cmd) -> Result<(), String> {
        let a: Vec<&str> = c.argv().iter().map(String::as_str).collect();
        match (c.name(), a.as_slice()) {
            ("swap-pane", ["-d", "-s", x, "-t", y]) => {
                let (x, y) = (self.pane_arg(x)?, self.pane_arg(y)?);
                let (xw, xi) = self.where_is(x).unwrap();
                let (yw, yi) = self.where_is(y).unwrap();
                self.windows[xw].panes[xi] = y;
                self.windows[yw].panes[yi] = x;
            }
            ("join-pane", ["-d", "-h" | "-v", "-s", x, "-t", y]) => {
                let (x, y) = (self.pane_arg(x)?, self.pane_arg(y)?);
                if x == y {
                    return Err("source and target panes must be different".into());
                }
                self.take_out(x);
                let (yw, yi) = self.where_is(y).unwrap();
                self.windows[yw].panes.insert(yi + 1, x);
            }
            ("break-pane", ["-d", "-s", x, "-t", to, rest @ ..]) => {
                let x = self.pane_arg(x)?;
                let session = self.session_target(to)?;
                let name = match rest {
                    ["-n", name] => Some(name.to_string()),
                    _ => None,
                };
                let (xw, _) = self.where_is(x).unwrap();
                if self.windows[xw].panes.len() == 1 {
                    // A pane alone in its window: the window moves.
                    let w = &mut self.windows[xw];
                    w.session = session;
                    if let Some(name) = name {
                        w.name = name;
                    }
                } else {
                    self.take_out(x);
                    let w = self.window(&session, name.as_deref().unwrap_or("sh"));
                    self.win_mut(w).panes.push(x);
                }
            }
            ("respawn-pane", ["-k", "-t", x, ..]) => {
                let x = self.pane_arg(x)?;
                self.respawned.push(x);
                let st = self.panes.get_mut(&x).unwrap();
                st.dead = false;
                st.bound = None;
            }
            ("kill-pane", ["-t", x]) => {
                let x = self.pane_arg(x)?;
                self.take_out(x);
                self.panes.remove(&x);
                self.killed.push(x);
            }
            ("set-option", ["-p", "-u", "-t", x, name]) => {
                let x = self.pane_arg(x)?;
                self.panes.get_mut(&x).unwrap().tags.remove(*name);
            }
            ("set-option", ["-p", "-t", x, name, value]) => {
                let x = self.pane_arg(x)?;
                let st = self.panes.get_mut(&x).unwrap();
                st.tags.insert(name.to_string(), value.to_string());
            }
            ("rename-window", ["-t", x, "--", name]) => {
                let x = self.pane_arg(x)?;
                let (xw, _) = self.where_is(x).unwrap();
                self.windows[xw].name = name.replace("##", "#");
            }
            // No geometry: a resize or a select only needs its pane.
            ("select-window" | "select-pane", ["-t", x])
            | ("resize-pane", ["-t", x, "-x" | "-y", _])
            | ("resize-window", ["-A", "-t", x])
            | ("set-option", ["-w", "-u", "-t", x, "window-size"]) => {
                self.pane_arg(x)?;
            }
            (name, args) => panic!("the simulator doesn't know {name} {args:?}"),
        }
        Ok(())
    }
}

impl From<&Scene> for World {
    /// A world that reads back as `scene`. The cockpit is named `moa` in session `moa`;
    /// other windows of `moa` are named after their id, so none is taken for the cockpit.
    fn from(scene: &Scene) -> World {
        let mut world = World::new();
        if !scene.moa_session {
            world.sessions.retain(|s| s != "moa");
        }
        for at in &scene.panes {
            let w = at.window.0;
            if !world.windows.iter().any(|x| x.id == w) {
                let (session, name) = match at.place {
                    Place::Cockpit => ("moa", "moa".to_string()),
                    Place::Moa => ("moa", format!("w{w}")),
                    Place::Hidden => ("_moa", format!("w{w}")),
                    Place::Elsewhere => ("elsewhere", format!("w{w}")),
                };
                if !world.sessions.iter().any(|s| s == session) {
                    world.sessions.push(session.into());
                }
                world.windows.push(Win {
                    id: w,
                    session: session.into(),
                    name,
                    panes: Vec::new(),
                });
            }
            let mut st = match at.role {
                Role::Panel(view) => PaneSt::panel(view),
                Role::Spot(None) => PaneSt::panel(View::EmptySlot),
                Role::Spot(Some(id)) => PaneSt::agent(id).tag("@moa_view", "agent"),
                Role::Agent(id) if at.tagged => PaneSt::agent(id).tag("@moa_view", "agent"),
                Role::Agent(id) => PaneSt::agent(id),
                Role::Other if at.tagged => PaneSt::stranger().tag("@moa_view", "agent"),
                Role::Other => PaneSt::stranger(),
            };
            st.dead = at.dead;
            world.put(w, at.pane, st);
        }
        world
    }
}
