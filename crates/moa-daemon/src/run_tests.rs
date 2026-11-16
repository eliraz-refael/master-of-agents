use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;
use crate::testing::FakeTime;

/// S1: a core that can't go on stops the daemon with its line, the socket file goes, and a
/// client with an op in flight is told something instead of being cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fatal_core_stops_the_daemon_and_answers_the_queue() {
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
        events_capacity: 16,
    })
    .await
    .expect("the core starts");
    let sock = brain.dir.path().join("t.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let sigs = Signals {
        term: signal(SignalKind::terminate()).unwrap(),
        int: signal(SignalKind::interrupt()).unwrap(),
        hup: signal(SignalKind::hangup()).unwrap(),
    };
    let daemon = tokio::spawn(keep_up(
        core.clone(),
        listener,
        sock.clone(),
        BrainName::default_brain(),
        moa_platform::current_uid(),
        None,
        sigs,
    ));

    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let hello = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"hello","params":{{"client":"t","build":"{}","caller":{{}}}}}}"#,
        moa_platform::build_id()
    );
    wr.write_all(format!("{hello}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    rd.read_line(&mut line).await.unwrap();
    assert!(line.contains("\"matched\":true"), "{line}");

    // The next input panics, and so does the reload after it: the core stops for good.
    core.panic_next_handles(2);
    let add = r#"{"jsonrpc":"2.0","id":2,"method":"group.add","params":{"path":"doomed"}}"#;
    wr.write_all(format!("{add}\n").as_bytes()).await.unwrap();
    line.clear();
    let n = tokio::time::timeout(Duration::from_secs(10), rd.read_line(&mut line))
        .await
        .expect("a line within 10 s")
        .unwrap();
    assert!(n > 0, "the client was cut off with EOF");
    assert!(line.contains("\"error\""), "{line}");

    let ended = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("the daemon ends within 10 s")
        .expect("no panic");
    assert!(matches!(ended, Err(DaemonError::Fatal(_))), "{ended:?}");
    assert!(!sock.exists(), "the socket file is removed");
}
