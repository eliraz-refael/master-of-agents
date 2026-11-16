mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_core::op::{Op, ReqId};
use moa_core::state::{CallerInfo, Deps};
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions, DaemonError, Ready, run, serve};
use moa_platform::{Paths, PathsError};
use moa_proto::code;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_then_ops_over_the_socket() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    let hello = c.hello();
    assert_eq!(hello["result"]["matched"], json!(true));
    assert_eq!(hello["result"]["brain"], json!("default"));
    let ls = c.call("task.ls", json!({}));
    let text = ls["result"]["text"].as_str().expect("a text");
    assert!(text.contains("## Lantern"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_message_must_be_hello() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    let r = c.call("task.ls", json!({}));
    assert_eq!(r["error"]["code"], json!(code::HELLO_FIRST));
    assert_eq!(r["error"]["message"], json!("say hello first"));
    // Saying hello afterwards works on the same connection.
    assert_eq!(c.hello()["result"]["matched"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hook_line_alone_is_accepted() {
    let d = boot().await;
    {
        let mut c = Client::connect(&d.sock);
        c.write(r#"{"jsonrpc":"2.0","method":"hook","params":{"event":"SessionStart"}}"#);
    }
    let mut c = Client::connect(&d.sock);
    assert_eq!(c.hello()["result"]["matched"], json!(true));
    assert!(c.call("status", json!({}))["result"]["brain"] == json!("default"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mismatched_build_may_only_hello_status_upgrade_shutdown_and_hook() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    assert_eq!(c.hello_as("x")["result"]["matched"], json!(false));
    let r = c.call("task.ls", json!({}));
    assert_eq!(r["error"]["code"], json!(code::BUILD_MISMATCH));
    assert_eq!(
        r["error"]["message"],
        json!("this moa is a different build; run the command again to restart it")
    );
    let s = c.call("status", json!({}));
    assert_eq!(s["result"]["build"], json!(moa_platform::build_id()));
    // A hook is dropped without a reply, even one with an id: the next status is the next
    // line we read.
    c.write(r#"{"jsonrpc":"2.0","method":"hook","params":{}}"#);
    c.write(r#"{"jsonrpc":"2.0","id":"h","method":"hook","params":{}}"#);
    let s = c.call("status", json!({}));
    assert!(s["result"].is_object(), "{s}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_sends_a_snapshot_then_patches() {
    let d = boot().await;
    let mut c1 = Client::connect(&d.sock);
    c1.hello();
    let sub = c1.call("subscribe", json!({"topics": ["tasks"]}));
    let rows = sub["result"]["topics"]["tasks"].as_array().expect("rows");
    assert_eq!(rows.len(), 5);
    let rev = sub["result"]["rev"].as_u64().expect("a rev");

    let mut c2 = Client::connect(&d.sock);
    c2.hello();
    let added = c2.call("task.add", json!({"project": "Lantern", "title": "Extra"}));
    assert!(added["result"].is_object(), "{added}");

    let patch = c1.notice("patch");
    assert_eq!(patch["upsert"]["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(patch["rev"].as_u64(), Some(rev + 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lagging_subscriber_gets_a_fresh_snapshot() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let core = CoreHandle::start(CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps::default(),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        // One event of slack: with a stalled reader, ten calls overflow it for certain.
        events_capacity: 1,
    })
    .await
    .expect("the core starts");

    // A small pipe, so the session stalls on a write while we don't read.
    let (client, server) = duplex(256);
    let stop = CancellationToken::new();
    tokio::spawn(serve(
        server,
        core.clone(),
        BrainName::default_brain(),
        stop.clone(),
    ));
    let (rd, mut wr) = tokio::io::split(client);
    let mut rd = BufReader::new(rd);
    let mut read = async || -> Value {
        let mut line = String::new();
        let n = timeout(Duration::from_secs(10), rd.read_line(&mut line))
            .await
            .expect("a line within 10 s")
            .expect("readable");
        assert!(n > 0, "the session closed");
        serde_json::from_str(&line).unwrap()
    };
    let hello = json!({"client": "t", "build": moa_platform::build_id(), "caller": {}});
    wr.write_all(format!("{}\n", request(1, "hello", hello)).as_bytes())
        .await
        .unwrap();
    assert_eq!(read().await["id"], json!(1));
    wr.write_all(
        format!(
            "{}\n",
            request(2, "subscribe", json!({"topics": ["groups"]}))
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(read().await["id"], json!(2));

    // Now stop reading, and make ten changes through another connection.
    let (conn, mut replies) = core.open_conn();
    for i in 0..10 {
        core.send(
            conn,
            ReqId(i),
            CallerInfo::default(),
            Op::GroupAdd {
                path: format!("lag{i}"),
            },
        );
    }
    for _ in 0..10 {
        let (_, r) = timeout(Duration::from_secs(10), replies.recv())
            .await
            .expect("a reply")
            .expect("open");
        r.expect("the group is added");
    }
    core.barrier().await;

    // Reading again, a snapshot arrives (after whatever patches were already in flight).
    let mut snapshot = None;
    for _ in 0..200 {
        let v = read().await;
        if v["method"] == json!("snapshot") {
            snapshot = Some(v["params"].clone());
            break;
        }
    }
    let snapshot = snapshot.expect("a snapshot notice");
    assert_eq!(snapshot["rev"].as_u64(), Some(core.model().borrow().rev));
    let groups = snapshot["topics"]["groups"].as_array().unwrap();
    assert!(
        groups.iter().any(|g| g["path"] == json!("lag9")),
        "{groups:?}"
    );
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn garbage_and_oversized_lines_get_an_error_not_a_crash() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    c.write("not json");
    assert_eq!(c.read().unwrap()["error"]["code"], json!(code::PARSE));

    let mut big = vec![b'a'; 9 << 20];
    big.push(b'\n');
    c.write_bytes(&big);
    let r = c.read().expect("the error line, not a reset");
    assert_eq!(r["error"]["code"], json!(code::TOO_LONG));
    assert_eq!(r["error"]["message"], json!("that message is too long"));
    assert!(c.read().is_none(), "the connection closes");

    let mut c = Client::connect(&d.sock);
    assert_eq!(c.hello()["result"]["matched"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_uid_is_turned_away() {
    let mine = moa_platform::current_uid();
    let d = boot_with(|_| {}, |o| o.uid_override = Some(mine + 1)).await;
    let mut c = Client::connect(&d.sock);
    // The daemon closes the connection without a word: the write may or may not get
    // through, and the read ends in EOF or a reset.
    c.try_write(&request(1, "status", json!({})));
    assert!(c.read().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_daemon_says_already_running() {
    let d = boot().await;
    let opts = options(&d.paths, &d.brain, &d.time, Ready::Nothing);
    let err = run(opts).await.expect_err("the lock is taken");
    assert!(matches!(err, DaemonError::AlreadyRunning(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "the default brain's moa is already running"
    );
    // The first one is unharmed.
    let mut c = Client::connect(&d.sock);
    assert_eq!(c.hello()["result"]["matched"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_socket_file_is_replaced() {
    let d = boot_with(
        |paths| {
            moa_platform::ensure_private_dir(&paths.run_dir()).unwrap();
            std::fs::write(paths.socket("default").unwrap(), b"stale").unwrap();
        },
        |_| {},
    )
    .await;
    let mut c = Client::connect(&d.sock);
    assert_eq!(c.hello()["result"]["matched"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_socket_dir_is_private() {
    let d = boot().await;
    let mode = std::fs::metadata(d.paths.run_dir())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    let root = short_root();
    let paths = Paths::with_root(root.path());
    let brain = TypicalBrain::new();
    place_db(&paths, &brain);
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    std::fs::set_permissions(paths.run_dir(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let time = FakeTime::new(UtcMs(0));
    let err = run(options(&paths, &brain, &time, Ready::Nothing))
        .await
        .expect_err("an open run dir is refused");
    assert!(
        matches!(err, DaemonError::Paths(PathsError::UnsafeDir(..))),
        "{err:?}"
    );
    let line = err.to_string();
    assert!(
        line.contains("is open to other users; moa won't use it (chmod 700 "),
        "{line}"
    );
}

async fn stops_cleanly(method: &str) {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    c.hello();
    let r = c.call(method, json!({}));
    assert_eq!(r["result"], json!({}));
    let ended = timeout(Duration::from_secs(10), d.done)
        .await
        .expect("run ends within 10 s")
        .expect("run didn't panic");
    assert!(ended.is_ok(), "{ended:?}");
    assert!(!d.sock.exists(), "the socket file is removed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_stops_cleanly() {
    stops_cleanly("shutdown").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_stops_cleanly() {
    stops_cleanly("upgrade").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipelining_client_gets_its_replies_in_order() {
    let d = boot().await;
    let mut c = Client::connect(&d.sock);
    c.hello();
    let lines = [
        request(10, "group.add", json!({"path": "p1"})),
        request(11, "group.add", json!({"path": "p2"})),
        request(12, "task.ls", json!({})),
    ];
    c.write(&lines.join("\n"));
    for id in [10, 11, 12] {
        assert_eq!(c.read().unwrap()["id"], json!(id));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brain_missing_from_the_config_is_refused() {
    let root = short_root();
    let paths = Paths::with_root(root.path());
    let brain = TypicalBrain::new();
    let time = FakeTime::new(UtcMs(0));
    let mut opts = options(&paths, &brain, &time, Ready::Nothing);
    opts.brain = BrainName::parse("work").unwrap();
    let err = run(opts).await.expect_err("work isn't configured");
    assert!(matches!(err, DaemonError::NoSuchBrain(..)), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!(
            "there's no brain named work in {}",
            paths.config_file().display()
        )
    );
}
