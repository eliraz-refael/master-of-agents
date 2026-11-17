//! Names, `ls`, `set`, `archive` and the group verbs, on the typical brain and on small
//! hand-built ones.

use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, Publish, handle_checked, reply_of};
use moa_core::live::display_state;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Session, SessionId,
};
use moa_core::names::{is_live_spike, label};
use moa_core::op::{ConnId, Done, Op, ReqId};
use moa_core::publish::CoreEvent;
use moa_core::state::{CallerInfo, LoadNote, Snapshot, State};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::{TypicalBrain, run};
use moa_testkit::ids::*;

fn typical() -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let st = b.state();
    (b, st)
}

fn sid(st: &State, conv: &str) -> SessionId {
    st.resolve(conv).unwrap()
}

fn h(conv: &str) -> &str {
    &conv[..8]
}

fn ok(st: &mut State, op: Op, now: &Now) -> Done {
    run(st, op, now).unwrap()
}

fn err(st: &mut State, op: Op, now: &Now) -> String {
    run(st, op, now).unwrap_err().0
}

fn effects(st: &mut State, op: Op, now: &Now) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(1),
            caller: CallerInfo::default(),
            op,
        },
        now.clone(),
    )
}

fn ui_texts(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Event(CoreEvent::Ui { text })) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn persisted(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Persist(_)))
}

fn set_op(q: &str, title: Option<&str>, group: Option<&str>) -> Op {
    Op::SessionSet {
        q: q.into(),
        title: title.map(Into::into),
        group: group.map(Into::into),
    }
}

fn group_paths(st: &State) -> Vec<String> {
    st.groups().map(|g| g.path.clone()).collect()
}

fn group_of(st: &State, conv: &str) -> Option<String> {
    st.session(sid(st, conv)).unwrap().group_path.clone()
}

// ---------- hand-built brains ----------

fn est() -> TimeZone {
    TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()
}

fn noon() -> Now {
    Now::at(UtcMs(1_800_000_000_000), Duration::ZERO, est())
}

fn session(id: &str, title: Option<&str>, seen: Option<i64>) -> Session {
    Session {
        id: id.parse().unwrap(),
        agent_kind: AgentKind::Claude,
        account: None,
        cwd: None,
        start_cwd: None,
        project_id: None,
        title: title.map(Into::into),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: None,
        activity_why: None,
        last_prompt: Some("Please continue".into()),
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: seen.map(UtcMs),
    }
}

fn conversation(session_id: &str, conversation_id: &str) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conversation_id.into(),
        session_id: session_id.parse().unwrap(),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

fn started(snapshot: Snapshot) -> State {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), noon());
    st
}

const M1: &str = "0195f3a2-1111-7000-8000-000000000001";
const M2: &str = "0195f3a2-1111-7000-8000-000000000002";
const C1: &str = "a1b2c3d4-0000-4000-8000-000000000001";
const C2: &str = "e5f6a7b8-0000-4000-8000-000000000002";

// ---------- resolution ----------

#[test]
fn names_resolve_like_the_spike() {
    let (_b, st) = typical();
    assert_eq!(st.resolve(&S1_WORKING[..8]).unwrap(), sid(&st, S1_WORKING));
    assert_eq!(
        st.resolve(&S1_WORKING[..6].to_uppercase()).unwrap(),
        sid(&st, S1_WORKING)
    );
    let moa_id = sid(&st, S1_WORKING).to_string();
    assert_eq!(st.resolve(&moa_id[..13]).unwrap(), sid(&st, S1_WORKING));
    assert_eq!(st.resolve("dimmer").unwrap(), sid(&st, S1_WORKING));
    assert_eq!(
        st.resolve("Beach scan").unwrap_err().0,
        "no session matches 'Beach scan'"
    );
    assert_eq!(
        st.resolve(&S6_ARCHIVED[..8]).unwrap(),
        sid(&st, S6_ARCHIVED)
    );
    let e = st.resolve("lantern").unwrap_err().0;
    assert!(e.starts_with("4 sessions match 'lantern'; be more specific:\n"));
    assert_eq!(
        e.lines()
            .skip(1)
            .map(|l| l.split_whitespace().next().unwrap())
            .collect::<Vec<_>>(),
        [
            h(S1_WORKING),
            h(S3_WAITING),
            h(S9_WAITING_IDLE),
            h(S5_ASLEEP)
        ]
    );
    insta::assert_snapshot!("ambiguous_lantern", e);
}

#[test]
fn an_exact_title_wins() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        set_op(h(S9_WAITING_IDLE), Some("Fix the flicker again"), None),
        &now,
    );
    // Both titles contain it; only S3's equals it.
    assert_eq!(st.resolve("fix the flicker").unwrap(), sid(&st, S3_WAITING));
    assert_eq!(
        st.resolve("FIX THE FLICKER AGAIN").unwrap(),
        sid(&st, S9_WAITING_IDLE)
    );
    // Not exact: both match.
    assert!(
        st.resolve("flicker")
            .unwrap_err()
            .0
            .starts_with("2 sessions match 'flicker'")
    );
}

#[test]
fn moa_ids_that_share_eight_characters_are_ambiguous_but_handles_are_not() {
    let st = started(Snapshot {
        sessions: vec![
            session(M1, Some("One"), Some(10)),
            session(M2, Some("Two"), Some(20)),
        ],
        conversations: vec![conversation(M1, C1), conversation(M2, C2)],
        ..Snapshot::default()
    });
    let e = st.resolve("0195f3a2").unwrap_err().0;
    assert!(
        e.starts_with("2 sessions match '0195f3a2'; be more specific:\n"),
        "{e}"
    );
    // The newest first, each by its own handle.
    assert_eq!(
        e.lines()
            .skip(1)
            .map(|l| l.split_whitespace().next().unwrap())
            .collect::<Vec<_>>(),
        [&C2[..8], &C1[..8]]
    );
    assert_eq!(st.resolve(&C1[..8]).unwrap(), M1.parse().unwrap());
    assert_eq!(st.resolve(&C2[..8]).unwrap(), M2.parse().unwrap());
    assert_ne!(
        st.handle_of(M1.parse().unwrap()),
        st.handle_of(M2.parse().unwrap())
    );
    // The full moa id is still enough.
    assert_eq!(st.resolve(M2).unwrap(), M2.parse().unwrap());
}

#[test]
fn a_session_with_no_conversation_shows_by_its_moa_id() {
    let st = started(Snapshot {
        sessions: vec![session(M1, Some("Lonely"), Some(10))],
        ..Snapshot::default()
    });
    let id: SessionId = M1.parse().unwrap();
    assert_eq!(st.handle_of(id), "0195f3a2");
    assert_eq!(st.resolve("0195f3a2").unwrap(), id);
    assert_eq!(st.resolve("lonely").unwrap(), id);
}

#[test]
fn a_prefix_matches_any_conversation_of_the_session() {
    let mut second = conversation(M1, C2);
    second.first_seen_at = UtcMs(5);
    let st = started(Snapshot {
        sessions: vec![session(M1, None, Some(10))],
        conversations: vec![conversation(M1, C1), second],
        ..Snapshot::default()
    });
    let id: SessionId = M1.parse().unwrap();
    // The current conversation is the latest first seen; the older one still finds it.
    assert_eq!(st.handle_of(id), &C2[..8]);
    assert_eq!(st.resolve(&C1[..8]).unwrap(), id);
    assert_eq!(st.resolve(&C2[..8]).unwrap(), id);
}

#[test]
fn titles_that_differ_only_in_case_are_both_exact() {
    let st = started(Snapshot {
        sessions: vec![
            session(M1, Some("Harbour"), Some(10)),
            session(M2, Some("harbour"), Some(20)),
        ],
        ..Snapshot::default()
    });
    assert_eq!(
        st.resolve("HARBOUR").unwrap_err().0,
        "2 sessions match 'HARBOUR'; be more specific:\n  0195f3a2  harbour  (-, idle)\n  0195f3a2  Harbour  (-, idle)"
    );
}

#[test]
fn an_empty_name_never_resolves() {
    let st = started(Snapshot {
        sessions: vec![session(M1, Some("Only"), Some(10))],
        ..Snapshot::default()
    });
    for q in ["", "  ", "\t"] {
        assert_eq!(
            st.resolve(q).unwrap_err().0,
            "a session name can't be empty"
        );
        assert!(st.candidates(q).is_empty());
    }
    let mut st = st;
    for op in [
        Op::SessionArchive {
            qs: vec!["".into()],
        },
        set_op(" ", Some("x"), None),
    ] {
        let fx = effects(&mut st, op, &noon());
        assert_eq!(
            reply_of(&fx).unwrap().as_ref().unwrap_err().0,
            "a session name can't be empty"
        );
        assert!(!persisted(&fx));
    }
    assert!(
        st.session(M1.parse().unwrap())
            .unwrap()
            .archived_at
            .is_none()
    );
}

#[test]
fn only_eight_candidates_are_listed() {
    let sessions = (1..=10)
        .map(|n| {
            session(
                &format!("0195f3a2-1111-7000-8000-{n:012}"),
                Some("Same"),
                Some(n),
            )
        })
        .collect();
    let st = started(Snapshot {
        sessions,
        ..Snapshot::default()
    });
    let e = st.resolve("same").unwrap_err().0;
    assert!(e.starts_with("10 sessions match 'same'; be more specific:\n"));
    assert_eq!(e.lines().count(), 9);
}

#[test]
fn the_label_and_the_live_filter() {
    let (_b, st) = typical();
    let s1 = st.session(sid(&st, S1_WORKING)).unwrap();
    assert_eq!(label(&st, s1, 40), "Wire up the dimmer [Private/Lantern]");
    let s3 = st.session(sid(&st, S3_WAITING)).unwrap();
    assert_eq!(label(&st, s3, 40), "Fix the flicker [Lantern]");
    let s2 = st.session(sid(&st, S2_IDLE)).unwrap();
    assert_eq!(label(&st, s2, 40), "Read the tide tables [-]");
    let mut s = s2.clone();
    s.title = None;
    assert_eq!(label(&st, &s, 6), "Please [-]");
    s.last_prompt = None;
    assert_eq!(label(&st, &s, 6), format!("{} [-]", h(S2_IDLE)));
    assert!(is_live_spike(s1));
    assert!(!is_live_spike(st.session(sid(&st, S4_ENDED)).unwrap()));
    assert!(!is_live_spike(st.session(sid(&st, S6_ARCHIVED)).unwrap()));
    assert!(is_live_spike(st.session(sid(&st, S7_ELSEWHERE)).unwrap()));
}

// ---------- ls ----------

#[test]
fn ls_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let now = b.now();
    let recent = ok(&mut st, Op::SessionLs { all: false }, &now);
    insta::assert_snapshot!(
        "ls",
        recent.text.replace(b.home.to_str().unwrap(), "<home>")
    );
    let all = ok(&mut st, Op::SessionLs { all: true }, &now);
    insta::assert_snapshot!(
        "ls_all",
        all.text.replace(b.home.to_str().unwrap(), "<home>")
    );
    let rows = all.data.as_array().unwrap();
    assert_eq!(rows.len(), 9);
    assert!(rows.iter().all(|r| r.as_object().unwrap().len() == 11));
    let first = &rows[0];
    assert_eq!(first["id"], sid(&st, S1_WORKING).to_string());
    assert_eq!(first["conversation"], S1_WORKING);
    assert_eq!(first["alive"], "working");
    assert_eq!(first["group"], "Private/Lantern");
}

#[test]
fn ls_shows_the_display_state() {
    let (b, mut st) = typical();
    let now = b.now();
    let all = ok(&mut st, Op::SessionLs { all: true }, &now);
    for row in all.data.as_array().unwrap() {
        let id: SessionId = row["id"].as_str().unwrap().parse().unwrap();
        let (state, why) = display_state(st.session(id).unwrap(), now.wall);
        assert_eq!(row["display"], state.word());
        assert_eq!(row["why"], why);
    }
    let first = &all.data[0];
    // The spike says working, but nothing has been heard for a while.
    assert_eq!(first["alive"], "working");
    assert_eq!(first["display"], "quiet");
}

#[test]
fn ls_hides_old_ended_sessions_but_not_asleep_ones() {
    let (b, mut st) = typical();
    let now = b.now();
    let text = ok(&mut st, Op::SessionLs { all: false }, &now).text;
    let handles: Vec<&str> = text.lines().map(|l| &l[..8]).collect();
    assert!(handles.contains(&h(S5_ASLEEP)), "asleep stays: {text}");
    assert!(handles.contains(&h(S7_ELSEWHERE)), "elsewhere stays");
    assert!(handles.contains(&h(S4_ENDED)), "exactly 2 days ago is kept");
    assert!(
        !handles.contains(&h(S8_CODEX)),
        "ended 6 days ago is hidden"
    );
    assert!(!handles.contains(&h(S6_ARCHIVED)));
}

// ---------- set ----------

#[test]
fn set_title_and_group() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        set_op(h(S2_IDLE), Some("Tide tables"), Some("Work/Infra")),
        &now,
    );
    assert_eq!(reply_of(&fx).unwrap().as_ref().unwrap().text, "ok");
    assert_eq!(ui_texts(&fx), ["session Tide tables → Work/Infra"]);
    assert!(group_paths(&st).contains(&"Work/Infra".to_string()));
    let s = st.session(sid(&st, S2_IDLE)).unwrap();
    assert_eq!(s.title.as_deref(), Some("Tide tables"));
    assert!(s.title_locked);
    assert_eq!(s.group_path.as_deref(), Some("Work/Infra"));
}

#[test]
fn set_returns_the_session_row() {
    let (b, mut st) = typical();
    let done = ok(
        &mut st,
        set_op(h(S2_IDLE), Some("Tide tables"), None),
        &b.now(),
    );
    assert_eq!(done.data["title"], "Tide tables");
    assert_eq!(done.data["title_locked"], true);
    assert_eq!(done.data["id"], sid(&st, S2_IDLE).to_string());
}

#[test]
fn set_a_group_alone_keeps_the_title_and_normalises() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        set_op(h(S2_IDLE), None, Some("/Work//Infra/")),
        &now,
    );
    assert_eq!(ui_texts(&fx), ["session Read the tide tables → Work/Infra"]);
    assert_eq!(group_of(&st, S2_IDLE).as_deref(), Some("Work/Infra"));
    // An existing group is not created again.
    let before = group_paths(&st).len();
    ok(&mut st, set_op(h(S4_ENDED), None, Some("Work/Infra")), &now);
    assert_eq!(group_paths(&st).len(), before);
}

#[test]
fn set_a_title_alone_is_a_rename() {
    let (b, mut st) = typical();
    let fx = effects(&mut st, set_op(h(S2_IDLE), Some("Tides"), None), &b.now());
    assert_eq!(ui_texts(&fx), ["session Tides renamed"]);
}

#[test]
fn set_with_no_flag_is_refused() {
    let (b, mut st) = typical();
    let fx = effects(&mut st, set_op(h(S2_IDLE), None, None), &b.now());
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "set needs --title or --group"
    );
    assert!(!persisted(&fx));
    assert!(ui_texts(&fx).is_empty());
}

#[test]
fn set_refuses_an_empty_title_and_an_empty_group() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(&mut st, set_op(h(S2_IDLE), Some(""), None), &now);
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "--title can't be empty"
    );
    assert!(!persisted(&fx));
    assert_eq!(
        err(&mut st, set_op(h(S2_IDLE), None, Some("//")), &now),
        "a group needs a name"
    );
    assert_eq!(
        err(&mut st, set_op("nope", Some("x"), None), &now),
        "no session matches 'nope'"
    );
}

#[test]
fn a_session_with_an_unknown_value_is_not_edited() {
    let b = TypicalBrain::new();
    let probe = b.state();
    let id = sid(&probe, S2_IDLE);
    let mut snapshot = b.snapshot();
    snapshot.unknown.push(LoadNote {
        table: "sessions",
        key: id.to_string(),
        reason: "x",
    });
    let mut st = State::new(BrainName::default_brain(), b.config());
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), b.now());
    let fx = effects(&mut st, set_op(h(S2_IDLE), Some("x"), None), &b.now());
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        format!(
            "session {} has a value this build doesn't know; update moa",
            h(S2_IDLE)
        )
    );
    assert!(!persisted(&fx));
    assert_eq!(
        st.session(id).unwrap().title.as_deref(),
        Some("Read the tide tables")
    );
}

// ---------- archive ----------

#[test]
fn archive_resolves_everything_first() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        Op::SessionArchive {
            qs: vec![h(S4_ENDED).into(), "nope".into()],
        },
        &now,
    );
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "no session matches 'nope'"
    );
    assert!(!persisted(&fx));
    assert!(
        st.session(sid(&st, S4_ENDED))
            .unwrap()
            .archived_at
            .is_none()
    );
}

#[test]
fn archive_marks_each_session_once() {
    let (b, mut st) = typical();
    let now = b.now();
    let s4 = sid(&st, S4_ENDED);
    let s2 = sid(&st, S2_IDLE);
    let fx = effects(
        &mut st,
        Op::SessionArchive {
            qs: vec![
                h(S4_ENDED).into(),
                "Sketch the harbour".into(),
                h(S2_IDLE).into(),
            ],
        },
        &now,
    );
    let done = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(done.text, "archived 2");
    assert_eq!(done.data["archived"], serde_json::json!([s4, s2]));
    assert_eq!(
        ui_texts(&fx),
        [
            "archived Sketch the harbour",
            "archived Read the tide tables"
        ]
    );
    assert_eq!(st.session(s4).unwrap().archived_at, Some(now.wall));
    // Again later: the first time stays.
    let later = Now::at(UtcMs(now.wall.0 + 60_000), Duration::ZERO, est());
    let fx = effects(
        &mut st,
        Op::SessionArchive {
            qs: vec![h(S4_ENDED).into()],
        },
        &later,
    );
    assert_eq!(reply_of(&fx).unwrap().as_ref().unwrap().text, "archived 0");
    assert!(!persisted(&fx), "nothing to save");
    assert_eq!(st.session(s4).unwrap().archived_at, Some(now.wall));
    assert_eq!(
        err(&mut st, Op::SessionArchive { qs: vec![] }, &now),
        "archive needs a session"
    );
}

// ---------- groups ----------

fn with_private_groups(st: &mut State, now: &Now) {
    for g in ["Private", "Private/Lantern", "Private2", "Private2/Lantern"] {
        ok(st, Op::GroupAdd { path: g.into() }, now);
    }
}

#[test]
fn group_rename_moves_the_subtree() {
    let (b, mut st) = typical();
    let now = b.now();
    with_private_groups(&mut st, &now);
    ok(
        &mut st,
        set_op(h(S2_IDLE), None, Some("Private2/Lantern")),
        &now,
    );
    let fx = effects(
        &mut st,
        Op::GroupRename {
            old: "Private".into(),
            new: "Home".into(),
        },
        &now,
    );
    let done = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(done.text, "ok");
    assert_eq!(
        done.data,
        serde_json::json!({"old": "Private", "new": "Home"})
    );
    assert_eq!(ui_texts(&fx), ["group Private → Home"]);
    let paths = group_paths(&st);
    for gone in ["Private", "Private/Lantern"] {
        assert!(!paths.contains(&gone.to_string()), "{paths:?}");
    }
    for kept in ["Home", "Home/Lantern", "Private2", "Private2/Lantern"] {
        assert!(paths.contains(&kept.to_string()), "{paths:?}");
    }
    assert_eq!(group_of(&st, S1_WORKING).as_deref(), Some("Home/Lantern"));
    // Lookalikes are untouched.
    assert_eq!(group_of(&st, S2_IDLE).as_deref(), Some("Private2/Lantern"));
}

#[test]
fn group_rename_into_an_existing_group_merges() {
    let (b, mut st) = typical();
    let now = b.now();
    with_private_groups(&mut st, &now);
    ok(
        &mut st,
        Op::GroupAdd {
            path: "Home".into(),
        },
        &now,
    );
    ok(
        &mut st,
        Op::GroupRename {
            old: "Private".into(),
            new: "Home".into(),
        },
        &now,
    );
    let paths = group_paths(&st);
    assert_eq!(paths.iter().filter(|p| *p == "Home").count(), 1);
    assert!(paths.contains(&"Home/Lantern".to_string()));
    assert_eq!(group_of(&st, S1_WORKING).as_deref(), Some("Home/Lantern"));
}

#[test]
fn group_rename_works_for_a_group_only_sessions_have() {
    let (b, mut st) = typical();
    let now = b.now();
    // S1 is in Private/Lantern; the row for it need not exist.
    ok(
        &mut st,
        Op::GroupRename {
            old: "Private/Lantern".into(),
            new: "Home".into(),
        },
        &now,
    );
    assert_eq!(group_of(&st, S1_WORKING).as_deref(), Some("Home"));
    assert!(group_paths(&st).contains(&"Home".to_string()));
}

#[test]
fn group_rename_of_nothing_is_refused() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        Op::GroupRename {
            old: "Nowhere".into(),
            new: "X".into(),
        },
        &now,
    );
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "no group Nowhere"
    );
    assert!(!persisted(&fx));
    assert_eq!(
        err(
            &mut st,
            Op::GroupRename {
                old: "".into(),
                new: "X".into()
            },
            &now
        ),
        "a group needs a name"
    );
}

#[test]
fn group_rm_refuses_while_sessions_remain() {
    let (b, mut st) = typical();
    let now = b.now();
    with_private_groups(&mut st, &now);
    assert_eq!(
        err(
            &mut st,
            Op::GroupRm {
                path: "Private".into()
            },
            &now
        ),
        "Private still has 1 session(s); move or archive them first"
    );
    ok(
        &mut st,
        Op::SessionArchive {
            qs: vec![h(S1_WORKING).into()],
        },
        &now,
    );
    let fx = effects(
        &mut st,
        Op::GroupRm {
            path: "Private".into(),
        },
        &now,
    );
    let done = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(done.text, "ok");
    assert_eq!(done.data, serde_json::json!({"path": "Private"}));
    assert_eq!(ui_texts(&fx), ["removed group Private"]);
    let paths = group_paths(&st);
    assert!(!paths.contains(&"Private".to_string()));
    assert!(!paths.contains(&"Private/Lantern".to_string()));
    assert!(paths.contains(&"Private2".to_string()), "lookalike stays");
    // Removing it again finds nothing, and says so instead of logging a removal.
    assert_eq!(
        err(
            &mut st,
            Op::GroupRm {
                path: "Private".into()
            },
            &now
        ),
        "no group Private"
    );
}

#[test]
fn group_changes_are_persisted_in_order() {
    let (b, mut st) = typical();
    let now = b.now();
    with_private_groups(&mut st, &now);
    let fx = effects(
        &mut st,
        Op::GroupRename {
            old: "Private2".into(),
            new: "Away".into(),
        },
        &now,
    );
    let Some(Effect::Persist(changes)) = fx.iter().find(|e| matches!(e, Effect::Persist(_))) else {
        panic!("no persist");
    };
    let first_insert = changes
        .iter()
        .position(|c| matches!(c, Change::Group(_)))
        .unwrap();
    let last_delete = changes
        .iter()
        .rposition(|c| matches!(c, Change::DeleteGroup(_)))
        .unwrap();
    assert!(last_delete < first_insert, "deletes come before inserts");
}
