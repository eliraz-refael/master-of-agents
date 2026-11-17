//! A stand-in for the `claude` binary: it runs the hooks of the injected plugin the way Claude
//! Code does (exec form, payload on stdin), driven by a script of `FakeStep`s.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use moa_testkit::FakeStep;
use serde_json::{Map, Value, json};

const FIRST_ID: &str = "00000000-0000-4000-8000-000000000001";

struct Fake {
    session_id: String,
    rotations: u32,
    cwd: PathBuf,
    hooks: Value,
}

fn main() {
    let (mut session_id, prompt) = parse_args(std::env::args().skip(1));
    if session_id.is_empty() {
        session_id = FIRST_ID.into();
    }
    let cwd = std::env::current_dir().expect("cwd");
    let mut fake = Fake {
        session_id,
        rotations: 1,
        hooks: load_hooks(),
        cwd,
    };
    if let Some(prompt) = prompt {
        let mut fields = Map::new();
        fields.insert("prompt".into(), Value::String(prompt));
        fake.fire("UserPromptSubmit", fields);
    }
    for step in load_script() {
        match step {
            FakeStep::Hook { event, fields } => fake.fire(&event, fields),
            FakeStep::Sleep { ms } => std::thread::sleep(Duration::from_millis(ms)),
            FakeStep::Wait { file } => {
                while !fake.cwd.join(&file).exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            FakeStep::Clear => {
                fake.fire("SessionEnd", one("reason", "clear"));
                fake.rotations += 1;
                fake.session_id = format!("00000000-0000-4000-8000-{:012}", fake.rotations);
                fake.fire("SessionStart", one("source", "clear"));
            }
            FakeStep::Exit => {
                fake.fire("SessionEnd", one("reason", "prompt_input_exit"));
                std::process::exit(0);
            }
            FakeStep::Die => std::process::exit(1),
            FakeStep::Hang => hang(),
        }
    }
    hang()
}

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn one(key: &str, value: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.into(), Value::String(value.into()));
    m
}

/// The conversation id (`--session-id` or `--resume`) and the positional prompt; Claude's other
/// flags are accepted and ignored.
fn parse_args(args: impl Iterator<Item = String>) -> (String, Option<String>) {
    let (mut id, mut prompt) = (String::new(), None);
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session-id" | "--resume" => id = args.next().unwrap_or_default(),
            "--name" => {
                args.next();
            }
            "--" => {
                prompt = args.next();
                break;
            }
            _ if arg.starts_with("--") => {}
            _ => prompt = Some(arg),
        }
    }
    (id, prompt)
}

fn load_script() -> Vec<FakeStep> {
    let path = std::env::var_os("FAKE_CLAUDE_SCRIPT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fake-claude.jsonl"));
    let Ok(text) = fs::read_to_string(&path) else {
        return vec![FakeStep::Hook {
            event: "SessionStart".into(),
            fields: Map::new(),
        }];
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad script line {l:?}: {e}")))
        .collect()
}

/// `hooks/hooks.json` of the first `CLAUDE_CODE_PLUGIN_DIRS` entry, or no hooks at all.
fn load_hooks() -> Value {
    let dirs = std::env::var("CLAUDE_CODE_PLUGIN_DIRS").unwrap_or_default();
    let Some(dir) = dirs.split(':').find(|d| !d.is_empty()) else {
        return json!({});
    };
    let text = fs::read_to_string(Path::new(dir).join("hooks/hooks.json")).unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("hooks").cloned())
        .unwrap_or_else(|| json!({}))
}

impl Fake {
    fn fire(&self, event: &str, fields: Map<String, Value>) {
        let config = std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default();
        let mut payload = fields;
        payload.insert("hook_event_name".into(), event.into());
        payload.insert("session_id".into(), self.session_id.clone().into());
        payload.insert("cwd".into(), self.cwd.to_string_lossy().into_owned().into());
        payload.insert(
            "transcript_path".into(),
            format!("{config}/projects/x/{}.jsonl", self.session_id).into(),
        );
        let subject = match event {
            "Notification" => payload.get("notification_type"),
            _ => payload.get("tool_name"),
        }
        .and_then(Value::as_str)
        .map(str::to_owned);
        let stdin = Value::Object(payload).to_string();
        let entries = self.hooks.get(event).and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            if let Some(matcher) = entry.get("matcher").and_then(Value::as_str)
                && !subject
                    .as_deref()
                    .is_some_and(|s| matcher.split('|').any(|name| name == s))
            {
                continue;
            }
            for hook in entry["hooks"].as_array().into_iter().flatten() {
                self.run(event, hook, &stdin);
            }
        }
    }

    /// Run one hook: spawn it directly (no shell), feed it the payload, and wait. Only
    /// SessionStart's stdout matters; it lands in `.fake-claude.context`, which a start makes
    /// even when it says nothing, so a test can tell "no note" from "no start yet".
    fn run(&self, event: &str, hook: &Value, stdin: &str) {
        let command = hook["command"].as_str().expect("hook command");
        let args = hook["args"].as_array().into_iter().flatten();
        let mut child = Command::new(command)
            .args(args.filter_map(Value::as_str))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("hook {command}: {e}"));
        let mut pipe = child.stdin.take().expect("stdin");
        let _ = pipe.write_all(stdin.as_bytes());
        drop(pipe);
        let out = child.wait_with_output().expect("hook output");
        if event == "SessionStart" {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.cwd.join(".fake-claude.context"))
                .expect("context file");
            file.write_all(&out.stdout).expect("write context");
        }
    }
}
