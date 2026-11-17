//! Hooks reach the core through the socket and through the spool, with no `hello`.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{Client, boot, boot_with, request};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::config::BrainName;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Session, SessionId,
};
use moa_core::state::Deps;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_platform::Paths;
use moa_platform::spool;
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ, TypicalBrain};
use serde_json::{Value, json};
use tokio::time::timeout;

const S: &str = "0195f3a2-1111-7000-8000-000000000001";
const C: &str = "a1b2c3d4-0000-4000-8000-000000000001";

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
    let time = FakeTime::new(brain.now().wall);
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
        Duration::from_secs(5),
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
    let core = timeout(
        Duration::from_secs(5),
        CoreHandle::start(options(&brain, &live)),
    )
    .await
    .expect("not stuck")
    .expect("the core starts");
    assert!(!draining.exists());
    core.stop().await;
}
