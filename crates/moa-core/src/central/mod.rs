//! moa's own agent, the central one: the core's side of its conversation. No IO of its own:
//! the files it is told (`prompt`) are read through `Deps::read_text`, as the layout ops do.
//!
//! A prompt starts moa's agent when it isn't running, opens today's thread when it isn't
//! open ([`threads`]), saves the prompt's `you` row, and only then sends it: the prompt waits
//! for the commit. One prompt at a time. What moa says in its turn lands in the feed as it
//! comes, and the turn's end answers whoever waits ([`turn`]). The session reports and your
//! yeses ride on the prompts ([`carry`]), or reach moa on their own ([`channel`]), as the
//! day's opening does ([`opening`]). An agent's ask to moa is a prompt of its own, whose
//! turn's end is the answer ([`crate::asks`]). A turn
//! that hangs is cancelled, then cut short and moa's agent restarted ([`watchdog`]). One that
//! fails for its account's sake says so in a card ([`account`]).

pub mod account;
pub mod carry;
pub mod channel;
pub mod due;
pub mod feed;
pub mod idle;
pub mod launch;
pub mod opening;
pub mod prompt;
pub mod reports;
pub mod threads;
mod turn;
pub mod watchdog;
pub mod wire;

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde_json::json;

use self::channel::Source;
use self::feed::{kind, text_body};
use self::threads::{How, choose, new_thread};
use self::watchdog::{Restart, Restarted, Watchdog};
use self::wire::{AcpCmd, AcpIn, AdapterSpec, Caps, Opened, PermOption, ThreadHow};
use crate::asks;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, Publish};
use crate::model::{
    AskId, FeedEntry, MoaThread, Suggestion, SuggestionId, Task, Terminal, ThreadKind,
};
use crate::op::{ConnId, Done, OpError, PermAnswer, ReqId};
use crate::panel::View;
use crate::publish::{CoreEvent, MoaRow};
use crate::state::{Caller, State};
use crate::tmux::link::log;
use crate::{Now, UtcMs};

/// The longest prompt, in characters.
pub const PROMPT_MAX: usize = 100_000;

const BUSY: &str = "moa is still answering; wait for it, or cancel it";

/// What a prompt hears while the watchdog restarts moa's agent.
const RESTARTING: &str = "moa's agent is restarting; try again in a moment";

/// The first line of a new thread opened because today's couldn't be reopened.
const REOPEN_FAILED: &str = "today's earlier thread couldn't be reopened, so this is a new one";

/// What goes between the parts of a prompt: the brief, what it carries, your text (D1).
const PART_GAP: &str = "\n\n---\n\n";

/// Said to whoever waits for a prompt whose row a failed commit lost.
const UNSAVED: &str = "moa couldn't save your prompt; nothing was sent";

/// Where moa's agent is.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    #[default]
    Off,
    Starting,
    Ready,
    Thinking,
    Failed(String),
}

/// Who sent a prompt, and whether they wait for moa's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Waiter {
    pub conn: ConnId,
    pub req: ReqId,
    pub wait: bool,
}

/// The thread moa's agent has open in this run. Its row is saved with its first prompt:
/// whatever commit carries the row carries what goes first with it, so a commit that is
/// refused, or lost and reloaded, leaves them all to the next prompt.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Open {
    /// Its `moa_threads` row. It is saved when the state has it, as the store does.
    pub row: MoaThread,
    /// The brief its first prompt carries: a new thread opened without a system prompt.
    pub brief: Option<String>,
    /// It was opened because today's earlier thread couldn't be reopened: its first row says
    /// so.
    pub reopen_failed: bool,
}

/// A prompt taken while moa's agent starts or its thread opens. Nothing of it is saved yet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pending {
    /// The core's number for its turn.
    pub prompt: u64,
    /// Its `you` row's `message_id`.
    pub message_id: String,
    /// Your text; an automatic prompt's is made at its commit.
    pub text: String,
    pub source: Source,
    pub waiter: Option<Waiter>,
    /// Today's earlier thread was asked to reopen.
    pub reopening: bool,
    /// It couldn't be: the new thread's first row says so.
    pub reopen_failed: bool,
    /// The brief, for the first prompt of the new thread asked for.
    pub brief: Option<String>,
}

/// A prompt sent, its row saved with it: moa is answering.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Turn {
    pub prompt: u64,
    pub message_id: String,
    pub thread: String,
    pub source: Source,
    /// Who waits for moa's answer. A prompt sent without `wait` was answered once its row
    /// was saved.
    pub waiter: Option<Waiter>,
    /// Why moa's agent went away during the turn. Its end may still come after.
    pub exited: Option<String>,
}

/// A permission request of moa's agent that waits for your answer, by its `feed` row's id.
/// Kept in memory: the run's requests can't outlive its connection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asking {
    /// The request's number, to answer it by.
    pub req: u64,
    pub options: Vec<PermOption>,
}

/// The core's state for moa's agent: its phase, which run of it (`generation`) the core is
/// listening to, the thread it has open, and the prompt under way.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Moa {
    pub phase: Phase,
    /// Every message from another run is dropped.
    pub generation: u64,
    /// The account this run is on.
    pub account: Option<AccountName>,
    /// What this run's agent offers, once it has started.
    pub caps: Option<Caps>,
    pub thread: Option<Open>,
    pub pending: Option<Pending>,
    pub turn: Option<Turn>,
    /// The thread a load is replaying, while its replay is under way: that thread's updates
    /// are checked against the feed's keys.
    pub replaying: Option<String>,
    /// The last prompt's number.
    pub prompts: u64,
    /// The rows the turn under way (or a load's replay) has written, by id, as written,
    /// saved or not: moa's next words extend them.
    pub written: BTreeMap<i64, FeedEntry>,
    /// moa's updates that couldn't be saved since the count was last logged, at a turn's
    /// end.
    pub unsaved: u64,
    /// `console.new` was said, or `console.account` switched: the next prompt opens a new
    /// thread of kind `Manual`, even the same day. Kept in memory; it ends once that thread
    /// is open.
    pub new_thread: bool,
    /// When the last prompt that carried session reports on its own was saved: the next
    /// wake waits a gap after it. Kept in memory.
    pub last_wake: Option<UtcMs>,
    /// Where the idle terminals told on a wake-up end ([`idle::mark`] of the last one told):
    /// the next wake-up tells only those after it. Set only by the commit of a prompt that
    /// carried an idle block, and never moved back (the day's opening tells them all). Kept
    /// in memory.
    pub idle_told: Option<idle::IdleMark>,
    /// The local day the user was here on while it wasn't opened yet ([`opening::present`]):
    /// the next prompt that day opens it, the brief's or the user's own (D3). Cleared by the
    /// commit of the prompt that carries the opening, kept while it is refused, and dropped
    /// once that day has passed: the next day waits for the user again. Kept in memory: `meta`
    /// `moa.briefed` says which day was opened.
    pub brief_due: Option<Date>,
    /// When the last automatic prompt, a brief's, a wake-up's or a yes's, failed to reach
    /// moa's agent:
    /// every automatic source waits a gap after it, rather than starting moa's agent, a thread
    /// or a commit again at once. A prompt that reaches moa's agent ends it. Kept in memory.
    pub failed_at: Option<UtcMs>,
    /// The deadline the `Wake` timer is armed at, if any, as `State::sleep_at` is.
    pub wake_armed: Option<UtcMs>,
    /// The turn's permission requests that wait for your answer, by row id: oldest first.
    pub asking: BTreeMap<i64, Asking>,
    /// The turn's requests you answered, by row id, kept until the turn ends: when the commit
    /// of an answer fails, its answer isn't sent and the reload finds its row still `pending`,
    /// so it waits for you again.
    pub answered: BTreeMap<i64, Asking>,
    /// The watchdog of the turn under way.
    pub watchdog: Watchdog,
    /// The watchdog's restart, until moa's agent has started again and opened its thread:
    /// its end finishes the card. Meanwhile a prompt is refused, and no wake-up goes.
    pub restarting: Option<Restart>,
    /// The restart couldn't reopen today's thread: the next prompt opens a new one, as one
    /// whose reopen failed does ([`Pending::reopen_failed`]).
    pub reopen_failed: bool,
    /// The account whose card ([`account`]) was said: it isn't said again for that account
    /// until a turn ends `end_turn` or `/account` switches. Kept in memory.
    pub account_card: Option<AccountName>,
}

impl Moa {
    /// The `state` row of the `feed` topic: `queued` reports wait, and `auto` says whether
    /// wake-ups read them.
    pub fn row(&self, queued: u32, auto: bool) -> MoaRow {
        let (phase, why) = match &self.phase {
            Phase::Off => ("off", None),
            Phase::Starting => ("starting", None),
            Phase::Ready => ("ready", None),
            Phase::Thinking => ("thinking", None),
            Phase::Failed(why) => ("failed", Some(why.clone())),
        };
        MoaRow {
            phase: phase.into(),
            thread: self.thread.as_ref().map(|o| o.row.thread_id.clone()),
            why,
            queued,
            asking: self.asking.keys().copied().collect(),
            auto,
        }
    }

    /// A prompt now would be one too many: moa is starting, opening its thread, or
    /// answering.
    pub(crate) fn busy(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Thinking) || self.pending.is_some()
    }

    /// moa's agent runs, and has said what it offers.
    fn running(&self) -> bool {
        self.phase == Phase::Ready && self.account.is_some() && self.caps.is_some()
    }
}

/// The one way a prompt reaches moa: `console.prompt`, and moa's own automatic prompts from
/// `source`, which are told apart by the `message_id` it records. An automatic prompt's text
/// is made at its commit, so it is sent with none. `waiter` hears `moa has it` once the
/// prompt's row is saved, or, with `wait`, moa's answer when the turn ends.
pub fn send(
    state: &mut State,
    now: &Now,
    source: Source,
    text: String,
    waiter: Option<Waiter>,
) -> Vec<Effect> {
    let text = text.trim();
    let refusal = match source {
        Source::You if text.is_empty() => Some("there's nothing to send".to_string()),
        Source::You if text.chars().count() > PROMPT_MAX => {
            Some(format!("a prompt is at most {PROMPT_MAX} characters"))
        }
        _ if state.central.restarting.is_some() => Some(RESTARTING.to_string()),
        _ if state.central.busy() => Some(BUSY.to_string()),
        _ => None,
    };
    if let Some(why) = refusal {
        return answer(waiter, Err(OpError(why)));
    }
    // Off, failed or gone: moa's agent starts first, if it can.
    let start = match state.central.running() {
        true => None,
        false => match launch::spec(state) {
            Ok(start) => Some(start),
            Err(e) => return answer(waiter, Err(e)),
        },
    };
    let message_id = (state.deps().new_uuid)(now.wall).to_string();
    let moa = &mut state.central;
    moa.prompts += 1;
    moa.pending = Some(Pending {
        prompt: moa.prompts,
        message_id,
        text: text.to_string(),
        source,
        waiter,
        reopening: false,
        reopen_failed: std::mem::take(&mut moa.reopen_failed),
        brief: None,
    });
    let mut fx = match start {
        Some((account, spec)) => {
            // A new run: nothing of the last one carries over. A turn its agent left
            // unended is over, and whoever still waits for it is told.
            let left = moa.turn.take().and_then(|t| {
                let why = t.exited.unwrap_or_else(|| "its turn never ended".into());
                t.waiter.map(|w| (w, why))
            });
            let mut fx = Vec::new();
            if let Some((w, why)) = left {
                fx.extend(answer(Some(w), Err(OpError(stopped_text(&why)))));
            }
            fx.push(new_run(moa, account, spec));
            fx
        }
        None => open(state, now),
    };
    fx.extend(said(state));
    fx
}

/// A new run of moa's agent on `account`: nothing of the last one carries over. The command
/// that starts it; the host stops the last run first.
fn new_run(moa: &mut Moa, account: AccountName, spec: AdapterSpec) -> Effect {
    moa.generation += 1;
    moa.phase = Phase::Starting;
    moa.account = Some(account);
    forget_run(moa);
    Effect::Acp(AcpCmd::Start {
        generation: moa.generation,
        spec,
    })
}

/// What a run of moa's agent leaves is forgotten: what it offered, its open thread and a
/// replay under way.
fn forget_run(moa: &mut Moa) {
    moa.caps = None;
    moa.thread = None;
    moa.replaying = None;
}

/// What a switch of account hears while moa starts or answers.
const ANSWERING: &str = "moa is answering; cancel it first, then switch";

/// `console.account` (`/account`): with `name`, moa's agent runs on that account from your
/// next prompt, in a new thread. The choice is saved over moa.toml's `moa_account` (removed
/// when it is that one), and a running agent is stopped once it is. Only you switch it: moa's
/// own call is refused, as is a switch while moa starts or answers. With no name, which
/// account moa is on, and the others it can run on.
pub fn switch_account(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    caller: &Caller,
    name: Option<String>,
) -> Vec<Effect> {
    let reply = |result| vec![Effect::Reply { conn, req, result }];
    let Some(name) = name else {
        return reply(accounts(state));
    };
    let refused = |why: String| reply(Err(OpError(why)));
    if *caller == Caller::Moa {
        return refused("only you can switch moa's account".into());
    }
    if state.central.busy() || state.central.restarting.is_some() {
        return refused(ANSWERING.into());
    }
    let runnable = AccountName::parse(&name)
        .ok()
        .filter(|a| launch::spec_on(state, a).is_ok());
    let Some(account) = runnable else {
        return refused(format!("there's no account {name} moa can run on"));
    };
    let data = json!({ "account": account.as_str() });
    if launch::account(state).as_ref() == Some(&account) {
        let text = format!("moa is on {account} already");
        return reply(Ok(Done { text, data }));
    }
    let value = (launch::configured(state).as_ref() != Some(&account)).then(|| account.to_string());
    let key = launch::ACCOUNT_KEY.to_string();
    let mut tx = Tx::new(now);
    if let Err(e) = tx.commit(state, vec![Change::Meta { key, value }]) {
        return reply(Err(e));
    }
    // Nothing of the last account's run carries over, nor its failed reopen. The next prompt
    // opens a new thread, as `console.new` has it, even when the account has one today.
    let moa = &mut state.central;
    let stop = (moa.phase == Phase::Ready).then_some(AcpCmd::Stop {
        generation: moa.generation,
    });
    moa.phase = Phase::Off;
    moa.account = None;
    forget_run(moa);
    moa.reopen_failed = false;
    moa.new_thread = true;
    moa.account_card = None;
    let mut fx = post_commit(state, tx);
    fx.extend(stop.map(Effect::Acp));
    // With reports waiting, a wake-up may start it before you do.
    let text =
        format!("moa switches to {account} with your next prompt or wake-up, in a new thread");
    fx.extend(reply(Ok(Done { text, data })));
    fx.extend(said(state));
    fx
}

/// A bare `/account`: the account moa is on, and the others moa.toml has that it can run on.
fn accounts(state: &State) -> Result<Done, OpError> {
    let on = launch::account(state).ok_or_else(|| launch::no_account(state))?;
    let others: Vec<String> = launch::others(state, &on)
        .into_iter()
        .map(|a| a.to_string())
        .collect();
    let text = match others.is_empty() {
        true => format!("moa is on {on}; no other account is set up"),
        false => format!("moa is on {on}; others: {}", others.join(", ")),
    };
    Ok(Done {
        text,
        data: json!({ "account": on.as_str(), "others": others }),
    })
}

/// A `card` row of `thread` (K7): what moa's core tells you, `what` naming which card and
/// `text` its Markdown. The caller commits it.
pub(crate) fn card(state: &State, tx: &mut Tx, thread: &str, what: &str, text: &str) -> FeedEntry {
    let body = feed::card_body(what, text);
    turn::new_row(state, tx, thread, kind::CARD, None, body, None)
}

/// A `suggestion` row of `thread` (D7): suggestion `id`, made with `text`. The caller commits
/// it with the suggestion.
pub(crate) fn suggestion_card(
    state: &State,
    tx: &mut Tx,
    thread: &str,
    id: SuggestionId,
    text: &str,
) -> FeedEntry {
    let body = feed::suggestion_body(id, text);
    turn::new_row(state, tx, thread, kind::SUGGESTION, None, body, None)
}

/// The ask moa's turn under way answers (A9): its turn's, while its agent hasn't gone.
pub(crate) fn answering(state: &State) -> Option<AskId> {
    let turn = state.central.turn.as_ref().filter(|t| t.exited.is_none())?;
    match turn.source {
        Source::Ask(id) => Some(id),
        _ => None,
    }
}

/// Whether moa's agent can take a turn without you (A9, A2): it runs, or a wake-up could
/// start it, busy or starting as it may be. Not when it failed to start (only your prompt
/// starts it again), or has no account it can run on.
pub(crate) fn can_answer(state: &State) -> bool {
    let moa = &state.central;
    !matches!(moa.phase, Phase::Failed(_)) && (moa.running() || launch::spec(state).is_ok())
}

/// The thread a card goes in (F18, D7): the open one when it is saved, else the newest saved
/// of any account; none when no thread is saved.
pub(crate) fn card_thread(state: &State) -> Option<String> {
    let open = state.central.thread.as_ref().map(|o| &o.row.thread_id);
    let newest = || {
        let newest = state
            .threads()
            .max_by(|a, b| (a.started_at, &a.thread_id).cmp(&(b.started_at, &b.thread_id)));
        newest.map(|t| &t.thread_id)
    };
    open.filter(|id| state.thread(id).is_some())
        .or_else(newest)
        .cloned()
}

/// `console.new`: the next prompt starts a new thread.
pub fn start_new(state: &mut State, conn: ConnId, req: ReqId) -> Vec<Effect> {
    state.central.new_thread = true;
    let result = Ok(Done {
        text: "the next prompt starts a new thread".into(),
        data: json!({}),
    });
    vec![Effect::Reply { conn, req, result }]
}

/// Whether a console is attached: some registered panel shows [`View::Console`].
pub fn console_attached(state: &State) -> bool {
    state.panels.values().any(|p| p.view == View::Console)
}

/// What moa's agent said, from the run the core listens to; any other run's is dropped.
pub fn heard(state: &mut State, now: &Now, input: AcpIn) -> Vec<Effect> {
    if input.generation() != state.central.generation {
        return Vec::new();
    }
    // Whatever the run says is news of its turn.
    state.central.watchdog.news(now.wall);
    let mut fx = match input {
        AcpIn::Started { caps, .. } => started(state, now, caps),
        AcpIn::Opened { result, .. } => opened(state, now, result),
        AcpIn::Update { thread, update, .. } => turn::update(state, now, &thread, update),
        AcpIn::Permission {
            req,
            thread,
            title,
            options,
            ..
        } => turn::permission(state, now, req, thread, &title, &options),
        AcpIn::TurnEnded { prompt, result, .. } => turn::ended(state, now, prompt, result),
        AcpIn::Exited { why, .. } => exited(state, now, why),
    };
    fx.extend(said(state));
    fx
}

/// `console.cancel`: moa's turn under way is cancelled; it ends as cancelled. The requests
/// that wait for your answer are cancelled with it.
pub fn cancel(state: &mut State, now: &Now, conn: ConnId, req: ReqId) -> Vec<Effect> {
    let moa = &state.central;
    let mut fx = Vec::new();
    let result = match (&moa.phase, &moa.turn) {
        (Phase::Thinking, Some(turn)) => {
            fx.push(Effect::Acp(AcpCmd::Cancel {
                generation: moa.generation,
                thread: turn.thread.clone(),
            }));
            fx.extend(turn::unanswered(state, now));
            Ok(Done {
                text: "cancelling moa's turn".into(),
                data: json!({}),
            })
        }
        _ => Err(OpError("moa isn't answering anything".into())),
    };
    fx.push(Effect::Reply { conn, req, result });
    fx.extend(said(state));
    fx
}

/// What `console.permission` says when the request doesn't offer the answer given.
pub const NOT_OFFERED: &str = "moa's agent didn't offer that answer";

/// `console.permission`: answers request `id` (the oldest waiting when `None`) with the
/// option of the kind `answer` names. Its row says which in a commit of its own, and the
/// answer goes once that is saved; while writes are refused it is refused, and still waits.
pub fn answer_permission(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    id: Option<i64>,
    answer: PermAnswer,
) -> Vec<Effect> {
    let asking = &state.central.asking;
    let found = match id {
        Some(id) => asking.get_key_value(&id),
        None => asking.first_key_value(),
    };
    let result = match (found, id) {
        (Some((&id, asking)), _) => match chosen(&asking.options, answer) {
            Some(option) => Ok((id, asking.req, option.clone())),
            None => Err(NOT_OFFERED),
        },
        (None, Some(_)) => Err("that request isn't waiting any more"),
        (None, None) => Err("nothing is waiting for an answer"),
    };
    let (id, asked, option) = match result {
        Ok(found) => found,
        Err(why) => {
            return vec![Effect::Reply {
                conn,
                req,
                result: Err(OpError(why.into())),
            }];
        }
    };
    let mut tx = Tx::new(now);
    let row = turn::answered_row(&state.central, id, option.kind);
    let changes = row.iter().cloned().map(Change::Feed).collect();
    if let Err(e) = tx.commit(state, changes) {
        return vec![Effect::Reply {
            conn,
            req,
            result: Err(e),
        }];
    }
    let moa = &mut state.central;
    if let Some(row) = row {
        moa.written.insert(row.id, row);
    }
    if let Some(asked) = moa.asking.remove(&id) {
        moa.answered.insert(id, asked);
    }
    let mut fx = post_commit(state, tx);
    fx.push(Effect::Acp(AcpCmd::Permission {
        generation: state.central.generation,
        req: asked,
        choice: Some(option.id),
    }));
    let text = match answer {
        PermAnswer::Once => "allowed once",
        PermAnswer::Always => "allowed always",
        PermAnswer::No => "refused",
    };
    let done = Done {
        text: text.into(),
        data: json!({ "id": id, "kind": option.kind.name() }),
    };
    fx.push(Effect::Reply {
        conn,
        req,
        result: Ok(done),
    });
    fx.extend(said(state));
    fx
}

/// The option `answer` stands for: `allow_once`, `allow_always`, or `reject_once` falling back
/// to `reject_always`.
fn chosen(options: &[PermOption], answer: PermAnswer) -> Option<&PermOption> {
    answer
        .kinds()
        .iter()
        .find_map(|k| options.iter().find(|o| o.kind == *k))
}

/// After a failed commit the rows are the store's again. A turn whose prompt's row was lost
/// is forgotten: its prompt, held behind that commit, was never sent. An open thread whose
/// row was lost is unsaved again, and its next prompt saves it (a reload saves nothing). A
/// prompt still waiting for its thread has saved nothing, and goes on.
pub fn reload(state: &mut State) -> Vec<Effect> {
    let unsent = state
        .central
        .turn
        .as_ref()
        .is_some_and(|t| !saved(state, &t.thread, &t.message_id));
    let moa = &mut state.central;
    let mut fx = Vec::new();
    if unsent && let Some(turn) = moa.turn.take() {
        if moa.phase == Phase::Thinking {
            moa.phase = Phase::Ready;
        }
        moa.written.clear();
        fx.extend(answer(turn.waiter, Err(OpError(UNSAVED.into()))));
    }
    turn::reloaded(state);
    fx.extend(said(state));
    fx
}

/// Connection `conn` is gone: nobody waits there any more. A turn it asked for goes on, and
/// lands in the feed.
pub fn client_gone(state: &mut State, conn: ConnId) {
    let moa = &mut state.central;
    let turn = moa.turn.as_mut().map(|t| &mut t.waiter);
    let pending = moa.pending.as_mut().map(|p| &mut p.waiter);
    for waiter in turn.into_iter().chain(pending) {
        if waiter.is_some_and(|w| w.conn == conn) {
            *waiter = None;
        }
    }
}

/// Whether the row with `message_id` in `thread` is saved.
fn saved(state: &State, thread: &str, message_id: &str) -> bool {
    state
        .feed_keys(thread)
        .is_some_and(|keys| keys.contains(message_id))
        || state
            .feed_tail()
            .any(|e| e.thread_id == thread && e.message_id.as_deref() == Some(message_id))
}

/// moa's agent has started: the waiting prompt's thread is opened. After the watchdog's
/// restart today's thread is, and moa stays starting until it is open.
fn started(state: &mut State, now: &Now, caps: Caps) -> Vec<Effect> {
    let moa = &mut state.central;
    if moa.phase != Phase::Starting {
        return Vec::new();
    }
    moa.caps = Some(caps);
    if moa.restarting.is_none() {
        moa.phase = Phase::Ready;
    }
    open(state, now)
}

/// Asks for the waiting prompt's thread, or sends the prompt when its thread is open. With
/// no prompt, after the watchdog's restart, it reopens today's thread, or opens a new one:
/// the one `console.new` asked for, or today's first when there is none yet (the stuck turn
/// began before midnight).
fn open(state: &mut State, now: &Now) -> Vec<Effect> {
    let moa = &state.central;
    let (Some(account), Some(caps)) = (&moa.account, moa.caps) else {
        return Vec::new();
    };
    let how = match &moa.pending {
        Some(p) if p.reopen_failed => How::New,
        Some(_) => choose(state, account, now.local_day, caps),
        None if moa.restarting.is_some() => match choose(state, account, now.local_day, caps) {
            How::Use(_) => return watchdog::reopened(state, now, Restarted::NoThread),
            how => how,
        },
        None => return Vec::new(),
    };
    let (how, brief, mut fx) = match how {
        How::Use(_) => {
            let open = moa.thread.clone();
            let (Some(p), Some(open)) = (state.central.pending.take(), open) else {
                return Vec::new();
            };
            return prompt(state, now, p, open);
        }
        How::Resume(thread) => (ThreadHow::Resume { thread }, None, Vec::new()),
        How::Load(thread) => (ThreadHow::Load { thread }, None, Vec::new()),
        How::New => new_thread(state, now, caps),
    };
    let cwd = state.deps().agent_dir.clone();
    let moa = &mut state.central;
    moa.replaying = match &how {
        ThreadHow::Load { thread } => Some(thread.clone()),
        _ => None,
    };
    if moa.replaying.is_some() {
        moa.written.clear();
    }
    if let Some(p) = &mut moa.pending {
        p.reopening = matches!(how, ThreadHow::Resume { .. } | ThreadHow::Load { .. });
        p.brief = brief;
    } else if let Some(r) = &mut moa.restarting {
        r.brief = brief;
    }
    fx.push(Effect::Acp(AcpCmd::Open {
        generation: moa.generation,
        how,
        cwd,
    }));
    fx
}

/// The waiting prompt's thread is open, or couldn't be. A thread that couldn't be reopened
/// is opened new; a new one that couldn't be opened refuses the prompt.
fn opened(state: &mut State, now: &Now, result: Result<(String, Opened), String>) -> Vec<Effect> {
    let moa = &mut state.central;
    moa.replaying = None;
    if moa.pending.is_none()
        && let (Some(restart), Some(account)) = (&moa.restarting, moa.account.clone())
    {
        let brief = restart.brief.clone();
        let asked = moa.new_thread;
        let mut unopened = None;
        let ended = match result {
            Ok((id, how)) => {
                let row = thread_row(state, now, &account, id, how);
                let open = Open {
                    row,
                    brief,
                    reopen_failed: false,
                };
                match how {
                    Opened::New if asked => Restarted::New(open),
                    Opened::New => Restarted::NewToday(open),
                    Opened::Resumed | Opened::Loaded => Restarted::Reopened(open),
                }
            }
            Err(why) => {
                unopened = Some(why);
                Restarted::NoThread
            }
        };
        let mut fx = Vec::new();
        if matches!(ended, Restarted::NoThread) {
            fx.push(warn("moa's thread couldn't be opened after a restart"));
        }
        fx.extend(watchdog::reopened(state, now, ended));
        // The thread may have failed for its account.
        if let Some(why) = unopened {
            fold_in(&mut fx, account::failed(state, now, &why));
        }
        return fx;
    }
    let (Some(mut p), Some(account)) = (moa.pending.take(), moa.account.clone()) else {
        return Vec::new();
    };
    match result {
        Ok((id, how)) => {
            let row = thread_row(state, now, &account, id, how);
            let open = Open {
                row,
                brief: p.brief.take(),
                reopen_failed: p.reopen_failed,
            };
            state.central.thread = Some(open.clone());
            prompt(state, now, p, open)
        }
        Err(_) if p.reopening => {
            p.reopening = false;
            p.reopen_failed = true;
            state.central.pending = Some(p);
            let mut fx = vec![warn(
                "moa's thread for today couldn't be reopened; a new one is opened",
            )];
            fx.extend(open(state, now));
            fx
        }
        Err(why) => {
            let line = format!(
                "moa couldn't start a thread: {}",
                why.lines().next().unwrap_or("")
            );
            let mut fx = vec![warn("moa's agent couldn't open a thread")];
            fold_in(&mut fx, failed_wake(state, &p, now, &line));
            fx.extend(answer(p.waiter, Err(OpError(line.clone()))));
            fx.push(Effect::Publish(Publish::Event(CoreEvent::Error {
                message: line,
            })));
            fold_in(&mut fx, account::failed(state, now, &why));
            fx
        }
    }
}

/// The row of thread `id`, opened as `how`: its saved row when it was reopened, else a new
/// one on `account`, of kind `Manual` when `console.new` asked for it. Only a new thread ends
/// `console.new`: one said while a reopen was under way still holds for the next prompt.
fn thread_row(
    state: &mut State,
    now: &Now,
    account: &AccountName,
    id: String,
    how: Opened,
) -> MoaThread {
    match (how, state.thread(&id)) {
        (Opened::Resumed | Opened::Loaded, Some(row)) => row.clone(),
        _ => MoaThread {
            thread_id: id,
            account: account.to_string(),
            day: now.local_day,
            kind: if std::mem::take(&mut state.central.new_thread) {
                ThreadKind::Manual
            } else {
                ThreadKind::Daily
            },
            started_at: now.wall,
        },
    }
}

/// What a prompt carries, chosen at its commit ([`waiting`], [`asked`]): the blocks that go
/// before your text (an automatic prompt is only them), its `you` row's body, what the same
/// commit marks delivered, and the card it writes before its row (an ask's).
struct Carried {
    blocks: Vec<String>,
    body: serde_json::Map<String, serde_json::Value>,
    changes: Vec<Change>,
    card: Option<String>,
    /// How many session reports it carries.
    reports: usize,
    /// Where the idle terminals it tells end ([`Moa::idle_told`]).
    idle_told: Option<idle::IdleMark>,
    /// It opens the day.
    opens: bool,
}

/// What prompt `p` carries, other than an ask's ([`Carried`]). What waits goes with it (D1), in
/// this order: the day's opening, on the brief's prompt or yours while it is due
/// ([`opening`]); your yeses ([`State::yeses`], [`carry::yes_text`]); the session reports
/// queued now ([`reports::batch`]); then, on a wake-up or a prompt that opens the day, what's
/// due ([`due`]) and the idle terminals ([`idle`]: a wake-up's since the last told, the
/// opening's all of them); and your text after them. An automatic prompt's text is those
/// blocks, and it reads the reports only while `/auto` is on. The same commit marks each
/// delivered: a report deleted, a yes told, a due task told today, the day opened. `None` for
/// an automatic prompt other than the opening that finds no report or yes left to carry:
/// what's due and the idle terminals never go alone (U18).
fn waiting(state: &State, now: &Now, p: &Pending) -> Option<Carried> {
    let automatic = p.source != Source::You;
    let opens = opening::due_today(state, now) && matches!(p.source, Source::Brief | Source::You);
    let wake = p.source == Source::Wake;
    let opening_block = opens.then(|| opening::OPENING.to_string());
    let yeses: Vec<&Suggestion> = state.yeses().collect();
    let yes_block = (!yeses.is_empty()).then(|| carry::yes_text(&yeses));
    let due: Vec<&Task> = match wake || opens {
        true => due::due_now(state, now),
        false => Vec::new(),
    };
    let due_block = (!due.is_empty()).then(|| due::block(state, now, &due));
    // The idle terminals not told yet, or every one when the day opens; the mark moves past
    // them once this commits.
    let idle: Vec<&Terminal> = match (wake, opens) {
        (_, true) => idle::idle_terms(state, now, None),
        (true, false) => idle::idle_terms(state, now, state.central.idle_told),
        (false, false) => Vec::new(),
    };
    let idle_block = (!idle.is_empty()).then(|| idle::block(state, now, &idle));
    let idle_told = idle.last().map(|t| idle::mark(state, t));
    // The reports' room is what the other blocks leave of the prompt.
    let used: usize = [&opening_block, &yes_block, &due_block, &idle_block]
        .into_iter()
        .flatten()
        .map(|b| b.chars().count() + PART_GAP.len())
        .sum();
    let reports = match automatic && !channel::auto_on(state) {
        true => Vec::new(),
        false => reports::batch(state, now, state.config().moa.wake_batch, used),
    };
    if automatic && !opens && reports.is_empty() && yeses.is_empty() {
        return None;
    }
    let n = reports.len();
    let yes_ids: Vec<i64> = yeses.iter().map(|s| s.id.0).collect();
    let mut changes: Vec<Change> = reports.iter().map(|r| Change::DeleteReport(r.id)).collect();
    changes.extend(carry::told(&yeses));
    changes.extend(due::told(now, &due));
    changes.extend(opens.then(|| opening::told(now)));
    let nags = due.len();
    // The blocks before your text, in D1's order; an automatic prompt is only them.
    let mut blocks: Vec<String> = opening_block.into_iter().chain(yes_block).collect();
    if n > 0 {
        blocks.push(match automatic {
            true => reports::wake_text(state, now, &reports),
            false => reports::prefix_text(state, now, &reports),
        });
    }
    blocks.extend(due_block);
    blocks.extend(idle_block);
    let carried = blocks.join(PART_GAP);
    let mut body = serde_json::Map::new();
    let row_text = if automatic { &carried } else { &p.text };
    body.insert("text".into(), row_text.as_str().into());
    if n > 0 {
        body.insert("reports".into(), n.into());
    }
    if !automatic && !blocks.is_empty() {
        body.insert("carried".into(), carried.as_str().into());
    }
    if !yes_ids.is_empty() {
        body.insert("yeses".into(), yes_ids.into());
    }
    if nags > 0 {
        body.insert("nags".into(), nags.into());
    }
    if opens {
        body.insert("opening".into(), true.into());
    }
    Some(Carried {
        blocks,
        body,
        changes,
        card: None,
        reports: n,
        idle_told,
        opens,
    })
}

/// What the prompt for ask `id` carries (A9 b, c): the hidden ask alone, nothing else that
/// waits, so moa's reply to the asker isn't mixed with it. Its row's body says whose ask it
/// answers (`ask: {id, from}`); its commit takes the ask (`received`) and writes its card
/// first. `None` when it is no `pending` ask to moa any more.
fn asked(state: &State, now: &Now, id: AskId) -> Option<Carried> {
    let ask = asks::for_moa(state, id, now.wall)?;
    let mut body = serde_json::Map::new();
    body.insert("text".into(), ask.prompt.as_str().into());
    body.insert("ask".into(), json!({ "id": id, "from": ask.from }));
    Some(Carried {
        blocks: vec![ask.prompt],
        body,
        changes: vec![Change::Ask(ask.received)],
        card: Some(ask.card),
        reports: 0,
        idle_told: None,
        opens: false,
    })
}

/// Saves prompt `p`'s `you` row in the open thread, then sends it, with what it carries
/// ([`waiting`], or [`asked`] for an ask's). The thread's first prompt (the one that saves its
/// row) also saves the line a failed reopen leaves, and carries the brief. An ask's card goes
/// before the row, in the prompt's thread, which this commit saves when it is new. While
/// writes are refused the prompt is refused too: nothing is sent, and all of it stays. An
/// automatic prompt with nothing to carry saves and sends nothing.
fn prompt(state: &mut State, now: &Now, p: Pending, open: Open) -> Vec<Effect> {
    let automatic = p.source != Source::You;
    let carried = match p.source {
        Source::Ask(id) => asked(state, now, id),
        _ => waiting(state, now, &p),
    };
    let Some(carried) = carried else {
        return Vec::new();
    };
    let body = serde_json::Value::Object(carried.body).to_string();
    let thread = open.row.thread_id.clone();
    let first = state.thread(&thread).is_none();
    let mut tx = Tx::new(now);
    let mut changes = Vec::new();
    if first {
        changes.push(Change::Thread(open.row));
        if open.reopen_failed {
            let line = text_body(REOPEN_FAILED);
            let line = feed::row(
                state,
                &mut tx,
                now.wall,
                &thread,
                kind::SYSTEM,
                None,
                line,
                None,
            );
            changes.push(Change::Feed(line));
        }
    }
    if let Some(text) = &carried.card {
        changes.push(Change::Feed(card(
            state,
            &mut tx,
            &thread,
            asks::CARD,
            text,
        )));
    }
    let id = Some(p.message_id.clone());
    let mut you = feed::row(state, &mut tx, now.wall, &thread, kind::YOU, id, body, None);
    you.automatic = automatic;
    changes.push(Change::Feed(you));
    changes.extend(carried.changes);
    if let Err(e) = tx.commit(state, changes) {
        // Refused writes hold the next wake until they end; any other refusal would come
        // again at once.
        let mut fx = Vec::new();
        if !state.writes_refused(now.wall) {
            fx = failed_wake(state, &p, now, &e.0);
        }
        fx.extend(answer(p.waiter, Err(e)));
        return fx;
    }
    let brief = open.brief.filter(|b| first && !b.is_empty());
    let yours = (!automatic).then(|| p.text.clone());
    let parts: Vec<String> = brief
        .into_iter()
        .chain(carried.blocks)
        .chain(yours)
        .collect();
    let text = parts.join(PART_GAP);
    let moa = &mut state.central;
    if automatic && carried.reports > 0 {
        moa.last_wake = Some(now.wall);
    }
    // The opening tells every idle terminal: the mark never moves back for it.
    if carried.idle_told.is_some() {
        moa.idle_told = moa.idle_told.max(carried.idle_told);
    }
    if carried.opens {
        moa.brief_due = None;
    }
    moa.failed_at = None;
    moa.phase = Phase::Thinking;
    moa.written.clear();
    moa.watchdog.watch(now.wall);
    moa.turn = Some(Turn {
        prompt: p.prompt,
        message_id: p.message_id.clone(),
        thread: thread.clone(),
        source: p.source,
        waiter: p.waiter.filter(|w| w.wait),
        exited: None,
    });
    let generation = moa.generation;
    let mut fx = post_commit(state, tx);
    if let Some(w) = p.waiter.filter(|w| !w.wait) {
        let done = Done {
            text: "moa has it".into(),
            data: json!({ "thread": thread, "message_id": p.message_id }),
        };
        fx.extend(answer(Some(w), Ok(done)));
    }
    fx.push(Effect::Acp(AcpCmd::Prompt {
        generation,
        thread,
        prompt: p.prompt,
        text,
    }));
    fx
}

/// moa's agent is gone: a start that failed leaves moa `Failed`, any other exit `Off`. The
/// prompt waiting for it is refused; the next prompt starts it again. A turn under way says
/// so in an `error` line, its running tool calls cancelled, and whoever waits for it hears
/// the same at once; the turn itself
/// stays until its end, which may still come with the run's last words: the exit can
/// overtake them.
fn exited(state: &mut State, now: &Now, why: String) -> Vec<Effect> {
    let moa = &mut state.central;
    let (phase, told, logged) = match moa.phase {
        Phase::Off | Phase::Failed(_) => return Vec::new(),
        Phase::Starting => (
            Phase::Failed(why.clone()),
            format!("moa's agent couldn't start: {why}"),
            "moa's agent couldn't start",
        ),
        Phase::Ready | Phase::Thinking => (Phase::Off, stopped_text(&why), "moa's agent stopped"),
    };
    moa.phase = phase;
    forget_run(moa);
    let pending = moa.pending.take();
    let mut waiters: Vec<Waiter> = pending
        .as_ref()
        .and_then(|p| p.waiter)
        .into_iter()
        .collect();
    let mut fx = vec![warn(logged)];
    if let Some(p) = &pending {
        let more = failed_wake(state, p, now, &format!("{logged}: {why}"));
        fold_in(&mut fx, more);
    }
    let moa = &mut state.central;
    if let Some(turn) = &mut moa.turn
        && turn.exited.is_none()
    {
        turn.exited = Some(why);
        waiters.extend(turn.waiter.take());
        let thread = turn.thread.clone();
        let mut tx = Tx::new(now);
        let says = turn::new_row(
            state,
            &mut tx,
            &thread,
            kind::ERROR,
            None,
            text_body(&told),
            None,
        );
        fx.extend(turn::cut_short(state, tx, says));
    }
    for w in waiters {
        fx.extend(answer(Some(w), Err(OpError(told.clone()))));
    }
    // A restart's agent that goes before its thread is open couldn't start again: moa is
    // `Failed`, as the card says.
    fx.extend(watchdog::reopened(state, now, Restarted::NotStarted));
    // A start that failed may have failed for its account.
    if let Phase::Failed(why) = state.central.phase.clone() {
        fold_in(&mut fx, account::failed(state, now, &why));
    }
    fx
}

/// Pending prompt `p` never reached moa's agent, as `why` says. An automatic prompt's failed
/// try holds back every automatic source but an ask for a gap ([`Moa::failed_at`]). An ask's
/// own prompt that fails fails its ask, `moa's turn failed: <why>` (A13): an ask isn't held
/// by the gap, so it mustn't be tried again.
fn failed_wake(state: &mut State, p: &Pending, now: &Now, why: &str) -> Vec<Effect> {
    if p.source == Source::You {
        return Vec::new();
    }
    state.central.failed_at = Some(now.wall);
    match p.source {
        Source::Ask(id) => {
            let why = format!("moa's turn failed: {why}");
            asks::moa_ended(state, now, id, Err(&why))
        }
        _ => Vec::new(),
    }
}

/// What whoever waits hears when moa's agent stops.
fn stopped_text(why: &str) -> String {
    format!("moa's agent stopped: {why}; your next prompt starts it again")
}

/// The answer to `waiter`, if anyone waits.
fn answer(waiter: Option<Waiter>, result: Result<Done, OpError>) -> Vec<Effect> {
    waiter
        .map(|w| Effect::Reply {
            conn: w.conn,
            req: w.req,
            result,
        })
        .into_iter()
        .collect()
}

/// The `feed` topic again, when moa's `state` row changed since it was last published (its
/// other rows were published with their commit).
fn said(state: &mut State) -> Option<Effect> {
    state
        .publish_moa_state()
        .map(|m| Effect::Publish(Publish::Model(m)))
}

fn warn(message: impl Into<String>) -> Effect {
    log(LogLevel::Warn, message.into())
}
