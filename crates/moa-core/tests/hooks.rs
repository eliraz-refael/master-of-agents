//! Hook events in the core: what each kind does to a session, in what order, and what is
//! written down. Every input goes through `handle_checked`.

use std::time::Duration;

use jiff::civil::date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Effect, Input, LogLevel, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked, reply_of,
};
use moa_core::hook::{HookEnvelope, HookKind, StartSource};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Conversation, ConversationReason, Lifecycle, Location,
    PaneBinding, Project, ProjectId, ProjectStatus, Session, SessionId,
};
use moa_core::op::{ConnId, ReqId};
use moa_core::state::{Snapshot, State};
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::status::counts_text;
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
    seeded_with(Vec::new())
}

fn seeded_with(pane_bindings: Vec<PaneBinding>) -> State {
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
            pane_bindings,
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
        started(1, 1000, StartSource::Resume),
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

fn started(seq: u32, plus_ms: i64, source: StartSource) -> HookEnvelope {
    envelope(seq, plus_ms, HookKind::SessionStarted { source })
}

fn ends(seq: u32, plus_ms: i64, reason: &str) -> HookEnvelope {
    envelope(
        seq,
        plus_ms,
        HookKind::SessionEnded {
            reason: reason.into(),
        },
    )
}

fn conversations(st: &State) -> Vec<Conversation> {
    let mut cs: Vec<Conversation> = st.conversations_of(sid()).cloned().collect();
    cs.sort_by_key(|c| c.first_seen_at);
    cs
}

/// Reloads the state with the session edited, as if it had been saved that way.
fn reseed(st: &mut State, refuse_writes: Option<RefuseWrites>, edit: impl FnOnce(&mut Session)) {
    let mut s = session(st);
    edit(&mut s);
    let snapshot = Snapshot {
        sessions: vec![s],
        conversations: conversations(st),
        ..Snapshot::default()
    };
    handle_checked(
        st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes,
        },
        now(),
    );
}

#[test]
fn start_brings_an_ended_session_back_and_clears_asleep() {
    let mut st = seeded();
    reseed(&mut st, None, |s| {
        s.lifecycle = Lifecycle::Ended;
        s.asleep_at = Some(UtcMs(T0 + 500));
        s.resume_failed = true;
        s.location = Location::Elsewhere;
        s.activity = Activity::Failed;
    });
    assert_eq!(shown(&st).0, DisplayState::Asleep);
    let fx = feed(&mut st, started(1, 1000, StartSource::Resume));
    let s = session(&st);
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);
    assert_eq!(s.location, Location::Here);
    assert_eq!(s.activity, Activity::Idle);
    assert!(!s.resume_failed);
    assert_eq!(event_kinds(&fx), ["session.started"]);
    assert_eq!(shown(&st).0, DisplayState::Idle);
}

#[test]
fn clear_rotates_the_conversation_and_stays_live() {
    let mut st = seeded();
    feed(&mut st, ends(1, 1000, "clear"));
    assert_eq!(session(&st).lifecycle, Lifecycle::Live);
    let mut start = started(2, 1500, StartSource::Clear);
    start.conversation_id = "b2c3d4e5-0000-4000-8000-000000000002".into();
    start.store_dir = Some("/stores/a".into());
    start.transcript_path = Some("/stores/a/t.jsonl".into());
    feed(&mut st, start);
    let cs = conversations(&st);
    assert_eq!(cs.len(), 2);
    assert_eq!(cs[1].reason, ConversationReason::Clear);
    assert_eq!(cs[1].store_dir.as_deref(), Some("/stores/a"));
    assert_eq!(cs[1].transcript_path.as_deref(), Some("/stores/a/t.jsonl"));
    assert_eq!(cs[1].first_seen_at, UtcMs(T0 + 1500));
    assert_eq!(cs[1].last_seen_at, UtcMs(T0 + 1500));
    assert_eq!(session(&st).lifecycle, Lifecycle::Live);
    // A conversation already known only moves its last_seen_at.
    feed(&mut st, started(3, 2500, StartSource::Resume));
    let again = conversations(&st);
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].last_seen_at, UtcMs(T0 + 2500));
    assert_eq!(again[1].last_seen_at, UtcMs(T0 + 1500));
}

#[test]
fn fork_and_compact_reasons() {
    for (source, reason) in [
        (StartSource::Fork, ConversationReason::Fork),
        (StartSource::Compact, ConversationReason::Clear),
    ] {
        let mut st = seeded();
        let mut start = started(1, 1000, source);
        start.conversation_id = "b2c3d4e5-0000-4000-8000-000000000002".into();
        feed(&mut st, start);
        assert_eq!(conversations(&st)[1].reason, reason);
    }
}

#[test]
fn exit_ends_the_session() {
    let mut st = seeded();
    let fx = feed(&mut st, ends(1, 1000, "prompt_input_exit"));
    assert_eq!(session(&st).lifecycle, Lifecycle::Ended);
    assert_eq!(event_kinds(&fx), ["session.ended"]);
    assert_eq!(activity_texts(&fx), ["Lantern ○ ended"]);
    assert_eq!(shown(&st).0, DisplayState::Ended);
}

#[test]
fn a_late_session_end_does_not_end_a_restarted_session() {
    let mut st = seeded();
    feed(&mut st, started(1, 2000, StartSource::Startup));
    let fx = feed(&mut st, ends(2, 1000, "logout"));
    assert_eq!(session(&st).lifecycle, Lifecycle::Live);
    assert_eq!(event_kinds(&fx), ["session.ended"]);
    assert!(activity_texts(&fx).is_empty());
}

fn at(plus_ms: i64) -> Now {
    Now::at(
        UtcMs(T0 + plus_ms),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn quiet_timers(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SetTimer {
                    key: TimerKey::Quiet,
                    ..
                } | Effect::CancelTimer(TimerKey::Quiet)
            )
        })
        .collect()
}

fn quiet_at(plus_ms: i64) -> Effect {
    Effect::SetTimer {
        key: TimerKey::Quiet,
        at: UtcMs(T0 + plus_ms),
    }
}

const TEN_MIN: i64 = 600_000;

#[test]
fn quiet_after_ten_minutes_of_silence() {
    let mut st = seeded();
    let fx = feed(&mut st, envelope(1, 1000, prompt("Fix the lamp")));
    assert_eq!(quiet_timers(&fx), [&quiet_at(1000 + TEN_MIN)]);
    let s = session(&st);
    let shown_at = |plus| display_state(&s, at(plus).wall);
    assert_eq!(shown_at(1000 + TEN_MIN - 1).0, DisplayState::Working);
    assert_eq!(
        shown_at(1000 + TEN_MIN),
        (DisplayState::Quiet, "quiet 10m".into())
    );
    assert_eq!(DisplayState::Quiet.word(), "quiet");
    assert_eq!(DisplayState::Quiet.glyph(), "◌");
}

#[test]
fn the_quiet_timer_firing_publishes_and_has_nothing_left_to_wait_for() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, prompt("a")));
    let fx = handle_checked(&mut st, Input::Timer(TimerKey::Quiet), at(1000 + TEN_MIN));
    assert_eq!(quiet_timers(&fx), [&Effect::CancelTimer(TimerKey::Quiet)]);
    assert!(fx.iter().any(|e| matches!(e, Effect::Publish(_))));
}

#[test]
fn the_quiet_timer_follows_the_earliest_working_session() {
    let mut st = seeded();
    let mut other = session(&st);
    other.id = "0195f3a2-1111-7000-8000-000000000002".parse().unwrap();
    other.activity = Activity::Working;
    other.activity_at = Some(UtcMs(T0 + 5000));
    let snapshot = Snapshot {
        sessions: vec![session(&st), other],
        conversations: conversations(&st),
        ..Snapshot::default()
    };
    handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: None,
        },
        now(),
    );
    let fx = feed(&mut st, envelope(1, 9000, prompt("later")));
    assert_eq!(quiet_timers(&fx), [&quiet_at(5000 + TEN_MIN)]);
    // Ending the later turn leaves the earlier one waiting.
    let fx = feed(&mut st, envelope(2, 10_000, ended("done")));
    assert_eq!(quiet_timers(&fx), [&quiet_at(5000 + TEN_MIN)]);
}

#[test]
fn the_quiet_timer_is_cancelled_when_none_is_working() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, prompt("a")));
    let fx = feed(&mut st, envelope(2, 2000, ended("b")));
    assert_eq!(quiet_timers(&fx), [&Effect::CancelTimer(TimerKey::Quiet)]);
}

#[test]
fn quiet_is_not_counted_in_the_status_line() {
    let mut st = seeded();
    feed(&mut st, envelope(1, 1000, prompt("a")));
    let later = at(1000 + TEN_MIN);
    let data = st.status(&Default::default(), later.wall);
    for word in ["working", "waiting", "question", "failed", "done"] {
        assert_eq!(data.sessions[word], 0, "{word}");
    }
    assert_eq!(data.sessions["quiet"], 1);
    handle_checked(&mut st, Input::Timer(TimerKey::Quiet), later);
    assert_eq!(counts_text(&st), "");
}

fn pane_binding(pid: i64) -> PaneBinding {
    PaneBinding {
        session_id: sid(),
        server_pid: pid,
        server_start: 1000,
        socket: "sock".into(),
        pane_id: "%5".into(),
        pane_pid: 99,
        bound_at: UtcMs(1),
    }
}

/// The link up on the server generation `10`/`1000`, with `%5` in a window.
fn link_up(st: &mut State) {
    let connected = TmuxIn::Connected {
        conn: 1,
        generation: Generation {
            pid: 10,
            start: 1000,
        },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
    };
    handle_checked(st, Input::Tmux(connected), now());
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    let lists = TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: vec![
            lines(&["S $0 moa", "S $1 _moa"]),
            lines(&["P %5 @1 $0 1 1 0 "]),
            lines(&["C 0 $0 /dev/ttys1"]),
        ],
    };
    handle_checked(st, Input::Tmux(lists), now());
}

fn pane_message(word: &str) -> Input {
    Input::Tmux(TmuxIn::Notification(Notification::Message(format!(
        "moa:hook {word} %5"
    ))))
}

#[test]
fn a_pane_exit_ends_its_bound_session() {
    for word in ["pane-exited", "pane-died"] {
        let mut st = seeded_with(vec![pane_binding(10)]);
        link_up(&mut st);
        let fx = handle_checked(&mut st, pane_message(word), now());
        assert_eq!(session(&st).lifecycle, Lifecycle::Ended);
        assert_eq!(activity_texts(&fx), ["Lantern ○ ended"]);
        assert!(
            changes(&fx)
                .iter()
                .any(|c| matches!(c, Change::DeletePaneBinding(_)))
        );
    }
}

#[test]
fn a_pane_exit_from_an_old_generation_changes_nothing() {
    let mut st = seeded_with(vec![pane_binding(9)]);
    link_up(&mut st);
    let fx = handle_checked(&mut st, pane_message("pane-exited"), now());
    assert_eq!(session(&st).lifecycle, Lifecycle::Live);
    assert_eq!(persists(&fx), 0);
}

#[test]
fn session_start_reply_carries_the_peers_note() {
    let mut st = seeded();
    let fx = feed_replying(
        &mut st,
        started(1, 1000, StartSource::Startup),
        Some((ConnId(3), ReqId(9))),
    );
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap().data,
        json!({"context": null})
    );
}

#[test]
fn an_event_with_nothing_to_write_is_no_failed_write() {
    let mut st = seeded();
    reseed(
        &mut st,
        Some(RefuseWrites {
            until: UtcMs(T0 + 3_600_000),
            reason: "test".into(),
        }),
        |s| s.activity_at = Some(UtcMs(T0 + 5000)),
    );
    let fx = feed(&mut st, envelope(2, 1000, HookKind::NeedsCleared));
    assert_eq!(logs(&fx), 0);
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
