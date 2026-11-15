//! import -> export: what a brain looks like after a trip to v1 and back to the spike's schema.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::export::{ExportEnv, ExportError, ExportReport, export_spike};
use moa_store::import::report::ASK_SWITCHED_REPLY;
use moa_store::import::{IdSource, ImportEnv, Level, RecordNote, import_spike};
use moa_testkit::spike_fixture::SpikeBrain;
use moa_testkit::spike_gen::brain_valid;
use moa_testkit::{Scenario, SpikeFixture, ids};
use proptest::prelude::*;
use rusqlite::Connection;

use common::{TimeMode, open_ro};

const NOW: UtcMs = UtcMs(1_800_000_000_000);

/// A zone without daylight saving, so the activity rows the spike's triggers stamp with the
/// machine's clock can never fall in a gap.
fn tz() -> TimeZone {
    TimeZone::posix("EST5").unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    home: PathBuf,
    spike: PathBuf,
    out: PathBuf,
    store: Store,
    notes: Vec<RecordNote>,
}

fn rig_with(write: impl FnOnce(&Path, &Path)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("spike.db");
    write(&spike, &home);
    let mut store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    let env = ImportEnv {
        tz: tz(),
        tz_spec: None,
        now: NOW,
        brain: "default".into(),
        accounts: AccountMap::new(home.clone()),
        ids: IdSource::Derived,
        transcript: find_transcript,
    };
    let notes = import_spike(&spike, &mut store, &env).unwrap().notes;
    let out = dir.path().join("out.db");
    Rig {
        _dir: dir,
        home,
        spike,
        out,
        store,
        notes,
    }
}

fn typical() -> Rig {
    rig_with(|spike, home| SpikeFixture::write(spike, Scenario::Typical, home).unwrap())
}

fn export_env(home: &Path) -> ExportEnv {
    ExportEnv {
        tz: tz(),
        accounts: AccountMap::new(home.to_path_buf()),
        setup_skipped: None,
    }
}

impl Rig {
    fn export(&self) -> Result<ExportReport, ExportError> {
        export_spike(&self.store, &self.out, &export_env(&self.home))
    }
}

fn assert_round_trip(rig: &Rig) {
    rig.export().unwrap();
    let excluded = common::assert_round_trip(&rig.spike, &rig.out, &rig.notes, &TimeMode::Text);
    assert_eq!(excluded, 0, "no spike-table record may be left out");
}

#[test]
fn typical_round_trips() {
    let rig = typical();
    assert_round_trip(&rig);

    // The known differences for the typical brain, each asserted on its own.
    let out = open_ro(&rig.out);
    let one = |sql: &str| -> Option<String> { out.query_row(sql, [], |r| r.get(0)).unwrap() };
    // The spike says `waiting`, but its newest event is the idle ping: idle.
    assert_eq!(
        one(&format!(
            "SELECT state FROM sessions WHERE session_id = '{}'",
            ids::S9_WAITING_IDLE
        )),
        Some("idle".into())
    );
    // Import drops the stale asleep mark of an archived session.
    assert_eq!(
        one(&format!(
            "SELECT asleep_at FROM sessions WHERE session_id = '{}'",
            ids::S6_ARCHIVED
        )),
        None
    );
    // A pending ask is dropped; one in flight fails with a reply that says why.
    assert_eq!(
        one("SELECT status FROM asks WHERE id = 2"),
        Some("dropped".into())
    );
    assert_eq!(
        one("SELECT status FROM asks WHERE id = 3"),
        Some("failed".into())
    );
    assert_eq!(
        one("SELECT reply FROM asks WHERE id = 3"),
        Some(ASK_SWITCHED_REPLY.into())
    );

    // The pane links survive, so the spike never starts a second copy of a running agent.
    let pane = |sid: &str| -> (Option<String>, Option<String>) {
        out.query_row(
            "SELECT pane, tmux FROM sessions WHERE session_id = ?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        pane(ids::S1_WORKING),
        (Some("%3".into()), Some("/tmp/x/moa".into()))
    );
    assert_eq!(
        pane(ids::S2_IDLE),
        (Some("%4".into()), Some("/tmp/x/moa".into()))
    );
    // Sessions come back with the exact strings the spike had: S2 by bare name, S1 by path.
    // Events carry the account the way `track.sh` writes it, the store folder's bare basename,
    // whatever string their session uses (S1's events say `acct-a`, its session a path).
    let account = |sql: &str| -> Option<String> { out.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        account(&format!(
            "SELECT account FROM sessions WHERE session_id = '{}'",
            ids::S2_IDLE
        )),
        Some("acct-b".into())
    );
    assert_eq!(
        account(&format!(
            "SELECT account FROM sessions WHERE session_id = '{}'",
            ids::S1_WORKING
        )),
        Some(rig.home.join(".acct-a").to_string_lossy().into_owned())
    );
    assert_eq!(
        account(&format!(
            "SELECT account FROM events WHERE session_id = '{}' AND kind = 'start'",
            ids::S2_IDLE
        )),
        Some("acct-b".into())
    );
    assert_eq!(
        account(&format!(
            "SELECT account FROM events WHERE session_id = '{}' AND kind = 'start'",
            ids::S1_WORKING
        )),
        Some("acct-a".into())
    );
    // The groups are exactly these two.
    let groups: Vec<String> = out
        .prepare("SELECT path FROM groups ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(groups, ["Private", "Private/Lantern"]);
    let cursor: String = out
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(cursor, "19");
}

#[test]
fn typical_report_names_records_not_contents() {
    let rig = typical();
    let report = rig.export().unwrap();
    // The two live sessions without a pane are named by their 8-character prefix.
    let no_pane: Vec<&str> = report
        .notes
        .iter()
        .filter(|n| n.level == Level::Warn && n.table == "sessions")
        .map(|n| n.key.as_str())
        .collect();
    assert_eq!(no_pane.len(), 2, "{no_pane:?}");
    assert!(no_pane.contains(&&ids::S3_WAITING[..8]));
    assert!(no_pane.contains(&&ids::S9_WAITING_IDLE[..8]));
    let sessions = report
        .tables
        .iter()
        .find(|t| t.table == "sessions")
        .unwrap();
    assert_eq!((sessions.source, sessions.imported), (9, 9));
    let text = report.render_text();
    for secret in [
        "Wire up the dimmer",
        "Lantern",
        "acct-a",
        ids::S1_WORKING,
        "Pin 7",
    ] {
        assert!(!text.contains(secret), "the report shows {secret}");
    }
}

#[test]
fn a_title_with_invalid_utf8_still_compares() {
    let mut b = SpikeFixture::brain(Scenario::Typical);
    let (_, row) = b.rows.iter_mut().find(|(t, _)| *t == "sessions").unwrap();
    let (_, v) = row.iter_mut().find(|(c, _)| *c == "title").unwrap();
    // A title cut in the middle of a three-byte character.
    *v = rusqlite::types::Value::Blob(vec![b'a', 0xE2, 0x82]);
    let rig = rig_with(|spike, home| b.write(spike, home));
    rig.export().unwrap();
    let excluded = common::assert_round_trip(&rig.spike, &rig.out, &rig.notes, &TimeMode::Text);
    assert_eq!(excluded, 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn valid_brains_round_trip(b in brain_valid()) {
        let b: SpikeBrain = b;
        let rig = rig_with(|spike, home| b.write(spike, home));
        assert_round_trip(&rig);
    }
}

#[test]
fn bindings_win_over_spike_panes() {
    let rig = typical();
    let sid: String = rig
        .store
        .conn()
        .query_row(
            "SELECT session_id FROM conversations WHERE conversation_id = ?1",
            [ids::S1_WORKING],
            |r| r.get(0),
        )
        .unwrap();
    rig.store
        .conn()
        .execute(
            "INSERT INTO pane_bindings (session_id, server_pid, server_start, socket, pane_id, \
             pane_pid, bound_at) VALUES (?1, 1, 2, '/tmp/y/moa', '%9', 3, 4)",
            [&sid],
        )
        .unwrap();
    rig.export().unwrap();
    let out = open_ro(&rig.out);
    let pane = |conv: &str| -> (Option<String>, Option<String>) {
        out.query_row(
            "SELECT pane, tmux FROM sessions WHERE session_id = ?1",
            [conv],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        pane(ids::S1_WORKING),
        (Some("%9".into()), Some("/tmp/y/moa".into()))
    );
    assert_eq!(
        pane(ids::S2_IDLE),
        (Some("%4".into()), Some("/tmp/x/moa".into()))
    );
}

#[test]
fn export_follows_rotation() {
    let rig = typical();
    let sid: String = rig
        .store
        .conn()
        .query_row(
            "SELECT session_id FROM conversations WHERE conversation_id = ?1",
            [ids::S1_WORKING],
            |r| r.get(0),
        )
        .unwrap();
    rig.store
        .conn()
        .execute(
            "INSERT INTO conversations (agent_kind, conversation_id, session_id, store_dir, \
             spike_account, reason, first_seen_at, last_seen_at) \
             VALUES ('claude', 'rotated-conv', ?1, NULL, NULL, 'clear', ?2, ?2)",
            rusqlite::params![sid, NOW.0],
        )
        .unwrap();
    rig.export().unwrap();
    let out = open_ro(&rig.out);
    let one = |sql: &str| -> String { out.query_row(sql, [], |r| r.get(0)).unwrap() };
    let count = |sql: &str| -> i64 { out.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT count(*) FROM sessions WHERE session_id = 'rotated-conv'"),
        1
    );
    assert_eq!(
        count(&format!(
            "SELECT count(*) FROM sessions WHERE session_id = '{}'",
            ids::S1_WORKING
        )),
        0
    );
    assert_eq!(
        one("SELECT session_id FROM tasks WHERE id = 1"),
        "rotated-conv"
    );
    assert_eq!(one("SELECT to_sid FROM asks WHERE id = 1"), "rotated-conv");
    // Old events keep the conversation they happened in.
    assert_eq!(
        count(&format!(
            "SELECT count(*) FROM events WHERE session_id = '{}'",
            ids::S1_WORKING
        )),
        2
    );
    // The rotated conversation has no account of its own, so the session's account comes back
    // as the store path the spike can launch.
    let account: String = out
        .query_row(
            "SELECT account FROM sessions WHERE session_id = 'rotated-conv'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(account, rig.home.join(".acct-a").to_string_lossy());
}

#[test]
fn export_reads_one_snapshot() {
    let rig = typical();
    let db = rig._dir.path().join("moa.db");
    let count = |table: &str| -> i64 {
        rig.store
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    let (projects, tasks, events) = (count("projects"), count("tasks"), count("events"));
    let max_event: i64 = rig
        .store
        .conn()
        .query_row("SELECT max(id) FROM events", [], |r| r.get(0))
        .unwrap();

    let mut reader = Store::open_read_only(&db).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    let writer_path = db.clone();
    // Mid-export, another connection (the daemon) writes a project, a task and an event.
    // The fixture is small and the count is per statement, so the handler runs every 20 ops.
    reader
        .conn_mut()
        .progress_handler(
            20,
            Some(move || {
                if !flag.swap(true, Ordering::SeqCst) {
                    let w = Connection::open(&writer_path).unwrap();
                    w.execute_batch(&format!(
                        "INSERT INTO projects (name, created_at, updated_at) VALUES ('Beacon', 0, 0); \
                         INSERT INTO tasks (title, created_at, updated_at) VALUES ('late', 0, 0); \
                         INSERT INTO events (id, at, kind) VALUES ({}, 0, 'turn.failed');",
                        max_event + 1
                    ))
                    .unwrap();
                }
                false
            }),
        )
        .unwrap();
    export_spike(&reader, &rig.out, &export_env(&rig.home)).unwrap();
    assert!(
        fired.load(Ordering::SeqCst),
        "the write must happen mid-export"
    );

    let out = open_ro(&rig.out);
    let out_count = |table: &str| -> i64 {
        out.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(out_count("projects"), projects);
    assert_eq!(out_count("tasks"), tasks);
    assert_eq!(out_count("events"), events);
    let cursor: String = out
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(cursor, max_event.to_string());

    // The write did land in the source.
    assert_eq!(count("projects"), projects + 1);
    assert_eq!(count("tasks"), tasks + 1);
    assert_eq!(count("events"), events + 1);
}

#[test]
fn cursor_is_the_newest_exported_event() {
    let rig = typical();
    rig.store
        .conn()
        .execute(
            "INSERT INTO events (id, at, session_id, conversation_id, kind) \
             VALUES (100, ?1, NULL, NULL, 'turn.failed')",
            [NOW.0],
        )
        .unwrap();
    let report = rig.export().unwrap();
    let out = open_ro(&rig.out);
    let cursor: String = out
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(cursor, "19");
    let events = report.tables.iter().find(|t| t.table == "events").unwrap();
    assert_eq!((events.source, events.imported), (20, 19));
    // One note for the kind that has no spike equivalent, and no more.
    let skipped: Vec<_> = report
        .notes
        .iter()
        .filter(|n| n.table == "events")
        .collect();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].level, Level::Info);
    assert_eq!(skipped[0].key, "#100");
}

#[test]
fn triggers_exist_and_activity_is_not_duplicated() {
    let rig = typical();
    let v1_activity: i64 = rig
        .store
        .conn()
        .query_row("SELECT count(*) FROM activity", [], |r| r.get(0))
        .unwrap();
    rig.export().unwrap();
    let out = Connection::open(&rig.out).unwrap();
    let count = |sql: &str| -> i64 { out.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT count(*) FROM sqlite_master WHERE type = 'trigger'"),
        9
    );
    assert_eq!(count("SELECT count(*) FROM activity"), v1_activity);
    // `track.sh` writes without the Python setup: the triggers fire on their own.
    out.execute(
        "INSERT INTO events (session_id, account, cwd, kind, text) \
         VALUES ('x', NULL, '/a/b', 'prompt', 'hello')",
        [],
    )
    .unwrap();
    assert_eq!(count("SELECT count(*) FROM activity"), v1_activity + 1);
}

#[test]
fn export_refuses_existing_targets_and_leftovers() {
    let rig = typical();
    let dir = rig.out.parent().unwrap().to_path_buf();
    let snapshot = |d: &Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = fs::read_dir(d)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let p = e.path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    if p.is_file() {
                        fs::read(&p).unwrap()
                    } else {
                        vec![]
                    },
                )
            })
            .collect();
        v.sort();
        v
    };
    let try_export = |target: &Path| {
        let before = snapshot(&dir);
        let err = export_spike(&rig.store, target, &export_env(&rig.home)).unwrap_err();
        assert!(matches!(err, ExportError::Exists(_)), "{err}");
        assert_eq!(before, snapshot(&dir), "nothing changed");
    };

    // dst exists
    fs::write(&rig.out, b"mine").unwrap();
    try_export(&rig.out);
    fs::remove_file(&rig.out).unwrap();
    // each leftover beside it
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = rig.out.as_os_str().to_owned();
        name.push(suffix);
        let leftover = PathBuf::from(name);
        fs::write(&leftover, b"left").unwrap();
        try_export(&rig.out);
        fs::remove_file(&leftover).unwrap();
    }
    // a target that is itself named like a leftover
    for suffix in ["-wal", "-shm", "-journal"] {
        try_export(&dir.join(format!("x.db{suffix}")));
    }
    // and with nothing in the way it works
    rig.export().unwrap();
}

#[test]
fn export_refuses_a_missing_folder() {
    let rig = typical();
    let target = rig.out.parent().unwrap().join("nowhere/out.db");
    let err = export_spike(&rig.store, &target, &export_env(&rig.home)).unwrap_err();
    assert!(matches!(err, ExportError::Io(_)), "{err}");
    assert!(!target.parent().unwrap().exists());
}

#[test]
fn export_leaves_no_wal() {
    let rig = typical();
    rig.export().unwrap();
    let mut wal = rig.out.as_os_str().to_owned();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    assert!(!wal.exists() || fs::metadata(&wal).unwrap().len() == 0);
    // The whole brain is in the main file.
    let n: i64 = open_ro(&rig.out)
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 9);
}

fn session_of(rig: &Rig, conv: &str) -> String {
    rig.store
        .conn()
        .query_row(
            "SELECT session_id FROM conversations WHERE conversation_id = ?1",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
}

/// Adds a suggestion whose one action is `set <arg>`, exports, and returns the exported
/// argument.
fn exported_arg(rig: &Rig, id: i64, arg: &str) -> String {
    let actions = serde_json::json!([{ "verb": "set", "args": [arg, "--title", "x"] }]);
    rig.store
        .conn()
        .execute(
            "INSERT INTO suggestions (id, text, actions, created_at) VALUES (?1, 't', ?2, 1)",
            rusqlite::params![id, actions.to_string()],
        )
        .unwrap();
    let out = rig.out.with_file_name(format!("out{id}.db"));
    export_spike(&rig.store, &out, &export_env(&rig.home)).unwrap();
    let action: String = open_ro(&out)
        .query_row("SELECT action FROM suggestions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    let lines: Vec<String> = serde_json::from_str(&action).unwrap();
    shell_words::split(&lines[0]).unwrap().remove(1)
}

#[test]
fn suggestion_args_are_rewritten_only_when_unambiguous() {
    let rig = typical();
    let s1 = session_of(&rig, ids::S1_WORKING);

    // A unique prefix of a v1 session id, 8 characters or more: the session's conversation.
    assert_eq!(exported_arg(&rig, 10, &s1[..12]), ids::S1_WORKING);
    // Under 8 characters: left alone.
    assert_eq!(exported_arg(&rig, 11, &s1[..7]), &s1[..7]);

    // Also a prefix of a conversation id: left alone (a spike prefix passes through).
    let other = session_of(&rig, ids::S2_IDLE);
    rig.store
        .conn()
        .execute(
            "INSERT INTO conversations (agent_kind, conversation_id, session_id, reason, \
             first_seen_at, last_seen_at) VALUES ('claude', ?1, ?2, 'resume', 1, 1)",
            rusqlite::params![format!("{}-x", &s1[..12]), other],
        )
        .unwrap();
    assert_eq!(exported_arg(&rig, 12, &s1[..12]), &s1[..12]);
}

#[test]
fn an_ambiguous_prefix_is_left_alone_even_for_a_session_with_no_conversation() {
    let rig = typical();
    let s1 = session_of(&rig, ids::S1_WORKING);
    // A twin of S1 that has no conversation, so it is never exported but is still a v1 session.
    let twin = format!("{}{}", &s1[..35], if s1.ends_with('0') { '1' } else { '0' });
    rig.store
        .conn()
        .execute(
            "INSERT INTO sessions (id, agent_kind, lifecycle, created_at) \
             VALUES (?1, 'claude', 'ended', 1)",
            [&twin],
        )
        .unwrap();
    assert_eq!(exported_arg(&rig, 10, &s1[..20]), &s1[..20]);
    // The full id still names exactly one.
    assert_eq!(exported_arg(&rig, 11, &s1), ids::S1_WORKING);
}

#[test]
fn a_session_with_no_conversation_is_named_by_rowid() {
    let rig = typical();
    rig.store
        .conn()
        .execute(
            "INSERT INTO sessions (id, agent_kind, lifecycle, created_at) \
             VALUES ('00000000-0000-7000-8000-00000000ffff', 'claude', 'ended', 1)",
            [],
        )
        .unwrap();
    let rowid: i64 = rig
        .store
        .conn()
        .query_row("SELECT max(rowid) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let report = rig.export().unwrap();
    let warn: Vec<_> = report
        .notes
        .iter()
        .filter(|n| n.level == Level::Warn && n.table == "sessions" && n.key.starts_with('#'))
        .collect();
    assert_eq!(warn.len(), 1);
    assert_eq!(warn[0].key, format!("#{rowid}"));
}

#[test]
fn spike_only_event_kinds_come_back_as_written() {
    let rig = typical();
    let conn = rig.store.conn();
    // What import keeps for a kind it doesn't know (`compact`), and a dotted v1 kind.
    for (id, kind) in [(50, "compact"), (51, "model.changed")] {
        conn.execute(
            "INSERT INTO events (id, at, session_id, conversation_id, cwd, kind, text) \
             VALUES (?1, ?2, NULL, NULL, '/a', ?3, 'hello')",
            rusqlite::params![id, NOW.0, kind],
        )
        .unwrap();
    }
    let report = rig.export().unwrap();
    let out = open_ro(&rig.out);
    let kind = |id: i64| -> Option<String> {
        out.query_row("SELECT kind FROM events WHERE id = ?1", [id], |r| r.get(0))
            .ok()
    };
    assert_eq!(kind(50), Some("compact".into()));
    // A dotted kind with no spike equivalent is skipped, with one Info.
    assert_eq!(kind(51), None);
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|n| n.table == "events" && n.level == Level::Info)
            .count(),
        1
    );
    // The cursor follows the newest event actually exported.
    let cursor: String = out
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(cursor, "50");
}

/// The panic text of a comparison that is expected to fail.
fn mismatch_message(rig: &Rig, mode: &TimeMode) -> String {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        common::assert_round_trip(&rig.spike, &rig.out, &rig.notes, mode);
    }))
    .expect_err("the comparison should have failed");
    caught
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| caught.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_default()
}

#[test]
fn a_real_brain_mismatch_names_no_contents() {
    let rig = typical();
    rig.export().unwrap();
    let src = open_ro(&rig.spike);
    let (task_title, session_title): (String, String) = (
        src.query_row("SELECT title FROM tasks WHERE id = 1", [], |r| r.get(0))
            .unwrap(),
        src.query_row(
            "SELECT title FROM sessions WHERE session_id = ?1",
            [ids::S1_WORKING],
            |r| r.get(0),
        )
        .unwrap(),
    );
    let planted = "PLANTED-VALUE-XYZ";
    let out = Connection::open(&rig.out).unwrap();

    out.execute(
        "UPDATE sessions SET title = ?1 WHERE session_id = ?2",
        [planted, ids::S1_WORKING],
    )
    .unwrap();
    let instant = TimeMode::Instant(tz());
    let msg = mismatch_message(&rig, &instant);
    assert_eq!(
        msg,
        format!("sessions {} column title differs", &ids::S1_WORKING[..8])
    );
    assert!(
        !msg.contains(planted) && !msg.contains(&session_title),
        "{msg}"
    );
    // Fixtures and generated brains keep the values in the message.
    let msg = mismatch_message(&rig, &TimeMode::Text);
    assert!(msg.contains(planted), "{msg}");

    out.execute("UPDATE tasks SET title = ?1 WHERE id = 1", [planted])
        .unwrap();
    out.execute(
        "UPDATE sessions SET title = ?1 WHERE session_id = ?2",
        [&session_title, ids::S1_WORKING],
    )
    .unwrap();
    let msg = mismatch_message(&rig, &instant);
    assert_eq!(msg, "tasks #1 column title differs");
    assert!(
        !msg.contains(planted) && !msg.contains(&task_title),
        "{msg}"
    );
}

#[test]
fn a_blank_reference_is_no_link_and_no_warning() {
    // The spike writes '' on purpose to unlink a task's session.
    let rig = rig_with(|spike, home| {
        SpikeFixture::write(spike, Scenario::Typical, home).unwrap();
        let c = Connection::open(spike).unwrap();
        let tasks = c
            .execute(
                "UPDATE tasks SET session_id = '' WHERE session_id IS NOT NULL",
                [],
            )
            .unwrap();
        let asks = c
            .execute(
                "UPDATE asks SET from_sid = '' WHERE from_sid IS NOT NULL",
                [],
            )
            .unwrap();
        assert!(tasks > 0 && asks > 0, "the fixture has links to blank");
    });
    let warns: Vec<_> = rig
        .notes
        .iter()
        .filter(|n| n.level == Level::Warn && matches!(n.table, "tasks" | "asks"))
        .collect();
    assert!(warns.is_empty(), "{warns:?}");
    assert_round_trip(&rig);
    let out = open_ro(&rig.out);
    let linked: i64 = out
        .query_row(
            "SELECT count(*) FROM tasks WHERE session_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, 0);
}
