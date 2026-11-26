//! `moa-client` against the real `moa daemon`, autostarted into a `/tmp` sandbox root.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_client::{Client, ClientError, ClientOptions, LiveMsg, OnMismatch, UPGRADE_WAIT};
use moa_core::config::BrainName;
use moa_core::op::{Op, TaskAdd};
use moa_core::publish::Topic;
use moa_core::state::CallerInfo;
use moa_platform::{build_id_of, pid_alive, spawn};
use moa_proto::code;
use moa_testkit::{TempBrain, wait_for};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

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
        upgrade_wait: UPGRADE_WAIT,
    }
}

/// A copy of `moa` at `<root>/<dir>/moa`, by its real path: another install.
fn install(tb: &TempBrain, dir: &str) -> PathBuf {
    let dir = tb.root().join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(moa_exe(), dir.join("moa")).unwrap();
    std::fs::canonicalize(dir.join("moa")).unwrap()
}

/// A new build at `exe`: a copy written beside it, given a later time, and renamed over it
/// (never written into a running program).
fn rebuild(exe: &Path) {
    let next = exe.with_extension("next");
    std::fs::copy(moa_exe(), &next).unwrap();
    let later = std::fs::metadata(exe).unwrap().modified().unwrap() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&next)
        .unwrap()
        .set_modified(later)
        .unwrap();
    std::fs::rename(&next, exe).unwrap();
}

/// A daemon played in this process on `tb`'s socket, as process `pid` (a stand-in that
/// lives until the fake ends) on build `old`. It answers `upgrade` with `upgrade` (a
/// JSON-RPC `result` or `error` member). With `then`, it is a live-update daemon (its hello
/// names [`FAKE_EXE`]) that hangs up after answering `upgrade` and comes back, the same
/// pid, on build `then`. It answers `shutdown` by ending, and ends when the client hangs
/// up. `seen` is every request's method and params; awaiting it also surfaces the fake's
/// own failures.
struct FakeDaemon {
    pid: u32,
    seen: tokio::task::JoinHandle<Vec<(String, Value)>>,
}

/// The program a live-update [`FakeDaemon`] says it runs.
const FAKE_EXE: &str = "/tmp/elsewhere/moa";

/// A daemon from before live updates: a [`FakeDaemon`] that never comes back on a new build.
fn old_daemon(tb: &TempBrain, upgrade: Value) -> FakeDaemon {
    fake_daemon(tb, upgrade, None)
}

fn fake_daemon(tb: &TempBrain, upgrade: Value, then: Option<&'static str>) -> FakeDaemon {
    let paths = tb.paths();
    moa_platform::ensure_private_dir(&paths.run_dir()).unwrap();
    let sock = paths.socket("default").unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let mut stand_in = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = stand_in.id();
    let seen = tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut build = "old";
        'conns: loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap().to_string();
                seen.push((method.clone(), request["params"].clone()));
                let mut answer = match method.as_str() {
                    "hello" => {
                        let mut ok = json!({
                            "brain": "default", "build": build, "rev": 0, "pid": pid,
                            "matched": request["params"]["build"] == build,
                        });
                        if then.is_some() {
                            ok["exe"] = FAKE_EXE.into();
                        }
                        json!({ "result": ok })
                    }
                    "upgrade" => upgrade.clone(),
                    "shutdown" => json!({"result": {}}),
                    other => panic!("the fake daemon was asked {other}"),
                };
                answer["jsonrpc"] = "2.0".into();
                answer["id"] = request["id"].clone();
                write
                    .write_all(format!("{answer}\n").as_bytes())
                    .await
                    .unwrap();
                if method == "shutdown" {
                    break 'conns;
                }
                if method == "upgrade"
                    && let Some(next) = then
                {
                    // Hangs up, and the next connection meets the new build.
                    build = next;
                    continue 'conns;
                }
            }
            break;
        }
        let _ = stand_in.kill();
        let _ = stand_in.wait();
        let _ = std::fs::remove_file(&sock);
        seen
    });
    FakeDaemon { pid, seen }
}

/// The methods a fake daemon was asked, in order.
async fn methods(fake: FakeDaemon) -> Vec<String> {
    let seen = fake.seen.await.expect("the fake daemon ran to its end");
    seen.into_iter().map(|s| s.0).collect()
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

/// L15: a root that is gone (a test's `TempBrain` dropped before a late first connect) is
/// never brought back by a daemon started into it. (Counts this process's launches, so it
/// needs nextest's process per test.)
#[tokio::test]
async fn autostart_refuses_a_root_that_is_gone() {
    // The brain stays held, so its teardown would stop a daemon started by mistake.
    let tb = TempBrain::new(None);
    let o = opts(&tb, true);
    std::fs::remove_dir(tb.root()).unwrap();
    let before = spawn::launches();
    let err = Client::connect(o).await.err().expect("refused");
    assert!(matches!(err, ClientError::RootGone(_)), "{err}");
    assert_eq!(
        err.to_string(),
        format!("moa's folder {} is gone", tb.root().display())
    );
    assert_eq!(spawn::launches(), before);
    assert!(!tb.root().exists());
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

/// (Counts this process's launches, so it needs nextest's process per test.)
#[tokio::test]
async fn clients_starting_at_once_share_one_daemon() {
    let tb = TempBrain::new(None);
    let before = spawn::launches();
    let (a, b) = tokio::join!(
        Client::connect(opts(&tb, true)),
        Client::connect(opts(&tb, true))
    );
    let (mut a, mut b) = (a.expect("a"), b.expect("b"));
    assert_eq!(a.hello().pid, b.hello().pid);
    assert_eq!(a.status().await.unwrap().pid, a.hello().pid);
    assert_eq!(b.status().await.unwrap().launches, 0);
    // The start lock: one of the two launched, the other found it running.
    assert_eq!(spawn::launches() - before, 1);
}

/// L1: a daemon run by another install (here a copy of the binary elsewhere) is current for
/// its own program, so it says it isn't upgrading, and the client is refused: it never stops
/// another install's daemon.
#[tokio::test]
async fn another_install_is_refused_not_restarted() {
    let tb = TempBrain::new(None);
    let other = install(&tb, "other");
    let old = Client::connect(opts_for(&tb, &other, true))
        .await
        .expect("the first daemon");
    let pid = old.hello().pid;

    let err = Client::connect(opts(&tb, true))
        .await
        .err()
        .expect("refused");
    match &err {
        ClientError::Rpc(e) => assert_eq!(e.code, code::BUILD_MISMATCH),
        other => panic!("{other}"),
    }
    assert_eq!(
        err.to_string(),
        format!(
            "moa: brain 'default' is run by {}; this moa is {}. Use that one, or stop the \
             daemon first: moa shutdown",
            other.display(),
            moa_exe().display()
        )
    );
    assert!(pid_alive(pid));
    let mut o = opts(&tb, false);
    o.on_mismatch = OnMismatch::Accept;
    let again = Client::connect(o).await.expect("still running");
    assert_eq!(again.hello().pid, pid);
}

/// L1: the same install, rebuilt under a running daemon, is upgraded in place: the same
/// pid, now on the new build. (The daemon's own watch may upgrade it before the client
/// asks; either way it is the same process on the new build.)
#[tokio::test]
async fn a_stale_daemon_upgrades_in_place() {
    let tb = TempBrain::new(None);
    let exe = install(&tb, "inst");
    let old = Client::connect(opts_for(&tb, &exe, true))
        .await
        .expect("the first daemon");
    let (pid, old_build) = (old.hello().pid, old.hello().build.clone());
    drop(old);

    rebuild(&exe);
    let new_build = build_id_of(&exe).unwrap();
    assert_ne!(new_build, old_build);
    let new = Client::connect(opts_for(&tb, &exe, true))
        .await
        .expect("upgraded");
    assert!(new.hello().matched);
    assert_eq!(new.hello().build, new_build);
    assert_eq!(new.hello().pid, pid, "upgraded in place");
}

/// L1, the path pinned: a stale daemon says it is upgrading, hangs up, and comes back as
/// the same process on the client's build; the client takes it, starting and stopping
/// nothing.
#[tokio::test]
async fn a_daemon_that_upgrades_in_place_is_waited_for() {
    let tb = TempBrain::new(None);
    let o = opts(&tb, true);
    let next: &'static str = o.build.clone().leak();
    let fake = fake_daemon(&tb, json!({"result": {"upgrading": true}}), Some(next));
    let before = spawn::launches();
    let client = Client::connect(o).await.expect("upgraded");
    assert!(client.hello().matched);
    assert_eq!(client.hello().pid, fake.pid);
    assert_eq!(client.hello().build, next);
    assert_eq!(spawn::launches(), before);
    drop(client);
    assert_eq!(methods(fake).await, ["hello", "upgrade", "hello"]);
}

/// L1: a daemon that upgrades but comes back on yet another build runs another install's
/// program: the client is refused, naming it, and stops nothing.
#[tokio::test]
async fn a_daemon_back_on_another_build_is_refused() {
    let tb = TempBrain::new(None);
    let fake = fake_daemon(&tb, json!({"result": {"upgrading": true}}), Some("third"));
    let before = spawn::launches();
    let err = Client::connect(opts(&tb, true))
        .await
        .err()
        .expect("refused");
    match &err {
        ClientError::Rpc(e) => assert_eq!(e.code, code::BUILD_MISMATCH),
        other => panic!("{other}"),
    }
    assert_eq!(
        err.to_string(),
        format!(
            "moa: brain 'default' is run by {FAKE_EXE}; this moa is {}. Use that one, or stop \
             the daemon first: moa shutdown",
            moa_exe().display()
        )
    );
    assert_eq!(spawn::launches(), before);
    assert_eq!(methods(fake).await, ["hello", "upgrade", "hello"]);
}

/// L1: a daemon from before live updates refuses `upgrade {if_stale}` (or answers without
/// `upgrading`): the client stops it, waits for it to exit and starts its own, once. That
/// is the only upgrade that changes the pid.
#[tokio::test]
async fn an_old_daemon_is_restarted_once() {
    let refused = json!({"error": {"code": code::INVALID_PARAMS, "message": "no if_stale"}});
    for answer in [refused, json!({"result": {}})] {
        let tb = TempBrain::new(None);
        let fake = old_daemon(&tb, answer.clone());
        let before = spawn::launches();
        let mut client = Client::connect(opts(&tb, true)).await.expect("restarted");
        assert!(client.hello().matched, "{answer}");
        assert_ne!(client.hello().pid, fake.pid);
        assert_eq!(client.status().await.unwrap().pid, client.hello().pid);
        assert_eq!(spawn::launches() - before, 1, "one start: {answer}");
        let seen = fake.seen.await.expect("the fake daemon ran to its end");
        let methods: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(methods, ["hello", "upgrade", "shutdown"], "{answer}");
        assert_eq!(seen[1].1, json!({"if_stale": true}));
    }
}

/// L4: a daemon whose upgrade waits on moa's turn is waited for, up to the client's limit,
/// then the client says so. Nothing is stopped.
#[tokio::test]
async fn a_busy_moa_makes_the_client_wait_then_say_so() {
    let tb = TempBrain::new(None);
    let busy = json!({"result": {"upgrading": true, "waiting_for_moa": true}});
    let fake = old_daemon(&tb, busy);
    let mut o = opts(&tb, true);
    o.upgrade_wait = Duration::from_millis(300);
    let before = spawn::launches();
    let started = std::time::Instant::now();
    let err = Client::connect(o).await.err().expect("still in a turn");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(matches!(err, ClientError::StillInTurn), "{err}");
    assert_eq!(
        err.to_string(),
        "moa is finishing a turn; it upgrades right after. Try again in a moment."
    );
    assert_eq!(spawn::launches(), before);
    assert_eq!(methods(fake).await, ["hello", "upgrade"], "never stopped");
}

#[tokio::test]
async fn a_different_build_is_kept_when_accepted() {
    let tb = TempBrain::new(None);
    let other = install(&tb, "other");
    let old = Client::connect(opts_for(&tb, &other, true)).await.unwrap();
    let mut o = opts(&tb, true);
    o.on_mismatch = OnMismatch::Accept;
    let new = Client::connect(o).await.expect("accepted");
    assert!(!new.hello().matched);
    assert_eq!(new.hello().pid, old.hello().pid);
}

/// A daemon from before live updates that is a different build after its restart too is
/// reported, not restarted again: one restart, then `BUILD_MISMATCH`. (Counts this
/// process's launches, so it needs nextest's process per test.)
#[tokio::test]
async fn a_restart_happens_at_most_once() {
    let tb = TempBrain::new(None);
    let refused = json!({"error": {"code": code::INVALID_PARAMS, "message": "no if_stale"}});
    let fake = old_daemon(&tb, refused);
    let mut o = opts(&tb, true);
    o.build = "not-the-daemons-build".into();
    let before = spawn::launches();
    let err = Client::connect(o).await.err().expect("still a mismatch");
    match &err {
        ClientError::Rpc(e) => assert_eq!(e.code, code::BUILD_MISMATCH),
        other => panic!("{other}"),
    }
    assert_eq!(
        err.to_string(),
        "moa was restarted and is still a different build"
    );
    // The one restart.
    assert_eq!(spawn::launches() - before, 1);
    assert_eq!(methods(fake).await, ["hello", "upgrade", "shutdown"]);
}

/// No client stops a daemon it can't restart: with no `autostart` a mismatch is reported
/// and the daemon is left running.
#[tokio::test]
async fn a_mismatch_without_autostart_does_not_stop_the_daemon() {
    let tb = TempBrain::new(None);
    let other = install(&tb, "other");
    let old = Client::connect(opts_for(&tb, &other, true)).await.unwrap();
    let pid = old.hello().pid;
    // `opts` is another build, and `Restart` is its mode, but there is no exe to start.
    let err = Client::connect(opts(&tb, false))
        .await
        .err()
        .expect("refused");
    match &err {
        ClientError::Rpc(e) => assert_eq!(e.code, code::BUILD_MISMATCH),
        other => panic!("{other}"),
    }
    assert!(pid_alive(pid));
    drop(old);
}

/// `Live` on a real daemon: a sent op comes back as its tagged reply, and the change it made
/// arrives as a patch.
#[tokio::test]
async fn live_follows_a_real_daemon() {
    let tb = TempBrain::new(None);
    tb.adopt_typical("default");
    let client = Client::connect(opts(&tb, true)).await.expect("autostart");
    let (mut live, replies, _) = client.live(&[Topic::Tasks], vec![]).await.unwrap();
    assert!(replies.is_empty());
    let tag = live.send(Op::TaskAdd(TaskAdd {
        project: "Lantern".into(),
        title: "Probe".into(),
        ..TaskAdd::default()
    }));
    let probe = |patch: &moa_proto::Patch| {
        patch
            .upsert
            .get(&Topic::Tasks)
            .is_some_and(|rows| rows.iter().any(|r| r["title"] == "Probe"))
    };
    let (mut replied, mut patched) = (false, false);
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        while !(replied && patched) {
            match live.recv().await {
                LiveMsg::Reply { tag: t, result } if t == tag => {
                    assert!(result.is_ok(), "{result:?}");
                    replied = true;
                }
                LiveMsg::Patch(p) if probe(&p) => patched = true,
                LiveMsg::Closed => panic!("the connection closed"),
                _ => {}
            }
        }
    })
    .await;
    assert!(seen.is_ok(), "replied {replied}, patched {patched}");
}
