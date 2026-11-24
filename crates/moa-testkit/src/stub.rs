//! The script and log files of `stub-acp`, a scripted stand-in for moa's agent: what a test
//! writes before the stub starts, and reads back after.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn yes() -> bool {
    true
}

/// One step of a `stub-acp` script, a JSON line in `<cwd>/.stub-acp.jsonl`. The steps up to each
/// `End`, `Fail`, `Hang`, `HangHard` or `Die` are one prompt's turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "do")]
pub enum StubStep {
    /// What `initialize` offers. Only as the first line; everything is offered when it is absent.
    Caps {
        #[serde(default = "yes")]
        load: bool,
        #[serde(default = "yes")]
        resume: bool,
        #[serde(default = "yes")]
        system_prompt: bool,
    },
    /// An answer chunk, with this message id.
    Say { text: String, id: Option<String> },
    /// A thought chunk.
    Think { text: String },
    /// A tool call starts.
    Tool {
        id: String,
        title: String,
        input: Option<Value>,
    },
    /// A tool call ends with this status (`completed`, `failed`, ...).
    ToolDone { id: String, status: String },
    /// A permission request; options are `(option id, kind)`. Its outcome is logged.
    Ask {
        title: String,
        options: Vec<(String, String)>,
    },
    /// `n` answer chunks, written as fast as the connection takes them, then a `burst.done`
    /// line in the log.
    Burst { n: u32 },
    /// A line on stderr.
    Stderr { line: String },
    /// Start a child (the stub itself, in a mode that only sleeps) and write its pid to
    /// `<cwd>/.stub-acp.child`.
    Spawn,
    /// Like `Spawn`, but the child ignores `SIGTERM`. Its pid is written by the child itself,
    /// once it ignores the signal, so a test that has read the pid can signal it at once.
    SpawnStubborn,
    /// Wait until `<cwd>/<file>` exists: a test opens the gate once it has seen what the steps
    /// before it did, so no step races the clock.
    Wait { file: String },
    /// End the turn with this stop reason (default `end_turn`).
    End { stop: Option<String> },
    /// Fail the turn: the prompt's answer is a JSON-RPC error (code `-32000`) with `message`.
    Fail { message: String },
    /// Wait for `session/cancel`, then end the turn `cancelled`.
    Hang,
    /// Never end the turn, whatever arrives.
    HangHard,
    /// Exit at once.
    Die,
}

/// Write `steps` as `<dir>/.stub-acp.jsonl`, the script stub-acp reads from its cwd.
pub fn write_stub_script(dir: &Path, steps: &[StubStep]) {
    let mut text = String::new();
    for step in steps {
        text.push_str(&serde_json::to_string(step).expect("step json"));
        text.push('\n');
    }
    std::fs::write(dir.join(".stub-acp.jsonl"), text).expect("write stub-acp script");
}

/// What stub-acp has logged in `<dir>/.stub-acp.log`: every request and notification it
/// received as `{"method", "params"}`, plus its own `permission.outcome` and `burst.done` lines.
/// The `initialize` line also has `env` (a few variables' values), `env_names` (every
/// variable's name), `cwd` and the stub's `pid`.
pub fn stub_log(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join(".stub-acp.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("a log line"))
        .collect()
}
