//! What the core does when a commit fails, the store dies, or `handle` panics.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_core::effect::{TmuxIn, TmuxToken};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId, TaskAdd, TaskSet};
use moa_core::publish::{CoreEvent, Topic};
use moa_core::state::{CallerInfo, Deps};
use moa_core::tmux::ids::{ClientName, Generation};
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use tokio::sync::{broadcast, mpsc as tmpsc};
use tokio::time::timeout;

type Reply = (ReqId, Result<Done, OpError>);

const RETRY: &str = "nothing was changed, try again";
const BUG: &str = "moa hit a bug answering that; it reloaded";

async fn within<T>(f: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out after 5 s")
}

struct Rig {
    brain: TypicalBrain,
    time: Arc<FakeTime>,
    core: CoreHandle,
}

async fn rig() -> Rig {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let opts = CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps::default(),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 256,
    };
    let core = within(CoreHandle::start(opts))
        .await
        .expect("the core starts");
    Rig { brain, time, core }
}

/// A brain whose store refuses every write, and a grammar that rejects every suggestion
/// action, so `Startup` itself writes (it flags suggestion #1) and fails.
async fn rig_read_only() -> Rig {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let store = brain.store();
    store.conn().execute_batch("PRAGMA query_only = 1").unwrap();
    let deps = Deps {
        parse_action: Some(|_, _| Err("no".into())),
        ..Deps::default()
    };
    let core = within(CoreHandle::start_with_store(
        CoreOptions {
            brain: BrainName::default_brain(),
            config: brain.config(),
            deps,
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: time.clone(),
            alarm: Box::new(time.alarm()),
            db: brain.db.clone(),
            backups: brain.dir.path().join("backups"),
            events_capacity: 256,
        },
        store,
    ))
    .await
    .expect("the core starts");
    Rig { brain, time, core }
}

fn send(core: &CoreHandle, c: ConnId, req: u64, op: Op) {
    core.send(c, ReqId(req), CallerInfo::default(), op);
}

fn group_add(name: &str) -> Op {
    Op::GroupAdd { path: name.into() }
}

fn task_ls() -> Op {
    Op::TaskLs {
        project: None,
        all: false,
    }
}

async fn reply(rx: &mut tmpsc::UnboundedReceiver<Reply>) -> Result<Done, OpError> {
    within(rx.recv()).await.expect("a reply").1
}

async fn err_text(rx: &mut tmpsc::UnboundedReceiver<Reply>) -> String {
    reply(rx).await.expect_err("an error").0
}

fn pause(core: &CoreHandle) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();
    core.store_fault(Fault::PauseNextCommit(rx));
    tx
}

async fn wait_until(mut ok: impl FnMut() -> bool) {
    within(async {
        while !ok() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// The next `Error` event's text.
async fn error_event(rx: &mut broadcast::Receiver<CoreEvent>) -> String {
    within(async {
        loop {
            if let CoreEvent::Error { message } = rx.recv().await.expect("an event") {
                return message;
            }
        }
    })
    .await
}

fn groups_in_model(core: &CoreHandle) -> BTreeSet<String> {
    core.model().borrow().topics[&Topic::Groups]
        .keys()
        .cloned()
        .collect()
}

fn groups_on_disk(rig: &Rig) -> BTreeSet<String> {
    let store = Store::open_read_only(&rig.brain.db).unwrap();
    store
        .load_snapshot()
        .unwrap()
        .groups
        .into_iter()
        .map(|g| g.path)
        .collect()
}

#[tokio::test]
async fn a_failed_commit_fails_the_reply_and_reloads() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    let mut events = rig.core.events();
    let generation = rig.core.generation();
    let before = *generation.borrow();
    rig.core.store_fault(Fault::FailNextCommit);
    send(&rig.core, c, 1, group_add("Work"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("moa couldn't save that ("), "{said}");
    assert!(
        said.ends_with("); nothing was changed, try again"),
        "{said}"
    );
    assert_eq!(
        error_event(&mut events).await,
        "moa couldn't save a change; it reloaded from disk"
    );
    assert!(*generation.borrow() > before);
    send(
        &rig.core,
        c,
        2,
        Op::GroupRename {
            old: "Work".into(),
            new: "X".into(),
        },
    );
    assert_eq!(err_text(&mut rx).await, "no group Work");
    send(&rig.core, c, 3, Op::Brief(Default::default()));
    reply(&mut rx).await.expect("brief still answers");
    assert!(!groups_in_model(&rig.core).contains("Work"));
}

#[tokio::test]
async fn writes_queued_behind_a_failed_commit_fail_too() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let (c, mut rx) = rig.core.open_conn();
    let commits = stats.commits.load(Ordering::SeqCst);
    let release = pause(&rig.core);
    send(&rig.core, c, 1, group_add("Primer"));
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    rig.core.store_fault(Fault::FailNextCommit);
    let inputs = stats.inputs.load(Ordering::SeqCst);
    send(
        &rig.core,
        c,
        2,
        Op::TaskAdd(TaskAdd {
            project: "Lantern".into(),
            title: "Solder".into(),
            ..TaskAdd::default()
        }),
    );
    send(&rig.core, c, 3, task_ls());
    settle().await;
    // Both are queued, not run.
    assert_eq!(stats.inputs.load(Ordering::SeqCst), inputs);
    release.send(()).unwrap();
    reply(&mut rx).await.expect("Primer is ok");
    for _ in 0..2 {
        let said = err_text(&mut rx).await;
        assert!(said.starts_with("moa couldn't save that ("), "{said}");
        assert!(said.ends_with(RETRY), "{said}");
    }
    send(
        &rig.core,
        c,
        4,
        Op::TaskSet(TaskSet {
            id: 6,
            status: Some("done".into()),
            ..TaskSet::default()
        }),
    );
    assert_eq!(err_text(&mut rx).await, "no task #6");
    send(&rig.core, c, 5, task_ls());
    let listed = reply(&mut rx).await.expect("task ls answers");
    assert!(!listed.text.contains("Solder"), "{}", listed.text);
}

#[tokio::test]
async fn a_commit_that_keeps_failing_backs_off_and_refuses_writes() {
    let rig = rig_read_only().await;
    let stats = rig.core.stats();
    let (c, mut rx) = rig.core.open_conn();
    // Startup's flagging commit was failure 1.
    assert_eq!(stats.commit_failures.load(Ordering::SeqCst), 1);
    for req in 1..=2 {
        send(&rig.core, c, req, group_add(&format!("G{req}")));
        let said = err_text(&mut rx).await;
        assert!(said.starts_with("moa couldn't save that ("), "{said}");
    }
    let commits = stats.commits.load(Ordering::SeqCst);
    send(&rig.core, c, 3, group_add("G3"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("moa can't save right now ("), "{said}");
    assert!(said.ends_with("); nothing was changed"), "{said}");
    assert_eq!(stats.commits.load(Ordering::SeqCst), commits);
    send(&rig.core, c, 4, task_ls());
    reply(&mut rx).await.expect("reads keep answering");
    assert_eq!(stats.reloads.load(Ordering::SeqCst), 3);

    // One second later the next write is tried, fails, and the refusal doubles.
    rig.time.advance(Duration::from_secs(1));
    send(&rig.core, c, 5, group_add("G5"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("moa couldn't save that ("), "{said}");
    assert_eq!(stats.commits.load(Ordering::SeqCst), commits + 1);
    rig.time.advance(Duration::from_secs(1));
    send(&rig.core, c, 6, group_add("G6"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("moa can't save right now ("), "{said}");
    assert_eq!(stats.commits.load(Ordering::SeqCst), commits + 1);
    rig.time.advance(Duration::from_secs(1));
    send(&rig.core, c, 7, group_add("G7"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("moa couldn't save that ("), "{said}");
    assert_eq!(stats.commits.load(Ordering::SeqCst), commits + 2);
}

#[tokio::test]
async fn the_store_thread_is_restarted() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    let mut events = rig.core.events();
    rig.core.store_fault(Fault::Crash);
    send(&rig.core, c, 1, group_add("Work"));
    match reply(&mut rx).await {
        Ok(_) => {}
        Err(OpError(said)) => assert_eq!(said, "moa couldn't save that: the store stopped"),
    }
    send(&rig.core, c, 2, group_add("Other"));
    reply(&mut rx).await.expect("the restarted store saves");
    assert_eq!(
        error_event(&mut events).await,
        "the store stopped and was restarted"
    );
    assert!(groups_on_disk(&rig).contains("Other"));
    // The handle's faults reach the new thread.
    rig.core.store_fault(Fault::FailNextCommit);
    send(&rig.core, c, 3, group_add("Third"));
    assert!(
        err_text(&mut rx)
            .await
            .starts_with("moa couldn't save that (")
    );
}

#[tokio::test]
async fn a_panicking_handle_is_reported_and_the_core_keeps_serving() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    let mut events = rig.core.events();
    let generation = rig.core.generation();
    let before = *generation.borrow();
    send(&rig.core, c, 1, Op::DebugPanic);
    assert_eq!(err_text(&mut rx).await, BUG);
    assert_eq!(error_event(&mut events).await, "moa hit a bug and reloaded");
    assert!(*generation.borrow() > before);
    send(&rig.core, c, 2, Op::Brief(Default::default()));
    reply(&mut rx).await.expect("brief still answers");
}

#[tokio::test]
async fn a_panic_in_the_backup_timer_keeps_it_armed() {
    let rig = rig().await;
    let generation = rig.core.generation();
    let before = *generation.borrow();
    let armed = rig.time.armed().expect("the backup is armed");
    rig.core.panic_next_handle();
    rig.time.advance(Duration::from_millis(
        u64::try_from(armed.0 - rig.brain.now().wall.0).unwrap(),
    ));
    wait_until(|| *generation.borrow() > before).await;
    within(rig.core.barrier()).await;
    assert_eq!(rig.time.armed(), Some(UtcMs(armed.0 + 24 * 3600 * 1000)));
}

#[tokio::test]
async fn a_panic_mid_drain_keeps_the_earlier_writes() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let start = groups_in_model(&rig.core);
    let (c, mut rx) = rig.core.open_conn();
    let commits = stats.commits.load(Ordering::SeqCst);
    let release = pause(&rig.core);
    send(&rig.core, c, 1, group_add("Primer"));
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    send(&rig.core, c, 2, group_add("Kept"));
    send(&rig.core, c, 3, Op::DebugPanic);
    send(&rig.core, c, 4, group_add("After"));
    release.send(()).unwrap();
    reply(&mut rx).await.expect("Primer is ok");
    reply(&mut rx).await.expect("Kept is ok");
    assert_eq!(err_text(&mut rx).await, BUG);
    reply(&mut rx).await.expect("After is ok");
    within(rig.core.barrier()).await;
    let mut want = start;
    want.extend(["Primer", "Kept", "After"].map(String::from));
    assert_eq!(groups_in_model(&rig.core), want);
    assert_eq!(groups_on_disk(&rig), want);
}

#[tokio::test]
async fn a_good_commit_resets_the_failure_count() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    // Fail, succeed, fail, succeed, fail: never three in a row, so never refused.
    for round in 0..3u64 {
        rig.core.store_fault(Fault::FailNextCommit);
        send(&rig.core, c, round * 2, group_add(&format!("Bad{round}")));
        let said = err_text(&mut rx).await;
        assert!(said.starts_with("moa couldn't save that ("), "{said}");
        if round < 2 {
            send(
                &rig.core,
                c,
                round * 2 + 1,
                group_add(&format!("Good{round}")),
            );
            reply(&mut rx).await.expect("a good write saves");
        }
    }
    // Three failures in all, not in a row: the next write is tried, not refused.
    send(&rig.core, c, 99, group_add("Last"));
    reply(&mut rx).await.expect("not refused");
}

#[tokio::test]
async fn stop_ends_finished_with_ok() {
    let rig = rig().await;
    rig.core.stop().await;
    within(rig.core.finished()).await.expect("a normal stop");
}

const BUG_LOADING: &str = "moa hit a bug loading the brain; see the log";

fn fatal_line(e: Result<(), moa_daemon::DaemonError>) -> String {
    match e.expect_err("the core ended with an error") {
        moa_daemon::DaemonError::Fatal(line) => line,
        other => panic!("not a fatal stop: {other}"),
    }
}

// Multi-thread, so `start()` can wake on another worker while the core task is still ending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_startup_fails_start() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let deps = Deps {
        parse_action: Some(|_, _| panic!("x")),
        ..Deps::default()
    };
    let started = within(CoreHandle::start(CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps,
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 256,
    }))
    .await;
    match started {
        Err(moa_daemon::DaemonError::Fatal(line)) => assert_eq!(line, BUG_LOADING),
        Err(other) => panic!("not a fatal stop: {other}"),
        Ok(_) => panic!("start should fail"),
    }
}

#[tokio::test]
async fn a_panic_while_reloading_stops_the_core() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    let mut events = rig.core.events();
    // The input panics, and so does the reload that follows it.
    rig.core.panic_next_handles(2);
    send(&rig.core, c, 1, group_add("Work"));
    assert_eq!(err_text(&mut rx).await, BUG_LOADING);
    assert_eq!(error_event(&mut events).await, BUG_LOADING);
    assert_eq!(fatal_line(within(rig.core.finished()).await), BUG_LOADING);
    send(&rig.core, c, 2, group_add("Later"));
    // Either told to try again (the core was still sweeping its inbox) or the connection is
    // already closed; never an answer, never a hang.
    match within(rx.recv()).await {
        None => {}
        Some((_, result)) => assert_eq!(result.unwrap_err().0, "moa is stopping; try again"),
    }
}

#[tokio::test]
async fn a_store_that_cannot_be_reopened_stops_the_core() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    let mut events = rig.core.events();
    rig.core.store_fault(Fault::Crash);
    // The reopen finds something that is not a brain.
    let db = &rig.brain.db;
    std::fs::write(db, b"not a database").unwrap();
    for ext in ["-wal", "-shm"] {
        let mut side = db.clone().into_os_string();
        side.push(ext);
        let _ = std::fs::remove_file(side);
    }
    send(&rig.core, c, 1, group_add("Work"));
    let said = err_text(&mut rx).await;
    assert!(said.starts_with("the store can't be opened: "), "{said}");
    let event = error_event(&mut events).await;
    assert!(event.starts_with("the store can't be opened: "), "{event}");
    let line = fatal_line(within(rig.core.finished()).await);
    assert!(line.starts_with("the store can't be opened: "), "{line}");
}

#[tokio::test]
async fn a_core_that_dies_unexpectedly_says_so_and_closes_connections() {
    let rig = rig().await;
    let (_c, mut rx) = rig.core.open_conn();
    rig.core.crash_core();
    assert_eq!(
        fatal_line(within(rig.core.finished()).await),
        "the core stopped unexpectedly; see the log"
    );
    // The connection's channel is closed, so a client sees the end instead of waiting.
    assert!(within(rx.recv()).await.is_none());
}

fn link_up() -> TmuxIn {
    TmuxIn::Connected {
        conn: 1,
        generation: Generation { pid: 1, start: 1 },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
    }
}

/// Someone attached: the clock's first write.
fn lists_with_a_user() -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: vec![
            Ok(vec!["S $0 moa".into()]),
            Ok(vec![]),
            Ok(vec!["C 0 $0 /dev/ttys1".into()]),
        ],
    }
}

// There is no terminal link in the daemon yet, so a released view has no observable but
// the counter: it counts views let go after a commit, dropped or not.
#[tokio::test]
async fn a_status_write_waits_for_the_commit_and_is_dropped_when_it_fails() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let (c, mut rx) = rig.core.open_conn();
    // The daemon's own "no link" answer is in before the test's link comes up.
    rig.core.barrier().await;
    let commits = stats.commits.load(Ordering::SeqCst);
    let release = pause(&rig.core);
    send(&rig.core, c, 1, group_add("Primer"));
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    rig.core.store_fault(Fault::FailNextCommit);
    // Queued behind the primer: a write that fails, and the link coming up (a status
    // write that shows state the failed commit never saved).
    send(&rig.core, c, 2, group_add("Doomed"));
    rig.core.tmux_in(link_up());
    release.send(()).unwrap();
    reply(&mut rx).await.expect("Primer is ok");
    assert!(
        err_text(&mut rx)
            .await
            .starts_with("moa couldn't save that (")
    );
    rig.core.barrier().await;
    // The write of the link coming up was dropped with the failed commit; the one
    // released is the reload's, which writes everything again from the saved state.
    assert_eq!(stats.views_released.load(Ordering::SeqCst), 1);

    // A write after a good commit is released.
    rig.core.tmux_in(lists_with_a_user());
    rig.core.barrier().await;
    assert_eq!(stats.views_released.load(Ordering::SeqCst), 2);
}
