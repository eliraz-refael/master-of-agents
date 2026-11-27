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
    up_with(brain, None, || {}).await
}

/// `up`, with the hook spool at `spool`, and `between` run once the core has started and
/// before the daemon accepts.
async fn up_with(
    brain: &TypicalBrain,
    spool: Option<PathBuf>,
    between: impl FnOnce(),
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
        spool,
        facts: None,
    })
    .await
    .expect("the core starts");
    between();
    let sock = brain.dir.path().join("t.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let sigs = Signals {
        term: signal(SignalKind::terminate()).unwrap(),
        int: signal(SignalKind::interrupt()).unwrap(),
        hup: signal(SignalKind::hangup()).unwrap(),
    };
    let up = Up {
        lock: File::create(brain.dir.path().join("t.lock")).unwrap(),
        listener: Arc::new(listener),
        sock: sock.clone(),
        core: core.clone(),
        sigs,
    };
    let exe = daemon_exe().unwrap();
    let build = moa_platform::build_id().to_string();
    let follow = Follow::new(exe.clone(), build.clone(), ProbeWait::default());
    let program = Arc::new(Program { exe, build });
    let daemon = tokio::spawn(async move {
        let uid = moa_platform::current_uid();
        let ended = keep_up(up, BrainName::default_brain(), program, uid, None, follow).await;
        ended.map(|handoff| assert!(handoff.is_none(), "no upgrade here"))
    });

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

/// I2: a hook that gave up on the daemon while it was starting (a line bigger than the
/// socket's buffer, written before the accept loop ran) is spooled after the drain at start.
/// It is drained once the daemon accepts, not at the next start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_spooled_during_start_is_drained_once_the_daemon_accepts() {
    let brain = TypicalBrain::new();
    let live = brain.dir.path().join("spool").join("default.jsonl");
    let line = r#"{"v":1,"agent":"claude","at_ns":1000000,"pid":1,"seq":1,"env":{},"payload":{"hook_event_name":"Stop","session_id":"a1b2c3d4-0000-4000-8000-000000000001"}}"#;
    let (core, _sock, daemon) = up_with(&brain, Some(live.clone()), || {
        moa_platform::spool::append(&live, line.as_bytes()).unwrap();
    })
    .await;
    let draining = moa_platform::spool::draining(&live);
    let drained = moa_testkit::wait_for(Duration::from_secs(10), || {
        let empty = std::fs::read_to_string(&live)
            .unwrap_or_default()
            .is_empty();
        // The drain counts itself after it empties the spool, so wait for the count too.
        let counted = core
            .stats()
            .spool_drains
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2;
        (empty && !draining.exists() && counted).then_some(())
    });
    assert!(drained.is_some(), "the line waits for the next start");
    core.stop().await;
    let ended = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("the daemon ends within 10 s")
        .expect("no panic");
    assert!(ended.is_ok(), "{ended:?}");
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

/// `CLAUDE_CODE_PLUGIN_DIRS` skips a relative entry, so the plugin's folder is made
/// absolute even under a relative home.
#[test]
fn the_plugin_dir_handed_to_sessions_is_absolute() {
    let relative = Paths::from_vars(None, Some("rel/home".into()), None, 501, true).unwrap();
    let dir = session_plugin_dir(&relative).unwrap();
    assert!(relative.session_plugin().is_relative());
    assert_eq!(
        dir,
        std::env::current_dir()
            .unwrap()
            .join(relative.session_plugin())
    );
    let rooted = Paths::with_root(Path::new("/tmp/moa-root"));
    assert_eq!(
        session_plugin_dir(&rooted).unwrap(),
        rooted.session_plugin()
    );
}

/// L7: a panel runs the daemon's program, and gets `MOA_ROOT` only from a sandboxed daemon.
#[test]
fn panels_get_moa_root_only_in_a_sandbox() {
    let exe = Path::new("/opt/moa/bin/moa");
    let rooted = Paths::with_root(Path::new("/tmp/moa-root"));
    assert_eq!(
        panel_launch(&rooted, exe),
        PanelLaunch {
            exe: exe.to_path_buf(),
            env: vec![("MOA_ROOT".into(), "/tmp/moa-root".into())],
        }
    );
    let real = Paths::from_vars(None, Some("/h".into()), None, 501, false).unwrap();
    assert_eq!(
        panel_launch(&real, exe),
        PanelLaunch {
            exe: exe.to_path_buf(),
            env: Vec::new()
        }
    );
}

/// Lesson 5: a daemon allowed the real home (`MOA_REAL_HOME=1`) passes that to its panels
/// explicitly, or every `moa panel` would refuse the real home; a sandboxed one never does.
#[test]
fn panels_get_moa_real_home_whenever_the_daemon_has_it() {
    let exe = Path::new("/opt/moa/bin/moa");
    let real = Paths::from_vars(None, Some("/h".into()), None, 501, true).unwrap();
    assert_eq!(
        panel_launch(&real, exe).env,
        [("MOA_REAL_HOME".to_string(), "1".to_string())]
    );
    let rooted = Paths::from_vars(Some("/r".into()), Some("/h".into()), None, 501, true).unwrap();
    assert_eq!(
        panel_launch(&rooted, exe).env,
        [("MOA_ROOT".to_string(), "/r".to_string())]
    );
}

/// The daemon's program, once: Linux names a binary rebuilt under a running daemon
/// `<path> (deleted)`, and the new one is at `<path>`.
#[test]
fn the_daemons_program_drops_linuxs_deleted_mark() {
    assert_eq!(
        undeleted(PathBuf::from("/opt/moa/bin/moa (deleted)")),
        Path::new("/opt/moa/bin/moa")
    );
    assert_eq!(
        undeleted(PathBuf::from("/opt/moa/bin/moa")),
        Path::new("/opt/moa/bin/moa")
    );
    // Its real path: the same file `moa_client::own_exe` names.
    let exe = daemon_exe().unwrap();
    assert!(exe.is_absolute(), "{exe:?}");
    assert_eq!(exe, std::fs::canonicalize(&exe).unwrap());
}

/// H16: a Claude account without a launcher gets the `claude` on `PATH`; one with a
/// launcher keeps it, and a Codex account is left alone.
#[test]
fn load_config_fills_launchers_from_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // Never run: only found.
    let claude = bin.join("claude");
    std::fs::write(&claude, "").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    let text = r#"
[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
launcher = "/opt/claude"
[accounts.acct-x]
kind = "codex"
store = "~/.codex-x"
"#;
    let parsed = || Config::parse(text, Path::new("/h")).unwrap();
    let launcher = |c: &Config, a: &str| {
        c.accounts[&moa_core::config::AccountName::parse(a).unwrap()]
            .launcher
            .clone()
    };
    let mut config = parsed();
    let path = std::env::join_paths(["/nonexistent".into(), bin.clone()]).unwrap();
    fill_launchers(&mut config, Some(&path));
    assert_eq!(launcher(&config, "acct-a"), Some(claude));
    assert_eq!(
        launcher(&config, "acct-b"),
        Some(PathBuf::from("/opt/claude"))
    );
    assert_eq!(launcher(&config, "acct-x"), None);

    let mut config = parsed();
    fill_launchers(&mut config, Some(OsStr::new("/nonexistent")));
    assert_eq!(launcher(&config, "acct-a"), None);
    fill_launchers(&mut config, None);
    assert_eq!(launcher(&config, "acct-a"), None);
}

/// The brain's folder is checked like the other private folders: a link in its place stops
/// the daemon before it starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brain_dir_that_is_a_symlink_is_refused() {
    let root = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::with_root(root.path());
    let real = root.path().join("elsewhere");
    ensure_private_dir(&real).unwrap();
    let link = paths.brain_dir("default");
    ensure_private_dir(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let mut config = brain.config();
    config.canonicalize_with(moa_platform::canonical);
    let opts = RunOptions {
        paths: paths.clone(),
        brain: BrainName::default_brain(),
        config,
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        zone: None,
        ready: Ready::Nothing,
        events_capacity: 16,
        uid_override: None,
        exe: daemon_exe().expect("the test's program"),
        build: moa_platform::build_id().to_string(),
        exec: Exec::Test(tokio::sync::mpsc::channel(1).0),
        inherited: None,
        probe_wait: ProbeWait::default(),
    };
    let err = tokio::time::timeout(Duration::from_secs(10), run(opts))
        .await
        .expect("the daemon stops before it starts")
        .expect_err("a linked brain folder is refused");
    assert!(
        matches!(
            &err,
            DaemonError::Paths(moa_platform::PathsError::UnsafeDir(dir, "is a symlink"))
                if *dir == link
        ),
        "{err:?}"
    );
}
