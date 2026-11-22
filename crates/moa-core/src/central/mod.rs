//! moa's own agent, the central one: the core's side of its conversation. No IO of its own:
//! the files it is told (`prompt`) are read through `Deps::read_text`, as the layout ops do.
//!
//! A prompt starts moa's agent when it isn't running, opens today's thread when it isn't
//! open ([`threads`]), saves the prompt's `you` row, and only then sends it: the prompt waits
//! for the commit. One prompt at a time. What moa says in its turn lands in the feed as it
//! comes, and the turn's end answers whoever waits ([`turn`]).

pub mod feed;
pub mod launch;
pub mod prompt;
pub mod threads;
mod turn;
pub mod wire;

use std::collections::BTreeMap;

use serde_json::json;

use self::feed::{kind, text_body};
use self::threads::{How, choose, new_thread};
use self::wire::{AcpCmd, AcpIn, Caps, Opened, ThreadHow};
use crate::Now;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, Publish};
use crate::model::{FeedEntry, MoaThread, ThreadKind};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::publish::{CoreEvent, MoaRow};
use crate::state::State;
use crate::tmux::link::log;

/// The longest prompt, in characters.
pub const PROMPT_MAX: usize = 100_000;

const BUSY: &str = "moa is still answering; wait for it, or cancel it";

/// The first line of a new thread opened because today's couldn't be reopened.
const REOPEN_FAILED: &str = "today's earlier thread couldn't be reopened, so this is a new one";

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
    pub text: String,
    pub automatic: bool,
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
    pub automatic: bool,
    /// Who waits for moa's answer. A prompt sent without `wait` was answered once its row
    /// was saved.
    pub waiter: Option<Waiter>,
    /// Why moa's agent went away during the turn. Its end may still come after.
    pub exited: Option<String>,
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
    /// A load's replay is under way: its updates are checked against the feed's keys.
    pub replaying: bool,
    /// The last prompt's number.
    pub prompts: u64,
    /// The rows the turn under way (or a load's replay) has written, by id, as written,
    /// saved or not: moa's next words extend them.
    pub written: BTreeMap<i64, FeedEntry>,
    /// moa's updates that couldn't be saved since the count was last logged, at a turn's
    /// end.
    pub unsaved: u64,
}

impl Moa {
    /// The `state` row of the `feed` topic.
    pub fn row(&self) -> MoaRow {
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
        }
    }

    /// A prompt now would be one too many: moa is starting, opening its thread, or
    /// answering.
    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Thinking) || self.pending.is_some()
    }

    /// moa's agent runs, and has said what it offers.
    fn running(&self) -> bool {
        self.phase == Phase::Ready && self.account.is_some() && self.caps.is_some()
    }
}

/// The one way a prompt reaches moa: `console.prompt`, and moa's own automatic prompts,
/// which are told apart by the `message_id` it records. `waiter` hears `moa has it` once
/// the prompt's row is saved, or, with `wait`, moa's answer when the turn ends.
pub fn send(
    state: &mut State,
    now: &Now,
    text: String,
    automatic: bool,
    waiter: Option<Waiter>,
) -> Vec<Effect> {
    let text = text.trim();
    let refusal = if text.is_empty() {
        Some("there's nothing to send".to_string())
    } else if text.chars().count() > PROMPT_MAX {
        Some(format!("a prompt is at most {PROMPT_MAX} characters"))
    } else if state.central.busy() {
        Some(BUSY.to_string())
    } else {
        None
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
        automatic,
        waiter,
        reopening: false,
        reopen_failed: false,
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
            moa.generation += 1;
            moa.phase = Phase::Starting;
            moa.account = Some(account);
            moa.caps = None;
            moa.thread = None;
            moa.replaying = false;
            fx.push(Effect::Acp(AcpCmd::Start {
                generation: moa.generation,
                spec,
            }));
            fx
        }
        None => open(state, now),
    };
    fx.extend(said(state));
    fx
}

/// What moa's agent said, from the run the core listens to; any other run's is dropped.
pub fn heard(state: &mut State, now: &Now, input: AcpIn) -> Vec<Effect> {
    if input.generation() != state.central.generation {
        return Vec::new();
    }
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

/// `console.cancel`: moa's turn under way is cancelled; it ends as cancelled.
pub fn cancel(state: &State, conn: ConnId, req: ReqId) -> Vec<Effect> {
    let moa = &state.central;
    let mut fx = Vec::new();
    let result = match (&moa.phase, &moa.turn) {
        (Phase::Thinking, Some(turn)) => {
            fx.push(Effect::Acp(AcpCmd::Cancel {
                generation: moa.generation,
                thread: turn.thread.clone(),
            }));
            Ok(Done {
                text: "cancelling moa's turn".into(),
                data: json!({}),
            })
        }
        _ => Err(OpError("moa isn't answering anything".into())),
    };
    fx.push(Effect::Reply { conn, req, result });
    fx
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

/// moa's agent has started: the waiting prompt's thread is opened.
fn started(state: &mut State, now: &Now, caps: Caps) -> Vec<Effect> {
    let moa = &mut state.central;
    if moa.phase != Phase::Starting {
        return Vec::new();
    }
    moa.caps = Some(caps);
    moa.phase = Phase::Ready;
    open(state, now)
}

/// Asks for the waiting prompt's thread, or sends the prompt when its thread is open.
fn open(state: &mut State, now: &Now) -> Vec<Effect> {
    let moa = &state.central;
    let (Some(account), Some(caps), Some(pending)) = (&moa.account, moa.caps, &moa.pending) else {
        return Vec::new();
    };
    let how = match pending.reopen_failed {
        true => How::New,
        false => choose(state, account, now.local_day, caps),
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
    moa.replaying = matches!(how, ThreadHow::Load { .. });
    if moa.replaying {
        moa.written.clear();
    }
    if let Some(p) = &mut moa.pending {
        p.reopening = matches!(how, ThreadHow::Resume { .. } | ThreadHow::Load { .. });
        p.brief = brief;
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
    moa.replaying = false;
    let (Some(mut p), Some(account)) = (moa.pending.take(), moa.account.clone()) else {
        return Vec::new();
    };
    match result {
        Ok((id, how)) => {
            let row = match (how, state.thread(&id)) {
                (Opened::Resumed | Opened::Loaded, Some(row)) => row.clone(),
                _ => MoaThread {
                    thread_id: id,
                    account: account.to_string(),
                    day: now.local_day,
                    kind: ThreadKind::Daily,
                    started_at: now.wall,
                },
            };
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
            fx.extend(answer(p.waiter, Err(OpError(line.clone()))));
            fx.push(Effect::Publish(Publish::Event(CoreEvent::Error {
                message: line,
            })));
            fx
        }
    }
}

/// Saves prompt `p`'s `you` row in the open thread, then sends it. The thread's first prompt
/// (the one that saves its row) also saves the line a failed reopen leaves, and carries the
/// brief. While writes are refused the prompt is refused too: nothing is sent.
fn prompt(state: &mut State, now: &Now, p: Pending, open: Open) -> Vec<Effect> {
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
    let body = text_body(&p.text);
    let id = Some(p.message_id.clone());
    let mut you = feed::row(state, &mut tx, now.wall, &thread, kind::YOU, id, body, None);
    you.automatic = p.automatic;
    changes.push(Change::Feed(you));
    if let Err(e) = tx.commit(state, changes) {
        return answer(p.waiter, Err(e));
    }
    let text = match open.brief.filter(|b| first && !b.is_empty()) {
        Some(brief) => format!("{brief}\n\n---\n\n{}", p.text),
        None => p.text,
    };
    let moa = &mut state.central;
    moa.phase = Phase::Thinking;
    moa.written.clear();
    moa.turn = Some(Turn {
        prompt: p.prompt,
        message_id: p.message_id.clone(),
        thread: thread.clone(),
        automatic: p.automatic,
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
    moa.caps = None;
    moa.thread = None;
    moa.replaying = false;
    let mut waiters: Vec<Waiter> = moa
        .pending
        .take()
        .and_then(|p| p.waiter)
        .into_iter()
        .collect();
    let mut fx = vec![warn(logged)];
    if let Some(turn) = &mut moa.turn
        && turn.exited.is_none()
    {
        turn.exited = Some(why);
        waiters.extend(turn.waiter.take());
        let thread = turn.thread.clone();
        fx.extend(turn::cut_short(state, now, &thread, &told));
    }
    for w in waiters {
        fx.extend(answer(Some(w), Err(OpError(told.clone()))));
    }
    fx
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
