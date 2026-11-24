//! `Store::load_snapshot` and `Store::apply`: every table loads, every `Change` writes what the
//! core's `State::apply` would hold, and a bad batch changes nothing.

use std::fs;
use std::path::Path;

use jiff::civil::date;
use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::model::*;
use moa_core::state::LoadNote;
use moa_store::accounts::AccountMap;
use moa_store::import::{IdSource, ImportEnv, import_spike};
use moa_store::{Snapshot, Store, StoreError};
use moa_testkit::{Scenario, SpikeFixture};

struct Rig {
    _dir: tempfile::TempDir,
    store: Store,
}

fn empty() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    Rig { _dir: dir, store }
}

fn typical() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("spike.db");
    SpikeFixture::write(&spike, Scenario::Typical, &home).unwrap();
    let mut store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    import_spike(&spike, &mut store, &env(&home)).unwrap();
    Rig { _dir: dir, store }
}

fn env(home: &Path) -> ImportEnv {
    ImportEnv {
        tz: TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
        tz_spec: None,
        now: UtcMs(1_800_000_000_000),
        brain: "test-brain".into(),
        accounts: AccountMap::new(home.to_path_buf()),
        ids: IdSource::Derived,
        transcript: find_transcript,
    }
}

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

fn project(id: i64, name: &str) -> Project {
    Project {
        id: ProjectId(id),
        name: name.into(),
        path: Some("~/private/lantern".into()),
        status: ProjectStatus::Paused,
        summary: Some("a lantern".into()),
        last_activity_on: Some(date(2026, 11, 14)),
        created_at: UtcMs(10),
        updated_at: UtcMs(11),
    }
}

fn session(n: u128) -> Session {
    Session {
        id: sid(n),
        agent_kind: AgentKind::Codex,
        account: Some("acct-a".into()),
        cwd: Some("/tmp/tidepool".into()),
        start_cwd: Some("/tmp/driftwood".into()),
        project_id: Some(ProjectId(1)),
        title: Some("Tidepool".into()),
        title_locked: true,
        group_path: Some("Lantern/Tidepool".into()),
        lifecycle: Lifecycle::Live,
        activity: Activity::NeedsAnswer,
        activity_at: Some(UtcMs(20)),
        activity_why: Some("asks".into()),
        last_prompt: Some("go".into()),
        last_turn_end_at: Some(UtcMs(21)),
        seen_at: Some(UtcMs(22)),
        asleep_at: Some(UtcMs(23)),
        archived_at: Some(UtcMs(24)),
        location: Location::Elsewhere,
        resume_failed: true,
        created_at: UtcMs(5),
        last_seen_at: Some(UtcMs(25)),
    }
}

fn task(id: i64) -> Task {
    Task {
        id: TaskId(id),
        project_id: Some(ProjectId(1)),
        title: "Trim the wick".into(),
        status: TaskStatus::Blocked,
        blocked_on: Some("oil".into()),
        today: Some(date(2026, 11, 16)),
        session_id: Some(sid(1)),
        note: Some("soon".into()),
        who: Some(Who::Agent),
        due: Some(date(2026, 11, 20)),
        priority: Some(Priority::P1),
        created_at: UtcMs(30),
        updated_at: UtcMs(31),
        done_at: Some(UtcMs(32)),
    }
}

fn conversation(kind: AgentKind, id: &str) -> Conversation {
    Conversation {
        agent_kind: kind,
        conversation_id: id.into(),
        session_id: sid(1),
        store_dir: Some("/tmp/store".into()),
        spike_account: Some("acct-b".into()),
        reason: ConversationReason::Fork,
        first_seen_at: UtcMs(40),
        last_seen_at: UtcMs(41),
        transcript_path: Some("/tmp/t.jsonl".into()),
        transcript_present: Some(true),
        transcript_checked_at: Some(UtcMs(42)),
    }
}

fn event() -> Event {
    Event {
        id: EventId(7),
        at: UtcMs(50),
        session_id: Some(sid(1)),
        conversation_id: Some("c-1".into()),
        account: Some("acct-a".into()),
        cwd: Some("/tmp/x".into()),
        kind: EventKind(EventKind::TURN_ENDED.into()),
        text: Some("done".into()),
        detail: Some(serde_json::json!({"n": 1})),
    }
}

fn feed() -> FeedEntry {
    FeedEntry {
        id: 4,
        thread_id: "th-1".into(),
        at: UtcMs(60),
        kind: "note".into(),
        message_id: Some("m-1".into()),
        automatic: true,
        body: "hello".into(),
        state: Some("sent".into()),
    }
}

fn nag() -> Nag {
    Nag {
        task_id: TaskId(1),
        due: date(2026, 11, 20),
        day: date(2026, 11, 18),
    }
}

fn report(id: i64, kind: ReportKind) -> Report {
    Report {
        id,
        session_id: sid(1),
        kind,
        queued_at: UtcMs(100),
        last_at: UtcMs(110),
        turns: vec!["Merged PR #3".into(), "Tests green".into()],
    }
}

fn pane_binding() -> PaneBinding {
    PaneBinding {
        session_id: sid(1),
        server_pid: 100,
        server_start: 101,
        socket: "/tmp/s".into(),
        pane_id: "%1".into(),
        pane_pid: 102,
        bound_at: UtcMs(70),
        tracked: true,
    }
}

fn terminal(id: i64) -> Terminal {
    Terminal {
        id: TermId(id),
        server_pid: 100,
        server_start: 101,
        pane_id: "%3".into(),
        opened_by: OpenedBy::Moa,
        cwd: "/tmp/lantern".into(),
        session_id: Some(sid(1)),
        opened_at: UtcMs(72),
        touched_at: Some(UtcMs(73)),
        name: "build".into(),
        note: Some("watch it".into()),
        timeout_at: Some(UtcMs(74)),
        shell: "zsh".into(),
        command: "make".into(),
        command_since: UtcMs(75),
        closed_at: None,
    }
}

fn spike_pane() -> SpikePane {
    SpikePane {
        session_id: sid(1),
        pane_id: "%2".into(),
        tmux: Some("main".into()),
        imported_at: UtcMs(71),
    }
}

fn group() -> Group {
    Group {
        path: "Lantern/Tidepool".into(),
        collapsed: true,
        created_at: UtcMs(80),
    }
}

fn thread() -> MoaThread {
    MoaThread {
        thread_id: "th-1".into(),
        account: "acct-a".into(),
        day: date(2026, 11, 16),
        kind: ThreadKind::Daily,
        started_at: UtcMs(90),
    }
}

fn activity(id: i64) -> ActivityRow {
    ActivityRow {
        id: ActivityId(id),
        at: UtcMs(1000 + id),
        who: ActivityWho::You,
        text: format!("did {id}"),
        ref_kind: Some("task".into()),
        ref_id: Some("1".into()),
    }
}

fn ask() -> Ask {
    Ask {
        id: AskId(2),
        at: UtcMs(100),
        from_session: Some(AskTarget::Session(sid(1))),
        from_name: Some("tide".into()),
        to_session: Some(AskTarget::Moa),
        text: "which?".into(),
        status: AskStatus::Answered,
        reply: Some("this".into()),
        claimed_at: Some(UtcMs(101)),
        answered_at: Some(UtcMs(102)),
    }
}

fn suggestion() -> Suggestion {
    Suggestion {
        id: SuggestionId(3),
        text: "tidy".into(),
        status: SuggestionStatus::Later,
        actions: vec![SuggestionAction {
            verb: "task".into(),
            args: vec!["a b".into(), "c".into()],
        }],
        actions_raw: Some("[\"task a\"]".into()),
        actions_invalid: Some("x".into()),
        result: Some("ok".into()),
        created_at: UtcMs(110),
        resolved_at: Some(UtcMs(111)),
    }
}

fn note(id: i64) -> Note {
    Note {
        id: NoteId(id),
        project_id: Some(ProjectId(1)),
        kind: NoteKind("milestone".into()),
        text: "shipped".into(),
        created_at: UtcMs(120),
    }
}

/// One row for every table, in an order the foreign keys accept.
fn one_of_each() -> Vec<Change> {
    vec![
        Change::Project(project(1, "Lantern")),
        Change::Session(session(1)),
        Change::Task(task(1)),
        Change::Note(note(1)),
        Change::Suggestion(suggestion()),
        Change::Conversation(conversation(AgentKind::Codex, "c-1")),
        Change::PaneBinding(pane_binding()),
        Change::SpikePane(spike_pane()),
        Change::Event(event()),
        Change::Activity(activity(1)),
        Change::Group(group()),
        Change::Ask(ask()),
        Change::Thread(thread()),
        Change::Feed(feed()),
        Change::Nag(nag()),
        Change::Report(report(1, ReportKind::TurnEnded)),
        Change::Terminal(terminal(1)),
        Change::Meta {
            key: "k".into(),
            value: Some("v".into()),
        },
    ]
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .conn()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn typical_snapshot_has_every_row() {
    let rig = typical();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.projects.len(), 3);
    assert_eq!(s.sessions.len(), 9);
    assert_eq!(s.tasks.len(), 5);
    assert_eq!(s.notes.len(), 4);
    assert_eq!(s.suggestions.len(), 4);
    assert_eq!(s.asks.len(), 3);
    assert_eq!(s.groups.len(), 2);
    assert_eq!(s.conversations.len(), 9);
    assert_eq!(s.spike_panes.len(), 2);
    assert_eq!(s.next_ids.task, 6);
    assert_eq!(s.next_ids.project, 4);
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);

    let total = count(&rig.store, "activity") as usize;
    assert_eq!(s.activity_tail.len(), total.min(300));
    let ids: Vec<i64> = s.activity_tail.iter().map(|a| a.id.0).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "oldest first");
    let max: i64 = rig
        .store
        .conn()
        .query_row("SELECT max(id) FROM activity", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ids.last(), Some(&max));
    assert_eq!(s.next_ids.activity, max + 1);
}

#[test]
fn an_empty_store_loads_an_empty_snapshot() {
    let rig = empty();
    assert_eq!(rig.store.load_snapshot().unwrap(), Snapshot::default());
}

#[test]
fn every_change_round_trips() {
    let mut rig = empty();
    let changes = one_of_each();
    rig.store.apply(&changes).unwrap();
    let s = rig.store.load_snapshot().unwrap();

    assert_eq!(s.projects, vec![project(1, "Lantern")]);
    assert_eq!(s.sessions, vec![session(1)]);
    assert_eq!(s.tasks, vec![task(1)]);
    assert_eq!(s.notes, vec![note(1)]);
    assert_eq!(s.suggestions, vec![suggestion()]);
    assert_eq!(s.conversations, vec![conversation(AgentKind::Codex, "c-1")]);
    assert_eq!(s.pane_bindings, vec![pane_binding()]);
    assert_eq!(s.spike_panes, vec![spike_pane()]);
    assert_eq!(s.activity_tail, vec![activity(1)]);
    assert_eq!(s.groups, vec![group()]);
    assert_eq!(s.asks, vec![ask()]);
    assert_eq!(s.threads, vec![thread()]);
    assert_eq!(s.nags, vec![nag()]);
    assert_eq!(s.reports, vec![report(1, ReportKind::TurnEnded)]);
    assert_eq!(s.terminals, vec![terminal(1)]);
    assert_eq!(s.meta, vec![("k".to_string(), "v".to_string())]);
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);
    assert_eq!(s.next_ids.event, 8);
    assert_eq!(s.next_ids.feed, 5);
    assert_eq!(s.next_ids.terminal, 2);
    assert_eq!(s.next_ids.report, 2);

    // Events and feed are not in a snapshot; read them back from the tables.
    let (ev, detail): (EventId, String) = rig
        .store
        .conn()
        .query_row(
            "SELECT id, detail FROM events WHERE session_id = ?1 AND conversation_id = 'c-1' \
             AND account = 'acct-a' AND cwd = '/tmp/x' AND kind = 'turn.ended' AND text = 'done' \
             AND at = 50",
            [sid(1).to_string()],
            |r| Ok((EventId(r.get(0)?), r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ev, event().id);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&detail).unwrap(),
        serde_json::json!({"n": 1})
    );
    let back = rig
        .store
        .conn()
        .query_row(
            "SELECT id, thread_id, at, kind, message_id, automatic, body, state FROM feed",
            [],
            |r| {
                Ok(FeedEntry {
                    id: r.get(0)?,
                    thread_id: r.get(1)?,
                    at: UtcMs(r.get(2)?),
                    kind: r.get(3)?,
                    message_id: r.get(4)?,
                    automatic: r.get(5)?,
                    body: r.get(6)?,
                    state: r.get(7)?,
                })
            },
        )
        .unwrap();
    assert_eq!(back, feed());

    // The deletes, and a meta key with no value, remove their rows and nothing else.
    rig.store
        .apply(&[
            Change::DeletePaneBinding(sid(1)),
            Change::DeleteSpikePane(sid(1)),
            Change::DeleteGroup("Lantern/Tidepool".into()),
            Change::Meta {
                key: "k".into(),
                value: None,
            },
        ])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert!(s.pane_bindings.is_empty());
    assert!(s.spike_panes.is_empty());
    assert!(s.groups.is_empty());
    assert!(s.meta.is_empty());
    assert_eq!(s.sessions, vec![session(1)]);
    assert_eq!(s.threads, vec![thread()]);
}

/// An adopted agent's binding is untracked, and stays so through a write and a load. A row
/// written without the column is tracked.
#[test]
fn bindings_round_trip_tracked() {
    let mut rig = empty();
    let untracked = PaneBinding {
        tracked: false,
        ..pane_binding()
    };
    rig.store
        .apply(&[
            Change::Project(project(1, "Lantern")),
            Change::Session(session(1)),
            Change::Session(session(2)),
            Change::PaneBinding(untracked.clone()),
        ])
        .unwrap();
    rig.store
        .conn()
        .execute(
            "INSERT INTO pane_bindings (session_id, server_pid, server_start, socket, pane_id, \
             pane_pid, bound_at) VALUES (?1, 100, 101, '/tmp/s', '%2', 103, 70)",
            [sid(2).to_string()],
        )
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    let tracked = PaneBinding {
        session_id: sid(2),
        pane_id: "%2".into(),
        pane_pid: 103,
        ..pane_binding()
    };
    assert_eq!(s.pane_bindings, vec![untracked, tracked]);
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);
}

#[test]
fn a_second_write_to_a_key_replaces_the_row() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    let mut p = project(1, "Lantern");
    p.status = ProjectStatus::Done;
    p.summary = None;
    let mut c = conversation(AgentKind::Codex, "c-1");
    c.reason = ConversationReason::Clear;
    // Same agent kind, another conversation id: a different row.
    let other = conversation(AgentKind::Codex, "c-2");
    // Same id as the claude one would be, so the key must include the kind.
    let claude = conversation(AgentKind::Claude, "c-1");
    let mut n = nag();
    n.due = date(2026, 11, 21);
    rig.store
        .apply(&[
            Change::Project(p.clone()),
            Change::Conversation(c.clone()),
            Change::Conversation(other.clone()),
            Change::Conversation(claude.clone()),
            Change::Nag(nag()),
            Change::Nag(n),
            Change::Meta {
                key: "k".into(),
                value: Some("w".into()),
            },
        ])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.projects, vec![p]);
    assert_eq!(s.conversations.len(), 3);
    assert!(s.conversations.contains(&c));
    assert!(s.conversations.contains(&other));
    assert!(s.conversations.contains(&claude));
    assert_eq!(s.nags.len(), 2);
    assert_eq!(s.meta, vec![("k".to_string(), "w".to_string())]);
}

#[test]
fn reports_load_and_change() {
    let mut rig = empty();
    let mut turn = report(1, ReportKind::TurnEnded);
    let needs = report(2, ReportKind::NeedsYou);
    rig.store
        .apply(&[Change::Report(turn.clone()), Change::Report(needs.clone())])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.reports, vec![turn.clone(), needs.clone()]);
    assert_eq!(s.next_ids.report, 3);
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);

    // An update keeps the row's id; the session's other kind is a row of its own.
    turn.turns.push("Pushed".into());
    turn.last_at = UtcMs(120);
    rig.store.apply(&[Change::Report(turn.clone())]).unwrap();
    assert_eq!(count(&rig.store, "reports"), 2);
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.reports, vec![turn.clone(), needs.clone()]);

    rig.store.apply(&[Change::DeleteReport(1)]).unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.reports, vec![needs]);
    // The next id is one past the highest row left, as a restart finds it.
    assert_eq!(s.next_ids.report, 3);

    // A kind this build doesn't know loads as a turn, with a note.
    rig.store
        .conn()
        .execute("UPDATE reports SET kind = 'nagging'", [])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.reports[0].kind, ReportKind::TurnEnded);
    assert_eq!(s.unknown.len(), 1, "{:?}", s.unknown);
    let LoadNote { table, key, .. } = &s.unknown[0];
    assert_eq!((*table, key.as_str()), ("reports", "#2"));
}

#[test]
fn upserting_a_project_keeps_what_points_at_it_linked() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    let mut p = project(1, "Lantern");
    p.summary = Some("changed".into());
    rig.store.apply(&[Change::Project(p.clone())]).unwrap();

    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.projects, vec![p]);
    assert_eq!(s.tasks[0].project_id, Some(ProjectId(1)));
    assert_eq!(s.notes[0].project_id, Some(ProjectId(1)));
    assert_eq!(s.sessions[0].project_id, Some(ProjectId(1)));

    // The same holds for a session that a task points at.
    let mut sess = session(1);
    sess.title = Some("renamed".into());
    rig.store.apply(&[Change::Session(sess)]).unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.tasks[0].session_id, Some(sid(1)));
}

#[test]
fn a_bad_batch_changes_nothing() {
    let mut rig = empty();
    let mut bad = task(1);
    bad.project_id = Some(ProjectId(999));
    bad.session_id = None;
    let err = rig
        .store
        .apply(&[Change::Project(project(1, "Lantern")), Change::Task(bad)])
        .unwrap_err();
    assert!(matches!(err, StoreError::Sqlite(_)), "{err}");
    assert_eq!(count(&rig.store, "projects"), 0);
    assert_eq!(count(&rig.store, "tasks"), 0);
}

#[test]
fn the_snapshot_keeps_the_newest_300_activity_rows_and_the_disk_keeps_all() {
    let mut rig = empty();
    let changes: Vec<Change> = (1..=305).map(|i| Change::Activity(activity(i))).collect();
    rig.store.apply(&changes).unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.activity_tail.len(), 300);
    assert_eq!(s.activity_tail.first().unwrap().id, ActivityId(6));
    assert_eq!(s.activity_tail.last().unwrap().id, ActivityId(305));
    assert_eq!(s.next_ids.activity, 306);
    assert_eq!(count(&rig.store, "activity"), 305);
}

#[test]
fn unknown_enum_text_loads_with_a_note() {
    let rig = typical();
    rig.store
        .conn()
        .execute("UPDATE tasks SET status = 'someday' WHERE id = 1", [])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    let t = s.tasks.iter().find(|t| t.id == TaskId(1)).unwrap();
    assert_eq!(t.status, TaskStatus::Todo);
    assert_eq!(s.unknown.len(), 1, "{:?}", s.unknown);
    let LoadNote { table, key, reason } = &s.unknown[0];
    assert_eq!((*table, key.as_str()), ("tasks", "#1"));
    assert!(reason.contains("status"), "{reason}");
}

#[test]
fn a_note_is_keyed_like_the_change_for_its_row() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    let c = rig.store.conn();
    for sql in [
        "UPDATE projects SET status = 'x'",
        "UPDATE tasks SET status = 'x'",
        "UPDATE sessions SET lifecycle = 'x'",
        "UPDATE conversations SET reason = 'x'",
        "UPDATE activity SET who = 'x'",
        "UPDATE asks SET status = 'x'",
        "UPDATE suggestions SET status = 'x'",
        "UPDATE moa_threads SET kind = 'x'",
        "UPDATE terminals SET opened_by = 'x'",
    ] {
        c.execute(sql, []).unwrap();
    }
    let s = rig.store.load_snapshot().unwrap();

    let mut want: Vec<(&str, String)> = vec![
        Change::Project(s.projects[0].clone()).row().unwrap(),
        Change::Task(s.tasks[0].clone()).row().unwrap(),
        Change::Session(s.sessions[0].clone()).row().unwrap(),
        Change::Conversation(s.conversations[0].clone())
            .row()
            .unwrap(),
        Change::Activity(s.activity_tail[0].clone()).row().unwrap(),
        Change::Ask(s.asks[0].clone()).row().unwrap(),
        Change::Suggestion(s.suggestions[0].clone()).row().unwrap(),
        Change::Thread(s.threads[0].clone()).row().unwrap(),
        Change::Terminal(s.terminals[0].clone()).row().unwrap(),
    ];
    let mut got: Vec<(&str, String)> = s.unknown.iter().map(|n| (n.table, n.key.clone())).collect();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    // Spot-check the formats the note keys promise.
    assert!(got.contains(&("conversations", "codex/c-1".to_string())));
    assert!(got.contains(&("sessions", sid(1).to_string())));
    assert!(got.contains(&("tasks", "#1".to_string())));
    assert!(got.contains(&("terminals", "#1".to_string())));
    // Nobody moa knows of opened it.
    assert_eq!(s.terminals[0].opened_by, OpenedBy::Found);
}

#[test]
fn text_that_is_not_utf8_loads_lossily_with_a_note() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    rig.store
        .conn()
        .execute(
            "UPDATE groups SET path = CAST(x'4c616e7465726eff' AS TEXT)",
            [],
        )
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.groups[0].path, "Lantern\u{fffd}");
    assert_eq!(s.unknown.len(), 1);
    assert_eq!(s.unknown[0].table, "groups");
    assert_eq!(s.unknown[0].key, "Lantern\u{fffd}");
}

fn fk_off(store: &Store) {
    store
        .conn()
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
}

#[test]
fn skipped_rows_are_noted_under_the_key_a_change_for_them_has() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    fk_off(&rig.store);
    let c = rig.store.conn();
    c.execute("UPDATE conversations SET session_id = 'nope'", [])
        .unwrap();
    c.execute("UPDATE moa_threads SET day = 'nope'", [])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert!(s.conversations.is_empty());
    assert!(s.threads.is_empty());

    let got: Vec<(&str, String)> = s.unknown.iter().map(|n| (n.table, n.key.clone())).collect();
    let conv = Change::Conversation(conversation(AgentKind::Codex, "c-1"))
        .row()
        .unwrap();
    let thr = Change::Thread(thread()).row().unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.contains(&conv), "{got:?}");
    assert!(got.contains(&thr), "{got:?}");
}

#[test]
fn keys_that_are_not_written_canonically_are_skipped_not_fixed() {
    let mut rig = empty();
    rig.store.apply(&one_of_each()).unwrap();
    fk_off(&rig.store);
    let c = rig.store.conn();
    c.execute(
        "UPDATE sessions SET id = '0000000A-0000-0000-0000-00000000000B'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE pane_bindings SET session_id = '0000000A-0000-0000-0000-00000000000B'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE spike_panes SET session_id = '0000000A-0000-0000-0000-00000000000B'",
        [],
    )
    .unwrap();
    c.execute("UPDATE nags SET due = '2026-11-20 09:00'", [])
        .unwrap();
    c.execute("UPDATE moa_threads SET day = '2026-11-16 09:00'", [])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert!(s.sessions.is_empty());
    assert!(s.pane_bindings.is_empty());
    assert!(s.spike_panes.is_empty());
    assert!(s.nags.is_empty());
    assert!(s.threads.is_empty());

    let upper = "0000000A-0000-0000-0000-00000000000B".to_string();
    let mut got: Vec<(&str, String)> = s.unknown.iter().map(|n| (n.table, n.key.clone())).collect();
    got.sort();
    let mut want = vec![
        ("sessions", upper.clone()),
        ("pane_bindings", upper.clone()),
        ("spike_panes", upper),
        ("nags", "#1/2026-11-20 09:00/2026-11-18".to_string()),
        ("moa_threads", "th-1".to_string()),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn meta_is_read_lossily_and_a_missing_value_is_noted() {
    let rig = empty();
    let c = rig.store.conn();
    c.execute("INSERT INTO meta (key, value) VALUES ('a', NULL)", [])
        .unwrap();
    c.execute(
        "INSERT INTO meta (key, value) VALUES ('b', CAST(x'66ff' AS TEXT))",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO meta (key, value) VALUES ('c', 'ok')", [])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(
        s.meta,
        vec![
            ("a".to_string(), String::new()),
            ("b".to_string(), "f\u{fffd}".to_string()),
            ("c".to_string(), "ok".to_string()),
        ]
    );
    let got: Vec<(&str, &str)> = s
        .unknown
        .iter()
        .map(|n| (n.table, n.key.as_str()))
        .collect();
    assert_eq!(got, vec![("meta", "a"), ("meta", "b")]);
}

/// A terminal with nothing optional set, that moa found open, comes back as written.
#[test]
fn a_bare_terminal_round_trips() {
    let mut rig = empty();
    let bare = Terminal {
        opened_by: OpenedBy::Found,
        session_id: None,
        touched_at: None,
        note: None,
        timeout_at: None,
        ..terminal(4)
    };
    rig.store.apply(&[Change::Terminal(bare.clone())]).unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.terminals, vec![bare]);
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);
}

/// tmux reuses `%N` across servers, so one pane id can be open once per server; on one
/// server, only once until that record closes.
#[test]
fn the_store_refuses_two_open_records_for_one_pane() {
    let mut rig = empty();
    rig.store
        .apply(&[
            Change::Project(project(1, "Lantern")),
            Change::Session(session(1)),
            Change::Terminal(terminal(1)),
        ])
        .unwrap();
    let err = rig
        .store
        .apply(&[Change::Terminal(terminal(2))])
        .unwrap_err();
    assert!(matches!(err, StoreError::Sqlite(_)), "{err}");
    assert_eq!(count(&rig.store, "terminals"), 1);

    let other_server = Terminal {
        server_start: 999,
        ..terminal(3)
    };
    let closed = Terminal {
        closed_at: Some(UtcMs(80)),
        ..terminal(1)
    };
    rig.store
        .apply(&[
            Change::Terminal(other_server),
            Change::Terminal(closed),
            Change::Terminal(terminal(2)),
        ])
        .unwrap();
    assert_eq!(count(&rig.store, "terminals"), 3);
}

/// Only open terminals load, so memory stays flat as history grows; a closed one's id is
/// still taken.
#[test]
fn a_closed_terminal_is_not_loaded_but_its_id_is_not_reused() {
    let mut rig = empty();
    let closed = Terminal {
        pane_id: "%4".into(),
        closed_at: Some(UtcMs(80)),
        ..terminal(2)
    };
    rig.store
        .apply(&[
            Change::Project(project(1, "Lantern")),
            Change::Session(session(1)),
            Change::Terminal(terminal(1)),
            Change::Terminal(closed),
        ])
        .unwrap();
    let s = rig.store.load_snapshot().unwrap();
    assert_eq!(s.terminals, vec![terminal(1)]);
    assert_eq!(s.next_ids.terminal, 3);
    assert_eq!(count(&rig.store, "terminals"), 2);
}

#[test]
fn a_clashing_event_id_fails_the_commit() {
    let mut rig = empty();
    rig.store.apply(&[Change::Event(event())]).unwrap();
    let mut other = event();
    other.text = Some("overwritten".into());
    let err = rig
        .store
        .apply(&[Change::Project(project(1, "Lantern")), Change::Event(other)])
        .unwrap_err();
    assert!(matches!(err, StoreError::Sqlite(_)), "{err}");
    let text: String = rig
        .store
        .conn()
        .query_row("SELECT text FROM events WHERE id = 7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "done");
    assert_eq!(count(&rig.store, "projects"), 0);

    // Activity is history too.
    rig.store.apply(&[Change::Activity(activity(1))]).unwrap();
    let mut again = activity(1);
    again.text = "overwritten".into();
    assert!(rig.store.apply(&[Change::Activity(again)]).is_err());
    assert_eq!(
        rig.store.load_snapshot().unwrap().activity_tail,
        vec![activity(1)]
    );
}

fn thread_of(id: &str, account: &str, started: i64) -> MoaThread {
    MoaThread {
        thread_id: id.into(),
        account: account.into(),
        started_at: UtcMs(started),
        ..thread()
    }
}

fn feed_row(id: i64, thread: &str, message_id: Option<&str>) -> FeedEntry {
    FeedEntry {
        id,
        thread_id: thread.into(),
        message_id: message_id.map(Into::into),
        ..feed()
    }
}

#[test]
fn a_snapshot_carries_the_feed_tail_and_each_accounts_newest_keys() {
    let mut rig = empty();
    let mut changes = vec![
        Change::Thread(thread_of("a-old", "acct-a", 10)),
        Change::Thread(thread_of("a-new", "acct-a", 20)),
        Change::Thread(thread_of("b-only", "acct-b", 5)),
    ];
    // 150 rows: ids 1..=50 on a-old, 51..=100 on a-new, 101..=150 on b-only; every other row
    // has no message id.
    for id in 1..=150 {
        let thread = match id {
            1..=50 => "a-old",
            51..=100 => "a-new",
            _ => "b-only",
        };
        let key = (id % 2 == 0).then(|| format!("m-{id}"));
        changes.push(Change::Feed(feed_row(id, thread, key.as_deref())));
    }
    rig.store.apply(&changes).unwrap();
    let s = rig.store.load_snapshot().unwrap();
    let ids: Vec<i64> = s.feed_tail.iter().map(|f| f.id).collect();
    assert_eq!(ids, (51..=150).collect::<Vec<_>>());
    let mut keys = s.feed_keys.clone();
    keys.sort();
    let want: Vec<(String, String)> = (51..=150)
        .filter(|id| id % 2 == 0)
        .map(|id| {
            let thread = if id <= 100 { "a-new" } else { "b-only" };
            (thread.to_string(), format!("m-{id}"))
        })
        .collect();
    let mut want = want;
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(s.next_ids.feed, 151);
    assert_eq!(count(&rig.store, "feed"), 150);
}
