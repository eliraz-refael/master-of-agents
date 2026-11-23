//! `Store::load_snapshot` and `Store::apply`: the store loads what the core starts from and
//! writes the same [`Change`]s the core applies in memory, so memory and disk change by one value.

use moa_core::change::Change;
use moa_core::state::{LoadNote, NextIds, Snapshot};
use rusqlite::TransactionBehavior;

use crate::import::{ImportError, Src};
use crate::{Store, StoreError, rows};

/// How many of the newest activity rows a snapshot carries: what the state keeps.
const ACTIVITY_TAIL: usize = moa_core::publish::ACTIVITY_SHOWN;

/// How many of the newest feed rows a snapshot carries: what the state keeps.
const FEED_TAIL: usize = moa_core::publish::FEED_SHOWN;

/// The threads that are some account's newest (by start, then id): the ones moa can still resume.
const NEWEST_THREADS: &str = "thread_id IN (SELECT t.thread_id FROM moa_threads t WHERE NOT EXISTS \
    (SELECT 1 FROM moa_threads u WHERE u.account = t.account \
     AND (u.started_at, u.thread_id) > (t.started_at, t.thread_id)))";

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
    /// Every row of every table except `events` (the core doesn't hold them), the newest 300
    /// activity rows, the newest 100 feed rows and the `message_id`s of each account's newest
    /// thread, the open terminals, and the next id of each integer-id table
    /// (closed terminals' ids included). Text this build doesn't know loads as the import's
    /// default and is listed in `unknown`.
    pub fn load_snapshot(&self) -> Result<Snapshot, StoreError> {
        // One read transaction, so the rows and the next ids are one moment of the file.
        let read = self.conn().unchecked_transaction()?;
        let src = Src { conn: &read };
        let rows_of = |table: &str, cols: &'static [&'static str], order: &str| {
            src.rows(table, cols, None, order).map_err(store_error)
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
        // Open terminals only: a closed one's row is history, and history keeps growing.
        let terminals = src
            .rows(
                "terminals",
                rows::TERMINALS,
                Some("closed_at IS NULL"),
                "id",
            )
            .map_err(store_error)?
            .iter()
            .map(|r| rows::terminal(r, n))
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

        let mut feed_tail: Vec<_> =
            rows_of("feed", rows::FEED, &format!("id DESC LIMIT {FEED_TAIL}"))?
                .iter()
                .map(rows::feed)
                .collect();
        feed_tail.reverse();
        let feed_keys = src
            .rows(
                "feed",
                &["thread_id", "message_id"],
                Some(&format!("message_id IS NOT NULL AND {NEWEST_THREADS}")),
                "id",
            )
            .map_err(store_error)?
            .iter()
            .filter_map(|r| {
                Some((
                    r.text_cow("thread_id")?.into_owned(),
                    r.text_cow("message_id")?.into_owned(),
                ))
            })
            .collect();

        let meta = rows_of("meta", rows::META, "key")?
            .iter()
            .filter_map(|r| rows::meta(r, n))
            .collect();

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
            terminals,
            activity_tail,
            feed_tail,
            feed_keys,
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
            terminal: next("terminals")?,
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
