//! What moa says in a turn lands in the feed as it comes (A5): each update is one commit,
//! a chunk extending its row, a tool call's or the plan's row changed in place. The turn's
//! end answers whoever waits (A6). A permission request is refused, by its options' kinds,
//! until moa can ask you (6c).
//!
//! The rows a turn writes are kept as written ([`Moa::written`]), saved or not: a refused
//! commit doesn't stop the turn, and its end still answers with moa's words.

use serde_json::{Value, json};

use super::feed::{self, body_text, kind, plan_body, text_body, tool_body};
use super::wire::{AcpCmd, PermKind, PermOption, StopReason, Update};
use super::{Moa, Phase, answer, log, warn};
use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::model::FeedEntry;
use crate::op::{Done, OpError};
use crate::state::{State, Table};

/// What a tool call's row says while it runs; a cancelled turn's end makes it `cancelled`.
const RUNNING: [&str; 2] = ["pending", "in_progress"];

/// moa's agent said `update` in `thread`. While a load replays a thread, only rows whose
/// keys the feed hasn't got are written, so no row is there twice.
pub(super) fn update(state: &mut State, now: &Now, thread: &str, update: Update) -> Vec<Effect> {
    match &update {
        Update::UserEcho => return vec![skipped("user_message_chunk")],
        Update::Other(name) => return vec![skipped(name)],
        _ => {}
    }
    let moa = &state.central;
    if !in_turn(moa, thread) && !(moa.replaying && state.thread(thread).is_some()) {
        return vec![log(
            LogLevel::Debug,
            "an update outside moa's turn isn't kept",
        )];
    }
    let mut tx = Tx::new(now);
    let row = match update {
        Update::Message { id, text } => chunk(state, &mut tx, thread, kind::MOA, id, &text),
        Update::Thought { id, text } => chunk(state, &mut tx, thread, kind::THOUGHT, id, &text),
        Update::Tool {
            id,
            title,
            kind: tool_kind,
            status,
            input,
        } => {
            let body = tool_body(&title, &tool_kind, input.as_deref());
            let key = format!("tool:{id}");
            match keyed(&state.central, kind::TOOL, &key) {
                Some(row) => Some(FeedEntry {
                    body,
                    state: Some(status),
                    ..row.clone()
                }),
                None if known(state, thread, &key) => None,
                None => Some(new_row(
                    state,
                    &mut tx,
                    thread,
                    kind::TOOL,
                    Some(key),
                    body,
                    Some(status),
                )),
            }
        }
        Update::ToolUpdate { id, title, status } => {
            keyed(&state.central, kind::TOOL, &format!("tool:{id}")).map(|row| {
                let mut row = row.clone();
                if let Some(title) = title
                    && let Ok(mut body) = serde_json::from_str::<Value>(&row.body)
                {
                    body["title"] = json!(title);
                    row.body = body.to_string();
                }
                if status.is_some() {
                    row.state = status;
                }
                row
            })
        }
        // One plan row a turn; a replay's is the feed's already.
        Update::Plan { entries } if !state.central.replaying => {
            let body = plan_body(&entries);
            let plan = state
                .central
                .written
                .values()
                .find(|r| r.kind == kind::PLAN);
            Some(match plan {
                Some(row) => FeedEntry {
                    body,
                    ..row.clone()
                },
                None => new_row(state, &mut tx, thread, kind::PLAN, None, body, None),
            })
        }
        Update::Plan { .. } | Update::UserEcho | Update::Other(_) => None,
    };
    match row {
        Some(row) => record(state, tx, vec![row]),
        None => Vec::new(),
    }
}

/// moa's agent asks before it goes on. moa can't ask you yet: the request is refused with
/// its `reject_once` option, or, when it has none, the turn is cancelled and the request
/// with it. Either way the turn says so in a `system` line.
pub(super) fn permission(
    state: &mut State,
    now: &Now,
    req: u64,
    thread: String,
    title: &str,
    options: &[PermOption],
) -> Vec<Effect> {
    let generation = state.central.generation;
    let title = match title.trim() {
        "" => "a tool call",
        _ => title,
    };
    let refused = format!("moa wanted to {title}, and was refused: moa can't ask you yet");
    let reject = options.iter().find(|o| o.kind == PermKind::RejectOnce);
    let (line, cmds) = match reject {
        Some(o) => (
            refused,
            vec![AcpCmd::Permission {
                generation,
                req,
                choice: Some(o.id.clone()),
            }],
        ),
        None => (
            format!("{refused}; its turn was cancelled"),
            vec![
                AcpCmd::Cancel {
                    generation,
                    thread: thread.clone(),
                },
                AcpCmd::Permission {
                    generation,
                    req,
                    choice: None,
                },
            ],
        ),
    };
    let mut fx = Vec::new();
    if in_turn(&state.central, &thread) {
        let mut tx = Tx::new(now);
        let row = new_row(
            state,
            &mut tx,
            &thread,
            kind::SYSTEM,
            None,
            text_body(&line),
            None,
        );
        fx.extend(record(state, tx, vec![row]));
    }
    fx.extend(cmds.into_iter().map(Effect::Acp));
    fx
}

/// Turn `prompt` ended, as `result` says: whoever waits hears moa's words, or why it ended
/// early (a `system` line, and an `error` line when it failed). A cancelled turn's running
/// tool calls are cancelled with it. An end that comes after moa's agent went away says
/// nothing more: the agent's going said it.
pub(super) fn ended(
    state: &mut State,
    now: &Now,
    prompt: u64,
    result: Result<StopReason, String>,
) -> Vec<Effect> {
    let moa = &mut state.central;
    let Some(turn) = moa.turn.take_if(|t| t.prompt == prompt) else {
        return Vec::new();
    };
    if moa.phase == Phase::Thinking {
        moa.phase = Phase::Ready;
    }
    let mut fx: Vec<Effect> = unsaved(moa).into_iter().collect();
    if turn.exited.is_some() {
        moa.written.clear();
        return fx;
    }
    let data = json!({
        "thread": turn.thread,
        "message_id": turn.message_id,
        "stop": result.as_ref().ok().map(StopReason::name),
    });
    let (line, reply) = match &result {
        Ok(StopReason::EndTurn) => {
            let words: Vec<String> = moa
                .written
                .values()
                .filter(|r| r.kind == kind::MOA)
                .map(|r| body_text(&r.body))
                .collect();
            let text = words.join("\n\n");
            (None, Ok(Done { text, data }))
        }
        Ok(stop) => {
            let text = format!("moa's turn ended early: {}", stop.name());
            (Some((kind::SYSTEM, text.clone())), Ok(Done { text, data }))
        }
        Err(why) => {
            let text = format!("moa's turn failed: {why}");
            (Some((kind::ERROR, text.clone())), Err(OpError(text)))
        }
    };
    let mut rows: Vec<FeedEntry> = Vec::new();
    if result == Ok(StopReason::Cancelled) {
        let running = moa.written.values().filter(|r| {
            r.kind == kind::TOOL && r.state.as_deref().is_some_and(|s| RUNNING.contains(&s))
        });
        rows.extend(running.map(|r| FeedEntry {
            state: Some("cancelled".into()),
            ..r.clone()
        }));
    }
    let mut tx = Tx::new(now);
    if let Some((line_kind, text)) = line {
        let body = text_body(&text);
        let thread = &turn.thread;
        rows.push(new_row(state, &mut tx, thread, line_kind, None, body, None));
    }
    if !rows.is_empty() {
        fx.extend(write(state, tx, rows).unwrap_or_default());
    }
    state.central.written.clear();
    fx.extend(answer(turn.waiter, reply));
    fx
}

/// moa's agent went away during the turn in `thread`: an `error` line says so with
/// `told`, the reason the waiter hears.
pub(super) fn stopped(state: &mut State, now: &Now, thread: &str, told: &str) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let body = text_body(told);
    let row = new_row(state, &mut tx, thread, kind::ERROR, None, body, None);
    let mut fx: Vec<Effect> = unsaved(&mut state.central).into_iter().collect();
    fx.extend(write(state, tx, vec![row]).unwrap_or_default());
    fx
}

/// After a reload the rows are the store's. A row the reloaded tail holds is taken from it;
/// a new row a failed commit lost (its id not yet the store's) is forgotten, so the updates
/// stepped again after the reload write it once. An older row is kept as written.
pub(super) fn reloaded(state: &mut State) {
    let next = state.next_id(Table::Feed);
    let written = std::mem::take(&mut state.central.written);
    state.central.written = written
        .into_iter()
        .filter_map(|(id, row)| match state.feed_tail.get(&id) {
            Some(saved) => Some((id, saved.clone())),
            None if id >= next => None,
            None => Some((id, row)),
        })
        .collect();
}

/// A piece of moa's answer or thinking. With a message id it extends that message's row;
/// without one it extends the turn's last row when that is the same kind and has no id.
/// Otherwise it starts a row.
fn chunk(
    state: &State,
    tx: &mut Tx,
    thread: &str,
    of: &str,
    id: Option<String>,
    text: &str,
) -> Option<FeedEntry> {
    let moa = &state.central;
    let found = match &id {
        Some(key) => keyed(moa, of, key),
        // A replay's pieces without an id can't be told from the feed's.
        None if moa.replaying => return None,
        None => moa
            .written
            .values()
            .next_back()
            .filter(|r| r.kind == of && r.message_id.is_none()),
    };
    match (found, id) {
        (Some(row), _) => Some(FeedEntry {
            body: text_body(&(body_text(&row.body) + text)),
            ..row.clone()
        }),
        (None, Some(key)) if known(state, thread, &key) => None,
        (None, id) => Some(new_row(state, tx, thread, of, id, text_body(text), None)),
    }
}

/// The row of `of` kind with `key` the turn (or replay) has written.
fn keyed<'a>(moa: &'a Moa, of: &str, key: &str) -> Option<&'a FeedEntry> {
    moa.written
        .values()
        .rev()
        .find(|r| r.kind == of && r.message_id.as_deref() == Some(key))
}

/// A load replays `thread`, and the feed has the row with `key` already.
fn known(state: &State, thread: &str, key: &str) -> bool {
    state.central.replaying
        && state
            .feed_keys(thread)
            .is_some_and(|keys| keys.contains(key))
}

/// A turn is under way in `thread`.
fn in_turn(moa: &Moa, thread: &str) -> bool {
    moa.turn.as_ref().is_some_and(|t| t.thread == thread)
}

/// A new row of `thread`. A row a refused commit left unsaved keeps its id for the turn, so
/// a new one never takes it.
fn new_row(
    state: &State,
    tx: &mut Tx,
    thread: &str,
    of: &str,
    key: Option<String>,
    body: String,
    state_col: Option<String>,
) -> FeedEntry {
    let wall = tx.wall;
    let mut row = feed::row(state, tx, wall, thread, of, key, body, state_col);
    while state.central.written.contains_key(&row.id) {
        row.id = tx.next_id(state, Table::Feed);
    }
    row
}

/// Writes what moa said: when the commit is refused the turn goes on, and the update is
/// counted as unsaved.
fn record(state: &mut State, tx: Tx, rows: Vec<FeedEntry>) -> Vec<Effect> {
    write(state, tx, rows).unwrap_or_else(|| {
        state.central.unsaved += 1;
        Vec::new()
    })
}

/// Commits `rows` and keeps them as the turn's, saved or not; `None` when the commit was
/// refused.
fn write(state: &mut State, mut tx: Tx, rows: Vec<FeedEntry>) -> Option<Vec<Effect>> {
    let written = &mut state.central.written;
    written.extend(rows.iter().map(|r| (r.id, r.clone())));
    let changes = rows.into_iter().map(Change::Feed).collect();
    tx.commit(state, changes).ok()?;
    Some(post_commit(state, tx))
}

/// The count of updates that couldn't be saved, said once and started again.
fn unsaved(moa: &mut Moa) -> Option<Effect> {
    let n = std::mem::take(&mut moa.unsaved);
    (n > 0).then(|| warn(format!("{n} of moa's updates couldn't be saved")))
}

/// An update kind moa doesn't keep: only its name is logged.
fn skipped(name: &str) -> Effect {
    log(
        LogLevel::Debug,
        format!("moa's agent sent {name}; it isn't kept"),
    )
}
