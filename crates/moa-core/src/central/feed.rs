//! moa's feed rows: the kinds a line can be, how its body is written, and how a row is made.

use serde_json::json;

use crate::UtcMs;
use crate::core::Tx;
use crate::model::FeedEntry;
use crate::state::{State, Table};

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

/// The body of a text row: `{"text": …}`.
pub fn text_body(text: &str) -> String {
    json!({ "text": text }).to_string()
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
