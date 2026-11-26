//! The injected session plugin.

use std::path::Path;

use moa_agents::claude::session_plugin;
use serde_json::Value;

const C10: [&str; 12] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionDenied",
    "Notification",
    "SessionEnd",
    "CwdChanged",
];

fn hooks() -> Value {
    let files = session_plugin(Path::new("/opt/moa/bin/moa"));
    let (_, text) = files
        .iter()
        .find(|(p, _)| *p == "hooks/hooks.json")
        .unwrap();
    serde_json::from_str(text).unwrap()
}

#[test]
fn the_plugin_is_stable() {
    let files = session_plugin(Path::new("/opt/moa/bin/moa"));
    let all: Vec<String> = files.iter().map(|(p, t)| format!("== {p}\n{t}")).collect();
    insta::assert_snapshot!(all.join("\n"));
}

#[test]
fn every_c10_event_is_subscribed() {
    let h = hooks();
    let mut keys: Vec<&str> = h["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = C10.to_vec();
    want.sort_unstable();
    assert_eq!(keys, want);
}

#[test]
fn only_session_start_waits() {
    let h = hooks();
    for (event, entries) in h["hooks"].as_object().unwrap() {
        for entry in entries.as_array().unwrap() {
            for hook in entry["hooks"].as_array().unwrap() {
                if event == "SessionStart" {
                    assert!(hook.get("async").is_none());
                } else {
                    assert_eq!(hook["async"], true, "{event}");
                }
            }
        }
    }
}

/// The `moa-peers` skill's file, as the plugin writes it.
fn skill() -> String {
    let files = session_plugin(Path::new("/opt/moa/bin/moa"));
    let (_, text) = files
        .iter()
        .find(|(p, _)| *p == "skills/moa-peers/SKILL.md")
        .expect("the skill is in the plugin");
    text.clone()
}

#[test]
fn the_plugin_ships_the_skill() {
    let text = skill();
    let rest = text.strip_prefix("---\n").expect("frontmatter first");
    let (front, body) = rest.split_once("\n---\n").expect("frontmatter closed");
    let lines: Vec<&str> = front.lines().collect();
    assert_eq!(lines.len(), 2, "{front}");
    assert_eq!(lines[0], "name: moa-peers");
    let description = lines[1]
        .strip_prefix("description: ")
        .expect("a description");
    assert!(!description.trim().is_empty());
    assert!(!body.trim().is_empty());
}

/// The skill says what the SessionStart note says, word for word, and more.
#[test]
fn the_skill_repeats_the_note() {
    assert!(skill().contains(moa_core::peers::PEERS_NOTE));
}
