//! Panels: the clients that draw a view in a tab's pane. Each tells the core which pane it
//! is and which view it shows (`ui.panel`), and which folder its selection is in
//! (`ui.here`). The registry lives in the core's memory, by connection.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::effect::{Effect, Publish};
use crate::model::UnknownValue;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::publish::Topic;
use crate::state::State;
use crate::tmux::ids::PaneId;

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

    /// Whether the panel draws its own frame: the spike's `FRAMED`.
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

/// One panel's registration: its pane (none outside moa's terminal), its view, and the
/// folder its selection is in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelReg {
    pub pane: Option<PaneId>,
    pub view: View,
    pub here: Option<String>,
}

/// What `ui.panel` says to a tab id that isn't one (`%<digits>`).
const BAD_PANE: &str = "a panel's tab id looks like %5";

/// What `ui.here` says before the connection said which panel it is.
const NOT_A_PANEL: &str = "say which panel this is first (ui.panel)";

/// The key of a connection's panel row: `c<conn>`.
pub(crate) fn key_of(conn: ConnId) -> String {
    format!("c{}", conn.0)
}

/// `ui.panel`: registers `conn` as the panel in `pane` showing `view`, else the view this
/// pane last had, else the picture. A panel another connection held in that pane is dropped.
pub(crate) fn register(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    pane: Option<&str>,
    view: Option<View>,
) -> Vec<Effect> {
    let pane = match pane.map(str::parse::<PaneId>).transpose() {
        Ok(p) => p,
        Err(_) => return vec![reply(conn, req, Err(OpError(BAD_PANE.into())))],
    };
    let view = view
        .or_else(|| pane.and_then(|p| state.pane_views.get(&p).copied()))
        .unwrap_or(View::Picture);
    if let Some(p) = pane {
        state.panels.retain(|c, r| *c == conn || r.pane != Some(p));
        state.pane_views.insert(p, view);
    }
    // The same panel saying again which it is keeps its folder.
    let here = state.panels.remove(&conn).and_then(|r| r.here);
    state.panels.insert(conn, PanelReg { pane, view, here });
    let mut fx: Vec<Effect> = publish_panels(state).into_iter().collect();
    fx.push(reply(
        conn,
        req,
        Ok(Done {
            text: String::new(),
            data: json!({"key": key_of(conn), "view": view}),
        }),
    ));
    fx
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
