//! Export a v1 brain back into the spike's schema: the fallback that lets the user switch
//! back to the prototype. It is the inverse of [`crate::import`].
//!
//! The output is a new file. It gets the spike's tables, then the data, then the spike's nine
//! activity triggers (they come last so they add no activity rows of their own). Reports
//! carry no record contents: records are named by table and `#id` or an 8-character
//! conversation prefix, and reasons are fixed sentences.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::time::utc_to_spike_local;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::accounts::AccountMap;
use crate::import::{Level, RecordNote, TableCounts};
use crate::spike::{SCHEMA_SQL, TRIGGERS_SQL};
use crate::{Store, StoreError};

/// What an export needs to know about the world.
pub struct ExportEnv {
    pub tz: TimeZone,
    pub accounts: AccountMap,
    /// The accounts the spike's setup skipped. `None` takes them from `meta.spike_import`;
    /// `Some` is the user's override.
    pub setup_skipped: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ExportError {
    /// The output, or a file the spike's database would leave beside it, already exists.
    Exists(PathBuf),
    Store(StoreError),
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists(p) => write!(f, "{} is in the way; nothing was written", p.display()),
            Self::Store(e) => write!(f, "{e}"),
            Self::Sqlite(e) => write!(f, "the database failed: {e}"),
            Self::Io(e) => write!(f, "a file could not be used: {e}"),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Exists(_) => None,
            Self::Store(e) => Some(e),
            Self::Sqlite(e) => Some(e),
            Self::Io(e) => Some(e),
        }
    }
}

impl From<StoreError> for ExportError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}
impl From<rusqlite::Error> for ExportError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What an export tells the user. `TableCounts::imported` counts the rows exported.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExportReport {
    pub tables: Vec<TableCounts>,
    pub notes: Vec<RecordNote>,
}

impl ExportReport {
    pub fn has_warnings(&self) -> bool {
        self.notes.iter().any(|n| n.level == Level::Warn)
    }

    pub fn render_text(&self) -> String {
        let mut out = String::from("Exported to the spike's format\n");
        for t in &self.tables {
            let _ = writeln!(
                out,
                "  {:<14} {} of {} exported, {} with warnings",
                t.table, t.imported, t.source, t.warned
            );
        }
        for n in &self.notes {
            let label = match n.level {
                Level::Info => "note",
                Level::Warn => "warning",
            };
            let _ = writeln!(out, "  {label}: {} {}: {}", n.table, n.key, n.reason);
        }
        out
    }
}

mod tables;

/// Things v1 holds that the spike has no place for: `(table, key, reason)`.
const NOT_EXPORTED: [(&str, &str, &str); 12] = [
    ("feed", "all", "the console feed is not exported"),
    ("nags", "all", "nags are not exported"),
    (
        "conversations",
        "all",
        "conversation history beyond the current conversation is not exported",
    ),
    (
        "pane_bindings",
        "all",
        "the generation fields of a pane link are not exported",
    ),
    ("tasks", "done_at", "done_at is not exported"),
    ("asks", "claimed_at", "claimed_at is not exported"),
    (
        "activity",
        "ref_kind",
        "ref_kind and ref_id are not exported",
    ),
    ("groups", "collapsed", "collapsed is not exported"),
    ("sessions", "seen_at", "seen_at is not exported"),
    ("sessions", "title_locked", "title_locked is not exported"),
    ("sessions", "activity_why", "activity_why is not exported"),
    ("sessions", "resume_failed", "resume_failed is not exported"),
];

fn leftovers(dst: &Path) -> Vec<PathBuf> {
    ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut s = dst.as_os_str().to_owned();
            s.push(suffix);
            PathBuf::from(s)
        })
        .collect()
}

/// Writes the brain in `src` as a spike database at `dst`. `dst` must not exist, nor may a
/// `-wal`, `-shm` or `-journal` beside it, and its folder must exist. Nothing is changed when
/// that refuses, and a failed export leaves no partial file.
pub fn export_spike(src: &Store, dst: &Path, env: &ExportEnv) -> Result<ExportReport, ExportError> {
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned());
    if name.is_some_and(|n| ["-wal", "-shm", "-journal"].iter().any(|s| n.ends_with(s))) {
        return Err(ExportError::Exists(dst.to_path_buf()));
    }
    for path in std::iter::once(dst.to_path_buf()).chain(leftovers(dst)) {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(ExportError::Exists(path));
        }
    }
    if let Some(parent) = dst.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.is_dir()
    {
        return Err(ExportError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "the folder for the output doesn't exist",
        )));
    }

    // Create the file ourselves, exclusively: `Connection::open` would happily write into a
    // file that appeared since the checks above, and the cleanup below would delete it.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
    {
        Ok(file) => drop(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExportError::Exists(dst.to_path_buf()));
        }
        Err(e) => return Err(ExportError::Io(e)),
    }
    let result = write_all(src, dst, env);
    if result.is_err() {
        let _ = fs::remove_file(dst);
        for p in leftovers(dst) {
            let _ = fs::remove_file(p);
        }
    }
    result
}

fn write_all(src: &Store, dst: &Path, env: &ExportEnv) -> Result<ExportReport, ExportError> {
    let mut conn = Connection::open(dst)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    conn.execute_batch(SCHEMA_SQL)?;
    let report = {
        let tx = conn.transaction()?;
        let mut ex = Exporter::new(src.conn(), env)?;
        ex.run(&tx)?;
        tx.commit()?;
        ExportReport {
            tables: ex.tables,
            notes: ex.notes,
        }
    };
    // What the spike's own `connect()` does. It runs after the data, so it adds no activity.
    conn.execute_batch(TRIGGERS_SQL)?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    conn.close().map_err(|(_, e)| e)?;
    let mut wal = dst.as_os_str().to_owned();
    wal.push("-wal");
    if fs::metadata(PathBuf::from(wal)).is_ok_and(|m| m.len() > 0) {
        return Err(ExportError::Io(io::Error::other(
            "the exported database still has a write-ahead log",
        )));
    }
    Ok(report)
}

/// The current conversation of a session: the spike's identity for it.
struct Current {
    conversation_id: String,
    spike_account: Option<String>,
    store_dir: Option<String>,
}

struct Exporter<'a> {
    src: &'a Connection,
    env: &'a ExportEnv,
    /// v1 session id (hyphenated) to its current conversation.
    current: HashMap<String, Current>,
    /// Every conversation id of every session, for the suggestion rewrite.
    all_conversations: Vec<String>,
    /// Every v1 session id, with or without a conversation, for the same rewrite.
    all_sessions: Vec<String>,
    notes: Vec<RecordNote>,
    tables: Vec<TableCounts>,
    dirty: bool,
}

fn key_of(id: i64) -> String {
    format!("#{id}")
}

fn prefix8(s: &str) -> String {
    s.chars().take(8).collect()
}

fn abs(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

impl<'a> Exporter<'a> {
    fn new(src: &'a Connection, env: &'a ExportEnv) -> Result<Self, ExportError> {
        let mut current = HashMap::new();
        let mut all_conversations = Vec::new();
        let mut stmt = src.prepare(
            "SELECT session_id, conversation_id, spike_account, store_dir FROM conversations \
             ORDER BY first_seen_at, rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Current {
                    conversation_id: r.get(1)?,
                    spike_account: r.get(2)?,
                    store_dir: r.get(3)?,
                },
            ))
        })?;
        for row in rows {
            let (sid, cur) = row?;
            all_conversations.push(cur.conversation_id.clone());
            current.insert(sid, cur);
        }
        let all_sessions = src
            .prepare("SELECT id FROM sessions")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            src,
            env,
            current,
            all_conversations,
            all_sessions,
            notes: Vec::new(),
            tables: Vec::new(),
            dirty: false,
        })
    }

    fn local(&self, t: i64) -> String {
        utc_to_spike_local(UtcMs(t), &self.env.tz)
    }

    fn local_opt(&self, t: Option<i64>) -> Option<String> {
        t.map(|t| self.local(t))
    }

    fn note(&mut self, level: Level, table: &'static str, key: &str, reason: &'static str) {
        if level == Level::Warn {
            self.dirty = true;
        }
        self.notes.push(RecordNote {
            table,
            key: key.to_string(),
            level,
            reason,
        });
    }

    fn end_record(&mut self) -> usize {
        std::mem::take(&mut self.dirty) as usize
    }

    fn finish(&mut self, table: &'static str, source: usize, exported: usize, warned: usize) {
        self.tables.push(TableCounts {
            table,
            source,
            imported: exported,
            warned,
        });
    }

    fn count(&self, table: &str) -> Result<usize, ExportError> {
        let n: i64 = self
            .src
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// The conversation id the spike knows a v1 session by.
    fn conversation_of(&self, session: &str) -> Option<&str> {
        self.current
            .get(session)
            .map(|c| c.conversation_id.as_str())
    }

    /// An ask's `from`/`to`: `moa`, or the session's current conversation id.
    fn target(&self, raw: Option<&str>) -> Option<String> {
        match raw? {
            "moa" => Some("moa".to_string()),
            s => self.conversation_of(s).map(str::to_string),
        }
    }

    /// An account name as the spike can launch it: `codex`, the account's store folder, or the
    /// name itself when nothing is known about it.
    fn account_string(&self, name: &str) -> String {
        if name == "codex" {
            return name.to_string();
        }
        match self.env.accounts.store_for(name) {
            Some(store) => abs(store).to_string_lossy().into_owned(),
            None => name.to_string(),
        }
    }

    fn run(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        self.projects(tx)?;
        self.sessions(tx)?;
        self.tasks(tx)?;
        self.notes_table(tx)?;
        self.suggestions(tx)?;
        let cursor = self.events(tx)?;
        self.asks(tx)?;
        self.groups(tx)?;
        self.activity(tx)?;
        self.meta(tx, cursor)?;
        for (table, key, reason) in NOT_EXPORTED {
            self.note(Level::Info, table, key, reason);
        }
        Ok(())
    }

    fn meta(&mut self, tx: &Transaction, cursor: Option<i64>) -> Result<(), ExportError> {
        let put = |key: &str, value: String| -> Result<(), ExportError> {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        };
        let mut written = 0;
        if let Some(c) = cursor {
            put("cursor", c.to_string())?;
            written += 1;
        }
        let thread: Option<(String, String, String)> = self
            .src
            .query_row(
                "SELECT thread_id, account, day FROM moa_threads \
                 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((sid, account, day)) = thread {
            // Not canonicalised: the spike compares this text as it is.
            let value = serde_json::json!({
                "sid": sid, "date": day, "account": self.account_string(&account),
            });
            put("console_thread", value.to_string())?;
            written += 1;
        }
        let skipped = match &self.env.setup_skipped {
            Some(names) => names.clone(),
            None => {
                let summary: Option<String> = self
                    .src
                    .query_row(
                        "SELECT value FROM meta WHERE key = 'spike_import'",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?;
                summary
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| serde_json::from_value(v["setup_skipped"].clone()).ok())
                    .unwrap_or_default()
            }
        };
        if !skipped.is_empty() {
            put("setup_skipped", serde_json::json!(skipped).to_string())?;
            written += 1;
        }
        let source = self.count("meta")?;
        self.finish("meta", source, written, 0);
        Ok(())
    }
}
