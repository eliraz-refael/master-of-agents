//! import -> export: what a brain looks like after a trip to v1 and back to the spike's schema.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::export::{ExportEnv, ExportError, ExportReport, export_spike};
use moa_store::import::report::ASK_SWITCHED_REPLY;
use moa_store::import::{IdSource, ImportEnv, Level, import_spike};
use moa_testkit::spike_fixture::SpikeBrain;
use moa_testkit::spike_gen::brain_valid;
use moa_testkit::{Scenario, SpikeFixture, ids};
use proptest::prelude::*;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

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
        now: NOW,
        brain: "default".into(),
        accounts: AccountMap::new(home.clone()),
        ids: IdSource::Derived,
        transcript: find_transcript,
    };
    import_spike(&spike, &mut store, &env).unwrap();
    let out = dir.path().join("out.db");
    Rig {
        _dir: dir,
        home,
        spike,
        out,
        store,
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

fn open_ro(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

type Table = (Vec<String>, Vec<Vec<Value>>);

fn dump(c: &Connection, table: &str, order: &str) -> Table {
    let mut stmt = c
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
        .unwrap();
    let cols: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
    let n = cols.len();
    let rows = stmt
        .query_map([], |r| {
            (0..n)
                .map(|i| r.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    (cols, rows)
}

/// The expected differences, each named: `(table, primary key, column, expected value)`.
type Exceptions = Vec<(&'static str, Value, &'static str, Value)>;

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn apply(t: &mut Table, table: &str, pk_col: &str, exceptions: &Exceptions) {
    let pk = t.0.iter().position(|c| c == pk_col).unwrap();
    for (tbl, key, col, new) in exceptions {
        if *tbl != table {
            continue;
        }
        let ci = t.0.iter().position(|c| c == col).unwrap();
        let row = t.1.iter_mut().find(|r| r[pk] == *key).unwrap_or_else(|| {
            panic!("exception names a row that is not in {table}");
        });
        row[ci] = new.clone();
    }
}

/// Suggestion actions compare as lists of word lists, not as text.
fn normalise_actions(t: &mut Table) {
    let ci = t.0.iter().position(|c| c == "action").unwrap();
    for row in &mut t.1 {
        if let Value::Text(raw) = &row[ci] {
            row[ci] = match serde_json::from_str::<Vec<String>>(raw) {
                Ok(lines) => {
                    let split: Vec<Vec<String>> = lines
                        .iter()
                        .map(|l| shell_words::split(l).unwrap())
                        .collect();
                    Value::Text(serde_json::to_string(&split).unwrap())
                }
                Err(_) => Value::Text(raw.clone()),
            };
        }
    }
}

fn json(c: &Connection, key: &str) -> Option<serde_json::Value> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .map(|v| serde_json::from_str(&v).unwrap())
}

/// Compares the export with the spike brain it came from, every column of every table,
/// except for the differences in `exceptions`, `groups` and `meta`, which have their own
/// rules below.
fn assert_round_trip(rig: &Rig, exceptions: &Exceptions) {
    rig.export().unwrap();
    let (a, b) = (open_ro(&rig.spike), open_ro(&rig.out));
    for (table, pk, order) in [
        ("projects", "id", "id"),
        ("tasks", "id", "id"),
        ("notes", "id", "id"),
        ("suggestions", "id", "id"),
        ("sessions", "session_id", "session_id"),
        ("events", "id", "id"),
        ("asks", "id", "id"),
        ("activity", "id", "id"),
    ] {
        let mut want = dump(&a, table, order);
        apply(&mut want, table, pk, exceptions);
        if table == "sessions" {
            // The spike reads `tmux` only together with a matching pane, and moactl.py clears
            // `pane` on other sessions when a pane is reused but leaves their `tmux`. So tmux
            // is compared only where the source pane is set; elsewhere the export's is NULL.
            let (pane, tmux) = (
                want.0.iter().position(|c| c == "pane").unwrap(),
                want.0.iter().position(|c| c == "tmux").unwrap(),
            );
            for row in &mut want.1 {
                if row[pane] == Value::Null {
                    row[tmux] = Value::Null;
                }
            }
        }
        let mut got = dump(&b, table, order);
        if table == "suggestions" {
            normalise_actions(&mut want);
            normalise_actions(&mut got);
        }
        assert_eq!(want.0, got.0, "{table}: columns");
        for (w, g) in want.1.iter().zip(&got.1) {
            for (i, col) in want.0.iter().enumerate() {
                assert_eq!(w[i], g[i], "{table} {:?} column {col}", w[0]);
            }
        }
        assert_eq!(want.1.len(), got.1.len(), "{table}: row count");
    }

    // groups: every source group is there as it was; the export adds the groups that only a
    // session's grp named.
    let src_groups = dump(&a, "groups", "path").1;
    let out_groups = dump(&b, "groups", "path").1;
    for g in &src_groups {
        assert!(out_groups.contains(g), "a source group is missing");
    }
    let mut expected: BTreeSet<String> = src_groups
        .iter()
        .map(|g| match &g[0] {
            Value::Text(t) => t.clone(),
            _ => unreachable!(),
        })
        .collect();
    let mut stmt = a
        .prepare("SELECT DISTINCT grp FROM sessions WHERE grp IS NOT NULL AND grp != ''")
        .unwrap();
    expected.extend(
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap),
    );
    let got: BTreeSet<String> = out_groups
        .iter()
        .map(|g| match &g[0] {
            Value::Text(t) => t.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(got, expected, "groups");

    // meta: the cursor is the newest exported event; the others are the same JSON.
    let max_event: Option<i64> = b
        .query_row("SELECT max(id) FROM events", [], |r| r.get(0))
        .unwrap();
    let cursor: Option<String> = b
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .ok();
    assert_eq!(cursor, max_event.map(|m| m.to_string()), "meta cursor");
    for key in ["console_thread", "setup_skipped"] {
        assert_eq!(json(&a, key), json(&b, key), "meta {key}");
    }
    let keys = |c: &Connection| -> BTreeSet<String> {
        c.prepare("SELECT key FROM meta")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(keys(&a), keys(&b), "meta keys");
}

#[test]
fn typical_round_trips() {
    let rig = typical();
    // The known differences for the typical brain:
    let exceptions: Exceptions = vec![
        // The spike says `waiting`, but its newest event is the idle ping, so import reads it
        // as idle and export writes `idle`.
        (
            "sessions",
            text(ids::S9_WAITING_IDLE),
            "state",
            text("idle"),
        ),
        // Import drops the stale asleep mark of an archived session.
        ("sessions", text(ids::S6_ARCHIVED), "asleep_at", Value::Null),
        // Import closes asks nobody waits for: a pending one is dropped, one in flight fails
        // with a reply that says why.
        ("asks", Value::Integer(2), "status", text("dropped")),
        ("asks", Value::Integer(3), "status", text("failed")),
        ("asks", Value::Integer(3), "reply", text(ASK_SWITCHED_REPLY)),
    ];
    assert_round_trip(&rig, &exceptions);

    // The pane links survive, so the spike never starts a second copy of a running agent.
    let out = open_ro(&rig.out);
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn valid_brains_round_trip(b in brain_valid()) {
        let b: SpikeBrain = b;
        let rig = rig_with(|spike, home| b.write(spike, home));
        assert_round_trip(&rig, &Vec::new());
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
