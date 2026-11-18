//! `inspect_pane` finds the agent a tab runs from its processes, in-process (C22).

use std::process::{Child, Command};
use std::time::Duration;

use moa_core::model::{AgentKind, Inspected};
use moa_daemon::inspect::inspect_pane;
use moa_platform::{kill_term, proc};
use moa_testkit::{FakeStep, fake_claude_named_claude, wait_for};

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

#[test]
fn a_process_moa_cant_read_is_unknown_not_gone() {
    // Pid 1 is root's: its arguments and environment are closed to anyone else.
    if moa_platform::current_uid() == 0 {
        return;
    }
    assert_ne!(inspect_pane(1, &[]), Inspected::Gone);
}
