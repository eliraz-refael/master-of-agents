use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use std::path::Path;

use super::*;
use crate::testing::FakeTime;

async fn up(
    brain: &TypicalBrain,
) -> (
    CoreHandle,
    PathBuf,
    tokio::task::JoinHandle<Result<(), DaemonError>>,
) {
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
        tmux: None,
        spool: None,
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

    (core, sock, daemon)
}

type Reader = BufReader<tokio::net::unix::OwnedReadHalf>;

async fn connect(sock: &Path) -> (Reader, tokio::net::unix::OwnedWriteHalf) {
    let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
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
    (rd, wr)
}

async fn line(rd: &mut Reader) -> String {
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(10), rd.read_line(&mut line))
        .await
        .expect("a line within 10 s")
        .unwrap();
    assert!(n > 0, "the client was cut off with EOF");
    line
}

/// S1: a core that can't go on stops the daemon with its line, the socket file goes, and a
/// client with an op in flight is told something instead of being cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fatal_core_stops_the_daemon_and_answers_the_queue() {
    let brain = TypicalBrain::new();
    let (core, sock, daemon) = up(&brain).await;

    let (mut rd, mut wr) = connect(&sock).await;
    // The next input panics, and so does the reload after it: the core stops for good.
    core.panic_next_handles(2);
    let add = r#"{"jsonrpc":"2.0","id":2,"method":"group.add","params":{"path":"doomed"}}"#;
    wr.write_all(format!("{add}\n").as_bytes()).await.unwrap();
    let reply = line(&mut rd).await;
    assert!(reply.contains("\"error\""), "{reply}");

    let ended = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("the daemon ends within 10 s")
        .expect("no panic");
    assert!(matches!(ended, Err(DaemonError::Fatal(_))), "{ended:?}");
    assert!(!sock.exists(), "the socket file is removed");
}

/// An op the core holds (its commit paused) when another client shuts the daemon down is
/// still answered: the sessions stay open until the core has ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_op_held_by_the_core_at_shutdown_is_answered() {
    for round in 0..5 {
        let brain = TypicalBrain::new();
        let (core, sock, daemon) = up(&brain).await;
        let (mut b_rd, mut b_wr) = connect(&sock).await;
        let (mut a_rd, mut a_wr) = connect(&sock).await;

        let (release, rx) = std::sync::mpsc::channel();
        core.store_fault(moa_store::actor::Fault::PauseNextCommit(rx));
        let stats = core.stats();
        let commits = stats.commits.load(std::sync::atomic::Ordering::SeqCst);
        let add = format!(
            r#"{{"jsonrpc":"2.0","id":90,"method":"group.add","params":{{"path":"held{round}"}}}}"#
        );
        b_wr.write_all(format!("{add}\n").as_bytes()).await.unwrap();
        // The core has the op and its commit is under way (paused): genuinely in flight.
        tokio::time::timeout(Duration::from_secs(10), async {
            while stats.commits.load(std::sync::atomic::Ordering::SeqCst) == commits {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the core takes the op");

        let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#;
        a_wr.write_all(format!("{shutdown}\n").as_bytes())
            .await
            .unwrap();
        assert!(line(&mut a_rd).await.contains("\"result\":{}"));
        // Give the daemon the chance to (wrongly) close B's session, then let the commit go.
        tokio::time::sleep(Duration::from_millis(100)).await;
        release.send(()).unwrap();

        let reply = line(&mut b_rd).await;
        assert!(reply.contains(r#""id":90"#), "{reply}");
        assert!(reply.contains("\"result\""), "{reply}");
        let ended = tokio::time::timeout(Duration::from_secs(10), daemon)
            .await
            .expect("the daemon ends within 10 s")
            .expect("no panic");
        assert!(ended.is_ok(), "{ended:?}");
    }
}

/// A panic in the accept loop is published like the scheduler's: an error event, with no
/// contents. (The accept loop has no panic hook, so this goes through what its failure
/// callback calls.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_actor_failure_is_published_as_an_error_event() {
    let brain = TypicalBrain::new();
    let (core, _sock, _daemon) = up(&brain).await;
    let mut events = core.events();
    core.actor_failed("accept loop");
    let message = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let moa_core::publish::CoreEvent::Error { message } = events.recv().await.unwrap() {
                return message;
            }
        }
    })
    .await
    .expect("an error event");
    assert_eq!(message, "accept loop stopped and was restarted");
}
