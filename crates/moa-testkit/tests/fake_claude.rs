//! fake-claude runs an injected plugin's hooks the way Claude Code does, from a script.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use moa_testkit::{FakeStep, fake_claude, write_script};
use serde_json::{Map, Value, json};

const ID1: &str = "00000000-0000-4000-8000-000000000001";
const ID2: &str = "00000000-0000-4000-8000-000000000002";

/// One hooks.json entry: the event, its optional matcher, the recorder's tag, and whether it is
/// async.
type Spec = (&'static str, Option<&'static str>, &'static str, bool);

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    /// A plugin whose hooks all run a recorder. The recorder appends `$1` and its stdin to
    /// `$CALLS`; the entry tagged `start` also answers with a peers note on stdout.
    fn new(specs: &[Spec]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let recorder = dir.path().join("recorder.sh");
        fs::write(
            &recorder,
            "#!/bin/sh\nprintf '%s ' \"$1\" >> \"$CALLS\"\ncat >> \"$CALLS\"\necho >> \"$CALLS\"\n\
             if [ \"$1\" = start ]; then echo 'peers note'; fi\n",
        )
        .unwrap();
        fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755)).unwrap();
        let mut events = Map::new();
        for (event, matcher, tag, is_async) in specs {
            let mut hook = json!({"type": "command", "command": recorder, "args": [tag]});
            if *is_async {
                hook["async"] = json!(true);
            }
            let mut entry = json!({ "hooks": [hook] });
            if let Some(m) = matcher {
                entry["matcher"] = json!(m);
            }
            events
                .entry(*event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(entry);
        }
        let plugin = dir.path().join("plugin");
        fs::create_dir_all(plugin.join("hooks")).unwrap();
        fs::write(
            plugin.join("hooks/hooks.json"),
            json!({ "hooks": events }).to_string(),
        )
        .unwrap();
        Rig { dir }
    }

    fn work(&self) -> PathBuf {
        let work = self.dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        work
    }

    fn run(&self, steps: &[FakeStep], args: &[&str]) -> Output {
        let work = self.work();
        write_script(&work, steps);
        Command::new(fake_claude())
            .args(args)
            .current_dir(&work)
            .env_remove("FAKE_CLAUDE_SCRIPT")
            .env("CLAUDE_CODE_PLUGIN_DIRS", self.dir.path().join("plugin"))
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("config"))
            .env("CALLS", self.dir.path().join("calls"))
            .output()
            .unwrap()
    }

    /// The recorded calls as (tag, payload).
    fn calls(&self) -> Vec<(String, Value)> {
        fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let (tag, json) = l.split_once(' ').unwrap();
                (tag.to_string(), serde_json::from_str(json).unwrap())
            })
            .collect()
    }
}

fn hook(event: &str, fields: Value) -> FakeStep {
    FakeStep::Hook {
        event: event.into(),
        fields: fields.as_object().unwrap().clone(),
    }
}

fn tags(calls: &[(String, Value)]) -> Vec<&str> {
    calls.iter().map(|(t, _)| t.as_str()).collect()
}

#[test]
fn runs_matching_hooks_in_order_with_the_payload() {
    let rig = Rig::new(&[
        ("SessionStart", None, "start", false),
        ("Stop", None, "one", true),
        ("Stop", None, "two", true),
    ]);
    let id = "11111111-1111-4111-8111-111111111111";
    let out = rig.run(
        &[
            hook("SessionStart", json!({"source": "startup"})),
            hook("Stop", json!({"last_assistant_message": "Lantern is lit"})),
            FakeStep::Exit,
        ],
        &["--session-id", id, "--name", "t"],
    );
    assert!(out.status.success(), "{out:?}");
    let calls = rig.calls();
    assert_eq!(tags(&calls), ["start", "one", "two"]);
    let start = &calls[0].1;
    assert_eq!(start["hook_event_name"], "SessionStart");
    assert_eq!(start["session_id"], id);
    assert_eq!(start["source"], "startup");
    assert_eq!(
        start["cwd"],
        rig.work().canonicalize().unwrap().to_str().unwrap()
    );
    let transcript = start["transcript_path"].as_str().unwrap();
    assert!(transcript.ends_with(&format!("config/projects/x/{id}.jsonl")));
    assert!(!PathBuf::from(transcript).exists());
    assert_eq!(calls[1].1["last_assistant_message"], "Lantern is lit");
    let context = fs::read_to_string(rig.work().join(".fake-claude.context")).unwrap();
    assert_eq!(context, "peers note\n");
}

/// A start that answers nothing still leaves the context file, empty: the agent got no note.
#[test]
fn a_silent_session_start_leaves_an_empty_context() {
    let rig = Rig::new(&[("SessionStart", None, "quiet", false)]);
    let out = rig.run(&[hook("SessionStart", json!({})), FakeStep::Exit], &[]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tags(&rig.calls()), ["quiet"]);
    let context = fs::read_to_string(rig.work().join(".fake-claude.context")).unwrap();
    assert_eq!(context, "");
}

#[test]
fn matchers_filter_by_tool_and_notification_type() {
    let rig = Rig::new(&[
        (
            "PreToolUse",
            Some("AskUserQuestion|ExitPlanMode"),
            "tool",
            true,
        ),
        ("Notification", Some("elicitation_dialog"), "note", true),
        ("PostToolUse", None, "all", true),
    ]);
    let out = rig.run(
        &[
            hook("PreToolUse", json!({"tool_name": "Bash"})),
            hook("PreToolUse", json!({"tool_name": "ExitPlanMode"})),
            hook("Notification", json!({"notification_type": "idle_prompt"})),
            hook(
                "Notification",
                json!({"notification_type": "elicitation_dialog"}),
            ),
            hook("PostToolUse", json!({"tool_name": "Bash"})),
            FakeStep::Exit,
        ],
        &[],
    );
    assert!(out.status.success(), "{out:?}");
    let calls = rig.calls();
    assert_eq!(tags(&calls), ["tool", "note", "all"]);
    assert_eq!(calls[0].1["tool_name"], "ExitPlanMode");
    assert_eq!(calls[1].1["notification_type"], "elicitation_dialog");
}

#[test]
fn clear_rotates_the_session_id() {
    let rig = Rig::new(&[
        ("SessionStart", None, "start", false),
        ("SessionEnd", None, "end", true),
    ]);
    let out = rig.run(
        &[
            hook("SessionStart", json!({"source": "startup"})),
            FakeStep::Clear,
            FakeStep::Exit,
        ],
        &[],
    );
    assert!(out.status.success(), "{out:?}");
    let calls = rig.calls();
    assert_eq!(tags(&calls), ["start", "end", "start", "end"]);
    let seen: Vec<_> = calls
        .iter()
        .map(|(_, p)| {
            (
                p["session_id"].as_str().unwrap(),
                p["source"].as_str().or(p["reason"].as_str()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (ID1, "startup"),
            (ID1, "clear"),
            (ID2, "clear"),
            (ID2, "prompt_input_exit")
        ]
    );
}

#[test]
fn a_prompt_argument_is_the_first_prompt() {
    let rig = Rig::new(&[("UserPromptSubmit", None, "prompt", true)]);
    let out = rig.run(
        &[FakeStep::Exit],
        &["--resume", ID2, "--name", "t", "--", "light the Lantern"],
    );
    assert!(out.status.success(), "{out:?}");
    let calls = rig.calls();
    assert_eq!(tags(&calls), ["prompt"]);
    assert_eq!(calls[0].1["prompt"], "light the Lantern");
    assert_eq!(calls[0].1["session_id"], ID2);
}
