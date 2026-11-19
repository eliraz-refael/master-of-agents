use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_core::op::{ConnId, Done, Op, OpError, ReqId, TaskAdd};
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, Deps};
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

type Reply = (ReqId, Result<Done, OpError>);

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
    let core = within(CoreHandle::start(CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps::default(),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 256,
        tmux: None,
        spool: None,
    }))
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

async fn reply(rx: &mut UnboundedReceiver<Reply>) -> Result<Done, OpError> {
    within(rx.recv()).await.expect("a reply").1
}

fn groups_on_disk(rig: &Rig) -> Vec<String> {
    let store = Store::open_read_only(&rig.brain.db).unwrap();
    store
        .load_snapshot()
        .unwrap()
        .groups
        .into_iter()
        .map(|g| g.path)
        .collect()
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

#[tokio::test]
async fn the_reply_waits_for_the_commit() {
    let rig = rig().await;
    let (a, mut a_rx) = rig.core.open_conn();
    let model = rig.core.model();
    send(&rig.core, a, 1, group_add("Work"));
    reply(&mut a_rx).await.expect("group add is ok");
    assert!(groups_on_disk(&rig).contains(&"Work".to_string()));
    assert!(model.borrow().topics[&Topic::Groups].contains_key("Work"));
}

/// A closed connection is one input for the core: its waiters are dropped there.
#[tokio::test]
async fn close_conn_sends_client_gone() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let (c, _rx) = rig.core.open_conn();
    within(rig.core.barrier()).await;
    let inputs = stats.inputs.load(Ordering::SeqCst);
    rig.core.close_conn(c);
    within(rig.core.barrier()).await;
    assert_eq!(stats.inputs.load(Ordering::SeqCst), inputs + 1);
}

#[tokio::test]
async fn a_read_during_a_pending_commit_waits_for_it() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let release = pause(&rig.core);
    let (a, mut a_rx) = rig.core.open_conn();
    let (b, mut b_rx) = rig.core.open_conn();
    let commits = stats.commits.load(Ordering::SeqCst);
    send(
        &rig.core,
        a,
        1,
        Op::TaskAdd(TaskAdd {
            project: "Lantern".into(),
            title: "Solder".into(),
            ..TaskAdd::default()
        }),
    );
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    let inputs = stats.inputs.load(Ordering::SeqCst);
    send(&rig.core, b, 2, task_ls());
    // Give the core every chance to take B: on this current-thread runtime it would run
    // here. The right core is parked on the commit and is not woken.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    // B was not handled, so it cannot have been answered.
    assert_eq!(stats.inputs.load(Ordering::SeqCst), inputs);
    assert!(b_rx.try_recv().is_err());
    release.send(()).unwrap();
    reply(&mut a_rx).await.expect("the add is ok");
    let listed = reply(&mut b_rx).await.expect("the list is ok");
    assert!(listed.text.contains("Solder"), "{}", listed.text);
}

#[tokio::test]
async fn writes_arriving_during_a_commit_share_the_next_one() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let release = pause(&rig.core);
    let (c, mut rx) = rig.core.open_conn();
    let commits = stats.commits.load(Ordering::SeqCst);
    send(&rig.core, c, 0, group_add("A"));
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    for i in 1..=10 {
        send(&rig.core, c, i, group_add(&format!("G{i}")));
    }
    release.send(()).unwrap();
    for _ in 0..11 {
        reply(&mut rx).await.expect("ok");
    }
    assert_eq!(stats.commits.load(Ordering::SeqCst), commits + 2);
}

#[tokio::test]
async fn a_drain_without_writes_answers_at_once() {
    let rig = rig().await;
    let _release = pause(&rig.core);
    let (c, mut rx) = rig.core.open_conn();
    send(&rig.core, c, 1, Op::Brief(Default::default()));
    reply(&mut rx).await.expect("brief answers");
    send(&rig.core, c, 2, task_ls());
    reply(&mut rx).await.expect("task ls answers");
}

#[tokio::test]
async fn stop_waits_for_the_batch_in_flight() {
    let rig = rig().await;
    let stats = rig.core.stats();
    let release = pause(&rig.core);
    let (c, mut rx) = rig.core.open_conn();
    let commits = stats.commits.load(Ordering::SeqCst);
    send(&rig.core, c, 1, group_add("Work"));
    wait_until(|| stats.commits.load(Ordering::SeqCst) == commits + 1).await;
    let core = rig.core.clone();
    let stopper = tokio::spawn(async move { core.stop().await });
    release.send(()).unwrap();
    within(stopper).await.unwrap();
    // The reply was sent before stop() returned.
    rx.try_recv()
        .expect("the reply is already there")
        .1
        .expect("the add is ok");
    assert!(groups_on_disk(&rig).contains(&"Work".to_string()));
}

#[tokio::test]
async fn a_zone_change_moves_the_backup_deadline() {
    let rig = rig().await;
    rig.core.zone_changed(TimeZone::UTC);
    within(rig.core.barrier()).await;
    let want = UtcMs(
        "2026-11-16T00:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    assert_eq!(rig.time.armed(), Some(want));
}

#[tokio::test]
async fn status_names_the_brain_and_the_caller() {
    let rig = rig().await;
    let status = within(rig.core.status(CallerInfo::default()))
        .await
        .expect("the core is running");
    assert_eq!(status.brain, "default");
    assert_eq!(status.caller, "an agent outside moa");
    assert!(!status.sessions.is_empty());
}

/// Startup's daily backup runs on the store thread: `start()` doesn't wait for it, while
/// `barrier()` does.
#[tokio::test]
async fn a_slow_startup_backup_does_not_delay_start() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let backups = brain.dir.path().join("backups");
    let (release, gate) = mpsc::channel();
    let core = within(CoreHandle::start_with_faults(
        CoreOptions {
            brain: BrainName::default_brain(),
            config: brain.config(),
            deps: Deps::default(),
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: time.clone(),
            alarm: Box::new(time.alarm()),
            db: brain.db.clone(),
            backups: backups.clone(),
            events_capacity: 256,
            tmux: None,
            spool: None,
        },
        brain.store(),
        vec![Fault::PauseNextBackup(gate)],
    ))
    .await
    .expect("the core starts with its backup still held");
    assert!(!backups.join("daily-2026-11-15.db").exists());

    release.send(()).unwrap();
    within(core.barrier()).await;
    assert!(backups.join("daily-2026-11-15.db").exists());
}

/// With no terminal link (a real brain before the switch-over, T21), the core task answers
/// every connect itself: an ensure is told v1 isn't switched over instead of waiting forever.
#[tokio::test]
async fn tmux_ensure_says_v1_isnt_switched_over() {
    let rig = rig().await;
    let (c, mut rx) = rig.core.open_conn();
    send(&rig.core, c, 1, Op::TmuxEnsure { tmux: None });
    assert_eq!(
        reply(&mut rx).await,
        Err(OpError(
            "moa v1 isn't switched over yet; try moa --sandbox".into()
        ))
    );
    let status = within(rig.core.status(CallerInfo::default()))
        .await
        .expect("a status");
    assert_eq!(status.tmux, None);
    assert_eq!(status.tmux_restarts, 0);
    assert_eq!(status.panel_starts, 0);
}
