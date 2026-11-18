//! Hooks reach the core through the socket and through the spool, with no `hello`.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{Client, Daemon, boot, boot_with, request};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_agents::claude::{last_assistant_text, normalize};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::config::BrainName;
use moa_core::hook::HookLine;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Session, SessionId,
};
use moa_core::op::{Op, ReqId};
use moa_core::state::CallerInfo;
use moa_core::state::Deps;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_platform::Paths;
use moa_platform::spool;
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ, TypicalBrain};
use moa_testkit::wait_for;
use serde_json::{Value, json};
use tokio::time::timeout;

const S: &str = "0195f3a2-1111-7000-8000-000000000001";
const C: &str = "a1b2c3d4-0000-4000-8000-000000000001";
/// How long a test waits for something the daemon does on its own.
const WAIT: Duration = Duration::from_secs(10);

/// The fake clock's now is the Typical brain's, in milliseconds; `ago` is before it.
fn ago(ms: i64) -> i64 {
    let now: Timestamp = TYPICAL_NOW.parse().unwrap();
    now.as_millisecond() - ms
}

/// A live, idle session, in the brain's store.
fn seed(store: &mut Store) {
    let id: SessionId = S.parse().unwrap();
    let session = Session {
        id,
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some("/work/Lantern".into()),
        start_cwd: Some("/work/Lantern".into()),
        project_id: None,
        title: None,
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(1)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(UtcMs(1)),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(1)),
    };
    let conversation = Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: C.into(),
        session_id: id,
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    };
    store
        .apply(&[Change::Session(session), Change::Conversation(conversation)])
        .expect("the session is saved");
}

fn seed_db(paths: &Paths) {
    let mut store = Store::open(&paths.db("default"), &paths.backups("default")).unwrap();
    seed(&mut store);
}

/// A raw hook line for the seeded session, at `at_ms`.
fn hook_line(event: &str, at_ms: i64, seq: u32) -> Value {
    let mut payload = json!({"session_id": C, "hook_event_name": event});
    if event == "UserPromptSubmit" {
        payload["prompt"] = json!("light the Lantern");
    }
    json!({
        "v": 1,
        "agent": "claude",
        "at_ns": at_ms * 1_000_000,
        "pid": 100,
        "seq": seq,
        "env": {"moa_session": S, "moa_brain": "default", "claude_session_id": C},
        "payload": payload,
    })
}

fn notice(line: &Value) -> String {
    json!({"jsonrpc": "2.0", "method": "hook", "params": line}).to_string()
}

fn with_id(id: i64, line: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "hook", "params": line}).to_string()
}

fn put_in_spool(path: &Path, lines: &[Value]) {
    for l in lines {
        spool::append(path, l.to_string().as_bytes()).unwrap();
    }
}

/// How `session.ls` shows the seeded session.
fn display(client: &mut Client) -> String {
    let reply = client.call("session.ls", json!({"all": true}));
    reply["result"]["data"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["id"] == S)
        .expect("the seeded session is listed")["display"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `at` of the seeded session's events, in the order they were written.
fn event_times(paths: &Paths) -> Vec<i64> {
    let store = Store::open_read_only(&paths.db("default")).unwrap();
    let mut stmt = store
        .conn()
        .prepare("SELECT at FROM events WHERE session_id = ?1 ORDER BY id")
        .unwrap();
    stmt.query_map([S], |r| r.get::<_, i64>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn spool_is_empty(paths: &Paths) -> bool {
    let live = paths.spool("default");
    let empty = |p: &Path| fs::read_to_string(p).map(|t| t.is_empty()).unwrap_or(true);
    empty(&live) && !spool::draining(&live).exists()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_line_updates_the_session() {
    let d = boot_with(seed_db, |_| {}).await;
    let mut c = Client::connect(&d.sock);
    c.write(&notice(&hook_line("UserPromptSubmit", ago(1000), 1)));
    // The hook needs no hello. `session.ls` does, and it is read after the hook.
    c.hello();
    assert_eq!(display(&mut c), "working");
}

/// A hook writes its line and exits at once, often before the daemon has accepted the
/// connection: none of those lines is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lines_from_hooks_that_already_left_all_arrive() {
    const N: usize = 20;
    let d = boot_with(seed_db, |_| {}).await;
    for i in 0..N {
        let at = ago(60_000 - 100 * i as i64);
        let mut s = std::os::unix::net::UnixStream::connect(&d.sock).unwrap();
        writeln!(
            s,
            "{}",
            notice(&hook_line("UserPromptSubmit", at, i as u32))
        )
        .unwrap();
    }
    let arrived = timeout(Duration::from_secs(10), async {
        while event_times(&d.paths).len() < N {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(arrived.is_ok(), "{} of {N}", event_times(&d.paths).len());
}

/// A daemon on the seeded brain, and its core.
async fn boot_with_core() -> (Daemon, CoreHandle) {
    let root = common::short_root();
    let paths = Paths::with_root(root.path());
    let brain = TypicalBrain::new();
    common::place_db(&paths, &brain);
    seed_db(&paths);
    let time = FakeTime::new(brain.now().wall);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = common::options(&paths, &brain, &time, moa_daemon::Ready::Core(tx));
    let done = tokio::spawn(moa_daemon::run(opts));
    let (sock, core) = timeout(WAIT, rx)
        .await
        .expect("ready")
        .expect("run didn't end");
    let d = Daemon {
        root,
        paths,
        sock,
        done,
        brain,
        time,
    };
    (d, core)
}

/// I2: a hook that gave up on the daemon while it was up (a full accept backlog) left its
/// line in the spool. The next live hook sees it there and drains it; H5 orders the two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_hook_drains_lines_spooled_while_the_daemon_was_up() {
    let (d, core) = boot_with_core().await;
    // The drain at start, and the one once the daemon accepts, have both run.
    let quiet = wait_for(WAIT, || {
        (core.stats().spool_drains.load(Ordering::SeqCst) >= 2).then_some(())
    });
    assert!(quiet.is_some(), "the drains at start never ran");
    let (t1, t2) = (ago(2000), ago(1000));
    put_in_spool(
        &d.paths.spool("default"),
        &[hook_line("UserPromptSubmit", t1, 1)],
    );
    let mut c = Client::connect(&d.sock);
    c.write(&notice(&hook_line("Stop", t2, 2)));
    let both = wait_for(WAIT, || {
        (event_times(&d.paths).len() == 2 && spool_is_empty(&d.paths)).then_some(())
    });
    assert!(both.is_some(), "{:?}", event_times(&d.paths));
    // The spooled prompt is older than the live Stop: history only.
    c.hello();
    assert_eq!(display(&mut c), "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_is_answered_with_a_null_context() {
    let d = boot_with(seed_db, |_| {}).await;
    let mut c = Client::connect(&d.sock);
    c.write(&with_id(7, &hook_line("SessionStart", ago(1000), 1)));
    let reply = c.read().expect("a reply");
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"], json!({"context": null}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_event_the_core_drops_is_still_answered_when_it_has_an_id() {
    // The brain has never heard of this session.
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    c.write(&with_id(3, &hook_line("SessionStart", ago(1000), 1)));
    let reply = c.read().expect("a reply");
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"], json!({"context": null}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_line_the_agent_cannot_read_is_answered_with_a_null_context() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    let mut line = hook_line("SessionStart", ago(1000), 1);
    line["v"] = json!(99);
    c.write(&with_id(9, &line));
    let reply = c.read().expect("a reply");
    assert_eq!(reply["id"], 9);
    assert_eq!(reply["result"], json!({"context": null}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_hook_line_is_refused_only_when_it_has_an_id() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    c.write(&with_id(5, &json!({"x": 1})));
    let reply = c.read().expect("a reply");
    assert_eq!(reply["id"], 5);
    assert_eq!(reply["error"]["code"], -32602);
    // Without an id nothing is said: the next thing read is the reply to what follows.
    c.write(&notice(&json!({"x": 1})));
    let id = c.id();
    c.write(&request(
        id,
        "hello",
        json!({"client": "t", "build": "x", "caller": {}}),
    ));
    assert_eq!(c.read().expect("a reply")["id"], id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spooled_events_are_drained_in_order_at_start() {
    let (t1, t2, t3) = (ago(30_000), ago(20_000), ago(10_000));
    let d = boot_with(
        |paths| {
            seed_db(paths);
            // Out of order: the newest first.
            put_in_spool(
                &paths.spool("default"),
                &[
                    hook_line("Stop", t3, 3),
                    hook_line("UserPromptSubmit", t1, 1),
                    hook_line("UserPromptSubmit", t2, 2),
                ],
            );
        },
        |_| {},
    )
    .await;
    let mut c = Client::connect(&d.sock);
    c.hello();
    assert_eq!(display(&mut c), "done");
    assert_eq!(event_times(&d.paths), vec![t1, t2, t3]);
    assert!(spool_is_empty(&d.paths));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leftover_draining_file_is_drained_first() {
    let (t1, t2) = (ago(10_000), ago(5_000));
    let d = boot_with(
        |paths| {
            seed_db(paths);
            // A drain that crashed: its line is in `.draining`, a newer one in the spool.
            let live = paths.spool("default");
            put_in_spool(&live, &[hook_line("Stop", t1, 1)]);
            fs::rename(&live, spool::draining(&live)).unwrap();
            put_in_spool(&live, &[hook_line("UserPromptSubmit", t2, 2)]);
        },
        |_| {},
    )
    .await;
    let mut c = Client::connect(&d.sock);
    c.hello();
    assert_eq!(display(&mut c), "working");
    assert_eq!(event_times(&d.paths), vec![t1, t2]);
    assert!(spool_is_empty(&d.paths));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_spool_lines_are_skipped_and_the_rest_drained() {
    let d = boot_with(
        |paths| {
            seed_db(paths);
            let live = paths.spool("default");
            spool::append(&live, b"not json at all").unwrap();
            put_in_spool(&live, &[hook_line("Stop", ago(10_000), 1)]);
        },
        |_| {},
    )
    .await;
    assert_eq!(event_times(&d.paths).len(), 1);
    assert!(spool_is_empty(&d.paths));
}

fn options(brain: &TypicalBrain, spool: &Path) -> CoreOptions {
    options_at(brain, spool, &FakeTime::new(brain.now().wall))
}

fn options_at(brain: &TypicalBrain, spool: &Path, time: &Arc<FakeTime>) -> CoreOptions {
    CoreOptions {
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
        spool: Some(spool.to_path_buf()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_whose_commit_fails_keeps_the_draining_file() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    put_in_spool(&live, &[hook_line("Stop", ago(10_000), 1)]);
    // Startup writes nothing here, so the fault meets the drain's commit.
    let core = timeout(
        WAIT,
        CoreHandle::start_with_faults(
            options(&brain, &live),
            brain.store(),
            vec![Fault::FailNextCommit],
        ),
    )
    .await
    .expect("not stuck")
    .expect("the core starts");
    let draining = spool::draining(&live);
    let kept = fs::read_to_string(&draining).expect("the lines wait for the next start");
    assert!(kept.contains("\"Stop\""), "{kept}");
    core.stop().await;

    // The next start drains them.
    let core = timeout(WAIT, CoreHandle::start(options(&brain, &live)))
        .await
        .expect("not stuck")
        .expect("the core starts");
    assert!(!draining.exists());
    core.stop().await;
}

fn core_hook(core: &CoreHandle, line: &Value) {
    let line: HookLine = serde_json::from_value(line.clone()).unwrap();
    let envelope = normalize(&line, &last_assistant_text).expect("a known event");
    core.hook(None, envelope, Some(line));
}

#[tokio::test]
async fn a_hook_left_queued_at_stop_is_spooled() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let core = timeout(WAIT, CoreHandle::start(options(&brain, &live)))
        .await
        .expect("not stuck")
        .expect("the core starts");
    // The first hook's commit is held; `stop` joins the inbox behind it, and the hooks
    // after that are still waiting when the core stops.
    let (release, held) = std::sync::mpsc::channel();
    core.store_fault(Fault::PauseNextCommit(held));
    let (t1, t2, t3, t4) = (ago(30_000), ago(20_000), ago(10_000), ago(5_000));
    core_hook(&core, &hook_line("UserPromptSubmit", t1, 1));
    let stats = core.stats();
    timeout(WAIT, async {
        while stats.commits.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first drain commits");
    let mut stopping = Box::pin(core.stop());
    // One poll sends the stop to the inbox.
    assert!(timeout(Duration::ZERO, &mut stopping).await.is_err());
    core_hook(&core, &hook_line("Stop", t2, 2));
    core_hook(&core, &hook_line("UserPromptSubmit", t3, 3));
    release.send(()).unwrap();
    timeout(WAIT, stopping).await.expect("the core stops");
    core.ended().await;
    // The core is gone: a hook now is spooled at once, the failed-enqueue path.
    core_hook(&core, &hook_line("Stop", t4, 4));
    let spooled = fs::read_to_string(&live).unwrap();
    assert_eq!(spooled.lines().count(), 3, "{spooled}");
    drop(core);

    // The next start applies them after the one that was committed.
    let core = timeout(WAIT, CoreHandle::start(options(&brain, &live)))
        .await
        .expect("not stuck")
        .expect("the core starts");
    let store = brain.store();
    let mut stmt = store
        .conn()
        .prepare("SELECT at FROM events WHERE session_id = ?1 ORDER BY id")
        .unwrap();
    let times: Vec<i64> = stmt
        .query_map([S], |r| r.get::<_, i64>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(times, vec![t1, t2, t3, t4]);
    assert!(!spool::draining(&live).exists());
    core.stop().await;
}

/// The seeded session's `activity`, as saved.
fn saved_activity(brain: &TypicalBrain) -> String {
    brain
        .store()
        .conn()
        .query_row("SELECT activity FROM sessions WHERE id = ?1", [S], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
}

/// A hook stepped in a drain whose commit fails isn't lost with it: it goes first into the
/// next drain, and is applied there, since the reload forgot the keys the failed drain had
/// remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_in_a_drain_whose_commit_fails_is_applied_after_the_reload() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let core = started(&brain, &live).await;
    let stats = core.stats();
    core.store_fault(Fault::FailNextCommit);
    core_hook(&core, &hook_line("UserPromptSubmit", ago(1000), 1));
    let applied = wait_for(WAIT, || (seen_events(&brain) == 1).then_some(()));
    assert!(
        applied.is_some(),
        "the hook was lost with the failed commit"
    );
    assert_eq!(stats.commit_failures.load(Ordering::SeqCst), 1);
    assert_eq!(stats.reloads.load(Ordering::SeqCst), 1);
    assert_eq!(saved_activity(&brain), "working");
    // Applied, not spooled.
    assert!(fs::read_to_string(&live).unwrap_or_default().is_empty());
    core.stop().await;
}

/// The same, when the failure stops the core (its store can't be reopened): the event is
/// spooled for the next start.
#[tokio::test]
async fn a_hook_in_a_drain_that_stops_the_core_is_spooled() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let core = started(&brain, &live).await;
    core.store_fault(Fault::Crash);
    // The reopen finds something that is not a brain.
    fs::write(&brain.db, b"not a database").unwrap();
    for ext in ["-wal", "-shm"] {
        let mut side = brain.db.clone().into_os_string();
        side.push(ext);
        let _ = fs::remove_file(side);
    }
    let line = hook_line("UserPromptSubmit", ago(1000), 1);
    core_hook(&core, &line);
    let ended = timeout(WAIT, core.finished())
        .await
        .expect("the core stops");
    assert!(ended.is_err(), "{ended:?}");
    timeout(WAIT, core.ended()).await.expect("the core ends");
    let kept = fs::read_to_string(&live).unwrap();
    let kept: HookLine = serde_json::from_str(kept.trim_end()).unwrap();
    assert_eq!(kept, serde_json::from_value::<HookLine>(line).unwrap());
}

#[tokio::test]
async fn a_hook_that_panics_the_core_is_still_answered() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let core = timeout(WAIT, CoreHandle::start(options(&brain, &live)))
        .await
        .expect("not stuck")
        .expect("the core starts");
    let (conn, mut replies) = core.open_conn();
    core.panic_next_handle();
    let line: HookLine = serde_json::from_value(hook_line("SessionStart", ago(1000), 1)).unwrap();
    let envelope = normalize(&line, &last_assistant_text).expect("a known event");
    core.hook(Some((conn, ReqId(1))), envelope, Some(line));
    // Unanswered, the session would wait on it, reading nothing, until the daemon stops.
    let (req, result) = timeout(WAIT, replies.recv())
        .await
        .expect("answered")
        .expect("a reply");
    assert_eq!(req, ReqId(1));
    assert!(result.is_err());
    core.stop().await;
}

async fn started(brain: &TypicalBrain, live: &Path) -> CoreHandle {
    timeout(WAIT, CoreHandle::start(options(brain, live)))
        .await
        .expect("not stuck")
        .expect("the core starts")
}

fn seen_events(brain: &TypicalBrain) -> i64 {
    let store = brain.store();
    store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE session_id = ?1",
            [S],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
}

/// The core refuses writes after three failed commits in a row: `Startup`'s flagging commit
/// is the first (the grammar rejects every suggestion), two group adds the rest.
#[tokio::test]
async fn a_hook_under_refused_writes_is_spooled_and_applied_after_a_restart() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let store = brain.store();
    store.conn().execute_batch("PRAGMA query_only = 1").unwrap();
    let mut opts = options(&brain, &live);
    opts.deps = Deps {
        parse_action: Some(|_, _| Err("no".into())),
        ..Deps::default()
    };
    let core = timeout(WAIT, CoreHandle::start_with_store(opts, store))
        .await
        .expect("not stuck")
        .expect("the core starts");
    let (conn, mut replies) = core.open_conn();
    for req in 1..=2 {
        let op = Op::GroupAdd {
            path: format!("G{req}"),
        };
        core.send(conn, ReqId(req), CallerInfo::default(), op);
        let (_, result) = timeout(WAIT, replies.recv())
            .await
            .expect("answered")
            .expect("a reply");
        assert!(result.is_err());
    }
    let commits = core.stats().commits.load(Ordering::SeqCst);
    let line = hook_line("UserPromptSubmit", ago(1000), 1);
    let parsed: HookLine = serde_json::from_value(line.clone()).unwrap();
    let envelope = normalize(&parsed, &last_assistant_text).unwrap();
    core.hook(Some((conn, ReqId(3))), envelope, Some(parsed));
    let (req, result) = timeout(WAIT, replies.recv())
        .await
        .expect("answered")
        .expect("a reply");
    assert_eq!(req, ReqId(3));
    assert_eq!(result.unwrap().data, json!({"context": null}));
    // Not applied, not committed: kept.
    assert_eq!(core.stats().commits.load(Ordering::SeqCst), commits);
    let kept = fs::read_to_string(&live).unwrap();
    let kept: HookLine = serde_json::from_str(kept.trim_end()).unwrap();
    assert_eq!(kept, serde_json::from_value::<HookLine>(line).unwrap());
    core.stop().await;
    drop(core);

    let core = started(&brain, &live).await;
    assert_eq!(seen_events(&brain), 1);
    assert!(!spool::draining(&live).exists());
    core.stop().await;
}

/// I2: hooks spooled under a refusal are drained when it ends, not at the next start. The
/// end of the refusal is a timer, so the fake clock moving past it is what drains them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_spooled_under_a_refusal_are_drained_when_it_ends() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let time = FakeTime::new(brain.now().wall);
    let core = timeout(WAIT, CoreHandle::start(options_at(&brain, &live, &time)))
        .await
        .expect("not stuck")
        .expect("the core starts");
    // Three failed commits in a row: writes are refused for a second.
    let (conn, mut replies) = core.open_conn();
    for req in 1..=3 {
        core.store_fault(Fault::FailNextCommit);
        let op = Op::GroupAdd {
            path: format!("G{req}"),
        };
        core.send(conn, ReqId(req), CallerInfo::default(), op);
        let (_, result) = timeout(WAIT, replies.recv())
            .await
            .expect("answered")
            .expect("a reply");
        assert!(result.is_err());
    }
    let spooled = core.stats().refused_hooks.load(Ordering::SeqCst);
    core_hook(&core, &hook_line("UserPromptSubmit", ago(1000), 1));
    let held = wait_for(WAIT, || {
        (core.stats().refused_hooks.load(Ordering::SeqCst) > spooled).then_some(())
    });
    assert!(
        held.is_some(),
        "the hook was not kept from the refused store"
    );
    assert_eq!(fs::read_to_string(&live).unwrap().lines().count(), 1);
    assert_eq!(seen_events(&brain), 0);

    time.advance(Duration::from_secs(1));
    let drained = wait_for(WAIT, || {
        (seen_events(&brain) == 1 && spool_file_is_empty(&live)).then_some(())
    });
    assert!(drained.is_some(), "the spooled hook waits for a restart");
    assert_eq!(saved_activity(&brain), "working");
    core.stop().await;
}

fn spool_file_is_empty(live: &Path) -> bool {
    fs::read_to_string(live).unwrap_or_default().is_empty() && !spool::draining(live).exists()
}

#[tokio::test]
async fn a_spool_that_panics_the_core_is_set_aside() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let dir = brain.dir.path().join("spool");
    let live = dir.join("default.jsonl");
    put_in_spool(
        &live,
        &[
            hook_line("UserPromptSubmit", ago(30_000), 1),
            hook_line("Stop", ago(20_000), 2),
        ],
    );
    // The first line makes the core panic; it reloads and goes on with the second.
    let core = timeout(
        WAIT,
        CoreHandle::start_with_drain_panics(options(&brain, &live), brain.store(), 1),
    )
    .await
    .expect("not stuck")
    .expect("the core starts");
    assert_eq!(core.stats().panics.load(Ordering::SeqCst), 1);
    assert!(!spool::draining(&live).exists());
    let bad: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("default.jsonl.bad-"))
        .collect();
    assert_eq!(bad.len(), 1, "{bad:?}");
    let kept = fs::read_to_string(dir.join(&bad[0])).unwrap();
    assert_eq!(kept.lines().count(), 2);
    core.stop().await;
    drop(core);

    // The next start has nothing to replay.
    let events = seen_events(&brain);
    let core = started(&brain, &live).await;
    assert_eq!(core.stats().panics.load(Ordering::SeqCst), 0);
    assert_eq!(seen_events(&brain), events);
    core.stop().await;
}

/// M6: a drain at start that brings the core down (a line panics it, then so does the
/// reload) fails `start` with the core's line, instead of a daemon that says `ready`.
#[tokio::test]
async fn a_drain_at_start_that_stops_the_core_fails_start() {
    let brain = TypicalBrain::new();
    seed(&mut brain.store());
    let live = brain.dir.path().join("spool").join("default.jsonl");
    put_in_spool(&live, &[hook_line("Stop", ago(10_000), 1)]);
    let started = timeout(
        WAIT,
        CoreHandle::start_with_drain_panics(options(&brain, &live), brain.store(), 2),
    )
    .await
    .expect("not stuck");
    match started {
        Err(moa_daemon::DaemonError::Fatal(line)) => {
            assert_eq!(line, "moa hit a bug loading the brain; see the log")
        }
        Err(e) => panic!("another error: {e}"),
        Ok(_) => panic!("start said the core was up"),
    }
    // The line is kept for the next start.
    assert!(spool::draining(&live).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_writes_the_plugin_once() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let d = boot().await;
    let dir = d.paths.session_plugin();
    let files = [
        dir.join(".claude-plugin/plugin.json"),
        dir.join("hooks/hooks.json"),
    ];
    for f in &files {
        assert_eq!(mode(f), 0o600, "{f:?}");
        assert_eq!(mode(f.parent().unwrap()), 0o700, "{f:?}");
    }
    assert_eq!(mode(&dir), 0o700);
    // `write_if_changed` writes a new file and renames it into place: the same inode is the
    // same file, never rewritten.
    let inode =
        |f: &std::path::PathBuf| std::os::unix::fs::MetadataExt::ino(&fs::metadata(f).unwrap());
    let before: Vec<_> = files.iter().map(inode).collect();
    // Stop it, and start another on the same root with the same exe: the files are left alone.
    let mut c = Client::connect(&d.sock);
    c.hello();
    assert_eq!(c.call("shutdown", json!({}))["result"], json!({}));
    timeout(Duration::from_secs(10), d.done)
        .await
        .expect("run ends")
        .expect("no panic")
        .expect("a clean stop");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = common::options(&d.paths, &d.brain, &d.time, moa_daemon::Ready::Channel(tx));
    let again = tokio::spawn(moa_daemon::run(opts));
    timeout(Duration::from_secs(10), rx)
        .await
        .expect("ready")
        .expect("run didn't end");
    let after: Vec<_> = files.iter().map(inode).collect();
    assert_eq!(before, after);
    again.abort();
}

/// M9: the plugin's folders are checked like the daemon's other private folders: its
/// `hooks.json` names a command Claude runs, so a folder of ours that others can reach is
/// closed to them before it is used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_folder_others_can_reach_is_closed_to_them() {
    use std::os::unix::fs::PermissionsExt;
    let d = boot_with(
        |paths| {
            let hooks = paths.session_plugin().join("hooks");
            fs::create_dir_all(&hooks).unwrap();
            fs::set_permissions(paths.session_plugin(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&hooks, fs::Permissions::from_mode(0o755)).unwrap();
        },
        |_| {},
    )
    .await;
    let hooks = d.paths.session_plugin().join("hooks");
    let mode = fs::metadata(&hooks).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}
