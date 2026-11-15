use std::fs;
use std::os::unix::fs::PermissionsExt;

use jiff::ToSpan;
use jiff::civil::{Date, date};
use moa_store::Store;
use moa_store::backup::{backups_to_delete, daily_backup, prune_backups, vacuum_into};
use proptest::prelude::*;

fn dates() -> impl Strategy<Value = Vec<Date>> {
    proptest::collection::btree_set(0i32..400, 0..60)
        .prop_map(|s| s.into_iter().map(|d| date(2026, 1, 1) + d.days()).collect())
}

proptest! {
    #[test]
    fn deletes_a_subset_and_keeps_the_newest_seven(ds in dates()) {
        let del = backups_to_delete(&ds);
        prop_assert!(del.iter().all(|d| ds.contains(d)));
        let mut sorted = ds.clone();
        sorted.sort();
        for d in sorted.iter().rev().take(7) {
            prop_assert!(!del.contains(d));
        }
        prop_assert!(ds.len() - del.len() <= 11);
    }

    #[test]
    fn is_idempotent_and_order_free(mut ds in dates()) {
        let kept: Vec<Date> = ds
            .iter()
            .copied()
            .filter(|d| !backups_to_delete(&ds).contains(d))
            .collect();
        prop_assert!(backups_to_delete(&kept).is_empty());
        ds.reverse();
        prop_assert_eq!(backups_to_delete(&ds).len(), ds.len() - kept.len());
    }

    #[test]
    fn pruning_daily_equals_pruning_once(ds in dates()) {
        let mut sorted = ds.clone();
        sorted.sort();
        let mut live: Vec<Date> = Vec::new();
        for d in &sorted {
            live.push(*d);
            let del = backups_to_delete(&live);
            live.retain(|x| !del.contains(x));
        }
        let del = backups_to_delete(&sorted);
        let mut once: Vec<Date> = sorted.iter().copied().filter(|d| !del.contains(d)).collect();
        once.sort();
        live.sort();
        prop_assert_eq!(live, once);
    }
}

fn touch(dir: &std::path::Path, name: &str) {
    fs::write(dir.join(name), b"x").unwrap();
}

#[test]
fn prune_ignores_pre_migration_and_switch_backups() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        let d = date(2026, 1, 1) + i.days();
        touch(dir.path(), &format!("daily-{d}.db"));
    }
    touch(dir.path(), "pre-v2.db");
    touch(dir.path(), "pre-switch-1.db");
    touch(dir.path(), "daily-2026-13-45.db");
    touch(dir.path(), "daily-2026-1-1.db");
    touch(dir.path(), "daily-2026-01-01.db-wal");

    let removed = prune_backups(dir.path()).unwrap();
    assert!(!removed.is_empty());
    for p in &removed {
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("daily-2026-01-") && name.ends_with(".db"),
            "{name}"
        );
        assert!(!p.exists());
    }
    for keep in [
        "pre-v2.db",
        "pre-switch-1.db",
        "daily-2026-13-45.db",
        "daily-2026-1-1.db",
        "daily-2026-01-01.db-wal",
    ] {
        assert!(dir.path().join(keep).exists(), "{keep}");
    }
    assert!(dir.path().join("daily-2026-01-20.db").exists());
}

#[test]
fn vacuum_into_refuses_an_existing_file_or_leftover_wal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("pre")).unwrap();

    let x = dir.path().join("out").join("x");
    vacuum_into(store.conn(), &x).unwrap();
    let before = fs::read(&x).unwrap();
    assert_already_exists(vacuum_into(store.conn(), &x));
    assert_eq!(fs::read(&x).unwrap(), before);

    let y = dir.path().join("out").join("y");
    fs::write(dir.path().join("out").join("y-wal"), b"w").unwrap();
    assert_already_exists(vacuum_into(store.conn(), &y));
    assert!(!y.exists());

    let mode = fs::metadata(dir.path().join("out"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn daily_backup_is_once_per_day() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("pre")).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO projects (name, created_at, updated_at) VALUES ('Lantern', 0, 0)",
            [],
        )
        .unwrap();
    let backups = dir.path().join("backups");
    let today = date(2026, 11, 15);

    let path = daily_backup(&store, &backups, today).unwrap().unwrap();
    assert_eq!(path, backups.join("daily-2026-11-15.db"));
    let copy =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let name: String = copy
        .query_row("SELECT name FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Lantern");

    assert!(daily_backup(&store, &backups, today).unwrap().is_none());
    assert!(
        daily_backup(&store, &backups, date(2026, 11, 16))
            .unwrap()
            .is_some()
    );
}

fn assert_already_exists(r: Result<(), moa_store::StoreError>) {
    match r {
        Err(moa_store::StoreError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists)
        }
        other => panic!("expected Io(AlreadyExists), got {other:?}"),
    }
}

#[test]
fn daily_backup_reports_a_leftover_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("pre")).unwrap();
    let backups = dir.path().join("backups");
    fs::create_dir(&backups).unwrap();
    fs::write(backups.join("daily-2026-11-15.db-wal"), b"w").unwrap();

    let r = daily_backup(&store, &backups, date(2026, 11, 15));
    assert!(
        r.is_err(),
        "a leftover -wal must not read as already backed up"
    );
    assert!(!backups.join("daily-2026-11-15.db").exists());
}

#[test]
fn a_refusal_names_the_file_not_its_folder() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("pre")).unwrap();
    let y = dir.path().join("out").join("y");
    fs::create_dir(dir.path().join("out")).unwrap();
    fs::write(dir.path().join("out").join("y-wal"), b"w").unwrap();
    let err = vacuum_into(store.conn(), &y).unwrap_err();
    assert_eq!(
        err.to_string(),
        "a file could not be used: y-wal already exists"
    );
}

#[test]
fn sidecars_are_the_three_files_sqlite_keeps_beside_a_database() {
    let db = std::path::Path::new("/x/brain.db");
    let names: Vec<String> = moa_store::sidecars(db)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["/x/brain.db-wal", "/x/brain.db-shm", "/x/brain.db-journal"]
    );
    assert!(moa_store::is_sidecar_name(std::path::Path::new(
        "/x/brain.db-journal"
    )));
    assert!(!moa_store::is_sidecar_name(db));
}
