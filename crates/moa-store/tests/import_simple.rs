use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_core::time::spike_local_to_utc;
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::import::{
    IdSource, ImportEnv, ImportError, ImportReport, Level, import_spike, snapshot_spike,
};
use moa_testkit::{Scenario, SpikeFixture};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

const NOW: UtcMs = UtcMs(1_800_000_000_000);

fn tz() -> TimeZone {
    TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()
}

fn env(home: &Path) -> ImportEnv {
    ImportEnv {
        tz: tz(),
        now: NOW,
        brain: "test-brain".into(),
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

fn setup(scenario: Scenario) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("spike.db");
    SpikeFixture::write(&spike, scenario, &home).unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    Setup {
        _dir: dir,
        home,
        spike,
        store,
    }
}

fn local(text: &str) -> i64 {
    spike_local_to_utc(text, &tz()).unwrap().0
}

fn hash(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| format!("{:x}", Sha256::digest(b)))
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn snapshot_sees_wal_rows_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike_dir = dir.path().join("spike");
    fs::create_dir(&spike_dir).unwrap();
    let spike = spike_dir.join("brain.db");
    let writer = SpikeFixture::write_holding_wal(&spike, Scenario::Typical, &home).unwrap();
    for i in 0..50 {
        writer
            .execute(
                "INSERT INTO projects (name) VALUES (?1)",
                [format!("extra {i}")],
            )
            .unwrap();
    }
    let wal = spike_dir.join("brain.db-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let before = (hash(&spike), hash(&wal), names(&spike_dir));

    let snap = dir.path().join("snap.db");
    snapshot_spike(&spike, &snap).unwrap();

    let after = (hash(&spike), hash(&wal), names(&spike_dir));
    assert_eq!(before.0, after.0, "main file changed");
    assert_eq!(before.1, after.1, "wal changed");
    for n in &after.2 {
        assert!(
            before.2.contains(n) || n.ends_with("-wal") || n.ends_with("-shm"),
            "new file {n}"
        );
    }
    let c = Connection::open(&snap).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 53);
    drop(writer);

    let gone = dir.path().join("nothing.db");
    assert!(matches!(
        snapshot_spike(&gone, &dir.path().join("s2.db")),
        Err(ImportError::SpikeMissing(_))
    ));
    assert!(!gone.exists());
}

fn run(s: &mut Setup) -> ImportReport {
    import_spike(&s.spike, &mut s.store, &env(&s.home)).unwrap()
}

#[test]
fn simple_tables_import() {
    let mut s = setup(Scenario::Typical);
    let report = run(&mut s);
    let c = s.store.conn();

    type P = (i64, String, String, Option<String>, i64, i64);
    let projects: Vec<P> = c
        .prepare("SELECT id, name, status, last_activity_on, created_at, updated_at FROM projects ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        projects,
        vec![
            (
                1,
                "Lantern".into(),
                "active".into(),
                Some("2026-11-15".into()),
                local("2026-11-02 08:00:00"),
                local("2026-11-15 09:00:00")
            ),
            (
                2,
                "Tidepool".into(),
                "idea".into(),
                None,
                local("2026-11-05 10:30:00"),
                local("2026-11-05 10:30:00")
            ),
            (
                3,
                "Driftwood".into(),
                "paused".into(),
                Some("2026-10-20".into()),
                local("2026-10-01 18:00:00"),
                local("2026-10-20 18:00:00")
            ),
        ]
    );

    let notes: i64 = c
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(notes, 4);
    let milestone: i64 = c
        .query_row(
            "SELECT count(*) FROM notes WHERE kind = 'milestone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(milestone >= 1);

    let groups: Vec<String> = c
        .prepare("SELECT path FROM groups ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(groups, ["Private", "Private/Lantern"]);
    let collapsed: i64 = c
        .query_row(
            "SELECT count(*) FROM groups WHERE collapsed != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(collapsed, 0);

    let src =
        Connection::open_with_flags(&s.spike, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let ids = |c: &Connection| -> Vec<i64> {
        c.prepare("SELECT id FROM activity ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(ids(c), ids(&src));
    assert!(!ids(c).is_empty());

    let th: Vec<(String, String, String, String, i64)> = c
        .prepare("SELECT thread_id, account, day, kind, started_at FROM moa_threads")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        th,
        vec![(
            moa_testkit::ids::S1_WORKING.to_string(),
            "acct-b".to_string(),
            "2026-11-15".to_string(),
            "daily".to_string(),
            local("2026-11-15 00:00:00")
        )]
    );

    assert_eq!(report.setup_skipped, ["acct-c"]);
    assert_eq!(report.accounts, ["acct-b"]);
    assert!(report.notes.iter().any(|n| {
        n.table == "meta"
            && n.level == Level::Info
            && n.reason == "meta cursor not carried (v1 has one channel)"
    }));
    assert!(!report.has_warnings());

    let json: String = c
        .query_row(
            "SELECT value FROM meta WHERE key = 'spike_import'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["source_name"], "spike.db");
    assert_eq!(v["at"], NOW.0);
    assert_eq!(v["setup_skipped"], serde_json::json!(["acct-c"]));
    let tables: BTreeMap<&str, i64> = v["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["table"].as_str().unwrap(),
                t["imported"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(tables["projects"], 3);
    assert_eq!(tables["notes"], 4);
    assert_eq!(tables["groups"], 2);
    for t in &report.tables {
        assert!(t.imported <= t.source);
    }
}

#[test]
fn report_text_and_json_hide_names_unless_asked() {
    let mut s = setup(Scenario::Typical);
    let report = run(&mut s);
    let plain = report.render_text(false);
    assert!(!plain.contains("acct-b") && !plain.contains("acct-c"));
    assert!(plain.contains("account #1"));
    let named = report.render_text(true);
    assert!(named.contains("acct-b") && named.contains("acct-c"));
    let j = serde_json::to_string(&report).unwrap();
    assert!(!j.contains("acct-b") && !j.contains("acct-c"));
    let j = report.to_json(true).to_string();
    assert!(j.contains("acct-b") && j.contains("acct-c"));
}

#[test]
fn import_refuses_a_non_empty_target() {
    let mut s = setup(Scenario::Typical);
    run(&mut s);
    let count = |s: &Setup| -> i64 {
        s.store
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM projects) * 1000 + (SELECT count(*) FROM notes) + (SELECT count(*) FROM activity) * 100000",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = count(&s);
    let again = import_spike(&s.spike, &mut s.store, &env(&s.home));
    assert!(matches!(again, Err(ImportError::NotEmpty)));
    assert_eq!(count(&s), before);
}

#[test]
fn a_missing_spike_is_an_error_and_is_not_created() {
    let mut s = setup(Scenario::Typical);
    let gone = s.home.join("nothing.db");
    let r = import_spike(&gone, &mut s.store, &env(&s.home));
    assert!(matches!(r, Err(ImportError::SpikeMissing(_))));
    assert!(!gone.exists());
}

#[test]
fn awkward_simple_tables_warn_without_contents() {
    let mut s = setup(Scenario::Awkward);
    let report = run(&mut s);
    assert!(report.has_warnings());
    let c = s.store.conn();
    // "Private/", "Private" and "A//B" normalise to two distinct groups.
    let mut groups: Vec<String> = c
        .prepare("SELECT path FROM groups")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    groups.sort();
    assert!(groups.contains(&"A/B".to_string()) && groups.contains(&"Private".to_string()));
    assert!(
        groups
            .iter()
            .all(|g| !g.starts_with('/') && !g.ends_with('/') && !g.contains("//"))
    );
    // The malformed console thread gave a warning and no row.
    let threads: i64 = c
        .query_row("SELECT count(*) FROM moa_threads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(threads, 0);
    let t = report
        .tables
        .iter()
        .find(|t| t.table == "moa_threads")
        .unwrap();
    assert_eq!((t.source, t.imported, t.warned), (1, 0, 1));
    // Activity text that the spike left NULL is imported as an empty string.
    let empty: i64 = c
        .query_row("SELECT count(*) FROM activity WHERE text = ''", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(empty >= 1);
    // Notes never carry record contents.
    for n in &report.notes {
        assert!(n.key.starts_with('#') || n.key.len() == 8, "{}", n.key);
    }
    let text = report.render_text(false);
    for w in ["Lantern", "Tidepool", "acct-a", "Private"] {
        assert!(!text.contains(w), "{w}");
    }
}

#[test]
fn an_old_spike_imports_with_the_late_tables_missing() {
    let mut s = setup(Scenario::Old);
    let report = run(&mut s);
    assert!(
        report
            .tables
            .iter()
            .any(|t| t.table == "projects" && t.imported > 0)
    );
    let groups: i64 = s
        .store
        .conn()
        .query_row("SELECT count(*) FROM groups", [], |r| r.get(0))
        .unwrap();
    assert_eq!(groups, 0);
}

#[test]
fn odd_rows_are_kept_with_fixed_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("odd.db");
    let c = Connection::open(&spike).unwrap();
    c.execute_batch(
        "CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, status TEXT, last_activity TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO projects VALUES (1, 'Lantern', 'someday', 'soon', 'garbage', NULL);
         INSERT INTO projects VALUES (2, 'lantern', 'active', NULL, '', '');
         INSERT INTO projects VALUES (3, 'LANTERN (2)', 'active', NULL, NULL, NULL);
         INSERT INTO projects VALUES (4, 'Two bad times', 'active', NULL, 'garbage', 'nonsense');
         CREATE TABLE notes (id INTEGER PRIMARY KEY, project_id INTEGER, kind TEXT, text TEXT, created_at TEXT);
         INSERT INTO notes VALUES (1, 99, 'milestone', 'secret words', NULL);
         CREATE TABLE activity (id INTEGER PRIMARY KEY, ts TEXT, who TEXT, text TEXT);
         INSERT INTO activity VALUES (1, NULL, 'bot', 'x');
         INSERT INTO activity VALUES (2, NULL, NULL, 'y');",
    )
    .unwrap();
    drop(c);
    let dst_dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dst_dir.path().join("m.db"), &dst_dir.path().join("b")).unwrap();
    let report = import_spike(&spike, &mut store, &env(&home)).unwrap();

    let names: Vec<String> = store
        .conn()
        .prepare("SELECT name FROM projects ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        names,
        ["Lantern", "lantern (2)", "LANTERN (2) (2)", "Two bad times"]
    );
    let (status, created): (String, i64) = store
        .conn()
        .query_row(
            "SELECT status, created_at FROM projects WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), created), ("active", NOW.0));
    let (kind, project): (String, Option<i64>) = store
        .conn()
        .query_row("SELECT kind, project_id FROM notes", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((kind.as_str(), project), ("milestone", None));
    let who: String = store
        .conn()
        .query_row("SELECT who FROM activity", [], |r| r.get(0))
        .unwrap();
    assert_eq!(who, "brain");
    let warned = |t: &str| report.tables.iter().find(|c| c.table == t).unwrap().warned;
    assert_eq!(
        (warned("projects"), warned("notes"), warned("activity")),
        (4, 1, 2)
    );
    // Two bad times on one record give two notes that each name their column.
    let reasons: Vec<&str> = report
        .notes
        .iter()
        .filter(|n| n.table == "projects" && n.key == "#4")
        .map(|n| n.reason)
        .collect();
    assert_eq!(reasons.len(), 2);
    assert!(reasons[0].starts_with("created_at") && reasons[1].starts_with("updated_at"));
    assert_ne!(reasons[0], reasons[1]);
    let text = report.render_text(false);
    assert!(!text.contains("secret") && !text.contains("Lantern") && !text.contains("garbage"));
}
