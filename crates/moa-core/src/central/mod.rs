//! moa's own agent, the central one: the core's side of its conversation. No IO of its own:
//! the files it is told (`prompt`) are read through `Deps::read_text`, as the layout ops do.
//!
//! A prompt starts moa's agent when it isn't running, opens today's thread when it isn't
//! open ([`threads`]), saves the prompt's `you` row, and only then sends it: the prompt waits
//! for the commit. One prompt at a time.

pub mod feed;
pub mod launch;
pub mod prompt;
pub mod threads;
pub mod wire;

use serde_json::json;

use self::feed::{kind, text_body};
use self::threads::{How, choose, new_thread};
use self::wire::{AcpCmd, AcpIn, Caps, Opened, ThreadHow};
use crate::Now;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine, Publish};
use crate::model::{MoaThread, ThreadKind};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::publish::{CoreEvent, MoaRow, Topic};
use crate::state::State;

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

/// The thread moa's agent has open in this run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Open {
    /// Its `moa_threads` row: saved with its first prompt, and again when a reload lost it.
    pub row: MoaThread,
    /// The brief, until the thread's first prompt carries it: a new thread opened without a
    /// system prompt.
    pub brief: Option<String>,
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
    /// The brief this prompt carried: the thread's again if a reload finds the prompt
    /// unsent.
    pub brief: Option<String>,
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
            // A new run: nothing of the last one carries over.
            moa.generation += 1;
            moa.phase = Phase::Starting;
            moa.account = Some(account);
            moa.caps = None;
            moa.thread = None;
            moa.turn = None;
            moa.replaying = false;
            vec![Effect::Acp(AcpCmd::Start {
                generation: moa.generation,
                spec,
            })]
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
        AcpIn::TurnEnded { prompt, .. } => turn_ended(state, prompt),
        AcpIn::Exited { why, .. } => exited(state, why),
        // What moa says in a turn isn't written to the feed yet.
        AcpIn::Update { .. } | AcpIn::Permission { .. } => Vec::new(),
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
/// row was lost is saved again (or with the next prompt, while writes are refused). A
/// prompt still waiting for its thread has saved nothing, and goes on.
pub fn reload(state: &mut State, now: &Now) -> Vec<Effect> {
    let unsent = state
        .central
        .turn
        .as_ref()
        .is_some_and(|t| !saved(state, &t.thread, &t.message_id));
    let moa = &mut state.central;
    let mut told = None;
    if unsent && let Some(turn) = moa.turn.take() {
        if moa.phase == Phase::Thinking {
            moa.phase = Phase::Ready;
        }
        if let Some(open) = moa
            .thread
            .as_mut()
            .filter(|o| o.row.thread_id == turn.thread)
        {
            open.brief = turn.brief;
        }
        told = turn.waiter;
    }
    let mut fx = Vec::new();
    if let Some(open) = &state.central.thread
        && state.thread(&open.row.thread_id).is_none()
    {
        let row = open.row.clone();
        let mut tx = Tx::new(now);
        if tx.commit(state, vec![Change::Thread(row)]).is_ok() {
            fx = post_commit(state, tx);
        }
    }
    fx.extend(answer(told, Err(OpError(UNSAVED.into()))));
    fx.extend(said(state));
    fx
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
            let row = moa.thread.as_ref().map(|o| o.row.clone());
            let (Some(p), Some(row)) = (state.central.pending.take(), row) else {
                return Vec::new();
            };
            return prompt(state, now, p, row);
        }
        How::Resume(thread) => (ThreadHow::Resume { thread }, None, Vec::new()),
        How::Load(thread) => (ThreadHow::Load { thread }, None, Vec::new()),
        How::New => new_thread(state, now, caps),
    };
    let cwd = state.deps().agent_dir.clone();
    let moa = &mut state.central;
    moa.replaying = matches!(how, ThreadHow::Load { .. });
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
            state.central.thread = Some(Open {
                row: row.clone(),
                brief: p.brief.take(),
            });
            prompt(state, now, p, row)
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

/// Saves prompt `p`'s `you` row in thread `row` (and `row`, when it isn't saved), then sends
/// it. While writes are refused the prompt is refused too: nothing is sent.
fn prompt(state: &mut State, now: &Now, p: Pending, row: MoaThread) -> Vec<Effect> {
    let thread = row.thread_id.clone();
    let mut tx = Tx::new(now);
    let mut changes = Vec::new();
    if state.thread(&thread).is_none() {
        changes.push(Change::Thread(row));
    }
    if p.reopen_failed {
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
    let body = text_body(&p.text);
    let id = Some(p.message_id.clone());
    let mut you = feed::row(state, &mut tx, now.wall, &thread, kind::YOU, id, body, None);
    you.automatic = p.automatic;
    changes.push(Change::Feed(you));
    if let Err(e) = tx.commit(state, changes) {
        return answer(p.waiter, Err(e));
    }
    let moa = &mut state.central;
    let brief = moa
        .thread
        .as_mut()
        .and_then(|o| o.brief.take())
        .filter(|b| !b.is_empty());
    let text = match &brief {
        Some(brief) => format!("{brief}\n\n---\n\n{}", p.text),
        None => p.text,
    };
    moa.phase = Phase::Thinking;
    moa.turn = Some(Turn {
        prompt: p.prompt,
        message_id: p.message_id.clone(),
        thread: thread.clone(),
        automatic: p.automatic,
        waiter: p.waiter.filter(|w| w.wait),
        brief,
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

/// Turn `prompt` ended: moa is ready for the next one.
fn turn_ended(state: &mut State, prompt: u64) -> Vec<Effect> {
    let moa = &mut state.central;
    if moa.turn.as_ref().is_none_or(|t| t.prompt != prompt) {
        return Vec::new();
    }
    moa.turn = None;
    if moa.phase == Phase::Thinking {
        moa.phase = Phase::Ready;
    }
    Vec::new()
}

/// moa's agent is gone: a start that failed leaves moa `Failed`, any other exit `Off`. The
/// prompt waiting for it is refused; the next prompt starts it again. A turn under way is
/// left to its own end, which may still come: the exit can overtake the run's last words.
fn exited(state: &mut State, why: String) -> Vec<Effect> {
    let moa = &mut state.central;
    let (phase, told, logged) = match moa.phase {
        Phase::Off | Phase::Failed(_) => return Vec::new(),
        Phase::Starting => (
            Phase::Failed(why.clone()),
            format!("moa's agent couldn't start: {why}"),
            "moa's agent couldn't start",
        ),
        Phase::Ready | Phase::Thinking => (
            Phase::Off,
            format!("moa's agent stopped: {why}; your next prompt starts it again"),
            "moa's agent stopped",
        ),
    };
    moa.phase = phase;
    moa.caps = None;
    moa.thread = None;
    moa.replaying = false;
    let waiter = moa.pending.take().and_then(|p| p.waiter);
    let mut fx = vec![warn(logged)];
    fx.extend(answer(waiter, Err(OpError(told))));
    fx
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

/// The `feed` topic again, when moa's `state` row (or any row) changed since it was last
/// published.
fn said(state: &mut State) -> Option<Effect> {
    state
        .publish_changed(Topic::Feed)
        .map(|m| Effect::Publish(Publish::Model(m)))
}

fn warn(message: &str) -> Effect {
    Effect::Log(LogLine {
        level: LogLevel::Warn,
        message: message.into(),
    })
}
