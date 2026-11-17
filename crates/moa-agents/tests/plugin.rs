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
