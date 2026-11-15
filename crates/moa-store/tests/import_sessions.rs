use std::fs;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_core::time::spike_local_to_utc;
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::import::{
    IdSource, ImportEnv, ImportReport, Level, RecordNote, TableCounts, import_spike,
};
use moa_store::spike::SCHEMA_SQL;
use moa_testkit::spike_gen::brain_any;
use moa_testkit::{Scenario, SpikeFixture, ids};
use proptest::prelude::*;
use rusqlite::Connection;

const NOW: UtcMs = UtcMs(1_800_000_000_000);

fn tz() -> TimeZone {
    TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()
}

fn env(home: &Path, brain: &str) -> ImportEnv {
    ImportEnv {
        tz: tz(),
        now: NOW,
        brain: brain.into(),
        accounts: AccountMap::new(home.to_path_buf()),
        ids: IdSource::Derived,
        transcript: find_transcript,
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    home: PathBuf,
    spike: PathBuf,
    store: Store,
}

fn empty_setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("spike.db");
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    Setup {
        _dir: dir,
        home,
        spike,
        store,
    }
}

fn setup(scenario: Scenario) -> Setup {
    let s = empty_setup();
    SpikeFixture::write(&s.spike, scenario, &s.home).unwrap();
    s
}

fn run(s: &mut Setup) -> ImportReport {
    import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap()
}

fn local(text: &str) -> i64 {
    spike_local_to_utc(text, &tz()).unwrap().0
}

fn col<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Every spike session id with its raw account string, read from the spike DB.
fn spike_pairs(spike: &Path) -> Vec<(String, Option<String>)> {
    let c = Connection::open_with_flags(spike, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.prepare("SELECT session_id, account FROM sessions")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn session_id(c: &Connection, conv: &str) -> String {
    col(
        c,
        &format!("SELECT session_id FROM conversations WHERE conversation_id = '{conv}'"),
    )
}

/// `(lifecycle, activity, location)` of a session by its spike id.
fn state(c: &Connection, conv: &str) -> (String, String, String) {
    c.query_row(
        "SELECT s.lifecycle, s.activity, s.location FROM sessions s \
         JOIN conversations v ON v.session_id = s.id WHERE v.conversation_id = ?1",
        [conv],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

/// The tables this task imports: every source row of each is kept.
const SESSION_TABLES: [&str; 7] = [
    "sessions",
    "conversations",
    "spike_panes",
    "events",
    "tasks",
    "suggestions",
    "asks",
];

fn warns(r: &ImportReport) -> Vec<&RecordNote> {
    r.notes.iter().filter(|n| n.level == Level::Warn).collect()
}

fn count(r: &ImportReport, table: &str) -> TableCounts {
    r.tables
        .iter()
        .find(|t| t.table == table)
        .unwrap_or_else(|| panic!("no counts for {table}"))
        .clone()
}

#[test]
#[allow(clippy::type_complexity)]
fn typical_brain_imports_cleanly() {
    let mut s = setup(Scenario::Typical);
    let report = run(&mut s);
    let c = s.store.conn();

    let w = warns(&report);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].table, "conversations");
    assert_eq!(w[0].key, &ids::S2_IDLE[..8]);
    assert!(
        w[0].reason
            .starts_with("transcript missing; resume will fail (account #")
    );

    assert_eq!(col::<i64>(c, "SELECT count(*) FROM sessions"), 9);
    assert_eq!(
        state(c, ids::S1_WORKING),
        ("live".into(), "working".into(), "here".into())
    );
    let (group, locked): (String, i64) = c
        .query_row(
            "SELECT group_path, title_locked FROM sessions s JOIN conversations v \
             ON v.session_id = s.id WHERE v.conversation_id = ?1",
            [ids::S1_WORKING],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((group.as_str(), locked), ("Private/Lantern", 1));
    assert_eq!(
        state(c, ids::S2_IDLE),
        ("live".into(), "idle".into(), "here".into())
    );
    assert_eq!(
        state(c, ids::S3_WAITING),
        ("live".into(), "needs_permission".into(), "here".into())
    );
    assert_eq!(
        state(c, ids::S9_WAITING_IDLE),
        ("live".into(), "idle".into(), "here".into())
    );
    assert_eq!(
        state(c, ids::S4_ENDED),
        ("ended".into(), "idle".into(), "here".into())
    );
    assert_eq!(
        state(c, ids::S7_ELSEWHERE),
        ("ended".into(), "idle".into(), "elsewhere".into())
    );

    // S9's idle ping is an idle.ping event, S3's permission request is not.
    let kind_of = |conv: &str, text: &str| -> String {
        c.query_row(
            "SELECT kind FROM events WHERE conversation_id = ?1 AND text LIKE ?2",
            [conv, text],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        kind_of(ids::S9_WAITING_IDLE, "%waiting for your input%"),
        "idle.ping"
    );
    assert_eq!(kind_of(ids::S3_WAITING, "%permission%"), "needs.permission");

    // The why of an idle session is the last line of its stop text.
    let why = |conv: &str| -> Option<String> {
        c.query_row(
            "SELECT s.activity_why FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = ?1",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        why(ids::S2_IDLE).as_deref(),
        Some("Want me to check the next one?")
    );
    assert_eq!(why(ids::S1_WORKING).as_deref(), Some("Please continue"));
    assert_eq!(
        why(ids::S3_WAITING).as_deref(),
        Some("Claude needs your permission to use Bash")
    );
    assert_eq!(why(ids::S4_ENDED).as_deref(), Some("Sketched"));

    // Times.
    let times =
        |conv: &str| -> (
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) {
            c.query_row(
            "SELECT s.created_at, s.last_seen_at, s.activity_at, s.last_turn_end_at, s.seen_at, \
             s.archived_at FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = ?1",
            [conv],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap()
        };
    let t2 = times(ids::S2_IDLE);
    assert_eq!(t2.0, local("2026-11-14 16:00:00"));
    assert_eq!(t2.1, Some(local("2026-11-14 16:45:00")));
    assert_eq!(t2.2, Some(local("2026-11-14 16:05:00")));
    assert_eq!(t2.3, Some(local("2026-11-14 16:05:00")));
    assert_eq!(t2.4, Some(NOW.0));
    assert_eq!(t2.5, None);
    // S9: the ping at 06:11 is ignored, the stop at 06:10 is the latest.
    assert_eq!(
        times(ids::S9_WAITING_IDLE).2,
        Some(local("2026-11-15 06:10:00"))
    );
    // S3 has no stop: its turn never ended; its latest event is the permission request.
    let t3 = times(ids::S3_WAITING);
    assert_eq!((t3.2, t3.3), (Some(local("2026-11-15 08:10:00")), None));
    // S7 has no events, so activity_at is last_seen.
    assert_eq!(
        times(ids::S7_ELSEWHERE).2,
        Some(local("2026-11-11 14:20:00"))
    );

    let asleep = |conv: &str| -> Option<i64> {
        c.query_row(
            "SELECT s.asleep_at FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = ?1",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(asleep(ids::S5_ASLEEP), Some(local("2026-11-12 10:30:00")));
    assert_eq!(state(c, ids::S5_ASLEEP).0, "ended");
    let t6 = times(ids::S6_ARCHIVED);
    assert_eq!(t6.5, Some(local("2026-11-10 09:30:00")));
    assert_eq!(asleep(ids::S6_ARCHIVED), None);
    assert_eq!(
        state(c, ids::S6_ARCHIVED),
        ("ended".into(), "idle".into(), "here".into())
    );
    assert!(report.notes.iter().any(|n| n.table == "sessions"
        && n.key == ids::S6_ARCHIVED[..8]
        && n.level == Level::Info
        && n.reason == "stale asleep mark dropped"));

    // start_cwd and resume_failed.
    let (start_cwd, cwd, failed): (String, String, i64) = c
        .query_row(
            "SELECT start_cwd, cwd, resume_failed FROM sessions s JOIN conversations v \
             ON v.session_id = s.id WHERE v.conversation_id = ?1",
            [ids::S1_WORKING],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(start_cwd, cwd);
    assert_eq!(failed, 0);

    // Accounts and agents.
    let (kind, account): (String, Option<String>) = c
        .query_row(
            "SELECT s.agent_kind, s.account FROM sessions s JOIN conversations v \
             ON v.session_id = s.id WHERE v.conversation_id = ?1",
            [ids::S8_CODEX],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), account.as_deref()),
        ("codex", Some("codex"))
    );
    let acct: String = col(
        c,
        &format!(
            "SELECT s.account FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = '{}'",
            ids::S1_WORKING
        ),
    );
    assert_eq!(acct, "acct-a");
    assert_eq!(report.accounts, ["acct-a", "acct-b", "codex"]);

    // Conversations.
    assert_eq!(col::<i64>(c, "SELECT count(*) FROM conversations"), 9);
    type Conv = (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<i64>,
        Option<String>,
        Option<i64>,
    );
    let conv = |id: &str| -> Conv {
        c.query_row(
            "SELECT agent_kind, conversation_id, store_dir, spike_account, reason, \
             transcript_present, transcript_path, transcript_checked_at \
             FROM conversations WHERE conversation_id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap()
    };
    let raw_account = format!("{}/.acct-a", s.home.display());
    let c1 = conv(ids::S1_WORKING);
    assert_eq!(c1.0, "claude");
    assert_eq!(c1.1, ids::S1_WORKING);
    assert_eq!(c1.2.as_deref(), Some(raw_account.as_str()));
    assert_eq!(c1.3.as_deref(), Some(raw_account.as_str()));
    assert_eq!(c1.4, "import");
    assert_eq!(c1.5, Some(1));
    assert!(
        c1.6.unwrap()
            .ends_with(&format!("{}.jsonl", ids::S1_WORKING))
    );
    assert_eq!(c1.7, Some(NOW.0));
    let c2 = conv(ids::S2_IDLE);
    assert_eq!((c2.5, c2.6, c2.7), (Some(0), None, Some(NOW.0)));
    let pairs = spike_pairs(&s.spike);
    assert_eq!(pairs.len(), 9);
    for (id, raw_acct) in pairs {
        let cv = conv(&id);
        assert_eq!(cv.1, id);
        assert_eq!(cv.3, raw_acct, "{id}");
    }
    let c8 = conv(ids::S8_CODEX);
    assert_eq!(c8.0, "codex");
    assert_eq!((c8.2, c8.5, c8.6, c8.7), (None, None, None, None));
    assert_eq!(c8.3.as_deref(), Some("codex"));
    assert!(report.notes.iter().any(|n| n.table == "conversations"
        && n.key == ids::S8_CODEX[..8]
        && n.level == Level::Info
        && n.reason == "transcript not checked"));
    // The session id of a conversation is the session's id, and it is a v7 uuid.
    let sid = session_id(c, ids::S1_WORKING);
    assert_eq!(sid.len(), 36);
    assert_eq!(&sid[14..15], "7");

    // spike_panes: S1 and S2 only.
    let panes: Vec<(String, String, Option<String>, i64)> = c
        .prepare(
            "SELECT v.conversation_id, p.pane_id, p.tmux, p.imported_at FROM spike_panes p \
             JOIN conversations v ON v.session_id = p.session_id ORDER BY v.conversation_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut want = vec![
        (
            ids::S1_WORKING.to_string(),
            "%3".to_string(),
            Some("/tmp/x/moa".to_string()),
            NOW.0,
        ),
        (
            ids::S2_IDLE.to_string(),
            "%4".to_string(),
            Some("/tmp/x/moa".to_string()),
            NOW.0,
        ),
    ];
    want.sort();
    assert_eq!(panes, want);
    assert!(report.notes.iter().any(|n| n.table == "sessions"
        && n.key == ids::S1_WORKING[..8]
        && n.level == Level::Info
        && n.reason == "pane not verified yet (checked at switch-over)"));

    // Events keep their ids, and a ui event has no session.
    assert_eq!(col::<i64>(c, "SELECT count(*) FROM events"), 19);
    assert_eq!(col::<i64>(c, "SELECT max(id) FROM events"), 19);
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM events WHERE kind = 'ui' AND session_id IS NULL"
        ),
        3
    );
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM events WHERE kind = 'session.started'"
        ),
        5
    );
    assert_eq!(
        col::<i64>(c, "SELECT count(*) FROM events WHERE kind = 'turn.started'"),
        5
    );
    assert_eq!(
        col::<i64>(c, "SELECT count(*) FROM events WHERE kind = 'turn.ended'"),
        3
    );
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM events WHERE kind = 'session.ended'"
        ),
        1
    );
    let s1_moa = session_id(c, ids::S1_WORKING);
    assert_eq!(
        col::<i64>(
            c,
            &format!(
                "SELECT count(*) FROM events WHERE session_id = '{s1_moa}' AND conversation_id = '{}'",
                ids::S1_WORKING
            )
        ),
        2
    );

    // Tasks.
    let t1: (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<i64>,
    ) = c
        .query_row(
            "SELECT status, session_id, who, due, priority, created_at, updated_at, done_at \
             FROM tasks WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(t1.0, "todo");
    assert_eq!(t1.1.as_deref(), Some(s1_moa.as_str()));
    assert_eq!(t1.2.as_deref(), Some("you"));
    assert_eq!(t1.3.as_deref(), Some("2026-11-16"));
    assert_eq!(t1.4.as_deref(), Some("p1"));
    assert_eq!(t1.5, local("2026-11-14 09:00:00"));
    assert_eq!(t1.7, None);
    let t2: (i64, Option<i64>) = c
        .query_row(
            "SELECT updated_at, done_at FROM tasks WHERE id = 2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(t2.1, Some(t2.0));
    assert_eq!(col::<i64>(c, "SELECT count(*) FROM tasks"), 5);

    // Suggestions.
    let args: String = col(c, "SELECT actions FROM suggestions WHERE id = 2");
    let actions: serde_json::Value = serde_json::from_str(&args).unwrap();
    assert_eq!(actions[0]["verb"], "set");
    assert_eq!(actions[0]["args"][0], &ids::S1_WORKING[..8]);
    assert_eq!(actions[0]["args"][1], "--title");
    assert_eq!(actions[0]["args"][2], "New name");
    let (raw, invalid): (Option<String>, Option<String>) = c
        .query_row(
            "SELECT actions_raw, actions_invalid FROM suggestions WHERE id = 2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(raw.unwrap().starts_with("[\"set "));
    assert_eq!(invalid, None);
    let none: (String, Option<String>) = c
        .query_row(
            "SELECT actions, actions_raw FROM suggestions WHERE id = 3",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(none, ("[]".to_string(), None));
    let note_actions: serde_json::Value = serde_json::from_str(&col::<String>(
        c,
        "SELECT actions FROM suggestions WHERE id = 4",
    ))
    .unwrap();
    assert_eq!(
        note_actions[0]["args"],
        serde_json::json!(["--kind", "fact", "x y"])
    );

    // Asks.
    type Ask = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let ask = |id: i64| -> Ask {
        c.query_row(
            "SELECT status, reply, from_session, to_session, at, claimed_at, answered_at \
             FROM asks WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap()
    };
    let s2_moa = session_id(c, ids::S2_IDLE);
    let s3_moa = session_id(c, ids::S3_WAITING);
    let a1 = ask(1);
    assert_eq!(a1.0, "answered");
    assert_eq!(a1.1.as_deref(), Some("Pin 7"));
    assert_eq!(a1.2.as_deref(), Some(s2_moa.as_str()));
    assert_eq!(a1.3.as_deref(), Some(s1_moa.as_str()));
    assert_eq!(a1.4, local("2026-11-15 08:20:00"));
    assert_eq!(a1.5, Some(local("2026-11-15 08:20:00")));
    assert_eq!(a1.6, Some(local("2026-11-15 08:25:00")));
    let a2 = ask(2);
    assert_eq!(
        (a2.0.as_str(), a2.3.as_deref(), a2.5),
        ("dropped", Some("moa"), None)
    );
    assert_eq!(a2.2.as_deref(), Some(s1_moa.as_str()));
    let a3 = ask(3);
    assert_eq!(a3.0, "failed");
    assert_eq!(
        a3.1.as_deref(),
        Some("moa switched over before it was answered")
    );
    assert_eq!(a3.2, None);
    assert_eq!(a3.3.as_deref(), Some(s3_moa.as_str()));
    assert_eq!(a3.5, Some(local("2026-11-15 08:35:00")));
    for (id, reason) in [(2, "pending"), (3, "sent")] {
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.table == "asks" && n.key == format!("#{id}") && n.level == Level::Info),
            "{reason}"
        );
    }

    for t in &report.tables {
        assert!(t.imported <= t.source, "{}", t.table);
    }
    for t in SESSION_TABLES {
        let n = count(&report, t);
        assert!(n.source > 0, "{t}");
        assert_eq!(n.source, n.imported, "{t}");
    }
    assert_eq!(count(&report, "spike_panes").imported, 2);
}

fn import_ids(brain: &str) -> Vec<(String, String, i64)> {
    let mut s = setup(Scenario::Typical);
    import_spike(&s.spike, &mut s.store, &env(&s.home, brain)).unwrap();
    s.store
        .conn()
        .prepare(
            "SELECT s.id, v.conversation_id, s.created_at FROM sessions s \
             JOIN conversations v ON v.session_id = s.id ORDER BY s.id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn ids_are_the_same_on_every_run() {
    let a = import_ids("default");
    let b = import_ids("default");
    assert_eq!(a, b);
    assert_eq!(a.len(), 9);
    let other = import_ids("other");
    let (mut x, mut y): (Vec<_>, Vec<_>) = (
        a.iter().map(|r| r.0.clone()).collect(),
        other.iter().map(|r| r.0.clone()).collect(),
    );
    x.sort();
    y.sort();
    assert!(x.iter().all(|i| !y.contains(i)));
    // Ids sort in created_at order.
    assert!(a.windows(2).all(|w| w[0].2 <= w[1].2), "{a:?}");
}

/// A small spike brain: one session per `(id, state)`, events as `(session, kind, text)` at
/// consecutive minutes.
fn small_brain(s: &Setup, sessions: &[(&str, &str)], events: &[(&str, &str, &str)]) {
    let c = Connection::open(&s.spike).unwrap();
    c.execute_batch(SCHEMA_SQL).unwrap();
    for (id, state) in sessions {
        c.execute(
            "INSERT INTO sessions (session_id, account, cwd, state, last_seen) \
             VALUES (?1, 'codex', '/tmp/x', ?2, '2026-11-15 10:00:00')",
            [id, state],
        )
        .unwrap();
    }
    for (i, (sid, kind, text)) in events.iter().enumerate() {
        c.execute(
            "INSERT INTO events (ts, session_id, cwd, kind, text) VALUES (?1, ?2, '/tmp/x', ?3, ?4)",
            [format!("2026-11-15 09:{i:02}:00"), sid.to_string(), kind.to_string(), text.to_string()],
        )
        .unwrap();
    }
}

#[test]
fn waiting_follows_the_spikes_rule() {
    let ping = "Claude is waiting for your input";
    let mut s = empty_setup();
    small_brain(
        &s,
        &[
            ("a-ping-only", "waiting"),
            ("b-permission-then-ping", "waiting"),
            ("c-ping-then-permission", "waiting"),
            ("d-stop-then-permission", "waiting"),
            ("e-permission-then-stop", "waiting"),
            ("f-no-events", "waiting"),
            ("g-idle-state", "idle"),
        ],
        &[
            ("a-ping-only", "prompt", "go"),
            ("a-ping-only", "waiting", ping),
            // The permission request is older than the ping: still needs permission.
            ("b-permission-then-ping", "prompt", "go"),
            ("b-permission-then-ping", "waiting", "May I run it?"),
            ("b-permission-then-ping", "waiting", ping),
            // The ping is older than the permission request.
            ("c-ping-then-permission", "waiting", ping),
            ("c-ping-then-permission", "waiting", "May I edit it?"),
            ("d-stop-then-permission", "stop", "Done."),
            ("d-stop-then-permission", "waiting", "May I push?"),
            ("e-permission-then-stop", "waiting", "May I push?"),
            ("e-permission-then-stop", "stop", "Done."),
            ("g-idle-state", "waiting", "May I?"),
        ],
    );
    import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap();
    let c = s.store.conn();
    let activity = |conv: &str| -> String { state(c, conv).1 };
    assert_eq!(activity("a-ping-only"), "idle");
    assert_eq!(activity("b-permission-then-ping"), "needs_permission");
    assert_eq!(activity("c-ping-then-permission"), "needs_permission");
    assert_eq!(activity("d-stop-then-permission"), "needs_permission");
    assert_eq!(activity("e-permission-then-stop"), "idle");
    assert_eq!(activity("f-no-events"), "idle");
    assert_eq!(activity("g-idle-state"), "idle");
    for conv in ["a-ping-only", "e-permission-then-stop", "f-no-events"] {
        assert_eq!(state(c, conv).0, "live");
    }
    let why = |conv: &str| -> Option<String> {
        c.query_row(
            "SELECT s.activity_why FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = ?1",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        why("b-permission-then-ping").as_deref(),
        Some("May I run it?")
    );
    assert_eq!(why("e-permission-then-stop").as_deref(), Some("Done."));
    // The latest counted event of g-idle-state is a waiting one, not a stop: no why, so
    // "asks you" can't be derived for it.
    assert_eq!(why("g-idle-state").as_deref(), Some(""));
    // Ping events are idle.ping, whatever the session state.
    assert_eq!(
        col::<i64>(c, "SELECT count(*) FROM events WHERE kind = 'idle.ping'"),
        3
    );
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM events WHERE kind = 'needs.permission'"
        ),
        5
    );
}

#[test]
fn why_follows_the_spikes_reading() {
    let mut s = empty_setup();
    small_brain(
        &s,
        &[
            ("w-working", "working"),
            ("q-question", "idle"),
            ("m-markdown", "idle"),
            ("c-command", "idle"),
            ("l-longtext", "idle"),
            ("p-prompt", "idle"),
        ],
        &[
            ("q-question", "stop", "First line\n**Shall I go on?**\n"),
            ("m-markdown", "stop", "text\n## Heading_ *"),
            ("c-command", "stop", "<command-name>x</command-name>"),
            ("l-longtext", "stop", "a\n\nb\nlast words  "),
            ("p-prompt", "prompt", "can you check X?"),
        ],
    );
    let c = Connection::open(&s.spike).unwrap();
    c.execute(
        "UPDATE sessions SET last_prompt = 'one\ntwo' WHERE session_id = 'w-working'",
        [],
    )
    .unwrap();
    drop(c);
    import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap();
    let c = s.store.conn();
    let why = |conv: &str| -> String {
        c.query_row(
            "SELECT s.activity_why FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = ?1",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(why("w-working"), "one two");
    // A question keeps its '#' and '?', stripped only of " *_".
    assert_eq!(why("q-question"), "Shall I go on?");
    assert_eq!(why("m-markdown"), "Heading");
    assert_eq!(why("c-command"), "");
    assert_eq!(why("l-longtext"), "last words");
    // A prompt cut off mid-turn is not a question for the user.
    assert_eq!(why("p-prompt"), "");
}

#[test]
fn awkward_brain_imports_with_warnings() {
    let mut s = setup(Scenario::Awkward);
    let report = run(&mut s);
    let c = s.store.conn();
    assert!(report.has_warnings());

    // Projects: the clashing name got a suffix.
    let names: Vec<String> = c
        .prepare("SELECT name FROM projects ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(&names[..2], ["Lantern", "lantern (2)"]);
    let groups: Vec<String> = c
        .prepare("SELECT path FROM groups")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(groups.iter().filter(|g| *g == "Private").count(), 1);
    assert!(groups.contains(&"A/B".to_string()));

    let has = |table: &str, key: &str, level: Level, start: &str| -> bool {
        report.notes.iter().any(|n| {
            n.table == table && n.key == key && n.level == level && n.reason.starts_with(start)
        })
    };
    // Sessions.
    assert!(has(
        "sessions",
        &ids::AWK_WEIRD_STATE[..8],
        Level::Warn,
        "unknown state; imported as ended"
    ));
    assert_eq!(
        state(c, ids::AWK_WEIRD_STATE),
        ("ended".into(), "idle".into(), "here".into())
    );
    assert!(has(
        "sessions",
        &ids::AWK_NO_ACCOUNT[..8],
        Level::Warn,
        "account unknown"
    ));
    let (kind, account): (String, Option<String>) = c
        .query_row(
            "SELECT s.agent_kind, s.account FROM sessions s JOIN conversations v \
             ON v.session_id = s.id WHERE v.conversation_id = ?1",
            [ids::AWK_NO_ACCOUNT],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), account), ("claude", None));
    assert!(has(
        "sessions",
        &ids::AWK_NO_FIRST_SEEN[..8],
        Level::Warn,
        "first_seen"
    ));
    // A NULL first_seen falls back to the earliest event.
    let created: i64 = col(
        c,
        &format!(
            "SELECT s.created_at FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = '{}'",
            ids::AWK_NO_FIRST_SEEN
        ),
    );
    assert_eq!(created, local("2026-11-14 09:30:00"));
    // The fold: 01:30 local on the day the clocks go back is the first, daylight-time, one.
    let fold: i64 = col(
        c,
        &format!(
            "SELECT at FROM events WHERE conversation_id = '{}'",
            ids::AWK_FOLD
        ),
    );
    assert_eq!(fold, 1_793_511_000_000, "05:30Z");
    // Conversations: an unsafe id, an account with no store.
    assert!(has(
        "conversations",
        &ids::AWK_PATH[..8],
        Level::Warn,
        "conversation id"
    ));
    assert!(has(
        "conversations",
        &ids::AWK_NO_STORE[..8],
        Level::Info,
        "transcript not checked"
    ));
    let (present, path): (Option<i64>, Option<String>) = c
        .query_row(
            "SELECT transcript_present, transcript_path FROM conversations WHERE conversation_id = ?1",
            [ids::AWK_NO_STORE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((present, path), (None, None));
    // The multi-line title is copied whole.
    let title: String = col(
        c,
        &format!(
            "SELECT title FROM sessions s JOIN conversations v ON v.session_id = s.id \
             WHERE v.conversation_id = '{}'",
            ids::AWK_PATH
        ),
    );
    assert_eq!(title, "名前 🌊\nsecond line");

    // Events: a NULL cwd gives NULL activity text in the spike, kept as "".
    let empty: i64 = col(c, "SELECT count(*) FROM activity WHERE text = ''");
    assert!(empty >= 1);

    // Tasks: unknown status, dates, dangling references.
    let task = |id: i64| -> (String, Option<String>, Option<String>, Option<i64>) {
        c.query_row(
            "SELECT status, due, session_id, project_id FROM tasks WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(task(1).0, "todo");
    assert!(has("tasks", "#1", Level::Warn, "status"));
    assert_eq!(task(2).1, None);
    assert!(has("tasks", "#2", Level::Warn, "due"));
    assert_eq!(task(3).1, None);
    assert!(
        !report
            .notes
            .iter()
            .any(|n| n.table == "tasks" && n.key == "#3")
    );
    assert_eq!(task(4).2, None);
    assert!(has("tasks", "#4", Level::Warn, "session_id"));
    assert_eq!(task(5).3, None);
    assert!(has("tasks", "#5", Level::Warn, "project_id"));

    // Suggestions: not usable as commands.
    for id in [1, 2] {
        let (actions, raw, invalid): (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT actions, actions_raw, actions_invalid FROM suggestions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(actions, "[]");
        assert!(raw.is_some());
        assert!(invalid.is_some());
        assert!(has("suggestions", &format!("#{id}"), Level::Warn, "action"));
    }
    assert_eq!(
        col::<String>(c, "SELECT actions_raw FROM suggestions WHERE id = 2"),
        "[\"rm -rf x\"]"
    );

    // The ask to an unknown session is kept as failed, with no target.
    let (status, to, from): (String, Option<String>, Option<String>) = c
        .query_row(
            "SELECT status, to_session, from_session FROM asks WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(to, None);
    assert!(from.is_some());
    assert!(has(
        "asks",
        "#1",
        Level::Warn,
        "target session isn't in this brain"
    ));

    for t in SESSION_TABLES {
        let n = count(&report, t);
        assert_eq!(n.source, n.imported, "{t}");
    }
    let text = report.render_text(false);
    for w in ["Lantern", "acct-a", "Odd task", "rm -rf", "Anyone there"] {
        assert!(!text.contains(w), "{w}");
    }
}

#[test]
fn old_brain_without_late_columns_imports() {
    let mut s = setup(Scenario::Old);
    let report = run(&mut s);
    let c = s.store.conn();
    for t in ["projects", "tasks", "sessions"] {
        let n = count(&report, t);
        assert_eq!((n.source, n.imported), (3, 3), "{t}");
    }
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM sessions WHERE group_path IS NOT NULL"
        ),
        0
    );
    assert_eq!(col::<i64>(c, "SELECT count(*) FROM spike_panes"), 0);
    assert_eq!(
        col::<i64>(
            c,
            "SELECT count(*) FROM tasks WHERE who IS NOT NULL OR due IS NOT NULL OR priority IS NOT NULL"
        ),
        0
    );
    // A missing late column is not a problem. The only warning is the session with no account.
    let w = warns(&report);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].table, "sessions");
    assert!(w[0].reason.starts_with("account unknown"));
    assert_eq!(w[0].key, &ids::OLD_3[..8]);
    assert_eq!(state(c, ids::OLD_3).2, "here");
    assert_eq!(state(c, ids::OLD_3).0, "live");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn import_never_fails(b in brain_any()) {
        let mut s = empty_setup();
        b.write(&s.spike, &s.home);
        let report = import_spike(&s.spike, &mut s.store, &env(&s.home, "default"));
        let report = report.expect("import must not fail");
        for t in SESSION_TABLES {
            let n = count(&report, t);
            prop_assert_eq!(n.source, n.imported, "{}", t);
        }
    }

    #[test]
    fn reports_are_contents_free(b in brain_any()) {
        let mut s = empty_setup();
        b.write(&s.spike, &s.home);
        let report = import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap();
        let text = report.render_text(false);
        let json = serde_json::to_string(&report).unwrap();
        let home = s.home.to_string_lossy().into_owned();
        for token in b.tokens() {
            prop_assert!(!text.contains(&token), "text holds a token");
            prop_assert!(!json.contains(&token), "json holds a token");
        }
        prop_assert!(!text.contains(&home) && !json.contains(&home));
    }
}

#[test]
fn unusable_session_ids_are_reported_not_fatal() {
    let mut s = empty_setup();
    small_brain(&s, &[("fine", "idle")], &[]);
    let c = Connection::open(&s.spike).unwrap();
    c.execute(
        "INSERT INTO sessions (session_id, state) VALUES (NULL, 'idle')",
        [],
    )
    .unwrap();
    // Two different blobs that both read back as the same text.
    for bad in [vec![0xffu8, b'x'], vec![0xfeu8, b'x']] {
        c.execute(
            "INSERT INTO sessions (session_id, state) VALUES (?1, 'idle')",
            [bad],
        )
        .unwrap();
    }
    drop(c);
    let report = import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap();
    let n = count(&report, "sessions");
    assert_eq!((n.source, n.imported), (4, 2));
    let reasons: Vec<&str> = warns(&report).iter().map(|n| n.reason).collect();
    assert!(reasons.contains(&"session_id is missing; not imported"));
    assert!(reasons.contains(&"session_id repeats another session; not imported"));
    assert!(warns(&report).iter().any(|n| n.key.starts_with('#')));
}

#[test]
fn invalid_utf8_and_wrong_typed_values_are_read_without_failing() {
    let mut s = empty_setup();
    small_brain(&s, &[], &[]);
    let c = Connection::open(&s.spike).unwrap();
    // The spike's own connections leave foreign keys off.
    c.pragma_update(None, "foreign_keys", false).unwrap();
    // A title cut off in the middle of a multi-byte character, and a project id that isn't a number.
    c.execute(
        "INSERT INTO sessions (session_id, account, title, project_id, state, first_seen) \
         VALUES ('cut', 'codex', CAST(x'41E697' AS TEXT), 'n/a', 'ended', '2026-11-15 09:00:00')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO tasks (id, title, project_id, session_id) VALUES (1, 'x', 'n/a', 7)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO events (ts, session_id, cwd, kind, text) \
         VALUES (1.5, 'cut', '/tmp', 'stop', CAST(x'E697' AS TEXT))",
        [],
    )
    .unwrap();
    drop(c);
    let report = import_spike(&s.spike, &mut s.store, &env(&s.home, "default")).unwrap();
    let db = s.store.conn();
    let title: String = col(db, "SELECT title FROM sessions");
    assert_eq!(title, "A\u{FFFD}");
    assert_eq!(
        col::<Option<i64>>(db, "SELECT project_id FROM sessions"),
        None
    );
    assert_eq!(col::<Option<i64>>(db, "SELECT project_id FROM tasks"), None);
    let has = |table: &str, reason: &str| {
        report
            .notes
            .iter()
            .any(|n| n.table == table && n.level == Level::Warn && n.reason == reason)
    };
    let lossy = "a text value isn't valid UTF-8; imported with replacement characters";
    assert!(has("sessions", lossy));
    assert!(has("events", lossy));
    assert!(has(
        "sessions",
        "project_id points at no project; imported as none"
    ));
    assert!(has(
        "tasks",
        "project_id points at no project; imported as none"
    ));
    assert!(has(
        "tasks",
        "session_id points at no session; imported as none"
    ));
    assert!(has(
        "events",
        "ts isn't a time; imported as the import time"
    ));
    for t in SESSION_TABLES {
        let n = count(&report, t);
        assert_eq!(n.source, n.imported, "{t}");
    }
    assert!(!report.render_text(false).contains('\u{FFFD}'));
    assert!(!serde_json::to_string(&report).unwrap().contains('\u{FFFD}'));
}

#[test]
fn a_session_with_no_time_at_all_keeps_its_id_between_runs() {
    let one_run = |now: UtcMs| -> (String, i64) {
        let mut s = empty_setup();
        let c = Connection::open(&s.spike).unwrap();
        c.execute_batch(SCHEMA_SQL).unwrap();
        c.execute(
            "INSERT INTO sessions (session_id, account, state) \
             VALUES ('timeless-conversation', 'acct-a', 'ended')",
            [],
        )
        .unwrap();
        drop(c);
        let mut e = env(&s.home, "default");
        e.now = now;
        let report = import_spike(&s.spike, &mut s.store, &e).unwrap();
        assert!(!warns(&report).is_empty());
        s.store
            .conn()
            .query_row("SELECT id, created_at FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    };
    let (id_a, created_a) = one_run(UtcMs(1_800_000_000_000));
    let (id_b, created_b) = one_run(UtcMs(1_800_000_999_000));
    assert_eq!(id_a, id_b);
    // created_at itself is still the moment of the import.
    assert_eq!(
        (created_a, created_b),
        (1_800_000_000_000, 1_800_000_999_000)
    );
}
