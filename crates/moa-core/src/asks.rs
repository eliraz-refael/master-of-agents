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
//!
//! An ask to a session moves at the end of every input ([`run`]): the oldest `pending` one to
//! a free peer is claimed (`sent`) and typed into its tab, its Enter half a second later. The
//! marker in the peer's next prompt is its receipt (`received`, [`receipt`]), and the full
//! message that ends that turn its answer ([`answer`]); a turn that ends any other way fails
//! it ([`hooked`], all in the hook's own commit). A typed ask with no receipt in 20 s fails;
//! a peer that stops running ends its asks ([`peer_ended`], [`end_unreachable`]).
//!
//! An ask to moa goes through moa's channel instead (A9): the oldest `pending` one is a hidden
//! prompt of its own ([`for_moa`]), and that turn's end is its answer ([`moa_ended`]); one moa
//! took whose turn is gone fails ([`MOA_RESTARTED`]). While moa waits on an ask of its own, its
//! watchdog holds ([`moa_waits`]).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::central::channel::Source;
use crate::change::Change;
use crate::core::{Tx, finish_op, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, TimerKey, rearm};
use crate::hook::{HookKind, StartSource};
use crate::live::{display_state, spike_state};
use crate::model::{Ask, AskId, AskStatus, AskTarget, Session, SessionId};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::ops::term::with_tilde;
use crate::state::{Caller, State, Table};
use crate::text::{cut, is_unpastable, one_line};
use crate::time::{MINUTE, span};
use crate::tmux::cmd::{Cmd, is_digits};
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::PaneId;
use crate::tmux::link::log;
use crate::{Now, UtcMs};

/// How long an ask's text is at most, in characters, once cleaned.
pub const ASK_TEXT_MAX: usize = 8000;
/// How long `ask` and `reply` wait for the answer when they aren't told: 9 minutes. A
/// session's Bash tool kills a command at 2 minutes unless it is given a timeout (10 minutes
/// at most), which is why the peers note says to run `moactl ask` with a Bash timeout of
/// 600000 ms. An ask whose own wait moa no longer remembers (a restart) waits this long from
/// when it was saved.
pub const ASK_WAIT_DEFAULT_MS: i64 = 540_000;
/// How long after an ask is pasted into a pane its Enter goes.
pub const ASK_ENTER_MS: i64 = 500;
/// How long after it is typed an ask waits for the agent to take it before it fails.
pub const ASK_RECEIPT_MS: i64 = 20_000;
/// The longest wait `ask` and `reply` take, in seconds.
pub const ASK_WAIT_MAX_S: i64 = 3600;

/// The reason in a `dropped` row whose asker's wait ran out before the peer was free.
pub const STAYED_BUSY: &str = "stayed busy";
/// The reason in a `failed` row typed into a pane that sent no receipt in
/// [`ASK_RECEIPT_MS`]. A receipt that comes later still takes it ([`receipt`]).
pub const NO_RECEIPT: &str = "no receipt (a dialog or copy mode had the pane)";
/// The reason in a `failed` row still in flight [`STALE_AFTER_MS`] after it was claimed,
/// when its peer could take the next ask.
pub const STALE: &str = "stale";
/// An ask in flight this long after its claim is stale: twice the longest default wait.
pub const STALE_AFTER_MS: i64 = 2 * ASK_WAIT_DEFAULT_MS;
/// The answer of a turn that ended with an empty message.
pub const NO_TEXT: &str = "(no text)";
/// The reason in a `failed` ask moa took whose turn is gone: moa's agent went, or was
/// restarted, or the daemon was (A9 e).
pub const MOA_RESTARTED: &str = "moa restarted before answering";
/// The `what` of the card an ask to moa writes (A9 c).
pub const CARD: &str = "ask";

/// How an ask's marker starts, and what comes before its number (A13). A label holds no `]`,
/// which ends the marker.
const MARKER_HEAD: &str = "[moa · message from ";
const MARKER_ASK: &str = " · ask #";

/// How many asks `asks` lists, the newest.
const LISTED: usize = 20;
/// How much of an ask's text `asks` shows, in characters.
const TEXT_SHOWN: usize = 60;

const NOT_A_PEER: &str = "ask works from a session or moa; type into the session yourself";
const YOURSELF: &str = "you can't ask yourself";
const EMPTY: &str = "an ask can't be empty";
const BAD_WAIT: &str = "a timeout is 1 to 3600 seconds";

/// A request (an `ask`, or a `reply` that waits again) that waits for ask `ask`'s answer
/// until `until`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Parked {
    pub conn: ConnId,
    pub req: ReqId,
    pub ask: AskId,
    pub until: UtcMs,
}

/// The asks' memory, kept in memory only:
///
/// - `parked`: the requests that wait, oldest first (a restart drops the connections they
///   came on);
/// - `waits`: each `pending` ask's own wait, when it is dropped (`stayed busy`) whoever still
///   waits for it; an ask a restart forgot waits [`ASK_WAIT_DEFAULT_MS`] from when it was
///   saved;
/// - `enter`: the Enters due for asks just pasted, the tab and when (one a restart loses
///   leaves its ask to fail for want of a receipt);
/// - `unheard`: the sessions whose last ask got no receipt, held back from the next until moa
///   hears from them again (a restart forgets them);
/// - `armed`: the deadline the `Asks` timer is armed at.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AsksMem {
    pub parked: Vec<Parked>,
    pub waits: BTreeMap<AskId, UtcMs>,
    pub enter: BTreeMap<AskId, (PaneId, UtcMs)>,
    pub unheard: BTreeSet<SessionId>,
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
/// `timeout_s` seconds ([`run`]); still `pending` then, the ask is dropped, whether or not its
/// asker still waits. A refusal is answered at once and saves nothing.
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
            let until = wait_until(now, timeout_s);
            state.asks_mem.waits.insert(id, until);
            park(state, conn, req, id, until);
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
    if let Some(theirs) = open_ask(state, target, from) {
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
                park(state, conn, req, id, wait_until(now, timeout_s));
                return Vec::new();
            }
        },
    };
    vec![Effect::Reply { conn, req, result }]
}

/// An open ask from `from` to `to`, if there is one.
fn open_ask(state: &State, from: AskTarget, to: AskTarget) -> Option<&Ask> {
    state
        .asks()
        .find(|a| a.from_session == Some(from) && a.to_session == Some(to) && is_open(a.status))
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

fn park(state: &mut State, conn: ConnId, req: ReqId, ask: AskId, until: UtcMs) {
    state.asks_mem.parked.push(Parked {
        conn,
        req,
        ask,
        until,
    });
}

fn no_ask(id: AskId) -> OpError {
    OpError(format!("no ask #{id}"))
}

// ---------- ending ----------

/// Ends open ask `id` as `status` (`answered`, `failed` or `dropped`) with `reply`: the full
/// answer, or the reason it failed or was dropped. **The one place an ask ends**: every
/// ending is [`ending`]'s row, and every request waiting for the ask is answered once, with
/// what its row says ([`answer_waiting`]).
///
/// The row goes in `tx` (with whatever else the caller commits there, before or after: a
/// claim's stale asks go in the claim's own commit); the replies to the waiting requests are
/// returned, for the caller to add after that commit's effects ([`fold_in`] after
/// [`post_commit`]). An ask already ended, or a refused commit, ends nothing and replies
/// nothing: the requests go on waiting.
///
/// An ending that a hook or a tab's exit brings ([`answer`], [`peer_ended`]) is the same row,
/// in that event's own commit; its requests hear at the end of the same input, from [`run`].
pub(crate) fn end(
    state: &mut State,
    tx: &mut Tx,
    id: AskId,
    status: AskStatus,
    reply: String,
) -> Vec<Effect> {
    let Some(ended) = ending(state, id, status, reply, tx.wall) else {
        return Vec::new();
    };
    if tx.commit(state, vec![Change::Ask(ended)]).is_err() {
        return vec![log(
            LogLevel::Warn,
            format!("ask #{id} wasn't saved as {status}; it stays open"),
        )];
    }
    answer_waiting(state, |p| p.ask == id)
}

/// Open ask `id` ended at `at` as `status` with `reply` ([`end`]), or `None` when it has
/// ended already.
fn ending(state: &State, id: AskId, status: AskStatus, reply: String, at: UtcMs) -> Option<Ask> {
    debug_assert!(!is_open(status), "an ask ends answered, failed or dropped");
    let row = state.ask(id).filter(|a| is_open(a.status))?;
    Some(Ask {
        status,
        reply: Some(reply),
        answered_at: Some(at),
        ..row.clone()
    })
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

/// At the end of every input, in this order:
///
/// - a request waiting on an ask that has ended is answered (its row ended by a hook or a
///   tab's exit in this input, or changed by a reload);
/// - every open ask to a session that isn't live ends, while the link is up
///   ([`end_unreachable`]), and every ask moa took whose turn is gone fails
///   ([`fail_orphans`]);
/// - an Enter that is due goes to its tab, while its ask is still `sent` there;
/// - a `sent` ask with no receipt [`ASK_RECEIPT_MS`] after its claim fails ([`NO_RECEIPT`]),
///   and holds its peer back until moa hears from it again ([`heard_from`]);
/// - a `pending` ask whose own wait ran out ([`AsksMem`]'s `waits`) is dropped (`stayed
///   busy`, A4 b), which answers every request on it, even when nobody waits for it any more
///   (R3, amended: an asker whose call was killed mustn't have its question typed hours
///   later). Any other wait that runs out, a `reply`'s, or one on an ask already sent, hears
///   "no reply yet", which exits 0, and the ask stays as it is: a `reply` never drops one;
/// - each session's oldest `pending` ask is delivered once the session is free ([`claim`]);
/// - the `Asks` timer is armed at the next deadline ([`deadline`]), or cancelled, when that
///   changed or when this input is the timer firing (`fired`).
///
/// Asks to moa are its channel's to deliver ([`for_moa`]). While writes are refused nothing fails, nothing
/// is dropped and nothing is claimed; what fell due meanwhile acts once writes are back
/// ([`deadline`]).
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
    let writes = !state.writes_refused(now.wall);
    if writes {
        end_unreachable(state, now, fx);
        fail_orphans(state, now, fx);
    }
    press_enters(state, now.wall, fx);
    if writes {
        fail_unreceived(state, now, fx);
    }
    if writes {
        drop_waited_out(state, now, fx);
    }
    // The rest ran out on an ask still open (sent, received, or pending under a `reply`'s
    // wait).
    let ran_out = |p: &Parked| p.until <= now.wall;
    let more = answer_waiting(state, ran_out);
    fold_in(fx, more);
    if writes {
        for (id, to) in next_in_line(state) {
            claim(state, now, id, to, fx);
        }
    }
    let next = deadline(state, now.wall);
    fx.extend(rearm(
        &mut state.asks_mem.armed,
        TimerKey::Asks,
        next,
        fired,
    ));
}

/// The asks' next deadline still ahead: the earliest request's wait to run out, Enter to
/// send, `pending` ask to drop, receipt to wait for ([`ASK_RECEIPT_MS`]), or ask in flight
/// to go stale while another waits behind it ([`STALE_AFTER_MS`]). The last three need a
/// write: one that fell due while writes are refused is due again when they come back, so
/// it is never lost; with writes allowed, [`run`] acted on it already.
fn deadline(state: &State, now: UtcMs) -> Option<UtcMs> {
    let waits = state.asks_mem.parked.iter().map(|p| p.until);
    let enters = state.asks_mem.enter.values().map(|(_, at)| *at);
    let drops = state
        .asks()
        .filter(|a| a.status == AskStatus::Pending)
        .map(|a| drop_at(state, a));
    let receipts = state
        .asks()
        .filter(|a| a.status == AskStatus::Sent)
        .filter_map(|a| after_claim(a, ASK_RECEIPT_MS));
    let stale = next_in_line(state)
        .into_iter()
        .flat_map(|(_, to)| state.in_flight_to(to))
        .filter_map(|a| after_claim(a, STALE_AFTER_MS));
    let back = state
        .refuse_writes()
        .map(|r| r.until)
        .filter(|until| now < *until);
    let written = drops.chain(receipts).chain(stale).map(|at| match back {
        Some(back) if at <= now => back,
        _ => at,
    });
    waits
        .chain(enters)
        .chain(written)
        .filter(|at| *at > now)
        .min()
}

/// When `pending` ask `a` is dropped: at its own wait, or [`ASK_WAIT_DEFAULT_MS`] after it
/// was saved when moa no longer remembers that (a restart).
fn drop_at(state: &State, a: &Ask) -> UtcMs {
    state
        .asks_mem
        .waits
        .get(&a.id)
        .copied()
        .unwrap_or(UtcMs(a.at.0 + ASK_WAIT_DEFAULT_MS))
}

/// Every `pending` ask whose own wait ran out is dropped (`stayed busy`), each in a commit of
/// its own, answering whoever still waits for it. The waits of asks no longer pending are
/// forgotten.
fn drop_waited_out(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let due: Vec<AskId> = state
        .asks()
        .filter(|a| a.status == AskStatus::Pending && drop_at(state, a) <= now.wall)
        .map(|a| a.id)
        .collect();
    for id in due {
        let mut tx = Tx::new(now);
        let replies = end(state, &mut tx, id, AskStatus::Dropped, STAYED_BUSY.into());
        fold_in(fx, post_commit(state, tx));
        fold_in(fx, replies);
    }
    let asks = &state.asks;
    state
        .asks_mem
        .waits
        .retain(|id, _| asks.get(id).is_some_and(|a| a.status == AskStatus::Pending));
}

/// `ms` after ask `a` was claimed, for an ask to a session.
fn after_claim(a: &Ask, ms: i64) -> Option<UtcMs> {
    let to_a_session = matches!(a.to_session, Some(AskTarget::Session(_)));
    a.claimed_at
        .filter(|_| to_a_session)
        .map(|at| UtcMs(at.0 + ms))
}

/// Each session's oldest `pending` ask, oldest first: the next each takes.
fn next_in_line(state: &State) -> Vec<(AskId, SessionId)> {
    let mut pending: Vec<&Ask> = state
        .asks()
        .filter(|a| a.status == AskStatus::Pending)
        .collect();
    pending.sort_by_key(|a| (a.at, a.id));
    let mut seen = BTreeSet::new();
    pending
        .into_iter()
        .filter_map(|a| match a.to_session {
            Some(AskTarget::Session(to)) if seen.insert(to) => Some((a.id, to)),
            _ => None,
        })
        .collect()
}

/// Claims `pending` ask `id` for session `to` and types it, when `to` is free once its stale
/// asks fail ([`State::is_free_once`], A5): in one commit they fail ([`STALE`]) and the ask
/// turns `sent`, claimed now; then its text goes into a buffer and is pasted into `to`'s
/// tab, and its Enter is due [`ASK_ENTER_MS`] later. Only with the link up; a refused commit
/// types nothing.
fn claim(state: &mut State, now: &Now, id: AskId, to: SessionId, fx: &mut Vec<Effect>) {
    let Some(pane) = bound_pane(state, to) else {
        return;
    };
    let is_stale = |a: &Ask| after_claim(a, STALE_AFTER_MS).is_some_and(|at| at <= now.wall);
    if !state.is_free_once(to, is_stale) {
        return;
    }
    let stale: Vec<AskId> = state.in_flight_to(to).map(|a| a.id).collect();
    let Some(row) = state.ask(id).cloned() else {
        return;
    };
    let mut tx = Tx::new(now);
    let mut replies = Vec::new();
    for old in stale {
        replies.extend(end(state, &mut tx, old, AskStatus::Failed, STALE.into()));
    }
    let claimed = Ask {
        status: AskStatus::Sent,
        claimed_at: Some(now.wall),
        ..row
    };
    let text = framed(state, &claimed);
    if tx.commit(state, vec![Change::Ask(claimed)]).is_ok() {
        let buffer = format!("moa-ask-{id}");
        tx.tmux(Cmd::set_buffer(&buffer, &text));
        tx.tmux(Cmd::paste_buffer(&buffer, pane));
        let due = UtcMs(now.wall.0 + ASK_ENTER_MS);
        state.asks_mem.enter.insert(id, (pane, due));
    } else {
        fx.push(log(
            LogLevel::Warn,
            format!("ask #{id} wasn't saved as sent; it waits"),
        ));
    }
    fold_in(fx, post_commit(state, tx));
    fold_in(fx, replies);
}

/// The Enters due by `now`, each to its tab while its ask is still `sent` there; the rest are
/// forgotten.
fn press_enters(state: &mut State, now: UtcMs, fx: &mut Vec<Effect>) {
    let due: Vec<(AskId, PaneId)> = state
        .asks_mem
        .enter
        .iter()
        .filter(|(_, (_, at))| *at <= now)
        .map(|(id, (pane, _))| (*id, *pane))
        .collect();
    let mut cmds = Vec::new();
    for (id, pane) in due {
        state.asks_mem.enter.remove(&id);
        let there = state
            .ask(id)
            .filter(|a| a.status == AskStatus::Sent)
            .and_then(|a| match a.to_session {
                Some(AskTarget::Session(to)) => bound_pane(state, to),
                _ => None,
            });
        if there == Some(pane) {
            cmds.push(Cmd::enter(pane));
        }
    }
    if let (false, Some(conn)) = (cmds.is_empty(), state.tmux().conn()) {
        fx.push(Effect::Tmux {
            conn,
            cmds,
            token: None,
        });
    }
}

/// Every ask to a session typed [`ASK_RECEIPT_MS`] ago or more with no receipt fails
/// ([`NO_RECEIPT`]).
fn fail_unreceived(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let due: Vec<AskId> = state
        .asks()
        .filter(|a| a.status == AskStatus::Sent)
        .filter(|a| after_claim(a, ASK_RECEIPT_MS).is_some_and(|at| at <= now.wall))
        .map(|a| a.id)
        .collect();
    for id in due {
        state.asks_mem.enter.remove(&id);
        let to = state.ask(id).and_then(|a| a.to_session);
        let mut tx = Tx::new(now);
        let replies = end(state, &mut tx, id, AskStatus::Failed, NO_RECEIPT.into());
        if let (false, Some(AskTarget::Session(to))) = (tx.changes.is_empty(), to) {
            state.asks_mem.unheard.insert(to);
        }
        fold_in(fx, post_commit(state, tx));
        fold_in(fx, replies);
    }
}

/// moa hears from session `id` (any hook event that is its): an ask that got no receipt
/// there no longer holds it back ([`State::is_free`]).
pub(crate) fn heard_from(state: &mut State, id: SessionId) {
    state.asks_mem.unheard.remove(&id);
}

// ---------- in a pane ----------

/// What ask `a` is typed into its peer's tab as (A13): its marker, its text, and whose the
/// answer is.
pub fn framed(state: &State, a: &Ask) -> String {
    let from = from_label(state, a);
    format!(
        "{MARKER_HEAD}{from}{MARKER_ASK}{}] {}\n\n(Your reply this turn goes back to {from} as the answer.)",
        a.id, a.text
    )
}

/// The ask the first marker in `prompt` names, anywhere in it (A6 a):
/// `[moa · message from <label> · ask #<N>]`.
pub fn marker_id(prompt: &str) -> Option<AskId> {
    marker_ids(prompt).next()
}

/// The asks the markers in `prompt` name, in order.
fn marker_ids(prompt: &str) -> impl Iterator<Item = AskId> + '_ {
    prompt.match_indices(MARKER_HEAD).filter_map(|(at, _)| {
        let rest = &prompt[at + MARKER_HEAD.len()..];
        let inside = &rest[..rest.find(']')?];
        let (_, n) = inside.rsplit_once(MARKER_ASK)?;
        if !is_digits(n) {
            return None;
        }
        n.parse().ok().map(AskId)
    })
}

/// The receipt session `id`'s prompt carries at `at` (A6 b): the ask to `id` its marker names
/// turns `received` when it is `sent`, or when it failed for want of a receipt (the dialog
/// that held the pane closed) while `id` is live, nothing else to `id` is in flight, `id`
/// waits on no ask to its asker, and it isn't stale yet ([`STALE_AFTER_MS`]): one ask in
/// flight per peer, and no two agents waiting on each other, hold for a revived ask too, and
/// a peer that isn't running would only end it again ([`end_unreachable`]). Any other prompt
/// changes no ask.
fn receipt(state: &State, id: SessionId, prompt: &str, at: UtcMs) -> Option<Change> {
    let me = AskTarget::Session(id);
    let named: Vec<&Ask> = marker_ids(prompt)
        .filter_map(|n| state.ask(n))
        .filter(|a| a.to_session == Some(me))
        .collect();
    let sent = named.iter().find(|a| a.status == AskStatus::Sent);
    let row = match sent {
        Some(a) => Ask {
            status: AskStatus::Received,
            ..(*a).clone()
        },
        None if !state.is_live(id) || state.in_flight_to(id).next().is_some() => return None,
        None => {
            let failed = named.iter().find(|a| {
                a.status == AskStatus::Failed
                    && a.reply.as_deref() == Some(NO_RECEIPT)
                    && after_claim(a, STALE_AFTER_MS).is_some_and(|stale| at < stale)
                    && a.from_session
                        .is_none_or(|by| open_ask(state, me, by).is_none())
            })?;
            Ask {
                status: AskStatus::Received,
                reply: None,
                answered_at: None,
                ..(*failed).clone()
            }
        }
    };
    Some(Change::Ask(row))
}

/// The ask session `id` is answering now: the one it received.
pub(crate) fn answering(state: &State, id: SessionId) -> Option<&Ask> {
    state
        .in_flight_to(id)
        .find(|a| a.status == AskStatus::Received)
}

/// The answer a turn of session `id` that ended at `at` with `message` gives (A6 c): the ask
/// it received turns `answered`, its reply the full message ([`NO_TEXT`] for an empty one).
/// A received ask is always its turn's: any other start or end of a turn ends it first
/// ([`hooked`]).
fn answer(state: &State, id: SessionId, message: &str, at: UtcMs) -> Option<Change> {
    let asked = answering(state, id)?.id;
    let reply = if message.trim().is_empty() {
        NO_TEXT.to_string()
    } else {
        message.to_string()
    };
    ending(state, asked, AskStatus::Answered, reply, at).map(Change::Ask)
}

/// The asks a hook of session `id`, applied at `at`, moves, as the hook's own commit saves
/// them ([`ending`]); their requests hear at the end of the input ([`run`]):
///
/// - a turn's start: the ask its marker names is received ([`receipt`]); one received before
///   whose marker it doesn't hold fails, `<label> moved on before answering` (an interrupt,
///   then a prompt of the user's);
/// - a turn's end: the ask received is answered ([`answer`]);
/// - a turn that fails: the ask received fails, `<label>'s turn failed`;
/// - a new conversation in the tab (`/clear`, a resume, a new agent, a fork): the ask
///   received fails, `<label> moved on before answering`. A compaction leaves it: one can
///   come mid-turn, and the turn carries on;
/// - the session's end: every open ask to it ends ([`peer_ended`]).
pub(crate) fn hooked(state: &State, id: SessionId, kind: &HookKind, at: UtcMs) -> Vec<Change> {
    let label = || state.label_of(AskTarget::Session(id));
    let unanswered = |why: String| {
        let a = answering(state, id)?;
        ending(state, a.id, AskStatus::Failed, why, at).map(Change::Ask)
    };
    let moved_on = || unanswered(format!("{} moved on before answering", label()));
    let change = match kind {
        HookKind::TurnStarted { prompt } => match answering(state, id) {
            Some(a) if marker_ids(prompt).any(|n| n == a.id) => None,
            Some(_) => moved_on(),
            None => receipt(state, id, prompt, at),
        },
        HookKind::TurnEnded { last_message } => answer(state, id, last_message, at),
        HookKind::TurnFailed { .. } => unanswered(format!("{}'s turn failed", label())),
        HookKind::SessionStarted { source } if *source != StartSource::Compact => moved_on(),
        HookKind::SessionEnded { .. } => return peer_ended(state, id, at),
        _ => None,
    };
    change.into_iter().collect()
}

/// How each open ask to session `id` ends once the session isn't running (A8): a `pending`
/// one is dropped, `<label> isn't running`; one in flight fails, `<label> ended`. The one
/// wording for every way a peer goes: its end, its tab's exit, and [`run`]'s look at every
/// peer that isn't live (close, sleep, archive, reconcile, …).
fn gone_endings(state: &State, id: SessionId) -> Vec<(AskId, AskStatus, String)> {
    let me = AskTarget::Session(id);
    let label = state.label_of(me);
    state
        .asks()
        .filter(|a| a.to_session == Some(me) && is_open(a.status))
        .map(|a| match a.status {
            AskStatus::Pending => (a.id, AskStatus::Dropped, format!("{label} isn't running")),
            _ => (a.id, AskStatus::Failed, format!("{label} ended")),
        })
        .collect()
}

/// The asks to session `id` that end with it at `at` ([`gone_endings`]), as the commit that
/// ends it, or puts it to sleep when its tab exits, saves them ([`ending`]). Its own asks to
/// others go on.
pub(crate) fn peer_ended(state: &State, id: SessionId, at: UtcMs) -> Vec<Change> {
    gone_endings(state, id)
        .into_iter()
        .filter_map(|(ask, status, why)| ending(state, ask, status, why, at))
        .map(Change::Ask)
        .collect()
}

/// Every open ask to a session that isn't live (A3) ends ([`gone_endings`]), in one commit of
/// its own: the catch-all for every way a peer stops running without a hook or a tab's exit
/// saying so in its own commit. Only while the link is up: before it, a daemon that is
/// starting hasn't looked at its tabs, and a peer may be running still.
fn end_unreachable(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    if state.tmux().conn().is_none() {
        return;
    }
    let gone: BTreeSet<SessionId> = state
        .asks()
        .filter(|a| is_open(a.status))
        .filter_map(|a| match a.to_session {
            Some(AskTarget::Session(to)) if !state.is_live(to) => Some(to),
            _ => None,
        })
        .collect();
    if gone.is_empty() {
        return;
    }
    let mut tx = Tx::new(now);
    let mut replies = Vec::new();
    for to in gone {
        for (ask, status, why) in gone_endings(state, to) {
            replies.extend(end(state, &mut tx, ask, status, why));
        }
    }
    fold_in(fx, post_commit(state, tx));
    fold_in(fx, replies);
}

/// Connection `conn` is gone: its waiting requests are dropped, and nothing else. Their asks
/// go on, and their answers are saved.
pub(crate) fn client_gone(state: &mut State, conn: ConnId) {
    state.asks_mem.parked.retain(|p| p.conn != conn);
}

// ---------- to moa ----------

/// The oldest `pending` ask to moa: the next its channel sends, before any other automatic
/// prompt (A9 a).
pub(crate) fn next_for_moa(state: &State) -> Option<AskId> {
    state
        .asks()
        .filter(|a| a.status == AskStatus::Pending && a.to_session == Some(AskTarget::Moa))
        .min_by_key(|a| (a.at, a.id))
        .map(|a| a.id)
}

/// What moa's channel sends for ask `id` at `at` (A9 b, c), while it is a `pending` ask to
/// moa.
pub(crate) struct ForMoa {
    /// Who asked, by label.
    pub from: String,
    /// The hidden prompt (A13): the ask, and whose the answer is.
    pub prompt: String,
    /// The card's text: `Kite · #12: Is the domain renewed?`.
    pub card: String,
    /// The ask received at `at`, for the prompt's commit.
    pub received: Ask,
}

/// What moa is sent for ask `id` at `at` ([`ForMoa`]); `None` when it is no `pending` ask to
/// moa any more (its wait ran out while moa's thread opened).
pub(crate) fn for_moa(state: &State, id: AskId, at: UtcMs) -> Option<ForMoa> {
    let a = state
        .ask(id)
        .filter(|a| a.status == AskStatus::Pending && a.to_session == Some(AskTarget::Moa))?;
    let from = from_label(state, a);
    let prompt = format!(
        "(moa · {from} asks you, ask #{id}, not typed by the user)\n{}\n\n\
         Your reply this turn goes back to {from} as the answer.",
        a.text
    );
    let card = format!("{from} · #{id}: {}", a.text);
    let received = Ask {
        status: AskStatus::Received,
        claimed_at: Some(at),
        ..a.clone()
    };
    Some(ForMoa {
        from,
        prompt,
        card,
        received,
    })
}

/// moa's turn for ask `id` ended (A9 d): `Ok` with the turn's words answers it ([`NO_TEXT`]
/// for none), `Err` with why the turn ended early or failed fails it. In a commit of its own,
/// then whoever waits for it hears ([`end`]).
pub(crate) fn moa_ended(
    state: &mut State,
    now: &Now,
    id: AskId,
    result: Result<&str, &str>,
) -> Vec<Effect> {
    let (status, reply) = match result {
        Ok(words) if words.trim().is_empty() => (AskStatus::Answered, NO_TEXT.to_string()),
        Ok(words) => (AskStatus::Answered, words.to_string()),
        Err(why) => (AskStatus::Failed, why.to_string()),
    };
    let mut tx = Tx::new(now);
    let replies = end(state, &mut tx, id, status, reply);
    let mut fx = post_commit(state, tx);
    fold_in(&mut fx, replies);
    fx
}

/// Every ask moa took (`sent` or `received`) with no live turn of moa's for it fails
/// ([`MOA_RESTARTED`], A9 e), in one commit of its own: moa's agent went in the middle of its
/// turn, the watchdog restarted it, or the daemon started again. Nothing else would end it.
fn fail_orphans(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let answering = state
        .central()
        .turn
        .as_ref()
        .filter(|t| t.exited.is_none())
        .and_then(|t| match t.source {
            Source::Ask(id) => Some(id),
            _ => None,
        });
    let orphans: Vec<AskId> = state
        .asks()
        .filter(|a| a.to_session == Some(AskTarget::Moa))
        .filter(|a| matches!(a.status, AskStatus::Sent | AskStatus::Received))
        .filter(|a| Some(a.id) != answering)
        .map(|a| a.id)
        .collect();
    if orphans.is_empty() {
        return;
    }
    let mut tx = Tx::new(now);
    let mut replies = Vec::new();
    for id in orphans {
        replies.extend(end(
            state,
            &mut tx,
            id,
            AskStatus::Failed,
            MOA_RESTARTED.into(),
        ));
    }
    fold_in(fx, post_commit(state, tx));
    fold_in(fx, replies);
}

/// Whether moa waits on an ask of its own (A10): a request waits for an open ask from moa
/// (its `moactl ask`, or a `reply` that waits again). Its watchdog holds meanwhile.
pub(crate) fn moa_waits(state: &State) -> bool {
    state.asks_mem.parked.iter().any(|p| {
        state
            .ask(p.ask)
            .is_some_and(|a| a.from_session == Some(AskTarget::Moa) && is_open(a.status))
    })
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
