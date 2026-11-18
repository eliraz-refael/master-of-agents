//! The terminal link on its own, against a real server (or a socket that turns every client
//! away), with the core played by a channel; and the core's handover to it.

use std::os::unix::net::UnixListener;
use std::sync::atomic::Ordering;

use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_core::state::{CallerInfo, Deps};
use moa_core::tmux::notify::Notification;
use moa_platform::fs::write_if_changed;
use moa_platform::spawn;
use moa_store::actor::Fault;
use moa_testkit::TempBrain;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use moa_tmux::conf;
use tokio::time::timeout;

use super::*;
use crate::core_task::{CoreHandle, CoreOptions};
use crate::testing::FakeTime;

const B: &str = "default";
const PATIENCE: Duration = Duration::from_secs(10);

// The launch counts read the process-wide `spawn::launches()`; they are exact only because
// nextest runs each test in a process of its own.

/// What the link told the core, each with the number of the actor's run that was going when
/// the core heard it (1 before any restart).
struct Heard(mpsc::UnboundedReceiver<(TmuxIn, usize)>);

impl Heard {
    async fn next_run(&mut self) -> (TmuxIn, usize) {
        timeout(PATIENCE, self.0.recv())
            .await
            .expect("the link said something in time")
            .expect("the link is still there")
    }

    async fn next(&mut self) -> TmuxIn {
        self.next_run().await.0
    }

    /// The next thing it said about being up or down, skipping events and answers, with
    /// the run it was heard in.
    async fn next_state_run(&mut self) -> (TmuxIn, usize) {
        loop {
            match self.next_run().await {
                (TmuxIn::Notification(_) | TmuxIn::Replies { .. }, _) => {}
                other => return other,
            }
        }
    }

    async fn next_state(&mut self) -> TmuxIn {
        self.next_state_run().await.0
    }

    /// Whether it says nothing about its state for `d`.
    async fn quiet_for(&mut self, d: Duration) -> Option<TmuxIn> {
        timeout(d, async {
            loop {
                match self.0.recv().await.map(|(input, _)| input) {
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
    let (actor, heard, _) = started_with(link, |_| {});
    (actor, heard)
}

/// The link started with `tweak` applied to what it shares across restarts, which the test
/// keeps a hold of.
fn started_with(
    link: TmuxLink,
    tweak: impl FnOnce(&mut Shared),
) -> (mpsc::UnboundedSender<TmuxCmd>, Heard, Arc<Shared>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let heard_in = runs.clone();
    let (actor, mut shared) = Shared::new(
        link,
        Box::new(move |input| {
            let _ = tx.send((input, heard_in.load(Ordering::SeqCst)));
        }),
    );
    shared.runs = runs;
    tweak(&mut shared);
    let shared = Arc::new(shared);
    supervised(shared.clone(), |_, _| {});
    (actor, Heard(rx), shared)
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

/// The server's control clients, by pid and name.
fn control_clients(tb: &TempBrain) -> Vec<(u32, String)> {
    let list = tb
        .tmux(
            B,
            &[
                "list-clients",
                "-F",
                "#{client_control_mode} #{client_pid} #{client_name}",
            ],
        )
        .unwrap_or_default();
    list.lines()
        .filter_map(|line| {
            let mut f = line.splitn(3, ' ');
            if f.next()? != "1" {
                return None;
            }
            Some((f.next()?.parse().ok()?, f.next()?.to_string()))
        })
        .collect()
}

/// Kills the process of the control client that `connected` names.
fn kill_client(tb: &TempBrain, connected: &TmuxIn) {
    let TmuxIn::Connected { ctl, .. } = connected else {
        panic!("not connected: {connected:?}");
    };
    let clients = control_clients(tb);
    let Some((pid, _)) = clients.iter().find(|(_, name)| *name == ctl.0) else {
        panic!("{ctl:?} isn't attached: {clients:?}");
    };
    moa_platform::kill_term(*pid);
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

/// A control client left attached by an earlier daemon, and still named in `@moa_ctl`, is
/// stopped when the link connects.
#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_link_from_before_is_reaped_on_connect() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let mut stuck = tb.control_client(B).expect("a control client starts");
    let pid = stuck.id();
    let name = moa_testkit::wait_for(PATIENCE, || {
        let clients = control_clients(&tb);
        clients.into_iter().find(|(p, _)| *p == pid).map(|(_, n)| n)
    })
    .expect("the stuck client attached");
    tb.tmux(B, &["set-option", "-g", "@moa_ctl", &name])
        .unwrap();
    let (actor, mut heard) = started(link);
    actor.send(connect()).unwrap();
    let ctl = match heard.next_state().await {
        TmuxIn::Connected { ctl, .. } => ctl,
        other => panic!("not connected: {other:?}"),
    };
    let exited = moa_testkit::wait_for(PATIENCE, || stuck.try_wait().ok().flatten());
    assert!(exited.is_some(), "the stuck client is still running");
    let gone = moa_testkit::wait_for(PATIENCE, || {
        let clients = control_clients(&tb);
        (!clients.iter().any(|(p, _)| *p == pid)).then_some(())
    });
    assert!(gone.is_some(), "{:?}", control_clients(&tb));
    assert_eq!(ctl_option(&tb), ctl.0);
}

/// Carry-forward (roadmap 11): the core heard of a panic only at the restart, a second or
/// more later.
#[tokio::test(flavor = "multi_thread")]
async fn a_panic_while_up_says_lost_at_once() {
    let tb = TempBrain::new(None);
    let (actor, mut heard) = started(server(&tb));
    actor.send(connect()).unwrap();
    let first = conn_of(&heard.next_state().await);
    actor.send(TmuxCmd::Crash).unwrap();
    let (lost, run) = heard.next_state_run().await;
    assert_eq!(lost, TmuxIn::Down(Down::Lost));
    assert_eq!(run, 1, "the loss waited for the restart");
    let (up, run) = heard.next_state_run().await;
    assert!(conn_of(&up) > first);
    assert_eq!(run, 2);
}

/// Carry-forward (roadmap 11): `lost()` marked the link idle before it probed, so a panic
/// there left the core at Lost with no reconnect.
#[tokio::test(flavor = "multi_thread")]
async fn a_panic_while_probing_still_reconnects() {
    let tb = TempBrain::new(None);
    let (actor, mut heard, shared) = started_with(server(&tb), |_| {});
    actor.send(connect()).unwrap();
    let up = heard.next_state().await;
    shared.panic_probe.store(true, Ordering::SeqCst);
    kill_client(&tb, &up);
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::Lost));
    let second = conn_of(&heard.next_state().await);
    assert!(second > conn_of(&up));
}

/// Carry-forward (roadmap 11): a restart during the backoff started over at step 0. Step 1
/// is 30 s here, so the restarted run scheduling it again is seen long before it is tried.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_during_backoff_resumes_its_step() {
    let tb = TempBrain::new(None);
    let mut waits = BACKOFF;
    waits[1..].fill(Duration::from_secs(30));
    let (actor, mut heard, shared) = started_with(server(&tb), |sh| sh.backoff = waits);
    actor.send(connect()).unwrap();
    // Up after step 0, then dropped again, so step 1 is next.
    for _ in 0..2 {
        let up = heard.next_state().await;
        kill_client(&tb, &up);
        assert_eq!(heard.next_state().await, TmuxIn::Down(Down::Lost));
    }
    let backing_off = moa_testkit::wait_for(PATIENCE, || {
        (shared.doing() == Doing::BackingOff(1)).then_some(())
    });
    assert!(backing_off.is_some(), "{:?}", shared.doing());
    actor.send(TmuxCmd::Crash).unwrap();
    let resumed = moa_testkit::wait_for(PATIENCE, || {
        let scheduled = lock(&shared.scheduled).clone();
        (scheduled.len() == 3).then_some(scheduled)
    });
    assert_eq!(resumed, Some(vec![(1, 0), (1, 1), (2, 1)]));
}

/// Sends `signal` to `pid` with the shell's own `kill`, which a build sandbox has even
/// where no `kill` program is installed. `moa_platform` has only `kill_term`, no stop or
/// continue, and this crate allows no `unsafe` to call libc itself.
fn signal(pid: &str, signal: &str) -> bool {
    std::process::Command::new("/bin/sh")
        .args(["-c", "kill -s \"$1\" \"$2\"", "sh", signal, pid])
        .status()
        .is_ok_and(|s| s.success())
}

/// Sends SIGCONT to a stopped process when dropped.
struct Resume(String);

impl Drop for Resume {
    fn drop(&mut self) {
        signal(&self.0, "CONT");
    }
}

/// Stops the brain's server; it continues when the guard drops. Declare the guard after
/// the brain, so it drops first: the brain's own drop talks to tmux.
fn stop_server(tb: &TempBrain) -> Resume {
    let pid = tb.tmux(B, &["display-message", "-p", "#{pid}"]).unwrap();
    assert!(signal(&pid, "STOP"), "the server stops");
    Resume(pid)
}

fn connect_options(link: &TmuxLink, wait: Duration) -> ConnectOptions {
    ConnectOptions {
        tmux: moa_testkit::tmux_path().unwrap_or_else(|e| panic!("{e}")),
        socket: link.socket.args().to_vec(),
        env: link.env.clone(),
        wait,
    }
}

/// A server that never answers (here, stopped) is given up on after `wait`, and says so as
/// a timeout, not as something tmux said.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_server_is_given_up_on_within_the_wait() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let _resume = stop_server(&tb);
    let opts = connect_options(&link, Duration::from_millis(300));
    let result = timeout(PATIENCE, ControlClient::connect(&opts))
        .await
        .expect("connect gave up in time");
    match result {
        Err(ConnectError::TimedOut) => {}
        Err(e) => panic!("{e:?}"),
        Ok((_, _, info)) => panic!("a stopped server answered: {info:?}"),
    }
}

/// Code review (b1): a connect that timed out was the final `CantLink`. It is tried again
/// like a server that turned the link away, so a server that was only slow is linked once
/// it answers. Connects wait 300 ms while the server is stopped, so the first one times out
/// soon; once it is resumed they wait a roomy 3 s, so the retry that links isn't raced.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_answers_late_is_tried_again() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let resume = stop_server(&tb);
    let (actor, mut heard, shared) = started_with(link, |sh| {
        *lock(&sh.connect_wait) = Duration::from_millis(300);
    });
    actor.send(connect()).unwrap();
    let retrying = moa_testkit::wait_for(PATIENCE, || {
        (!lock(&shared.scheduled).is_empty()).then_some(())
    });
    assert!(retrying.is_some(), "no retry: {:?}", shared.doing());
    *lock(&shared.connect_wait) = Duration::from_secs(3);
    drop(resume);
    conn_of(&heard.next_state().await);
}

/// Review fix (Task 1): a reap step that times out is a loss, as every bootstrap step's is,
/// not "nothing to reap". The server stops after the connect, so the reap's first command
/// is the one that waits.
#[tokio::test(flavor = "multi_thread")]
async fn a_reap_on_a_hung_server_is_a_loss() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let (client, _events, info) = connected_client(&link).await;
    let resume = stop_server(&tb);
    let by = Instant::now() + SETUP_WAIT;
    let reaped = timeout(PATIENCE, reap_stale(&client, &info.client, by))
        .await
        .expect("the reap gave up in time");
    assert_eq!(reaped, Err(CmdError::Gone));
    drop(resume);
    client.close().await;
}

/// A connected client on the brain's server, as the link has it before its setup.
async fn connected_client(link: &TmuxLink) -> (ControlClient, Events, ServerInfo) {
    timeout(
        PATIENCE,
        ControlClient::connect(&connect_options(link, CONNECT_WAIT)),
    )
    .await
    .expect("connected in time")
    .unwrap_or_else(|e| panic!("connect failed: {e}"))
}

/// Code review (b1): a server that stalls during the setup is a loss, given up on at the
/// setup's deadline.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_stalls_during_setup_is_a_loss() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let (client, _events, info) = connected_client(&link).await;
    let resume = stop_server(&tb);
    let by = Instant::now() + SETUP_WAIT;
    let set = timeout(PATIENCE, set_up(&client, &info, &link.moa_tmux, by))
        .await
        .expect("the setup gave up in time");
    assert_eq!(set, Err(CmdError::Gone));
    drop(resume);
    client.close().await;
}

/// Code review (b1): the setup's steps share one deadline, so connect and setup together
/// stay under the front door's 10 s; no step starts a wait of its own. With the deadline
/// already passed, a healthy server's setup is cut short, and the client is never named.
#[tokio::test(flavor = "multi_thread")]
async fn the_setup_steps_share_one_deadline() {
    let tb = TempBrain::new(None);
    let link = server(&tb);
    let (client, _events, info) = connected_client(&link).await;
    let set = timeout(
        PATIENCE,
        set_up(&client, &info, &link.moa_tmux, Instant::now()),
    )
    .await
    .expect("the setup ended in time");
    assert_eq!(set, Err(CmdError::Gone));
    assert_eq!(ctl_option(&tb), "", "the client was named");
    client.close().await;
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
    // Everything the link said, for a failure's message (roadmap 11: this failed once
    // under load, and its text was lost).
    let mut seen = Vec::new();
    // The backoff's waits add up to 6.3 s, plus a drop per connect.
    let gave_up = timeout(Duration::from_secs(30), async {
        while let Some((input, _)) = heard.0.recv().await {
            seen.push(input.clone());
            match input {
                TmuxIn::Notification(_) | TmuxIn::Replies { .. } => {}
                TmuxIn::Connected { .. } => {
                    assert!(lost_since_up, "a reconnect came before Lost: {seen:?}");
                    ups += 1;
                    assert!(
                        ups <= 1 + BACKOFF.len(),
                        "more than 1 + 6 connects: {seen:?}"
                    );
                    lost_since_up = false;
                }
                TmuxIn::Down(Down::Lost) => lost_since_up = true,
                TmuxIn::Down(Down::GaveUp) => return,
                _ => panic!("{seen:?}"),
            }
        }
        panic!("the link went away: {seen:?}");
    })
    .await;
    assert!(gave_up.is_ok(), "the link never gave up: {seen:?}");
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
    // The panic said Lost; the stop says nothing more.
    assert_eq!(heard.next_state().await, TmuxIn::Down(Down::Lost));
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
        0,
    )
    .await
    .expect("the core starts");
    let stats = core.stats();
    core.tmux_input(TmuxIn::Connected {
        conn: 1,
        generation: moa_core::tmux::ids::Generation { pid: 1, start: 1 },
        version: "3.8".into(),
        ctl: moa_core::tmux::ids::ClientName("client-1".into()),
        socket: "moa".into(),
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

/// The typical brain opening with `acct-a`, whose launcher is never run: the link is a
/// channel.
const OPEN_TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// A core that can open sessions, on a link played by a channel that is up on connection 1.
async fn opening_core(
    brain: &TypicalBrain,
    time: &Arc<FakeTime>,
) -> (CoreHandle, mpsc::UnboundedReceiver<TmuxCmd>) {
    let mut opts = core_options(brain, time);
    opts.config = moa_core::config::Config::parse(OPEN_TOML, &brain.home).unwrap();
    opts.deps = Deps {
        session_plugin: "/opt/moa/plugin/session".into(),
        ..Deps::default()
    };
    let (link, mut rx) = mpsc::unbounded_channel();
    let core = CoreHandle::start_inner(opts, brain.store(), vec![], Some(link), 0)
        .await
        .expect("the core starts");
    core.tmux_input(TmuxIn::Connected {
        conn: 1,
        generation: moa_core::tmux::ids::Generation { pid: 1, start: 1 },
        version: "3.8".into(),
        ctl: moa_core::tmux::ids::ClientName("client-1".into()),
        socket: "moa".into(),
    });
    core.barrier().await;
    sent(&mut rx);
    (core, rx)
}

fn open_lantern(brain: &TypicalBrain) -> moa_core::op::Op {
    moa_core::op::Op::SessionOpen {
        path: brain.home.join("private/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: false,
    }
}

/// The session an `Open` send is for.
fn opened_id(cmd: &TmuxCmd) -> Option<moa_core::model::SessionId> {
    match cmd {
        TmuxCmd::Send {
            token: Some(TmuxToken::Open(id)),
            ..
        } => Some(*id),
        _ => None,
    }
}

/// I4: an `open`'s new window starts an agent, so it is sent only once its session is
/// saved; then tmux's answer binds the pane and answers the open.
#[tokio::test]
async fn an_opens_new_window_waits_for_its_commit() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let (core, mut rx) = opening_core(&brain, &time).await;
    let stats = core.stats();
    let (c, mut replies) = core.open_conn();
    let (release, gate) = std::sync::mpsc::channel();
    core.store_fault(Fault::PauseNextCommit(gate));
    let commits = stats.commits.load(Ordering::SeqCst);
    core.send(
        c,
        moa_core::op::ReqId(1),
        CallerInfo::default(),
        open_lantern(&brain),
    );
    timeout(PATIENCE, async {
        while stats.commits.load(Ordering::SeqCst) == commits {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the open's commit started");
    assert!(
        !sent(&mut rx).iter().any(|c| opened_id(c).is_some()),
        "the new window went out before its session was saved"
    );
    release.send(()).unwrap();
    let id = timeout(PATIENCE, async {
        loop {
            if let Some(id) = opened_id(&rx.recv().await.expect("the link is held")) {
                return id;
            }
        }
    })
    .await
    .expect("the new window is sent after the commit");

    core.tmux_input(TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Open(id),
        results: vec![Ok(vec!["P %7 4242".into()])],
    });
    let (req, result) = timeout(PATIENCE, replies.recv()).await.unwrap().unwrap();
    assert_eq!(req, moa_core::op::ReqId(1));
    let done = result.expect("opened");
    assert!(
        done.text.starts_with("opened lantern with acct-a in "),
        "{}",
        done.text
    );
    core.barrier().await;
    assert!(
        sent(&mut rx).iter().any(|c| says(c, "@moa_session")),
        "the pane is tagged"
    );
    // The link is a channel, which never answers its stop: closing it skips the wait.
    drop(rx);
    core.stop().await;
}

/// I4: when the commit that saves an `open`'s session fails, its new window is dropped with
/// the drain, and the reload answers the open: nothing was opened.
#[tokio::test]
async fn an_open_whose_commit_fails_starts_no_tab() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let (core, mut rx) = opening_core(&brain, &time).await;
    let (c, mut replies) = core.open_conn();
    core.store_fault(Fault::FailNextCommit);
    core.send(
        c,
        moa_core::op::ReqId(1),
        CallerInfo::default(),
        open_lantern(&brain),
    );
    let (req, result) = timeout(PATIENCE, replies.recv()).await.unwrap().unwrap();
    assert_eq!(req, moa_core::op::ReqId(1));
    assert_eq!(
        result.unwrap_err().0,
        "moa couldn't save that session; nothing was opened, try again"
    );
    core.barrier().await;
    assert!(
        !sent(&mut rx).iter().any(|c| opened_id(c).is_some()),
        "a tab was opened for a session that wasn't saved"
    );
    assert_eq!(core.stats().commit_failures.load(Ordering::SeqCst), 1);
    // The link is a channel, which never answers its stop: closing it skips the wait.
    drop(rx);
    core.stop().await;
}
