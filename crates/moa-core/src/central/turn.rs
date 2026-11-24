//! What moa says in a turn lands in the feed as it comes (A5): each update is one commit,
//! a chunk extending its row, a tool call's or the plan's row changed in place. The turn's
//! end answers whoever waits (A6). A permission request waits for your answer as a
//! `permission` row (K5), until you answer it or nobody can any more.
//!
//! The rows a turn writes are kept as written ([`Moa::written`]), saved or not: a refused
//! commit doesn't stop the turn, and its end still answers with moa's words.

use serde_json::{Value, json};

use super::feed::{
    self, body_text, kind, permission_body, plan_body, text_body, tool_body, tool_input,
};
use super::wire::{AcpCmd, PermKind, PermOption, StopReason, Update};
use super::{Asking, Moa, Phase, account, answer, chosen, warn};
use crate::Now;
use crate::change::Change;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::model::FeedEntry;
use crate::op::{Done, OpError, PermAnswer};
use crate::state::{State, Table};
use crate::tmux::link::log;

/// What a tool call's row says while it runs; a turn's end that isn't `end_turn`, or moa's
/// agent going, makes it `cancelled`.
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
    if !in_turn(moa, thread) && moa.replaying.as_deref() != Some(thread) {
        let line = "an update outside moa's turn isn't kept";
        return vec![log(LogLevel::Debug, line.into())];
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
        Update::ToolUpdate {
            id,
            title,
            status,
            input,
        } => keyed(&state.central, kind::TOOL, &format!("tool:{id}")).map(|row| {
            let mut row = row.clone();
            if (title.is_some() || input.is_some())
                && let Ok(mut body) = serde_json::from_str::<Value>(&row.body)
            {
                if let Some(title) = title {
                    body["title"] = json!(title);
                }
                if let Some(input) = input {
                    body["input"] = json!(tool_input(&input));
                }
                row.body = body.to_string();
            }
            if status.is_some() {
                row.state = status;
            }
            row
        }),
        // One plan row a turn; a replay's is the feed's already.
        Update::Plan { entries } if state.central.replaying.is_none() => {
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

/// moa's agent asks before it goes on. In the turn under way the request waits for your
/// answer: a `permission` row, `pending`, and an entry in [`Moa::asking`]; nothing is sent
/// until you answer. One that offers nothing y, a or n could pick is cancelled at once, its
/// row `cancelled`. Any other request (another thread, no turn, or a run that has exited)
/// is cancelled at once, with no row.
pub(super) fn permission(
    state: &mut State,
    now: &Now,
    req: u64,
    thread: String,
    title: &str,
    options: &[PermOption],
) -> Vec<Effect> {
    let moa = &state.central;
    let live = moa
        .turn
        .as_ref()
        .is_some_and(|t| t.thread == thread && t.exited.is_none());
    let cancelled = Effect::Acp(AcpCmd::Permission {
        generation: moa.generation,
        req,
        choice: None,
    });
    if !live {
        return vec![cancelled];
    }
    let title = match title.trim() {
        "" => "a tool call",
        _ => title,
    };
    let answerable = PermAnswer::ALL
        .into_iter()
        .any(|a| chosen(options, a).is_some());
    let mut tx = Tx::new(now);
    let body = permission_body(title, options);
    let row_state = match answerable {
        true => "pending",
        false => "cancelled",
    };
    let row = new_row(
        state,
        &mut tx,
        &thread,
        kind::PERMISSION,
        None,
        body,
        Some(row_state.to_string()),
    );
    if !answerable {
        let mut fx = record(state, tx, vec![row]);
        fx.push(cancelled);
        return fx;
    }
    let options = options.to_vec();
    state.central.asking.insert(row.id, Asking { req, options });
    record(state, tx, vec![row])
}

/// Request `id`'s row, answered with the option of `kind`: it says which.
pub(super) fn answered_row(moa: &Moa, id: i64, kind: PermKind) -> Option<FeedEntry> {
    asked_row(moa, id).map(|row| FeedEntry {
        state: Some(kind.name().into()),
        ..row.clone()
    })
}

/// Nobody will answer the requests that wait any more (`console.cancel`): moa's agent hears
/// each cancelled, and their rows say so.
pub(super) fn unanswered(state: &mut State, now: &Now) -> Vec<Effect> {
    let (rows, cmds) = forsaken(&mut state.central, true);
    let mut fx = match rows.is_empty() {
        true => Vec::new(),
        false => record(state, Tx::new(now), rows),
    };
    fx.extend(cmds.into_iter().map(Effect::Acp));
    fx
}

/// Takes every request that waits: their rows as `cancelled`, and, when moa's agent is
/// there to hear it (`told`), each answered cancelled.
fn forsaken(moa: &mut Moa, told: bool) -> (Vec<FeedEntry>, Vec<AcpCmd>) {
    let asking = std::mem::take(&mut moa.asking);
    let rows = asking
        .keys()
        .filter_map(|&id| asked_row(moa, id))
        .map(|row| FeedEntry {
            state: Some("cancelled".into()),
            ..row.clone()
        })
        .collect();
    let cmds = match told {
        true => asking
            .into_values()
            .map(|a| AcpCmd::Permission {
                generation: moa.generation,
                req: a.req,
                choice: None,
            })
            .collect(),
        false => Vec::new(),
    };
    (rows, cmds)
}

/// The `permission` row with `id` the turn has written.
fn asked_row(moa: &Moa, id: i64) -> Option<&FeedEntry> {
    moa.written.get(&id).filter(|r| r.kind == kind::PERMISSION)
}

/// Turn `prompt` ended, as `result` says: whoever waits hears moa's words, or why it ended
/// early (a `system` line, and an `error` line when it failed). A turn that didn't end
/// normally (cancelled, stopped early, failed) cancels its running tool calls. An end that
/// comes after moa's agent went away says nothing more, the agent's going said it, but
/// cancels the tool calls late updates started.
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
    // What still waits for your answer can't be answered any more.
    let (cards, cmds) = forsaken(moa, turn.exited.is_none());
    moa.answered.clear();
    if turn.exited.is_some() {
        // Late updates after the exit may have started tool calls the exit didn't see.
        let mut rows = cancelled_tools(&state.central);
        rows.extend(cards);
        if !rows.is_empty() {
            fx.extend(write(state, Tx::new(now), rows).unwrap_or_default());
        }
        state.central.written.clear();
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
    // A turn that ended early or failed leaves nothing running.
    let mut rows = match result {
        Ok(StopReason::EndTurn) => Vec::new(),
        _ => cancelled_tools(moa),
    };
    rows.extend(cards);
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
    // A turn that failed for its account says so; one that ended well lets it say so again.
    match &result {
        Ok(StopReason::EndTurn) => state.central.account_card = None,
        Ok(_) => {}
        Err(why) => fold_in(&mut fx, account::failed(state, now, why)),
    }
    fx.extend(cmds.into_iter().map(Effect::Acp));
    fx.extend(answer(turn.waiter, reply));
    fx
}

/// moa's agent went away during the turn, or the watchdog is restarting it, cutting the turn
/// short: its running tool calls and the requests that wait for your answer are cancelled
/// (nobody is there to hear it), in one commit `tx` with `says`, the row that says why.
pub(super) fn cut_short(state: &mut State, tx: Tx, says: FeedEntry) -> Vec<Effect> {
    let mut rows = cancelled_tools(&state.central);
    rows.extend(forsaken(&mut state.central, false).0);
    state.central.answered.clear();
    rows.push(says);
    let mut fx: Vec<Effect> = unsaved(&mut state.central).into_iter().collect();
    fx.extend(write(state, tx, rows).unwrap_or_default());
    fx
}

/// The turn's tool calls still running, as cancelled: their turn ended without them.
fn cancelled_tools(moa: &Moa) -> Vec<FeedEntry> {
    let running = moa.written.values().filter(|r| {
        r.kind == kind::TOOL && r.state.as_deref().is_some_and(|s| RUNNING.contains(&s))
    });
    running
        .map(|r| FeedEntry {
            state: Some("cancelled".into()),
            ..r.clone()
        })
        .collect()
}

/// After a reload the rows are the store's. A row the reloaded tail holds is taken from it;
/// a new row a failed commit lost (its id not yet the store's) is forgotten, so the updates
/// stepped again after the reload write it once, as is a permission request it held. An
/// answer whose commit failed is undone.
///
/// An older row, gone from the tail, is kept as written: the snapshot carries only the
/// newest [`crate::publish::FEED_SHOWN`] rows, and the core reads nothing else, so the store's
/// version of it can't be known here. Stepping again is exact for every update but a chunk
/// that extended such a row in the lost commit, which would be added twice; that takes a
/// message whose pieces are more than a tail's worth of rows apart.
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
    // A request whose row was lost is heard again, stepped after the reload. One you
    // answered whose row is still `pending` lost its answer's commit, and the answer with it
    // (it waits for the commit): it waits for you again.
    let moa = &mut state.central;
    moa.asking.retain(|id, _| moa.written.contains_key(id));
    for (id, asked) in std::mem::take(&mut moa.answered) {
        if asked_row(moa, id).is_some_and(|r| r.state.as_deref() == Some("pending")) {
            moa.asking.insert(id, asked);
        }
    }
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
        None if moa.replaying.is_some() => return None,
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
    state.central.replaying.as_deref() == Some(thread)
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
pub(super) fn new_row(
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
pub(super) fn record(state: &mut State, tx: Tx, rows: Vec<FeedEntry>) -> Vec<Effect> {
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
