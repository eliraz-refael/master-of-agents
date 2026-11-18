//! fake-claude runs an injected plugin's hooks the way Claude Code does, from a script.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use moa_agents::claude::project_slot;
use moa_testkit::{FakeStep, fake_claude, wait_for, write_script};
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

    fn command(&self, steps: &[FakeStep], args: &[&str]) -> Command {
        let work = self.work();
        write_script(&work, steps);
        let mut cmd = Command::new(fake_claude());
        cmd.args(args)
            .current_dir(&work)
            .env_remove("FAKE_CLAUDE_SCRIPT")
            .env("CLAUDE_CODE_PLUGIN_DIRS", self.dir.path().join("plugin"))
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("config"))
            .env("CALLS", self.dir.path().join("calls"));
        cmd
    }

    fn run(&self, steps: &[FakeStep], args: &[&str]) -> Output {
        self.command(steps, args).output().unwrap()
    }

    /// Where fake-claude keeps the transcript of `id`: `<store>/projects/<slot>/<id>.jsonl`.
    fn transcript(&self, id: &str) -> PathBuf {
        let slot = project_slot(&self.work().canonicalize().unwrap());
        self.dir
            .path()
            .join("config/projects")
            .join(slot)
            .join(format!("{id}.jsonl"))
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
    // R34: the transcript is in the store, under the cwd's project slot, from the start.
    assert_eq!(
        start["transcript_path"].as_str().unwrap(),
        rig.transcript(id).to_str().unwrap()
    );
    assert!(rig.transcript(id).is_file());
    assert_eq!(calls[1].1["last_assistant_message"], "Lantern is lit");
    let context = fs::read_to_string(rig.work().join(".fake-claude.context")).unwrap();
    assert_eq!(context, "peers note\n");
}

/// A child that is killed when dropped: a failed assertion doesn't leave fake-claude polling
/// for its gate forever.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `Wait` holds the script until its file exists; the hook after it runs only then.
#[test]
fn wait_holds_the_script_until_its_file_exists() {
    let rig = Rig::new(&[
        ("UserPromptSubmit", None, "before", true),
        ("Stop", None, "after", true),
    ]);
    let steps = [
        hook("UserPromptSubmit", json!({})),
        FakeStep::Wait {
            file: "gate".into(),
        },
        hook("Stop", json!({})),
        FakeStep::Exit,
    ];
    let mut child = KillOnDrop(
        rig.command(&steps, &[])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // The recorder writes a call in pieces; it is whole once its line ends.
    let calls = rig.dir.path().join("calls");
    let before = wait_for(Duration::from_secs(10), || {
        fs::read_to_string(&calls)
            .ok()
            .filter(|t| t.ends_with('\n'))
    });
    assert!(before.is_some(), "the first hook never ran");
    assert_eq!(tags(&rig.calls()), ["before"]);
    // Held: a while later nothing more has run and it hasn't exited.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(tags(&rig.calls()), ["before"]);
    assert!(child.0.try_wait().unwrap().is_none());

    fs::write(rig.work().join("gate"), "").unwrap();
    assert!(child.0.wait().unwrap().success());
    assert_eq!(tags(&rig.calls()), ["before", "after"]);
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

/// R34: a real Claude has a transcript by its first hook, so fake-claude writes one line
/// where Claude would, and names that file in its hooks.
#[test]
fn a_session_start_writes_a_synthetic_transcript() {
    let rig = Rig::new(&[("SessionStart", None, "start", false)]);
    let out = rig.run(
        &[
            hook("SessionStart", json!({"source": "startup"})),
            FakeStep::Clear,
            FakeStep::Exit,
        ],
        &["--session-id", ID1],
    );
    assert!(out.status.success(), "{out:?}");
    let calls = rig.calls();
    assert_eq!(tags(&calls), ["start", "start"]);
    for (id, (_, payload)) in [ID1, ID2].into_iter().zip(&calls) {
        assert_eq!(
            payload["transcript_path"].as_str().unwrap(),
            rig.transcript(id).to_str().unwrap()
        );
        assert_eq!(
            fs::read_to_string(rig.transcript(id)).unwrap(),
            "{\"type\":\"summary\",\"summary\":\"fake\"}\n"
        );
    }

    // `--resume` of a conversation with no transcript makes one, before any hook.
    let id = "00000000-0000-4000-8000-000000000003";
    let out = rig.run(&[FakeStep::Exit], &["--resume", id]);
    assert!(out.status.success(), "{out:?}");
    assert!(rig.transcript(id).is_file());

    // A transcript that is there already is left as it is.
    fs::write(rig.transcript(id), "kept\n").unwrap();
    let out = rig.run(
        &[
            hook("SessionStart", json!({"source": "resume"})),
            FakeStep::Exit,
        ],
        &["--resume", id],
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read_to_string(rig.transcript(id)).unwrap(), "kept\n");
}

/// With no `CLAUDE_CONFIG_DIR` the store is `$HOME/.claude`. A store outside the temp dir
/// (a developer's real `HOME`, inherited) is never written to. Both stores below are under
/// `/`, where a write would fail fake-claude, so a broken guard can't pass unseen.
#[test]
fn no_transcript_is_written_outside_the_temp_dir() {
    let rig = Rig::new(&[("SessionStart", None, "start", false)]);
    let home = PathBuf::from("/nonexistent-moa-test-home");
    let out = rig
        .command(
            &[hook("SessionStart", json!({})), FakeStep::Exit],
            &["--resume", ID1],
        )
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let slot = project_slot(&rig.work().canonicalize().unwrap());
    let path = home
        .join(".claude/projects")
        .join(slot)
        .join(format!("{ID1}.jsonl"));
    let calls = rig.calls();
    assert_eq!(
        calls[0].1["transcript_path"].as_str().unwrap(),
        path.to_str().unwrap()
    );
    assert!(!home.exists());

    // A store that leaves `/tmp` through `..` isn't inside it.
    let escape = PathBuf::from("/tmp/../nonexistent-moa-test-store");
    let out = rig
        .command(&[hook("SessionStart", json!({})), FakeStep::Exit], &[])
        .env("CLAUDE_CONFIG_DIR", &escape)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(!escape.exists());
}
