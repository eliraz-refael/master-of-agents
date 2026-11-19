//! A `Scene`: what a fresh listing says about every pane, as the cockpit engine needs it.

use crate::model::SessionId;
use crate::panel::View;
use crate::tmux::ids::{PaneId, WindowId};
use crate::tmux::topology::Topology;

/// The session the user's terminal shows, which holds the cockpit and the agents' tabs.
pub(crate) const MOA: &str = "moa";
/// The hidden session where panels wait, parked.
pub(crate) const HIDDEN: &str = "_moa";
/// The `@moa_view` of the agent in the spot.
pub(crate) const AGENT: &str = "agent";

/// What a pane is to the cockpit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// A panel showing a view (an `empty-slot` panel outside the cockpit included).
    Panel(View),
    /// The cockpit's spot occupant: an agent tagged `@moa_view agent` (`Some`), or an
    /// `empty-slot` panel (`None`).
    Spot(Option<SessionId>),
    /// An agent anywhere else, tagged `agent` or not: a pane with a binding or a
    /// `@moa_session` tag.
    Agent(SessionId),
    /// Anything else: a shell, a user's program, or a pane tagged `agent` that isn't one.
    Other,
}

/// Where a pane is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// The cockpit window: `moa` in session `moa`.
    Cockpit,
    /// Another window of session `moa`: an agent's or a terminal's tab.
    Moa,
    /// Session `_moa`: parked.
    Hidden,
    /// Any other session.
    Elsewhere,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneAt {
    pub pane: PaneId,
    pub window: WindowId,
    pub place: Place,
    pub dead: bool,
    pub role: Role,
    /// The pane carries a `@moa_view` tag, whatever its role: an `Other` that has one has
    /// it unset as it goes home (L6 step 2).
    pub tagged: bool,
}

impl PaneAt {
    /// The view of a panel, the spot's `empty-slot` included.
    pub(crate) fn panel_view(&self) -> Option<View> {
        match self.role {
            Role::Panel(view) => Some(view),
            Role::Spot(None) => Some(View::EmptySlot),
            _ => None,
        }
    }

    /// The session of an agent, in the spot or not.
    pub(crate) fn agent(&self) -> Option<SessionId> {
        match self.role {
            Role::Spot(Some(id)) | Role::Agent(id) => Some(id),
            _ => None,
        }
    }
}

/// Every pane, as a fresh listing (L17) shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scene {
    pub panes: Vec<PaneAt>,
    /// Session `moa` exists.
    pub moa_session: bool,
    pub cockpit: Option<WindowId>,
    /// A user's terminal is attached.
    pub users: bool,
}

impl Scene {
    /// The scene in a topology just rebuilt from a listing. A pane is an agent when `bound`
    /// gives its session, else when its `@moa_session` tag does (L6 step 2).
    pub fn read(topology: &Topology, bound: impl Fn(PaneId) -> Option<SessionId>) -> Scene {
        let cockpit = topology.cockpit();
        let panes = topology
            .panes()
            .filter_map(|pane| {
                let window = topology.window_of(pane)?;
                let place = if Some(window) == cockpit {
                    Place::Cockpit
                } else {
                    match topology.session_name_of(pane) {
                        Some(MOA) => Place::Moa,
                        Some(HIDDEN) => Place::Hidden,
                        _ => Place::Elsewhere,
                    }
                };
                let view = topology.view_of(pane);
                let agent = bound(pane).or_else(|| topology.moa_session(pane));
                let in_cockpit = place == Place::Cockpit;
                let role = match (agent, view) {
                    (Some(id), Some(AGENT)) if in_cockpit => Role::Spot(Some(id)),
                    (Some(id), _) => Role::Agent(id),
                    (None, Some(view)) => match view.parse::<View>() {
                        Ok(View::EmptySlot) if in_cockpit => Role::Spot(None),
                        Ok(view) => Role::Panel(view),
                        Err(_) => Role::Other,
                    },
                    (None, None) => Role::Other,
                };
                Some(PaneAt {
                    pane,
                    window,
                    place,
                    dead: topology.is_dead(pane),
                    role,
                    tagged: view.is_some(),
                })
            })
            .collect();
        Scene {
            panes,
            moa_session: topology.session_named(MOA).is_some(),
            cockpit,
            users: topology.users_attached(),
        }
    }

    /// The pane `p`, if the listing has it.
    pub fn pane(&self, p: PaneId) -> Option<&PaneAt> {
        self.panes.iter().find(|at| at.pane == p)
    }

    /// The spot's occupant: an agent before an empty slot, should the user's own moves have
    /// left both in the cockpit; else the first listed.
    pub fn spot(&self) -> Option<&PaneAt> {
        let spot = |agent: bool| {
            self.in_cockpit()
                .find(|at| matches!(at.role, Role::Spot(id) if id.is_some() == agent))
        };
        spot(true).or_else(|| spot(false))
    }

    /// The panes in the cockpit, in the listing's order.
    pub(crate) fn in_cockpit(&self) -> impl Iterator<Item = &PaneAt> {
        self.panes.iter().filter(|at| at.place == Place::Cockpit)
    }
}
