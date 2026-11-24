//! The status line: counts, per-window state and the minute clock, every input through
//! `handle_checked`.

mod common;

use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Down, Effect, Input, LogLevel, LogLine, TimerKey, TmuxIn, TmuxToken, check_order,
    handle_checked,
};
use moa_core::model::{Activity, AgentKind, Lifecycle, Location, PaneBinding, Session, SessionId};
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::state::{CallerInfo, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::status::{clock_text, counts_text, next_minute};
use moa_core::{Now, UtcMs};

fn ts(t: &str) -> UtcMs {
    UtcMs(t.parse::<Timestamp>().unwrap().as_millisecond())
}

fn utc(t: &str) -> Now {
    Now::at(
        ts(t),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn now() -> Now {
    utc("2026-11-15T14:05:30Z")
}

fn sid(n: u32) -> SessionId {
    format!("00000000-0000-4000-8000-{n:012}").parse().unwrap()
}

fn session(n: u32, activity: Activity, lifecycle: Lifecycle) -> Session {
    Session {
        id: sid(n),
        agent_kind: AgentKind::Claude,
        account: None,
        cwd: None,
        start_cwd: None,
        project_id: None,
        title: Some(format!("Session {n}")),
        title_locked: false,
        group_path: None,
        lifecycle,
        activity,
        activity_at: None,
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: None,
    }
}

fn live(n: u32, activity: Activity) -> Session {
    session(n, activity, Lifecycle::Live)
}

/// An idle session that finished a turn nobody has seen.
fn done(n: u32) -> Session {
    Session {
        last_turn_end_at: Some(UtcMs(10)),
        seen_at: Some(UtcMs(5)),
        ..live(n, Activity::Idle)
    }
}

fn binding(n: u32, pane: &str) -> PaneBinding {
    PaneBinding {
        session_id: sid(n),
        server_pid: 10,
        server_start: 1000,
        socket: "sock".into(),
        pane_id: pane.into(),
        pane_pid: 99,
        bound_at: UtcMs(1),
        tracked: true,
    }
}

fn started_with(snap: Snapshot) -> State {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut s, Input::Startup(Box::new(snap)), now());
    s
}

fn started(sessions: Vec<Session>, bindings: Vec<PaneBinding>) -> State {
    started_with(Snapshot {
        sessions,
        pane_bindings: bindings,
        ..Snapshot::default()
    })
}

fn feed_at(s: &mut State, t: TmuxIn, at: Now) -> Vec<Effect> {
    handle_checked(s, Input::Tmux(t), at)
}

fn feed(s: &mut State, t: TmuxIn) -> Vec<Effect> {
    feed_at(s, t, now())
}

fn connect(s: &mut State) -> Vec<Effect> {
    feed(
        s,
        TmuxIn::Connected {
            conn: 1,
            generation: Generation {
                pid: 10,
                start: 1000,
            },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    )
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

const USER: &[&str] = &["C 0 $0 /dev/ttys1"];
const CONTROL_ONLY: &[&str] = &["C 1 $0 client-9"];

fn up_at(s: &mut State, panes: &[&str], clients: &[&str], at: Now) -> Vec<Effect> {
    connect(s);
    feed_at(s, lists(panes, clients), at)
}

/// Every command of the untokened `Tmux` effects, as lines.
fn written(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds),
            _ => None,
        })
        .flatten()
        .map(Cmd::line)
        .collect()
}

fn writes_of(fx: &[Effect], option: &str) -> Vec<String> {
    written(fx)
        .into_iter()
        .filter(|l| l.split(' ').any(|w| w == option))
        .collect()
}

fn untokened(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::Tmux { token: None, .. }))
        .count()
}

fn timers(fx: &[Effect]) -> Vec<(TimerKey, UtcMs)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::SetTimer { key, at } => Some((*key, *at)),
            _ => None,
        })
        .collect()
}

#[test]
fn counts_text_matches_the_spike() {
    let mut rows = Vec::new();
    let mut n = 0;
    let mut add = |count: u32, f: &dyn Fn(u32) -> Session| {
        for _ in 0..count {
            n += 1;
            rows.push(f(n));
        }
    };
    add(2, &|n| live(n, Activity::NeedsPermission));
    add(1, &|n| live(n, Activity::NeedsAnswer));
    add(1, &|n| live(n, Activity::Failed));
    add(3, &done);
    add(4, &|n| live(n, Activity::Working));
    add(19, &|n| live(n, Activity::Idle));
    // Not counted.
    add(2, &|n| session(n, Activity::Idle, Lifecycle::Ended));
    add(1, &|n| Session {
        asleep_at: Some(UtcMs(3)),
        ..session(n, Activity::Idle, Lifecycle::Ended)
    });
    add(1, &|n| Session {
        archived_at: Some(UtcMs(3)),
        ..live(n, Activity::Idle)
    });
    let s = started(rows, vec![]);
    assert_eq!(
        counts_text(&s),
        "#[fg=#ebcb8b]⚠ 2  #[fg=#d08770]? 1  #[fg=#d57780]✗ 1  #[fg=#a3be8c]✓ 3  \
         #[fg=#88c0d0]◐ 4  #[fg=#8c9ab3]● 19"
    );

    // Zeros are left out.
    let s = started(vec![live(1, Activity::Working)], vec![]);
    assert_eq!(counts_text(&s), "#[fg=#88c0d0]◐ 1");

    // No live sessions.
    let s = started(vec![session(1, Activity::Idle, Lifecycle::Ended)], vec![]);
    assert_eq!(counts_text(&s), "");
    let s = started(vec![], vec![]);
    assert_eq!(counts_text(&s), "");
}

#[test]
fn the_status_is_written_once_per_change() {
    let mut s = started(
        vec![live(1, Activity::Working), live(2, Activity::Idle)],
        vec![],
    );
    // Connecting writes the counts at once; the lists change nothing.
    let fx = connect(&mut s);
    check_order(&fx).unwrap();
    assert_eq!(untokened(&fx), 1);
    let more = feed(&mut s, lists(&[], CONTROL_ONLY));
    assert_eq!(untokened(&more), 0);
    let all = [fx, more].concat();
    let status = writes_of(&all, "@moa_status");
    assert_eq!(
        status,
        vec!["set-option -g @moa_status '#[fg=#88c0d0]◐ 1  #[fg=#8c9ab3]● 1'"]
    );
    assert_eq!(untokened(&all), 1);

    // An input that changes nothing writes nothing.
    let fx = feed(&mut s, lists(&[], CONTROL_ONLY));
    assert_eq!(untokened(&fx), 0);

    // An op that archives a live session writes the new counts, after its Persist.
    let fx = handle_checked(
        &mut s,
        Input::Client {
            conn: ConnId(7),
            req: ReqId(1),
            caller: CallerInfo::default(),
            op: Op::SessionArchive {
                qs: vec![sid(1).to_string()],
            },
        },
        now(),
    );
    check_order(&fx).unwrap();
    assert_eq!(
        writes_of(&fx, "@moa_status"),
        vec!["set-option -g @moa_status '#[fg=#8c9ab3]● 1'"]
    );
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let view = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { token: None, .. }))
        .unwrap();
    assert!(persist < view);
}

#[test]
fn a_reconnect_writes_everything_again() {
    let mut s = started(vec![live(1, Activity::Working)], vec![binding(1, "%1")]);
    let panes = ["P %1 @1 $0 1 1 0 0 "];
    up_at(&mut s, &panes, USER, now());
    let fx = feed(&mut s, TmuxIn::Down(Down::Lost));
    assert_eq!(untokened(&fx), 0);
    let all = [connect(&mut s), feed(&mut s, lists(&panes, USER))].concat();
    assert_eq!(writes_of(&all, "@moa_status").len(), 1);
    assert_eq!(writes_of(&all, "@moa_clock").len(), 1);
    assert_eq!(
        writes_of(&all, "@moa_state"),
        vec!["set-option -w -t @1 @moa_state working"]
    );
}

#[test]
fn a_window_the_server_still_marks_is_unset_after_a_reset() {
    // A window with no trusted pane, found by the first lists: whatever an earlier run
    // left on it goes.
    let mut s = started(vec![live(1, Activity::Working)], vec![]);
    let fx = up_at(&mut s, &["P %7 @1 $0 1 1 0 0 "], CONTROL_ONLY, now());
    assert_eq!(
        writes_of(&fx, "@moa_state"),
        vec!["set-option -w -u -t @1 @moa_state"]
    );
    // Once.
    let fx = feed(&mut s, lists(&["P %7 @1 $0 1 1 0 0 "], CONTROL_ONLY));
    assert!(writes_of(&fx, "@moa_state").is_empty());

    // A window marked, then a reload after it lost its binding: the reload forgot what
    // was written, so it clears the mark again.
    let mut s = started(vec![live(1, Activity::Working)], vec![binding(1, "%1")]);
    let panes = ["P %1 @1 $0 1 1 0 0 "];
    let fx = up_at(&mut s, &panes, CONTROL_ONLY, now());
    assert_eq!(
        writes_of(&fx, "@moa_state"),
        vec!["set-option -w -t @1 @moa_state working"]
    );
    let fx = handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::new(Snapshot {
                sessions: vec![live(1, Activity::Working)],
                ..Snapshot::default()
            }),
            refuse_writes: None,
        },
        now(),
    );
    check_order(&fx).unwrap();
    assert_eq!(
        writes_of(&fx, "@moa_state"),
        vec!["set-option -w -u -t @1 @moa_state"]
    );
}

#[test]
fn only_counted_states_mark_a_window() {
    let mut s = started(
        vec![
            session(1, Activity::Idle, Lifecycle::Ended),
            Session {
                asleep_at: Some(UtcMs(3)),
                ..session(2, Activity::Idle, Lifecycle::Ended)
            },
            Session {
                archived_at: Some(UtcMs(3)),
                ..live(3, Activity::Idle)
            },
        ],
        vec![binding(1, "%1"), binding(2, "%2"), binding(3, "%3")],
    );
    let fx = up_at(
        &mut s,
        &[
            "P %1 @1 $0 1 1 0 0 ",
            "P %2 @2 $0 1 0 0 0 ",
            "P %3 @3 $0 1 0 0 0 ",
        ],
        CONTROL_ONLY,
        now(),
    );
    // No state is set; each window is only cleared of whatever it had.
    assert_eq!(
        writes_of(&fx, "@moa_state"),
        vec![
            "set-option -w -u -t @1 @moa_state",
            "set-option -w -u -t @2 @moa_state",
            "set-option -w -u -t @3 @moa_state",
        ]
    );
}

#[test]
fn a_clock_timer_that_fires_early_is_re_armed() {
    let mut s = started(vec![], vec![]);
    up_at(&mut s, &[], USER, now());
    // The wall clock stepped back: the timer fires while the text is still the same.
    let fx = handle_checked(
        &mut s,
        Input::Timer(TimerKey::Clock),
        utc("2026-11-15T14:05:40Z"),
    );
    assert_eq!(
        timers(&fx),
        vec![(TimerKey::Clock, ts("2026-11-15T14:06:00Z"))]
    );
}

#[test]
fn window_states_follow_trusted_panes_and_are_unset_when_gone() {
    // Two sessions share window @1; a third is alone in @2; a fourth's binding is for
    // a pane that doesn't exist; a fifth's binding is from another server run.
    let stale = PaneBinding {
        server_pid: 99,
        ..binding(5, "%5")
    };
    let mut s = started(
        vec![
            live(1, Activity::Working),
            live(2, Activity::NeedsPermission),
            done(3),
            live(4, Activity::Idle),
            live(5, Activity::NeedsPermission),
        ],
        vec![
            binding(1, "%1"),
            binding(2, "%2"),
            binding(3, "%3"),
            binding(4, "%9"),
            stale,
        ],
    );
    let panes = [
        "P %1 @1 $0 1 1 0 0 ",
        "P %2 @1 $0 0 1 0 0 ",
        "P %3 @2 $0 1 0 0 0 ",
        "P %5 @3 $0 1 0 0 0 ",
    ];
    let fx = up_at(&mut s, &panes, CONTROL_ONLY, now());
    check_order(&fx).unwrap();
    let w = writes_of(&fx, "@moa_state");
    assert_eq!(
        w,
        vec![
            "set-option -w -t @1 @moa_state waiting",
            "set-option -w -t @2 @moa_state done",
            // The stale binding is ignored; the window is only cleared.
            "set-option -w -u -t @3 @moa_state",
        ]
    );

    // Nothing changed: nothing written.
    let fx = feed(&mut s, lists(&panes, CONTROL_ONLY));
    assert_eq!(untokened(&fx), 0);

    // Pane %3 goes while its window stays (it gains another, unbound pane).
    let fx = feed(
        &mut s,
        lists(
            &[
                "P %1 @1 $0 1 1 0 0 ",
                "P %2 @1 $0 0 1 0 0 ",
                "P %7 @2 $0 1 0 0 0 ",
                "P %5 @3 $0 1 0 0 0 ",
            ],
            CONTROL_ONLY,
        ),
    );
    assert_eq!(
        writes_of(&fx, "@moa_state"),
        vec!["set-option -w -u -t @2 @moa_state"]
    );

    // The whole window goes: forgotten, nothing to unset.
    let fx = feed(
        &mut s,
        lists(
            &["P %1 @1 $0 1 1 0 0 ", "P %2 @1 $0 0 1 0 0 "],
            CONTROL_ONLY,
        ),
    );
    assert!(writes_of(&fx, "@moa_state").is_empty());
    // And a window that comes back unset isn't unset twice.
    let fx = feed(
        &mut s,
        lists(
            &[
                "P %1 @1 $0 1 1 0 0 ",
                "P %2 @1 $0 0 1 0 0 ",
                "P %7 @2 $0 1 0 0 0 ",
            ],
            CONTROL_ONLY,
        ),
    );
    assert!(writes_of(&fx, "@moa_state").is_empty());
}

#[test]
fn the_clock_runs_only_while_someone_is_attached() {
    let mut s = started(vec![], vec![]);
    // No users: no clock.
    let fx = up_at(&mut s, &[], CONTROL_ONLY, now());
    assert!(timers(&fx).iter().all(|(k, _)| *k != TimerKey::Clock));
    assert!(writes_of(&fx, "@moa_clock").is_empty());

    // A user attaches at 14:05:30.
    let fx = feed(&mut s, lists(&[], USER));
    check_order(&fx).unwrap();
    assert_eq!(
        writes_of(&fx, "@moa_clock"),
        vec!["set-option -g @moa_clock 14:05"]
    );
    assert_eq!(
        timers(&fx),
        vec![(TimerKey::Clock, ts("2026-11-15T14:06:00Z"))]
    );

    // The timer fires.
    let at = utc("2026-11-15T14:06:00.004Z");
    let fx = handle_checked(&mut s, Input::Timer(TimerKey::Clock), at);
    assert_eq!(
        writes_of(&fx, "@moa_clock"),
        vec!["set-option -g @moa_clock 14:06"]
    );
    assert_eq!(
        timers(&fx),
        vec![(TimerKey::Clock, ts("2026-11-15T14:07:00Z"))]
    );

    // Other inputs in the same minute leave it alone.
    let fx = feed_at(&mut s, lists(&[], USER), utc("2026-11-15T14:06:20Z"));
    assert!(timers(&fx).is_empty() && untokened(&fx) == 0);

    // The user detaches.
    let fx = feed_at(
        &mut s,
        lists(&[], CONTROL_ONLY),
        utc("2026-11-15T14:06:40Z"),
    );
    assert_eq!(fx, vec![Effect::CancelTimer(TimerKey::Clock)]);

    // A timer that was already on its way is answered with silence, not a re-arm.
    let fx = handle_checked(
        &mut s,
        Input::Timer(TimerKey::Clock),
        utc("2026-11-15T14:07:00Z"),
    );
    assert!(fx.is_empty());
}

#[test]
fn the_clock_stops_when_the_link_goes_down() {
    let mut s = started(vec![], vec![]);
    up_at(&mut s, &[], USER, now());
    let fx = feed(&mut s, TmuxIn::Down(Down::Lost));
    // The user's first attach had the cockpit built (L9): that goes with the link too.
    assert_eq!(
        fx,
        vec![
            Effect::CancelTimer(TimerKey::Clock),
            Effect::Log(LogLine {
                level: LogLevel::Info,
                message: "a cockpit change was dropped: moa lost its terminal".into(),
            }),
        ]
    );
    // A timer already on its way finds nobody to tell.
    let fx = handle_checked(
        &mut s,
        Input::Timer(TimerKey::Clock),
        utc("2026-11-15T14:06:00Z"),
    );
    assert!(fx.is_empty());
    let fx = feed(&mut s, TmuxIn::Down(Down::NoServer));
    assert!(fx.is_empty());
}

#[test]
fn an_idle_brain_with_nobody_attached_wakes_only_for_backups() {
    let mut s = started(vec![live(1, Activity::Idle)], vec![]);
    let mut all = up_at(&mut s, &[], CONTROL_ONLY, now());
    let mut at = now();
    for _ in 0..(24 * 60 / 5) {
        at = Now::at(
            UtcMs(at.wall.0 + 5 * 60_000),
            at.mono + Duration::from_secs(300),
            at.tz.clone(),
        );
        all.extend(feed_at(&mut s, lists(&[], CONTROL_ONLY), at.clone()));
        if at.wall.0 % (24 * 3_600_000) < 5 * 60_000 {
            all.extend(handle_checked(
                &mut s,
                Input::Timer(TimerKey::Backup),
                at.clone(),
            ));
        }
    }
    let keys: Vec<TimerKey> = timers(&all).into_iter().map(|(k, _)| k).collect();
    assert!(keys.iter().all(|k| *k == TimerKey::Backup), "{keys:?}");
}

#[test]
fn the_clock_follows_a_zone_change() {
    let mut s = started(vec![], vec![]);
    let at = ts("2026-11-15T17:05:30Z");
    let est = Now::at(
        at,
        Duration::from_secs(1),
        TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
    );
    let fx = up_at(&mut s, &[], USER, est);
    assert_eq!(
        writes_of(&fx, "@moa_clock"),
        vec!["set-option -g @moa_clock 12:05"]
    );

    let pst = Now::at(
        at,
        Duration::from_secs(2),
        TimeZone::posix("PST8PDT,M3.2.0,M11.1.0").unwrap(),
    );
    assert_eq!(clock_text(&pst), "09:05");
    let fx = handle_checked(&mut s, Input::ZoneChanged, pst);
    check_order(&fx).unwrap();
    assert_eq!(
        writes_of(&fx, "@moa_clock"),
        vec!["set-option -g @moa_clock 09:05"]
    );
    assert!(timers(&fx).contains(&(TimerKey::Clock, ts("2026-11-15T17:06:00Z"))));
}

/// What the status line starts with while moa's agent waits for your answer.
const NEEDS_YOU: &str = "#[fg=#ebcb8b,bold]⚠ moa needs you#[nobold]";

#[test]
fn moa_needs_you_without_a_console() {
    use common::central::{first_turn, moa};
    use moa_core::central::wire::{AcpIn, PermKind, PermOption};
    use moa_core::op::PermAnswer;
    use moa_core::panel::View;
    use moa_testkit::brain::TypicalBrain;

    let ask = |req| AcpIn::Permission {
        generation: 1,
        req,
        thread: "th-1".into(),
        title: "Run git push".into(),
        options: vec![PermOption {
            id: "no".into(),
            kind: PermKind::RejectOnce,
            name: "No".into(),
        }],
    };
    let answer = |st: &mut State, b: &TypicalBrain, req| {
        handle_checked(
            st,
            Input::Client {
                conn: ConnId(1),
                req: ReqId(req),
                caller: CallerInfo::default(),
                op: Op::ConsolePermission {
                    id: None,
                    answer: PermAnswer::No,
                },
            },
            b.now(),
        )
    };
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    common::link_up(&b, &mut st);
    first_turn(&mut st, &b.now(), "th-1", false);
    let counts = counts_text(&st);
    assert!(!counts.is_empty());

    let fx = handle_checked(&mut st, Input::Acp(ask(3)), b.now());
    assert_eq!(
        writes_of(&fx, "@moa_status"),
        [format!("set-option -g @moa_status '{NEEDS_YOU}  {counts}'")]
    );
    let fx = answer(&mut st, &b, 10);
    assert_eq!(
        writes_of(&fx, "@moa_status"),
        [format!("set-option -g @moa_status '{counts}'")]
    );

    // With a console open, the console shows it instead.
    let panel = Op::UiPanel {
        pane: None,
        view: Some(View::Console),
        placed: false,
        once: false,
    };
    common::send_on(&b, &mut st, 2, 11, panel);
    let fx = handle_checked(&mut st, Input::Acp(ask(4)), b.now());
    assert!(writes_of(&fx, "@moa_status").is_empty(), "{fx:?}");
    assert_eq!(counts_text(&st), counts);
}

#[test]
fn next_minute_is_strictly_after() {
    assert_eq!(
        next_minute(ts("2026-11-15T14:05:30Z")),
        ts("2026-11-15T14:06:00Z")
    );
    assert_eq!(
        next_minute(ts("2026-11-15T14:06:00Z")),
        ts("2026-11-15T14:07:00Z")
    );
    assert_eq!(next_minute(UtcMs(-1)), UtcMs(0));
}
