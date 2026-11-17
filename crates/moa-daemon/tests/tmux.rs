//! The terminal link against a real tmux server: it finds a server without starting one,
//! adopts it, reconnects when its client is detached, and takes its hooks down on a clean
//! stop. Every server here is created by the test, as the front door would (T1), and is
//! reached only through `TempBrain::tmux` (T23).

mod common;

use std::path::PathBuf;
use std::time::Duration;

use moa_daemon::{CoreHandle, Ready, run};
use moa_platform::Paths;
use moa_store::actor::Fault;
use moa_testkit::brain::TypicalBrain;
use moa_testkit::{TempBrain, ids, wait_for};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::time::timeout;

use common::*;

const B: &str = "default";
const NOT_SWITCHED_OVER: &str = "moa v1 isn't switched over yet; try moa --sandbox";
/// The backoff's connects.
const BACKOFF_CONNECTS: u64 = 6;

struct Rig {
    paths: Paths,
    brain: TypicalBrain,
    sock: PathBuf,
    core: CoreHandle,
    done: Done,
    /// Dropped last (fields drop in order): it stops the daemon, then kills the server.
    tb: TempBrain,
}

/// An in-process daemon on a TempBrain root, with the Typical brain's DB.
async fn start_on(tb: TempBrain, paths: Paths) -> Rig {
    let brain = TypicalBrain::new();
    place_db(&paths, &brain);
    let time = moa_daemon::testing::FakeTime::new(brain.now().wall);
    let (tx, rx) = oneshot::channel();
    let done = tokio::spawn(run(options(&paths, &brain, &time, Ready::Core(tx))));
    let (sock, core) = match timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(up)) => up,
        Ok(Err(_)) => panic!("run ended before ready: {:?}", done.await),
        Err(_) => panic!("run was not ready after 10 s"),
    };
    Rig {
        paths,
        brain,
        sock,
        core,
        done,
        tb,
    }
}

async fn start() -> Rig {
    let tb = TempBrain::new(None);
    let paths = tb.paths();
    start_on(tb, paths).await
}

impl Rig {
    fn client(&self) -> Client {
        let mut c = Client::connect(&self.sock);
        c.hello();
        c
    }

    /// Creates the server exactly as the front door does (T1).
    fn create_server(&self) {
        let conf = self.paths.tmux_conf(B);
        let home = self.paths.home().to_string_lossy().into_owned();
        self.tmux(&[
            "-f",
            &conf.to_string_lossy(),
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
        ]);
    }

    fn tmux(&self, args: &[&str]) -> String {
        self.tb
            .tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    }

    fn option(&self, name: &str) -> String {
        self.tmux(&["show-options", "-gqv", name])
    }

    /// The names of the server's control clients.
    fn control_clients(&self) -> Vec<String> {
        let list = self.tmux(&[
            "list-clients",
            "-F",
            "#{client_control_mode} #{client_name}",
        ]);
        list.lines()
            .filter_map(|l| l.strip_prefix("1 "))
            .map(str::to_string)
            .collect()
    }

    /// Both hook lists: session hooks and window hooks are listed apart.
    fn hooks(&self) -> String {
        self.tmux(&["show-hooks", "-g"]) + "\n" + &self.tmux(&["show-hooks", "-gw"])
    }

    fn counts_text(&self) -> String {
        moa_core::tmux::status::counts_text(&self.brain.state())
    }

    /// A clean stop: `shutdown`, then `run` returns.
    async fn stop(&mut self) {
        let mut c = self.client();
        assert_eq!(c.call("shutdown", json!({}))["result"], json!({}));
        drop(c);
        let ended = timeout(Duration::from_secs(10), &mut self.done)
            .await
            .expect("run ends within 10 s")
            .expect("no panic");
        assert!(ended.is_ok(), "{ended:?}");
    }
}

fn status(c: &mut Client) -> Value {
    c.call("status", json!({}))["result"].clone()
}

fn launches(c: &mut Client) -> u64 {
    status(c)["launches"].as_u64().expect("launches")
}

fn ensure(c: &mut Client) -> Value {
    c.call("tmux.ensure", json!({}))
}

/// Waits (up to 2 s) for `ok`, which may block on the daemon.
fn within_2s(what: &str, mut ok: impl FnMut() -> bool) {
    let done = wait_for(Duration::from_secs(2), || ok().then_some(()));
    assert!(done.is_some(), "{what} within 2 s");
}

/// A daemon adopting a server the test created: ensure says running.
async fn adopted() -> (Rig, Client) {
    let rig = start().await;
    rig.create_server();
    let mut c = rig.client();
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": true}));
    (rig, c)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_server_ensure_says_not_running_and_launches_nothing() {
    let rig = start().await;
    let mut c = rig.client();
    let before = launches(&mut c);
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": false}));
    assert_eq!(launches(&mut c), before);
    assert_eq!(status(&mut c)["tmux"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_never_starts_a_server() {
    let rig = start().await;
    let mut c = rig.client();
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": false}));
    let socket = rig.paths.tmux_socket(B).unwrap().path(None, 0);
    assert!(!socket.exists(), "{} exists", socket.display());
}

/// The `moa` session is the front door's, made in the user's environment: a server with
/// only `_moa` is running, and the daemon leaves it that way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_never_makes_the_users_session() {
    let rig = start().await;
    rig.create_server();
    rig.tmux(&["kill-session", "-t", "=moa"]);
    let mut c = rig.client();
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": true}));
    let want = rig.counts_text();
    within_2s("@moa_status shows the counts", || {
        rig.option("@moa_status") == want
    });
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": true}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        rig.tmux(&["list-sessions", "-F", "#{session_name}"]),
        "_moa"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_adopts_a_running_server() {
    let (rig, mut c) = adopted().await;
    let s = status(&mut c);
    assert!(s["tmux"]["version"].is_string(), "{s}");

    let hooks = rig.hooks();
    for h in [
        "pane-exited[90]",
        "pane-died[90]",
        "window-layout-changed[90]",
        "client-attached[90]",
        "client-detached[90]",
    ] {
        assert!(hooks.contains(h), "{h} isn't set:\n{hooks}");
    }
    assert_eq!(hooks.matches("[90]").count(), 5, "{hooks}");

    let ctl = rig.control_clients();
    assert_eq!(ctl.len(), 1, "{ctl:?}");
    assert_eq!(rig.option("@moa_ctl"), ctl[0]);

    let want = rig.counts_text();
    assert!(!want.is_empty());
    within_2s("@moa_status shows the counts", || {
        rig.option("@moa_status") == want
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_removes_hooks_and_the_ctl_option() {
    let (mut rig, c) = adopted().await;
    drop(c);
    assert!(!rig.option("@moa_ctl").is_empty());
    rig.stop().await;
    let hooks = rig.hooks();
    assert!(!hooks.contains("[90]"), "{hooks}");
    assert_eq!(rig.option("@moa_ctl"), "");
    // The server itself is left running.
    assert!(rig.control_clients().is_empty());
    assert!(
        rig.tmux(&["list-sessions", "-F", "#{session_name}"])
            .contains("moa")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_reconnects_after_its_client_is_detached() {
    let (rig, mut c) = adopted().await;
    let old = rig.option("@moa_ctl");
    assert!(!old.is_empty());
    let before = launches(&mut c);
    rig.tmux(&["detach-client", "-s", "_moa"]);
    within_2s("a new control client is linked", || {
        let ctl = rig.option("@moa_ctl");
        !ctl.is_empty()
            && ctl != old
            && rig.control_clients() == [ctl]
            && !status(&mut c)["tmux"].is_null()
    });
    let s = status(&mut c);
    assert_eq!(s["tmux_restarts"], json!(0), "{s}");
    assert_eq!(s["launches"].as_u64().unwrap(), before + 1, "{s}");
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": true}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_brains_stay_off_tmux() {
    let tb = TempBrain::new(None);
    let paths = Paths::from_vars(
        None,
        Some(tb.root().into()),
        None,
        moa_platform::current_uid(),
        true,
    )
    .unwrap();
    assert!(!paths.is_sandboxed());
    let rig = start_on(tb, paths).await;
    let mut c = rig.client();
    let before = launches(&mut c);
    let e = ensure(&mut c);
    assert_eq!(e["error"]["message"], json!(NOT_SWITCHED_OVER), "{e}");
    let s = status(&mut c);
    assert_eq!(s["tmux"], Value::Null);
    assert_eq!(s["launches"].as_u64().unwrap(), before);
    let log = std::fs::read_to_string(rig.paths.log(B)).unwrap_or_default();
    assert!(
        log.contains("the terminal link waits for the switch-over"),
        "{log}"
    );
    // No config was written for a server v1 doesn't own yet.
    assert!(!rig.paths.moa_tmux(B).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_commit_keeps_the_link_working() {
    let (rig, mut c) = adopted().await;
    let want = rig.counts_text();
    within_2s("@moa_status shows the counts", || {
        rig.option("@moa_status") == want
    });
    rig.tmux(&["set-option", "-g", "@moa_status", "junk"]);
    rig.core.store_fault(Fault::FailNextCommit);
    let r = c.call("session.archive", json!({"qs": [ids::S2_IDLE]}));
    let line = r["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(line.starts_with("moa couldn't save that ("), "{r}");
    within_2s("the reload writes @moa_status again", || {
        rig.option("@moa_status") == want
    });
    assert_eq!(ensure(&mut c)["result"]["data"], json!({"running": true}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_connect_never_starts_a_second_client() {
    let (rig, mut c) = adopted().await;
    let before = launches(&mut c);
    let old = rig.option("@moa_ctl");
    // The detach starts the backoff; the ensures land while it runs.
    rig.tmux(&["detach-client", "-s", "_moa"]);
    let mut d = rig.client();
    let id1 = d.id();
    d.write(&request(id1, "tmux.ensure", json!({})));
    let id2 = d.id();
    d.write(&request(id2, "tmux.ensure", json!({})));
    let mut answered = 0;
    while answered < 2 {
        let v = d.read().expect("an answer");
        if v.get("id") == Some(&json!(id1)) || v.get("id") == Some(&json!(id2)) {
            assert_eq!(v["result"]["data"], json!({"running": true}), "{v}");
            answered += 1;
        }
    }
    within_2s("one new control client is linked", || {
        let ctl = rig.option("@moa_ctl");
        !ctl.is_empty() && ctl != old && rig.control_clients() == [ctl]
    });
    let grew = launches(&mut c) - before;
    assert!((1..=BACKOFF_CONNECTS).contains(&grew), "{grew} launches");
}
