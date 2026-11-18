//! `inspect_pane` finds the agent a tab runs from its processes, in-process (C22).

use std::process::{Child, Command};
use std::time::Duration;

use moa_core::model::AgentKind;
use moa_daemon::inspect::inspect_pane;
use moa_platform::{kill_term, proc};
use moa_testkit::{FakeStep, fake_claude_named_claude, wait_for};

const ID1: &str = "00000000-0000-4000-8000-000000000001";

/// The shell is stopped when dropped, its children first, so no fake-claude is left hanging.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        for kid in proc::children(self.0.id()) {
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
    let agent = wait_for(Duration::from_secs(10), || inspect_pane(pid, &[]))
        .expect("claude was never found under the shell");
    assert_eq!(agent.kind, AgentKind::Claude);
    assert_eq!(agent.conversation.as_deref(), Some(ID1));
    assert_eq!(agent.moa_session.as_deref(), Some("x"));
    assert!(agent.started_at.is_some());

    // The start command alone names the conversation; a pane that runs nothing is no agent.
    let start = ["claude".to_string(), "--resume".into(), "S".into()];
    assert_eq!(
        inspect_pane(pid, &start).unwrap().conversation.as_deref(),
        Some("S")
    );
    assert_eq!(inspect_pane(0, &[]), None);
    assert_eq!(inspect_pane(-1, &[]), None);
}
