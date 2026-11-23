//! Recovery: putting the core right after a failed commit, a dead store or a panic.

use std::sync::atomic::Ordering;

use moa_core::UtcMs;
use moa_core::effect::{Input, Publish, RefuseWrites};
use moa_core::op::OpError;
use moa_core::publish::CoreEvent;
use moa_store::Store;
use moa_store::actor::{CommitError, StoreHandle};

use super::drain::{BUG_LOADING, Core, Flow, Stepped};
use super::*;

impl Core {
    /// Puts the core right after a failed commit, a dead store or a panic: tells the drain's
    /// callers, then reloads the in-memory state from disk. The session events kept after
    /// `unsaved` (an event mark) are for changes the reload undoes: they go first, so the
    /// reload publishes none of them. `Err` is the line the daemon stops with.
    pub(super) async fn recover(
        &mut self,
        failed: Option<CommitError>,
        panicked: bool,
        unsaved: u64,
        held: &mut Vec<Held>,
    ) -> Result<(), String> {
        // Reloading lifts a refusal unless it is handed back.
        let mut refuse = self.state.refuse_writes().cloned();
        let mut restarted = false;
        match failed {
            Some(CommitError::Failed(why)) => {
                self.stats.commit_failures.fetch_add(1, Ordering::SeqCst);
                self.failures_in_row += 1;
                tracing::error!("a commit failed: {why}");
                fail_held(
                    held,
                    format!("moa couldn't save that ({why}); nothing was changed, try again"),
                );
                held.push(error_event(
                    "moa couldn't save a change; it reloaded from disk",
                ));
                refuse = self.backoff(&why);
            }
            Some(CommitError::Stopped) => {
                tracing::error!("the store thread stopped");
                fail_held(
                    held,
                    "moa couldn't save that: the store stopped".to_string(),
                );
                self.reopen_store().await?;
                restarted = true;
            }
            None => {}
        }
        if panicked {
            held.push(error_event("moa hit a bug and reloaded"));
        }
        self.state.drop_events_after(unsaved);
        self.reload(refuse, held, restarted).await?;
        // A status asked in the failed drain shows the reloaded state, not the failed one.
        let now = self.clock.wall();
        for h in held.iter_mut() {
            if let Held::Status(_, data, caller) = h {
                *data = self.state.status(caller, now);
            }
        }
        Ok(())
    }

    /// How long writes are refused after this failure, if they are: nothing for the first
    /// two in a row, then 1 s, 2 s, 4 s ... up to 30 s.
    pub(super) fn backoff(&self, why: &str) -> Option<RefuseWrites> {
        let n = self.failures_in_row;
        if n < 3 {
            return None;
        }
        let secs = (1u64 << (n - 3).min(5)).min(30);
        let wait = i64::try_from(secs * 1000).unwrap_or(i64::MAX);
        Some(RefuseWrites {
            until: UtcMs(self.clock.wall().0.saturating_add(wait)),
            reason: why.to_string(),
        })
    }

    /// Loads the store's state back into the core and publishes the whole model. When the
    /// store can't even snapshot, its thread is restarted once. A reload doesn't count as a
    /// failure.
    pub(super) async fn reload(
        &mut self,
        refuse_writes: Option<RefuseWrites>,
        held: &mut Vec<Held>,
        mut restarted: bool,
    ) -> Result<(), String> {
        let snapshot = match self.store().snapshot().await {
            Ok(s) => s,
            Err(_) => {
                self.reopen_store().await?;
                restarted = true;
                self.store()
                    .snapshot()
                    .await
                    .map_err(|e| cannot_open(&e.to_string()))?
            }
        };
        let input = Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes,
        };
        // A reload persists nothing.
        let mut scratch = Vec::new();
        if !matches!(self.step(input, &mut scratch, held), Stepped::Done) {
            return Err(BUG_LOADING.to_string());
        }
        self.generation_tx.send_modify(|g| *g += 1);
        self.stats.reloads.fetch_add(1, Ordering::SeqCst);
        if restarted {
            // The message stays out: it may carry contents.
            let input = Input::ActorFailed {
                actor: "the store",
                message: String::new(),
            };
            let _ = self.step(input, &mut scratch, held);
        }
        Ok(())
    }

    /// Opens the database again on a new store thread. `Err` is the line to stop with.
    pub(super) async fn reopen_store(&mut self) -> Result<(), String> {
        let (db, backups) = (self.db.clone(), self.backups_dir.clone());
        let opened = tokio::task::spawn_blocking(move || Store::open(&db, &backups)).await;
        let store = match opened {
            Ok(Ok(store)) => store,
            Ok(Err(e)) => return Err(cannot_open(&e.to_string())),
            Err(e) => return Err(cannot_open(&e.to_string())),
        };
        *lock(&self.store) = StoreHandle::spawn(store, self.backups_dir.clone());
        Ok(())
    }

    /// The core is done for: say why once (event and log), answer what the drain held, and
    /// let the loop end. A held error becomes `line`; a held `Ok` stays: its commit already
    /// happened (`fail_held` has turned the replies of a drain that didn't save into errors).
    pub(super) fn fatal(&mut self, line: String, held: Vec<Held>) -> Flow {
        tracing::error!("the core stops: {line}");
        // Before anything is dropped: a `start()` woken by a dropped `committed()` must find it.
        self.finished_tx.send_replace(Some(Err(line.clone())));
        let _ = self.events.send(CoreEvent::Error {
            message: line.clone(),
        });
        for h in held {
            if let Held::Reply { conn, req, result } = h {
                let result = result.map_err(|_| OpError(line.clone()));
                reply_to(&self.conns, conn, req, result);
            }
        }
        Flow::Fatal
    }
}

pub(super) fn error_event(message: &str) -> Held {
    Held::Publish(Publish::Event(CoreEvent::Error {
        message: message.to_string(),
    }))
}

/// The drain's callers are told it wasn't saved, and nothing it would have said is said:
/// every held reply (a read may have seen the failed state) becomes `line`, and held
/// publishes, views for the terminal server and prompts for moa's agent (whose rows weren't
/// saved) are dropped.
pub(super) fn fail_held(held: &mut Vec<Held>, line: String) {
    held.retain(|h| !matches!(h, Held::Publish(_) | Held::Tmux { .. } | Held::Acp(_)));
    for h in held.iter_mut() {
        if let Held::Reply { result, .. } = h {
            *result = Err(OpError(line.clone()));
        }
    }
}

pub(super) fn cannot_open(why: &str) -> String {
    format!(
        "the store can't be opened: {}",
        why.lines().next().unwrap_or("")
    )
}
