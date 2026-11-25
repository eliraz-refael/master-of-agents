//! Asks between agents (slice 7): a session or moa asks another agent something and waits
//! for the answer. `ask` saves a `pending` row and waits; `reply` waits again; `peers` lists
//! who can be asked; `asks` lists the caller's asks.
//!
//! A request that waits is kept in memory ([`Parked`]) and answered with an
//! [`Effect::Reply`] when its ask ends ([`end`]) or its wait runs out ([`run`]); a
//! connection that goes drops its requests and nothing else ([`client_gone`]). The rows are
//! the truth: every status change is in the commit that causes it.
//!
//! Statuses (A1): `pending` (saved, not yet typed), `sent` (claimed and typed), `received`
//! (the agent took it), `answered` (`reply` holds the answer), `failed` and `dropped`
//! (`reply` holds the reason).

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::change::Change;
use crate::core::{Tx, finish_op, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, TimerKey, rearm};
use crate::live::{display_state, spike_state};
use crate::model::{Ask, AskId, AskStatus, AskTarget, Session};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::ops::term::with_tilde;
use crate::state::{Caller, State, Table};
use crate::text::{cut, is_unpastable, one_line};
use crate::time::{MINUTE, span};
use crate::tmux::link::log;
use crate::{Now, UtcMs};

/// How long an ask's text is at most, in characters, once cleaned.
pub const ASK_TEXT_MAX: usize = 8000;
/// How long `ask` and `reply` wait for the answer when they aren't told: 9 minutes, under
/// the 10 a session's Bash tool allows.
pub const ASK_WAIT_DEFAULT_MS: i64 = 540_000;
/// How long after an ask is pasted into a pane its Enter goes.
pub const ASK_ENTER_MS: i64 = 500;
/// How long after it is typed an ask waits for the agent to take it before it fails.
pub const ASK_RECEIPT_MS: i64 = 20_000;
/// The longest wait `ask` and `reply` take, in seconds.
pub const ASK_WAIT_MAX_S: i64 = 3600;

/// The reason in a `dropped` row whose asker's wait ran out before the peer was free.
pub const STAYED_BUSY: &str = "stayed busy";

/// How many asks `asks` lists, the newest.
const LISTED: usize = 20;
/// How much of an ask's text `asks` shows, in characters.
const TEXT_SHOWN: usize = 60;

const NOT_A_PEER: &str = "ask works from a session or moa; type into the session yourself";
const YOURSELF: &str = "you can't ask yourself";
const EMPTY: &str = "an ask can't be empty";
const BAD_WAIT: &str = "a timeout is 1 to 3600 seconds";

/// What a waiting request came for: the ask itself, or a `reply` that waits again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Waits {
    Ask,
    Reply,
}

/// A request that waits for ask `ask`'s answer until `until`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Parked {
    pub conn: ConnId,
    pub req: ReqId,
    pub ask: AskId,
    pub until: UtcMs,
    pub kind: Waits,
}

/// The asks' memory: the requests that wait, oldest first, and the deadline the `Asks` timer
/// is armed at. Kept in memory: a daemon restart drops the connections they came on.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AsksMem {
    pub parked: Vec<Parked>,
    pub armed: Option<UtcMs>,
}

/// Whether an ask with `status` is still open: not answered, failed or dropped.
pub fn is_open(status: AskStatus) -> bool {
    matches!(
        status,
        AskStatus::Pending | AskStatus::Sent | AskStatus::Received
    )
}

/// `text` as an ask saves it (A2): every control character but `\n` and `\t` gone, then
/// trimmed.
pub fn clean(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !is_unpastable(*c)).collect();
    kept.trim().to_string()
}

// ---------- ask and reply ----------

/// `ask TO TEXT` (A2): saves a `pending` ask from the caller and waits for its answer, up to
/// `timeout_s` seconds ([`run`]). A refusal is answered at once and saves nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ask(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    caller: &Caller,
    to: &str,
    text: &str,
    timeout_s: i64,
) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    match save(state, &mut tx, caller, to, text, timeout_s) {
        Ok(id) => {
            let fx = post_commit(state, tx);
            park(state, conn, req, id, wait_until(now, timeout_s), Waits::Ask);
            fx
        }
        Err(e) => finish_op(state, tx, conn, req, Err(e)),
    }
}

/// Checks an ask, in A2's order, then commits its row.
fn save(
    state: &mut State,
    tx: &mut Tx,
    caller: &Caller,
    to: &str,
    text: &str,
    timeout_s: i64,
) -> Result<AskId, OpError> {
    let from = target_of(caller).ok_or_else(|| OpError(NOT_A_PEER.into()))?;
    let text = clean(text);
    if text.is_empty() {
        return Err(OpError(EMPTY.into()));
    }
    if text.chars().count() > ASK_TEXT_MAX {
        return Err(OpError(format!(
            "an ask is at most {ASK_TEXT_MAX} characters; point to a file instead"
        )));
    }
    check_wait(timeout_s)?;
    let target = state.peer(to)?;
    if target == from {
        return Err(OpError(YOURSELF.into()));
    }
    if let AskTarget::Session(id) = target
        && !state.is_live(id)
    {
        return Err(OpError(format!(
            "{} isn't running; `moactl resume {}` starts it, then ask again",
            state.label_of(target),
            state.handle_of(id)
        )));
    }
    // Two agents each waiting on the other would both wait out their whole wait.
    if let Some(theirs) = state
        .asks()
        .find(|a| a.from_session == Some(target) && a.to_session == Some(from) && is_open(a.status))
    {
        return Err(OpError(format!(
            "{} is waiting on your answer to ask #{}; answer that first",
            state.label_of(target),
            theirs.id
        )));
    }
    let id = AskId(tx.next_id(state, Table::Ask));
    let row = Ask {
        id,
        at: tx.wall,
        from_session: Some(from),
        from_name: Some(state.label_of(from)),
        to_session: Some(target),
        text,
        status: AskStatus::Pending,
        reply: None,
        claimed_at: None,
        answered_at: None,
    };
    tx.commit(state, vec![Change::Ask(row)])?;
    Ok(id)
}

/// `reply ID` (A4 d): an ask that ended is answered at once; an open one waits again, up to
/// `timeout_s` seconds. Saves nothing.
pub(crate) fn reply(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    id: i64,
    timeout_s: i64,
) -> Vec<Effect> {
    let id = AskId(id);
    let result = match (check_wait(timeout_s), state.ask(id)) {
        (Err(e), _) => Err(e),
        (Ok(()), None) => Err(no_ask(id)),
        (Ok(()), Some(a)) => match answer_of(state, a) {
            Some(result) => result,
            None => {
                park(
                    state,
                    conn,
                    req,
                    id,
                    wait_until(now, timeout_s),
                    Waits::Reply,
                );
                return Vec::new();
            }
        },
    };
    vec![Effect::Reply { conn, req, result }]
}

fn check_wait(timeout_s: i64) -> Result<(), OpError> {
    if (1..=ASK_WAIT_MAX_S).contains(&timeout_s) {
        Ok(())
    } else {
        Err(OpError(BAD_WAIT.into()))
    }
}

fn wait_until(now: &Now, timeout_s: i64) -> UtcMs {
    UtcMs(now.wall.0 + timeout_s * 1_000)
}

fn park(state: &mut State, conn: ConnId, req: ReqId, ask: AskId, until: UtcMs, kind: Waits) {
    state.asks_mem.parked.push(Parked {
        conn,
        req,
        ask,
        until,
        kind,
    });
}

fn no_ask(id: AskId) -> OpError {
    OpError(format!("no ask #{id}"))
}

// ---------- ending ----------

/// Ends open ask `id` as `status` (`answered`, `failed` or `dropped`) with `reply`: the full
/// answer, or the reason it failed or was dropped. **The one place an ask ends**: delivery
/// and receipts (in a pane) and asks to moa end theirs only through here, so that every
/// request waiting for the ask is answered once, with what its row says.
///
/// The row goes in `tx`'s commit (with whatever else the caller commits there); the replies
/// to the waiting requests are returned, for the caller to add after that commit's effects
/// ([`fold_in`] after [`post_commit`]). An ask already ended, or a refused commit, ends
/// nothing and replies nothing: the requests go on waiting.
pub(crate) fn end(
    state: &mut State,
    tx: &mut Tx,
    id: AskId,
    status: AskStatus,
    reply: String,
) -> Vec<Effect> {
    debug_assert!(!is_open(status), "an ask ends answered, failed or dropped");
    let Some(row) = state.ask(id).filter(|a| is_open(a.status)) else {
        return Vec::new();
    };
    let ended = Ask {
        status,
        reply: Some(reply),
        answered_at: Some(tx.wall),
        ..row.clone()
    };
    if tx.commit(state, vec![Change::Ask(ended)]).is_err() {
        return vec![log(
            LogLevel::Warn,
            format!("ask #{id} wasn't saved as {status}; it stays open"),
        )];
    }
    answer_waiting(state, |p| p.ask == id)
}

/// The waiting requests `which` picks, each answered with what its ask's row says (an open
/// ask: "no reply yet"; one that is gone: `no ask #N`), and forgotten.
fn answer_waiting(state: &mut State, which: impl Fn(&Parked) -> bool) -> Vec<Effect> {
    let (done, waiting): (Vec<Parked>, Vec<Parked>) = std::mem::take(&mut state.asks_mem.parked)
        .into_iter()
        .partition(|p| which(p));
    state.asks_mem.parked = waiting;
    done.into_iter()
        .map(|p| {
            let result = match state.ask(p.ask) {
                Some(a) => answer_of(state, a).unwrap_or_else(|| Ok(no_reply_yet(state, a))),
                None => Err(no_ask(p.ask)),
            };
            Effect::Reply {
                conn: p.conn,
                req: p.req,
                result,
            }
        })
        .collect()
}

/// What the asker hears of an ask that ended (A13), or `None` while it is open: the full
/// answer, or why it failed or was dropped.
fn answer_of(state: &State, a: &Ask) -> Option<Result<Done, OpError>> {
    let reason = a.reply.as_deref().unwrap_or("");
    Some(match a.status {
        AskStatus::Pending | AskStatus::Sent | AskStatus::Received => return None,
        AskStatus::Answered => Ok(Done {
            text: reason.to_string(),
            data: json!({
                "id": a.id,
                "from": from_label(state, a),
                "to": to_label(state, a),
                "status": a.status,
            }),
        }),
        AskStatus::Failed => Err(OpError(format!("ask #{} failed: {reason}", a.id))),
        AskStatus::Dropped if reason == STAYED_BUSY => {
            let ended = a.answered_at.unwrap_or(a.at);
            Err(OpError(format!(
                "{} stayed busy for {}; ask #{} was dropped",
                to_label(state, a),
                wait_text(ended.0 - a.at.0),
                a.id
            )))
        }
        AskStatus::Dropped => Err(OpError(format!("{reason}; ask #{} was dropped", a.id))),
    })
}

/// How long an ask waited, as its texts say it: `{n}s` under a minute (a wait can be as
/// short as 1 s), else [`span`]'s `{m}m`, `{h}h` or `{d}d`.
fn wait_text(ms: i64) -> String {
    if ms < MINUTE {
        format!("{}s", ms.max(0) / 1_000)
    } else {
        span(ms)
    }
}

/// What a wait that runs out on an open ask hears: it exits 0, and `reply` waits again.
fn no_reply_yet(state: &State, a: &Ask) -> Done {
    Done {
        text: format!(
            "no reply yet from {}; `moactl reply {}` waits again",
            to_label(state, a),
            a.id
        ),
        data: json!({ "id": a.id, "status": a.status }),
    }
}

fn from_label(state: &State, a: &Ask) -> String {
    match (&a.from_name, a.from_session) {
        (Some(name), _) => name.clone(),
        (None, Some(from)) => state.label_of(from),
        (None, None) => "-".into(),
    }
}

fn to_label(state: &State, a: &Ask) -> String {
    a.to_session
        .map_or_else(|| "-".into(), |to| state.label_of(to))
}

// ---------- every input ----------

/// At the end of every input: a request waiting on an ask that has ended is answered (its
/// row changed by a reload, say). When the ask's own wait (its [`Waits::Ask`] request) runs
/// out on a `pending` ask, the ask is dropped (`stayed busy`, A4 b), which answers every
/// request on it. Any other wait that runs out, a `reply`'s, or one on an ask already sent,
/// hears "no reply yet", which exits 0, and the ask stays as it is. Then the `Asks` timer is
/// armed at the next deadline ([`deadline`]), or cancelled, when that changed or when this
/// input is the timer firing (`fired`).
///
/// A `pending` ask whose asker went away ([`client_gone`]) has no wait left to drop it: it
/// stays `pending`, is delivered when its peer is free, and its answer is saved for `reply`.
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    if fired {
        state.asks_mem.armed = None;
    }
    let ended: BTreeSet<AskId> = state
        .asks_mem
        .parked
        .iter()
        .map(|p| p.ask)
        .filter(|id| state.ask(*id).is_none_or(|a| !is_open(a.status)))
        .collect();
    if !ended.is_empty() {
        let more = answer_waiting(state, |p| ended.contains(&p.ask));
        fold_in(fx, more);
    }
    let ran_out = |p: &Parked| p.until <= now.wall;
    // Only the ask's own wait drops it: a `reply` that runs out says no more than that.
    let to_drop: BTreeSet<AskId> = state
        .asks_mem
        .parked
        .iter()
        .filter(|p| ran_out(p) && p.kind == Waits::Ask)
        .map(|p| p.ask)
        .collect();
    for id in to_drop {
        if state
            .ask(id)
            .is_some_and(|a| a.status == AskStatus::Pending)
        {
            let mut tx = Tx::new(now);
            let replies = end(state, &mut tx, id, AskStatus::Dropped, STAYED_BUSY.into());
            fold_in(fx, post_commit(state, tx));
            fold_in(fx, replies);
        }
    }
    // The rest ran out on an ask still open (sent, received, pending under a `reply`'s
    // wait, or a drop that wasn't saved).
    let more = answer_waiting(state, ran_out);
    fold_in(fx, more);
    let next = deadline(state);
    fx.extend(rearm(
        &mut state.asks_mem.armed,
        TimerKey::Asks,
        next,
        fired,
    ));
}

/// The asks' next deadline: the earliest wait to run out. One place, so each new deadline
/// (an Enter to send, a receipt to wait for) joins the same `min`.
fn deadline(state: &State) -> Option<UtcMs> {
    state.asks_mem.parked.iter().map(|p| p.until).min()
}

/// Connection `conn` is gone: its waiting requests are dropped, and nothing else. Their asks
/// go on, and their answers are saved.
pub(crate) fn client_gone(state: &mut State, conn: ConnId) {
    state.asks_mem.parked.retain(|p| p.conn != conn);
}

// ---------- peers and asks ----------

/// `peers` (A11): moa first, then every live session but the caller, by label. A line each:
/// `handle label state folder last-asked`.
pub(crate) fn peers(state: &State, caller: &Caller, now: &Now) -> Result<Done, OpError> {
    let me = target_of(caller);
    let mut lines = Vec::new();
    let mut data = Vec::new();
    if me != Some(AskTarget::Moa) {
        let word = moa_state(state);
        let last = last_asked(state, AskTarget::Moa);
        lines.push(peer_line("moa", "moa", word, "-", last, now));
        data.push(json!({
            "id": "moa",
            "handle": "moa",
            "label": "moa",
            "state": word,
            "folder": Value::Null,
            "last_asked": last,
            "agent_kind": Value::Null,
            "project": Value::Null,
            "cwd": Value::Null,
            "doing": Value::Null,
        }));
    }
    let mut live: Vec<(String, &Session)> = state
        .sessions()
        .filter(|s| state.is_live(s.id) && me != Some(AskTarget::Session(s.id)))
        .map(|s| (state.label_of(AskTarget::Session(s.id)), s))
        .collect();
    live.sort_by_key(|(label, s)| (label.to_lowercase(), s.id));
    let deps = state.deps();
    for (label, s) in live {
        let target = AskTarget::Session(s.id);
        let handle = state.handle_of(s.id);
        let word = spike_state(s);
        let folder = s
            .cwd
            .as_deref()
            .map_or_else(|| "-".into(), |c| with_tilde(c, &deps.home, deps.canonical));
        let last = last_asked(state, target);
        lines.push(peer_line(&handle, &label, word, &folder, last, now));
        let (_, why) = display_state(s, now.wall);
        let project = s
            .project_id
            .and_then(|p| state.projects.get(&p))
            .map(|p| p.name.clone());
        data.push(json!({
            "id": s.id,
            "handle": handle,
            "label": label,
            "state": word,
            "folder": folder,
            "last_asked": last,
            "agent_kind": s.agent_kind,
            "project": project,
            "cwd": s.cwd,
            "doing": (!why.is_empty()).then_some(why),
        }));
    }
    let text = if lines.is_empty() {
        "no live sessions to ask".to_string()
    } else {
        lines.join("\n")
    };
    Ok(Done {
        text,
        data: Value::Array(data),
    })
}

fn peer_line(
    handle: &str,
    label: &str,
    word: &str,
    folder: &str,
    last: Option<UtcMs>,
    now: &Now,
) -> String {
    let last = last.map_or_else(
        || "-".into(),
        |at| format!("asked {}", ago(now.wall.0 - at.0)),
    );
    format!("{handle:<8}  {label:<20} {word:<9} {folder:<24} {last}")
}

/// moa's state as `peers` says it: `busy` while it starts or answers, `ready` when it can
/// take a prompt, `off` when its agent isn't running (an ask starts it).
fn moa_state(state: &State) -> &'static str {
    use crate::central::Phase;
    let moa = state.central();
    if moa.busy() {
        return "busy";
    }
    match moa.phase {
        Phase::Ready => "ready",
        Phase::Starting | Phase::Thinking => "busy",
        Phase::Off | Phase::Failed(_) => "off",
    }
}

/// When the newest ask to `who` was saved.
fn last_asked(state: &State, who: AskTarget) -> Option<UtcMs> {
    state
        .asks()
        .filter(|a| a.to_session == Some(who))
        .map(|a| a.at)
        .max()
}

/// `asks` (A11): the caller's asks, from or to it (every ask for a caller that is neither a
/// session nor moa), newest first, at most [`LISTED`]. A line each:
/// `#N  from → to  status  <span> ago  <text>`.
pub(crate) fn list(state: &State, caller: &Caller, now: &Now) -> Result<Done, OpError> {
    let me = target_of(caller);
    let mut mine: Vec<&Ask> = state
        .asks()
        .filter(|a| me.is_none() || a.from_session == me || a.to_session == me)
        .collect();
    mine.sort_by_key(|a| std::cmp::Reverse((a.at, a.id)));
    mine.truncate(LISTED);
    let lines: Vec<String> = mine
        .iter()
        .map(|a| {
            format!(
                "#{}  {} → {}  {}  {}  {}",
                a.id,
                from_label(state, a),
                to_label(state, a),
                a.status,
                ago(now.wall.0 - a.at.0),
                cut(&one_line(&a.text), TEXT_SHOWN)
            )
        })
        .collect();
    let text = if lines.is_empty() {
        "no asks yet".to_string()
    } else {
        lines.join("\n")
    };
    Ok(Done {
        text,
        data: serde_json::to_value(&mine).unwrap_or(Value::Null),
    })
}

/// Who the caller is as an ask names it: `None` for a caller that is neither moa nor a
/// session.
fn target_of(caller: &Caller) -> Option<AskTarget> {
    match caller {
        Caller::Moa => Some(AskTarget::Moa),
        Caller::Session(id, _) => Some(AskTarget::Session(*id)),
        Caller::Unknown { .. } => None,
    }
}

/// `<span> ago`, or `now` under a minute.
fn ago(ms: i64) -> String {
    match span(ms).as_str() {
        "now" => "now".into(),
        s => format!("{s} ago"),
    }
}

#[cfg(test)]
mod tests;
