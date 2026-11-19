//! The cockpit engine (L5, L6, L10, L11): which pane plays each leaf, the one chain that
//! builds a layout in place, and the agent spot's swap and vacate. The chains run in a
//! simulator (`sim`) that moves panes between windows and sessions as tmux does.

#[path = "cockpit/sim.rs"]
mod sim;

use std::collections::BTreeSet;

use moa_core::cockpit::{Build, Plan, Role, Scene, plan, swap_in, vacate};
use moa_core::layout::{Leaf, Node, PRESET_KEYS, leaves, presets};
use moa_core::model::SessionId;
use moa_core::panel::View;
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::PaneId;
use proptest::prelude::*;

use sim::{Kind, PaneSt, World};

/// Session `n`'s title is the `n`th. `moa` comes early, so the proptest's agents get it.
const TITLES: [&str; 8] = [
    "Lantern",
    "Kite",
    "moa",
    "Beacon",
    "fix #12",
    "a title well past twenty-four characters",
    "🚀 תיקון באג בדף הבית של האתר",
    "Harbor",
];

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

/// Session `n`'s title: `sid(1)` is Lantern, `sid(2)` Kite.
fn title(id: SessionId) -> String {
    let n = id.0.as_u128() as usize;
    TITLES[(n + TITLES.len() - 1) % TITLES.len()].to_string()
}

fn node(name: &str) -> Node {
    presets().node(name).unwrap()
}

fn p(n: u32) -> PaneId {
    PaneId(n)
}

/// Each command of a chain as one line of its words, unquoted.
fn lines(cmd: &Cmd) -> Vec<String> {
    cmd.parts()
        .iter()
        .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
        .collect()
}

fn built(plan: Plan) -> Build {
    assert_eq!(plan.spawn, Vec::<View>::new());
    plan.build.expect("a build")
}

/// L10's tags for a panel pane.
fn panel_tags(pane: PaneId, view: View) -> Vec<String> {
    let mut out = vec![
        format!("set-option -p -t {pane} @moa_view {}", view.word()),
        format!("set-option -p -t {pane} @moa_label {}", view.label()),
    ];
    if view.framed() {
        out.extend([
            format!("set-option -p -t {pane} @moa_framed 1"),
            format!("set-option -p -t {pane} window-style fg=#4c566a"),
            format!("set-option -p -t {pane} window-active-style fg=#88c0d0"),
        ]);
    } else {
        out.extend(unframed(pane));
    }
    out
}

fn unframed(pane: PaneId) -> Vec<String> {
    ["@moa_framed", "window-style", "window-active-style"]
        .iter()
        .map(|o| format!("set-option -p -u -t {pane} {o}"))
        .collect()
}

/// L10's tags for the agent in the spot.
fn agent_tags(pane: PaneId, label: &str) -> Vec<String> {
    let mut out = vec![
        format!("set-option -p -t {pane} @moa_view agent"),
        format!("set-option -p -t {pane} @moa_label ● {label}"),
    ];
    out.extend(unframed(pane));
    out
}

fn starting_with<'a>(lines: &'a [String], prefix: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter(|l| l.starts_with(prefix))
        .map(String::as_str)
        .collect()
}

/// The shell in the cockpit (`%1`), and tree, console and plan parked (`%2`–`%4`).
fn shell_and_parked_panels() -> World {
    let mut w = World::new();
    let c = w.cockpit();
    w.pane(c, PaneSt::stranger());
    for view in [View::Tree, View::Console, View::Plan] {
        w.park(view);
    }
    w
}

/// The `focus` layout built: tree `%1`, plan `%2`, `spot` as `%3` and console `%4`.
fn focus_built(spot: PaneSt) -> World {
    let mut w = World::new();
    let c = w.cockpit();
    w.pane(c, PaneSt::panel(View::Tree));
    w.pane(c, PaneSt::panel(View::Plan));
    w.pane(c, spot);
    w.pane(c, PaneSt::panel(View::Console));
    w
}

fn spot_agent(n: u128) -> PaneSt {
    PaneSt::agent(sid(n)).tag("@moa_view", "agent")
}

#[test]
fn the_first_cockpit_spawns_its_three_panels() {
    let mut w = World::new();
    let c = w.cockpit();
    w.pane(c, PaneSt::stranger());
    let plan = plan(&w.scene(), &node("cockpit"), title);
    assert_eq!(plan.spawn, vec![View::Tree, View::Console, View::Plan]);
    assert_eq!(plan.build, None);
}

#[test]
fn with_panels_parked_the_cockpit_builds_in_one_chain() {
    let mut w = shell_and_parked_panels();
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let mut want: Vec<String> = [
        "swap-pane -d -s %2 -t %1",
        "break-pane -d -s %1 -t =moa: -n ⌨ shell",
        "set-option -p -t %1 @moa_term 1",
        "set-option -p -t %1 @moa_label ⌨ shell",
        "join-pane -d -h -s %3 -t %2",
        "join-pane -d -h -s %4 -t %3",
        "resize-pane -t %2 -x 26%",
        "resize-pane -t %4 -x 30%",
        "set-option -p -t %2 @moa_view tree",
        "set-option -p -t %2 @moa_label ▤ sessions",
        "set-option -p -t %2 @moa_framed 1",
        "set-option -p -t %2 window-style fg=#4c566a",
        "set-option -p -t %2 window-active-style fg=#88c0d0",
    ]
    .map(String::from)
    .to_vec();
    want.extend(panel_tags(p(3), View::Console));
    want.extend(panel_tags(p(4), View::Plan));
    assert_eq!(lines(&b.chain), want);
    assert_eq!(b.views, vec![]);
    assert_eq!(
        b.leaves,
        vec![
            (Leaf::Panel(View::Tree), p(2)),
            (Leaf::Panel(View::Console), p(3)),
            (Leaf::Panel(View::Plan), p(4)),
        ]
    );
    assert_eq!(b.spot, None);
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(2), p(3), p(4)]);
    assert_eq!(w.session_of(p(1)), Some("moa"));
}

/// The stranger the keeper swap pushes into `_moa` leaves it right away, so a chain that
/// stops at any later command never leaves it there.
#[test]
fn a_chain_cut_after_the_swap_strands_no_stranger() {
    let mut w = shell_and_parked_panels();
    let c = w.cockpit_leaves();
    assert_eq!(c, vec![p(1)]);
    // A panel the layout doesn't use, in the cockpit too: it goes home after the shell.
    let cockpit = w.windows[0].id;
    let status = w.pane(cockpit, PaneSt::panel(View::Status));
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let l = lines(&b.chain);
    assert_eq!(l[0], "swap-pane -d -s %2 -t %1");
    assert_eq!(l[1], "break-pane -d -s %1 -t =moa: -n ⌨ shell");
    assert!(l.contains(&format!("break-pane -d -s {status} -t =_moa:")));
    for n in 2..=l.len() {
        let mut cut = w.clone();
        cut.run_first(&b.chain, n).unwrap();
        assert_eq!(cut.session_of(p(1)), Some("moa"), "cut after {n} commands");
    }
}

#[test]
fn the_focus_layout_nests() {
    let mut w = shell_and_parked_panels();
    let slot = w.park(View::EmptySlot);
    let b = built(plan(&w.scene(), &node("focus"), title));
    let l = lines(&b.chain);
    assert_eq!(
        starting_with(&l, "join-pane"),
        vec![
            format!("join-pane -d -h -s {slot} -t %2"),
            "join-pane -d -v -s %4 -t %2".into(),
            format!("join-pane -d -v -s %3 -t {slot}"),
        ]
    );
    assert_eq!(
        starting_with(&l, "resize-pane"),
        vec![
            "resize-pane -t %4 -y 45%",
            "resize-pane -t %3 -y 30%",
            "resize-pane -t %2 -x 24%",
        ]
    );
    assert_eq!(b.spot, Some(slot));
    // The empty slot is tagged as a panel showing itself.
    let tags = panel_tags(slot, View::EmptySlot);
    assert!(tags.iter().all(|t| l.contains(t)), "{l:#?}");
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(2), p(4), slot, p(3)]);
}

#[test]
fn a_switch_reuses_panels_before_starting_one() {
    let mut w = shell_and_parked_panels();
    w.run(&built(plan(&w.scene(), &node("cockpit"), title)).chain)
        .unwrap();
    let b = built(plan(&w.scene(), &node("triage"), title));
    assert_eq!(b.views, vec![(p(2), View::Status)]);
    assert_eq!(
        b.leaves,
        vec![
            (Leaf::Panel(View::Status), p(2)),
            (Leaf::Panel(View::Console), p(3)),
            (Leaf::Panel(View::Plan), p(4)),
        ]
    );
    let l = lines(&b.chain);
    assert!(l.contains(&"set-option -p -t %2 @moa_view status".to_string()));
    assert_eq!(starting_with(&l, "swap-pane"), Vec::<&str>::new());
    assert_eq!(starting_with(&l, "break-pane"), Vec::<&str>::new());
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(2), p(3), p(4)]);
}

#[test]
fn kept_panes_stay_in_the_window() {
    let mut w = focus_built(spot_agent(1));
    let b = built(plan(&w.scene(), &node("focus"), title));
    let l = lines(&b.chain);
    assert_eq!(starting_with(&l, "break-pane"), Vec::<&str>::new());
    assert_eq!(starting_with(&l, "swap-pane"), Vec::<&str>::new());
    assert_eq!(
        starting_with(&l, "join-pane"),
        vec![
            "join-pane -d -h -s %3 -t %1",
            "join-pane -d -v -s %2 -t %1",
            "join-pane -d -v -s %4 -t %3",
        ]
    );
    assert!(agent_tags(p(3), "Lantern").iter().all(|t| l.contains(t)));
    assert_eq!(b.spot, Some(p(3)));
    assert_eq!(b.views, vec![]);
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(1), p(2), p(3), p(4)]);
}

#[test]
fn the_spot_agent_goes_home_when_the_layout_has_no_spot() {
    let mut w = focus_built(spot_agent(1));
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let l = lines(&b.chain);
    let home = [
        "break-pane -d -s %3 -t =moa: -n Lantern",
        "set-option -p -u -t %3 @moa_view",
        "set-option -p -u -t %3 @moa_label",
    ];
    let at = l.iter().position(|x| x == home[0]).expect("the break");
    assert_eq!(l[at..at + 3], home);
    assert!(!l.iter().any(|x| x.contains("@moa_term")));
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(1), p(4), p(2)]);
    let (wi, _) = w.where_is(p(3)).unwrap();
    assert_eq!(
        (w.windows[wi].session.as_str(), w.windows[wi].name.as_str()),
        ("moa", "Lantern")
    );
}

#[test]
fn a_dead_panel_keeps_its_leaf() {
    let mut w = World::new();
    let c = w.cockpit();
    w.pane(c, PaneSt::panel(View::Tree));
    w.pane(c, PaneSt::panel(View::Console));
    w.pane(c, PaneSt::panel(View::Plan).dead());
    // A live status panel is parked: it could play `plan`, but the dead plan keeps its leaf.
    w.park(View::Status);
    let b = built(plan(&w.scene(), &node("classic"), title));
    assert!(b.leaves.contains(&(Leaf::Panel(View::Plan), p(3))));
    assert!(!b.views.iter().any(|(pane, _)| *pane == p(3)));
    assert_eq!(b.views, vec![]);
    assert!(lines(&b.chain).contains(&"join-pane -d -h -s %3 -t %2".to_string()));
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(2), p(3), p(1)]);
    assert!(w.panes[&p(3)].dead);
}

#[test]
fn a_title_with_a_hash_is_literal() {
    // Session 5 is titled `fix #12`.
    let w = focus_built(spot_agent(5));
    let cockpit = built(plan(&w.scene(), &node("cockpit"), title)).chain;
    assert!(
        cockpit
            .line()
            .contains("break-pane -d -s %3 -t =moa: -n 'fix #12'"),
        "{}",
        cockpit.line()
    );
    let focus = built(plan(&w.scene(), &node("focus"), title)).chain;
    assert!(lines(&focus).contains(&"set-option -p -t %3 @moa_label ● fix ##12".to_string()));

    let mut w = focus_built(spot_agent(5));
    let tab = w.window("moa", "Lantern");
    w.pane(tab, PaneSt::agent(sid(1)));
    let swap = swap_in(&w.scene(), p(5), title).unwrap();
    assert!(lines(&swap).contains(&"rename-window -t %3 -- fix ##12".to_string()));
    w.run(&swap).unwrap();
    let (wi, _) = w.where_is(p(3)).unwrap();
    assert_eq!(w.windows[wi].name, "fix #12");
}

#[test]
fn titles_are_cut_to_24_characters() {
    let w = focus_built(spot_agent(6));
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let cut: String = TITLES[5].chars().take(24).collect();
    assert_eq!(cut, "a title well past twenty");
    assert!(lines(&b.chain).contains(&format!("break-pane -d -s %3 -t =moa: -n {cut}")));
}

/// The cut counts characters, so a title in Hebrew, or with an emoji, is never cut inside one.
#[test]
fn a_multibyte_title_is_cut_on_a_character() {
    let w = focus_built(spot_agent(7));
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let cut: String = TITLES[6].chars().take(24).collect();
    assert_eq!(cut.chars().count(), 24);
    assert!(cut.len() > 24, "the cut is past byte 24");
    assert!(TITLES[6].starts_with(&cut));
    assert!(lines(&b.chain).contains(&format!("break-pane -d -s %3 -t =moa: -n {cut}")));
    let focus = built(plan(&w.scene(), &node("focus"), title));
    assert!(lines(&focus.chain).contains(&format!("set-option -p -t %3 @moa_label ● {cut}")));
}

/// A tab named `moa` in session `moa` would read as the cockpit, so an agent titled `moa`
/// never gets one; its frame still says `● moa`.
#[test]
fn an_agent_titled_moa_never_names_a_tab_moa() {
    // Session 3 is titled `moa`.
    let w = focus_built(spot_agent(3));
    let home = built(plan(&w.scene(), &node("cockpit"), title));
    assert!(
        lines(&home.chain).contains(&"break-pane -d -s %3 -t =moa: -n moa (agent)".to_string())
    );
    let mut out = w.clone();
    out.run(&home.chain).unwrap();
    assert_eq!(out.cockpit_leaves(), vec![p(1), p(4), p(2)]);
    let focus = built(plan(&w.scene(), &node("focus"), title));
    assert!(lines(&focus.chain).contains(&"set-option -p -t %3 @moa_label ● moa".to_string()));

    let mut w = w;
    let tab = w.window("moa", "Lantern");
    w.put(tab, p(9), PaneSt::agent(sid(1)));
    let swap = swap_in(&w.scene(), p(9), title).unwrap();
    assert!(lines(&swap).contains(&"rename-window -t %3 -- moa (agent)".to_string()));
    w.run(&swap).unwrap();
    let cockpits = w
        .windows
        .iter()
        .filter(|x| x.session == "moa" && x.name == "moa");
    assert_eq!(cockpits.count(), 1);
}

/// When the keeper must come in and every cockpit pane is needed, a panel is swapped out
/// for it, never the spot's agent: a chain that stops before the joins leaves the agent on
/// screen.
#[test]
fn the_keeper_swap_never_parks_the_spots_agent() {
    let mut w = World::new();
    let c = w.cockpit();
    let agent = w.pane(c, spot_agent(1));
    let console = w.pane(c, PaneSt::panel(View::Console));
    let plan_pane = w.pane(c, PaneSt::panel(View::Plan));
    let tree = w.park(View::Tree);
    let b = built(plan(&w.scene(), &node("focus"), title));
    let l = lines(&b.chain);
    assert_eq!(l[0], format!("swap-pane -d -s {tree} -t {console}"));
    for n in 1..=l.len() {
        let mut cut = w.clone();
        cut.run_first(&b.chain, n).unwrap();
        assert!(cut.in_cockpit(agent), "cut after {n} commands");
    }
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![tree, plan_pane, agent, console]);
}

/// Two spot roles in the cockpit (the user's own moves): the agent is the spot.
#[test]
fn the_spot_is_the_agent_before_an_empty_slot() {
    let mut w = World::new();
    let c = w.cockpit();
    let slot = w.pane(c, PaneSt::panel(View::EmptySlot));
    let agent = w.pane(c, spot_agent(1));
    for view in [View::Tree, View::Plan, View::Console] {
        w.park(view);
    }
    let scene = w.scene();
    assert_eq!(scene.pane(slot).map(|s| s.role), Some(Role::Spot(None)));
    assert_eq!(scene.spot().map(|s| s.pane), Some(agent));
    let b = built(plan(&scene, &node("focus"), title));
    assert_eq!(b.spot, Some(agent));
    // The slot is the keeper swap's C: the tree's place in `_moa` is already its home.
    assert_eq!(lines(&b.chain)[0], format!("swap-pane -d -s %3 -t {slot}"));
    w.run(&b.chain).unwrap();
    assert_eq!(w.session_of(slot), Some("_moa"));
    assert_eq!(w.cockpit_leaves(), vec![p(3), p(4), agent, p(5)]);
}

#[test]
fn no_moa_session_builds_nothing() {
    let mut w = World::new();
    w.sessions.retain(|s| s != "moa");
    for view in [View::Tree, View::Console, View::Plan] {
        w.park(view);
    }
    let plan = plan(&w.scene(), &node("cockpit"), title);
    assert_eq!(
        plan,
        Plan {
            spawn: vec![],
            build: None
        }
    );
    // With nothing parked, nothing is started either.
    let mut empty = World::new();
    empty.sessions.retain(|s| s != "moa");
    assert_eq!(planned(&empty, "focus"), (vec![], None));
}

fn planned(w: &World, name: &str) -> (Vec<View>, Option<Build>) {
    let p = plan(&w.scene(), &node(name), title);
    (p.spawn, p.build)
}

#[test]
fn a_missing_cockpit_is_made_from_the_first_leaf() {
    let mut w = World::new();
    let tab = w.window("moa", "⌨ shell");
    let shell = w.pane(tab, PaneSt::stranger());
    for view in [View::Tree, View::Console, View::Plan] {
        w.park(view);
    }
    let scene = w.scene();
    assert_eq!(scene.cockpit, None);
    assert!(scene.moa_session);
    let b = built(plan(&scene, &node("cockpit"), title));
    let l = lines(&b.chain);
    assert_eq!(l[0], "break-pane -d -s %2 -t =moa: -n moa");
    assert_eq!(l[1], "join-pane -d -h -s %3 -t %2");
    w.run(&b.chain).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(2), p(3), p(4)]);
    assert_eq!(w.where_is(shell).map(|(wi, _)| w.windows[wi].id), Some(tab));
}

#[test]
fn swap_in_parks_the_empty_slot() {
    for split in [false, true] {
        let mut w = focus_built(PaneSt::panel(View::EmptySlot));
        let tab = w.window("moa", "Lantern");
        w.put(tab, p(9), PaneSt::agent(sid(1)));
        let shell = split.then(|| w.pane(tab, PaneSt::stranger()));
        let swap = swap_in(&w.scene(), p(9), title).unwrap();
        let mut want = vec![
            "swap-pane -d -s %9 -t %3".to_string(),
            "break-pane -d -s %3 -t =_moa:".into(),
        ];
        want.extend(agent_tags(p(9), "Lantern"));
        assert_eq!(lines(&swap), want, "split: {split}");
        w.run(&swap).unwrap();
        assert_eq!(w.cockpit_leaves(), vec![p(1), p(2), p(9), p(4)]);
        assert_eq!(w.session_of(p(3)), Some("_moa"));
        if let Some(shell) = shell {
            let (wi, _) = w.where_is(shell).unwrap();
            assert_eq!(w.windows[wi].id, tab);
            assert_eq!(w.windows[wi].panes, vec![shell]);
        }
    }
}

/// A dead agent's pane (kept by the user's own `remain-on-exit`) never takes the spot from
/// the live one there: the caller only selects its tab.
#[test]
fn a_dead_agent_never_takes_the_spot() {
    let mut w = focus_built(spot_agent(2));
    let tab = w.window("moa", "Lantern");
    w.put(tab, p(9), PaneSt::agent(sid(1)).dead());
    assert_eq!(swap_in(&w.scene(), p(9), title), None);
    w.panes.get_mut(&p(9)).unwrap().dead = false;
    assert!(swap_in(&w.scene(), p(9), title).is_some());
}

#[test]
fn swap_in_sends_the_old_occupant_home_by_title() {
    let mut w = focus_built(spot_agent(2));
    let tab = w.window("moa", "Lantern");
    w.put(tab, p(9), PaneSt::agent(sid(1)));
    let scene = w.scene();
    let swap = swap_in(&scene, p(9), title).unwrap();
    let mut want = vec![
        "swap-pane -d -s %9 -t %3".to_string(),
        "rename-window -t %3 -- Kite".into(),
        "set-option -p -u -t %3 @moa_view".into(),
        "set-option -p -u -t %3 @moa_label".into(),
    ];
    want.extend(agent_tags(p(9), "Lantern"));
    assert_eq!(lines(&swap), want);
    // The spot's own agent, and a cockpit with no spot, need no swap.
    assert_eq!(swap_in(&scene, p(3), title), None);
    let no_spot = shell_and_parked_panels();
    let mut no_spot = no_spot;
    let t = no_spot.window("moa", "Lantern");
    no_spot.put(t, p(9), PaneSt::agent(sid(1)));
    assert_eq!(swap_in(&no_spot.scene(), p(9), title), None);

    w.run(&swap).unwrap();
    assert_eq!(w.cockpit_leaves(), vec![p(1), p(2), p(9), p(4)]);
    let (wi, _) = w.where_is(p(3)).unwrap();
    assert_eq!(
        (w.windows[wi].session.as_str(), w.windows[wi].name.as_str()),
        ("moa", "Kite")
    );
    assert_eq!(w.panes[&p(3)].tags.get("@moa_view"), None);
}

/// What the caller puts after `respawn-pane -k -t <spot>` (R3).
fn respawn_tail() -> Vec<String> {
    [
        "-c",
        "/h",
        "-e",
        "MOA_BRAIN=default",
        "/x/moa",
        "panel",
        "empty-slot",
        "--brain",
        "default",
    ]
    .map(String::from)
    .to_vec()
}

#[test]
fn vacate_swaps_a_spare_or_respawns_in_place() {
    let mut spare = World::new();
    let c = spare.cockpit();
    spare.pane(c, PaneSt::panel(View::Tree));
    spare.put(c, p(9), spot_agent(1));
    let slot = spare.park(View::EmptySlot);
    let mut none = spare.clone();
    let cmd = vacate(&spare.scene(), p(9), &respawn_tail());
    assert_eq!(
        cmd.line(),
        format!("swap-pane -d -s {slot} -t %9 ; kill-pane -t %9")
    );
    spare.run(&cmd).unwrap();
    assert_eq!(spare.cockpit_leaves(), vec![p(1), slot]);
    assert!(!spare.panes.contains_key(&p(9)));

    // A stopped slot is no spare: the spot is respawned, and the stopped one left alone.
    none.panes.get_mut(&slot).unwrap().dead = true;
    let cmd = vacate(&none.scene(), p(9), &respawn_tail());
    let mut want = vec![
        "respawn-pane -k -t %9 -c /h -e MOA_BRAIN=default /x/moa panel empty-slot --brain default"
            .to_string(),
        "set-option -p -u -t %9 @moa_session".into(),
        "set-option -p -u -t %9 @moa_adopted".into(),
        "set-option -p -u -t %9 @moa_term".into(),
    ];
    want.extend(panel_tags(p(9), View::EmptySlot));
    want.extend([
        "set-option -p -t %9 remain-on-exit on".into(),
        "set-option -p -t %9 remain-on-exit-format panel stopped · Ctrl-a r brings it back".into(),
    ]);
    assert_eq!(lines(&cmd), want);
    none.run(&cmd).unwrap();
    assert_eq!(none.cockpit_leaves(), vec![p(1), p(9)]);
    let scene = none.scene();
    assert_eq!(
        scene.spot().map(|s| (s.pane, s.role)),
        Some((p(9), Role::Spot(None)))
    );
    assert_eq!(none.respawned, vec![p(9)]);
    assert_eq!(none.session_of(slot), Some("_moa"));
}

#[test]
fn an_unbound_agent_with_its_tag_goes_home_by_title() {
    let mut w = focus_built(spot_agent(1).unbound());
    assert_eq!(
        w.scene().spot().map(|s| s.role),
        Some(Role::Spot(Some(sid(1))))
    );
    let b = built(plan(&w.scene(), &node("cockpit"), title));
    let l = lines(&b.chain);
    assert!(l.contains(&"break-pane -d -s %3 -t =moa: -n Lantern".to_string()));
    assert!(!l.iter().any(|x| x.contains("@moa_term")));
    w.run(&b.chain).unwrap();

    // Tagged `agent` with neither a binding nor a session tag: a shell.
    let mut w = focus_built(PaneSt::stranger().tag("@moa_view", "agent"));
    let scene = w.scene();
    assert_eq!(scene.pane(p(3)).map(|s| s.role), Some(Role::Other));
    assert_eq!(scene.spot(), None);
    let b = built(plan(&scene, &node("cockpit"), title));
    let l = lines(&b.chain);
    let home = [
        "break-pane -d -s %3 -t =moa: -n ⌨ shell",
        "set-option -p -t %3 @moa_term 1",
        "set-option -p -t %3 @moa_label ⌨ shell",
        "set-option -p -u -t %3 @moa_view",
        "set-option -p -u -t %3 @moa_framed",
        "set-option -p -u -t %3 window-style",
        "set-option -p -u -t %3 window-active-style",
    ];
    let at = l.iter().position(|x| x == home[0]).expect("the break");
    assert_eq!(l[at..at + home.len()], home);
    w.run(&b.chain).unwrap();
    assert_eq!(w.panes[&p(3)].tags.get("@moa_view"), None);
}

/// A stranger still wearing a panel's frame (a view tag moa doesn't know, and the frame's
/// options) goes home as a plain terminal: no view, no frame.
#[test]
fn a_framed_stranger_loses_its_frame_on_the_way_home() {
    let framed = PaneSt::stranger()
        .tag("@moa_view", "old-view")
        .tag("@moa_framed", "1")
        .tag("window-style", "fg=#4c566a")
        .tag("window-active-style", "fg=#88c0d0");
    let mut w = focus_built(framed);
    let scene = w.scene();
    assert_eq!(
        scene.pane(p(3)).map(|s| (s.role, s.tagged)),
        Some((Role::Other, true))
    );
    let b = built(plan(&scene, &node("cockpit"), title));
    w.run(&b.chain).unwrap();
    let tags: Vec<&str> = w.panes[&p(3)].tags.keys().map(String::as_str).collect();
    assert_eq!(tags, vec!["@moa_label", "@moa_term"]);
    assert_eq!(w.session_of(p(3)), Some("moa"));
}

#[test]
fn a_scene_reads_back_from_its_world() {
    let mut w = focus_built(spot_agent(1));
    let tab = w.window("moa", "Kite");
    w.pane(tab, PaneSt::agent(sid(2)));
    w.pane(tab, PaneSt::stranger());
    w.park(View::Status);
    let picture = w.park(View::Picture);
    w.panes.get_mut(&picture).unwrap().dead = true;
    let scene = w.scene();
    assert_eq!(World::from(&scene).scene(), scene);
}

// ---------- the proptest ----------

/// A random scene: where agents, strangers and panels start.
#[derive(Clone, Debug)]
struct Spec {
    cockpit: bool,
    /// Agents in their own tabs; `true` has a split shell (the first two only).
    agents: Vec<bool>,
    spot: bool,
    tagged_outside: bool,
    /// Strangers in the cockpit; `true` is tagged `@moa_view agent`.
    strangers: Vec<bool>,
    /// (view, in the cockpit, dead).
    panels: Vec<(usize, bool, bool)>,
}

#[derive(Clone, Debug)]
enum Step {
    Plan(usize),
    SwapIn(usize),
    Vacate,
}

const VIEWS: [View; 5] = [
    View::Tree,
    View::Console,
    View::Plan,
    View::Status,
    View::EmptySlot,
];

fn spec() -> impl Strategy<Value = Spec> {
    (
        prop::bool::weighted(0.85),
        prop::collection::vec(any::<bool>(), 0..=3),
        any::<bool>(),
        any::<bool>(),
        prop::collection::vec(any::<bool>(), 0..=2),
        prop::collection::vec(
            (0..VIEWS.len(), any::<bool>(), prop::bool::weighted(0.3)),
            0..=5,
        ),
    )
        .prop_map(
            |(cockpit, agents, spot, tagged_outside, strangers, panels)| Spec {
                cockpit,
                agents,
                spot,
                tagged_outside,
                strangers,
                panels,
            },
        )
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            (0..PRESET_KEYS.len()).prop_map(Step::Plan),
            any::<usize>().prop_map(Step::SwapIn),
            Just(Step::Vacate),
        ],
        1..=6,
    )
}

fn world_of(spec: &Spec) -> World {
    let mut w = World::new();
    let mut next = 0u128;
    let mut id = || {
        next += 1;
        sid(next)
    };
    let cockpit = spec.cockpit.then(|| w.cockpit());
    let mut splits = 0;
    for split in &spec.agents {
        let session = id();
        let tab = w.window("moa", &format!("{} tab", title(session)));
        w.pane(tab, PaneSt::agent(session));
        if *split && splits < 2 {
            splits += 1;
            w.pane(tab, PaneSt::stranger());
        }
    }
    if spec.tagged_outside {
        let session = id();
        let tab = w.window("moa", &format!("{} tab", title(session)));
        let label = format!("● {}", title(session));
        w.pane(
            tab,
            PaneSt::agent(session)
                .tag("@moa_view", "agent")
                .tag("@moa_label", &label),
        );
    }
    if let Some(c) = cockpit {
        if spec.spot {
            w.pane(c, PaneSt::agent(id()).tag("@moa_view", "agent"));
        }
        for tagged in &spec.strangers {
            let st = PaneSt::stranger();
            w.pane(
                c,
                if *tagged {
                    st.tag("@moa_view", "agent")
                } else {
                    st
                },
            );
        }
    }
    for (view, in_cockpit, dead) in &spec.panels {
        let st = PaneSt {
            dead: *dead,
            ..PaneSt::panel(VIEWS[*view])
        };
        match cockpit {
            Some(c) if *in_cockpit => {
                w.pane(c, st);
            }
            _ => {
                let parked = w.park(VIEWS[*view]);
                w.panes.get_mut(&parked).unwrap().dead = *dead;
            }
        }
    }
    if let Some(c) = cockpit
        && w.windows.iter().any(|x| x.id == c && x.panes.is_empty())
    {
        // A window always holds a pane: the login shell.
        w.pane(c, PaneSt::stranger());
    }
    if !w.windows.iter().any(|x| x.session == "moa") {
        let tab = w.window("moa", "⌨ shell");
        w.pane(tab, PaneSt::stranger());
    }
    w
}

/// After every step (Review Focus 1): nothing lost or stranded, nothing dead touched.
fn check(
    before: &BTreeSet<PaneId>,
    dead: &[(PaneId, Option<String>)],
    may_kill: Option<PaneId>,
    w: &World,
) -> Result<(), TestCaseError> {
    let now: BTreeSet<PaneId> = w.panes.keys().copied().collect();
    let mut want = before.clone();
    if let Some(k) = may_kill
        && !now.contains(&k)
    {
        want.remove(&k);
    }
    prop_assert_eq!(&now, &want, "a pane was lost or appeared");
    for (pane, st) in &w.panes {
        let session = w.session_of(*pane).unwrap();
        if w.in_cockpit(*pane) {
            continue;
        }
        if st.is_agent() {
            prop_assert_eq!(session, "moa", "agent {} stranded", pane);
        } else if st.panel_view().is_some() {
            prop_assert_eq!(session, "_moa", "panel {} stranded", pane);
        }
        if st.kind == Kind::Stranger {
            prop_assert_ne!(session, "_moa", "stranger {} in _moa", pane);
        }
    }
    for (pane, view) in dead {
        let st = w.panes.get(pane);
        prop_assert!(st.is_some(), "dead {} killed", pane);
        prop_assert_eq!(&st.unwrap().tags.get("@moa_view").cloned(), view);
        prop_assert!(!w.respawned.contains(pane), "dead {} respawned", pane);
    }
    let cockpits = w
        .windows
        .iter()
        .filter(|x| x.session == "moa" && x.name == "moa")
        .count();
    prop_assert!(cockpits <= 1, "two cockpits");
    Ok(())
}

proptest! {
    /// Any sequence of layout switches, swaps into the spot and vacates, from any scene,
    /// keeps every pane, in its right place, and leaves dead panes alone.
    #[test]
    fn no_sequence_kills_or_strands_a_pane(spec in spec(), steps in steps()) {
        let start = world_of(&spec).scene();
        let mut w = World::from(&start);
        prop_assert_eq!(&w.scene(), &start);
        let dead: Vec<(PaneId, Option<String>)> = w
            .panes
            .iter()
            .filter(|(_, st)| st.dead)
            .map(|(p, st)| (*p, st.tags.get("@moa_view").cloned()))
            .collect();
        for step in steps {
            let scene = w.scene();
            let mut before: BTreeSet<PaneId> = w.panes.keys().copied().collect();
            let mut may_kill = None;
            match step {
                Step::Plan(i) => {
                    let layout = node(PRESET_KEYS[i]);
                    let mut planned = plan(&scene, &layout, title);
                    if !planned.spawn.is_empty() {
                        prop_assert_eq!(&planned.build, &None);
                        for view in &planned.spawn {
                            before.insert(w.park(*view));
                        }
                        planned = plan(&w.scene(), &layout, title);
                        prop_assert_eq!(&planned.spawn, &Vec::<View>::new());
                    }
                    let b = planned.build.expect("session moa is there");
                    for (pane, _) in &b.views {
                        prop_assert!(!w.panes[pane].dead, "dead {} reassigned", pane);
                    }
                    w.run(&b.chain).map_err(TestCaseError::fail)?;
                    let wanted: Vec<Leaf> = b.leaves.iter().map(|(l, _)| *l).collect();
                    prop_assert_eq!(wanted, leaves(&layout));
                    let panes: Vec<PaneId> = b.leaves.iter().map(|(_, p)| *p).collect();
                    prop_assert_eq!(w.cockpit_leaves(), panes);
                }
                Step::SwapIn(k) => {
                    let agents: Vec<PaneId> = w
                        .panes
                        .iter()
                        .filter(|(_, st)| st.is_agent())
                        .map(|(p, _)| *p)
                        .collect();
                    if agents.is_empty() {
                        continue;
                    }
                    let pane = agents[k % agents.len()];
                    if let Some(cmd) = swap_in(&scene, pane, title) {
                        w.run(&cmd).map_err(TestCaseError::fail)?;
                        prop_assert!(w.in_cockpit(pane));
                        prop_assert_eq!(
                            w.scene().spot().map(|s| s.pane),
                            Some(pane)
                        );
                    }
                }
                Step::Vacate => {
                    let Some(spot) = scene.spot() else { continue };
                    let Role::Spot(Some(_)) = spot.role else { continue };
                    may_kill = Some(spot.pane);
                    let cmd = vacate(&scene, spot.pane, &respawn_tail());
                    w.run(&cmd).map_err(TestCaseError::fail)?;
                    let now = w.scene();
                    prop_assert_eq!(now.spot().map(|s| s.role), Some(Role::Spot(None)));
                }
            }
            check(&before, &dead, may_kill, &w)?;
        }
    }
}

/// A scene's spot and panes, as `Scene::read` sees them.
#[test]
fn a_scene_knows_its_spot_and_roles() {
    let mut w = focus_built(spot_agent(1));
    let tab = w.window("moa", "Kite");
    w.pane(tab, PaneSt::agent(sid(2)).tag("@moa_view", "agent"));
    let slot = w.park(View::EmptySlot);
    let other = w.window("work", "mine");
    let mine = w.pane(other, PaneSt::stranger());
    let scene: Scene = w.scene();
    assert_eq!(scene.spot().map(|s| s.pane), Some(p(3)));
    assert_eq!(scene.pane(p(5)).map(|s| s.role), Some(Role::Agent(sid(2))));
    assert_eq!(
        scene.pane(slot).map(|s| s.role),
        Some(Role::Panel(View::EmptySlot))
    );
    assert_eq!(
        scene.pane(mine).map(|s| s.place),
        Some(moa_core::cockpit::Place::Elsewhere)
    );
    assert!(scene.moa_session);
    assert!(!scene.users);
}
