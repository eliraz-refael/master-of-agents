//! A recording of moa's agent: the file `acp-tap` writes, `acp-scrub` makes safe to commit and
//! `stub-acp` replays. One JSON line per message, `{"dir": "in"|"out", "msg": …}`: `in` is what
//! moa sent its agent, `out` what the agent sent moa, in the order the tap saw them. A
//! committed recording starts with a [`Header`] line, which `acp-scrub` writes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The file in the agent dir that makes `stub-acp` replay it, beside its script
/// (`.stub-acp.jsonl`), which it then ignores: the account's `acp` takes no arguments, so a
/// test that runs the stub through moa can't pass `--replay`.
pub const REPLAY_FILE: &str = ".stub-acp-replay.jsonl";

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// moa to its agent.
    In,
    /// The agent to moa.
    Out,
}

/// One message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub dir: Dir,
    pub msg: Value,
}

/// A committed recording's first line: the agent's name (its `agentInfo.name`, less any
/// `@scope/`) and version, as its `initialize` answer gave them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub adapter: String,
    pub version: String,
}

/// One line of a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Header(Header),
    Entry(Entry),
}

impl Line {
    /// Reads one line; the error says what's wrong with it, not what it holds.
    pub fn parse(text: &str) -> Result<Line, String> {
        let value: Value = serde_json::from_str(text).map_err(|_| "isn't JSON".to_string())?;
        if value.get("dir").is_some() {
            serde_json::from_value(value)
                .map(Line::Entry)
                .map_err(|_| "isn't a {\"dir\", \"msg\"} entry".into())
        } else {
            serde_json::from_value(value)
                .map(Line::Header)
                .map_err(|_| "is neither an entry nor the header".into())
        }
    }
}

/// A recording's header, if it has one, and its entries. Blank lines are skipped; an error
/// names the line.
pub fn read(path: &Path) -> Result<(Option<Header>, Vec<Entry>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (mut header, mut entries) = (None, Vec::new());
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match Line::parse(line)
            .map_err(|why| format!("{} line {}: {why}", path.display(), n + 1))?
        {
            Line::Header(h) => header = Some(h),
            Line::Entry(e) => entries.push(e),
        }
    }
    Ok((header, entries))
}

/// The made-up recording, in the packaged adapter's shapes: nothing in it is from a real
/// machine.
pub fn synthetic() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("recordings/synthetic.jsonl")
}

/// What a JSON-RPC message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind<'a> {
    /// A request, with its method: it has an `id`.
    Request(&'a str),
    /// A notification, with its method: it has no `id`.
    Notification(&'a str),
    /// An answer to a request: a result or an error, and no method.
    Answer,
}

impl<'a> Kind<'a> {
    pub fn of(msg: &'a Value) -> Kind<'a> {
        match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some(method), Some(_)) => Kind::Request(method),
            (Some(method), None) => Kind::Notification(method),
            (None, _) => Kind::Answer,
        }
    }

    /// The method, or `an answer`.
    pub fn describe(self) -> &'a str {
        match self {
            Kind::Request(method) | Kind::Notification(method) => method,
            Kind::Answer => "an answer",
        }
    }
}
