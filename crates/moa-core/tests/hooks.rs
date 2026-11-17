//! Hook events in the core: what each kind does to a session, in what order, and what is
//! written down. Every input goes through `handle_checked`.

use std::time::Duration;

use jiff::civil::date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, LogLevel, handle_checked, reply_of};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Conversation, ConversationReason, Lifecycle, Location,
    Project, ProjectId, ProjectStatus, Session, SessionId,
};
use moa_core::op::{ConnId, ReqId};
use moa_core::state::{Snapshot, State};
use moa_core::{Now, UtcMs};
use proptest::prelude::*;
use serde_json::json;

const S: &str = "0195f3a2-1111-7000-8000-000000000001";
const C: &str = "a1b2c3d4-0000-4000-8000-000000000001";
const T0: i64 = 1_800_000_000_000;

fn now() -> Now {
    Now::at(
        UtcMs(T0 + 60_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn sid() -> SessionId {
    S.parse().unwrap()
}

fn seeded() -> State {
    let session = Session {
        id: sid(),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some("/work/Lantern".into()),
        start_cwd: Some("/work/Lantern".into()),
        project_id: Some(ProjectId(1)),
        title: None,
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(T0)),
        activity_why: None,
        last_prompt: Some("earlier".into()),
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
    let project = Project {
        id: ProjectId(1),
        name: "Lantern".into(),
        path: None,
        status: ProjectStatus::Active,
        summary: None,
        last_activity_on: Some(date(2000, 1, 1)),
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
    };
    let mut st = State::new(BrainName::default_brain(), Config::default());
    handle_checked(
        &mut st,
        Input::Startup(Box::new(Snapshot {
            sessions: vec![session],
            conversations: vec![conversation],
            projects: vec![project],
            ..Snapshot::default()
        })),
        now(),
    );
    st
}

fn envelope(seq: u32, plus_ms: i64, kind: HookKind) -> HookEnvelope {
    HookEnvelope {
        agent: AgentKind::Claude,
        slot: Some(S.into()),
        brain: Some("default".into()),
        store_dir: None,
        conversation_id: C.into(),
        kind,
        at: UtcMs(T0 + plus_ms),
        at_ns: (T0 + plus_ms) * 1_000_000,
        pid: 100,
        seq,
        cwd: None,
        transcript_path: None,
    }
}

fn feed(st: &mut State, env: HookEnvelope) -> Vec<Effect> {
    feed_replying(st, env, None)
}

fn feed_replying(
    st: &mut State,
    envelope: HookEnvelope,
    reply: Option<(ConnId, ReqId)>,
) -> Vec<Effect> {
    handle_checked(st, Input::Hook { reply, envelope }, now())
}

fn session(st: &State) -> Session {
    st.session(sid()).unwrap().clone()
}

fn shown(st: &State) -> (DisplayState, String) {
    display_state(&session(st), now().wall)
}

fn changes(fx: &[Effect]) -> Vec<&Change> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .flatten()
        .collect()
}

fn persists(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::Persist(_)))
        .count()
}

fn event_kinds(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Event(e) => Some(e.kind.0.clone()),
            _ => None,
        })
        .collect()
}

fn activity_texts(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

fn logs(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::Log(_))).count()
}

fn prompt(text: &str) -> HookKind {
    HookKind::TurnStarted {
        prompt: text.into(),
    }
}

fn ended(text: &str) -> HookKind {
    HookKind::TurnEnded {
        last_message: text.into(),
    }
}

#[test]
fn a_prompt_makes_it_working() {
    let mut st = seeded();
    let fx = feed(&mut st, envelope(1, 1000, prompt("Fix the lamp")));
    assert_eq!(shown(&st), (DisplayState::Working, "Fix the lamp".into()));
    assert_eq!(session(&st).last_prompt.as_deref(), Some("Fix the lamp"));
    assert_eq!(persists(&fx), 1);
    let cs = changes(&fx);
    assert!(matches!(cs[0], Change::Session(_)));
    assert_eq!(event_kinds(&fx), ["turn.started"]);
    assert_eq!(activity_texts(&fx), ["Lantern › Fix the lamp"]);
    let Some(Change::Activity(a)) = cs.iter().find(|c| matches!(c, Change::Activity(_))) else {
        panic!("no activity row");
    };
    assert_eq!(a.who, ActivityWho::Session);
    assert_eq!(a.ref_kind.as_deref(), Some("session"));
    assert_eq!(a.ref_id.as_deref(), Some(S));
}

#[test]
fn a_long_prompt_is_one_line_of_ninety_characters_in_the_feed() {
    let mut st = seeded();
    let long = format!("first\nsecond {}", "é".repeat(200));
    let fx = feed(&mut st, envelope(1, 1000, prompt(&long)));
    let text = &activity_texts(&fx)[0];
    let want: String = long.replace('\n', " ").chars().take(90).collect();
    assert_eq!(text, &format!("Lantern › {want}"));
}

#[test]
fn stop_makes_it_done_until_seen() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, ended("All done.")));
    assert_eq!(shown(&st).0, DisplayState::Done);
    assert_eq!(session(&st).last_turn_end_at, Some(UtcMs(T0 + 1000)));
    let mut s = session(&st);
    s.seen_at = Some(UtcMs(T0 + 2000));
    assert_eq!(display_state(&s, now().wall).0, DisplayState::Idle);
}

#[test]
fn a_reply_ending_in_a_question_asks_you() {
    let mut st = seeded();
    let fx = feed(&mut st, envelope(1, 1000, ended("Looks fine, ship it?")));
    assert_eq!(shown(&st).0, DisplayState::Question);
    assert_eq!(activity_texts(&fx), ["Lantern ✓ turn done"]);
}

#[test]
fn permission_waits_and_post_tool_clears_it() {
    let mut st = seeded();
    let fx = feed(
        &mut st,
        envelope(
            1,
            1000,
            HookKind::NeedsPermission {
                tool: "Bash".into(),
            },
        ),
    );
    assert_eq!(
        shown(&st),
        (DisplayState::Waiting, "permission: Bash".into())
    );
    assert_eq!(activity_texts(&fx), ["Lantern ⚠ needs you"]);
    let fx = feed(&mut st, envelope(2, 2000, HookKind::NeedsCleared));
    assert_eq!(shown(&st).0, DisplayState::Working);
    assert_eq!(session(&st).activity_why.as_deref(), Some("earlier"));
    assert!(event_kinds(&fx).is_empty());
    assert!(activity_texts(&fx).is_empty());
    assert_eq!(persists(&fx), 1);
}

#[test]
fn a_clear_with_nothing_to_clear_only_moves_the_times() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, ended("Done.")));
    feed(&mut st, envelope(2, 2000, HookKind::NeedsCleared));
    let s = session(&st);
    assert_eq!(s.activity, Activity::Idle);
    assert_eq!(s.activity_at, Some(UtcMs(T0 + 2000)));
    assert_eq!(s.activity_why.as_deref(), Some("Done."));
}

#[test]
fn stop_failure_is_failed() {
    let mut st = seeded();
    let fx = feed(
        &mut st,
        envelope(
            1,
            1000,
            HookKind::TurnFailed {
                error: "rate limit".into(),
            },
        ),
    );
    assert_eq!(shown(&st), (DisplayState::Failed, "rate limit".into()));
    assert_eq!(activity_texts(&fx), ["Lantern ✗ failed"]);
}

#[test]
fn a_question_tool_asks_you() {
    let mut st = seeded();
    feed(
        &mut st,
        envelope(
            1,
            1000,
            HookKind::NeedsAnswer {
                what: "Which lamp?".into(),
            },
        ),
    );
    assert_eq!(shown(&st), (DisplayState::Question, "Which lamp?".into()));
}

#[test]
fn a_cwd_change_updates_the_display_only() {
    let mut st = seeded();
    let fx = feed(
        &mut st,
        envelope(
            1,
            1000,
            HookKind::CwdChanged {
                cwd: "/work/Harbour".into(),
            },
        ),
    );
    assert_eq!(session(&st).cwd.as_deref(), Some("/work/Harbour"));
    assert_eq!(session(&st).start_cwd.as_deref(), Some("/work/Lantern"));
    assert_eq!(session(&st).project_id, Some(ProjectId(1)));
    assert!(event_kinds(&fx).is_empty());
}

#[test]
fn an_older_event_only_adds_history() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 5000, ended("Done.")));
    let before = session(&st);
    let fx = feed(&mut st, envelope(2, 4000, HookKind::NeedsCleared));
    assert_eq!(session(&st), before);
    assert!(activity_texts(&fx).is_empty());
    let fx = feed(&mut st, envelope(3, 4000, prompt("late")));
    assert_eq!(session(&st), before);
    assert_eq!(event_kinds(&fx), ["turn.started"]);
    assert!(activity_texts(&fx).is_empty());
    assert!(
        !changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Session(_) | Change::Project(_)))
    );
}

#[test]
fn equal_times_apply_in_arrival_order() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, prompt("one")));
    feed(&mut st, envelope(2, 1000, ended("two")));
    assert_eq!(session(&st).activity, Activity::Idle);
    assert_eq!(session(&st).activity_why.as_deref(), Some("two"));
}

#[test]
fn a_duplicate_changes_nothing() {
    let mut st = seeded();
    let e = envelope(1, 1000, prompt("Fix the lamp"));
    assert_eq!(persists(&feed(&mut st, e.clone())), 1);
    let before = session(&st);
    let fx = feed(&mut st, e);
    assert_eq!(persists(&fx), 0);
    assert_eq!(session(&st), before);
}

#[test]
fn events_for_other_brains_and_moa_are_dropped() {
    let mut st = seeded();
    let mut other = envelope(1, 1000, prompt("x"));
    other.brain = Some("work".into());
    let fx = feed(&mut st, other);
    assert_eq!((persists(&fx), logs(&fx)), (0, 1));

    let mut moa = envelope(2, 1000, prompt("x"));
    moa.slot = Some("moa".into());
    let fx = feed(&mut st, moa);
    assert_eq!((persists(&fx), logs(&fx)), (0, 0));

    let mut unknown = envelope(3, 1000, prompt("x"));
    unknown.slot = Some("not-a-uuid".into());
    unknown.conversation_id = "unknown-conv".into();
    let fx = feed(&mut st, unknown);
    assert_eq!((persists(&fx), logs(&fx)), (0, 1));
    for e in &fx {
        if let Effect::Log(l) = e {
            assert_eq!(l.level, LogLevel::Info);
            assert!(!l.message.contains("unknown-conv"), "{}", l.message);
        }
    }
    assert_eq!(session(&st).activity, Activity::Idle);
}

#[test]
fn the_conversation_id_finds_a_session_without_a_slot() {
    let mut st = seeded();
    let mut e = envelope(1, 1000, prompt("Fix the lamp"));
    e.slot = None;
    feed(&mut st, e);
    assert_eq!(shown(&st).0, DisplayState::Working);
}

#[test]
fn an_event_bumps_the_projects_last_activity_once_a_day() {
    let mut st = seeded();
    let fx = feed(&mut st, envelope(1, 1000, prompt("one")));
    let bumps: Vec<_> = changes(&fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Project(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(bumps.len(), 1);
    assert_eq!(bumps[0].last_activity_on, Some(now().local_day));
    assert_eq!(bumps[0].updated_at, UtcMs(1));
    let fx = feed(&mut st, envelope(2, 2000, ended("two")));
    assert!(!changes(&fx).iter().any(|c| matches!(c, Change::Project(_))));
}

#[test]
fn a_session_start_request_is_answered_after_the_commit() {
    let mut st = seeded();
    let fx = feed_replying(
        &mut st,
        envelope(1, 1000, prompt("x")),
        Some((ConnId(3), ReqId(9))),
    );
    let done = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(done.data, json!({"context": null}));
    let at = |f: fn(&Effect) -> bool| fx.iter().position(f).unwrap();
    assert!(at(|e| matches!(e, Effect::Persist(_))) < at(|e| matches!(e, Effect::Reply { .. })));
}

#[test]
fn a_dropped_request_is_still_answered() {
    let mut st = seeded();
    let mut moa = envelope(1, 1000, prompt("x"));
    moa.slot = Some("moa".into());
    let fx = feed_replying(&mut st, moa, Some((ConnId(3), ReqId(9))));
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap().data,
        json!({"context": null})
    );
    assert_eq!(persists(&fx), 0);
}

#[test]
fn start_and_end_get_history_and_times_only() {
    let mut st = seeded();
    let fx = feed(
        &mut st,
        envelope(
            1,
            1000,
            HookKind::SessionEnded {
                reason: "logout".into(),
            },
        ),
    );
    assert_eq!(event_kinds(&fx), ["session.ended"]);
    assert_eq!(session(&st).activity_at, Some(UtcMs(T0 + 1000)));
    assert_eq!(session(&st).lifecycle, Lifecycle::Live);
}

fn kind_strategy() -> impl Strategy<Value = HookKind> {
    prop_oneof![
        "[a-z]{1,8}".prop_map(|p| HookKind::TurnStarted { prompt: p }),
        "[a-z]{1,8}".prop_map(|m| HookKind::TurnEnded { last_message: m }),
        "[a-z]{1,8}".prop_map(|e| HookKind::TurnFailed { error: e }),
        "[a-z]{1,8}".prop_map(|t| HookKind::NeedsPermission { tool: t }),
        "[a-z]{1,8}".prop_map(|w| HookKind::NeedsAnswer { what: w }),
    ]
}

/// Envelopes with distinct times 1s apart, in time order, and the same in any other order.
fn envelopes_and_a_shuffle() -> impl Strategy<Value = (Vec<HookEnvelope>, Vec<HookEnvelope>)> {
    proptest::collection::vec(kind_strategy(), 1..6).prop_flat_map(|kinds| {
        let envs: Vec<HookEnvelope> = kinds
            .into_iter()
            .enumerate()
            .map(|(i, k)| envelope(i as u32, 1000 * (i as i64 + 1), k))
            .collect();
        (Just(envs.clone()), Just(envs).prop_shuffle())
    })
}

proptest! {
    #[test]
    fn no_order_of_events_regresses_state((envs, shuffled) in envelopes_and_a_shuffle()) {
        let newest = envs.last().unwrap().clone();
        let mut alone = seeded();
        feed(&mut alone, newest.clone());
        let mut st = seeded();
        for e in shuffled {
            feed(&mut st, e);
        }
        prop_assert_eq!(session(&st).activity, session(&alone).activity);
        prop_assert_eq!(session(&st).activity_why, session(&alone).activity_why);
        prop_assert_eq!(session(&st).activity_at, Some(newest.at));
    }
}
