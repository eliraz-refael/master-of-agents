//! The terminal link against a real tmux server: it finds a server without starting one,
//! adopts it, reconnects when its client is detached, and takes its hooks down on a clean
//! stop. Every server here is created by the test, as the front door would (T1), and is
//! reached only through `TempBrain::tmux` (T23).

mod common;

use std::path::PathBuf;
use std::time::Duration;

use moa_core::cockpit::{spawn_cmds, spawn_tags};
use moa_core::config::{BrainName, Config};
use moa_core::panel::View;
use moa_core::state::{Deps, PanelLaunch, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{PaneId, WindowId};
use moa_core::tmux::panes::{PANE_INFO, PANE_START, Tag, parse_panes};
use moa_core::tmux::topology::{self, Topology};
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

    /// The status line's counts once the daemon has reconciled its tabs.
    fn counts_text(&self) -> String {
        moa_core::tmux::status::counts_text(&self.brain.reconciled_state())
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

/// Waits (up to 10 s) for `@moa_status` to be `want`: the counts may wait for the
/// connection's reconcile.
fn status_shows(rig: &Rig, what: &str, want: &str) {
    let done = wait_for(Duration::from_secs(10), || {
        (rig.option("@moa_status") == want).then_some(())
    });
    assert!(
        done.is_some(),
        "{what}: @moa_status is {:?}",
        rig.option("@moa_status")
    );
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
    status_shows(&rig, "@moa_status shows the counts", &want);
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
    status_shows(&rig, "@moa_status shows the counts", &want);
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
    assert_eq!(s["panel_starts"], json!(0), "{s}");
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
    status_shows(&rig, "@moa_status shows the counts", &want);
    rig.tmux(&["set-option", "-g", "@moa_status", "junk"]);
    rig.core.store_fault(Fault::FailNextCommit);
    let r = c.call("session.archive", json!({"qs": [ids::S2_IDLE]}));
    let line = r["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(line.starts_with("moa couldn't save that ("), "{r}");
    status_shows(&rig, "the reload writes @moa_status again", &want);
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

/// R3: tmux prints a tab's start command on one line, quoted and escaped, and
/// `split_start_command` gives back exactly its arguments; `PANE_INFO` reads the tab too.
#[test]
fn start_commands_parse_as_tmux_prints_them() {
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "moa",
        "-n",
        "moa",
        "-x",
        "10",
        "-y",
        "3",
        "/bin/sh",
    ]);
    let argv = [
        "/bin/sh", "-c", "sleep 30", "x", "a b", "it's", "#c", "one\ntwo",
    ];
    let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", "=moa:"];
    args.extend(argv);
    let pane: PaneId = tmux(&args).parse().expect("a pane id");

    let start = tmux(&["list-panes", "-a", "-F", PANE_START]);
    let start: Vec<String> = start.lines().map(str::to_string).collect();
    let mine = format!("S\t{pane}\t");
    assert_eq!(
        start.iter().filter(|l| l.starts_with(&mine)).count(),
        1,
        "{start:?}"
    );
    let info = tmux(&["list-panes", "-a", "-F", PANE_INFO]);
    let info: Vec<String> = info.lines().map(str::to_string).collect();
    let panes = parse_panes(&info, &start);
    assert_eq!(panes.len(), 2, "{info:?}");
    let p = panes
        .iter()
        .find(|p| p.pane == pane)
        .expect("the new tab is listed");
    assert_eq!(p.start, argv);
    assert!(p.pid > 0 && !p.dead && !p.term && !p.adopted, "{p:?}");
    assert_eq!(p.tag, Tag::Unset);
    assert!(!p.cwd.is_empty());
}

/// The cockpit flag in `LIST_PANES` is tmux's own reading: the window named `moa` in the
/// session named `moa`, and no other.
#[test]
fn the_cockpit_flag_names_the_moa_window() {
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "moa",
        "-n",
        "moa",
        "-x",
        "10",
        "-y",
        "3",
        "/bin/sh",
    ]);
    tmux(&["new-session", "-d", "-s", "_moa", "-n", "moa", "/bin/sh"]);
    let other = tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        "=moa:",
        "/bin/sh",
    ]);
    let cockpit = tmux(&["display-message", "-p", "-t", "=moa:moa", "#{window_id}"]);
    let lines = |format: &str, list: &str| -> Vec<String> {
        tmux(&[list, "-a", "-F", format])
            .lines()
            .map(str::to_string)
            .collect()
    };
    let sessions: Vec<String> = tmux(&["list-sessions", "-F", topology::LIST_SESSIONS])
        .lines()
        .map(str::to_string)
        .collect();
    let panes = lines(topology::LIST_PANES, "list-panes");
    let t = Topology::from_lists(&sessions, &panes, &[]);
    let windows: Vec<WindowId> = t.window_ids().collect();
    assert_eq!(windows.len(), 3, "{panes:?}");
    for w in windows {
        assert_eq!(
            t.is_cockpit(w),
            w.to_string() == cockpit,
            "{w} ({other} is another tab): {panes:?}"
        );
    }
}

/// L7 on a real server: a panel starts parked in `_moa`, in a window named by its view, in
/// `-c`'s folder, with its view, brain and root; its tags land; and when its program stops
/// the pane stays, saying how to bring it back. `exe` stands in for moa: it writes what it
/// was started with, then sleeps until the test stops it.
#[test]
fn a_spawned_panel_runs_in_the_hidden_session() {
    use std::os::unix::fs::PermissionsExt;
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    // Runs a command moa would send, word for word.
    let send = |cmd: &Cmd| {
        assert_eq!(cmd.check(), Ok(()), "{}", cmd.line());
        let mut args = vec![cmd.name()];
        args.extend(cmd.argv().iter().map(String::as_str));
        tmux(&args)
    };
    let home = tb.root().join("panel-home");
    std::fs::create_dir_all(&home).unwrap();
    let said = tb.root().join("said");
    let exe = tb.root().join("moa-stand-in");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\n\
             {{ pwd -P; echo \"$MOA_BRAIN\"; echo \"$MOA_ROOT\"; echo \"$*\"; }} > {said}.tmp\n\
             mv {said}.tmp {said}\n\
             exec sleep 30\n",
            said = said.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    // `_moa` opens elsewhere: the panel's folder is `-c`'s, not the session's.
    let root = tb.root().display().to_string();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "_moa",
        "-c",
        &root,
        "-x",
        "80",
        "-y",
        "24",
        "/bin/sh",
    ]);

    let deps = Deps {
        home: home.clone(),
        panel: PanelLaunch {
            exe,
            env: vec![("MOA_ROOT".into(), "/r".into())],
        },
        ..Deps::default()
    };
    let state = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    let [spawn] = &spawn_cmds(&state, &[View::Tree])[..] else {
        panic!("one command per panel");
    };
    let pane: PaneId = send(spawn).parse().expect("a pane id");
    for tag in spawn_tags(pane, View::Tree) {
        send(&tag);
    }

    let target = pane.to_string();
    let show = |format: &str| tmux(&["display-message", "-p", "-t", &target, format]);
    assert_eq!(
        show("#{session_name}|#{window_name}|#{@moa_view}|#{@moa_label}"),
        "_moa|tree|tree|▤ sessions"
    );
    let said = wait_for(Duration::from_secs(10), || {
        std::fs::read_to_string(&said).ok()
    })
    .expect("the panel started");
    let folder = std::fs::canonicalize(&home).unwrap();
    assert_eq!(
        said.lines().collect::<Vec<_>>(),
        [
            folder.to_str().unwrap(),
            "default",
            "/r",
            "panel tree --brain default"
        ]
    );

    // The panel stops: its pane stays, and says how to bring it back.
    let pid = show("#{pane_pid}");
    let killed = std::process::Command::new("kill")
        .arg(&pid)
        .status()
        .unwrap();
    assert!(killed.success());
    let screen = wait_for(Duration::from_secs(10), || {
        let screen = tmux(&["capture-pane", "-p", "-t", &target]);
        (show("#{pane_dead}") == "1" && screen.contains("panel stopped")).then_some(screen)
    })
    .expect("the stopped panel's pane stays");
    assert!(
        screen.contains("panel stopped · Ctrl-a r brings it back"),
        "{screen}"
    );
}
