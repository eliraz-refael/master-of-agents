//! Panels: the clients that draw a view in a tab's pane. Each tells the core which pane it
//! is and which view it shows (`ui.panel`), and which folder its selection is in
//! (`ui.here`). The registry lives in the core's memory, by connection. A pane moa tagged
//! with `@moa_view` shows that view, whatever the panel asked for (L8).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cockpit::{HIDDEN, spawn_tags};
use crate::effect::{Effect, Publish, TmuxToken};
use crate::model::UnknownValue;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::publish::Topic;
use crate::state::State;
use crate::tmux::ids::PaneId;
use crate::tmux::link::TmuxState;
use crate::tmux::topology::Topology;

/// What a panel shows.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum View {
    Console,
    Tree,
    Plan,
    Status,
    Today,
    Activity,
    Projects,
    Picture,
    Picker,
    EmptySlot,
    Help,
    Setup,
}

impl View {
    pub const ALL: &'static [View] = &[
        View::Console,
        View::Tree,
        View::Plan,
        View::Status,
        View::Today,
        View::Activity,
        View::Projects,
        View::Picture,
        View::Picker,
        View::EmptySlot,
        View::Help,
        View::Setup,
    ];

    /// The view's name, as the wire and `moa panel` spell it.
    pub fn word(self) -> &'static str {
        match self {
            View::Console => "console",
            View::Tree => "tree",
            View::Plan => "plan",
            View::Status => "status",
            View::Today => "today",
            View::Activity => "activity",
            View::Projects => "projects",
            View::Picture => "picture",
            View::Picker => "picker",
            View::EmptySlot => "empty-slot",
            View::Help => "help",
            View::Setup => "setup",
        }
    }

    /// The title on the panel's frame: the spike's `LABELS`, else the word.
    pub fn label(self) -> &'static str {
        match self {
            View::Console => "◆ moa",
            View::Tree => "▤ sessions",
            View::Plan => "◷ big picture",
            View::Today => "☀ today",
            View::Activity => "≡ activity",
            View::Projects => "▦ projects",
            View::Status => "◉ status",
            View::Picture => "◫ picture",
            View::EmptySlot => "○ agent spot",
            View::Picker | View::Help | View::Setup => self.word(),
        }
    }

    /// Whether the panel draws its own frame: the spike's `FRAMED`, plus the picker, help and
    /// setup (S11).
    pub fn framed(self) -> bool {
        matches!(
            self,
            View::Console
                | View::Tree
                | View::Plan
                | View::Today
                | View::Activity
                | View::Projects
                | View::Status
                | View::Picker
                | View::Help
                | View::Setup
        )
    }
}

impl FromStr for View {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, UnknownValue> {
        View::ALL
            .iter()
            .copied()
            .find(|v| v.word() == s)
            .ok_or(UnknownValue { what: "view" })
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// One panel's registration: its pane (none outside moa's terminal), its view, the folder
/// its selection is in, whether its view is moa's, and whether moa placed it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelReg {
    pub pane: Option<PaneId>,
    pub view: View,
    pub here: Option<String>,
    /// The pane carries moa's `@moa_view`, which the view follows (L8).
    pub managed: bool,
    /// moa started this panel (it runs with `MOA_NO_AUTOSTART`): left untagged in `_moa`, it
    /// gets the tags its spawn would have given it (L18).
    pub placed: bool,
}

/// What `ui.panel` says to a tab id that isn't one (`%<digits>`).
const BAD_PANE: &str = "a panel's tab id looks like %5";

/// What `ui.here` says before the connection said which panel it is.
const NOT_A_PANEL: &str = "say which panel this is first (ui.panel)";

/// The key of a connection's panel row: `c<conn>`.
pub(crate) fn key_of(conn: ConnId) -> String {
    format!("c{}", conn.0)
}

/// A tab id as `ui.panel` and `ui.select_view` take it.
pub(crate) fn pane_arg(pane: &str) -> Result<PaneId, OpError> {
    pane.parse().map_err(|_| OpError(BAD_PANE.into()))
}

/// `ui.panel`: registers `conn` as the panel in `pane`. Its view is the one moa tagged the
/// pane with, else `view`, else the view this pane last had, else the picture (L8). A panel
/// another connection held in that pane is dropped. A panel moa placed that was never tagged
/// is tagged now (L18), unless a cockpit operation runs.
pub(crate) fn register(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    pane: Option<&str>,
    view: Option<View>,
    placed: bool,
) -> Vec<Effect> {
    let pane = match pane.map(pane_arg).transpose() {
        Ok(p) => p,
        Err(e) => return vec![reply(conn, req, Err(e))],
    };
    let view = view
        .or_else(|| pane.and_then(|p| state.pane_views.get(&p).copied()))
        .unwrap_or(View::Picture);
    if let Some(p) = pane {
        state.panels.retain(|c, r| *c == conn || r.pane != Some(p));
    }
    // The same panel saying again which it is keeps its folder.
    let here = state.panels.remove(&conn).and_then(|r| r.here);
    let mut reg = PanelReg {
        pane,
        view,
        here,
        managed: false,
        placed,
    };
    // Not while a cockpit operation runs: the next listing heals it then (L17).
    let mut fx: Vec<Effect> = match crate::cockpit::run::idle(state) {
        true => heal(&mut state.tmux, [&reg]).into_iter().collect(),
        false => Vec::new(),
    };
    resolve(&state.tmux.topology, &mut state.pane_views, &mut reg);
    let (view, managed) = (reg.view, reg.managed);
    state.panels.insert(conn, reg);
    fx.extend(publish_panels(state));
    fx.push(reply(
        conn,
        req,
        Ok(Done {
            text: String::new(),
            data: json!({"key": key_of(conn), "view": view, "managed": managed}),
        }),
    ));
    fx
}

/// `reg`'s view and `managed` from its pane's `@moa_view`, as the topology has it: a tag
/// naming a view wins (L8). An untagged pane keeps the view it has, as its own. The pane
/// remembers the view either way.
fn resolve(topology: &Topology, pane_views: &mut BTreeMap<PaneId, View>, reg: &mut PanelReg) {
    let Some(pane) = reg.pane else {
        reg.managed = false;
        return;
    };
    let tagged = topology.view_of(pane).and_then(|v| v.parse().ok());
    reg.managed = tagged.is_some();
    reg.view = tagged.unwrap_or(reg.view);
    pane_views.insert(pane, reg.view);
}

/// L8: after a listing, each panel's view and `managed` read again from the tags, and
/// `panels` published when one changed: a panel that registered before moa read its tag (a
/// restart, a spawn just answered) is corrected.
pub(crate) fn resolve_panels(state: &mut State) -> Option<Effect> {
    let topology = &state.tmux.topology;
    for reg in state.panels.values_mut() {
        resolve(topology, &mut state.pane_views, reg);
    }
    publish_panels(state)
}

/// L18: the panels moa placed whose tab sits untagged in `_moa` (their spawn's answer was
/// lost, so nothing tagged them) get the tags a spawn gives: their view, its label, and a
/// tab that stays when the panel stops. Sent at once, and recorded as a spawn's are. A panel
/// the user started is never tagged this way.
fn heal<'a>(tmux: &mut TmuxState, regs: impl IntoIterator<Item = &'a PanelReg>) -> Option<Effect> {
    let conn = tmux.conn()?;
    let topology = &tmux.topology;
    let orphans: Vec<(PaneId, View)> = regs
        .into_iter()
        .filter(|r| r.placed)
        .filter_map(|r| Some((r.pane?, r.view)))
        .filter(|(p, _)| {
            topology.view_of(*p).is_none() && topology.session_name_of(*p) == Some(HIDDEN)
        })
        .collect();
    if orphans.is_empty() {
        return None;
    }
    let mut cmds = Vec::new();
    for (pane, view) in orphans {
        cmds.extend(spawn_tags(pane, view));
        tmux.view_sent(pane, view.word());
    }
    Some(Effect::Tmux {
        conn,
        cmds,
        token: Some(TmuxToken::Tags),
    })
}

/// What every listing does for the panels: [`heal`] when `heal` (a listing taken while a
/// cockpit chain may be in flight doesn't: its retag could land after the chain's), then
/// [`resolve_panels`]. An orphan stays untagged, so a later listing heals it.
pub(crate) fn listed(state: &mut State, heal_now: bool) -> Vec<Effect> {
    let healed = heal_now
        .then(|| heal(&mut state.tmux, state.panels.values()))
        .flatten();
    healed.into_iter().chain(resolve_panels(state)).collect()
}

/// Another tmux server: its pane ids name other tabs, so no panel has its tab any more
/// (L8), as the views the old tabs had go.
pub(crate) fn server_changed(state: &mut State) -> Option<Effect> {
    for reg in state.panels.values_mut() {
        reg.pane = None;
        reg.managed = false;
    }
    publish_panels(state)
}

/// `ui.here`: the folder the panel's selection is in. Not published.
pub(crate) fn here(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    folder: Option<String>,
) -> Vec<Effect> {
    let result = match state.panels.get_mut(&conn) {
        Some(reg) => {
            reg.here = folder;
            Ok(Done {
                text: String::new(),
                data: json!({}),
            })
        }
        None => Err(OpError(NOT_A_PANEL.into())),
    };
    vec![reply(conn, req, result)]
}

/// The connection closed: its panel goes.
pub(crate) fn gone(state: &mut State, conn: ConnId) -> Option<Effect> {
    state.panels.remove(&conn)?;
    publish_panels(state)
}

/// `panels`, when its rows changed.
pub(crate) fn publish_panels(state: &mut State) -> Option<Effect> {
    state
        .publish_changed(Topic::Panels)
        .map(|m| Effect::Publish(Publish::Model(m)))
}

fn reply(conn: ConnId, req: ReqId, result: Result<Done, OpError>) -> Effect {
    Effect::Reply { conn, req, result }
}
