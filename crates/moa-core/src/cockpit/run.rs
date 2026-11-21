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

use super::scene::{Role, Scene};
use crate::effect::{Effect, LogLevel, Publish};
use crate::layout::{DEFAULT_LAYOUT, Leaf, Node, current_layout};
use crate::model::SessionId;
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::ops::launch::title_of;
use crate::ops::term::{Tell, told};
use crate::panel::View;
use crate::publish::Topic;
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::bound_session;
use crate::tmux::ids::{ClientName, PaneId, TmuxSessionId};
use crate::tmux::link::{Asked, NO_TERMINAL, log};
use crate::tmux::topology::refresh_cmds;

mod answers;
mod decide;
mod sheets;

pub(crate) use answers::{chain_answered, scene_answered, spawn_answered};
use decide::{Said, Step, Then, layout_refused, wants_lazy};
use sheets::{linked_out, sheet_name, sheet_of, stale_sheets};
pub(crate) use sheets::{sheet_news, sheets_notified, ui_close};

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
    /// Opens the `view` sheet in tmux session `session` (S9): the tab of its panel, parked
    /// in `_moa`, linked into the session and selected, or selected when it is there
    /// already. With no panel, one is started first; a stopped one is restarted in place.
    /// `client` pressed the key, and hears a refusal.
    Sheet {
        view: View,
        session: TmuxSessionId,
        client: ClientName,
    },
    /// Closes the sheet whose panel is in `pane` (S9): its tab unlinked from every session
    /// but `_moa`, where it waits for the next open. Nobody hears of it.
    Unsheet { pane: PaneId },
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
    /// The dead panels tmux was nudged to reap (`run-shell true`) while no `pane-died` had
    /// come for them, until a listing shows them running or gone: once per death.
    nudged: BTreeSet<PaneId>,
    /// The link just came up: its first listing may queue the lazy build (L9).
    lazy_armed: bool,
    /// The vacate in flight took its agent down: its kill (or the spot's respawn) was
    /// answered `Ok`, whatever becomes of the tags after it (R24).
    taken: bool,
    /// The vacate in flight takes its agent down by a plain `kill-pane`: a refusal of it has
    /// nothing plainer to fall back on.
    plain_kill: bool,
    /// The vacate's chain still to go once its agent is taken down: the empty slot's tags.
    rest: Option<(Vec<Cmd>, Then)>,
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
        self.taken = false;
        self.plain_kill = false;
        self.rest = None;
    }
}

/// Whom `op` answers: none for an operation nobody asked for, or whose asker left.
fn tells_of(op: &CockpitOp) -> Vec<Tell> {
    match op {
        CockpitOp::Build { tell, .. }
        | CockpitOp::Repair { tell }
        | CockpitOp::SelectView { tell, .. } => tell.iter().cloned().collect(),
        CockpitOp::Show { tells, .. } => tells.clone(),
        CockpitOp::Sheet { client, .. } => vec![Tell::Notice(client.clone())],
        CockpitOp::Lazy
        | CockpitOp::Refill
        | CockpitOp::Vacate { .. }
        | CockpitOp::Unsheet { .. } => Vec::new(),
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
/// build of its kind, which is told so; a refill behind a build is dropped; a sheet's open
/// or close replaces a waiting one of the same sheet, which nobody hears of (a key has no
/// reply). With no link, only a vacate waits; the rest are refused.
pub(crate) fn queue(state: &mut State, op: CockpitOp) -> Vec<Effect> {
    if state.tmux.conn().is_none() && !matches!(op, CockpitOp::Vacate { .. }) {
        let no = Err(OpError(NO_TERMINAL.into()));
        return told_all(state, tells_of(&op), &no);
    }
    if op == CockpitOp::Refill && state.cockpit.waiting.iter().any(|w| refills(state, w)) {
        return Vec::new();
    }
    if let Some(sheet) = sheet_of(state, &op) {
        let waiting = std::mem::take(&mut state.cockpit.waiting);
        let kept = waiting
            .into_iter()
            .filter(|w| sheet_of(state, w) != Some(sheet))
            .collect();
        state.cockpit.waiting = kept;
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

/// The panes a vacate is taking down, running or waiting: their sessions are saved as gone,
/// so nothing binds them again meanwhile (reconcile).
pub(crate) fn vacating(state: &State) -> impl Iterator<Item = PaneId> + '_ {
    let run = &state.cockpit;
    run.in_flight
        .iter()
        .map(|(_, op)| op)
        .chain(run.waiting.iter())
        .filter_map(|op| match op {
            CockpitOp::Vacate { pane, .. } => Some(*pane),
            _ => None,
        })
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
    // The empty slot is the spot's, and the picker and help are sheets, parked between opens
    // (S9): none is a panel's to pick.
    let pane = match crate::panel::pane_arg(pane) {
        Ok(_) if view == View::EmptySlot || view.is_sheet() => {
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
        CockpitOp::Vacate { .. }
            | CockpitOp::SelectView { .. }
            | CockpitOp::Show { .. }
            | CockpitOp::Sheet { .. }
            | CockpitOp::Unsheet { .. }
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
    let taken = run.taken;
    run.reset_op();
    // A vacate a restart waits for: done when its agent went (its kill or respawn answered
    // `Ok`, whatever became of the slot's tags) or there was nothing left to take down;
    // else not (L17, R24).
    if let Some(CockpitOp::Vacate { pane, .. }) = &op
        && run.held.contains_key(pane)
    {
        run.vacated
            .push((*pane, taken || matches!(said, Said::Nothing)));
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
    let mut unsheeted = false;
    for op in ops {
        match op {
            CockpitOp::Vacate { .. } => state.cockpit.waiting.push_back(op),
            // A sheet's close that won't run forgets its open (S9): the next listing finds
            // its tab linked with no open recorded, and closes it then (`stale_sheets`).
            CockpitOp::Unsheet { pane } => unsheeted |= state.sheets.remove(&pane).is_some(),
            op => fx.extend(told_all(state, tells_of(&op), &Err(OpError(LOST.into())))),
        }
    }
    if unsheeted {
        fx.extend(crate::panel::publish_panels(state));
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
        state.cockpit.nudged.clear();
    }
    state.cockpit.lazy_armed = true;
    pump(state)
}

/// What every listing does for stopped panels (L12):
/// - the ones it shows running again, or gone, are forgotten, so their next death is logged
///   and nudged too;
/// - a panel it shows dead with no `pane-died` yet gets tmux nudged to reap it, once per
///   death. tmux 3.4 and 3.5 built with utempter can lose a pane's exit when its pty closes
///   first: the pane is dead, but tmux never draws its stopped line nor says `pane-died`
///   until a child of its own ends. `run-shell true` is that child; tmux then draws the line
///   and says `pane-died`, which [`died`] logs. Elsewhere it changes nothing.
fn stopped_listed(state: &mut State) -> Option<Effect> {
    let topology = &state.tmux.topology;
    let run = &mut state.cockpit;
    run.stopped.retain(|p| topology.is_dead(*p));
    run.nudged.retain(|p| topology.is_dead(*p));
    let conn = state.tmux.conn()?;
    let unsaid: Vec<PaneId> = topology
        .panes()
        .filter(|p| topology.is_dead(*p) && !run.stopped.contains(p) && !run.nudged.contains(p))
        .filter(|p| {
            topology
                .view_of(*p)
                .is_some_and(|v| v.parse::<View>().is_ok())
        })
        .collect();
    if unsaid.is_empty() {
        return None;
    }
    run.nudged.extend(unsaid);
    Some(reap_nudge(conn))
}

/// `run-shell true` on `conn`: a child of tmux's own whose end makes it reap the children
/// whose exits it lost (tmux 3.4 and 3.5 built with utempter; see [`stopped_listed`]).
pub(crate) fn reap_nudge(conn: u64) -> Effect {
    Effect::Tmux {
        conn,
        cmds: vec![Cmd::new("run-shell").arg("true")],
        token: None,
    }
}

/// The link's own listing came back: what it shows of stopped panels is taken in
/// ([`stopped_listed`]), and of sheets left open ([`stale_sheets`]). The first after a
/// link-up queues the lazy build when
/// it shows what the build looks for, a user's terminal and a cockpit with no panel in it;
/// the build then takes a listing of its own, as every operation does (L17).
pub(crate) fn link_listed(state: &mut State) -> Vec<Effect> {
    let mut fx: Vec<Effect> = stopped_listed(state).into_iter().collect();
    fx.extend(stale_sheets(state));
    if !std::mem::take(&mut state.cockpit.lazy_armed) {
        return fx;
    }
    let scene = Scene::read(&state.tmux.topology, |p| bound_session(state, p));
    if !wants_lazy(&scene) {
        return fx;
    }
    fx.extend(queue(state, CockpitOp::Lazy));
    fx
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
            // A sheet's key tells its client, which no connection is.
            CockpitOp::Lazy
            | CockpitOp::Refill
            | CockpitOp::Vacate { .. }
            | CockpitOp::Sheet { .. }
            | CockpitOp::Unsheet { .. } => {}
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
