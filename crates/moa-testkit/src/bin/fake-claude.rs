//! A stand-in for the `claude` binary: it runs the hooks of the injected plugin the way Claude
//! Code does (exec form, payload on stdin), driven by a script of `FakeStep`s. Like Claude, it
//! has a transcript for a conversation by its first hook: one line, written only inside the
//! temp dir (R34).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use moa_agents::claude::project_slot;
use moa_testkit::FakeStep;
use serde_json::{Map, Value, json};

const FIRST_ID: &str = "00000000-0000-4000-8000-000000000001";

/// The one line of every synthetic transcript (R34).
const TRANSCRIPT_LINE: &str = "{\"type\":\"summary\",\"summary\":\"fake\"}\n";

struct Fake {
    session_id: String,
    rotations: u32,
    cwd: PathBuf,
    hooks: Value,
    /// Where Claude would keep its conversations: `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
    store: PathBuf,
    /// The ids a `SessionStart` has fired for.
    started: HashSet<String>,
}

fn main() {
    let args = parse_args(std::env::args().skip(1));
    let session_id = if args.id.is_empty() {
        FIRST_ID.into()
    } else {
        args.id
    };
    let cwd = std::env::current_dir().expect("cwd");
    let mut fake = Fake {
        session_id,
        rotations: 1,
        hooks: load_hooks(),
        cwd,
        store: store(),
        started: HashSet::new(),
    };
    // A resumed conversation has a transcript already; one that lost it gets one.
    if args.resumed {
        fake.write_transcript();
    }
    if let Some(prompt) = args.prompt {
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

/// What fake-claude reads from its arguments.
struct Args {
    /// The conversation id, from `--session-id` or `--resume`.
    id: String,
    /// The id came from `--resume`.
    resumed: bool,
    prompt: Option<String>,
}

/// The conversation id (`--session-id` or `--resume`) and the positional prompt; Claude's other
/// flags are accepted and ignored.
fn parse_args(args: impl Iterator<Item = String>) -> Args {
    let (mut id, mut resumed, mut prompt) = (String::new(), false, None);
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session-id" | "--resume" => {
                resumed = arg == "--resume";
                id = args.next().unwrap_or_default();
            }
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
    Args {
        id,
        resumed,
        prompt,
    }
}

/// `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`: where Claude keeps its conversations.
fn store() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"),
    }
}

/// Whether `path` is inside the temp dir or `/tmp`, so fake-claude may write there. A path
/// with `..` in it never is, and neither is a temp dir of `/`. A run that inherited a
/// developer's real `HOME` can't touch their Claude folder.
fn in_temp(path: &Path) -> bool {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    [std::env::temp_dir(), PathBuf::from("/tmp")]
        .into_iter()
        .flat_map(|root| {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            [root, canonical]
        })
        .filter(|root| root.parent().is_some())
        .any(|root| path.starts_with(root))
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
    /// `<store>/projects/<project_slot(cwd)>/<id>.jsonl`, where Claude keeps the current
    /// conversation.
    fn transcript(&self) -> PathBuf {
        self.store
            .join("projects")
            .join(project_slot(&self.cwd))
            .join(format!("{}.jsonl", self.session_id))
    }

    /// Writes the current conversation's synthetic transcript, one line, unless it is there
    /// already or the store isn't inside the temp dir (R34).
    fn write_transcript(&self) {
        let path = self.transcript();
        if !in_temp(&path) {
            return;
        }
        fs::create_dir_all(path.parent().expect("a projects folder")).expect("projects folder");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file
                .write_all(TRANSCRIPT_LINE.as_bytes())
                .expect("write transcript"),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("transcript {path:?}: {e}"),
        }
    }

    fn fire(&mut self, event: &str, fields: Map<String, Value>) {
        // A real Claude has its transcript by its first hook for a conversation.
        if event == "SessionStart" && self.started.insert(self.session_id.clone()) {
            self.write_transcript();
        }
        let mut payload = fields;
        payload.insert("hook_event_name".into(), event.into());
        payload.insert("session_id".into(), self.session_id.clone().into());
        payload.insert("cwd".into(), self.cwd.to_string_lossy().into_owned().into());
        payload.insert(
            "transcript_path".into(),
            self.transcript().to_string_lossy().into_owned().into(),
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
