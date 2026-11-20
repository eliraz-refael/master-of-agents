//! The focus model: what a user can see, and `seen` for sessions bound to visible panes,
//! every input through `handle_checked`.

use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Effect, Input, LogLevel, LogLine, Publish, RefuseWrites, TmuxIn, TmuxToken, check_order,
    handle_checked,
};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{Activity, AgentKind, Lifecycle, Location, PaneBinding, Session, SessionId};
use moa_core::publish::CoreEvent;
use moa_core::state::{LoadNote, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::{Applied, Topology, refresh_cmds};
use moa_core::{Now, UtcMs};

const SID: &str = "00000000-0000-4000-8000-000000000001";

fn now() -> Now {
    let wall = UtcMs(
        "2026-11-15T17:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    Now::at(
        wall,
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn sid() -> SessionId {
    SID.parse().unwrap()
}

fn done_session() -> Session {
    Session {
        id: sid(),
        agent_kind: AgentKind::Claude,
        account: None,
        cwd: None,
        start_cwd: None,
        project_id: None,
        title: Some("Tide tables".into()),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: None,
        activity_why: None,
        last_prompt: Some("Please continue".into()),
        last_turn_end_at: Some(UtcMs(10)),
        seen_at: Some(UtcMs(5)),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: None,
    }
}

fn binding(pid: i64, start: i64) -> PaneBinding {
    PaneBinding {
        session_id: sid(),
        server_pid: pid,
        server_start: start,
        socket: "sock".into(),
        pane_id: "%3".into(),
        pane_pid: 99,
        bound_at: UtcMs(1),
        tracked: true,
    }
}

fn snapshot(b: PaneBinding) -> Snapshot {
    Snapshot {
        sessions: vec![done_session()],
        pane_bindings: vec![b],
        ..Snapshot::default()
    }
}

fn started_with(snap: Snapshot) -> State {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut s, Input::Startup(Box::new(snap)), now());
    s
}

fn started() -> State {
    started_with(snapshot(binding(10, 1000)))
}

fn feed(s: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(s, Input::Tmux(t), now())
}

fn connect(s: &mut State, g: Generation) -> Vec<Effect> {
    feed(
        s,
        TmuxIn::Connected {
            conn: 1,
            generation: g,
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    )
}

fn g() -> Generation {
    Generation {
        pid: 10,
        start: 1000,
    }
}

fn lines(l: &[&str]) -> Result<Vec<String>, String> {
    Ok(l.iter().map(|s| s.to_string()).collect())
}

fn lists(panes: &[&str], clients: &[&str]) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: vec![
            lines(&["S $0 moa", "S $1 _moa"]),
            lines(panes),
            lines(clients),
            Ok(Vec::new()),
        ],
    }
}

/// `%3` is on screen for a user client.
const SEEN: &[&str] = &["P %3 @1 $0 1 1 0 0 "];
const USER: &[&str] = &["C 0 $0 /dev/ttys1"];

fn persisted(fx: &[Effect]) -> Vec<&Vec<Change>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn state_of(s: &State) -> DisplayState {
    display_state(s.session(sid()).unwrap(), now().wall).0
}

fn refreshes(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| {
            matches!(
                e,
                Effect::Tmux {
                    token: Some(TmuxToken::Refresh),
                    ..
                }
            )
        })
        .count()
}

/// Up, with the lists in.
fn up(s: &mut State, panes: &[&str], clients: &[&str]) -> Vec<Effect> {
    connect(s, g());
    feed(s, lists(panes, clients))
}

fn window_changed(window: u32) -> TmuxIn {
    TmuxIn::Notification(Notification::SessionWindowChanged {
        session: TmuxSessionId(0),
        window: WindowId(window),
    })
}

#[test]
fn a_visible_done_session_becomes_seen() {
    let mut s = started();
    assert_eq!(state_of(&s), DisplayState::Done);
    let fx = up(&mut s, SEEN, USER);
    check_order(&fx).unwrap();
    let ps = persisted(&fx);
    assert_eq!(ps.len(), 1);
    match &ps[0][..] {
        [Change::Session(row)] => {
            assert_eq!(row.id, sid());
            assert_eq!(row.seen_at, Some(now().wall));
        }
        other => panic!("{other:?}"),
    }
    let at = |f: fn(&Effect) -> bool| fx.iter().position(f).unwrap();
    let persist = at(|e| matches!(e, Effect::Persist(_)));
    let publish = at(|e| matches!(e, Effect::Publish(Publish::Model(_))));
    assert!(persist < publish);
    assert_eq!(state_of(&s), DisplayState::Idle);
}

/// tmux says which window is the cockpit: the one named `moa` in the session named `moa`,
/// whatever its index (`base-index 1` makes it 1).
#[test]
fn the_cockpit_is_the_window_named_moa() {
    let mut s = started();
    up(
        &mut s,
        &[
            "P %1 @1 $0 1 1 0 1 ",
            &format!("P %3 @2 $0 1 0 0 0 {SID}"),
            "P %4 @3 $1 1 1 0 0 ",
        ],
        USER,
    );
    let t = &s.tmux().topology;
    assert!(t.is_cockpit(WindowId(1)));
    assert!(!t.is_cockpit(WindowId(2)));
    assert!(!t.is_cockpit(WindowId(3)));
    assert!(!t.is_cockpit(WindowId(9)));
    // The tag still reads after the flag.
    assert_eq!(t.moa_session(PaneId(3)), Some(sid()));
    assert_eq!(t.moa_session(PaneId(1)), None);
    assert!(
        moa_core::tmux::topology::LIST_PANES
            .contains(" #{&&:#{==:#{session_name},moa},#{==:#{window_name},moa}} #{@moa_session}")
    );
}

#[test]
fn zoom_hides_the_other_panes() {
    let mut s = started();
    let fx = up(
        &mut s,
        &["P %3 @1 $0 0 1 1 0 ", "P %4 @1 $0 1 1 1 0 "],
        USER,
    );
    assert!(persisted(&fx).is_empty());
    assert_eq!(state_of(&s), DisplayState::Done);
}

#[test]
fn a_binding_from_another_generation_is_not_trusted() {
    let mut s = started_with(snapshot(binding(1, 1)));
    connect(&mut s, Generation { pid: 2, start: 2 });
    let fx = feed(&mut s, lists(SEEN, USER));
    assert!(persisted(&fx).is_empty());
    assert_eq!(state_of(&s), DisplayState::Done);
}

#[test]
fn a_binding_for_a_pane_that_is_gone_is_not_trusted() {
    let mut s = started();
    let fx = up(&mut s, &["P %9 @1 $0 1 1 0 0 "], USER);
    assert!(persisted(&fx).is_empty());
}

#[test]
fn focus_notifications_move_visibility() {
    // %3 starts in a background window.
    let mut s = started();
    let fx = up(
        &mut s,
        &["P %5 @1 $0 1 1 0 0 ", "P %3 @2 $0 1 0 0 0 "],
        USER,
    );
    assert!(persisted(&fx).is_empty());
    let fx = feed(&mut s, window_changed(2));
    assert_eq!(persisted(&fx).len(), 1);
    assert_eq!(state_of(&s), DisplayState::Idle);

    // %3 is behind the zoomed %4 until the pane change brings it forward.
    let mut s = started();
    let fx = up(
        &mut s,
        &["P %3 @2 $0 0 1 1 0 ", "P %4 @2 $0 1 1 1 0 "],
        USER,
    );
    assert!(persisted(&fx).is_empty());
    let fx = feed(
        &mut s,
        TmuxIn::Notification(Notification::WindowPaneChanged {
            window: WindowId(2),
            pane: PaneId(3),
        }),
    );
    assert_eq!(persisted(&fx).len(), 1);
    assert_eq!(state_of(&s), DisplayState::Idle);
}

#[test]
fn control_clients_make_nothing_visible() {
    let mut s = started();
    let fx = up(&mut s, SEEN, &["C 1 $0 client-9"]);
    assert!(persisted(&fx).is_empty());
    assert_eq!(state_of(&s), DisplayState::Done);
}

#[test]
fn a_detached_user_sees_nothing() {
    let mut s = started();
    let fx = up(&mut s, SEEN, &[]);
    assert!(persisted(&fx).is_empty());
    // Attached to another window, then detached, then that session moves to the pane's
    // window: nobody is looking.
    let mut s = started();
    up(
        &mut s,
        &["P %3 @1 $0 1 0 0 0 ", "P %5 @2 $0 1 1 0 0 "],
        USER,
    );
    feed(
        &mut s,
        TmuxIn::Notification(Notification::ClientDetached {
            client: ClientName("/dev/ttys1".into()),
        }),
    );
    let fx = feed(&mut s, window_changed(1));
    assert!(persisted(&fx).is_empty());
    assert_eq!(state_of(&s), DisplayState::Done);
}

#[test]
fn seen_is_written_once() {
    let mut s = started();
    let fx = up(&mut s, SEEN, USER);
    assert_eq!(persisted(&fx).len(), 1);
    // A later clock: only `end > seen` holds it back, not the same instant.
    let later = Now::at(
        UtcMs(now().wall.0 + 1_000),
        Duration::from_secs(2),
        TimeZone::posix("UTC0").unwrap(),
    );
    let fx = handle_checked(&mut s, Input::Tmux(window_changed(1)), later);
    assert!(persisted(&fx).is_empty());
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Publish(_))),
        "nothing changed, nothing published"
    );
}

#[test]
fn seen_is_never_before_the_turn_end() {
    let mut snap = snapshot(binding(10, 1000));
    let end = UtcMs(now().wall.0 + 5_000);
    snap.sessions[0].last_turn_end_at = Some(end);
    let mut s = started_with(snap);
    let fx = up(&mut s, SEEN, USER);
    assert_eq!(persisted(&fx).len(), 1);
    assert_eq!(s.session(sid()).unwrap().seen_at, Some(end));
    assert_eq!(state_of(&s), DisplayState::Idle);
    let fx = feed(&mut s, window_changed(1));
    assert!(persisted(&fx).is_empty());
}

/// Nothing warned, logged as an error or published as one.
fn quiet(fx: &[Effect]) -> bool {
    !fx.iter().any(|e| {
        matches!(
            e,
            Effect::Log(LogLine {
                level: LogLevel::Warn | LogLevel::Error,
                ..
            }) | Effect::Publish(Publish::Event(CoreEvent::Error { .. }))
        )
    })
}

#[test]
fn seen_respects_the_write_guards() {
    // A row this build doesn't fully know is left alone.
    let mut snap = snapshot(binding(10, 1000));
    snap.unknown.push(LoadNote {
        table: "sessions",
        key: sid().to_string(),
        reason: "x",
    });
    let mut s = started_with(snap);
    let fx = up(&mut s, SEEN, USER);
    assert!(persisted(&fx).is_empty());
    assert!(quiet(&fx));
    assert_eq!(state_of(&s), DisplayState::Done);

    // While writes are refused nothing is marked, and nothing fails.
    let mut s = started();
    handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::new(snapshot(binding(10, 1000))),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(now().wall.0 + 60_000),
                reason: "disk full".into(),
            }),
        },
        now(),
    );
    let fx = up(&mut s, SEEN, USER);
    assert!(persisted(&fx).is_empty());
    assert!(quiet(&fx));
    assert_eq!(state_of(&s), DisplayState::Done);
}

#[test]
fn structural_changes_refresh() {
    let mut s = started();
    up(&mut s, &["P %5 @1 $0 1 1 0 0 "], &[]);
    // %4 is no session's pane: an exit of a bound one would also put its session to sleep.
    let hooks = [
        "moa:hook pane-exited %4",
        "moa:hook pane-died %4",
        "moa:hook layout @1",
        "moa:hook client-attached /dev/ttys1",
    ];
    let mut all = vec![
        Notification::WindowAdd {
            window: WindowId(2),
        },
        Notification::WindowClose {
            window: WindowId(2),
        },
        Notification::SessionsChanged,
    ];
    all.extend(hooks.iter().map(|h| Notification::Message(h.to_string())));
    for n in all {
        let fx = feed(&mut s, TmuxIn::Notification(n.clone()));
        assert_eq!(refreshes(&fx), 1, "{n:?}");
        let cmds: Vec<Cmd> = match fx.last() {
            Some(Effect::Tmux { cmds, .. }) => cmds.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(cmds, refresh_cmds());
        feed(&mut s, lists(&["P %5 @1 $0 1 1 0 0 "], &[]));
    }
    // Other mailbox lines and unknown hooks don't.
    for m in ["moa:prompt 1 hi", "moa:hook nope x", "hello"] {
        let fx = feed(
            &mut s,
            TmuxIn::Notification(Notification::Message(m.into())),
        );
        assert_eq!(refreshes(&fx), 0, "{m}");
    }
}

fn renamed(window: u32, name: &str) -> Notification {
    Notification::WindowRenamed {
        window: WindowId(window),
        name: name.into(),
    }
}

/// L16: a rename is applied from its notification, with no listing. The cockpit is the
/// window named `moa` in the session named `moa`, so a rename can make one or end one.
#[test]
fn renames_apply_without_a_listing() {
    let sessions = ["S $0 moa".to_string(), "S $1 _moa".to_string()];
    let panes = [
        "P %1 @1 $0 1 1 0 1 ".to_string(),
        "P %2 @2 $0 1 0 0 0 ".to_string(),
        "P %3 @3 $1 1 1 0 0 ".to_string(),
    ];
    let mut t = Topology::from_lists(&sessions, &panes, &[]);
    assert_eq!(t.cockpit(), Some(WindowId(1)));
    // The cockpit's window takes another name: there is no cockpit.
    assert_eq!(t.apply(&renamed(1, "Tide tables")), Applied::Changed);
    assert!(!t.is_cockpit(WindowId(1)));
    assert_eq!(t.cockpit(), None);
    // `%unlinked-window-renamed @2 moa`, a window in the session `moa`: the cockpit.
    assert_eq!(t.apply(&renamed(2, "moa")), Applied::Changed);
    assert!(t.is_cockpit(WindowId(2)));
    assert_eq!(t.cockpit(), Some(WindowId(2)));
    assert_eq!(t.apply(&renamed(2, "moa")), Applied::Same);
    // A window named `moa` in another session isn't one.
    assert_eq!(t.apply(&renamed(3, "moa")), Applied::Same);
    assert!(!t.is_cockpit(WindowId(3)));
    // Renamed away, it isn't any more.
    assert_eq!(t.apply(&renamed(2, "Kite")), Applied::Changed);
    assert!(!t.is_cockpit(WindowId(2)));

    // The session `moa` renamed: no window in it is the cockpit, and the name moves.
    assert_eq!(t.apply(&renamed(2, "moa")), Applied::Changed);
    let session = |name: &str| Notification::SessionRenamed {
        session: TmuxSessionId(0),
        name: name.into(),
    };
    assert_eq!(t.apply(&session("Harbor")), Applied::Changed);
    assert!(!t.is_cockpit(WindowId(2)));
    assert_eq!(t.cockpit(), None);
    assert_eq!(t.session_named("Harbor"), Some(TmuxSessionId(0)));
    assert_eq!(t.session_named("moa"), None);
    assert_eq!(t.session_name_of(PaneId(2)), Some("Harbor"));
    assert_eq!(t.apply(&session("Harbor")), Applied::Same);
    // Renamed back to `moa`, which of its windows is named `moa` takes a listing: the lists
    // carry no window names.
    assert_eq!(t.apply(&session("moa")), Applied::Refresh);

    // Through the link, a rename sends no listing.
    let mut s = started();
    up(
        &mut s,
        &["P %1 @1 $0 1 1 0 1 ", &format!("P %3 @2 $0 1 0 0 0 {SID}")],
        USER,
    );
    for (n, cockpit) in [(renamed(1, "Kite"), None), (renamed(2, "moa"), Some(2))] {
        let fx = feed(&mut s, TmuxIn::Notification(n.clone()));
        assert_eq!(refreshes(&fx), 0, "{n:?}");
        assert_eq!(s.tmux().topology.cockpit(), cockpit.map(WindowId), "{n:?}");
    }
}

/// L16: the listing reads each pane's `@moa_view` tag and whether it is dead.
#[test]
fn the_listing_reads_tags_and_liveness() {
    let mut s = started();
    up(
        &mut s,
        &[
            "P %1 @1 $0 1 1 0 1  1 tree",
            &format!("P %3 @1 $0 0 1 0 1 {SID} 0 agent"),
            "P %4 @3 $1 1 1 0 0  0 ",
            // 4b's lines, before the two fields: alive and untagged.
            "P %5 @4 $0 1 0 0 0 ",
        ],
        USER,
    );
    let t = &s.tmux().topology;
    assert_eq!(t.view_of(PaneId(1)), Some("tree"));
    assert!(t.is_dead(PaneId(1)));
    assert_eq!(t.view_of(PaneId(3)), Some("agent"));
    assert!(!t.is_dead(PaneId(3)));
    assert_eq!(t.moa_session(PaneId(3)), Some(sid()));
    for p in [4, 5, 9] {
        assert_eq!(t.view_of(PaneId(p)), None, "%{p}");
        assert!(!t.is_dead(PaneId(p)), "%{p}");
    }
    assert_eq!(t.cockpit(), Some(WindowId(1)));
    let cockpit: Vec<PaneId> = t.panes_in(WindowId(1)).collect();
    assert_eq!(cockpit, [PaneId(1), PaneId(3)]);
    assert_eq!(t.panes_in(WindowId(9)).count(), 0);
    assert_eq!(t.session_name_of(PaneId(3)), Some("moa"));
    assert_eq!(t.session_name_of(PaneId(4)), Some("_moa"));
    assert_eq!(t.session_name_of(PaneId(9)), None);
    assert!(
        moa_core::tmux::topology::LIST_PANES
            .ends_with(" #{@moa_session} #{pane_dead} #{@moa_view}")
    );
}
