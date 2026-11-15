//! moa's SQLite store: schema v1, migrations and `Store::open`.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use rusqlite_migration::{Migrations, SchemaVersion};

pub mod accounts;
pub mod backup;
pub mod export;
pub mod import;
mod schema;
pub mod spike;

pub use schema::{SCHEMA_VERSION, migrations};

/// Why a store call failed. No variant carries record contents; `Missing` holds a path the
/// user chose.
#[derive(Debug)]
pub enum StoreError {
    Missing(PathBuf),
    Sqlite(rusqlite::Error),
    Migration(rusqlite_migration::Error),
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(p) => write!(f, "there is no database at {}", p.display()),
            Self::Sqlite(e) => write!(f, "the database failed: {e}"),
            Self::Migration(e) => write!(f, "the database could not be upgraded: {e}"),
            Self::Io(e) => write!(f, "a file could not be used: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Missing(_) => None,
            Self::Sqlite(e) => Some(e),
            Self::Migration(e) => Some(e),
            Self::Io(e) => Some(e),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<rusqlite_migration::Error> for StoreError {
    fn from(e: rusqlite_migration::Error) -> Self {
        Self::Migration(e)
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// An open brain database.
pub struct Store {
    conn: Connection,
    /// `Some(v)` when the file is at a schema newer than this build knows. It is opened
    /// as-is: the additive-only rule (C5) keeps that safe.
    pub ahead_of_build: Option<usize>,
}

pub(crate) fn create_private_dirs(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

impl Store {
    /// Opens (creating if needed) the database at `db`, migrating it to the current schema.
    /// An older database is copied into `backups` first.
    pub fn open(db: &Path, backups: &Path) -> Result<Store, StoreError> {
        Self::open_with(db, backups, &migrations(), SCHEMA_VERSION)
    }

    /// Opens an existing database without creating or migrating anything.
    pub fn open_read_only(db: &Path) -> Result<Store, StoreError> {
        if !db.exists() {
            return Err(StoreError::Missing(db.to_path_buf()));
        }
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let ahead_of_build = match migrations().current_version(&conn)? {
            SchemaVersion::Outside(v) => Some(v.get()),
            _ => None,
        };
        Ok(Store {
            conn,
            ahead_of_build,
        })
    }

    pub(crate) fn open_with(
        db: &Path,
        backups: &Path,
        migrations: &Migrations<'_>,
        target: usize,
    ) -> Result<Store, StoreError> {
        if let Some(parent) = db.parent() {
            create_private_dirs(parent)?;
        }
        let mut conn = Connection::open(db)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;

        let mut ahead_of_build = None;
        match migrations.current_version(&conn)? {
            SchemaVersion::Outside(v) => ahead_of_build = Some(v.get()),
            SchemaVersion::Inside(v) if v.get() < target => {
                backup::vacuum_into(&conn, &backup_path(backups, target))?;
                migrations.to_latest(&mut conn)?;
            }
            SchemaVersion::Inside(_) => {}
            SchemaVersion::NoneSet => migrations.to_latest(&mut conn)?,
        }
        Ok(Store {
            conn,
            ahead_of_build,
        })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// `backups/pre-v<target>.db`, or with `-<unix ms>` added when that name is taken.
fn backup_path(backups: &Path, target: usize) -> PathBuf {
    let plain = backups.join(format!("pre-v{target}.db"));
    if fs::symlink_metadata(&plain).is_err() {
        return plain;
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    backups.join(format!("pre-v{target}-{ms}.db"))
}

#[cfg(test)]
mod tests {
    use rusqlite_migration::M;

    use super::*;

    #[test]
    fn migrating_an_older_db_backs_it_up_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("moa.db");
        let backups = dir.path().join("backups");
        let one = Migrations::new(vec![M::up(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;",
        )]);
        let two = Migrations::new(vec![
            M::up("CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;"),
            M::up("CREATE TABLE extra (id INTEGER PRIMARY KEY) STRICT;"),
        ]);

        let store = Store::open_with(&db, &backups, &one, 1).unwrap();
        store
            .conn()
            .execute("INSERT INTO projects (name) VALUES ('Lantern')", [])
            .unwrap();
        drop(store);
        assert!(!backups.exists(), "a fresh db is not backed up");

        let store = Store::open_with(&db, &backups, &two, 2).unwrap();
        let version: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let backup = Connection::open_with_flags(
            backups.join("pre-v2.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let version: i64 = backup
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let name: String = backup
            .query_row("SELECT name FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Lantern");
    }

    #[test]
    fn a_taken_pre_migration_name_gets_a_millisecond_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        fs::create_dir(&backups).unwrap();
        fs::write(backups.join("pre-v3.db"), b"old").unwrap();

        let path = backup_path(&backups, 3);
        let name = path.file_name().unwrap().to_str().unwrap();
        let ms = name
            .strip_prefix("pre-v3-")
            .and_then(|n| n.strip_suffix(".db"))
            .unwrap_or_else(|| panic!("unexpected name {name}"));
        assert!(ms.parse::<u128>().is_ok(), "{name}");
        assert_eq!(fs::read(backups.join("pre-v3.db")).unwrap(), b"old");
    }
}
