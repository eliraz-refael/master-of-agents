//! `moa adopt-spike` and `moa export-spike`, run as the real binary against a made-up brain.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use moa_store::Store;
use moa_testkit::{Scenario, SpikeFixture, ids};
use sha2::{Digest, Sha256};

const TZ: &str = "EST5EDT,M3.2.0,M11.1.0";

struct Rig {
    dir: tempfile::TempDir,
    home: PathBuf,
    spike: PathBuf,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let spike = dir.path().join("spike.db");
    SpikeFixture::write(&spike, Scenario::Typical, &home).unwrap();
    Rig { dir, home, spike }
}

impl Rig {
    fn moa(&self, args: &[&str]) -> Output {
        Command::cargo_bin("moa")
            .unwrap()
            .env("HOME", &self.home)
            .args(args)
            .args(["--tz", TZ])
            .output()
            .unwrap()
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        // SQLite may create empty -wal/-shm files beside a database it reads.
        .filter(|n| !n.ends_with("-wal") && !n.ends_with("-shm"))
        .collect();
    v.sort();
    v
}

fn hash(p: &Path) -> Option<String> {
    // A missing file and an empty one are the same to SQLite.
    Some(format!(
        "{:x}",
        Sha256::digest(fs::read(p).unwrap_or_default())
    ))
}

fn sessions(db: &Path) -> i64 {
    let store = Store::open_read_only(db).unwrap();
    store
        .conn()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn dry_run_prints_counts_and_writes_nothing() {
    let r = rig();
    let wal = PathBuf::from(format!("{}-wal", s(&r.spike)));
    let before = (listing(r.dir.path()), hash(&r.spike), hash(&wal));
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike)]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    for want in ["projects", "sessions", "dry run: nothing written"] {
        assert!(t.contains(want), "missing {want}: {t}");
    }
    assert_eq!(before, (listing(r.dir.path()), hash(&r.spike), hash(&wal)));
}

#[test]
fn into_writes_a_v1_brain() {
    let r = rig();
    let into = r.path("moa.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&into)]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("dry run"));
    assert_eq!(sessions(&into), 9);
}

#[test]
fn into_an_existing_file_or_leftover_wal_is_refused() {
    let r = rig();
    let into = r.path("moa.db");
    fs::write(&into, b"mine").unwrap();
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&into)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("moa.db already exists; moa never overwrites a brain"));
    assert!(!text(&o).contains(s(r.dir.path())));
    assert_eq!(fs::read(&into).unwrap(), b"mine");

    let fresh = r.path("fresh.db");
    let wal = r.path("fresh.db-wal");
    fs::write(&wal, b"x").unwrap();
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&fresh)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("fresh.db-wal already exists; moa never overwrites a brain"));
    assert!(!fresh.exists());
    assert_eq!(fs::read(&wal).unwrap(), b"x");

    // Into the source itself is refused, even by another spelling of its path.
    let alias = r.dir.path().join(".").join("spike.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&alias)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("the source and the target are the same file"));
}

#[test]
fn missing_source_is_refused_and_not_created() {
    let r = rig();
    let gone = r.path("nope.db");
    for cmd in ["adopt-spike", "export-spike"] {
        let o = r.moa(&[cmd, "--from", s(&gone), "--into", s(&r.path("out.db"))]);
        assert_eq!(o.status.code(), Some(1));
        let want = if cmd == "adopt-spike" {
            "no spike brain at nope.db"
        } else {
            "no v1 brain at nope.db"
        };
        assert!(text(&o).contains(want), "{}", text(&o));
        assert!(!text(&o).contains(s(r.dir.path())));
        assert!(!gone.exists() && !r.path("out.db").exists());
    }
    let o = r.moa(&["adopt-spike", "--from", s(&gone)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!gone.exists());
}

#[test]
fn bad_tz_is_refused() {
    let r = rig();
    let o = Command::cargo_bin("moa")
        .unwrap()
        .env("HOME", &r.home)
        .args(["adopt-spike", "--from", s(&r.spike), "--tz", "Nowhere/Land"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("time zone"));
}

#[test]
fn bad_account_flag_is_refused_without_echoing_it() {
    let r = rig();
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--account", "sneaky"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!text(&o).contains("sneaky"));
}

#[test]
fn output_is_contents_free_without_names() {
    let r = rig();
    let home = s(&r.home).to_string();
    for extra in [&[][..], &["--json"][..]] {
        let mut args = vec!["adopt-spike", "--from", s(&r.spike)];
        args.extend_from_slice(extra);
        let o = r.moa(&args);
        assert!(o.status.success(), "{}", text(&o));
        let t = text(&o);
        for secret in [
            "Wire up the dimmer",
            "Lantern",
            "acct-a",
            "acct-b",
            ids::S1_WORKING,
            "Pin 7",
            home.as_str(),
        ] {
            assert!(!t.contains(secret), "output shows {secret}");
        }
        if extra.contains(&"--json") {
            serde_json::from_slice::<serde_json::Value>(&o.stdout).expect("stdout is JSON");
        }
    }
}

#[test]
fn names_flag_shows_accounts() {
    let r = rig();
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--names"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("acct-a"));
}

#[test]
fn export_spike_round_trip_via_cli() {
    let r = rig();
    let v1 = r.path("moa.db");
    let out = r.path("out.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&v1)]);
    assert!(o.status.success(), "{}", text(&o));
    let o = r.moa(&["export-spike", "--from", s(&v1), "--into", s(&out)]);
    assert!(o.status.success(), "{}", text(&o));
    let db =
        rusqlite::Connection::open_with_flags(&out, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let n: i64 = db
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 9);
    let triggers: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(triggers > 0);

    // Exporting again over the result is refused.
    let o = r.moa(&["export-spike", "--from", s(&v1), "--into", s(&out)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("out.db already exists; moa never overwrites a brain"));
}

#[test]
fn export_spike_refuses_an_existing_target() {
    let r = rig();
    let v1 = r.path("v1.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&v1)]);
    assert!(o.status.success(), "{}", text(&o));
    let into = r.path("moa.db");
    fs::write(&into, b"mine").unwrap();

    let o = r.moa(&["export-spike", "--from", s(&v1), "--into", s(&into)]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("moa.db already exists; moa never overwrites a brain"),
        "{}",
        text(&o)
    );
    assert!(!text(&o).contains(s(r.dir.path())));
    assert_eq!(fs::read(&into).unwrap(), b"mine");
}

#[test]
fn export_spike_refuses_a_sidecar_name() {
    let r = rig();
    let v1 = r.path("v1.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&v1)]);
    assert!(o.status.success(), "{}", text(&o));
    let into = r.path("x.db-wal");

    let o = r.moa(&["export-spike", "--from", s(&v1), "--into", s(&into)]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("x.db-wal is a name SQLite keeps for its own files; pick another"),
        "{}",
        text(&o)
    );
    assert!(!text(&o).contains("already exists"), "{}", text(&o));
    assert!(!into.exists());
}

#[test]
fn export_from_a_file_that_is_not_a_v1_brain_says_so() {
    let r = rig();
    let garbage = r.path("garbage.db");
    fs::write(&garbage, b"this is not a database at all, just text").unwrap();
    for from in [&r.spike, &garbage] {
        let out = r.path("out.db");
        let o = r.moa(&["export-spike", "--from", s(from), "--into", s(&out)]);
        assert_eq!(o.status.code(), Some(1), "{}", text(&o));
        let name = from.file_name().unwrap().to_str().unwrap();
        assert!(
            text(&o).contains(&format!("{name} isn't a moa brain")),
            "{}",
            text(&o)
        );
        assert!(!text(&o).contains("upgraded"), "{}", text(&o));
        assert!(!out.exists());
    }
}

#[test]
fn into_a_missing_folder_is_refused_plainly() {
    let r = rig();
    let into = r.path("nowhere").join("moa.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&into)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o).contains("the folder for moa.db doesn't exist"),
        "{}",
        text(&o)
    );
    assert!(!text(&o).contains(s(r.dir.path())));
    assert!(!r.path("nowhere").exists());
}

#[test]
fn into_a_sidecar_name_is_refused() {
    let r = rig();
    for name in ["brain.db-journal", "brain.db-wal", "brain.db-shm"] {
        let into = r.path(name);
        let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&into)]);
        assert_eq!(o.status.code(), Some(1), "{name}");
        assert!(
            text(&o).contains(&format!(
                "{name} is a name SQLite keeps for its own files; pick another"
            )),
            "{}",
            text(&o)
        );
        assert!(!into.exists(), "{name}");
    }
}

#[test]
fn a_failed_adopt_leaves_nothing_behind() {
    let r = rig();
    let garbage = r.path("garbage.db");
    fs::write(&garbage, b"this is not a database at all, just text").unwrap();
    let into = r.path("x.db");
    let o = r.moa(&["adopt-spike", "--from", s(&garbage), "--into", s(&into)]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let left: Vec<String> = listing(r.dir.path())
        .into_iter()
        .filter(|n| n.starts_with("x.db"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

fn event_times(db: &Path) -> Vec<String> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    c.prepare("SELECT ts FROM events ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn export_reads_the_zone_the_import_recorded() {
    let r = rig();
    let v1 = r.path("moa.db");
    let o = r.moa(&["adopt-spike", "--from", s(&r.spike), "--into", s(&v1)]);
    assert!(o.status.success(), "{}", text(&o));
    let recorded: String = rusqlite::Connection::open(&v1)
        .unwrap()
        .query_row(
            "SELECT json_extract(value, '$.tz') FROM meta WHERE key = 'spike_import'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, TZ);

    // Without --tz the recorded zone is used, whatever this machine's zone is.
    let run = |into: &Path, tz: Option<&str>| -> Output {
        let mut cmd = Command::cargo_bin("moa").unwrap();
        cmd.env("HOME", &r.home).env("TZ", "UTC");
        cmd.args(["export-spike", "--from", s(&v1), "--into", s(into)]);
        if let Some(tz) = tz {
            cmd.args(["--tz", tz]);
        }
        cmd.output().unwrap()
    };
    let note = "note: exporting with a different time zone than the import used";
    let plain = r.path("plain.db");
    let o = run(&plain, None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains(note), "{}", text(&o));
    assert_eq!(event_times(&plain), event_times(&r.spike));

    let same = r.path("same.db");
    let o = run(&same, Some(TZ));
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains(note), "{}", text(&o));

    let other = r.path("other.db");
    let o = run(&other, Some("UTC"));
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(text(&o).matches(note).count(), 1, "{}", text(&o));
    assert_ne!(event_times(&other), event_times(&r.spike));
}
