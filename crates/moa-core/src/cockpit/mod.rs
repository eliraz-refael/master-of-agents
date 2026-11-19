//! The cockpit engine: moa's first tab, `moa` in session `moa`, holding the current layout's
//! panels around the agent spot. Pure: from a `Scene` (a fresh listing) and a layout, which
//! panels to start, and the one chain of tmux commands that moves panes into place, parks
//! the rest, resizes and tags (L5, L6, L10). The spot's swap and vacate are chains too (L11).
//! The core runs them one at a time, each from its own listing (L17).

mod assign;
mod build;
mod scene;

use std::collections::BTreeSet;

pub use scene::{PaneAt, Place, Role, Scene};

use crate::layout::{Leaf, Node, leaves};
use crate::model::SessionId;
use crate::panel::View;
use crate::tmux::cmd::Cmd;
use crate::tmux::ids::PaneId;

/// What a layout needs: panels to start first, or the build.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// The views to start, parked, before the build (L5 step 4), in layout order.
    pub spawn: Vec<View>,
    /// `None` while `spawn` isn't empty, and when there is no session `moa`.
    pub build: Option<Build>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Build {
    /// One chain: swap, break, join, resize, tag.
    pub chain: Cmd,
    /// The live panels it gives another view (L5 step 3): published as row changes.
    pub views: Vec<(PaneId, View)>,
    /// Each leaf's pane, in layout order: the caller selects from these (L6 step 7).
    pub leaves: Vec<(Leaf, PaneId)>,
    /// The agent leaf's pane, when the layout has one.
    pub spot: Option<PaneId>,
}

/// L5, L6 and L10: which pane plays each of `layout`'s leaves, and the chain that builds it
/// in place. `title` names an agent's tab and frame (cut to 24 characters).
pub fn plan(scene: &Scene, layout: &Node, title: impl Fn(SessionId) -> String) -> Plan {
    if !scene.moa_session {
        return Plan {
            spawn: Vec::new(),
            build: None,
        };
    }
    let assigned = assign::assign(scene, &leaves(layout));
    let spawn: Vec<View> = assigned
        .leaves
        .iter()
        .filter(|(_, pane)| pane.is_none())
        .map(|(leaf, _)| assign::view_of(*leaf))
        .collect();
    if !spawn.is_empty() {
        return Plan { spawn, build: None };
    }
    let leaves: Vec<(Leaf, PaneId)> = assigned
        .leaves
        .iter()
        .filter_map(|(leaf, pane)| pane.map(|p| (*leaf, p)))
        .collect();
    debug_assert_eq!(
        leaves
            .iter()
            .map(|(_, p)| *p)
            .collect::<BTreeSet<_>>()
            .len(),
        leaves.len(),
        "one pane per leaf"
    );
    let chain = build::chain(scene, layout, &leaves, &title);
    let spot = leaves
        .iter()
        .find_map(|(leaf, pane)| (*leaf == Leaf::Agent).then_some(*pane));
    Plan {
        spawn,
        build: Some(Build {
            chain,
            views: assigned.views,
            leaves,
            spot,
        }),
    }
}

/// L11: the agent in `pane` swapped into the spot, and the old occupant sent home, as one
/// chain (with no select: the caller adds it). `None` when there's no spot, when `pane`
/// already is the spot, or when `pane` isn't an agent's.
pub fn swap_in(scene: &Scene, pane: PaneId, title: impl Fn(SessionId) -> String) -> Option<Cmd> {
    build::swap_in(scene, pane, &title)
}

/// L11: the spot's agent in `spot` taken out, leaving the empty slot: a spare one parked is
/// swapped in and the agent killed, else the spot is respawned in place. `respawn` is the
/// rest of `respawn-pane -k -t <spot>`: `-c <home> -e K=V … <exe> panel empty-slot --brain <b>`.
pub fn vacate(scene: &Scene, spot: PaneId, respawn: &[String]) -> Cmd {
    build::vacate(scene, spot, respawn)
}
