//! Slice 9: the daemon upgrades itself in place, between moa's turns, keeping its socket and
//! its lock (L4, L5). Each daemon runs inside its test's process, so its upgrade hands both
//! to the test (`Exec::Test`), which starts the next image with them ([`Rig::restart`]).
//!
//! The daemon's program is a copy of `stub-acp`, which answers `build-id` as moa does
//! ([`Rig::with_exe`]): a new build is a copy with another time renamed over it.

mod common;

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::atomic::Ordering;

use common::Client;
use common::hooks::{ago, hook_notice};
use common::rig::{Rig, Up, WAIT, answer, end, gate, prompt, say, session_stop};
use moa_core::config::BrainName;
use moa_core::state::CallerInfo;
use moa_core::upgrade::{BUILD_KEY, NOTICE_NOTHING};
use moa_platform::pid_alive;
use moa_testkit::{StubStep, wait_for};
use serde_json::{Value, json};

/// What `status` says while an upgrade waits for moa's turn to end.
const WAITING: &str = "waiting for moa's turn";

/// The socket file's inode: the same file, never made again.
fn inode(sock: &Path) -> u64 {
    std::fs::metadata(sock).expect("the socket file").ino()
}

fn hello() -> Value {
    json!({"client": "test", "build": moa_platform::build_id(), "caller": {}})
}

/// Asks `up` to upgrade; its reply's result.
fn upgrade(up: &Up, params: Value) -> Value {
    let reply = up.client().call("upgrade", params);
    reply["result"].clone()
}

fn status_upgrade(up: &Up) -> Value {
    up.client().call("status", json!({}))["result"]["upgrade"].clone()
}

/// Waits until `status` says the upgrade waits for moa's turn.
fn wait_waiting(up: &Up) {
    wait_for(WAIT, || {
        (status_upgrade(up) == json!(WAITING)).then_some(())
    })
    .unwrap_or_else(|| panic!("the upgrade never waited: {}", status_upgrade(up)));
}

/// How many inputs the core has stepped.
fn inputs(up: &Up) -> u64 {
    up.core.stats().inputs.load(Ordering::SeqCst)
}

/// How many new builds of its program the daemon found (`found`) or skipped as
/// half-written. Read from its counters, not its log: the log is the process's first
/// daemon's, and a test may share its process with others.
fn builds(up: &Up, found: bool) -> u64 {
    let stats = up.core.stats();
    let n = match found {
        true => &stats.builds_found,
        false => &stats.builds_skipped,
    };
    n.load(Ordering::SeqCst)
}

/// `moa upgrade`: the daemon hands its listening socket to its next image. A connection from
/// before the handoff ends; one made during it waits in the socket's backlog and is answered
/// by the next image. The socket file is the same one throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moa_upgrade_hands_over_the_socket() {
    let rig = Rig::new(&[]).with_exe();
    let up = rig.start().await;
    let sock = up.sock.clone();
    let ino = inode(&sock);
    let mut before = Client::connect(&sock);
    let greeted = before.call("hello", hello());
    let exe = rig.exe().display().to_string();
    assert_eq!(greeted["result"]["exe"], json!(exe), "{greeted}");

    assert_eq!(
        upgrade(&up, json!({})),
        json!({"upgrading": true, "waiting_for_moa": false})
    );
    let build = rig.build();
    let inherited = up.handoff().await;
    assert_eq!(inherited.from_build, build);
    assert_eq!(
        before.read(),
        None,
        "a connection from before the handoff ends"
    );

    let mut during = Client::connect(&sock);
    let id = during.send("hello", hello());
    let up = rig.restart(inherited).await;
    assert_eq!(up.sock, sock);
    let greeted = during.reply(id);
    assert_eq!(greeted["result"]["matched"], json!(true), "{greeted}");
    assert_eq!(greeted["result"]["pid"], json!(std::process::id()));
    assert_eq!(inode(&sock), ino, "the socket file was never made again");
    up.stop().await;
}

/// While moa is in a turn the upgrade waits, and `status` says so; the turn's end lets it go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_upgrade_waits_for_moas_turn() {
    let rig = Rig::new(&[say("done", "m1"), gate("g1"), end()]).with_exe();
    let up = rig.start().await;
    assert_eq!(status_upgrade(&up), Value::Null);
    let mut you = up.client();
    let turn = you.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_saved("moa", "done");

    assert_eq!(
        upgrade(&up, json!({})),
        json!({"upgrading": true, "waiting_for_moa": true})
    );
    assert_eq!(status_upgrade(&up), json!(WAITING));
    assert!(!up.handed_over());

    rig.open("g1");
    assert_eq!(answer(&you.reply(turn)), "done");
    let inherited = up.handoff().await;
    rig.restart(inherited).await.stop().await;
}

/// A parked `ask` and a parked `reply` both hear A13's "no reply yet" before the handoff:
/// nobody waiting on an ask is cut off, and `moactl reply` waits again on the next image.
/// The ask is to moa, mid-turn: once the upgrade waits, moa's channel holds it, so the turn's
/// end lets the upgrade go rather than the ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parked_asks_hear_no_reply_yet_first() {
    let rig = Rig::new(&[say("done", "m1"), gate("g1"), end()]).with_exe();
    let ids = rig.seed_sessions(2);
    let up = rig.start().await;
    let mut you = up.client();
    let turn = you.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_saved("moa", "done");
    let mut kite = up.session_client(ids[0]);
    let ask = kite.send(
        "ask",
        json!({"to": "moa", "text": "Is the domain renewed?"}),
    );
    // A request sent isn't yet one the core has: a barrier fences only what has reached it.
    // The ask has once its row is saved.
    rig.wait_ask(1, "pending");
    up.core.barrier().await;
    let mut other = up.session_client(ids[1]);
    // A `reply` saves nothing: it has reached the core once the core has stepped one more
    // input (nothing else moves here, with time stopped and moa's turn held at its gate).
    let before = inputs(&up);
    let again = other.send("reply", json!({"id": 1}));
    wait_for(WAIT, || (inputs(&up) > before).then_some(())).expect("the reply reaches the core");
    up.core.barrier().await;

    assert_eq!(
        upgrade(&up, json!({})),
        json!({"upgrading": true, "waiting_for_moa": true})
    );
    rig.open("g1");
    assert_eq!(answer(&you.reply(turn)), "done");
    let want = "no reply yet from moa; `moactl reply 1` waits again";
    assert_eq!(answer(&kite.reply(ask)), want);
    assert_eq!(answer(&other.reply(again)), want);
    let inherited = up.handoff().await;
    assert_eq!(
        rig.logged("session/prompt").len(),
        1,
        "the ask waits for the next image"
    );
    rig.restart(inherited).await.stop().await;
}

/// A new build renamed over the daemon's program: the daemon upgrades into it once the
/// change has settled and the new program has said its build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_upgrades() {
    let rig = Rig::new(&[]).with_exe();
    let up = rig.start().await;
    let old = rig.build();
    rig.rebuild(1);
    let inherited = up.handoff().await;
    assert_eq!(inherited.from_build, old);
    assert_ne!(rig.build(), old);
    let up = rig.restart(inherited).await;
    assert_eq!(status_upgrade(&up), Value::Null);
    up.stop().await;
}

/// Review Focus 1: a program caught half-written can't say its build, so nothing winds down;
/// the whole copy after it upgrades.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_half_written_binary_is_skipped() {
    let rig = Rig::new(&[]).with_exe();
    let up = rig.start().await;
    rig.half_rebuild();
    wait_for(WAIT, || (builds(&up, false) == 1).then_some(()))
        .expect("the half-written program is skipped");
    assert_eq!(builds(&up, true), 0);
    assert!(!up.handed_over());
    assert_eq!(status_upgrade(&up), Value::Null);

    rig.rebuild(2);
    let inherited = up.handoff().await;
    rig.restart(inherited).await.stop().await;
}

/// Review Focus 2: two rebuilds while moa thinks make one upgrade, after the turn, into the
/// latest build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuilds_while_moa_thinks_exec_once() {
    let rig = Rig::new(&[say("done", "m1"), gate("g1"), end()]).with_exe();
    let up = rig.start().await;
    let old = rig.build();
    let mut you = up.client();
    let turn = you.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_saved("moa", "done");

    rig.rebuild(1);
    wait_waiting(&up);
    rig.rebuild(2);
    wait_for(WAIT, || (builds(&up, true) == 2).then_some(())).expect("the second build is found");
    assert!(!up.handed_over());

    rig.open("g1");
    assert_eq!(answer(&you.reply(turn)), "done");
    // Once: `handoff` checks that nothing more was handed over.
    let inherited = up.handoff().await;
    assert_eq!(inherited.from_build, old);
    let latest = rig.build();
    let up = rig.restart(inherited).await;
    let status = up.client().call("status", json!({}));
    assert_eq!(status["result"]["upgrade"], Value::Null, "{status}");
    assert_ne!(latest, old);
    up.stop().await;
}

/// Review Focus 4: a hook sent between the old image's last accept and the new one's start
/// waits in the socket's backlog, and the new image records it, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hook_during_the_handoff_is_kept() {
    let rig = Rig::new(&[]).with_exe();
    rig.seed_sessions(1);
    let up = rig.start().await;
    let sock = up.sock.clone();
    upgrade(&up, json!({}));
    let inherited = up.handoff().await;

    let line = session_stop(1, ago(0), "the Beacon tests pass");
    Client::connect(&sock).write(&hook_notice(&line));
    let up = rig.restart(inherited).await;
    up.wait_queued(1);
    up.core.barrier().await;
    assert_eq!(up.moa_state()["queued"], json!(1));
    up.stop().await;
}

/// `upgrade {if_stale: true}` to a daemon whose program is still its own build: no upgrade,
/// and the reply names the program, for the client to say which install runs this brain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_if_stale_on_a_current_daemon_says_no() {
    let rig = Rig::new(&[]).with_exe();
    let up = rig.start().await;
    let exe = rig.exe().display().to_string();
    assert_eq!(
        upgrade(&up, json!({"if_stale": true})),
        json!({"upgrading": false, "waiting_for_moa": false, "exe": exe})
    );
    up.core.barrier().await;
    assert!(!up.handed_over());
    assert_eq!(status_upgrade(&up), Value::Null);
    up.stop().await;
}

/// Spec §6.15.2, "the thread resumes": the handoff stops moa's agent, and the next prompt
/// resumes today's thread on a new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moas_thread_resumes_after_the_handoff() {
    let rig = Rig::new(&[say("first", "m1"), end(), say("second", "m2"), end()]).with_exe();
    let up = rig.start().await;
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "hello")), "first");
    let thread = rig.threads();
    assert_eq!(thread.len(), 1);
    let pid = rig.logged("initialize")[0]["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .expect("the stub's pid");

    upgrade(&up, json!({}));
    let inherited = up.handoff().await;
    wait_for(WAIT, || (!pid_alive(pid)).then_some(()))
        .unwrap_or_else(|| panic!("moa's agent {pid} still runs"));

    let up = rig.restart(inherited).await;
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "again")), "second");
    let resumed = rig.logged("session/resume");
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0]["params"]["sessionId"], thread[0].as_str());
    assert_eq!(rig.threads(), thread);
    up.stop().await;
}

/// The rows of activity whose text starts `moa upgraded`.
fn activity_notices(rig: &Rig) -> Vec<String> {
    let store = rig.store();
    let mut stmt = store
        .conn()
        .prepare("SELECT text FROM activity WHERE text LIKE 'moa upgraded%' ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Waits until `meta.daemon.build` is `build`.
fn wait_build_saved(rig: &Rig, build: &str) {
    wait_for(WAIT, || {
        (rig.meta(BUILD_KEY).as_deref() == Some(build)).then_some(())
    })
    .unwrap_or_else(|| panic!("{build} was never saved: {:?}", rig.meta(BUILD_KEY)));
}

/// L7: the first start saves its build and says nothing. An upgrade in place into a new
/// build says so once: a `system` line in moa's thread and an activity row, in the commit
/// that saves the new build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upgrade_in_place_says_nothing_to_do() {
    let rig = Rig::new(&[say("first", "m1"), end()]).with_exe();
    let up = rig.start().await;
    let old = rig.build();
    wait_build_saved(&rig, &old);
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "hello")), "first");
    assert!(
        activity_notices(&rig).is_empty(),
        "a first start isn't an upgrade"
    );

    rig.rebuild(1);
    let inherited = up.handoff().await;
    let new = rig.build();
    assert_ne!(new, old);
    let up = rig.restart(inherited).await;
    wait_build_saved(&rig, &new);
    let notices: Vec<(String, String)> = rig
        .feed()
        .into_iter()
        .filter(|(_, text)| text.starts_with("moa upgraded"))
        .collect();
    assert_eq!(
        notices,
        [("system".to_string(), NOTICE_NOTHING.to_string())],
        "{:?}",
        rig.feed()
    );
    assert_eq!(activity_notices(&rig), [NOTICE_NOTHING]);
    up.stop().await;
}

/// L8, Review Focus 4: a call that reaches the old image during its wind-down, after its
/// core has stopped, is answered by the new image rather than refused. moa's agent ignores
/// `SIGTERM`, so stopping it holds the wind-down open until its `SIGKILL` (2 s later): the
/// `status` is sent once the agent has had its `SIGTERM`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_during_the_wind_down_is_answered_by_the_next_image() {
    let rig = Rig::new(&[StubStep::IgnoreTerm, say("done", "m1"), end()]).with_exe();
    let up = rig.start().await;
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "hello")), "done");

    let mut client = moa_client::Client::connect(moa_client::ClientOptions {
        paths: rig.paths.clone(),
        brain: BrainName::default_brain(),
        client: "upgrade-test",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: moa_client::OnMismatch::Accept,
        build: moa_platform::build_id().into(),
        upgrade_wait: moa_client::UPGRADE_WAIT,
    })
    .await
    .expect("a client");
    assert_eq!(
        upgrade(&up, json!({})),
        json!({"upgrading": true, "waiting_for_moa": false})
    );
    // The core has ended its loop and is stopping moa's agent: the window.
    let termed = rig.agent.join(".stub-acp.termed");
    wait_for(WAIT, || termed.exists().then_some(())).expect("moa's agent is being stopped");
    let status = tokio::spawn(async move { client.status().await });

    let inherited = up.handoff().await;
    let up = rig.restart(inherited).await;
    let status = tokio::time::timeout(WAIT, status)
        .await
        .expect("the status is answered within 10 s")
        .expect("no panic");
    match status {
        Ok(s) => assert_eq!(s.pid, std::process::id()),
        Err(e) => panic!("refused during the wind-down: {e}"),
    }
    up.stop().await;
}

/// A new program that says nothing within the first limit (a slow first launch) is asked
/// once more, with a longer one, and the daemon upgrades into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_first_launch_is_asked_once_more() {
    let mut rig = Rig::new(&[]).with_exe();
    rig.probe_wait = moa_daemon::ProbeWait {
        first: std::time::Duration::from_millis(300),
        again: WAIT,
    };
    let up = rig.start().await;
    rig.slow_next_build_id(2_000);
    rig.rebuild(1);
    let inherited = up.handoff().await;
    assert!(
        !rig.exe().with_extension("slow").exists(),
        "the slow launch was asked"
    );
    rig.restart(inherited).await.stop().await;
}
