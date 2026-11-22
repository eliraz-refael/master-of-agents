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
use moa_core::tmux::cmd::literal_format;
use moa_core::tmux::ids::{PaneId, WindowId};
use moa_core::tmux::panes::{PANE_INFO, PANE_START, Tag, parse_panes};
use moa_core::tmux::topology::{self, Topology};
use moa_daemon::{CoreHandle, Ready, run};
use moa_platform::Paths;
use moa_store::actor::Fault;
use moa_testkit::brain::TypicalBrain;
use moa_testkit::{TempBrain, ids, wait_for};
use moa_tmux::client::{CONNECT_WAIT, ConnectOptions, ControlClient};
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

/// The refresh's queries on a real server, as the link sends them, and the topology they
/// build, with the raw lists for a failure message.
fn listed_topology(tmux: &impl Fn(&[&str]) -> String) -> (Topology, Vec<Vec<String>>) {
    let lists: Vec<Vec<String>> = topology::refresh_cmds()
        .iter()
        .map(|query| {
            let args: Vec<&str> = std::iter::once(query.name())
                .chain(query.argv().iter().map(String::as_str))
                .collect();
            tmux(&args).lines().map(str::to_string).collect()
        })
        .collect();
    let [sessions, panes, clients, terms] = &lists[..] else {
        panic!("four lists: {lists:?}");
    };
    let mut t = Topology::from_lists(sessions, panes, clients);
    t.terms_listed(terms);
    (t, lists)
}

/// The refresh's queries on a real server. Every tab is listed with its process, the
/// program in front and that program's folder, word for word (a space and a `#` in it);
/// only the tab tagged `@moa_term` is a terminal, with its label read as moa set it, a run
/// of `#`s halved and not collapsed (S6b). A shell at its prompt lists as its process's own
/// name (`process_name`, what a terminal's record keeps as its shell), whatever
/// `default-shell` is called: macOS runs `/bin/sh` as `bash`. A program typed into it
/// lists as something else; its name isn't checked, as a nix `sleep` lists as
/// `coreutils`.
#[test]
fn the_pane_query_lists_programs_and_folders() {
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    let folder = tb.root().join("a b").join("#x");
    std::fs::create_dir_all(&folder).unwrap();
    // tmux lists a folder as the system resolves it (`/tmp` is `/private/tmp` on macOS).
    let folder = std::fs::canonicalize(&folder)
        .unwrap()
        .display()
        .to_string();
    tmux(&["new-session", "-d", "-s", "moa", "/bin/sh"]);
    // A tab running `default-shell`, as a terminal moa opens does.
    let new_shell = |cwd: &str| -> PaneId {
        let id = tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-c",
            &literal_format(cwd),
            "-t",
            "=moa:",
        ]);
        id.parse().expect("a pane id")
    };
    let shell = new_shell(&folder);
    let target = shell.to_string();
    tmux(&["set-option", "-p", "-t", &target, "@moa_term", "1"]);
    let label = literal_format("⌨ kite ##x #[fg=red]");
    tmux(&["set-option", "-p", "-t", &target, "@moa_label", &label]);
    let busy = new_shell(&tb.root().display().to_string());
    let busy_target = busy.to_string();
    tmux(&["send-keys", "-t", &busy_target, "sleep 100", "Enter"]);

    // Each of the refresh's queries, as the link sends them.
    let listed = || listed_topology(&tmux);
    // Whether `p` lists its own process's name: its shell is in front.
    let at_its_prompt = |t: &Topology, p: PaneId| {
        let name = t.pid_of(&p).and_then(moa_platform::proc::name);
        name.is_some() && t.command_of(&p) == name.as_deref()
    };

    let seen = wait_for(Duration::from_secs(10), || {
        let (t, lists) = listed();
        let busy_in_front = t.command_of(&busy).is_some() && !at_its_prompt(&t, busy);
        (at_its_prompt(&t, shell) && busy_in_front).then_some((t, lists))
    });
    let Some((t, lists)) = seen else {
        panic!(
            "the shell never listed as itself, or the sleep as other: {:?}",
            listed().1
        );
    };
    assert!(t.panes_listed());
    assert_eq!(t.cwd_of(&shell), Some(folder.as_str()), "{lists:?}");
    assert_eq!(
        t.terms().collect::<Vec<_>>(),
        [(shell, "kite ##x #[fg=red]")],
        "{lists:?}"
    );

    // Stopped, the program leaves its shell in front again.
    tmux(&["send-keys", "-t", &busy_target, "C-c"]);
    let back = wait_for(Duration::from_secs(10), || {
        at_its_prompt(&listed().0, busy).then_some(())
    });
    assert!(
        back.is_some(),
        "the shell never came back: {:?}",
        listed().1
    );
}

/// A program named with 16 bytes or more lists as tmux cuts its name (15 bytes on macOS),
/// and `process_name` cuts it the same way: the two are equal, which is what a terminal's
/// stored shell needs to be compared with its listed program. The program is a copy of
/// `sleep` under a long name (a symlink would show its target's name), signed again on macOS.
#[test]
fn a_long_program_name_lists_as_process_name_reads_it() {
    use std::os::unix::fs::PermissionsExt;
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    // `/bin/sleep` is a plain binary on macOS; elsewhere `sleep` may be a multicall
    // `coreutils`, which picks its tool from argv[0] and refuses an unknown name, so a
    // renamed copy is told the tool with `--coreutils-prog=sleep`.
    let sleep = std::fs::canonicalize(if cfg!(target_os = "macos") {
        PathBuf::from("/bin/sleep")
    } else {
        which_sleep()
    })
    .unwrap();
    let multicall = sleep.file_name().is_some_and(|n| n == "coreutils");
    let dir = tb.root().join("long");
    std::fs::create_dir_all(&dir).unwrap();
    let long = dir.join("a-long-program-name-for-sleep");
    std::fs::copy(&sleep, &long).unwrap();
    std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o755)).unwrap();
    if cfg!(target_os = "macos") {
        // A copy of a system binary is killed on launch until it is signed again.
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["-f", "-s", "-"])
            .arg(&long)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
    }
    let mut cmd = format!("exec '{}'", long.display());
    if multicall {
        cmd.push_str(" --coreutils-prog=sleep");
    }
    cmd.push_str(" 100");
    tmux(&["new-session", "-d", "-s", "moa", "/bin/sh"]);
    let pane: PaneId = tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "=moa:",
        &cmd,
    ])
    .parse()
    .expect("a pane id");
    let listed = || listed_topology(&tmux).0;
    // The sh the program is exec'd from lists first; wait for the program itself.
    let seen = wait_for(Duration::from_secs(10), || {
        let t = listed();
        let name = t.pid_of(&pane).and_then(moa_platform::proc::name)?;
        name.starts_with("a-long-program")
            .then(|| (t.command_of(&pane).map(str::to_string), name))
    });
    let Some((command, name)) = seen else {
        panic!(
            "the long-named program never listed: {:?}",
            listed().command_of(&pane)
        );
    };
    assert_eq!(command.as_deref(), Some(name.as_str()));
    // macOS cuts a name to 15 bytes; Linux's argv0 isn't cut.
    if cfg!(target_os = "macos") {
        assert_eq!(name, "a-long-program-");
    }
}

fn which_sleep() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("sleep"))
        .find(|p| p.exists())
        .expect("a sleep on PATH")
}

/// L7 on a real server: a panel starts parked in `_moa`, in a window named by its view, in
/// `-c`'s folder, with its view, brain and root; its tags land; and when its program stops
/// the pane stays, saying how to bring it back. `exe` stands in for moa: it writes what it
/// was started with, then sleeps until the test stops it. The commands go as the daemon's
/// link sends them, each a quoted line over a control client, and the program and the home
/// are in folders whose names have a space, as a home on macOS can.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_panel_runs_in_the_hidden_session() {
    use std::os::unix::fs::PermissionsExt;
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    let home = tb.root().join("panel home");
    std::fs::create_dir_all(&home).unwrap();
    let said = tb.root().join("said");
    let bin = tb.root().join("moa bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("moa");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\n\
             {{ pwd -P; echo \"$MOA_BRAIN\"; echo \"$MOA_ROOT\"; echo \"$MOA_NO_AUTOSTART\"; echo \"$*\"; }} > '{said}.tmp'\n\
             mv '{said}.tmp' '{said}'\n\
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
    let socket = tb.paths().tmux_socket(B).unwrap();
    let (ctl, _events, _) = ControlClient::connect(&ConnectOptions {
        tmux: moa_testkit::tmux_path().unwrap_or_else(|e| panic!("{e}")),
        socket: socket.args().to_vec(),
        env: tb.env(),
        wait: CONNECT_WAIT,
    })
    .await
    .expect("a control client");
    // The client checks each command, then writes its line (`Cmd::line`).
    let pane: PaneId = match &ctl.run(spawn).await.expect("the panel's window")[..] {
        [line] => line.parse().expect("a pane id"),
        other => panic!("not one pane: {other:?}"),
    };
    for tag in spawn_tags(pane, View::Tree) {
        ctl.run(&tag).await.expect("a tag");
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
            "1",
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
    wait_for(Duration::from_secs(10), || {
        (show("#{pane_dead}") == "1").then_some(())
    })
    .expect("the panel's pane died");
    // tmux 3.4 and 3.5 built with utempter can lose the panel's exit (moa-tmux's
    // `every_hook_reaches_the_mailbox` says how): its pane is dead, but the stopped line
    // waits until tmux reaps a child. moa nudges it so (`run-shell true`, L12); with no moa
    // here, the test does.
    tmux(&["run-shell", "true"]);
    let screen = wait_for(Duration::from_secs(10), || {
        let screen = tmux(&["capture-pane", "-p", "-t", &target]);
        screen.contains("panel stopped").then_some(screen)
    })
    .expect("the stopped panel's pane stays");
    assert!(
        screen.contains("panel stopped · Ctrl-a r brings it back"),
        "{screen}"
    );
    ctl.close().await;
}

/// One pane of a window as tmux lays it out: its `@moa_view`, left, top, width and height.
type Cell = (String, u32, u32, u32, u32);

/// L6 on a real server, with the cockpit window gone (session `moa` is there, but no window in
/// it is named `moa`). The keeper's parked window was made in `_moa` at 10x3, before a user's
/// terminal attached, and tmux resizes a window only once someone looks at it: the chain
/// sizes it to the terminal first, so its joins and sizes are laid out at 120x29. Then tmux
/// sizes it again: a terminal resize reaches the cockpit. With no terminal on session `moa`
/// (a `moa layout apply` from a script), the build still goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_cockpit_is_made_again_at_its_terminals_size() {
    use moa_core::cockpit::{Scene, plan};
    use moa_core::layout::{PRESET_KEYS, leaves, presets};
    use moa_testkit::Terminal;
    let tb = TempBrain::new(None);
    let tmux = |args: &[&str]| {
        tb.tmux(B, args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
    };
    let root = tb.root().display().to_string();
    tmux(&[
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
        "shell",
        "-c",
        &root,
        "/bin/sh",
    ]);
    // Parked before anyone attached: each window is `_moa`'s 10x3.
    for view in ["tree", "console", "plan", "status", "empty-slot"] {
        let pane = tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "=_moa:",
            "-n",
            view,
            "sleep",
            "300",
        ]);
        tmux(&["set-option", "-p", "-t", pane.trim(), "@moa_view", view]);
    }
    let socket = tb.paths().tmux_socket(B).unwrap();
    let program = moa_testkit::tmux_path().unwrap_or_else(|e| panic!("{e}"));
    let mut attach: Vec<std::ffi::OsString> = vec!["-u".into()];
    attach.extend(socket.args().iter().cloned());
    attach.extend(["attach".into(), "-t".into(), "=moa".into()]);
    let users = || {
        tmux(&["list-clients", "-F", "#{client_control_mode}"])
            .lines()
            .filter(|l| *l == "0")
            .count()
    };
    let term = Terminal::spawn(&program, &attach, &tb.env(), 120, 30).unwrap();
    wait_for(Duration::from_secs(10), || (users() == 1).then_some(()))
        .expect("the user's terminal is attached");
    let (ctl, _events, _) = ControlClient::connect(&ConnectOptions {
        tmux: program.clone(),
        socket: socket.args().to_vec(),
        env: tb.env(),
        wait: CONNECT_WAIT,
    })
    .await
    .expect("a control client");

    let lines = |args: &[&str]| -> Vec<String> { tmux(args).lines().map(str::to_string).collect() };
    let scene = || {
        let t = Topology::from_lists(
            &lines(&["list-sessions", "-F", topology::LIST_SESSIONS]),
            &lines(&["list-panes", "-a", "-F", topology::LIST_PANES]),
            &lines(&["list-clients", "-F", topology::LIST_CLIENTS]),
        );
        Scene::read(&t, |_| None)
    };
    let size = |pane: &str| {
        tmux(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{window_width}x#{window_height}",
        ])
    };
    let cells = |pane: &str| -> Vec<Cell> {
        lines(&[
            "list-panes",
            "-t",
            pane,
            "-F",
            "#{@moa_view} #{pane_left} #{pane_top} #{pane_width} #{pane_height}",
        ])
        .iter()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let n = |i: usize| f[i].parse::<u32>().unwrap();
            (f[0].to_string(), n(1), n(2), n(3), n(4))
        })
        .collect()
    };
    // Builds `name` with the cockpit window gone, from parked panels: the keeper's pane.
    let build = async |name: &str| -> (String, Result<Vec<String>, String>) {
        let before = scene();
        assert!(
            before.moa_session && before.cockpit.is_none(),
            "{name}: {before:?}"
        );
        let layout = presets().node(name).unwrap();
        let build = plan(&before, &layout, |_| String::new())
            .build
            .unwrap_or_else(|| panic!("{name}: a build from parked panels"));
        let keeper = build.leaves[0].1.to_string();
        let answer = ctl.run(&build.chain).await.map_err(|e| e.to_string());
        let after = scene();
        let cockpit = after
            .cockpit
            .unwrap_or_else(|| panic!("{name}: no cockpit"));
        let held = after.panes.iter().filter(|at| at.window == cockpit).count();
        assert_eq!(held, leaves(&layout).len(), "{name}: {after:?}");
        (keeper, answer)
    };
    // Every pane of the cockpit parked again, so the next build finds no cockpit.
    let park = |keeper: &str| {
        for (pane, _) in lines(&["list-panes", "-t", keeper, "-F", "#{pane_id} x"])
            .iter()
            .filter_map(|l| l.split_once(' '))
        {
            tmux(&["break-pane", "-d", "-s", pane, "-t", "=_moa:"]);
        }
    };
    let near = |got: u32, want: u32| got.abs_diff(want) <= 1;

    let mut keeper = String::new();
    for name in PRESET_KEYS {
        if !keeper.is_empty() {
            park(&keeper);
        }
        let (k, answer) = build(name).await;
        keeper = k;
        assert_eq!(answer, Ok(Vec::new()), "{name}");
        assert_eq!(size(&keeper), "120x29", "{name}");
        let widths: Vec<(String, u32)> = cells(&keeper)
            .into_iter()
            .map(|(v, _, _, w, _)| (v, w))
            .collect();
        // 26% and 30% of 120 columns, the console the rest, borders between.
        let want: &[(&str, u32)] = match name {
            "cockpit" => &[("tree", 31), ("console", 51), ("plan", 36)],
            "triage" => &[("status", 36), ("console", 49), ("plan", 33)],
            _ => &[],
        };
        if !want.is_empty() {
            assert_eq!(widths.len(), want.len(), "{name}: {widths:?}");
            for ((view, got), (v, w)) in widths.iter().zip(want) {
                assert!(view == v && near(*got, *w), "{name}: {widths:?}");
            }
        }
    }

    // The cockpit follows the terminal again: no size of its own is left set.
    assert_eq!(
        tmux(&["show-options", "-wqv", "-t", &keeper, "window-size"]),
        ""
    );
    tmux(&["select-window", "-t", &keeper]);
    term.resize(100, 30).unwrap();
    wait_for(Duration::from_secs(10), || {
        (size(&keeper) == "100x29").then_some(())
    })
    .unwrap_or_else(|| panic!("the cockpit didn't follow the terminal: {}", size(&keeper)));

    // With no terminal on session `moa`, `resize-window -A` changes nothing and the build
    // still goes through.
    tmux(&["select-window", "-t", "=moa:shell"]);
    park(&keeper);
    drop(term);
    wait_for(Duration::from_secs(10), || (users() == 0).then_some(()))
        .expect("the user's terminal is gone");
    let (keeper, answer) = build("cockpit").await;
    assert_eq!(answer, Ok(Vec::new()));
    assert_eq!(cells(&keeper).len(), 3);
    ctl.close().await;
}
