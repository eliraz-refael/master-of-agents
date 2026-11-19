//! The chains: a layout built in place (L6), its tags (L10), and the agent spot's swap and
//! vacate (L11). Each is one `Cmd::chain`, so tmux stops at the first command that fails.

use std::collections::BTreeSet;

use super::assign::view_of;
use super::scene::{AGENT, HIDDEN, MOA, PaneAt, Place, Role, Scene};
use crate::layout::{Axis, Leaf, Node, Size};
use crate::model::SessionId;
use crate::panel::View;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::ids::PaneId;
use crate::tmux::link::{set_pane_option, unset_pane_option};

/// The spike's cut for a title on a tab or a frame.
const TITLE_CHARS: usize = 24;
/// The tab a stranger leaving the cockpit gets, and its frame's label.
const SHELL: &str = "⌨ shell";
/// A framed panel's frame colour, idle and focused (the spike's `FRAME_IDLE`/`FRAME_ACTIVE`).
const FRAME_IDLE: &str = "fg=#4c566a";
const FRAME_ACTIVE: &str = "fg=#88c0d0";
/// What a stopped panel shows (L7).
const STOPPED: &str = "panel stopped · Ctrl-a r brings it back";

/// A session's title, as a tab or a frame shows it: cut to 24 characters.
fn cut(title: String) -> String {
    title.chars().take(TITLE_CHARS).collect()
}

/// An agent's tab name: its title, cut. Never `moa`, which in session `moa` would read as
/// the cockpit (after a rebuild made the cockpit a newer window than the agent's tab).
fn tab_name(title: String) -> String {
    match cut(title) {
        name if name == MOA => format!("{MOA} (agent)"),
        name => name,
    }
}

fn swap(src: PaneId, dst: PaneId) -> Cmd {
    Cmd::new("swap-pane").args(["-d", "-s", &src.to_string(), "-t", &dst.to_string()])
}

fn join(axis: Axis, src: PaneId, dst: PaneId) -> Cmd {
    let flag = match axis {
        Axis::H => "-h",
        Axis::V => "-v",
    };
    Cmd::new("join-pane").args(["-d", flag, "-s", &src.to_string(), "-t", &dst.to_string()])
}

/// `break-pane` to session `to`; `name` is taken literally.
fn break_to(pane: PaneId, to: &str, name: Option<&str>) -> Cmd {
    let cmd =
        Cmd::new("break-pane").args(["-d", "-s", &pane.to_string(), "-t", &format!("={to}:")]);
    match name {
        Some(name) => cmd.args(["-n", name]),
        None => cmd,
    }
}

fn resize(axis: Axis, pane: PaneId, size: Size) -> Cmd {
    let flag = match axis {
        Axis::H => "-x",
        Axis::V => "-y",
    };
    Cmd::new("resize-pane").args(["-t", &pane.to_string(), flag, &size.arg()])
}

fn kill(pane: PaneId) -> Cmd {
    Cmd::new("kill-pane").args(["-t", &pane.to_string()])
}

/// The frame's three options, set for a framed panel and unset for anything else.
fn frame(pane: PaneId, framed: bool) -> Vec<Cmd> {
    let options = [
        ("@moa_framed", "1"),
        ("window-style", FRAME_IDLE),
        ("window-active-style", FRAME_ACTIVE),
    ];
    options
        .iter()
        .map(|(name, value)| match framed {
            true => set_pane_option(pane, name, value),
            false => unset_pane_option(pane, name),
        })
        .collect()
}

/// A panel's tags (L10): its view, its label, and its frame.
pub(crate) fn panel_tags(pane: PaneId, view: View) -> Vec<Cmd> {
    let mut cmds = vec![
        set_pane_option(pane, "@moa_view", view.word()),
        set_pane_option(pane, "@moa_label", &literal_format(view.label())),
    ];
    cmds.extend(frame(pane, view.framed()));
    cmds
}

/// The tags of the agent in the spot (L10): `agent`, `● <title>`, and no frame.
fn agent_tags(pane: PaneId, title: &str) -> Vec<Cmd> {
    let mut cmds = vec![
        set_pane_option(pane, "@moa_view", AGENT),
        set_pane_option(pane, "@moa_label", &format!("● {}", literal_format(title))),
    ];
    cmds.extend(frame(pane, false));
    cmds
}

/// A panel's pane stays when its program stops, and says so (L7).
pub(crate) fn keep_on_exit(pane: PaneId) -> Vec<Cmd> {
    vec![
        set_pane_option(pane, "remain-on-exit", "on"),
        set_pane_option(pane, "remain-on-exit-format", STOPPED),
    ]
}

/// The tags an agent leaving the spot loses: its tab's name shows on its frame again.
fn untag_agent(pane: PaneId) -> [Cmd; 2] {
    [
        unset_pane_option(pane, "@moa_view"),
        unset_pane_option(pane, "@moa_label"),
    ]
}

/// The session a pane belongs in outside the cockpit: a panel's is `_moa`.
fn home_of(at: &PaneAt) -> &'static str {
    match at.panel_view() {
        Some(_) => HIDDEN,
        None => MOA,
    }
}

/// L6 step 2: a pane the layout doesn't use goes home. A panel is parked; an agent gets a
/// tab named by its title; anything else is a terminal, `⌨ shell`. Only a pane with no
/// session gets `@moa_term`.
fn go_home(at: &PaneAt, title: &dyn Fn(SessionId) -> String) -> Vec<Cmd> {
    let p = at.pane;
    if at.panel_view().is_some() {
        return vec![break_to(p, HIDDEN, None)];
    }
    if let Some(id) = at.agent() {
        let mut cmds = vec![break_to(p, MOA, Some(&tab_name(title(id))))];
        cmds.extend(untag_agent(p));
        return cmds;
    }
    let mut cmds = vec![
        break_to(p, MOA, Some(SHELL)),
        set_pane_option(p, "@moa_term", "1"),
        set_pane_option(p, "@moa_label", &literal_format(SHELL)),
    ];
    if at.tagged {
        cmds.push(unset_pane_option(p, "@moa_view"));
    }
    cmds
}

/// The session a place is in.
fn session_of(place: Place) -> Option<&'static str> {
    match place {
        Place::Cockpit | Place::Moa => Some(MOA),
        Place::Hidden => Some(HIDDEN),
        Place::Elsewhere => None,
    }
}

/// The number of leaves under `n`.
fn count(n: &Node) -> usize {
    match n {
        Node::Leaf { .. } => 1,
        Node::Split { children, .. } => children.iter().map(count).sum(),
    }
}

/// Each child, with its leaves' panes (a slice of `panes`, which are `n`'s in order).
fn cells<'a>(children: &'a [Node], panes: &'a [PaneId]) -> Vec<(&'a Node, &'a [PaneId])> {
    let mut start = 0;
    children
        .iter()
        .map(|child| {
            let n = count(child);
            let cell = (child, &panes[start..start + n]);
            start += n;
            cell
        })
        .collect()
}

/// L6 step 4, breadth first: each later child's first pane after the previous child's,
/// then each child inside its own cell.
fn joins(n: &Node, panes: &[PaneId], out: &mut Vec<Cmd>) {
    let Node::Split { axis, children, .. } = n else {
        return;
    };
    let cells = cells(children, panes);
    for pair in cells.windows(2) {
        out.push(join(*axis, pair[1].1[0], pair[0].1[0]));
    }
    for (child, panes) in cells {
        joins(child, panes, out);
    }
}

/// L6 step 5, children first: after a split's children, the first pane of each sized one.
fn resizes(n: &Node, panes: &[PaneId], out: &mut Vec<Cmd>) {
    let Node::Split { axis, children, .. } = n else {
        return;
    };
    let cells = cells(children, panes);
    for (child, panes) in &cells {
        resizes(child, panes, out);
    }
    for (child, panes) in cells {
        let (Node::Split { size, .. } | Node::Leaf { size, .. }) = child;
        if let Some(size) = size {
            out.push(resize(*axis, panes[0], *size));
        }
    }
}

/// The chain that builds `layout` in place (L6) with `leaves`' panes (all in the scene, in
/// layout order), then tags them (L10). Nothing is selected and nothing is killed.
pub(crate) fn chain(
    scene: &Scene,
    layout: &Node,
    leaves: &[(Leaf, PaneId)],
    title: &dyn Fn(SessionId) -> String,
) -> Cmd {
    let assigned: BTreeSet<PaneId> = leaves.iter().map(|(_, p)| *p).collect();
    let panes: Vec<PaneId> = leaves.iter().map(|(_, p)| *p).collect();
    let keeper = panes[0];
    let at = |p: PaneId| scene.pane(p).expect("an assigned pane is in the scene");
    let mut cmds = Vec::new();

    // 1. The keeper: the first leaf's pane, in the cockpit window.
    let mut swapped = None;
    match scene.cockpit {
        // The window it is in becomes the cockpit.
        None => cmds.push(break_to(keeper, MOA, Some(MOA))),
        Some(_) if at(keeper).place == Place::Cockpit => {}
        Some(_) => {
            // A cockpit pane the layout doesn't use first, a stranger first of those. Among
            // the ones it uses, a panel before the spot's agent: should the chain stop
            // before C's join brings it back, C waits in `_moa`, and an agent mustn't.
            let c = scene
                .in_cockpit()
                .min_by_key(|c| {
                    (
                        assigned.contains(&c.pane),
                        c.agent().is_some(),
                        c.role != Role::Other,
                    )
                })
                .expect("a window holds a pane");
            cmds.push(swap(keeper, c.pane));
            // 3. C now sits where the keeper was. In the wrong session, it goes home at
            // once, so a chain that stops later never leaves a stranger in `_moa`.
            if !assigned.contains(&c.pane) && session_of(at(keeper).place) != Some(home_of(c)) {
                cmds.extend(go_home(c, title));
            }
            swapped = Some(c.pane);
        }
    }

    // 2. Clear out what the layout doesn't use.
    for c in scene.in_cockpit() {
        if !assigned.contains(&c.pane) && Some(c.pane) != swapped {
            cmds.extend(go_home(c, title));
        }
    }

    // 4. Join, then 5. resize.
    joins(layout, &panes, &mut cmds);
    resizes(layout, &panes, &mut cmds);

    // 6. Tag.
    for (leaf, pane) in leaves {
        match (leaf, at(*pane).agent()) {
            (Leaf::Agent, Some(id)) => cmds.extend(agent_tags(*pane, &cut(title(id)))),
            _ => cmds.extend(panel_tags(*pane, view_of(*leaf))),
        }
    }
    Cmd::chain(cmds)
}

/// L11's swap of the agent in `pane` into the spot, then the old occupant's way home: an
/// empty slot is parked; an agent stays in `pane`'s old tab, which takes its title. `None`
/// with no spot, when `pane` already is the spot, or when it isn't an agent's.
pub(crate) fn swap_in(
    scene: &Scene,
    pane: PaneId,
    title: &dyn Fn(SessionId) -> String,
) -> Option<Cmd> {
    let spot = scene.spot()?;
    let at = scene.pane(pane)?;
    let id = at.agent()?;
    if spot.pane == pane {
        return None;
    }
    let mut cmds = vec![swap(pane, spot.pane)];
    match spot.agent() {
        // An agent from the spot keeps a tab of moa's: `pane`'s, named by its title.
        Some(old) if at.place == Place::Moa => {
            cmds.push(Cmd::new("rename-window").args([
                "-t",
                &spot.pane.to_string(),
                "--",
                &literal_format(&tab_name(title(old))),
            ]));
            cmds.extend(untag_agent(spot.pane));
        }
        // An empty slot already parked by the swap stays.
        None if at.place == Place::Hidden => {}
        _ => cmds.extend(go_home(spot, title)),
    }
    cmds.extend(agent_tags(pane, &cut(title(id))));
    Some(Cmd::chain(cmds))
}

/// L11's vacate of the spot's agent in `spot`: a spare empty slot swapped in and the agent
/// killed; else the spot respawned in place as the empty slot (`respawn` is the rest of
/// `respawn-pane -k -t <spot>`), untagged as a session and tagged as the slot.
pub(crate) fn vacate(scene: &Scene, spot: PaneId, respawn: &[String]) -> Cmd {
    let spare = scene.panes.iter().find(|at| {
        at.role == Role::Panel(View::EmptySlot) && at.place == Place::Hidden && !at.dead
    });
    if let Some(slot) = spare {
        return Cmd::chain(vec![swap(slot.pane, spot), kill(spot)]);
    }
    let mut cmds = vec![
        Cmd::new("respawn-pane")
            .args(["-k", "-t", &spot.to_string()])
            .args(respawn.iter().cloned()),
        unset_pane_option(spot, "@moa_session"),
    ];
    cmds.extend(panel_tags(spot, View::EmptySlot));
    cmds.extend(keep_on_exit(spot));
    Cmd::chain(cmds)
}
