//! Claude's hook payloads, normalized. Fixtures are synthetic: made-up names, never a real
//! payload.

use std::io::Write;
use std::path::Path;

use moa_agents::claude::{last_assistant_text, normalize};
use moa_core::UtcMs;
use moa_core::hook::{HOOK_LINE_V, HookEnv, HookEnvelope, HookKind, HookLine, StartSource};
use moa_core::model::AgentKind;
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn line(payload: Value) -> HookLine {
    HookLine {
        v: HOOK_LINE_V,
        agent: AgentKind::Claude,
        at_ns: 1_700_000_000_123_456_789,
        pid: 4242,
        seq: 7,
        env: HookEnv::default(),
        payload,
    }
}

fn no_tail(_: &Path) -> Option<String> {
    None
}

fn kind_of(payload: Value) -> Option<HookKind> {
    normalize(&line(payload), &no_tail).map(|e| e.kind)
}

#[test]
fn each_subscribed_event_normalizes() {
    let s = |t: &str| t.to_string();
    let cases = [
        (
            "session_start",
            Some(HookKind::SessionStarted {
                source: StartSource::Clear,
            }),
        ),
        (
            "user_prompt_submit",
            Some(HookKind::TurnStarted {
                prompt: s("tidy the Lantern notes"),
            }),
        ),
        (
            "stop",
            Some(HookKind::TurnEnded {
                last_message: s("All tidy."),
            }),
        ),
        (
            "stop_failure",
            Some(HookKind::TurnFailed {
                error: s("rate limit"),
            }),
        ),
        (
            "permission_request",
            Some(HookKind::NeedsPermission { tool: s("Bash") }),
        ),
        (
            "pre_tool_use_ask",
            Some(HookKind::NeedsAnswer {
                what: s("Which lamp?"),
            }),
        ),
        (
            "pre_tool_use_plan",
            Some(HookKind::NeedsAnswer {
                what: s("plan approval"),
            }),
        ),
        ("pre_tool_use_other", None),
        ("post_tool_use", Some(HookKind::NeedsCleared)),
        (
            "notification_elicitation",
            Some(HookKind::NeedsAnswer {
                what: s("Pick a wick"),
            }),
        ),
        (
            "session_end",
            Some(HookKind::SessionEnded { reason: s("clear") }),
        ),
        (
            "cwd_changed",
            Some(HookKind::CwdChanged {
                cwd: s("/work/Lantern/sub"),
            }),
        ),
    ];
    for (name, want) in cases {
        assert_eq!(kind_of(fixture(name)), want, "{name}");
    }
}

#[test]
fn the_other_events_and_the_fallbacks() {
    let ev = |name: &str| json!({"hook_event_name": name, "session_id": "conv-1"});
    for name in ["PostToolUseFailure", "PermissionDenied"] {
        assert_eq!(kind_of(ev(name)), Some(HookKind::NeedsCleared), "{name}");
    }
    assert_eq!(
        kind_of(ev("StopFailure")),
        Some(HookKind::TurnFailed {
            error: "failed".into()
        })
    );
    assert_eq!(
        kind_of(ev("PermissionRequest")),
        Some(HookKind::NeedsPermission {
            tool: "a tool".into()
        })
    );
    let mut ask = ev("PreToolUse");
    ask["tool_name"] = json!("AskUserQuestion");
    assert_eq!(
        kind_of(ask),
        Some(HookKind::NeedsAnswer {
            what: "a question".into()
        })
    );
    let mut form = ev("Notification");
    form["notification_type"] = json!("elicitation_dialog");
    assert_eq!(
        kind_of(form),
        Some(HookKind::NeedsAnswer {
            what: "a form".into()
        })
    );
    assert_eq!(
        kind_of(ev("SessionEnd")),
        Some(HookKind::SessionEnded {
            reason: "other".into()
        })
    );
    assert_eq!(kind_of(ev("CwdChanged")), None);
    assert_eq!(kind_of(ev("SomethingNew")), None);
    assert_eq!(kind_of(json!({"hook_event_name": "Stop"})), None);
}

#[test]
fn idle_prompt_and_other_notifications_are_dropped() {
    assert_eq!(
        normalize(&line(fixture("notification_idle")), &no_tail),
        None
    );
    let mut other = fixture("notification_idle");
    other["notification_type"] = json!("permission_prompt");
    assert_eq!(normalize(&line(other), &no_tail), None);
}

#[test]
fn a_stop_without_its_message_reads_the_transcript_tail() {
    let mut payload = fixture("stop");
    payload
        .as_object_mut()
        .unwrap()
        .remove("last_assistant_message");
    let tail = |p: &Path| {
        assert_eq!(p, Path::new("/work/Lantern/conv-1.jsonl"));
        Some("from the tail".to_string())
    };
    let env = normalize(&line(payload), &tail).unwrap();
    assert_eq!(
        env.kind,
        HookKind::TurnEnded {
            last_message: "from the tail".into()
        }
    );
    assert_eq!(
        env.transcript_path.as_deref(),
        Some("/work/Lantern/conv-1.jsonl")
    );
}

#[test]
fn a_long_reply_keeps_its_head_and_tail() {
    let mut payload = fixture("stop");
    payload["last_assistant_message"] = json!("é".repeat(3000));
    assert_eq!(
        kind_of(payload),
        Some(HookKind::TurnEnded {
            last_message: format!("{}\n[…]\n{}", "é".repeat(600), "é".repeat(1800))
        })
    );
}

#[test]
fn a_line_from_another_version_is_dropped() {
    let mut l = line(fixture("stop"));
    l.v = 2;
    assert_eq!(normalize(&l, &no_tail), None);
}

#[test]
fn env_fields_fill_the_envelope() {
    let mut l = line(fixture("cwd_changed"));
    l.env = HookEnv {
        moa_session: Some("slot-1".into()),
        moa_brain: Some("work".into()),
        claude_config_dir: Some("/acct-a".into()),
        claude_session_id: Some("env-conv".into()),
        ..HookEnv::default()
    };
    let got = normalize(&l, &no_tail).unwrap();
    assert_eq!(
        got,
        HookEnvelope {
            agent: AgentKind::Claude,
            slot: Some("slot-1".into()),
            brain: Some("work".into()),
            store_dir: Some("/acct-a".into()),
            conversation_id: "conv-1".into(),
            kind: HookKind::CwdChanged {
                cwd: "/work/Lantern/sub".into()
            },
            at: UtcMs(1_700_000_000_123),
            at_ns: 1_700_000_000_123_456_789,
            pid: 4242,
            seq: 7,
            cwd: Some("/work/Lantern".into()),
            transcript_path: None,
        }
    );
    assert_eq!(got.dedupe_key(), (4242, 7, 1_700_000_000_123_456_789));
}

#[test]
fn the_env_session_id_stands_in_for_a_missing_one() {
    let mut l = line(json!({"hook_event_name": "PostToolUse"}));
    l.env.claude_session_id = Some("env-conv".into());
    assert_eq!(normalize(&l, &no_tail).unwrap().conversation_id, "env-conv");
}

#[test]
fn last_assistant_text_takes_the_last_text_item() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    let lines = [
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}),
        json!({"type":"user","message":{"content":[{"type":"text","text":"a question"}]}}),
        json!({"type":"assistant","message":{"content":[
            {"type":"text","text":"second, early"},
            {"type":"tool_use","name":"Read"},
            {"type":"text","text":"second, last"},
            {"type":"tool_use","name":"Read"}]}}),
    ];
    let mut f = std::fs::File::create(&path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    assert_eq!(last_assistant_text(&path).as_deref(), Some("second, last"));
    assert_eq!(last_assistant_text(&dir.path().join("nope.jsonl")), None);
}

#[test]
fn last_assistant_text_reads_only_the_tail_and_survives_junk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "{}",
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"early"}]}})
    )
    .unwrap();
    // More than 256 KiB of other lines pushes the assistant line out of the window.
    for _ in 0..3000 {
        writeln!(f, "{}", json!({"type":"user","pad":"x".repeat(100)})).unwrap();
    }
    writeln!(f, "not json at all").unwrap();
    drop(f);
    assert_eq!(last_assistant_text(&path), None);
}

/// The positive case of the window: an assistant line inside the last 256 KiB, behind more
/// than 256 KiB of other lines, is found, even with a line cut by the window's start.
#[test]
fn last_assistant_text_finds_a_line_inside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    for _ in 0..3000 {
        writeln!(f, "{}", json!({"type":"user","pad":"x".repeat(100)})).unwrap();
    }
    writeln!(
        f,
        "{}",
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"in the window"}]}})
    )
    .unwrap();
    for _ in 0..100 {
        writeln!(f, "{}", json!({"type":"user","pad":"y".repeat(100)})).unwrap();
    }
    drop(f);
    assert!(std::fs::metadata(&path).unwrap().len() > 256 * 1024);
    assert_eq!(last_assistant_text(&path).as_deref(), Some("in the window"));
}

/// M2: an empty or blank message is no message: the transcript's tail stands in.
#[test]
fn a_stop_with_a_blank_message_reads_the_transcript_tail() {
    for blank in ["", "  \n"] {
        let mut payload = fixture("stop");
        payload["last_assistant_message"] = json!(blank);
        let tail = |_: &Path| Some("from the tail".to_string());
        let env = normalize(&line(payload), &tail).unwrap();
        assert_eq!(
            env.kind,
            HookKind::TurnEnded {
                last_message: "from the tail".into()
            },
            "{blank:?}"
        );
    }
}

/// M4: a transcript path that names a folder or a FIFO is not read: opening a FIFO would
/// block the daemon's thread until something writes to it.
#[test]
fn last_assistant_text_reads_only_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(last_assistant_text(dir.path()), None);
    let fifo = dir.path().join("pipe.jsonl");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(last_assistant_text(&fifo));
    });
    let read = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("reading a FIFO blocked");
    assert_eq!(read, None);
}

/// The seam between the plugin and the normalizer: every event the plugin subscribes to
/// (each subject its matcher names, too) normalizes to an event for the core, from a
/// minimal synthetic payload.
#[test]
fn every_subscribed_event_normalizes_to_something() {
    let files = moa_agents::claude::session_plugin(Path::new("/opt/moa"));
    let (_, text) = files
        .iter()
        .find(|(rel, _)| *rel == "hooks/hooks.json")
        .expect("the plugin's hooks");
    let hooks: Value = serde_json::from_str(text).unwrap();
    let events = hooks["hooks"].as_object().expect("hooks by event");
    assert!(!events.is_empty());
    for (event, entries) in events {
        for entry in entries.as_array().unwrap() {
            let subjects: Vec<Option<&str>> = match entry["matcher"].as_str() {
                Some(m) => m.split('|').map(Some).collect(),
                None => vec![None],
            };
            for subject in subjects {
                let mut payload = json!({"hook_event_name": event, "session_id": "conv-1"});
                match event.as_str() {
                    "PreToolUse" => payload["tool_name"] = json!(subject.unwrap()),
                    "Notification" => payload["notification_type"] = json!(subject.unwrap()),
                    "CwdChanged" => payload["new_cwd"] = json!("/work/Lantern"),
                    _ => {}
                }
                assert!(
                    normalize(&line(payload), &no_tail).is_some(),
                    "{event} {subject:?}"
                );
            }
        }
    }
}
