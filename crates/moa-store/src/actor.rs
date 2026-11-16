//! The store actor: one OS thread that owns the read-write connection and runs the commands it
//! is handed, one at a time, in queue order.
//!
//! The thread does no batching or reordering of its own. The core drains its inbox into one
//! batch, hands it to [`StoreHandle::commit`] and waits for the answer, so at most one batch is
//! in flight (group commit). Each command answers on its own oneshot. When the thread dies, its
//! senders drop, so a waiting or later call returns [`CommitError::Stopped`] instead of hanging.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use jiff::civil::Date;
use moa_core::change::Change;
use moa_core::state::Snapshot;
use tokio::sync::oneshot;

use crate::backup::{Pruned, daily_backup, prune_backups};
use crate::{Store, StoreError};

/// Why a call to the store thread failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommitError {
    /// The database refused, as one line without record contents.
    Failed(String),
    /// The thread is gone.
    Stopped,
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(why) => f.write_str(why),
            Self::Stopped => f.write_str("the store thread has stopped"),
        }
    }
}

impl std::error::Error for CommitError {}

/// What a daily backup did.
#[derive(Debug)]
pub struct BackupDone {
    /// The new file, or `None` when the day already had one.
    pub written: Option<PathBuf>,
    pub pruned: Pruned,
}

/// A fault to inject, for tests. It is a queued command, so it applies to the next commit
/// after it.
#[doc(hidden)]
pub enum Fault {
    /// The next commit fails as if SQLite refused it, writing nothing.
    FailNextCommit,
    /// The next commit waits until the receiver gets a message (or its sender is dropped).
    PauseNextCommit(mpsc::Receiver<()>),
    /// The next backup waits until the receiver gets a message (or its sender is dropped).
    PauseNextBackup(mpsc::Receiver<()>),
    /// The thread panics.
    Crash,
}

type Reply<T> = oneshot::Sender<Result<T, CommitError>>;

enum Cmd {
    Commit(Vec<Change>, Reply<()>),
    Snapshot(Reply<Snapshot>),
    Backup(Date, Reply<BackupDone>),
    Sync(oneshot::Sender<()>),
    Fault(Fault),
}

/// A handle to the store thread. Cloning it shares the thread; it stops when every handle is
/// dropped.
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::Sender<Cmd>,
}

/// The first line of the error's text, so a reply or log carries no more than one.
fn one_line(e: &StoreError) -> CommitError {
    let text = e.to_string();
    CommitError::Failed(text.lines().next().unwrap_or("").to_string())
}

impl StoreHandle {
    /// Starts the thread. `backups` is where [`StoreHandle::backup`] writes.
    pub fn spawn(store: Store, backups: PathBuf) -> StoreHandle {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("moa-store".into())
            .spawn(move || run(store, &backups, &rx))
            .expect("the OS refused a thread");
        StoreHandle { tx }
    }

    async fn ask<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, CommitError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(make(reply))
            .map_err(|_| CommitError::Stopped)?;
        answer.await.map_err(|_| CommitError::Stopped)?
    }

    /// Writes the changes in one transaction: all of them or none.
    pub async fn commit(&self, changes: Vec<Change>) -> Result<(), CommitError> {
        self.ask(|r| Cmd::Commit(changes, r)).await
    }

    /// Loads what the core starts from.
    pub async fn snapshot(&self) -> Result<Snapshot, CommitError> {
        self.ask(Cmd::Snapshot).await
    }

    /// Writes `daily-<day>.db` unless it exists, then prunes old dailies.
    pub async fn backup(&self, day: Date) -> Result<BackupDone, CommitError> {
        self.ask(|r| Cmd::Backup(day, r)).await
    }

    /// A no-op that answers in queue order: when it returns, every command sent before it has
    /// run. It also returns when the thread is gone.
    pub async fn sync(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Cmd::Sync(reply)).is_ok() {
            let _ = answer.await;
        }
    }

    /// Tests only.
    #[doc(hidden)]
    pub fn fault(&self, f: Fault) {
        let _ = self.tx.send(Cmd::Fault(f));
    }
}

fn run(mut store: Store, backups: &Path, rx: &mpsc::Receiver<Cmd>) {
    let mut fail_next = false;
    let mut pause_next: Option<mpsc::Receiver<()>> = None;
    let mut pause_backup: Option<mpsc::Receiver<()>> = None;
    // A reply to a caller that has gone fails to send; that never stops the thread.
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Commit(changes, reply) => {
                if let Some(gate) = pause_next.take() {
                    let _ = gate.recv();
                }
                let result = if std::mem::take(&mut fail_next) {
                    Err(CommitError::Failed(
                        "the database refused the commit".into(),
                    ))
                } else {
                    store.apply(&changes).map_err(|e| one_line(&e))
                };
                let _ = reply.send(result);
            }
            Cmd::Snapshot(reply) => {
                let _ = reply.send(store.load_snapshot().map_err(|e| one_line(&e)));
            }
            Cmd::Backup(day, reply) => {
                if let Some(gate) = pause_backup.take() {
                    let _ = gate.recv();
                }
                let _ = reply.send(backup(&store, backups, day));
            }
            Cmd::Sync(reply) => {
                let _ = reply.send(());
            }
            Cmd::Fault(Fault::FailNextCommit) => fail_next = true,
            Cmd::Fault(Fault::PauseNextCommit(gate)) => pause_next = Some(gate),
            Cmd::Fault(Fault::PauseNextBackup(gate)) => pause_backup = Some(gate),
            Cmd::Fault(Fault::Crash) => panic!("the store thread was told to crash"),
        }
    }
}

fn backup(store: &Store, dir: &Path, day: Date) -> Result<BackupDone, CommitError> {
    let written = daily_backup(store, dir, day).map_err(|e| one_line(&e))?;
    let pruned = prune_backups(dir).map_err(|e| {
        CommitError::Failed(format!("old backups could not be listed: {}", e.kind()))
    })?;
    Ok(BackupDone { written, pruned })
}
