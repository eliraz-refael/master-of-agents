//! The session reports queue in the core: which hook events queue a report, and what the row
//! holds. Every input goes through `handle_checked`.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::central::reports::{LAST_MAX, TURNS_KEPT};
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Report, ReportKind,
    Session, SessionId,
};
use moa_core::publish::Topic;
use moa_core::state::{Deps, Snapshot, State};
use moa_core::{Now, UtcMs};

const S: &str = "0195f3a2-1111-7000-8000-000000000001";
const C: &str = "a1b2c3d4-0000-4000-8000-000000000001";
const T0: i64 = 1_800_000_000_000;

fn now() -> Now {
    Now::at(
        UtcMs(T0 + 600_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn sid() -> SessionId {
    S.parse().unwrap()
}

/// Lantern's session in `cwd`, idle since `T0`, started with `deps`.
fn started(cwd: &str, deps: Deps) -> State {
    let session = Session {
        id: sid(),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.into()),
        start_cwd: Some(cwd.into()),
        project_id: None,
        title: None,
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(T0)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(UtcMs(T0)),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(T0)),
    };
    let conversation = Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: C.into(),
        session_id: sid(),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    };
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle_checked(
        &mut st,
        Input::Startup(Box::new(Snapshot {
            sessions: vec![session],
            conversations: vec![conversation],
            ..Snapshot::default()
        })),
        now(),
    );
    st
}

fn lantern() -> State {
    started("/work/Lantern", Deps::default())
}

fn hook(seq: u32, plus_ms: i64, kind: HookKind) -> HookEnvelope {
    common::envelope(S, C, UtcMs(T0 + plus_ms), seq, kind)
}

fn turn(seq: u32, plus_ms: i64, text: &str) -> HookEnvelope {
    hook(
        seq,
        plus_ms,
        HookKind::TurnEnded {
            last_message: text.into(),
        },
    )
}

fn feed(st: &mut State, envelope: HookEnvelope) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Hook {
            reply: None,
            envelope,
        },
        now(),
    )
}

/// The reports `fx` saves.
fn saved(fx: &[Effect]) -> Vec<Report> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Report(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn waiting(st: &State) -> Vec<Report> {
    st.reports().cloned().collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_finished_turn_queues_a_report() {
    let mut st = lantern();
    let env = turn(1, 1_000, "Merged PR #3");
    let fx = feed(&mut st, env.clone());
    // One commit holds the session and its report.
    let persists: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(persists.len(), 1);
    assert!(matches!(persists[0][0], Change::Session(_)));
    let reports = saved(&fx);
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!((r.session_id, r.kind), (sid(), ReportKind::TurnEnded));
    assert_eq!(r.turns, strs(&["Merged PR #3"]));
    assert_eq!((r.queued_at, r.last_at), (env.at, env.at));
    assert_eq!(waiting(&st), reports);
}

#[test]
fn six_turns_are_kept() {
    let mut st = lantern();
    for n in 1..=8u32 {
        feed(&mut st, turn(n, 1_000 * i64::from(n), &format!("turn {n}")));
    }
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(TURNS_KEPT, 6);
    assert_eq!(
        rows[0].turns,
        strs(&["turn 3", "turn 4", "turn 5", "turn 6", "turn 7", "turn 8"])
    );
    assert_eq!(rows[0].queued_at, UtcMs(T0 + 1_000));
    assert_eq!(rows[0].last_at, UtcMs(T0 + 8_000));
}

#[test]
fn needs_you_keeps_the_latest() {
    let mut st = lantern();
    feed(
        &mut st,
        hook(
            1,
            1_000,
            HookKind::NeedsPermission {
                tool: "Bash".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows[0].kind, ReportKind::NeedsYou);
    assert_eq!(rows[0].turns, strs(&["permission: Bash"]));
    feed(
        &mut st,
        hook(
            2,
            2_000,
            HookKind::NeedsAnswer {
                what: "Which branch?".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turns, strs(&["Which branch?"]));
    assert_eq!(rows[0].queued_at, UtcMs(T0 + 1_000));
    assert_eq!(rows[0].last_at, UtcMs(T0 + 2_000));
}

#[test]
fn a_turn_and_a_question_queue_apart() {
    let mut st = lantern();
    feed(&mut st, turn(1, 1_000, "Done"));
    feed(
        &mut st,
        hook(
            2,
            2_000,
            HookKind::NeedsAnswer {
                what: "Which branch?".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        [ReportKind::TurnEnded, ReportKind::NeedsYou]
    );
    assert_ne!(rows[0].id, rows[1].id);
    assert!(rows.iter().all(|r| r.session_id == sid()));
}

#[test]
fn a_long_turn_is_cut_to_2600() {
    let mut st = lantern();
    assert_eq!(LAST_MAX, 2600);
    feed(&mut st, turn(1, 1_000, &"é".repeat(5_000)));
    assert_eq!(waiting(&st)[0].turns[0].chars().count(), 2600);
    feed(
        &mut st,
        hook(
            2,
            2_000,
            HookKind::NeedsAnswer {
                what: "x".repeat(5_000),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows[1].turns[0].chars().count(), 2600);
}

#[test]
fn not_every_event_is_a_report() {
    // An empty (or blank) last message says nothing.
    let mut st = lantern();
    let fx = feed(&mut st, turn(1, 1_000, "  \n "));
    assert!(saved(&fx).is_empty());
    // A duplicate hook is applied once.
    let fx = feed(&mut st, turn(2, 2_000, "Done"));
    assert_eq!(saved(&fx).len(), 1);
    let fx = feed(&mut st, turn(2, 2_000, "Done"));
    assert!(saved(&fx).is_empty());
    assert_eq!(waiting(&st)[0].turns, strs(&["Done"]));
    // An event older than the session's last activity is history only.
    let mut st = lantern();
    let fx = feed(&mut st, turn(1, -1_000, "Old news"));
    assert!(saved(&fx).is_empty());
    assert!(waiting(&st).is_empty());
    // Starting, a failure and a cleared need say nothing to moa either.
    for (seq, kind) in [
        (
            2,
            HookKind::TurnFailed {
                error: "boom".into(),
            },
        ),
        (3, HookKind::NeedsCleared),
        (
            4,
            HookKind::TurnStarted {
                prompt: "go".into(),
            },
        ),
    ] {
        assert!(saved(&feed(&mut st, hook(seq, 1_000 * i64::from(seq), kind))).is_empty());
    }
}

#[test]
fn moas_own_folder_queues_nothing() {
    let deps = || Deps {
        agent_dir: PathBuf::from("/work/moa-agent"),
        ..Deps::default()
    };
    for cwd in ["/work/moa-agent", "/work/moa-agent/notes"] {
        let mut st = started(cwd, deps());
        let fx = feed(&mut st, turn(1, 1_000, "Done"));
        assert!(saved(&fx).is_empty(), "{cwd}");
    }
    // A sibling whose name starts the same isn't under it.
    let mut st = started("/work/moa-agent-2", deps());
    assert_eq!(saved(&feed(&mut st, turn(1, 1_000, "Done"))).len(), 1);
}

#[test]
fn the_state_row_counts_them() {
    let mut st = lantern();
    let queued = |st: &State| st.model().topics[&Topic::Feed]["state"]["queued"].clone();
    assert_eq!(queued(&st), 0);
    feed(&mut st, turn(1, 1_000, "Done"));
    feed(&mut st, turn(2, 2_000, "Again"));
    assert_eq!(queued(&st), 1);
    feed(
        &mut st,
        hook(
            3,
            3_000,
            HookKind::NeedsPermission {
                tool: "Bash".into(),
            },
        ),
    );
    // Rows, not sessions.
    assert_eq!(queued(&st), 2);
}

#[test]
fn reports_come_in_line_order() {
    // Spooled hooks from several sessions arrive out of order: ids don't follow `queued_at`.
    let report = |id, queued_at| Report {
        id,
        session_id: sid(),
        kind: ReportKind::TurnEnded,
        queued_at: UtcMs(queued_at),
        last_at: UtcMs(queued_at),
        turns: vec![format!("report {id}")],
    };
    let mut st = lantern();
    handle_checked(
        &mut st,
        Input::Startup(Box::new(Snapshot {
            reports: vec![report(1, 5_000), report(2, 3_000), report(3, 3_000)],
            ..Snapshot::default()
        })),
        now(),
    );
    assert_eq!(
        waiting(&st).iter().map(|r| r.id).collect::<Vec<_>>(),
        [2, 3, 1]
    );
}
