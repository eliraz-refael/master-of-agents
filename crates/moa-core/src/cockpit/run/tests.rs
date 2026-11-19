//! The queue (L17) where only the crate reaches it: until `kill_tab` queues a vacate (Task
//! 10), no op can, so a waiting `Vacate` is queued here directly (R4). Also the vacate
//! itself, the refill, and a chain's tags kept against an older listing.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;

use super::*;
use crate::config::{BrainName, Config};
use crate::core::handle;
use crate::effect::{Down, Input, TmuxIn, TmuxToken};
use crate::op::Op;
use crate::state::{CallerInfo, Deps, PanelLaunch};
use crate::tmux::ids::{ClientName, Generation};
use crate::tmux::notify::Notification;
use crate::{Now, UtcMs};

fn now() -> Now {
    Now::at(
        UtcMs(1_000_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn connected(pid: i64) -> TmuxIn {
    TmuxIn::Connected {
        conn: 1,
        generation: Generation { pid, start: 1000 },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
        socket: "moa".into(),
    }
}

fn feed(st: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle(st, Input::Tmux(t), now())
}

/// A started core whose panels are `/x/moa` at home `/h`, its link up, whose first listing
/// built nothing.
fn up() -> State {
    let deps = Deps {
        home: PathBuf::from("/h"),
        panel: PanelLaunch {
            exe: PathBuf::from("/x/moa"),
            env: Vec::new(),
        },
        ..Deps::default()
    };
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle(&mut st, Input::Startup(Box::default()), now());
    feed(&mut st, connected(10));
    // No session `moa` yet: the link's first listing builds nothing.
    feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: vec![Ok(vec!["S $1 _moa".into()]), Ok(Vec::new()), Ok(Vec::new())],
        },
    );
    st
}

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

/// A `list-panes` line: `%id` in `@window` of `$session`, in the cockpit or not, with its
/// `@moa_session` (empty: none) and `@moa_view` (empty: none).
fn pane(id: u32, window: u32, session: u32, cockpit: bool, moa: &str, view: &str) -> String {
    format!(
        "P %{id} @{window} ${session} 1 1 0 {} {moa} 0 {view}",
        u8::from(cockpit)
    )
}

fn lists(panes: Vec<String>) -> Vec<Result<Vec<String>, String>> {
    vec![
        Ok(vec!["S $0 moa".into(), "S $1 _moa".into()]),
        Ok(panes),
        Ok(vec!["C 0 $0 /dev/ttys1".into()]),
    ]
}

fn refresh(panes: Vec<String>) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: lists(panes),
    }
}

fn answer(n: u64, results: Vec<Result<Vec<String>, String>>) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results,
    }
}

/// `focus` built, with Lantern's agent (`%9`) in the spot; tree `%2`, plan `%4`, console
/// `%3`; with a spare empty slot `%5` parked, or none.
fn lantern_in_the_spot(spare: bool) -> Vec<String> {
    let lantern = sid(1).to_string();
    let mut panes = vec![
        pane(2, 1, 0, true, "", "tree"),
        pane(4, 1, 0, true, "", "plan"),
        pane(9, 1, 0, true, &lantern, "agent"),
        pane(3, 1, 0, true, "", "console"),
    ];
    if spare {
        panes.push(pane(5, 105, 1, false, "", "empty-slot"));
    }
    panes
}

/// Kite's agent (`%9`) in a tab of its own.
fn kite_in_its_tab() -> Vec<String> {
    vec![
        pane(2, 1, 0, true, "", "tree"),
        pane(3, 1, 0, true, "", "console"),
        pane(4, 1, 0, true, "", "plan"),
        pane(9, 7, 0, false, &sid(2).to_string(), ""),
    ]
}

fn queries(fx: &[Effect]) -> Vec<(u64, Vec<Cmd>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Query(n)),
            } => Some((*n, cmds.clone())),
            _ => None,
        })
        .collect()
}

fn the_listing(fx: &[Effect]) -> u64 {
    match &queries(fx)[..] {
        [(n, cmds)] if *cmds == refresh_cmds() => *n,
        other => panic!("not one listing: {other:?}"),
    }
}

/// The one chain asked in `fx`, as its query number and its lines.
fn the_chain(fx: &[Effect]) -> (u64, Vec<String>) {
    match &queries(fx)[..] {
        [(n, cmds)] if cmds.len() == 1 && *cmds != refresh_cmds() => (
            *n,
            cmds[0]
                .parts()
                .iter()
                .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
                .collect(),
        ),
        other => panic!("not one chain: {other:?}"),
    }
}

fn replies(fx: &[Effect]) -> Vec<(ReqId, Result<Done, OpError>)> {
    let mut out: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Reply { req, result, .. } => Some((*req, result.clone())),
            _ => None,
        })
        .collect();
    out.sort_by_key(|(r, _)| r.0);
    out
}

fn apply(st: &mut State, req: u64, name: &str) -> Vec<Effect> {
    handle(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op: Op::LayoutApply {
                name: Some(name.into()),
            },
        },
        now(),
    )
}

fn lost(req: u64) -> (ReqId, Result<Done, OpError>) {
    (ReqId(req), Err(OpError(LOST.into())))
}

fn vacate_of(st: &mut State, pane: u32) -> Vec<Effect> {
    queue(st, CockpitOp::Vacate { pane: PaneId(pane) })
}

#[test]
fn a_reload_unwedges_the_cockpit_queue() {
    let mut st = up();
    let old = the_listing(&apply(&mut st, 1, "focus"));
    assert!(queries(&vacate_of(&mut st, 9)).is_empty());
    assert!(queries(&apply(&mut st, 2, "triage")).is_empty());

    let fx = handle(
        &mut st,
        Input::Reload {
            snapshot: Box::default(),
            refuse_writes: None,
        },
        now(),
    );
    // Every request is told; the vacate, whose commit is saved, runs.
    assert_eq!(replies(&fx), [lost(1), lost(2)]);
    let n = the_listing(&fx);
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(kite_in_its_tab()))));
    assert_eq!(lines, ["kill-pane -t %9"]);
    assert!(queries(&feed(&mut st, answer(n, vec![Ok(Vec::new())]))).is_empty());

    // A key after that runs.
    let fx = feed(
        &mut st,
        TmuxIn::Notification(Notification::Message(
            "moa:key layout-1 %3 $0 /dev/ttys1".into(),
        )),
    );
    let key = the_listing(&fx);
    // The old operation's late answer is ignored: the key's still waits for its own.
    let fx = feed(&mut st, answer(old, lists(kite_in_its_tab())));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    let (_, lines) = the_chain(&feed(&mut st, answer(key, lists(kite_in_its_tab()))));
    assert!(
        lines.iter().any(|l| l.starts_with("select-pane")),
        "{lines:?}"
    );
}

#[test]
fn vacates_survive_a_link_drop() {
    for (pid, runs) in [(10, true), (11, false)] {
        let mut st = up();
        the_listing(&apply(&mut st, 1, "focus"));
        vacate_of(&mut st, 9);
        let fx = feed(&mut st, TmuxIn::Down(Down::Lost));
        assert_eq!(replies(&fx), [lost(1)]);
        assert!(queries(&fx).is_empty());
        // Back to the same server, the vacate runs; to another, its pane is gone anyway.
        let fx = feed(&mut st, connected(pid));
        if runs {
            let n = the_listing(&fx);
            let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(kite_in_its_tab()))));
            assert_eq!(lines, ["kill-pane -t %9"]);
        } else {
            assert!(queries(&fx).is_empty(), "{fx:?}");
            assert!(st.cockpit.waiting.is_empty());
        }
    }
}

/// L17's other half: a vacate in flight when the link goes is queued again, ahead.
#[test]
fn a_lost_link_queues_an_in_flight_vacate_again() {
    let mut st = up();
    the_listing(&vacate_of(&mut st, 9));
    assert!(queries(&apply(&mut st, 1, "focus")).is_empty());
    let fx = feed(&mut st, TmuxIn::Down(Down::Lost));
    assert_eq!(replies(&fx), [lost(1)]);
    assert!(
        fx.iter().all(|e| !matches!(e, Effect::Log(_))),
        "nothing was dropped: {fx:?}"
    );
    assert_eq!(st.cockpit.waiting, [CockpitOp::Vacate { pane: PaneId(9) }]);
    let n = the_listing(&feed(&mut st, connected(10)));
    let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(kite_in_its_tab()))));
    assert_eq!(lines, ["kill-pane -t %9"]);
}

/// R4: from the spot, the empty slot takes the agent's place: respawned in place (a panel
/// start), or a spare one swapped in. Elsewhere, a plain kill; gone, nothing.
#[test]
fn a_vacate_leaves_the_spot_whole() {
    let mut st = up();
    let n = the_listing(&vacate_of(&mut st, 9));
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(lantern_in_the_spot(false)))));
    assert_eq!(
        lines[0],
        "respawn-pane -k -t %9 -c /h -e MOA_BRAIN=default -e MOA_NO_AUTOSTART=1 /x/moa panel empty-slot --brain default"
    );
    assert!(lines.contains(&"set-option -p -t %9 @moa_view empty-slot".to_string()));
    assert_eq!(st.panel_starts, 0);
    assert_eq!(st.tmux.topology.moa_session(PaneId(9)), Some(sid(1)));
    feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(st.panel_starts, 1);
    assert_eq!(st.tmux.topology.view_of(PaneId(9)), Some("empty-slot"));
    // No longer a session's tab, at once: the chain took its session tag off.
    assert_eq!(st.tmux.topology.moa_session(PaneId(9)), None);

    let mut st = up();
    let n = the_listing(&vacate_of(&mut st, 9));
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(lantern_in_the_spot(true)))));
    assert_eq!(lines, ["swap-pane -d -s %5 -t %9", "kill-pane -t %9"]);
    feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(st.panel_starts, 0);

    let mut st = up();
    let n = the_listing(&vacate_of(&mut st, 9));
    let fx = feed(
        &mut st,
        answer(n, lists(lantern_in_the_spot(false)[..2].to_vec())),
    );
    assert!(queries(&fx).is_empty(), "gone: nothing to do");
    assert!(st.cockpit.in_flight.is_none());
}

/// A refill builds the current layout, selecting nothing, and a newer build never takes
/// its place.
#[test]
fn a_refill_builds_and_is_never_replaced() {
    let mut st = up();
    the_listing(&apply(&mut st, 1, "focus"));
    queue(&mut st, CockpitOp::Refill);
    let fx = apply(&mut st, 2, "triage");
    assert!(replies(&fx).is_empty(), "nothing replaced: {fx:?}");
    assert_eq!(st.cockpit.waiting.len(), 2);
    assert!(matches!(st.cockpit.waiting[0], CockpitOp::Refill));

    let mut st = up();
    let n = the_listing(&queue(&mut st, CockpitOp::Refill));
    let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(kite_in_its_tab()))));
    assert!(!lines.iter().any(|l| l.starts_with("select-")), "{lines:?}");
}

/// L17, and Task 6's M1: a chain's tags are the topology's at once, and a listing older
/// than the chain, which doesn't show a just-started panel's tag yet, doesn't bring back
/// the view the panel was started with.
#[test]
fn a_listing_older_than_a_chain_keeps_its_tags() {
    let mut st = up();
    st.tmux.view_sent(PaneId(2), "tree");
    st.tmux
        .views_set(&[(PaneId(2), Some("status")), (PaneId(9), None)]);
    assert_eq!(st.tmux.topology.view_of(PaneId(2)), Some("status"));
    feed(&mut st, TmuxIn::Notification(Notification::SessionsChanged));
    feed(
        &mut st,
        refresh(vec![
            pane(2, 102, 1, false, "", ""),
            pane(9, 7, 0, false, "", "agent"),
        ]),
    );
    assert_eq!(st.tmux.topology.view_of(PaneId(2)), Some("status"));
    // A tag the chain unset is the listing's again once it shows one.
    assert_eq!(st.tmux.topology.view_of(PaneId(9)), Some("agent"));
}

/// A vacate queued again after a lost link may find its chain already ran: the pane is the
/// empty slot now, and it is left alone. So is any pane that isn't an agent's.
#[test]
fn a_vacate_run_again_kills_no_panel() {
    let mut st = up();
    let n = the_listing(&vacate_of(&mut st, 9));
    // Its chain goes out, and tmux runs it, but the link goes before the answer.
    the_chain(&feed(&mut st, answer(n, lists(lantern_in_the_spot(false)))));
    feed(&mut st, TmuxIn::Down(Down::Lost));
    let n = the_listing(&feed(&mut st, connected(10)));
    let mut slot = lantern_in_the_spot(false);
    slot[2] = pane(9, 1, 0, true, "", "empty-slot");
    let fx = feed(&mut st, answer(n, lists(slot)));
    assert!(queries(&fx).is_empty(), "the empty slot stays: {fx:?}");
    assert!(st.cockpit.in_flight.is_none());

    // A panel parked, or a shell, is never killed by a vacate either.
    for view in ["tree", ""] {
        let mut st = up();
        let n = the_listing(&vacate_of(&mut st, 9));
        let mut panes = kite_in_its_tab();
        panes[3] = pane(9, 7, 0, false, "", view);
        let fx = feed(&mut st, answer(n, lists(panes)));
        assert!(queries(&fx).is_empty(), "{view:?}: {fx:?}");
    }
}
