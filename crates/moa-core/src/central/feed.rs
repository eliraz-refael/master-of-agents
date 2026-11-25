//! moa's feed rows: the kinds a line can be, how its body is written, and how a row is made.

use serde_json::{Value, json};

use super::wire::{PermOption, PlanEntry};
use crate::UtcMs;
use crate::core::Tx;
use crate::model::{FeedEntry, SuggestionId};
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
    pub const PERMISSION: &str = "permission";
    pub const CARD: &str = "card";
    pub const SUGGESTION: &str = "suggestion";
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
    let input = input.map(tool_input);
    json!({ "title": title, "kind": kind, "input": input }).to_string()
}

/// A tool's input as a `tool` row keeps it: cut to [`TOOL_INPUT_MAX`] characters.
pub fn tool_input(input: &str) -> String {
    cut(input, TOOL_INPUT_MAX)
}

/// The body of a `plan` row: `{"entries": [{"content", "status", "priority"}]}`.
pub fn plan_body(entries: &[PlanEntry]) -> String {
    let entries: Vec<Value> = entries
        .iter()
        .map(|e| json!({ "content": e.content, "status": e.status, "priority": e.priority }))
        .collect();
    json!({ "entries": entries }).to_string()
}

/// The body of a `permission` row: `{"title", "options": [{"kind", "name"}]}`, its title
/// (the tool call it asks about) cut as a tool's input is ([`tool_input`]). The options' ids
/// stay in the core.
pub fn permission_body(title: &str, options: &[PermOption]) -> String {
    let options: Vec<Value> = options
        .iter()
        .map(|o| json!({ "kind": o.kind.name(), "name": o.name }))
        .collect();
    json!({ "title": tool_input(title), "options": options }).to_string()
}

/// The body of a `card` row (K7): `{"what", "text"}`, `what` naming the card and `text` its
/// Markdown.
pub fn card_body(what: &str, text: &str) -> String {
    json!({ "what": what, "text": text }).to_string()
}

/// The body of a `suggestion` row (D7): `{"id", "text"}`, the suggestion's id and the text it
/// was made with. The console draws the suggestion as its topic has it, this text standing in
/// when the topic hasn't got it.
pub fn suggestion_body(id: SuggestionId, text: &str) -> String {
    json!({ "id": id, "text": text }).to_string()
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
