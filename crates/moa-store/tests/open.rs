use std::os::unix::fs::PermissionsExt;

use moa_store::{SCHEMA_VERSION, Store, StoreError, migrations};
use rusqlite::ErrorCode;

fn user_version(store: &Store) -> i64 {
    store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn migrations_are_valid() {
    migrations().validate().unwrap();
}

#[test]
fn new_db_is_at_the_current_version_with_every_table() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("brain/moa.db"),
        &dir.path().join("backups"),
    )
    .unwrap();
    assert_eq!(user_version(&store), SCHEMA_VERSION as i64);
    assert_eq!(store.ahead_of_build, None);

    let mut stmt = store
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        tables,
        [
            "activity",
            "asks",
            "conversations",
            "events",
            "feed",
            "groups",
            "meta",
            "moa_threads",
            "nags",
            "notes",
            "pane_bindings",
            "projects",
            "sessions",
            "spike_panes",
            "suggestions",
            "tasks"
        ]
    );

    let mode: String = store
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = store
        .conn()
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
}

#[test]
fn created_dirs_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("brain/moa.db");
    Store::open(&db, &dir.path().join("backups")).unwrap();
    let mode = std::fs::metadata(db.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn project_names_are_unique_ignoring_case() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    let insert = |name: &str| {
        store.conn().execute(
            "INSERT INTO projects (name, created_at, updated_at) VALUES (?1, 0, 0)",
            [name],
        )
    };
    insert("Lantern").unwrap();
    let err = insert("lantern").unwrap_err();
    assert_eq!(
        err.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );
    assert!(err.to_string().contains("UNIQUE"), "{err}");
}

#[test]
fn a_db_from_a_newer_build_opens_without_migrating() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("moa.db");
    let backups = dir.path().join("backups");
    let store = Store::open(&db, &backups).unwrap();
    store
        .conn()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    drop(store);

    let store = Store::open(&db, &backups).unwrap();
    assert_eq!(store.ahead_of_build, Some(99));
    assert_eq!(user_version(&store), 99);
}

#[test]
fn read_only_open_never_creates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nope/moa.db");
    let err = Store::open_read_only(&db).err().expect("must fail");
    assert!(
        matches!(err, StoreError::Missing(ref p) if p == &db),
        "{err}"
    );
    assert!(!db.exists());
    assert!(!db.parent().unwrap().exists());
}
