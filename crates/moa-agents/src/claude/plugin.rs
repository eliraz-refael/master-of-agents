//! The Claude Code plugin moa starts sessions with: it subscribes the hooks that tell the
//! daemon what a session is doing, and carries the `moa-peers` skill. The daemon writes it,
//! so edits are replaced.

use std::path::Path;

use serde_json::{Value, json};

/// The events moa subscribes to, with the tool or notification matcher some of them need.
const EVENTS: [(&str, Option<&str>); 12] = [
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("StopFailure", None),
    ("PermissionRequest", None),
    ("PreToolUse", Some("AskUserQuestion|ExitPlanMode")),
    ("PostToolUse", None),
    ("PostToolUseFailure", None),
    ("PermissionDenied", None),
    ("Notification", Some("elicitation_dialog")),
    ("SessionEnd", None),
    ("CwdChanged", None),
];

/// The `moa-peers` skill: how to ask the other agents and moa, and how to answer (A12).
const PEERS_SKILL: &str = include_str!("skills/moa-peers.md");

/// The plugin's files as (relative path, text): its manifest, its hooks and its skill. Every
/// hook runs `<exe> hook claude`; only `SessionStart` waits (its answer is the session's
/// identity, and its peers note), the rest are async so they never slow Claude Code down.
pub fn session_plugin(exe: &Path) -> Vec<(&'static str, String)> {
    let exe = exe.to_string_lossy();
    let waits =
        json!({"type": "command", "command": exe, "args": ["hook", "claude"], "timeout": 5});
    let async_ =
        json!({"type": "command", "command": exe, "args": ["hook", "claude"], "async": true});
    let mut hooks = serde_json::Map::new();
    for (event, matcher) in EVENTS {
        let hook = if event == "SessionStart" {
            &waits
        } else {
            &async_
        };
        let mut entry = serde_json::Map::new();
        if let Some(m) = matcher {
            entry.insert("matcher".into(), json!(m));
        }
        entry.insert("hooks".into(), json!([hook]));
        hooks.insert(event.into(), json!([Value::Object(entry)]));
    }
    let manifest = json!({
        "name": "moa",
        "version": "1",
        "description": "moa tracks this session (generated; edits are replaced)",
    });
    let render = |v: &Value| serde_json::to_string_pretty(v).expect("json") + "\n";
    vec![
        (".claude-plugin/plugin.json", render(&manifest)),
        ("hooks/hooks.json", render(&json!({ "hooks": hooks }))),
        ("skills/moa-peers/SKILL.md", PEERS_SKILL.to_string()),
    ]
}
