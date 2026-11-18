//! What counts as Claude in a tab (R9), and how the start command and the process combine.

use std::path::Path;

use moa_agents::claude::{merge, pane_agent, project_slot};
use moa_core::model::{AgentKind, PaneAgent};

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn agent(line: &str) -> Option<PaneAgent> {
    pane_agent(&words(line), &[])
}

fn conversation(line: &str) -> Option<String> {
    agent(line).expect(line).conversation
}

#[test]
fn claude_is_recognised() {
    assert_eq!(conversation("claude --resume X").as_deref(), Some("X"));
    assert_eq!(
        conversation("/opt/bin/claude --session-id=Y").as_deref(),
        Some("Y")
    );
    assert_eq!(
        conversation("node /x/claude-code/cli.js -r Z").as_deref(),
        Some("Z")
    );
    assert_eq!(conversation("claude --resume=W").as_deref(), Some("W"));
    assert_eq!(conversation("claude").as_deref(), None);
    // A bare `--resume` (the picker) doesn't swallow the flag after it.
    assert_eq!(
        conversation("claude --resume --session-id V").as_deref(),
        Some("V")
    );

    let a = agent("env CLAUDE_CONFIG_DIR=/s claude").unwrap();
    assert_eq!(a.kind, AgentKind::Claude);
    assert_eq!(a.store_dir.as_deref(), Some("/s"));
    assert_eq!(a.conversation, None);

    let a = agent("/usr/bin/env -u CLAUDE_CONFIG_DIR /opt/claude --session-id Q").unwrap();
    assert_eq!(a.conversation.as_deref(), Some("Q"));
    assert_eq!(a.store_dir, None);

    // `-u` unsets a value given before it, and `-i` drops them all.
    let a = agent("CLAUDE_CONFIG_DIR=/s env -u CLAUDE_CONFIG_DIR claude").unwrap();
    assert_eq!(a.store_dir, None);
    let a = pane_agent(
        &words("env -i claude"),
        &env(&[("CLAUDE_CONFIG_DIR", "/s")]),
    )
    .unwrap();
    assert_eq!(a.store_dir, None);

    for other in ["bash", "vim", "codex resume X", "node /x/server.js", ""] {
        assert_eq!(agent(other), None, "{other:?}");
    }

    let a = pane_agent(
        &words("claude"),
        &env(&[
            ("MOA_SESSION", "m"),
            ("MOA_BRAIN", "default"),
            ("MOA_TITLE", "Lantern"),
            ("MOA_GROUP", "work"),
            ("CLAUDE_CONFIG_DIR", "/s"),
        ]),
    )
    .unwrap();
    assert_eq!(a.moa_session.as_deref(), Some("m"));
    assert_eq!(a.moa_brain.as_deref(), Some("default"));
    assert_eq!(a.title.as_deref(), Some("Lantern"));
    assert_eq!(a.group.as_deref(), Some("work"));
    assert_eq!(a.store_dir.as_deref(), Some("/s"));
    assert_eq!(a.started_at, None);
}

/// Node's `process.title` can overwrite argv, so a running Claude may show plain `claude`;
/// the start command tmux kept still names the conversation.
#[test]
fn merge_prefers_the_start_command_for_a_retitled_agent() {
    let process = pane_agent(&words("claude"), &env(&[("MOA_SESSION", "m")]));
    let start = pane_agent(&words("/opt/claude --resume X"), &[]);
    let both = merge(start, process).unwrap();
    assert_eq!(both.conversation.as_deref(), Some("X"));
    assert_eq!(both.moa_session.as_deref(), Some("m"));
    assert_eq!(both.kind, AgentKind::Claude);

    // The start command wins over the process argv, and either view alone is enough.
    let process = pane_agent(&words("claude --resume P"), &[]);
    let start = pane_agent(&words("claude --resume S"), &[]);
    assert_eq!(
        merge(start.clone(), process.clone())
            .unwrap()
            .conversation
            .as_deref(),
        Some("S")
    );
    assert_eq!(merge(None, process.clone()), process);
    assert_eq!(merge(start.clone(), None), start);
    assert_eq!(merge(None, None), None);
}

#[test]
fn project_slot_replaces_non_alphanumerics() {
    assert_eq!(
        project_slot(Path::new("/Users/x/private/a.b")),
        "-Users-x-private-a-b"
    );
}

/// The core keeps a copy for resume (it can't depend on this crate): the two must agree.
#[test]
fn the_cores_project_slot_is_the_same() {
    for cwd in [
        "/Users/x/private/a.b",
        "/tmp/moa x/work/a#(b)",
        "/h/ünï/c_d-e",
        "",
        "relative/path",
    ] {
        let p = Path::new(cwd);
        assert_eq!(moa_core::model::project_slot(p), project_slot(p), "{cwd}");
    }
}
