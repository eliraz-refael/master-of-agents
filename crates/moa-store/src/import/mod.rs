//! Import from a spike brain (the prototype's SQLite DB) into a v1 store.
//!
//! The spike DB is never changed: it is opened read-only and copied with `VACUUM INTO`, and
//! the copy is what gets read. Everything is written in one transaction of the target store.
//! Reports carry no record contents (see [`report`]).

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::model::{SessionId, TranscriptCheck};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::accounts::AccountMap;
use crate::{Store, StoreError};

pub mod report;
pub mod rules;
mod simple;

pub use report::{ImportReport, Level, RecordNote, TableCounts};

/// How new session ids are made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdSource {
    /// A uuid v7 that depends only on the brain, the conversation id and its start time, so
    /// every import of the same brain gives the same ids and ids sort by start time.
    Derived,
    /// `00000000-0000-7000-8000-{k:012x}` for k = n, n + 1, ... in import order.
    Sequential(u64),
}

impl IdSource {
    pub fn generator(self) -> IdGen {
        IdGen {
            source: self,
            next: match self {
                IdSource::Derived => 0,
                IdSource::Sequential(n) => n,
            },
        }
    }
}

/// Hands out session ids in import order.
#[derive(Debug)]
pub struct IdGen {
    source: IdSource,
    next: u64,
}

impl IdGen {
    pub fn next_id(&mut self, brain: &str, conversation_id: &str, created_at: UtcMs) -> SessionId {
        match self.source {
            IdSource::Derived => {
                let mut h = Sha256::new();
                h.update(brain.as_bytes());
                h.update([0u8]);
                h.update(conversation_id.as_bytes());
                let digest = h.finalize();
                let mut rand = [0u8; 10];
                rand.copy_from_slice(&digest[..10]);
                let ms = created_at.0.max(0) as u64;
                SessionId(uuid::Builder::from_unix_timestamp_millis(ms, &rand).into_uuid())
            }
            IdSource::Sequential(_) => {
                let k = self.next;
                self.next += 1;
                let base: u128 = 0x0000_0000_0000_7000_8000_0000_0000_0000;
                SessionId(uuid::Uuid::from_u128(
                    base | u128::from(k & 0xffff_ffff_ffff),
                ))
            }
        }
    }
}

/// Everything an import needs to know about the world.
pub struct ImportEnv {
    pub tz: TimeZone,
    pub now: UtcMs,
    /// Names the brain, so derived session ids differ between brains.
    pub brain: String,
    pub accounts: AccountMap,
    pub ids: IdSource,
    pub transcript: fn(&Path, &str) -> TranscriptCheck,
}

#[derive(Debug)]
pub enum ImportError {
    SpikeMissing(PathBuf),
    NotEmpty,
    Store(StoreError),
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpikeMissing(p) => write!(f, "there is no spike database at {}", p.display()),
            Self::NotEmpty => f.write_str("this brain already has projects, sessions or tasks"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Sqlite(e) => write!(f, "the database failed: {e}"),
            Self::Io(e) => write!(f, "a file could not be used: {e}"),
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpikeMissing(_) | Self::NotEmpty => None,
            Self::Store(e) => Some(e),
            Self::Sqlite(e) => Some(e),
            Self::Io(e) => Some(e),
        }
    }
}

impl From<StoreError> for ImportError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}
impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

fn open_spike_read_only(path: &Path) -> Result<Connection, ImportError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    Ok(conn)
}

/// Copies the spike DB to `dest` with `VACUUM INTO`. The spike DB is opened read-only and is
/// never created: a missing one is `SpikeMissing`. Rows that live only in its `-wal` are copied.
pub fn snapshot_spike(src: &Path, dest: &Path) -> Result<(), ImportError> {
    if !src.exists() {
        return Err(ImportError::SpikeMissing(src.to_path_buf()));
    }
    let conn = open_spike_read_only(src)?;
    crate::backup::vacuum_into(&conn, dest)?;
    Ok(())
}

/// One row of a spike table, read by column name.
pub(crate) struct SrcRow {
    cols: Vec<&'static str>,
    vals: Vec<Value>,
}

impl SrcRow {
    fn get(&self, col: &str) -> &Value {
        self.cols
            .iter()
            .position(|c| *c == col)
            .map_or(&Value::Null, |i| &self.vals[i])
    }

    /// The column as text. Numbers are shown as text, a NULL or missing column is `None`.
    pub(crate) fn text(&self, col: &str) -> Option<String> {
        match self.get(col) {
            Value::Null => None,
            Value::Integer(i) => Some(i.to_string()),
            Value::Real(r) => Some(r.to_string()),
            Value::Text(t) => Some(t.clone()),
            Value::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        }
    }

    pub(crate) fn int(&self, col: &str) -> Option<i64> {
        match self.get(col) {
            Value::Integer(i) => Some(*i),
            Value::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }
}

/// The read-only snapshot of the spike DB. A table or column an older spike DB lacks reads
/// as empty or NULL (G6).
pub(crate) struct Src<'a> {
    pub(crate) conn: &'a Connection,
}

impl Src<'_> {
    fn columns(&self, table: &str) -> Result<HashSet<String>, ImportError> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(HashSet::new());
        }
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        Ok(names.collect::<Result<_, _>>()?)
    }

    /// Reads `cols` of `table` ordered by `order`. `rowid` is always available.
    pub(crate) fn rows(
        &self,
        table: &str,
        cols: &[&'static str],
        order: &str,
    ) -> Result<Vec<SrcRow>, ImportError> {
        let have = self.columns(table)?;
        if have.is_empty() {
            return Ok(Vec::new());
        }
        let select: Vec<String> = cols
            .iter()
            .map(|c| {
                if *c == "rowid" || have.contains(*c) {
                    format!("\"{c}\"")
                } else {
                    format!("NULL AS \"{c}\"")
                }
            })
            .collect();
        let sql = format!(
            "SELECT {} FROM \"{table}\" ORDER BY {order}",
            select.join(", ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            let vals = (0..cols.len())
                .map(|i| r.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SrcRow {
                cols: cols.to_vec(),
                vals,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// The state shared by the table importers of one import.
pub(crate) struct Run<'a> {
    pub(crate) env: &'a ImportEnv,
    pub(crate) src: Src<'a>,
    pub(crate) notes: Vec<RecordNote>,
    pub(crate) tables: Vec<TableCounts>,
    pub(crate) setup_skipped: Vec<String>,
    pub(crate) project_ids: HashSet<i64>,
    dirty: bool,
}

impl Run<'_> {
    pub(crate) fn warn(&mut self, table: &'static str, key: &str, reason: &'static str) {
        self.dirty = true;
        self.notes.push(RecordNote {
            table,
            key: key.to_string(),
            level: Level::Warn,
            reason,
        });
    }

    pub(crate) fn info(&mut self, table: &'static str, key: &str, reason: &'static str) {
        self.notes.push(RecordNote {
            table,
            key: key.to_string(),
            level: Level::Info,
            reason,
        });
    }

    /// Takes the value of a rule and warns if the rule had a reason.
    pub(crate) fn rule<T>(&mut self, table: &'static str, key: &str, ruled: rules::Ruled<T>) -> T {
        if let Some(reason) = ruled.1 {
            self.warn(table, key, reason);
        }
        ruled.0
    }

    /// Ends a record: 1 if it got any warning since the last call, else 0.
    pub(crate) fn end_record(&mut self) -> usize {
        std::mem::take(&mut self.dirty) as usize
    }

    pub(crate) fn finish(
        &mut self,
        table: &'static str,
        source: usize,
        imported: usize,
        warned: usize,
    ) {
        self.tables.push(TableCounts {
            table,
            source,
            imported,
            warned,
        });
    }
}

/// `#<id>`: how a report names a record that has an integer id.
pub(crate) fn id_key(id: i64) -> String {
    format!("#{id}")
}

/// The first 8 characters of a spike conversation id: how a report names a session.
pub(crate) fn conversation_key(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Imports the spike brain at `spike_db` into the empty store `dst`, in one transaction.
pub fn import_spike(
    spike_db: &Path,
    dst: &mut Store,
    env: &ImportEnv,
) -> Result<ImportReport, ImportError> {
    let tmp = tempfile::tempdir()?;
    let snapshot = tmp.path().join("spike-snapshot.db");
    snapshot_spike(spike_db, &snapshot)?;
    let conn = open_spike_read_only(&snapshot)?;

    let tx = dst
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects) OR EXISTS(SELECT 1 FROM sessions) \
         OR EXISTS(SELECT 1 FROM tasks)",
        [],
        |r| r.get(0),
    )?;
    if occupied {
        return Err(ImportError::NotEmpty);
    }

    let mut run = Run {
        env,
        src: Src { conn: &conn },
        notes: Vec::new(),
        tables: Vec::new(),
        setup_skipped: Vec::new(),
        project_ids: HashSet::new(),
        dirty: false,
    };
    simple::projects(&mut run, &tx)?;
    // Sessions and everything that points at them are imported here (Task 7).
    simple::notes(&mut run, &tx)?;
    simple::groups(&mut run, &tx)?;
    simple::activity(&mut run, &tx)?;
    simple::meta(&mut run, &tx)?;

    let source_name = spike_db
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let summary = serde_json::json!({
        "source_name": source_name,
        "at": env.now.0,
        "tables": run.tables,
        "setup_skipped": run.setup_skipped,
    });
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('spike_import', ?1)",
        [summary.to_string()],
    )?;
    tx.commit()?;

    Ok(ImportReport {
        source_name,
        tables: run.tables,
        notes: run.notes,
        accounts: env.accounts.seen(),
        setup_skipped: run.setup_skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_ids_count_up_in_order() {
        let mut g = IdSource::Sequential(5).generator();
        let a = g.next_id("b", "c1", UtcMs(0));
        let b = g.next_id("b", "c2", UtcMs(0));
        assert_eq!(a.to_string(), "00000000-0000-7000-8000-000000000005");
        assert_eq!(b.to_string(), "00000000-0000-7000-8000-000000000006");
        let mut g = IdSource::Sequential(0xabc).generator();
        assert_eq!(
            g.next_id("b", "c", UtcMs(0)).to_string(),
            "00000000-0000-7000-8000-000000000abc"
        );
    }

    #[test]
    fn derived_ids_are_stable_v7_and_sort_by_time() {
        let mut g = IdSource::Derived.generator();
        let a = g.next_id("brain", "conv", UtcMs(1_700_000_000_000));
        let again =
            IdSource::Derived
                .generator()
                .next_id("brain", "conv", UtcMs(1_700_000_000_000));
        assert_eq!(a, again);
        assert_eq!(a.0.get_version_num(), 7);
        assert_ne!(a, g.next_id("other", "conv", UtcMs(1_700_000_000_000)));
        assert_ne!(a, g.next_id("brain", "conv2", UtcMs(1_700_000_000_000)));
        let later = g.next_id("brain", "conv", UtcMs(1_700_000_001_000));
        assert!(a.to_string() < later.to_string());
        // Before 1970 the time clamps to zero instead of wrapping.
        let early = g.next_id("brain", "conv", UtcMs(-5));
        assert!(early.to_string().starts_with("00000000-0000-7"));
    }

    #[test]
    fn keys_carry_no_contents() {
        assert_eq!(id_key(7), "#7");
        assert_eq!(conversation_key("3f9a1c52-7d4e"), "3f9a1c52");
        assert_eq!(conversation_key("ab"), "ab");
    }
}
