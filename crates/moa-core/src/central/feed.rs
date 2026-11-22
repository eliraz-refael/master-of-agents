//! moa's feed rows: the kinds a line can be, how its body is written, and how a row is made.

use serde_json::{Value, json};

use super::wire::PlanEntry;
use crate::UtcMs;
use crate::core::Tx;
use crate::model::FeedEntry;
use crate::state::{State, Table};
use crate::text::cut;

/// The `kind` column of a feed row.
pub mod kind {
    pub const YOU: &str = "you";
    pub const MOA: &str = "moa";
    pub const THOUGHT: &str = "thought";
    pub const TOOL: &str = "tool";
    pub const PLAN: &str = "plan";
    pub const SYSTEM: &str = "system";
    pub const ERROR: &str = "error";
}

/// The most of a tool's input a `tool` row keeps, in characters.
pub const TOOL_INPUT_MAX: usize = 2_000;

/// The body of a text row: `{"text": …}`.
pub fn text_body(text: &str) -> String {
    json!({ "text": text }).to_string()
}

/// The text of a text row's `body`; empty when it has none.
pub fn body_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["text"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The body of a `tool` row: `{"title", "kind", "input"}`, its input (JSON, as the agent
/// gave it) cut to [`TOOL_INPUT_MAX`] characters.
pub fn tool_body(title: &str, kind: &str, input: Option<&str>) -> String {
    let input = input.map(|i| cut(i, TOOL_INPUT_MAX));
    json!({ "title": title, "kind": kind, "input": input }).to_string()
}

/// The body of a `plan` row: `{"entries": [{"content", "status", "priority"}]}`.
pub fn plan_body(entries: &[PlanEntry]) -> String {
    let entries: Vec<Value> = entries
        .iter()
        .map(|e| json!({ "content": e.content, "status": e.status, "priority": e.priority }))
        .collect();
    json!({ "entries": entries }).to_string()
}

/// A new row of `thread`, with the next id of the table (counting this transaction's). The
/// caller commits it as `Change::Feed`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn row(
    state: &State,
    tx: &mut Tx,
    now: UtcMs,
    thread: &str,
    kind: &str,
    message_id: Option<String>,
    body: String,
    state_col: Option<String>,
) -> FeedEntry {
    FeedEntry {
        id: tx.next_id(state, Table::Feed),
        thread_id: thread.to_string(),
        at: now,
        kind: kind.to_string(),
        message_id,
        automatic: false,
        body,
        state: state_col,
    }
}
