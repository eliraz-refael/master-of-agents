use super::*;
use crate::change::Change;
use crate::config::{BrainName, Config};
use crate::publish::Topic;
use crate::time::UtcMs;
use jiff::civil::date;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

fn project(id: i64, name: &str) -> Project {
    Project {
        id: ProjectId(id),
        name: name.into(),
        path: None,
        status: ProjectStatus::Active,
        summary: None,
        last_activity_on: Some(date(2026, 11, 1)),
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
    }
}

fn task(id: i64, status: TaskStatus) -> Task {
    Task {
        id: TaskId(id),
        project_id: Some(ProjectId(1)),
        title: format!("task {id}"),
        status,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: None,
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    }
}

fn session(n: u128, project: Option<i64>, activity: Activity) -> Session {
    Session {
        id: sid(n),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some("/work/Lantern".into()),
        start_cwd: None,
        project_id: project.map(ProjectId),
        title: Some(format!("session {n}")),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity,
        activity_at: Some(UtcMs(n as i64)),
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

fn conv(session: u128, id: &str, first: i64) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: id.into(),
        session_id: sid(session),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(first),
        last_seen_at: UtcMs(first),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

fn activity_row(id: i64) -> ActivityRow {
    ActivityRow {
        id: ActivityId(id),
        at: UtcMs(id),
        who: ActivityWho::You,
        text: format!("did thing {id}"),
        ref_kind: None,
        ref_id: None,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        projects: vec![project(1, "Lantern"), project(2, "Tidepool")],
        tasks: vec![task(1, TaskStatus::Todo), task(2, TaskStatus::Active)],
        sessions: vec![
            session(1, Some(1), Activity::Working),
            session(2, Some(2), Activity::NeedsPermission),
            session(3, None, Activity::Idle),
        ],
        conversations: vec![
            conv(1, "aaaaaaaa-1111", 10),
            conv(2, "bbbbbbbb-2222", 10),
            conv(3, "cccccccc-3333", 10),
        ],
        groups: vec![Group {
            path: "Driftwood".into(),
            collapsed: false,
            created_at: UtcMs(1),
        }],
        activity_tail: vec![activity_row(7)],
        ..Snapshot::default()
    }
}

fn new_state() -> State {
    State::new(BrainName::default_brain(), Config::default())
}

fn state() -> State {
    let mut s = new_state();
    s.load(snapshot());
    s
}

fn all_topics() -> BTreeSet<Topic> {
    Topic::ALL.iter().copied().collect()
}

fn topics_of(c: &Change) -> BTreeSet<Topic> {
    c.topics().iter().copied().collect()
}

#[test]
fn load_then_publish_builds_every_topic() {
    let mut s = state();
    let m = s.publish(&all_topics()).unwrap();
    assert_eq!(m.rev, 1);
    assert_eq!(m.topics.len(), Topic::ALL.len());
    let keys = |t: Topic| -> Vec<String> { m.topics[&t].keys().cloned().collect() };
    assert_eq!(keys(Topic::Projects), ["1", "2"]);
    assert_eq!(keys(Topic::Tasks), ["1", "2"]);
    assert_eq!(keys(Topic::Groups), ["Driftwood"]);
    assert_eq!(keys(Topic::Activity), ["7"]);
    assert_eq!(keys(Topic::Status), ["counts"]);
    assert!(keys(Topic::Suggestions).is_empty());
    assert!(keys(Topic::Asks).is_empty());
    assert_eq!(
        keys(Topic::Sessions),
        [sid(1), sid(2), sid(3)].map(|i| i.to_string())
    );
    for topic in Topic::ALL {
        for (k, row) in m.topics[topic].iter() {
            assert_eq!(row["key"], k.as_str(), "{topic:?} row {k}");
        }
    }
    let row = &m.topics[&Topic::Sessions][&sid(2).to_string()];
    assert_eq!(row["handle"], "bbbbbbbb");
    assert_eq!(row["conversation"], "bbbbbbbb-2222");
    assert_eq!(row["project"], "Tidepool");
    assert_eq!(row["display_state"], "waiting");
    assert_eq!(row["why"], "needs you");
    assert_eq!(row["title"], "session 2");
    let no_project = &m.topics[&Topic::Sessions][&sid(3).to_string()];
    assert!(no_project["project"].is_null());
    assert_eq!(m.topics[&Topic::Tasks]["1"]["status"], "todo");
    assert_eq!(
        m.topics[&Topic::Projects]["1"]["last_activity_on"],
        "2026-11-01"
    );
}

#[test]
fn apply_then_publish_shows_the_change() {
    let mut s = state();
    let before = s.publish(&all_topics()).unwrap();
    let c = Change::Task(task(1, TaskStatus::Done));
    s.apply(&c);
    let after = s.publish(&topics_of(&c)).unwrap();
    assert_eq!(after.rev, 2);
    assert_eq!(after.topics[&Topic::Tasks]["1"]["status"], "done");
    assert_eq!(before.topics[&Topic::Tasks]["1"]["status"], "todo");
    for t in Topic::ALL.iter().filter(|t| **t != Topic::Tasks) {
        assert!(
            Arc::ptr_eq(&before.topics[t], &after.topics[t]),
            "{t:?} rebuilt"
        );
    }
    assert!(Arc::ptr_eq(&s.model(), &after));
    assert!(s.publish(&BTreeSet::new()).is_none());
    assert_eq!(s.model().rev, 2);
}

#[test]
fn archiving_a_session_updates_the_status_counts() {
    let mut s = state();
    let m = s.publish(&all_topics()).unwrap();
    let counts = &m.topics[&Topic::Status]["counts"];
    assert_eq!(counts["working"], 1);
    assert_eq!(counts["waiting"], 1);
    assert_eq!(counts["idle"], 1);
    assert!(counts.get("ended").is_none() && counts.get("archived").is_none());
    let mut archived = session(1, Some(1), Activity::Working);
    archived.archived_at = Some(UtcMs(50));
    let c = Change::Session(archived);
    s.apply(&c);
    let m = s.publish(&topics_of(&c)).unwrap();
    let counts = &m.topics[&Topic::Status]["counts"];
    assert_eq!(counts["working"], 0);
    assert_eq!(counts["waiting"], 1);
    assert_eq!(
        m.topics[&Topic::Sessions][&sid(1).to_string()]["display_state"],
        "archived"
    );
}

#[test]
fn load_keeps_rev_moving() {
    let mut s = state();
    for _ in 0..5 {
        s.publish(&all_topics());
    }
    assert_eq!(s.model().rev, 5);
    s.load(Snapshot {
        projects: vec![project(9, "Driftwood")],
        ..Snapshot::default()
    });
    assert!(s.projects().all(|p| p.id == ProjectId(9)));
    assert_eq!(s.sessions().count(), 0);
    let m = s.publish(&all_topics()).unwrap();
    assert_eq!(m.rev, 6);
    assert!(m.topics[&Topic::Sessions].is_empty());
    assert_eq!(m.topics[&Topic::Projects].len(), 1);
}

#[test]
fn every_change_names_its_row() {
    assert_eq!(
        Change::Task(task(3, TaskStatus::Todo)).row(),
        Some(("tasks", "#3".to_string()))
    );
    assert_eq!(
        Change::Session(session(5, None, Activity::Idle)).row(),
        Some(("sessions", sid(5).to_string()))
    );
    assert_eq!(
        Change::Group(Group {
            path: "Driftwood".into(),
            collapsed: false,
            created_at: UtcMs(1)
        })
        .row(),
        Some(("groups", "Driftwood".to_string()))
    );
    assert_eq!(
        Change::DeleteGroup("Driftwood".into()).row(),
        Some(("groups", "Driftwood".to_string()))
    );
    assert_eq!(
        Change::DeletePaneBinding(sid(5)).row(),
        Some(("pane_bindings", sid(5).to_string()))
    );
    assert_eq!(
        Change::Meta {
            key: "k".into(),
            value: None
        }
        .row(),
        None
    );
}

#[test]
fn changes_name_their_topics() {
    let event = Change::Event(Event {
        id: EventId(1),
        at: UtcMs(1),
        session_id: None,
        conversation_id: None,
        account: None,
        cwd: None,
        kind: EventKind(EventKind::UI.into()),
        text: None,
        detail: None,
    });
    assert!(event.topics().is_empty());
    assert_eq!(
        Change::Session(session(1, None, Activity::Idle)).topics(),
        [Topic::Sessions, Topic::Status]
    );
    assert_eq!(
        Change::Conversation(conv(1, "x", 1)).topics(),
        [Topic::Sessions]
    );
    assert_eq!(
        Change::Project(project(1, "Lantern")).topics(),
        [Topic::Projects, Topic::Sessions]
    );
    assert_eq!(
        Change::Task(task(1, TaskStatus::Todo)).topics(),
        [Topic::Tasks]
    );
    assert_eq!(
        Change::Activity(activity_row(1)).topics(),
        [Topic::Activity]
    );
    assert_eq!(Change::DeleteGroup("g".into()).topics(), [Topic::Groups]);
    assert!(
        Change::Meta {
            key: "k".into(),
            value: Some("v".into())
        }
        .topics()
        .is_empty()
    );
}

#[test]
fn topics_are_ordered_and_serialize_lowercase() {
    assert!(Topic::Sessions < Topic::Status);
    assert_eq!(
        serde_json::to_string(&Topic::Suggestions).unwrap(),
        "\"suggestions\""
    );
    let e = crate::publish::CoreEvent::Ui { text: "hi".into() };
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"kind":"ui","text":"hi"}"#
    );
}

#[test]
fn next_ids_start_at_one_and_follow_applied_rows() {
    let mut s = new_state();
    assert_eq!(s.next_id(Table::Task), 1);
    s.apply(&Change::Task(task(4, TaskStatus::Todo)));
    assert_eq!(s.next_id(Table::Task), 5);
    s.apply(&Change::Task(task(2, TaskStatus::Todo)));
    assert_eq!(s.next_id(Table::Task), 5);
    assert_eq!(s.next_id(Table::Project), 1);
    s.load(Snapshot {
        next_ids: NextIds {
            task: 12,
            ..NextIds::default()
        },
        ..Snapshot::default()
    });
    assert_eq!(s.next_id(Table::Task), 12);
    assert_eq!(s.next_id(Table::Feed), 1);
}

#[test]
fn apply_edits_rows_in_memory() {
    let mut s = state();
    s.apply(&Change::Meta {
        key: "k".into(),
        value: Some("v".into()),
    });
    assert_eq!(s.meta("k"), Some("v"));
    s.apply(&Change::Meta {
        key: "k".into(),
        value: None,
    });
    assert_eq!(s.meta("k"), None);
    s.apply(&Change::DeleteGroup("Driftwood".into()));
    assert_eq!(s.groups().count(), 0);
    for i in 100..500 {
        s.apply(&Change::Activity(activity_row(i)));
    }
    let m = s.publish(&all_topics()).unwrap();
    assert_eq!(m.topics[&Topic::Activity].len(), 300);
    assert!(m.topics[&Topic::Activity].contains_key("499"));
    assert!(!m.topics[&Topic::Activity].contains_key("7"));
}

fn info(moa: Option<&str>, claude: Option<&str>, cwd: Option<&str>) -> CallerInfo {
    CallerInfo {
        moa_session: moa.map(Into::into),
        claude_session: claude.map(Into::into),
        cwd: cwd.map(PathBuf::from),
    }
}

#[test]
fn the_caller_is_found_by_moa_session_then_conversation() {
    let s = state();
    assert_eq!(s.caller(&info(Some("moa"), None, None)), Caller::Moa);
    assert_eq!(Caller::Moa.to_string(), "moa");
    let by_id = s.caller(&info(Some(&sid(2).to_string()), None, Some("/x/y")));
    assert_eq!(by_id, Caller::Session(sid(2), "bbbbbbbb".into()));
    assert_eq!(by_id.to_string(), "bbbbbbbb");
    let by_conv = s.caller(&info(None, Some("cccccccc-3333"), None));
    assert_eq!(by_conv, Caller::Session(sid(3), "cccccccc".into()));
    // an unknown moa session falls through to the conversation
    let both = s.caller(&info(
        Some(&sid(99).to_string()),
        Some("aaaaaaaa-1111"),
        None,
    ));
    assert_eq!(both, Caller::Session(sid(1), "aaaaaaaa".into()));
    let unknown = s.caller(&info(None, Some("zzz"), Some("/home/me/Lantern")));
    assert_eq!(
        unknown,
        Caller::Unknown {
            folder: "Lantern".into()
        }
    );
    assert_eq!(unknown.to_string(), "unknown in Lantern");
    let none = s.caller(&info(None, None, None));
    assert_eq!(none.to_string(), "an agent outside moa");
}

#[test]
fn handles_use_the_current_conversation() {
    let mut s = state();
    assert_eq!(s.handle_of(sid(1)), "aaaaaaaa");
    s.apply(&Change::Conversation(conv(1, "dddddddd-4444", 20)));
    assert_eq!(s.handle_of(sid(1)), "dddddddd");
    assert_eq!(s.conversations_of(sid(1)).count(), 2);
    // a session with no conversation uses its own id
    s.apply(&Change::Session(session(0xabc, None, Activity::Idle)));
    assert_eq!(s.handle_of(sid(0xabc)), "00000000");
    // ids shorter than 8 characters are kept whole
    s.apply(&Change::Conversation(conv(2, "xy", 99)));
    assert_eq!(s.handle_of(sid(2)), "xy");
}
