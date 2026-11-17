//! The terminal link on its own, against a real server (or a socket that turns every client
//! away), with the core played by a channel; and the core's handover to it.

use std::os::unix::net::UnixListener;
use std::sync::atomic::Ordering;

use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_core::state::{CallerInfo, Deps};
use moa_core::tmux::notify::Notification;
use moa_platform::spawn;
use moa_store::actor::Fault;
use moa_testkit::TempBrain;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use moa_tmux::conf::{self, write_if_changed};
use tokio::time::timeout;

use super::*;
use crate::core_task::{CoreHandle, CoreOptions};
use crate::testing::FakeTime;

const B: &str = "default";
const PATIENCE: Duration = Duration::from_secs(10);

// The launch counts read the process-wide `spawn::launches()`; they are exact only because
// nextest runs each test in a process of its own.

/// What the link told the core.
struct Heard(mpsc::UnboundedReceiver<TmuxIn>);

impl Heard {
    async fn next(&mut self) -> TmuxIn {
        timeout(PATIENCE, self.0.recv())
            .await
            .expect("the link said something in time")
            .expect("the link is still there")
    }

    /// The next thing it said about being up or down, skipping events and answers.
    async fn next_state(&mut self) -> TmuxIn {
        loop {
            match self.next().await {
                TmuxIn::Notification(_) | TmuxIn::Replies { .. } => {}
                other => return other,
            }
        }
    }

    /// Whether it says nothing about its state for `d`.
    async fn quiet_for(&mut self, d: Duration) -> Option<TmuxIn> {
        timeout(d, async {
            loop {
                match self.0.recv().await {
                    Some(TmuxIn::Notification(_) | TmuxIn::Replies { .. }) => {}
                    other => return other,
                }
            }
        })
        .await
        .ok()
        .flatten()
    }
}

fn started(link: TmuxLink) -> (mpsc::UnboundedSender<TmuxCmd>, Heard) {
    let (tx, rx) = mpsc::unbounded_channel();
    let actor = start(
        link,
        move |input| {
            let _ = tx.send(input);
        },
        |_, _| {},
    );
    (actor, Heard(rx))
}

/// The control client's environment, with the root as its home.
fn env(tb: &TempBrain) -> Vec<(OsString, OsString)> {
    let home = ("HOME".into(), tb.root().as_os_str().to_owned());
    tmux_env(
        std::env::vars_os()
            .filter(|(k, _)| k != "HOME")
            .chain([home]),
    )
}

/// The config files written and the server created as the front door does (T1).
fn server(tb: &TempBrain) -> TmuxLink {
    let paths = tb.paths();
    std::fs::create_dir_all(paths.brain_dir(B)).unwrap();
    let moa_tmux = paths.moa_tmux(B);
    write_if_changed(&moa_tmux, &conf::moa_tmux(B)).unwrap();
    write_if_changed(&paths.tmux_conf(B), &conf::tmux_conf(&moa_tmux)).unwrap();
    let conf = paths.tmux_conf(B).to_string_lossy().into_owned();
    let home = tb.root().to_string_lossy().into_owned();
    tb.tmux(
        B,
        &[
            "-f",
            &conf,
            "new-session",
            "-d",
            "-s",
            "_moa",
            "-x",
            "10",
            "-y",
            "3",
            "/bin/sh",
            ";",
            "new-session",
            "-d",
            "-s",
            "moa",
            "-n",
            "moa",
            "-c",
            &home,
        ],
    )
    .unwrap_or_else(|e| panic!("the server starts: {e}"));
    TmuxLink {
        socket: paths.tmux_socket(B).unwrap(),
        moa_tmux,
        env: env(tb),
    }
}

fn connect() -> TmuxCmd {
    TmuxCmd::Connect { tmux: None }
}

fn conn_of(input: &TmuxIn) -> u64 {
    match input {
        TmuxIn::Connected { conn, .. } => *conn,
        other => panic!("not connected: {other:?}"),
    }
}

fn ctl_option(tb: &TempBrain) -> String {
    tb.tmux(B, &["show-options", "-gqv", "@moa_ctl"]).unwrap()
}

fn sessions() -> Vec<Cmd> {
    vec![Cmd::new("list-sessions").args(["-F", "S #{session_name}"])]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_connect_reports_the_same_client_and_launches_nothing() {
    let tb = TempBrain::new(None);
    let (actor, mut heard) = started(server(&tb));
    let before = spawn::launches();
    actor.send(connect()).unwrap();
    let first = heard.next_state().await;
    let conn = conn_of(&first);
    actor.send(connect()).unwrap();
    assert_eq!(heard.next_state().await, first);
    assert_eq!(spawn::launches(), before + 1);

    // Plumbing on this connection is answered with its token; on another, it is dropped
    // (its list, which would come first, says `X`).
    actor
        .send(TmuxCmd::Send {
            conn: conn + 1,
            cmds: vec![Cmd::new("list-sessions").args(["-F", "X #{session_name}"])],
            token: Some(TmuxToken::Refresh),
        })
        .unwrap();
    actor
        .send(TmuxCmd::Send {
            conn,
            cmds: sessions(),
            token: Some(TmuxToken::Refresh),
        })
        .unwrap();
    let answer = loop {
        if let TmuxIn::Replies {
            conn: c,
            token,
            results,
        } = heard.next().await
        {
            break (c, token, results);
        }
    };
    assert_eq!(answer.0, conn);
    assert_eq!(answer.1, TmuxToken::Refresh);
    let listed = answer.2[0].as_ref().expect("the list");
    assert!(listed.contains(&"S moa".to_string()), "{listed:?}");
}

/// Carry-forward: the core keeps its topology through `Connected`, so the link always says
/// `Lost` before it reconnects.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_client_is_reported_before_the_reconnect() {
    let tb = TempBrain::new(None);
    let (actor, mut heard) = started(server(&tb));
    actor.send(connect()).unwrap();
    let first = conn_of(&heard.next_state().await);
    tb.tmux(B, &["detach-client", "-s", "_moa"]).unwrap();
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::Lost));
    let second = conn_of(&heard.next_state().await);
    assert!(second > first);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_is_reported_as_lost_before_it_reconnects() {
    let tb = TempBrain::new(None);
    let (actor, mut heard) = started(server(&tb));
    actor.send(connect()).unwrap();
    let first = conn_of(&heard.next_state().await);
    let old_ctl = ctl_option(&tb);
    actor.send(TmuxCmd::Crash).unwrap();
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::Lost));
    let second = conn_of(&heard.next_state().await);
    assert!(second > first, "a restart never reuses a number");
    let ctl = ctl_option(&tb);
    assert!(!ctl.is_empty() && ctl != old_ctl, "{ctl:?}");
    // The old client is gone with the actor that held it.
    let wait = moa_testkit::wait_for(Duration::from_secs(2), || {
        let list = tb
            .tmux(B, &["list-clients", "-F", "#{client_control_mode}"])
            .ok()?;
        (list.lines().filter(|l| *l == "1").count() == 1).then_some(())
    });
    assert!(wait.is_some(), "one control client");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_server_and_no_tmux_launch_nothing() {
    let tb = TempBrain::new(None);
    let paths = tb.paths();
    let link = TmuxLink {
        socket: paths.tmux_socket(B).unwrap(),
        moa_tmux: paths.moa_tmux(B),
        env: env(&tb),
    };
    let before = spawn::launches();
    let (actor, mut heard) = started(link.clone());
    actor.send(connect()).unwrap();
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::NoServer));

    let no_path = TmuxLink {
        env: link.env.into_iter().filter(|(k, _)| k != "PATH").collect(),
        ..link
    };
    let (actor, mut heard) = started(no_path);
    actor.send(connect()).unwrap();
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::NoTmux));
    assert_eq!(spawn::launches(), before);
}

/// A socket that takes every connection and closes it: tmux turns away each client, and the
/// link backs off. Connects asked meanwhile start nothing.
#[tokio::test(flavor = "multi_thread")]
async fn connects_while_backing_off_start_nothing_and_it_gives_up_after_six() {
    let dir = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("refuser");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let link = TmuxLink {
        socket: TmuxSocket::Path(sock),
        moa_tmux: dir.path().join("moa.tmux"),
        env: tmux_env(std::env::vars_os()),
    };
    let before = spawn::launches();
    let (actor, mut heard) = started(link);
    actor.send(connect()).unwrap();
    // The first connect fails; the backoff has started.
    let backing_off = moa_testkit::wait_for(Duration::from_secs(5), || {
        (spawn::launches() >= before + 2).then_some(())
    });
    assert!(backing_off.is_some(), "the link didn't retry");
    actor.send(connect()).unwrap();
    actor.send(connect()).unwrap();
    // The waits add up to 6.3 s.
    let said = heard.quiet_for(Duration::from_secs(20)).await;
    assert_eq!(said, Some(TmuxIn::Down(Down::GaveUp)));
    assert_eq!(spawn::launches() - before, 1 + BACKOFF.len() as u64);
}

/// A server whose user hook detaches every client once it has attached: the link keeps
/// counting its backoff through each drop and gives up after 1 + 6 launches, saying `Lost`
/// before every reconnect. Returns how many times it was up.
async fn dropped_after_each_attach(hook: impl FnOnce(&TempBrain) -> String) -> usize {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let hook = hook(&tb);
    tb.tmux(B, &["set-hook", "-g", "client-attached", &hook])
        .unwrap();
    let before = spawn::launches();
    let (actor, mut heard) = started(link);
    actor.send(connect()).unwrap();
    let mut ups = 0;
    let mut lost_since_up = true;
    // The backoff's waits add up to 6.3 s, plus a drop per connect.
    let gave_up = timeout(Duration::from_secs(30), async {
        loop {
            match heard.next_state().await {
                TmuxIn::Connected { .. } => {
                    assert!(lost_since_up, "a reconnect came before Lost");
                    ups += 1;
                    assert!(ups <= 1 + BACKOFF.len(), "more than 1 + 6 connects");
                    lost_since_up = false;
                }
                TmuxIn::Down(Down::Lost) => lost_since_up = true,
                TmuxIn::Down(Down::GaveUp) => break,
                other => panic!("{other:?}"),
            }
        }
    })
    .await;
    assert!(gave_up.is_ok(), "the link never gave up");
    let launched = spawn::launches() - before;
    assert!(launched <= 1 + BACKOFF.len() as u64, "{launched} launches");
    // It stays given up: nothing more is launched.
    assert_eq!(heard.quiet_for(Duration::from_millis(500)).await, None);
    assert_eq!(spawn::launches() - before, launched);
    ups
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_detaches_the_client_on_attach_is_given_up_on() {
    dropped_after_each_attach(|_| "detach-client".to_string()).await;
}

/// The drop comes 0.5 s after the attach, normally after the link is up, so the losses are
/// ones after a `Connected`. A bootstrap slower than that (a loaded runner) is dropped
/// mid-way instead, which counts the same, so only "at least one came up" is asserted.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_drops_the_client_soon_after_attach_is_given_up_on() {
    let ups = dropped_after_each_attach(|tb| {
        let sock = tb.paths().tmux_socket(B).unwrap().path(None, 0);
        format!(
            "run-shell -b \"sleep 0.5; tmux -S '{}' detach-client -t '#{{hook_client}}'\"",
            sock.display()
        )
    })
    .await;
    assert!(ups >= 1, "no connect came up before its drop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_no_server_stays_idle() {
    let tb = TempBrain::new(None);
    let paths = tb.paths();
    let link = TmuxLink {
        socket: paths.tmux_socket(B).unwrap(),
        moa_tmux: paths.moa_tmux(B),
        env: env(&tb),
    };
    let (actor, mut heard) = started(link);
    actor.send(connect()).unwrap();
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::NoServer));
    actor.send(TmuxCmd::Crash).unwrap();
    // The restart comes after supervise's 1 s; it says nothing and connects to nothing.
    assert_eq!(heard.quiet_for(Duration::from_secs(2)).await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_queued_before_a_restart_is_answered_without_a_reconnect() {
    let tb = TempBrain::new(None);
    let (actor, mut heard) = started(server(&tb));
    actor.send(connect()).unwrap();
    conn_of(&heard.next_state().await);
    let before = spawn::launches();
    actor.send(TmuxCmd::Crash).unwrap();
    let (done, stopped) = oneshot::channel();
    actor.send(TmuxCmd::Stop(done)).unwrap();
    timeout(PATIENCE, stopped)
        .await
        .expect("the stop is answered in time")
        .expect("the stop is answered");
    assert_eq!(spawn::launches(), before, "a restart reconnected");
    assert_eq!(heard.quiet_for(Duration::from_millis(300)).await, None);
}

// --- The core's handover to the link ---

fn core_options(brain: &TypicalBrain, time: &Arc<FakeTime>) -> CoreOptions {
    CoreOptions {
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
    }
}

/// Everything the core has sent the link so far.
fn sent(rx: &mut mpsc::UnboundedReceiver<TmuxCmd>) -> Vec<TmuxCmd> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn says(cmd: &TmuxCmd, text: &str) -> bool {
    let TmuxCmd::Send { cmds, .. } = cmd else {
        return false;
    };
    cmds.iter()
        .any(|c| c.argv().iter().any(|a| a.contains(text)))
}

fn untokened(cmd: &TmuxCmd) -> bool {
    matches!(cmd, TmuxCmd::Send { token: None, .. })
}

/// Carry-forward (T10): a view held by a drain whose commit fails is dropped, never sent to
/// the link; the reload's views are. One thread, as in `failures.rs`: the core counts a
/// commit and sends it to the store in one poll, so the fault queued after the count always
/// lands behind the primer's commit.
#[tokio::test]
async fn a_view_held_by_a_failed_commit_never_reaches_the_link() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let (link, mut rx) = mpsc::unbounded_channel();
    let core = CoreHandle::start_inner(
        core_options(&brain, &time),
        brain.store(),
        vec![],
        Some(link),
    )
    .await
    .expect("the core starts");
    let stats = core.stats();
    core.tmux_input(TmuxIn::Connected {
        conn: 1,
        generation: moa_core::tmux::ids::Generation { pid: 1, start: 1 },
        version: "3.8".into(),
        ctl: moa_core::tmux::ids::ClientName("client-1".into()),
    });
    core.barrier().await;
    let up = sent(&mut rx);
    assert!(matches!(up[0], TmuxCmd::Connect { .. }), "Startup connects");
    assert!(up.iter().any(untokened), "linking writes the status");

    let (c, mut replies) = core.open_conn();
    let (release, gate) = std::sync::mpsc::channel();
    core.store_fault(Fault::PauseNextCommit(gate));
    let commits = stats.commits.load(Ordering::SeqCst);
    let group = |name: &str| moa_core::op::Op::GroupAdd { path: name.into() };
    core.send(
        c,
        moa_core::op::ReqId(1),
        CallerInfo::default(),
        group("Primer"),
    );
    timeout(PATIENCE, async {
        while stats.commits.load(Ordering::SeqCst) == commits {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the primer's commit started");
    core.store_fault(Fault::FailNextCommit);
    // Queued behind the primer, in one drain: a write that fails and a key's notice.
    core.send(
        c,
        moa_core::op::ReqId(2),
        CallerInfo::default(),
        group("Doomed"),
    );
    core.tmux_input(TmuxIn::Notification(Notification::Message(
        "moa:key console %1 $0 /dev/ttys9".into(),
    )));
    release.send(()).unwrap();
    let first = timeout(PATIENCE, replies.recv()).await.unwrap().unwrap();
    assert!(first.1.is_ok(), "{first:?}");
    let second = timeout(PATIENCE, replies.recv()).await.unwrap().unwrap();
    assert!(second.1.is_err(), "{second:?}");
    core.barrier().await;

    let after = sent(&mut rx);
    assert!(
        !after.iter().any(|c| says(c, "isn't in this build yet")),
        "the failed drain's notice reached the link"
    );
    assert!(
        after.iter().any(untokened),
        "the reload writes the status again"
    );
    assert_eq!(stats.commit_failures.load(Ordering::SeqCst), 1);
}
