//! The store actor: one transaction per commit, answers in queue order, `Stopped` on death,
//! and daily backups driven through the thread.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use jiff::ToSpan;
use jiff::civil::{Date, date};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::model::*;
use moa_store::Store;
use moa_store::actor::{CommitError, Fault, StoreHandle};
use moa_store::backup::backups_to_delete;

const PATIENCE: Duration = Duration::from_secs(10);

struct Rig {
    dir: tempfile::TempDir,
    handle: StoreHandle,
}

fn rig() -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let backups = dir.path().join("backups");
    let store = Store::open(&dir.path().join("moa.db"), &backups).unwrap();
    let handle = StoreHandle::spawn(store, backups);
    Rig { dir, handle }
}

fn project(id: i64) -> Change {
    Change::Project(Project {
        id: ProjectId(id),
        name: "Lantern".into(),
        path: None,
        status: ProjectStatus::Paused,
        summary: None,
        last_activity_on: None,
        created_at: UtcMs(10),
        updated_at: UtcMs(11),
    })
}

fn task(id: i64, project_id: i64) -> Change {
    Change::Task(Task {
        id: TaskId(id),
        project_id: Some(ProjectId(project_id)),
        title: "Trim the wick".into(),
        status: TaskStatus::Blocked,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: None,
        priority: None,
        created_at: UtcMs(30),
        updated_at: UtcMs(31),
        done_at: None,
    })
}

fn count(db: &Path, table: &str) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn db(rig: &Rig) -> std::path::PathBuf {
    rig.dir.path().join("moa.db")
}

#[tokio::test]
async fn a_commit_is_one_transaction() {
    let rig = rig();
    let got = rig.handle.commit(vec![project(1), task(1, 999)]).await;
    assert!(matches!(got, Err(CommitError::Failed(_))), "{got:?}");
    assert_eq!(count(&db(&rig), "projects"), 0);
    assert_eq!(count(&db(&rig), "tasks"), 0);
}

/// `FailNextCommit` answers before anything is applied, so this can't show that a commit
/// failing partway rolls back (`a_commit_is_one_transaction` does). It pins the rest of the
/// contract: the caller hears `Failed`, nothing is on disk, and the store keeps working.
#[tokio::test]
async fn an_injected_failure_answers_failed_and_the_next_commit_works() {
    let rig = rig();
    rig.handle.fault(Fault::FailNextCommit);
    let changes = vec![project(1), task(1, 1)];
    let got = rig.handle.commit(changes.clone()).await;
    assert!(matches!(got, Err(CommitError::Failed(_))), "{got:?}");
    assert_eq!(count(&db(&rig), "projects"), 0);
    assert_eq!(count(&db(&rig), "tasks"), 0);

    rig.handle.commit(changes).await.unwrap();
    assert_eq!(count(&db(&rig), "projects"), 1);
    assert_eq!(count(&db(&rig), "tasks"), 1);
}

#[tokio::test]
async fn commands_answer_in_queue_order() {
    let rig = rig();
    let (release, gate) = mpsc::channel();
    rig.handle.fault(Fault::PauseNextCommit(gate));
    let mut commit = Box::pin(rig.handle.commit(vec![project(1)]));
    let mut snapshot = Box::pin(rig.handle.snapshot());

    // Both are queued (a future sends on its first poll). The gate holds the commit, so
    // neither can answer: the pause is real, and the snapshot waits behind the commit.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut commit)
            .await
            .is_err(),
        "the commit answered while paused"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut snapshot)
            .await
            .is_err(),
        "the snapshot overtook the paused commit"
    );
    assert_eq!(count(&db(&rig), "projects"), 0);

    release.send(()).unwrap();
    tokio::time::timeout(PATIENCE, &mut commit)
        .await
        .expect("the commit answered after release")
        .unwrap();
    let snapshot = tokio::time::timeout(PATIENCE, &mut snapshot)
        .await
        .expect("the snapshot answered")
        .unwrap();
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].id, ProjectId(1));
}

#[tokio::test]
async fn a_dead_thread_answers_stopped() {
    let rig = rig();
    rig.handle.fault(Fault::Crash);
    let both = tokio::time::timeout(PATIENCE.min(Duration::from_secs(2)), async {
        let commit = rig.handle.commit(vec![project(1)]).await;
        let snapshot = rig.handle.snapshot().await;
        (commit, snapshot)
    })
    .await
    .expect("a dead thread must not hang a call");
    assert_eq!(both.0, Err(CommitError::Stopped));
    assert!(matches!(both.1, Err(CommitError::Stopped)));
    // `sync` returns too.
    tokio::time::timeout(Duration::from_secs(2), rig.handle.sync())
        .await
        .expect("sync on a dead thread returns");
}

#[tokio::test]
async fn backup_writes_today_once_and_prunes() {
    let rig = rig();
    let backups = rig.dir.path().join("backups");
    fs::create_dir_all(&backups).unwrap();
    let today = date(2026, 11, 15);
    let mut dates: Vec<Date> = (1..=20).map(|n| today - (n * 3).days()).collect();
    for d in &dates {
        fs::write(backups.join(format!("daily-{d}.db")), b"old").unwrap();
    }
    dates.push(today);
    let expected = backups_to_delete(&dates);
    assert!(
        !expected.is_empty(),
        "the fixture must make pruning do something"
    );

    let done = rig.handle.backup(today).await.unwrap();
    assert_eq!(done.written, Some(backups.join("daily-2026-11-15.db")));
    assert_eq!(done.pruned.removed.len(), expected.len());
    for d in &expected {
        assert!(!backups.join(format!("daily-{d}.db")).exists(), "{d}");
    }
    assert!(done.pruned.failed.is_empty());

    let again = rig.handle.backup(today).await.unwrap();
    assert_eq!(again.written, None);
}

#[tokio::test]
async fn a_feed_page_is_the_rows_before_oldest_first() {
    let rig = rig();
    let entry = |id: i64| {
        Change::Feed(FeedEntry {
            id,
            thread_id: "th-1".into(),
            at: UtcMs(id),
            kind: "moa".into(),
            message_id: None,
            automatic: false,
            body: "{}".into(),
            state: None,
        })
    };
    let mut changes = vec![Change::Thread(MoaThread {
        thread_id: "th-1".into(),
        account: "acct-a".into(),
        day: date(2026, 11, 20),
        kind: ThreadKind::Daily,
        started_at: UtcMs(1),
    })];
    changes.extend((1..=5).map(entry));
    rig.handle.commit(changes).await.unwrap();

    let ids = |rows: Vec<FeedEntry>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids(rig.handle.feed_page(5, 2).await.unwrap()), [3, 4]);
    assert_eq!(ids(rig.handle.feed_page(3, 200).await.unwrap()), [1, 2]);
    assert!(rig.handle.feed_page(1, 10).await.unwrap().is_empty());
}
