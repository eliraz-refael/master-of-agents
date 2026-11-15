//! `Store::load_snapshot` and `Store::apply`: the store loads what the core starts from and
//! writes the same [`Change`]s the core applies in memory, so memory and disk change by one value.

use moa_core::change::Change;
use moa_core::state::{LoadNote, NextIds, Snapshot};
use rusqlite::TransactionBehavior;

use crate::import::{ImportError, Src};
use crate::{Store, StoreError, rows};

/// How many of the newest activity rows a snapshot carries (what the state keeps).
const ACTIVITY_TAIL: usize = 300;

fn store_error(e: ImportError) -> StoreError {
    match e {
        ImportError::Store(e) => e,
        ImportError::Sqlite(e) => StoreError::Sqlite(e),
        ImportError::Io(e) => StoreError::Io(e),
        // The two import-only cases come from the spike file, which `Src` never opens here.
        ImportError::SpikeMissing(p) => StoreError::Missing(p),
        ImportError::NotEmpty => StoreError::NotABrain,
    }
}

impl Store {
    /// Every row of every table except `events` and `feed` (the core doesn't hold them), the
    /// newest 300 activity rows, and the next id of each integer-id table. Text this build
    /// doesn't know loads as the import's default and is listed in `unknown`.
    pub fn load_snapshot(&self) -> Result<Snapshot, StoreError> {
        let src = Src { conn: self.conn() };
        let rows_of = |table: &str, cols: &'static [&'static str], order: &str| {
            src.rows(table, cols, order).map_err(store_error)
        };
        let mut unknown: Vec<LoadNote> = Vec::new();
        let n = &mut unknown;

        let projects = rows_of("projects", rows::PROJECTS, "id")?
            .iter()
            .map(|r| rows::project(r, n))
            .collect();
        let tasks = rows_of("tasks", rows::TASKS, "id")?
            .iter()
            .map(|r| rows::task(r, n))
            .collect();
        let notes = rows_of("notes", rows::NOTES, "id")?
            .iter()
            .map(|r| rows::note(r, n))
            .collect();
        let suggestions = rows_of("suggestions", rows::SUGGESTIONS, "id")?
            .iter()
            .map(|r| rows::suggestion(r, n))
            .collect();
        let sessions = rows_of("sessions", rows::SESSIONS, "rowid")?
            .iter()
            .filter_map(|r| rows::session(r, n))
            .collect();
        let conversations = rows_of(
            "conversations",
            rows::CONVERSATIONS,
            "agent_kind, conversation_id",
        )?
        .iter()
        .filter_map(|r| rows::conversation(r, n))
        .collect();
        let groups = rows_of("groups", rows::GROUPS, "path")?
            .iter()
            .map(|r| rows::group(r, n))
            .collect();
        let asks = rows_of("asks", rows::ASKS, "id")?
            .iter()
            .map(|r| rows::ask(r, n))
            .collect();
        let pane_bindings = rows_of("pane_bindings", rows::PANE_BINDINGS, "session_id")?
            .iter()
            .filter_map(|r| rows::pane_binding(r, n))
            .collect();
        let spike_panes = rows_of("spike_panes", rows::SPIKE_PANES, "session_id")?
            .iter()
            .filter_map(|r| rows::spike_pane(r, n))
            .collect();
        let threads = rows_of("moa_threads", rows::MOA_THREADS, "thread_id")?
            .iter()
            .filter_map(|r| rows::thread(r, n))
            .collect();
        let nags = rows_of("nags", rows::NAGS, "task_id, due, day")?
            .iter()
            .filter_map(|r| rows::nag(r, n))
            .collect();
        let mut activity_tail: Vec<_> = rows_of(
            "activity",
            rows::ACTIVITY,
            &format!("id DESC LIMIT {ACTIVITY_TAIL}"),
        )?
        .iter()
        .map(|r| rows::activity(r, n))
        .collect();
        activity_tail.reverse();

        let meta = self
            .conn()
            .prepare("SELECT key, value FROM meta ORDER BY key")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Snapshot {
            projects,
            tasks,
            notes,
            suggestions,
            sessions,
            conversations,
            groups,
            asks,
            pane_bindings,
            spike_panes,
            threads,
            nags,
            activity_tail,
            meta,
            next_ids: self.next_ids()?,
            unknown,
        })
    }

    fn next_ids(&self) -> Result<NextIds, StoreError> {
        let next = |table: &str| -> Result<i64, StoreError> {
            Ok(self.conn().query_row(
                &format!("SELECT coalesce(max(id), 0) + 1 FROM {table}"),
                [],
                |r| r.get(0),
            )?)
        };
        Ok(NextIds {
            project: next("projects")?,
            task: next("tasks")?,
            note: next("notes")?,
            suggestion: next("suggestions")?,
            ask: next("asks")?,
            event: next("events")?,
            activity: next("activity")?,
            feed: next("feed")?,
        })
    }

    /// Writes the changes in one transaction: all of them or none. Each upsert is
    /// `INSERT ... ON CONFLICT(<key>) DO UPDATE`, so rows that point at an updated row stay linked.
    pub fn apply(&mut self, changes: &[Change]) -> Result<(), StoreError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for change in changes {
            rows::write(&tx, change)?;
        }
        tx.commit()?;
        Ok(())
    }
}
