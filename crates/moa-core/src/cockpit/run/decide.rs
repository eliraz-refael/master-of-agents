use serde_json::json;

use super::{CockpitOp, Select, current, finish, in_cockpit, lost_now, refused, title};
use crate::cockpit::assign::view_of;
use crate::cockpit::build::{occupant_stays, panel_tags};
use crate::cockpit::scene::{AGENT, Place, Role, Scene};
use crate::cockpit::spawn::{respawn, respawn_tail, spawn};
use crate::cockpit::{Plan, plan, swap_in, vacate};
use crate::effect::Effect;
use crate::layout::{LayoutError, Leaf, Node, leaves};
use crate::op::{Done, OpError};
use crate::ops::launch::{GONE, focus_cmds, shown};
use crate::panel::View;
use crate::sleep::kill_pane;
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Asked, NO_TERMINAL};

#[derive(Debug, Default)]
pub(super) enum Step {
    #[default]
    Listing,
    Spawning,
    /// Repair's respawns of these panes.
    Respawning(Vec<PaneId>),
    Chain(Then),
}

/// What a chain's `Ok` records and says.
#[derive(Debug)]
pub(super) struct Then {
    /// The `@moa_view`s it sets (`None`: unsets), recorded at once (L17).
    pub(super) tags: Vec<(PaneId, Option<&'static str>)>,
    /// The panes it takes `@moa_session` off, recorded at once too.
    pub(super) untagged: Vec<PaneId>,
    /// A swap into the spot: where it puts the panes, recorded at once too.
    pub(super) moved: Option<Moved>,
    /// A build's live panels given another view: their rows change.
    pub(super) views: Option<Vec<(PaneId, View)>>,
    /// The panels it starts in place.
    pub(super) starts: u32,
    /// The layout it builds, for its error.
    pub(super) layout: String,
    pub(super) ok: Said,
}

/// Where a swap into the spot (L11) puts the panes: `pane` takes the spot's place, and the
/// spot's occupant takes `pane`'s old one when it `stays` there, else it goes to a new tab.
#[derive(Clone, Copy, Debug)]
pub(super) struct Moved {
    pub(super) pane: PaneId,
    pub(super) spot: PaneId,
    pub(super) stays: bool,
}

/// How an operation ends.
#[derive(Debug)]
pub(super) enum Said {
    /// `layout <name> applied`, after the brain's current layout is saved.
    Applied(String),
    Reply(Result<Done, OpError>),
    Logged(String),
    Nothing,
}

/// What an operation does next, from its listing.
pub(super) enum Next {
    Done(Said),
    Spawn(Vec<View>),
    Respawn(Vec<(PaneId, View)>),
    Chain(Vec<Cmd>, Then),
    /// A vacate's chain that takes its agent down, then, once that is answered `Ok`, the
    /// rest (the empty slot's tags), as a chain of its own: tmux's one answer for a chain
    /// can't tell which command failed (R24 asks whether the agent went).
    TakeDown(Vec<Cmd>, Then, Option<(Vec<Cmd>, Then)>),
}

/// A layout that can't be built, as said to whoever asked for it. While `layouts.json`
/// doesn't parse, a name it doesn't have says so (L2).
pub(super) fn layout_refused(state: &State, e: LayoutError) -> String {
    match (&state.layouts.file_error, e) {
        (Some(why), LayoutError::NoSuch { .. }) => format!("layouts.json has an error: {why}"),
        (_, e) => e.to_string(),
    }
}

/// L9: what the lazy build looks for, a user's terminal and a cockpit (in session `moa`)
/// with no panel in it.
pub(super) fn wants_lazy(scene: &Scene) -> bool {
    scene.moa_session && scene.users && !scene.in_cockpit().any(|at| at.tagged)
}

/// What `op` does next in `scene`.
pub(super) fn decide(state: &State, op: &CockpitOp, scene: &Scene) -> Next {
    let planned = state.cockpit.replanned;
    match op {
        CockpitOp::Build { name, select, .. } => {
            if !scene.moa_session {
                return Next::Done(refused(op, NO_TERMINAL.into()));
            }
            // No name is the current layout, a broken entry falling back to the preset (L2).
            let (layout, node) = match name {
                None => current(state),
                Some(name) => match state.layouts.node(name) {
                    Ok(node) => (name.clone(), node),
                    Err(e) => return Next::Done(refused(op, layout_refused(state, e))),
                },
            };
            match select {
                Select::Default => build(state, op, scene, layout, &node, Some(*select)),
                Select::Leaf(leaf) => {
                    if let Some(pane) = in_cockpit(scene, *leaf) {
                        return Next::Chain(
                            focus_cmds(pane).to_vec(),
                            Then {
                                tags: Vec::new(),
                                untagged: Vec::new(),
                                moved: None,
                                views: None,
                                starts: 0,
                                layout,
                                ok: went_to(*leaf),
                            },
                        );
                    }
                    if !leaves(&node).contains(leaf) {
                        let text =
                            format!("the {layout} layout has no {leaf} panel (moa layout list)");
                        return Next::Done(refused(op, text));
                    }
                    build(state, op, scene, layout, &node, Some(*select))
                }
            }
        }
        CockpitOp::Repair { .. } => {
            if !scene.moa_session {
                return Next::Done(refused(op, NO_TERMINAL.into()));
            }
            if state.cockpit.restarted.is_none() {
                // Stopped panels only: an agent is never restarted here.
                let dead: Vec<(PaneId, View)> = scene
                    .panes
                    .iter()
                    .filter(|at| at.dead)
                    .filter_map(|at| Some((at.pane, at.panel_view()?)))
                    .collect();
                if !dead.is_empty() {
                    return Next::Respawn(dead);
                }
            }
            let (layout, node) = current(state);
            build(state, op, scene, layout, &node, None)
        }
        CockpitOp::Lazy => {
            if !planned && !wants_lazy(scene) {
                return Next::Done(Said::Nothing);
            }
            let (layout, node) = current(state);
            build(state, op, scene, layout, &node, None)
        }
        CockpitOp::Refill => {
            if !planned && (!scene.moa_session || scene.cockpit.is_none()) {
                return Next::Done(Said::Nothing);
            }
            let (layout, node) = current(state);
            build(state, op, scene, layout, &node, None)
        }
        CockpitOp::Vacate { pane, id } => {
            let Some(at) = scene.pane(*pane) else {
                return Next::Done(Said::Nothing);
            };
            // Still the agent's: its session's, or no one's. `kill_tab`'s commit deleted the
            // binding, and the session tag may never have landed; a pane id names one pane
            // on its server, and a vacate never runs on another. Not a panel's: its chain
            // already ran (a vacate queued again after a lost link or a reload finds the
            // empty slot it left).
            let ours = match at.role {
                Role::Spot(Some(s)) | Role::Agent(s) => s == *id,
                Role::Other => true,
                Role::Panel(_) | Role::Spot(None) => false,
            };
            if !ours {
                return Next::Done(Said::Nothing);
            }
            // The spot's occupant, tagged `agent` there, with or without its session tag (an
            // agent with a session and another view isn't the spot's).
            let in_spot = at.place == Place::Cockpit
                && (matches!(at.role, Role::Spot(Some(_)))
                    || (matches!(at.role, Role::Other) && at.tagged));
            let then = |tags, untagged, starts| Then {
                tags,
                untagged,
                moved: None,
                views: None,
                starts,
                layout: String::new(),
                ok: Said::Nothing,
            };
            if !in_spot {
                return Next::TakeDown(
                    vec![kill_pane(*pane)],
                    then(Vec::new(), Vec::new(), 0),
                    None,
                );
            }
            let v = vacate(scene, *pane, &respawn_tail(state, View::EmptySlot));
            let cmds = v.chain.parts().to_vec();
            if !v.respawned {
                // A spare one swapped in already is the slot, and the agent's pane is gone: no
                // longer the spot's, so nothing reads an exit of it as the spot's agent ending.
                return Next::TakeDown(cmds, then(vec![(*pane, None)], Vec::new(), 0), None);
            }
            // In place: the respawn takes the agent down (a panel start) and its pane is no
            // longer a session's tab, then the spot is tagged as the empty slot.
            let (respawn, tags) = cmds.split_at(v.taken_down);
            let slot = then(vec![(*pane, Some(View::EmptySlot.word()))], Vec::new(), 0);
            Next::TakeDown(
                respawn.to_vec(),
                then(Vec::new(), vec![*pane], 1),
                Some((tags.to_vec(), slot)),
            )
        }
        CockpitOp::SelectView { pane, view, .. } => {
            // Only a pane moa tagged as a panel; never the spot (its empty slot or its agent),
            // a spare empty slot parked for it (a vacate's), an agent's or a stranger's.
            let at = scene.pane(*pane);
            if at.is_some_and(|at| matches!(at.role, Role::Spot(_))) {
                let text = format!("{pane} is the agent spot; it can't show another view");
                return Next::Done(refused(op, text));
            }
            match at.and_then(|at| at.panel_view()) {
                None => return Next::Done(refused(op, format!("{pane} isn't a moa panel"))),
                Some(View::EmptySlot) => {
                    let text = format!("{pane} is a spare agent spot; it can't show another view");
                    return Next::Done(refused(op, text));
                }
                Some(_) => {}
            }
            let done = Done {
                text: String::new(),
                data: json!({ "pane": pane.to_string(), "view": view }),
            };
            Next::Chain(
                panel_tags(*pane, *view),
                Then {
                    tags: vec![(*pane, Some(view.word()))],
                    untagged: Vec::new(),
                    moved: None,
                    views: Some(vec![(*pane, *view)]),
                    starts: 0,
                    layout: String::new(),
                    ok: Said::Reply(Ok(done)),
                },
            )
        }
        CockpitOp::Show { pane, .. } => {
            // Its tab closed while the show waited (or holds no agent now): nothing moves. A
            // launch's show has nobody to tell (its caller heard already): it is logged.
            let Some(id) = scene.pane(*pane).and_then(|at| at.agent()) else {
                return Next::Done(refused(op, GONE.into()));
            };
            let then = |tags, moved: Option<Moved>| Then {
                tags,
                untagged: Vec::new(),
                moved,
                views: None,
                starts: 0,
                layout: String::new(),
                ok: Said::Reply(Ok(shown(state, id, moved.is_some()))),
            };
            let swap = swap_in(scene, *pane, |id| title(state, id));
            match (swap, scene.spot(), scene.pane(*pane)) {
                (Some(chain), Some(old), Some(at)) => {
                    let mut cmds = chain.parts().to_vec();
                    cmds.extend(focus_cmds(*pane));
                    // The spot's new agent, and its old occupant: an empty slot stays one,
                    // parked; an agent goes back to a tab, without the spot's view.
                    let view = old.agent().is_none().then(|| View::EmptySlot.word());
                    let tags = vec![(*pane, Some(AGENT)), (old.pane, view)];
                    let moved = Moved {
                        pane: *pane,
                        spot: old.pane,
                        stays: occupant_stays(old, at),
                    };
                    Next::Chain(cmds, then(tags, Some(moved)))
                }
                // No spot, or it is the spot already: 4b's select.
                _ => Next::Chain(focus_cmds(*pane).to_vec(), then(Vec::new(), None)),
            }
        }
    }
}

/// `goto`'s answer: the leaf's word.
fn went_to(leaf: Leaf) -> Said {
    Said::Reply(Ok(Done {
        text: leaf.word().to_string(),
        data: json!({ "panel": leaf }),
    }))
}

/// The build of `layout` in `scene`: its panels started first (once), else its chain, with
/// the select asked for.
fn build(
    state: &State,
    op: &CockpitOp,
    scene: &Scene,
    layout: String,
    node: &Node,
    select: Option<Select>,
) -> Next {
    let Plan { spawn, build } = plan(scene, node, |id| title(state, id));
    if let Some(view) = spawn.first() {
        if state.cockpit.replanned {
            let text = format!("couldn't start the {view} panel; see panel.log");
            return Next::Done(refused(op, text));
        }
        return Next::Spawn(spawn);
    }
    let Some(b) = build else {
        return Next::Done(refused(op, NO_TERMINAL.into()));
    };
    let mut cmds = b.chain.parts().to_vec();
    let pane_of = |leaf: Leaf| b.leaves.iter().find(|(l, _)| *l == leaf).map(|(_, p)| *p);
    let selected = match select {
        None => None,
        Some(Select::Default) => {
            pane_of(Leaf::Panel(View::Console)).or(b.leaves.first().map(|(_, p)| *p))
        }
        Some(Select::Leaf(leaf)) => pane_of(leaf),
    };
    if let Some(pane) = selected {
        cmds.extend(focus_cmds(pane));
    }
    // The views it tags each leaf with, and the ones it unsets on the panes it sends home.
    let mut tags: Vec<(PaneId, Option<&'static str>)> = b
        .leaves
        .iter()
        .map(|(leaf, pane)| {
            let agent = scene.pane(*pane).and_then(|at| at.agent()).is_some();
            let view = match leaf {
                Leaf::Agent if agent => AGENT,
                leaf => view_of(*leaf).word(),
            };
            (*pane, Some(view))
        })
        .collect();
    for at in scene.in_cockpit() {
        let used = b.leaves.iter().any(|(_, p)| *p == at.pane);
        if !used && (at.agent().is_some() || (at.role == Role::Other && at.tagged)) {
            tags.push((at.pane, None));
        }
    }
    let ok = match (op, select) {
        (CockpitOp::Build { .. }, Some(Select::Leaf(leaf))) => went_to(leaf),
        (CockpitOp::Build { .. }, _) => Said::Applied(layout.clone()),
        (CockpitOp::Repair { .. }, _) => {
            let text = match state.cockpit.restarted.unwrap_or(0) {
                0 => format!("layout {layout} applied"),
                1 => "repaired: 1 panel restarted".to_string(),
                n => format!("repaired: {n} panels restarted"),
            };
            Said::Reply(Ok(Done {
                text,
                data: json!({ "name": layout, "restarted": state.cockpit.restarted.unwrap_or(0) }),
            }))
        }
        _ => Said::Nothing,
    };
    Next::Chain(
        cmds,
        Then {
            tags,
            untagged: Vec::new(),
            moved: None,
            views: Some(b.views),
            starts: 0,
            layout,
            ok,
        },
    )
}

/// Does the in-flight operation's next step.
pub(super) fn act(state: &mut State, seq: u64, next: Next) -> Vec<Effect> {
    match next {
        Next::Done(said) => finish(state, said),
        Next::Spawn(views) => {
            state.cockpit.replanned = true;
            state.cockpit.step = Step::Spawning;
            match spawn(state, views, seq) {
                Some(e) => vec![e],
                None => lost_now(state),
            }
        }
        Next::Respawn(dead) => {
            let cmds = dead.iter().map(|(p, v)| respawn(state, *p, *v)).collect();
            state.cockpit.step = Step::Respawning(dead.iter().map(|(p, _)| *p).collect());
            match state.tmux.ask(Asked::Chain { seq }, cmds) {
                Some(e) => vec![e],
                None => lost_now(state),
            }
        }
        Next::Chain(cmds, then) => {
            state.cockpit.step = Step::Chain(then);
            match state.tmux.ask(Asked::Chain { seq }, vec![Cmd::chain(cmds)]) {
                Some(e) => vec![e],
                None => lost_now(state),
            }
        }
        Next::TakeDown(cmds, then, rest) => {
            state.cockpit.plain_kill = matches!(&cmds[..], [c] if c.name() == "kill-pane");
            state.cockpit.rest = rest;
            act(state, seq, Next::Chain(cmds, then))
        }
    }
}
