//! `inspect_pane` finds the agent a tab runs from its processes, in-process (C22).

use std::process::{Child, Command};
use std::time::Duration;

use moa_core::config::{BrainName, Config};
use moa_core::effect::{Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::{AgentKind, Inspected, Lifecycle, Location};
use moa_core::state::{Deps, State};
use moa_core::tmux::ids::{ClientName, Generation};
use moa_daemon::inspect::inspect_pane;
use moa_platform::{kill_term, proc};
use moa_testkit::brain::TypicalBrain;
use moa_testkit::{FakeStep, fake_claude_named_claude, wait_for};

/// A brain with no folders of its own, so every folder is its.
const BRAIN_TOML: &str = r#"
[brains.default]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

const ID1: &str = "00000000-0000-4000-8000-000000000001";

/// The shell is stopped when dropped, its children first, so no fake-claude is left hanging.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        for kid in proc::children(self.0.id()).unwrap_or_default() {
            kill_term(kid);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn inspect_finds_claude_under_a_shell() {
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let claude = fake_claude_named_claude(dir.path());
    let script = dir.path().join("script.jsonl");
    std::fs::write(
        &script,
        format!("{}\n", serde_json::to_string(&FakeStep::Hang).unwrap()),
    )
    .unwrap();
    // `; :` keeps the shell from exec-ing into claude, so claude is the shell's child.
    let line = format!("'{}' --session-id {ID1}; :", claude.display());
    let sh = Guard(
        Command::new("/bin/sh")
            .args(["-c", &line])
            .current_dir(dir.path())
            .env("FAKE_CLAUDE_SCRIPT", &script)
            .env("CLAUDE_CONFIG_DIR", dir.path().join("store"))
            .env("MOA_SESSION", "x")
            .env_remove("CLAUDE_CODE_PLUGIN_DIRS")
            .spawn()
            .unwrap(),
    );
    let pid = i64::from(sh.0.id());
    let agent = wait_for(Duration::from_secs(10), || {
        inspect_pane(pid, &[]).into_agent()
    })
    .expect("claude was never found under the shell");
    assert_eq!(agent.kind, AgentKind::Claude);
    assert_eq!(agent.conversation.as_deref(), Some(ID1));
    assert_eq!(agent.moa_session.as_deref(), Some("x"));
    assert!(agent.started_at.is_some());

    // The start command alone names the conversation; a pane that runs nothing is no agent.
    let start = ["claude".to_string(), "--resume".into(), "S".into()];
    assert_eq!(
        inspect_pane(pid, &start)
            .into_agent()
            .unwrap()
            .conversation
            .as_deref(),
        Some("S")
    );
}

#[test]
fn a_tab_whose_process_doesnt_exist_is_gone() {
    for pid in [0, -1, i64::from(i32::MAX)] {
        assert_eq!(inspect_pane(pid, &[]), Inspected::Gone, "{pid}");
    }
}

#[test]
fn a_shell_running_no_claude_is_gone() {
    let sh = Guard(
        Command::new("/bin/sh")
            .args(["-c", "sleep 30; :"])
            .spawn()
            .unwrap(),
    );
    let pid = sh.0.id();
    // Once the shell's child is listed, every read answers, and none is Claude.
    wait_for(Duration::from_secs(10), || {
        proc::children(pid).ok().filter(|k| !k.is_empty())
    })
    .expect("the shell's sleep was never listed");
    assert_eq!(inspect_pane(i64::from(pid), &[]), Inspected::Gone);
}

/// A long-lived process another user owns, with no children, if this machine shows one: pid
/// 1, else one of its children. A build sandbox's pid namespace may hold only the build
/// user's; then the rule is checked with made-up reads (`inspect`'s unit tests).
fn another_users_process() -> Option<u32> {
    let found = std::iter::once(1)
        .chain(proc::children(1).unwrap_or_default())
        .find(|&p| proc::owned_by_another(p) == Some(true) && proc::children(p) == Ok(Vec::new()));
    if found.is_none() {
        // Only a Nix build sandbox's pid namespace holds none (`nix develop` sets
        // `NIX_BUILD_TOP` too, and `IN_NIX_SHELL` with it).
        assert!(
            std::env::var_os("NIX_BUILD_TOP").is_some()
                && std::env::var_os("IN_NIX_SHELL").is_none(),
            "no other user's childless process is visible: can moa read who owns one?"
        );
        eprintln!(
            "in a Nix build, no other user's childless process is visible; \
             `inspect::tests::another_users_processes_are_no_agent_of_ours` covers the rule"
        );
    }
    found
}

/// A process another user owns is no agent of ours: a tab running it, with nothing under
/// it, is proven to run none, not unread.
#[test]
fn another_users_processes_are_no_agent_of_ours() {
    let Some(pid) = another_users_process() else {
        return;
    };
    assert_eq!(
        inspect_pane(i64::from(pid), &[]),
        Inspected::Gone,
        "pid {pid}"
    );
}

/// Reconcile with the real process reads: a live session with no tab, and one untagged tab
/// whose process is another user's. That tab is proven to run no agent of ours, so it
/// doesn't hold the session back from sleep, as a tab that can't be read would (R5, R31).
#[test]
fn another_users_tab_doesnt_keep_tabless_sessions_live() {
    let Some(pid) = another_users_process() else {
        return;
    };
    let b = TypicalBrain::new();
    let mut snap = b.snapshot();
    snap.pane_bindings.clear();
    snap.spike_panes.clear();
    for s in &mut snap.sessions {
        s.lifecycle = Lifecycle::Ended;
    }
    let s = &mut snap.sessions[0];
    s.lifecycle = Lifecycle::Live;
    s.asleep_at = None;
    s.location = Location::Here;
    let d = s.id;
    let deps = Deps {
        home: b.home.clone(),
        inspect: inspect_pane,
        ..Deps::default()
    };
    let config = Config::parse(BRAIN_TOML, &b.home).unwrap();
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    handle_checked(
        &mut st,
        Input::Tmux(TmuxIn::Connected {
            conn: 1,
            generation: Generation { pid: 10, start: 1 },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        }),
        b.now(),
    );
    handle_checked(&mut st, Input::StartDrained, b.now());
    let asked: Vec<u64> = st.tmux().queries().collect();
    let [n] = asked[..] else {
        panic!("one pane query waiting, not {asked:?}");
    };
    let tab = format!("I\t%9\t{pid}\t0\t\t\t\t/");
    let fx = handle_checked(
        &mut st,
        Input::Tmux(TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec![tab]), Ok(Vec::new())],
        }),
        b.now(),
    );
    let s = st.session(d).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Ended, "{fx:?}");
    assert!(s.asleep_at.is_some(), "{fx:?}");
}
