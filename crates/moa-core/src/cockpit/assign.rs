//! Which pane plays each leaf (L5): the spot's occupant, then exact matches, then any spare
//! live panel, reassigned. What is still missing is started.

use std::collections::BTreeSet;

use super::scene::{PaneAt, Place, Scene};
use crate::layout::Leaf;
use crate::panel::View;
use crate::tmux::ids::PaneId;

/// The view a leaf's panel shows: the agent spot's is the empty slot.
pub(crate) fn view_of(leaf: Leaf) -> View {
    match leaf {
        Leaf::Panel(view) => view,
        Leaf::Agent => View::EmptySlot,
    }
}

pub(crate) struct Assignment {
    /// Each leaf, in layout order, with its pane: `None` needs a new panel.
    pub leaves: Vec<(Leaf, Option<PaneId>)>,
    /// The live panels given another view.
    pub views: Vec<(PaneId, View)>,
}

/// The panes for `layout`'s leaves (in layout order), as L5 picks them.
pub(crate) fn assign(scene: &Scene, layout: &[Leaf]) -> Assignment {
    // Panels a leaf can take: in the cockpit first, then parked; elsewhere, never.
    let mut panels: Vec<&PaneAt> = scene
        .panes
        .iter()
        .filter(|at| at.panel_view().is_some())
        .filter(|at| matches!(at.place, Place::Cockpit | Place::Hidden))
        .collect();
    panels.sort_by_key(|at| at.place != Place::Cockpit);
    let mut taken: BTreeSet<PaneId> = BTreeSet::new();
    let mut leaves: Vec<(Leaf, Option<PaneId>)> = layout.iter().map(|l| (*l, None)).collect();
    let mut views = Vec::new();

    // 1. The spot: its occupant, even a dead one; else a spare live empty slot.
    if let Some(at) = leaves.iter().position(|(l, _)| *l == Leaf::Agent) {
        let slot = scene.spot().or_else(|| {
            panels
                .iter()
                .find(|p| !p.dead && p.panel_view() == Some(View::EmptySlot))
                .copied()
        });
        if let Some(slot) = slot {
            taken.insert(slot.pane);
            leaves[at].1 = Some(slot.pane);
        }
    }

    // 2. Each panel leaf: a panel showing exactly its view, live first, then dead. A dead
    // panel is taken here only, so it keeps its own leaf.
    for (leaf, pane) in leaves.iter_mut() {
        let Leaf::Panel(view) = *leaf else { continue };
        if pane.is_some() {
            continue;
        }
        let exact = |dead: bool| {
            panels.iter().find(|p| {
                p.dead == dead && p.panel_view() == Some(view) && !taken.contains(&p.pane)
            })
        };
        if let Some(p) = exact(false).or_else(|| exact(true)) {
            taken.insert(p.pane);
            *pane = Some(p.pane);
        }
    }

    // 3. Every leaf still empty: any spare live panel, given the leaf's view.
    for (leaf, pane) in leaves.iter_mut() {
        if pane.is_some() {
            continue;
        }
        if let Some(p) = panels.iter().find(|p| !p.dead && !taken.contains(&p.pane)) {
            taken.insert(p.pane);
            *pane = Some(p.pane);
            let view = view_of(*leaf);
            if p.panel_view() != Some(view) {
                views.push((p.pane, view));
            }
        }
    }

    // 4. The rest are started by the caller, parked, before the build.
    Assignment { leaves, views }
}
