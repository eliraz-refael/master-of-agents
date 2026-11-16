//! `moa-client` against the real `moa daemon`, autostarted into a `/tmp` sandbox root.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_client::{Client, ClientError, ClientOptions, OnMismatch};
use moa_core::config::BrainName;
use moa_core::op::Op;
use moa_core::publish::Topic;
use moa_core::state::CallerInfo;
use moa_platform::{build_id_of, pid_alive, spawn};
use moa_proto::code;
use moa_testkit::{TempBrain, wait_for};

fn moa_exe() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_moa")).expect("the moa binary exists")
}

/// Options for the default brain, built against `exe`'s build id (a test binary is a
/// different build from the `moa` it autostarts).
fn opts_for(tb: &TempBrain, exe: &Path, autostart: bool) -> ClientOptions {
    ClientOptions {
        paths: tb.paths(),
        brain: BrainName::default_brain(),
        client: "client-test",
        caller: CallerInfo::default(),
        autostart: autostart.then(|| exe.to_path_buf()),
        on_mismatch: OnMismatch::Restart,
        build: build_id_of(exe).expect("a build id"),
    }
}

fn opts(tb: &TempBrain, autostart: bool) -> ClientOptions {
    opts_for(tb, &moa_exe(), autostart)
}

#[tokio::test]
async fn a_fresh_root_autostarts() {
    let tb = TempBrain::new(None);
    assert!(!tb.root().join(".local").exists());
    let mut client = Client::connect(opts(&tb, true)).await.expect("autostart");
    assert!(client.hello().matched);
    assert_eq!(client.hello().brain, "default");
    let mode = std::fs::metadata(tb.paths().run_dir())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    // The daemon itself launched nothing (the launches here are this client's).
    assert_eq!(client.status().await.unwrap().launches, 0);
}

#[tokio::test]
async fn a_daemon_that_cannot_start_says_why() {
    let tb = TempBrain::new(Some("this is not [ toml"));
    let err = Client::connect(opts(&tb, true)).await.err().expect("fails");
    assert!(matches!(err, ClientError::Start(_)), "{err}");
    let text = err.to_string();
    assert!(text.starts_with("couldn't start moa: moa.toml"), "{text}");
    let log = tb.paths().stderr_log("default");
    assert!(
        text.ends_with(&format!("(log: {})", log.display())),
        "{text}"
    );
}

#[tokio::test]
async fn no_autostart_means_not_running() {
    let tb = TempBrain::new(None);
    let err = Client::connect(opts(&tb, false))
        .await
        .err()
        .expect("fails");
    assert!(matches!(err, ClientError::NotRunning(_)));
    assert_eq!(err.to_string(), "the default brain's moa isn't running");
}

/// D20: v1 may not touch an unsandboxed home before the switch-over, so autostart refuses
/// before creating anything or launching anything.
#[tokio::test]
async fn autostart_refuses_an_unswitched_real_home() {
    let home = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = moa_platform::Paths::from_vars(
        None,
        Some(home.path().into()),
        None,
        moa_platform::current_uid(),
        false,
    )
    .unwrap();
    let mut o = opts(&TempBrain::new(None), true);
    o.paths = paths;
    let before = spawn::launches();
    let err = Client::connect(o).await.err().expect("refused");
    assert!(
        matches!(
            err,
            ClientError::Paths(moa_platform::PathsError::NotSwitchedOver)
        ),
        "{err}"
    );
    assert!(!home.path().join(".local").exists());
    assert_eq!(spawn::launches(), before);
}

#[tokio::test]
async fn temp_brain_stops_its_daemons() {
    let tb = TempBrain::new(None);
    let mut client = Client::connect(opts(&tb, true)).await.expect("autostart");
    let pid = client.status().await.unwrap().pid;
    assert!(pid_alive(pid));
    // The client is still connected when the brain goes away.
    drop(tb);
    let gone = wait_for(Duration::from_secs(5), || (!pid_alive(pid)).then_some(()));
    assert!(gone.is_some(), "the daemon outlived its TempBrain");
    drop(client);
}

#[tokio::test]
async fn an_adopted_brain_is_served_and_subscribed() {
    let tb = TempBrain::new(None);
    tb.adopt_typical("default");
    assert!(tb.db("default").exists());
    let mut client = Client::connect(opts(&tb, true)).await.expect("autostart");
    let done = client.call(Op::SessionLs { all: true }).await.unwrap();
    assert!(!done.text.is_empty());
    let snapshot = client.subscribe(&[Topic::Sessions]).await.unwrap();
    assert!(!snapshot.topics[&Topic::Sessions].is_empty());
    // A refusal comes back as the daemon's message.
    let err = client
        .call(Op::GroupRm {
            path: "no-such-group".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Rpc(ref e) if e.code == code::FAILED),
        "{err}"
    );
}

#[tokio::test]
async fn clients_starting_at_once_share_one_daemon() {
    let tb = TempBrain::new(None);
    let (a, b) = tokio::join!(
        Client::connect(opts(&tb, true)),
        Client::connect(opts(&tb, true))
    );
    let (mut a, mut b) = (a.expect("a"), b.expect("b"));
    assert_eq!(a.hello().pid, b.hello().pid);
    assert_eq!(a.status().await.unwrap().pid, a.hello().pid);
    assert_eq!(b.status().await.unwrap().launches, 0);
}

#[tokio::test]
async fn a_different_build_is_restarted() {
    let tb = TempBrain::new(None);
    // The same binary at another path is another build.
    let other = tb.root().join("moa-other");
    std::fs::copy(moa_exe(), &other).unwrap();
    let other = std::fs::canonicalize(other).unwrap();
    let old = Client::connect(opts_for(&tb, &other, true))
        .await
        .expect("the first daemon");
    assert!(old.hello().matched);
    let old_pid = old.hello().pid;

    let new = Client::connect(opts(&tb, true)).await.expect("restarted");
    assert!(new.hello().matched);
    assert_ne!(new.hello().pid, old_pid);
    assert!(!pid_alive(old_pid));
}

#[tokio::test]
async fn a_different_build_is_kept_when_accepted() {
    let tb = TempBrain::new(None);
    let other = tb.root().join("moa-other");
    std::fs::copy(moa_exe(), &other).unwrap();
    let other = std::fs::canonicalize(other).unwrap();
    let old = Client::connect(opts_for(&tb, &other, true)).await.unwrap();
    let mut o = opts(&tb, true);
    o.on_mismatch = OnMismatch::Accept;
    let new = Client::connect(o).await.expect("accepted");
    assert!(!new.hello().matched);
    assert_eq!(new.hello().pid, old.hello().pid);
}

/// A daemon that is a different build after its restart too is reported, not restarted
/// again: one restart, then `BUILD_MISMATCH`. (Counts this process's launches, so it
/// needs nextest's process per test.)
#[tokio::test]
async fn a_restart_happens_at_most_once() {
    let tb = TempBrain::new(None);
    let mut o = opts(&tb, true);
    o.build = "not-the-daemons-build".into();
    let before = spawn::launches();
    let err = Client::connect(o).await.err().expect("still a mismatch");
    match &err {
        ClientError::Rpc(e) => assert_eq!(e.code, code::BUILD_MISMATCH),
        other => panic!("{other}"),
    }
    // The first start and the one restart.
    assert_eq!(spawn::launches() - before, 2);
}
