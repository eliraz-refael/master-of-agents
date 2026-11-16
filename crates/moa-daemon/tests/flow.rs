//! A scripted flow of twenty verbs through `moa-client` against an in-process daemon: none of
//! them launches a process.

use std::time::Duration;

use clap::Parser;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_client::{Client, ClientError, ClientOptions, OnMismatch};
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_core::state::CallerInfo;
use moa_daemon::testing::FakeTime;
use moa_daemon::{Ready, RunOptions, load_config, run};
use moa_platform::spawn;
use moa_proto::cli::{Moactl, VerbCtx};
use moa_testkit::TempBrain;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use tokio::sync::oneshot;
use tokio::time::timeout;

/// A verb line as `moactl` would be given it, split on spaces; a `"…"` stays one word.
fn words(line: &str) -> Vec<String> {
    let mut out = vec!["moactl".to_string()];
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn op_of(line: &str, home: &std::path::Path) -> moa_core::op::Op {
    let m = Moactl::try_parse_from(words(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
    let ctx = VerbCtx {
        cwd: Some(home.to_path_buf()),
        home: home.to_path_buf(),
    };
    m.verb.to_op(&ctx).unwrap_or_else(|e| panic!("{line}: {e}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_verbs_launch_nothing() {
    let tb = TempBrain::new(None);
    tb.adopt_typical("default");
    let paths = tb.paths();
    let now: Timestamp = TYPICAL_NOW.parse().unwrap();
    let time = FakeTime::new(UtcMs(now.as_millisecond()));
    let (tx, rx) = oneshot::channel();
    let daemon = tokio::spawn(run(RunOptions {
        paths: paths.clone(),
        brain: BrainName::default_brain(),
        config: load_config(&paths).expect("a config"),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        zone: None,
        ready: Ready::Channel(tx),
        events_capacity: 256,
        uid_override: None,
    }));
    match timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(_)) => {}
        other => panic!("the daemon wasn't ready: {other:?}"),
    }
    let launched = spawn::launches();

    let mut client = Client::connect(ClientOptions {
        paths: paths.clone(),
        brain: BrainName::default_brain(),
        client: "flow-test",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: OnMismatch::Restart,
        build: moa_platform::build_id().into(),
    })
    .await
    .expect("connects to the running daemon");

    let verbs = [
        "brief",
        "attention",
        "ls",
        "ls --all",
        "find lantern",
        "project Lantern",
        "task ls",
        "task add Lantern Solder --due fri",
        "task set 6 --status active",
        "task set 6 --today",
        "note \"volts\" --project Lantern",
        "group add Work/Infra",
        "set b84e07d1 --group Work/Infra",
        "set b84e07d1 --title \"Tide tables\"",
        "group rename Work/Infra Work/Platform",
        "suggest \"Finish it?\" --do \"task set 6 --status done\"",
        "suggestion ls",
        "suggestion yes 5",
        "archive 6a2f8e40",
        "group rm Work/Platform",
    ];
    assert_eq!(verbs.len(), 20);
    for (i, line) in verbs.iter().enumerate() {
        let result = client.call(op_of(line, tb.root())).await;
        if i == 19 {
            // S2 is still in the group.
            assert!(
                matches!(result, Err(ClientError::Rpc(_))),
                "{line}: {result:?}"
            );
        } else {
            result.unwrap_or_else(|e| panic!("{line}: {e}"));
        }
    }

    assert_eq!(spawn::launches(), launched, "a verb launched a process");
    assert_eq!(client.status().await.unwrap().launches, 0);

    // `Client::shutdown` waits for the daemon's process to exit, and this one is ours.
    drop(client);
    daemon.abort();
}
