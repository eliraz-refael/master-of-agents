use serde_json::json;

use super::decide::{Next, Said, Sheeted, Step, Then, act, decide};
use super::{
    CockpitOp, awaits, finish, listing, queue, refused, stale_sheets, stopped_listed, tells_of,
};
use crate::Now;
use crate::change::Change;
use crate::cockpit::scene::Scene;
use crate::cockpit::spawn::spawned;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::layout::CURRENT_KEY;
use crate::op::{Done, OpError};
use crate::panel::View;
use crate::sleep::kill_pane;
use crate::state::State;
use crate::tmux::focus::bound_session;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Answers, log};

/// tmux's first words, or a reason all the same.
fn first_line(e: &str) -> &str {
    e.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("tmux gave no reason")
}

/// A chain tmux gave no answer for (the link went first).
const NO_ANSWER: &str = "tmux didn't answer";

/// Why a chain didn't go through, in plain words, from tmux's: never tmux's own (they go to
/// the log).
fn plainly(e: &str) -> &'static str {
    let why = first_line(e);
    if ["can't find pane", "can't find window", "can't find session"]
        .iter()
        .any(|s| why.contains(s))
    {
        "something moa was moving closed meanwhile"
    } else if why == NO_ANSWER {
        "the terminal didn't answer"
    } else {
        "the terminal refused the change"
    }
}

/// A vacate's chain refused (`e`), or not answered:
/// - its agent was taken down already: only the empty slot's tags are missing;
/// - its pane is gone: nothing runs there, so it is done (R24);
/// - the plain kill itself, or a pane moa last knew as a panel: it ends, said;
/// - else `None`: the agent is killed plainly instead ([`vacate_plainly`]).
fn vacate_refused(state: &State, pane: PaneId, e: &str) -> Option<Said> {
    let why = plainly(e);
    if state.cockpit.taken {
        return Some(Said::Logged(format!(
            "the agent in {pane} was taken down, but the empty slot there couldn't be set up: {why}"
        )));
    }
    if first_line(e) == format!("can't find pane: {pane}") {
        return Some(Said::Nothing);
    }
    if state.cockpit.plain_kill || known_panel(state, pane) {
        return Some(Said::Logged(format!(
            "the agent in {pane} couldn't be taken down: {why}; it may still be running"
        )));
    }
    None
}

/// Whether moa last knew `pane` as a panel (the empty slot included): never killed blind.
fn known_panel(state: &State, pane: PaneId) -> bool {
    state
        .tmux
        .topology
        .view_of(pane)
        .is_some_and(|v| v.parse::<View>().is_ok())
}

/// The vacate's agent in `pane` killed plainly, as 4b did (a pane id names one pane on its
/// server), when its own chain was refused: an agent running while the brain says it's gone
/// is worse than a spot left empty, which a refill builds again.
fn vacate_plainly(state: &mut State, seq: u64, pane: PaneId) -> Vec<Effect> {
    let mut fx = vec![log(
        LogLevel::Warn,
        format!("the agent in {pane} is closed plainly; the cockpit is built again after"),
    )];
    let then = Then {
        tags: vec![(pane, None)],
        ..Then::quiet(Said::Nothing)
    };
    fx.extend(act(
        state,
        seq,
        Next::TakeDown(vec![kill_pane(pane)], then, None),
    ));
    fx.extend(queue(state, CockpitOp::Refill));
    fx
}

/// A chain's error in plain words (L18): the layout's fit said as such, the rest with a
/// plain reason.
fn chain_failed(op: &CockpitOp, layout: &str, e: &str) -> Said {
    let raw = first_line(e);
    let why = plainly(e);
    let text = match op {
        CockpitOp::Vacate { pane, .. } => {
            format!("the agent in {pane} couldn't be taken down: {why}; it may still be running")
        }
        CockpitOp::SelectView { pane, view, .. } => {
            format!("the panel in {pane} couldn't show {view}: {why}")
        }
        CockpitOp::Show { .. } => format!("moa couldn't bring it to the screen: {why}"),
        CockpitOp::Sheet { view, .. } => format!("the {view} couldn't open: {why}"),
        CockpitOp::Unsheet { pane } => {
            format!("the tab of the panel in {pane} couldn't close: {why}")
        }
        _ if raw.contains("no space for") || raw.contains("pane too small") => {
            format!("the {layout} layout doesn't fit this window; make the terminal bigger")
        }
        _ => format!("layout {layout} couldn't be applied: {why}; Ctrl-a r tries again"),
    };
    refused(op, text)
}

/// An operation's listing came back: the topology is rebuilt from it, the panels' views are
/// read again from it (L8), then the operation plans in it and does its next step.
pub(crate) fn scene_answered(state: &mut State, seq: u64, answers: Answers) -> Vec<Effect> {
    if !awaits(state, seq, |s| matches!(s, Step::Listing)) {
        return Vec::new();
    }
    let Some(logs) = state.tmux.scene_listed(&answers) else {
        // tmux's own words, only when tmux returned an error; else moa couldn't parse it.
        let raw = log(
            LogLevel::Warn,
            match answers.iter().find_map(|a| a.as_ref().err()) {
                Some(e) => format!("tmux couldn't list the terminal: {}", first_line(e)),
                None => "moa couldn't read tmux's listing: it didn't parse".to_string(),
            },
        );
        // A vacate can't plan with no listing, but its commit is saved: its agent is killed
        // plainly, as 4b did (a pane id names one pane on its server), unless moa last knew
        // the pane as a panel (its chain already ran).
        if let Some((_, CockpitOp::Vacate { pane, .. })) = &state.cockpit.in_flight {
            let pane = *pane;
            if !known_panel(state, pane) {
                let mut fx = vec![
                    raw,
                    log(
                        LogLevel::Warn,
                        format!(
                            "moa couldn't read the terminal's state; the agent in {pane} is closed without it"
                        ),
                    ),
                ];
                let then = Then {
                    tags: vec![(pane, None)],
                    ..Then::quiet(Said::Nothing)
                };
                fx.extend(act(
                    state,
                    seq,
                    Next::TakeDown(vec![kill_pane(pane)], then, None),
                ));
                return fx;
            }
        }
        let text = "moa couldn't read the terminal's state; try again".to_string();
        // tmux's words go to the log only. Logged either way; an operation nobody asked for
        // has the log line only (and a vacate's ends as not done).
        let mut fx = vec![raw];
        let said = match &state.cockpit.in_flight {
            Some((_, op)) if !tells_of(op).is_empty() => {
                fx.push(log(LogLevel::Warn, text.clone()));
                refused(op, text)
            }
            _ => Said::Logged(text),
        };
        fx.extend(finish(state, said));
        return fx;
    };
    let mut fx = logs;
    fx.extend(stopped_listed(state));
    // The operation's own listing: a panel healed now is tagged before its chain goes out.
    fx.extend(crate::panel::listed(state, true));
    let scene = Scene::read(&state.tmux.topology, |p| bound_session(state, p));
    let Some((_, op)) = state.cockpit.in_flight.clone() else {
        return fx;
    };
    let next = decide(state, &op, &scene);
    // A close forgets its open as it plans (S9): the notifications its own unlink brings
    // never read as leaving an open sheet.
    if let CockpitOp::Unsheet { pane } = &op
        && state.sheets.remove(pane).is_some()
    {
        fx.extend(crate::panel::publish_panels(state));
    }
    fx.extend(act(state, seq, next));
    fx.extend(stale_sheets(state));
    fx
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
                        state.cockpit.nudged.remove(&pane);
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
            let answer = match answers.into_iter().next() {
                Some(Ok(_)) => Ok(()),
                // A close whose tab is gone from a session is done there (S9). Sessions in
                // a group share their tabs, so the first unlink takes it from them all, and
                // the next one finds no tab. Should a session tmux skipped after it still
                // hold the tab, the next listing finds it linked with no open, and closes it.
                Some(Err(e))
                    if matches!(op, CockpitOp::Unsheet { .. })
                        && first_line(&e).starts_with("can't find window") =>
                {
                    Ok(())
                }
                Some(Err(e)) => Err(e),
                None => Err(NO_ANSWER.to_string()),
            };
            let said = match answer {
                Ok(()) => {
                    state.tmux.views_set(&then.tags);
                    // A panel that registered while its tab still read the old view gets
                    // its view, and whether moa manages it, from the tags just set (L8).
                    fx.extend(crate::panel::resolve_panels(state));
                    for pane in &then.untagged {
                        state.tmux.untag_sent(*pane);
                        state.tmux.term_unset(*pane);
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
                    if let Some(sheeted) = then.sheet {
                        fx.extend(sheeted_now(state, now, sheeted));
                    }
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
                    // A vacate's first chain took its agent down; its tags go next.
                    if matches!(op, CockpitOp::Vacate { .. }) {
                        state.cockpit.taken = true;
                        if let Some((cmds, then)) = state.cockpit.rest.take() {
                            fx.extend(act(state, seq, Next::Chain(cmds, then)));
                            return fx;
                        }
                    }
                    said
                }
                Err(e) => {
                    // tmux's own words go to the log; whoever asked hears a plain reason.
                    let tmux_said = match e.as_str() {
                        NO_ANSWER => "tmux didn't answer a cockpit change".to_string(),
                        e => format!("tmux refused a cockpit change: {}", first_line(e)),
                    };
                    fx.push(log(LogLevel::Warn, tmux_said));
                    match op {
                        CockpitOp::Vacate { pane, .. } => match vacate_refused(state, pane, &e) {
                            Some(said) => said,
                            None => {
                                fx.extend(vacate_plainly(state, seq, pane));
                                return fx;
                            }
                        },
                        _ => chain_failed(&op, &then.layout, &e),
                    }
                }
            };
            fx.extend(finish(state, said));
        }
        step => state.cockpit.step = step,
    }
    fx
}

/// A sheet's chain went through (S9), straight into the topology as a swap's does. An open
/// is recorded by its panel's pane, numbered by the wall clock (one later than the last
/// should two fall in one millisecond), and `panels` says so: the panel clears itself for
/// it.
fn sheeted_now(state: &mut State, now: &Now, sheeted: Sheeted) -> Option<Effect> {
    match sheeted {
        Sheeted::Opened {
            pane,
            window,
            session,
        } => {
            state.tmux.topology.linked(window, session);
            let number = now.wall.0.max(state.last_sheet + 1);
            state.last_sheet = number;
            state.sheets.insert(pane, number);
            crate::panel::publish_panels(state)
        }
        Sheeted::Closed { window, sessions } => {
            for s in sessions {
                state.tmux.topology.unlinked(window, s);
            }
            None
        }
    }
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
