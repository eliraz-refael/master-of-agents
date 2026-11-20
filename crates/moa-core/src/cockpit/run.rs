//! The cockpit queue (L17): every operation on the cockpit runs one at a time, each planned
//! from a listing of its own, taken when it starts. It sends its chain and waits for the
//! answer before the next one starts, so no plan is ever made from a scene that a chain
//! still in flight is changing.
//!
//! An operation goes: its listing (`Asked::Scene`), then, as it needs, its panels started
//! (`Asked::Spawn`, then a listing again, once), Repair's respawns (`Asked::Chain`, then a
//! listing again), and its chain (`Asked::Chain`). Every answer carries the operation's
//! `seq`; one for an operation no longer in flight is ignored.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;

use super::assign::view_of;
use super::build::{occupant_stays, panel_tags};
use super::scene::{AGENT, Place, Role, Scene};
use super::spawn::{respawn, respawn_tail, spawn, spawned};
use super::{Plan, plan, swap_in, vacate};
use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, Publish};
use crate::layout::{CURRENT_KEY, DEFAULT_LAYOUT, LayoutError, Leaf, Node, current_layout, leaves};
use crate::model::SessionId;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::ops::launch::{GONE, focus_cmds, shown, title_of};
use crate::ops::term::{Tell, told};
use crate::panel::View;
use crate::publish::Topic;
use crate::sleep::kill_pane;
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::bound_session;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Answers, Asked, NO_TERMINAL, log};
use crate::tmux::topology::refresh_cmds;

/// An operation the link went away under, or a reload dropped.
pub(crate) const LOST: &str = "moa lost its terminal; try again";
/// A waiting build a newer one took the place of.
const REPLACED: &str = "replaced by a later layout request";
/// A waiting `goto` a newer one took the place of.
const REPLACED_GOTO: &str = "replaced by a later key";

/// One operation on the cockpit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum CockpitOp {
    /// Builds layout `name` (the current one when `None`): `layout.apply`, a layout key, or
    /// `goto` (`Select::Leaf`), which first looks for its panel in the cockpit. Only a build
    /// of its kind (a switch, or a `goto`) replaces a waiting one.
    Build {
        name: Option<String>,
        tell: Option<Tell>,
        select: Select,
    },
    /// Restarts every stopped panel in place, then builds the current layout (L12).
    Repair { tell: Option<Tell> },
    /// Builds the current layout when a user's terminal is attached and the cockpit holds
    /// no panel yet (L9).
    Lazy,
    /// Builds the current layout again, whatever the cockpit holds, selecting nothing: the
    /// spot's agent ended (L11). A build waiting already does that: a refill behind one is
    /// dropped. It never takes another's place, nor is replaced.
    Refill,
    /// Takes session `id`'s agent in `pane` down (`kill_tab`): from the spot, leaving the
    /// empty slot (L11); elsewhere, a plain kill; gone, or no longer its, nothing. It
    /// survives a lost link and a reload.
    Vacate { pane: PaneId, id: SessionId },
    /// `ui.select_view`: the panel in `pane` shows `view`, its tag and frame set (L8, L10).
    /// Queued, so it never races a build's retagging of the same pane.
    SelectView {
        pane: PaneId,
        view: View,
        tell: Option<Tell>,
    },
    /// Brings the agent in `pane` to the screen (L11): swapped into the spot when the
    /// cockpit has one, else its tab selected. `focus` and the tree's Enter, and an `open`
    /// or `resume` once its tab is bound, whose `tells` are the focus requests that waited
    /// for the tab (the launch's own caller was answered by it). Never replaced, and never
    /// replaces a waiting operation: each runs in its turn.
    Show { pane: PaneId, tells: Vec<Tell> },
}

/// Which pane a build selects once it is done (L6 step 7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Select {
    /// The console's, else the first leaf's: a layout key and `layout.apply`.
    Default,
    /// This leaf's: `goto`, which replies with the leaf's word.
    Leaf(Leaf),
}

/// The queue (L17): the operation in flight with its `seq`, and those waiting.
#[derive(Debug, Default)]
pub(crate) struct CockpitRun {
    /// The last `seq` handed out.
    pub(crate) seq: u64,
    pub(crate) in_flight: Option<(u64, CockpitOp)>,
    pub(crate) waiting: VecDeque<CockpitOp>,
    /// What the operation in flight waits for.
    step: Step,
    /// It started panels already: the next plan that needs one gives up.
    replanned: bool,
    /// Repair's restarts, once its respawns were answered.
    restarted: Option<u32>,
    /// The panels logged as stopped, until they run again: one line per death (L12).
    stopped: BTreeSet<PaneId>,
    /// The link just came up: its first listing may queue the lazy build (L9).
    lazy_armed: bool,
    /// Restarts whose new tab waits for the old agent to go (R24), by the vacated pane.
    pub(crate) held: BTreeMap<PaneId, Held>,
    /// The held vacates that ended since, and whether they took the agent down: the end of
    /// the input releases or refuses their restarts (`ops::launch::released`).
    pub(crate) vacated: Vec<(PaneId, bool)>,
}

/// A restart's resume window, held until its vacate ends.
#[derive(Debug)]
pub(crate) struct Held {
    pub(crate) id: SessionId,
    pub(crate) window: Effect,
}

impl CockpitRun {
    /// What an operation keeps while it runs, cleared as one starts or ends.
    fn reset_op(&mut self) {
        self.step = Step::Listing;
        self.replanned = false;
        self.restarted = None;
    }
}

#[derive(Debug, Default)]
enum Step {
    #[default]
    Listing,
    Spawning,
    /// Repair's respawns of these panes.
    Respawning(Vec<PaneId>),
    Chain(Then),
}

/// What a chain's `Ok` records and says.
#[derive(Debug)]
struct Then {
    /// The `@moa_view`s it sets (`None`: unsets), recorded at once (L17).
    tags: Vec<(PaneId, Option<&'static str>)>,
    /// The panes it takes `@moa_session` off, recorded at once too.
    untagged: Vec<PaneId>,
    /// A swap into the spot: where it puts the panes, recorded at once too.
    moved: Option<Moved>,
    /// A build's live panels given another view: their rows change.
    views: Option<Vec<(PaneId, View)>>,
    /// The panels it starts in place.
    starts: u32,
    /// The layout it builds, for its error.
    layout: String,
    ok: Said,
}

/// Where a swap into the spot (L11) puts the panes: `pane` takes the spot's place, and the
/// spot's occupant takes `pane`'s old one when it `stays` there, else it goes to a new tab.
#[derive(Clone, Copy, Debug)]
struct Moved {
    pane: PaneId,
    spot: PaneId,
    stays: bool,
}

/// How an operation ends.
#[derive(Debug)]
enum Said {
    /// `layout <name> applied`, after the brain's current layout is saved.
    Applied(String),
    Reply(Result<Done, OpError>),
    Logged(String),
    Nothing,
}

/// What an operation does next, from its listing.
enum Next {
    Done(Said),
    Spawn(Vec<View>),
    Respawn(Vec<(PaneId, View)>),
    Chain(Vec<Cmd>, Then),
}

/// Whom `op` answers: none for an operation nobody asked for, or whose asker left.
fn tells_of(op: &CockpitOp) -> Vec<Tell> {
    match op {
        CockpitOp::Build { tell, .. }
        | CockpitOp::Repair { tell }
        | CockpitOp::SelectView { tell, .. } => tell.iter().cloned().collect(),
        CockpitOp::Show { tells, .. } => tells.clone(),
        CockpitOp::Lazy | CockpitOp::Refill | CockpitOp::Vacate { .. } => Vec::new(),
    }
}

/// `result`, told to each of `tells`.
fn told_all(state: &State, tells: Vec<Tell>, result: &Result<Done, OpError>) -> Vec<Effect> {
    tells
        .into_iter()
        .filter_map(|tell| told(state, Some(tell), result.clone()))
        .collect()
}

/// `text` as `op`'s refusal: said to whoever asked, or logged when nobody is left to hear it
/// (an operation nobody asked for, or one whose asker left).
fn refused(op: &CockpitOp, text: String) -> Said {
    match tells_of(op).is_empty() {
        true => Said::Logged(text),
        false => Said::Reply(Err(OpError(text))),
    }
}

/// tmux's first words, or a reason all the same.
fn first_line(e: &str) -> &str {
    e.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("tmux gave no reason")
}

/// A chain's error in plain words (L18).
fn chain_failed(op: &CockpitOp, layout: &str, e: &str) -> Said {
    let why = first_line(e);
    let text = match op {
        CockpitOp::Vacate { pane, .. } => {
            format!("the agent in {pane} couldn't be taken down: {why}")
        }
        CockpitOp::SelectView { pane, view, .. } => {
            format!("the panel in {pane} couldn't show {view}: {why}")
        }
        CockpitOp::Show { .. } => format!("moa couldn't bring it to the screen: {why}"),
        _ if why.contains("no space for") || why.contains("pane too small") => {
            format!("the {layout} layout doesn't fit this window; make the terminal bigger")
        }
        _ => format!("layout {layout} couldn't be applied: {why}; Ctrl-a r tries again"),
    };
    refused(op, text)
}

/// A layout that can't be built, as said to whoever asked for it. While `layouts.json`
/// doesn't parse, a name it doesn't have says so (L2).
fn layout_refused(state: &State, e: LayoutError) -> String {
    match (&state.layouts.file_error, e) {
        (Some(why), LayoutError::NoSuch { .. }) => format!("layouts.json has an error: {why}"),
        (_, e) => e.to_string(),
    }
}

/// The current layout, falling back to the `cockpit` preset when even its entry is broken.
fn current(state: &State) -> (String, Node) {
    let name = current_layout(state);
    match state.layouts.node(&name) {
        Ok(node) => (name, node),
        Err(_) => (
            DEFAULT_LAYOUT.to_string(),
            crate::layout::presets()
                .node(DEFAULT_LAYOUT)
                .expect("the cockpit preset parses"),
        ),
    }
}

fn title(state: &State, id: SessionId) -> String {
    match state.session(id) {
        Some(s) => title_of(state, s),
        None => state.handle_of(id),
    }
}

/// `leaf`'s pane in the cockpit, if it has one: for `agent`, the spot's.
fn in_cockpit(scene: &Scene, leaf: Leaf) -> Option<PaneId> {
    match leaf {
        Leaf::Agent => scene.spot().map(|at| at.pane),
        Leaf::Panel(view) => scene
            .in_cockpit()
            .find(|at| at.role == Role::Panel(view))
            .map(|at| at.pane),
    }
}

/// The kind of build `op` is, if it is one: a layout switch, or a `goto`. Only a build of
/// the same kind replaces a waiting one, so a `goto` never drops a switch, nor a switch a
/// `goto`.
fn build_kind(op: &CockpitOp) -> Option<bool> {
    match op {
        CockpitOp::Build { select, .. } => Some(matches!(select, Select::Leaf(_))),
        _ => None,
    }
}

/// Whether waiting `op` builds a layout whatever the cockpit holds, as a refill would: a
/// layout switch (of a layout there is), Repair, or a refill.
fn refills(state: &State, op: &CockpitOp) -> bool {
    match op {
        CockpitOp::Build {
            name,
            select: Select::Default,
            ..
        } => name.as_ref().is_none_or(|n| state.layouts.node(n).is_ok()),
        CockpitOp::Repair { .. } | CockpitOp::Refill => true,
        _ => false,
    }
}

/// Runs `op` after those queued before it, now if none is. A build replaces a waiting
/// build of its kind, which is told so; a refill behind a build is dropped. With no link,
/// only a vacate waits; the rest are refused.
pub(crate) fn queue(state: &mut State, op: CockpitOp) -> Vec<Effect> {
    if state.tmux.conn().is_none() && !matches!(op, CockpitOp::Vacate { .. }) {
        let no = Err(OpError(NO_TERMINAL.into()));
        return told_all(state, tells_of(&op), &no);
    }
    if op == CockpitOp::Refill && state.cockpit.waiting.iter().any(|w| refills(state, w)) {
        return Vec::new();
    }
    let mut fx = Vec::new();
    let kind = build_kind(&op);
    let waiting = &mut state.cockpit.waiting;
    match waiting.iter().position(|w| build_kind(w) == kind) {
        // The replaced one gives up its place: the newer waits at the end, so keys of the two
        // kinds still run in the order they were pressed.
        Some(i) if kind.is_some() => {
            let old = waiting.remove(i).expect("the position is in the queue");
            waiting.push_back(op);
            let why = match kind {
                Some(true) => REPLACED_GOTO,
                _ => REPLACED,
            };
            fx.extend(told_all(state, tells_of(&old), &Err(OpError(why.into()))));
        }
        _ => waiting.push_back(op),
    }
    fx.extend(pump(state));
    fx
}

/// Whether bringing a session to the screen, or taking its tab down, goes through the queue
/// (L11, L17): a cockpit is there, so it may have a spot, or an operation that may build
/// one is queued, and the show or the vacate keeps its place behind it. Otherwise there is
/// no spot: the session's tab is selected, or killed, at once (4b).
pub(crate) fn through_queue(state: &State) -> bool {
    state.tmux.topology.cockpit().is_some() || !idle(state)
}

/// Nothing runs in the queue and nothing waits: a command sent now can't land between an
/// operation's listing and its chain.
pub(crate) fn idle(state: &State) -> bool {
    state.cockpit.in_flight.is_none() && state.cockpit.waiting.is_empty()
}

/// `layout.apply`: the layout built, then the reply.
pub(crate) fn apply(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    name: Option<String>,
) -> Vec<Effect> {
    queue(
        state,
        CockpitOp::Build {
            name,
            tell: Some(Tell::Reply(conn, req)),
            select: Select::Default,
        },
    )
}

/// `goto`: the panel selected, after the current layout is built when it's missing.
pub(crate) fn goto(state: &mut State, conn: ConnId, req: ReqId, panel: Leaf) -> Vec<Effect> {
    queue(
        state,
        CockpitOp::Build {
            name: None,
            tell: Some(Tell::Reply(conn, req)),
            select: Select::Leaf(panel),
        },
    )
}

/// `ui.select_view`: the panel in `pane` retagged to show `view`, then its row and the
/// reply (L8).
pub(crate) fn select_view(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    pane: &str,
    view: View,
) -> Vec<Effect> {
    // The empty slot is the spot's, and the picker a prompt's: neither is a panel's to pick.
    let pane = match crate::panel::pane_arg(pane) {
        Ok(_) if matches!(view, View::EmptySlot | View::Picker) => {
            Err(OpError(format!("{view} can't be chosen for a panel")))
        }
        p => p,
    };
    let pane = match pane {
        Ok(pane) => pane,
        Err(e) => {
            let result = Err(e);
            return vec![Effect::Reply { conn, req, result }];
        }
    };
    queue(
        state,
        CockpitOp::SelectView {
            pane,
            view,
            tell: Some(Tell::Reply(conn, req)),
        },
    )
}

/// Starts the operations waiting, while none is in flight.
fn pump(state: &mut State) -> Vec<Effect> {
    let mut fx = Vec::new();
    while state.cockpit.in_flight.is_none() {
        let Some(op) = state.cockpit.waiting.pop_front() else {
            break;
        };
        if state.tmux.conn().is_none() {
            // Only vacates wait for the link (`queue` refuses the rest).
            state.cockpit.waiting.push_front(op);
            break;
        }
        fx.extend(start(state, op));
    }
    fx
}

/// `op` in flight, with its own `seq`, asking for its listing. `layouts.json` is read again
/// for an operation that builds (L2).
fn start(state: &mut State, op: CockpitOp) -> Vec<Effect> {
    let mut fx = Vec::new();
    if !matches!(
        op,
        CockpitOp::Vacate { .. } | CockpitOp::SelectView { .. } | CockpitOp::Show { .. }
    ) {
        state.layouts = crate::ops::layout::read(state);
        fx.extend(
            state
                .publish_changed(Topic::Layouts)
                .map(|m| Effect::Publish(Publish::Model(m))),
        );
    }
    let run = &mut state.cockpit;
    run.seq += 1;
    let seq = run.seq;
    run.in_flight = Some((seq, op));
    run.reset_op();
    // A named layout that isn't there is said at once.
    if let Some((
        _,
        CockpitOp::Build {
            name: Some(name), ..
        },
    )) = &state.cockpit.in_flight
        && let Err(e) = state.layouts.node(name)
    {
        let text = layout_refused(state, e);
        fx.extend(finish(state, Said::Reply(Err(OpError(text)))));
        return fx;
    }
    fx.extend(listing(state, seq));
    fx
}

/// Asks the in-flight operation's listing.
fn listing(state: &mut State, seq: u64) -> Vec<Effect> {
    state.cockpit.step = Step::Listing;
    match state.tmux.ask(Asked::Scene { seq }, refresh_cmds()) {
        Some(e) => vec![e],
        None => lost_now(state),
    }
}

/// The link isn't there to ask: the operation ends as a lost link would end it.
fn lost_now(state: &mut State) -> Vec<Effect> {
    let said = match &state.cockpit.in_flight {
        Some((_, op)) => refused(op, LOST.into()),
        None => Said::Nothing,
    };
    finish(state, said)
}

/// The operation in flight ends: its answer, then the next one starts.
fn finish(state: &mut State, said: Said) -> Vec<Effect> {
    let run = &mut state.cockpit;
    let op = run.in_flight.take().map(|(_, op)| op);
    run.reset_op();
    // A vacate a restart waits for: done when it ended with nothing to say (its chain's
    // `Ok`, or nothing left to take down), else not (L17, R24).
    if let Some(CockpitOp::Vacate { pane, .. }) = &op
        && run.held.contains_key(pane)
    {
        run.vacated.push((*pane, matches!(said, Said::Nothing)));
    }
    let mut fx = Vec::new();
    match said {
        Said::Reply(result) => {
            let tells = op.as_ref().map(tells_of).unwrap_or_default();
            fx.extend(told_all(state, tells, &result));
        }
        Said::Logged(text) => fx.push(log(LogLevel::Warn, text)),
        Said::Applied(_) | Said::Nothing => {}
    }
    fx.extend(pump(state));
    fx
}

/// Whether the operation in flight is `seq` and waits for `step`'s kind of answer.
fn awaits(state: &State, seq: u64, step: fn(&Step) -> bool) -> bool {
    matches!(state.cockpit.in_flight, Some((s, _)) if s == seq) && step(&state.cockpit.step)
}

/// An operation's listing came back: the topology is rebuilt from it, the panels' views are
/// read again from it (L8), then the operation plans in it and does its next step.
pub(crate) fn scene_answered(state: &mut State, seq: u64, answers: Answers) -> Vec<Effect> {
    if !awaits(state, seq, |s| matches!(s, Step::Listing)) {
        return Vec::new();
    }
    if !state.tmux.scene_listed(&answers) {
        let why = answers
            .iter()
            .find_map(|a| a.as_ref().err())
            .map_or("tmux's answer didn't parse", |e| first_line(e));
        let text = format!("moa couldn't read the terminal's state: {why}; try again");
        // Logged either way; an operation nobody asked for has the log line only (and a
        // vacate's ends as not done).
        let (mut fx, said) = match &state.cockpit.in_flight {
            Some((_, op)) if !tells_of(op).is_empty() => {
                (vec![log(LogLevel::Warn, text.clone())], refused(op, text))
            }
            _ => (Vec::new(), Said::Logged(text)),
        };
        fx.extend(finish(state, said));
        return fx;
    }
    forget_running(state);
    // The operation's own listing: a panel healed now is tagged before its chain goes out.
    let mut fx = crate::panel::listed(state, true);
    let scene = Scene::read(&state.tmux.topology, |p| bound_session(state, p));
    let Some((_, op)) = state.cockpit.in_flight.clone() else {
        return fx;
    };
    let next = decide(state, &op, &scene);
    fx.extend(act(state, seq, next));
    fx
}

/// L9: what the lazy build looks for, a user's terminal and a cockpit (in session `moa`)
/// with no panel in it.
fn wants_lazy(scene: &Scene) -> bool {
    scene.moa_session && scene.users && !scene.in_cockpit().any(|at| at.tagged)
}

/// What `op` does next in `scene`.
fn decide(state: &State, op: &CockpitOp, scene: &Scene) -> Next {
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
            // The spot's occupant, tagged `agent` there, with or without its session tag.
            let in_spot =
                at.place == Place::Cockpit && (matches!(at.role, Role::Spot(Some(_))) || at.tagged);
            let then = |tags, untagged, starts| Then {
                tags,
                untagged,
                moved: None,
                views: None,
                starts,
                layout: String::new(),
                ok: Said::Nothing,
            };
            if in_spot {
                let v = vacate(scene, *pane, &respawn_tail(state, View::EmptySlot));
                // In place, the spot becomes the empty slot, no longer a session's tab; a spare
                // one swapped in already is the slot, and the agent's pane is gone: no longer
                // the spot's, so nothing reads an exit of it as the spot's agent ending.
                let then = match v.respawned {
                    true => then(vec![(*pane, Some(View::EmptySlot.word()))], vec![*pane], 1),
                    false => then(vec![(*pane, None)], Vec::new(), 0),
                };
                Next::Chain(v.chain.parts().to_vec(), then)
            } else {
                Next::Chain(vec![kill_pane(*pane)], then(Vec::new(), Vec::new(), 0))
            }
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
fn act(state: &mut State, seq: u64, next: Next) -> Vec<Effect> {
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
    }
}

/// Panels an operation asked for started (or didn't): each that did is tagged, whatever
/// became of the operation, and the operation plans again from a fresh listing (L7).
pub(crate) fn spawn_answered(
    state: &mut State,
    views: &[View],
    seq: u64,
    answers: Answers,
) -> Vec<Effect> {
    let (mut fx, _failed) = spawned(state, views, answers);
    if awaits(state, seq, |s| matches!(s, Step::Spawning)) {
        fx.extend(listing(state, seq));
    }
    fx
}

/// An operation's commands came back: Repair's respawns, then a fresh listing; or its
/// chain, whose `Ok` records its tags and ends it, and whose error is said (L18).
pub(crate) fn chain_answered(
    state: &mut State,
    now: &Now,
    seq: u64,
    answers: Answers,
) -> Vec<Effect> {
    if !awaits(state, seq, |s| {
        matches!(s, Step::Respawning(_) | Step::Chain(_))
    }) {
        return Vec::new();
    }
    let mut fx = Vec::new();
    match std::mem::take(&mut state.cockpit.step) {
        Step::Respawning(panes) => {
            let mut answers = answers.into_iter();
            let mut n = 0;
            for pane in panes {
                match answers.next() {
                    Some(Ok(_)) => {
                        n += 1;
                        state.cockpit.stopped.remove(&pane);
                    }
                    Some(Err(e)) => fx.push(log(
                        LogLevel::Warn,
                        format!("the panel in {pane} couldn't restart: {}", first_line(&e)),
                    )),
                    None => fx.push(log(
                        LogLevel::Warn,
                        format!("the panel in {pane} couldn't restart: tmux didn't answer"),
                    )),
                }
            }
            state.panel_starts += n;
            state.cockpit.restarted = Some(n);
            fx.extend(listing(state, seq));
        }
        Step::Chain(then) => {
            let Some((_, op)) = state.cockpit.in_flight.clone() else {
                return fx;
            };
            let said = match answers.into_iter().next() {
                Some(Ok(_)) => {
                    state.tmux.views_set(&then.tags);
                    for pane in &then.untagged {
                        state.tmux.untag_sent(*pane);
                    }
                    // Straight into the topology: any listing answered after this `Ok` was
                    // run after the chain, so it shows the panes where the chain put them.
                    if let Some(m) = then.moved {
                        state.tmux.topology.swapped(m.pane, m.spot);
                        if !m.stays {
                            state.tmux.topology.lifted(m.spot);
                        }
                    }
                    state.panel_starts += then.starts;
                    let said = match then.ok {
                        Said::Applied(layout) => {
                            let (more, said) = applied(state, now, layout);
                            fx.extend(more);
                            said
                        }
                        said => said,
                    };
                    if let Some(views) = then.views {
                        fx.extend(views_given(state, &views));
                    }
                    said
                }
                Some(Err(e)) => chain_failed(&op, &then.layout, &e),
                None => chain_failed(&op, &then.layout, "tmux didn't answer"),
            };
            fx.extend(finish(state, said));
        }
        step => state.cockpit.step = step,
    }
    fx
}

/// A chain tagged these panels with another view (a build, L5 step 3; `ui.select_view`):
/// their rows show it, as moa's (L8).
fn views_given(state: &mut State, views: &[(PaneId, View)]) -> Option<Effect> {
    for (pane, view) in views {
        state.pane_views.insert(*pane, *view);
        for reg in state.panels.values_mut() {
            if reg.pane == Some(*pane) {
                reg.view = *view;
                reg.managed = true;
            }
        }
    }
    crate::panel::publish_panels(state)
}

/// `layout.apply`'s layout was built: it becomes the brain's current one, with a `ui`
/// event, then the reply. A commit the store refuses is logged; the reply still says the
/// layout was applied, since it was. A broken `layouts.json` is said too (L2).
fn applied(state: &mut State, now: &Now, layout: String) -> (Vec<Effect>, Said) {
    let mut tx = Tx::new(now);
    let change = Change::Meta {
        key: CURRENT_KEY.to_string(),
        value: Some(layout.clone()),
    };
    // tmux built it whatever the brain says: a refused commit is logged, not said.
    let fx = match tx.ui(state, vec![change], format!("layout → {layout}")) {
        Ok(()) => post_commit(state, tx),
        Err(OpError(e)) => vec![log(
            LogLevel::Warn,
            format!("moa couldn't remember {layout} as the current layout: {e}"),
        )],
    };
    let mut text = format!("layout {layout} applied");
    if let Some(why) = &state.layouts.file_error {
        text.push_str(&format!("\nlayouts.json has an error: {why}"));
    }
    let done = Done {
        text,
        data: json!({ "name": layout }),
    };
    (fx, Said::Reply(Ok(done)))
}

/// Ends what the queue holds as the link goes, or a reload starts over: vacates wait for
/// the link again, the one in flight first; every other request is told it was lost (a
/// key's notice can't be shown with no link).
fn drop_all(state: &mut State) -> (Vec<Effect>, bool) {
    let run = &mut state.cockpit;
    // A spawn the link lost is logged with its panels (`spawn_lost`): once is enough.
    let spawning = matches!(run.step, Step::Spawning);
    run.reset_op();
    let in_flight = run.in_flight.take().map(|(_, op)| op);
    let dropped = !spawning
        && in_flight
            .as_ref()
            .is_some_and(|op| !matches!(op, CockpitOp::Vacate { .. }));
    let ops: Vec<CockpitOp> = in_flight.into_iter().chain(run.waiting.drain(..)).collect();
    let mut fx = Vec::new();
    for op in ops {
        match op {
            CockpitOp::Vacate { .. } => state.cockpit.waiting.push_back(op),
            op => fx.extend(told_all(state, tells_of(&op), &Err(OpError(LOST.into())))),
        }
    }
    (fx, dropped)
}

/// The link went down, or another connection replaced it: nothing asked on it will be
/// answered (L17).
pub(crate) fn lost(state: &mut State) -> Vec<Effect> {
    // The restarts held for their vacates heard the link go with their launch (R24): their
    // windows are never sent, while the vacates still run once the link is back.
    state.cockpit.held.clear();
    state.cockpit.vacated.clear();
    let (mut fx, dropped) = drop_all(state);
    if dropped {
        fx.push(log(
            LogLevel::Info,
            "a cockpit change was dropped: moa lost its terminal".into(),
        ));
    }
    fx
}

/// After a reload: the queue starts over, its vacates first (their commits are saved).
pub(crate) fn reload(state: &mut State) -> Vec<Effect> {
    let (mut fx, _) = drop_all(state);
    fx.extend(pump(state));
    fx
}

/// The link came up on a new connection: the vacates waiting run, unless the server is
/// another one (their panes went with the old one). The lazy build (L9) waits for the
/// link's first listing, which the connection asks at once.
pub(crate) fn linked(state: &mut State, another_server: bool) -> Vec<Effect> {
    if another_server {
        state
            .cockpit
            .waiting
            .retain(|op| !matches!(op, CockpitOp::Vacate { .. }));
        state.cockpit.stopped.clear();
    }
    state.cockpit.lazy_armed = true;
    pump(state)
}

/// The panels logged as stopped that a listing shows running again: their next death is
/// logged too (L12).
fn forget_running(state: &mut State) {
    let topology = &state.tmux.topology;
    state.cockpit.stopped.retain(|p| topology.is_dead(*p));
}

/// The link's own listing came back: the stopped panels it shows running are forgotten.
/// The first after a link-up queues the lazy build when
/// it shows what the build looks for, a user's terminal and a cockpit with no panel in it;
/// the build then takes a listing of its own, as every operation does (L17).
pub(crate) fn link_listed(state: &mut State) -> Vec<Effect> {
    forget_running(state);
    if !std::mem::take(&mut state.cockpit.lazy_armed) {
        return Vec::new();
    }
    let scene = Scene::read(&state.tmux.topology, |p| bound_session(state, p));
    if !wants_lazy(&scene) {
        return Vec::new();
    }
    queue(state, CockpitOp::Lazy)
}

/// `conn` closed: its requests still run, with nobody to answer.
pub(crate) fn client_gone(state: &mut State, conn: ConnId) {
    let run = &mut state.cockpit;
    let ops = run
        .in_flight
        .iter_mut()
        .map(|(_, op)| op)
        .chain(run.waiting.iter_mut());
    for op in ops {
        let gone = |tell: &Tell| matches!(tell, Tell::Reply(c, _) if *c == conn);
        match op {
            CockpitOp::Build { tell, .. }
            | CockpitOp::Repair { tell }
            | CockpitOp::SelectView { tell, .. } => {
                if tell.as_ref().is_some_and(gone) {
                    *tell = None;
                }
            }
            CockpitOp::Show { tells, .. } => tells.retain(|t| !gone(t)),
            CockpitOp::Lazy | CockpitOp::Refill | CockpitOp::Vacate { .. } => {}
        }
    }
}

/// A `pane-died` hook: a panel's program stopped, and its pane stays (L12). Logged once
/// per death.
pub(crate) fn died(state: &mut State, pane: PaneId) -> Option<Effect> {
    let view: View = state.tmux.topology.view_of(pane)?.parse().ok()?;
    state
        .cockpit
        .stopped
        .insert(pane)
        .then(|| log(LogLevel::Info, format!("panel {view} in {pane} stopped")))
}

#[cfg(test)]
mod tests;
