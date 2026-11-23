//! moa's own agent played by hand: its `moa.toml`, its prompts, and what it says back.

use jiff::civil::Date;
use moa_core::central::Phase;
use moa_core::central::wire::{AcpCmd, AcpIn, Caps, Opened, StopReason};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, Publish, handle_checked};
use moa_core::model::{FeedEntry, MoaThread, ThreadKind};
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::publish::Topic;
use moa_core::state::{Deps, Snapshot, State};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use serde_json::Value;

use super::{changes, loaded_with};

pub const LAUNCHER: &str = "/opt/claude/bin/claude";
pub const STUB: &str = "/opt/moa/bin/stub-acp";

/// `moa.toml` with moa's account `account` (none for `""`), and both accounts run by the stub.
pub fn toml(account: &str) -> String {
    let moa = if account.is_empty() {
        String::new()
    } else {
        format!("moa_account = \"{account}\"\n")
    };
    let acct = |name: &str| {
        format!(
            "[accounts.{name}]\nkind = \"claude\"\nstore = \"~/.claude-{name}\"\n\
             launcher = \"{LAUNCHER}\"\nacp = \"{STUB}\"\n"
        )
    };
    format!(
        "[brains.default]\nfolders = [\"~/private\"]\n{moa}{}{}",
        acct("acct-a"),
        acct("acct-b")
    )
}

/// The typical brain with moa's account `account` and these `moa_threads` rows.
pub fn moa_with(b: &TypicalBrain, account: &str, threads: Vec<MoaThread>) -> State {
    let snapshot = Snapshot {
        threads,
        ..b.snapshot()
    };
    moa_from(b, account, snapshot)
}

/// The typical brain with moa's account `account`, loaded from `snapshot`.
pub fn moa_from(b: &TypicalBrain, account: &str, snapshot: Snapshot) -> State {
    let deps = Deps {
        home: b.home.clone(),
        ..Deps::default()
    };
    loaded_with(b, &toml(account), deps, snapshot)
}

pub fn moa(b: &TypicalBrain) -> State {
    moa_with(b, "acct-a", vec![])
}

pub fn thread(id: &str, account: &str, day: Date, started_at: i64) -> MoaThread {
    MoaThread {
        thread_id: id.into(),
        account: account.into(),
        day,
        kind: ThreadKind::Daily,
        started_at: UtcMs(started_at),
    }
}

pub fn caps(load: bool, resume: bool, system_prompt: bool) -> Caps {
    Caps {
        load,
        resume,
        system_prompt,
    }
}

/// `console.prompt` with `text`, as request `req` on connection 1.
pub fn prompt_at(st: &mut State, now: Now, req: u64, text: &str, wait: bool) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(req),
            caller: Default::default(),
            op: Op::ConsolePrompt {
                text: text.into(),
                wait,
            },
        },
        now,
    )
}

pub fn prompt(b: &TypicalBrain, st: &mut State, req: u64, text: &str) -> Vec<Effect> {
    prompt_at(st, b.now(), req, text, false)
}

pub fn hear_at(st: &mut State, now: Now, input: AcpIn) -> Vec<Effect> {
    handle_checked(st, Input::Acp(input), now)
}

pub fn hear(b: &TypicalBrain, st: &mut State, input: AcpIn) -> Vec<Effect> {
    hear_at(st, b.now(), input)
}

pub fn started(generation: u64, caps: Caps) -> AcpIn {
    AcpIn::Started { generation, caps }
}

pub fn opened(generation: u64, id: &str, how: Opened) -> AcpIn {
    AcpIn::Opened {
        generation,
        result: Ok((id.into(), how)),
    }
}

pub fn ended(generation: u64, prompt: u64) -> AcpIn {
    AcpIn::TurnEnded {
        generation,
        prompt,
        result: Ok(StopReason::EndTurn),
    }
}

/// The commands for moa's agent in `fx`.
pub fn acps(fx: &[Effect]) -> Vec<&AcpCmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Acp(c) => Some(c),
            _ => None,
        })
        .collect()
}

/// The texts of the prompts sent in `fx`.
pub fn prompts_sent(fx: &[Effect]) -> Vec<&str> {
    acps(fx)
        .into_iter()
        .filter_map(|c| match c {
            AcpCmd::Prompt { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The feed rows saved in `fx`.
pub fn feed_rows(fx: &[Effect]) -> Vec<&FeedEntry> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Feed(r) => Some(r),
            _ => None,
        })
        .collect()
}

/// The thread rows saved in `fx`.
pub fn thread_rows(fx: &[Effect]) -> Vec<&MoaThread> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Thread(r) => Some(r),
            _ => None,
        })
        .collect()
}

/// The `state` row of the last `feed` publish in `fx`.
pub fn published_state(fx: &[Effect]) -> Option<Value> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => m.topics.get(&Topic::Feed),
            _ => None,
        })
        .next_back()
        .map(|rows| rows["state"].clone())
}

pub fn phase(st: &State) -> &Phase {
    &st.central().phase
}

pub fn text_of(row: &FeedEntry) -> Value {
    serde_json::from_str::<Value>(&row.body).unwrap()["text"].clone()
}

/// One prompt, request 1 (`wait` as given), from moa off to its turn under way in thread
/// `id`, opened new.
pub fn first_turn(st: &mut State, now: &Now, id: &str, wait: bool) {
    let generation = st.central().generation + 1;
    prompt_at(st, now.clone(), 1, "hello", wait);
    hear_at(st, now.clone(), started(generation, caps(true, true, true)));
    let fx = hear_at(st, now.clone(), opened(generation, id, Opened::New));
    assert_eq!(*phase(st), Phase::Thinking, "{fx:?}");
}

/// What the store would load now, had every commit of `st` reached it: its threads, its feed
/// rows and their keys.
pub fn saved(b: &TypicalBrain, st: &State) -> Snapshot {
    let mut snapshot = b.snapshot();
    snapshot.threads = st.threads().cloned().collect();
    snapshot.feed_tail = st.feed_tail().cloned().collect();
    snapshot.feed_keys = st
        .threads()
        .flat_map(|t| {
            let keys = st.feed_keys(&t.thread_id).into_iter().flatten();
            keys.map(|k| (t.thread_id.clone(), k.clone()))
        })
        .collect();
    if let Some(last) = snapshot.feed_tail.last() {
        snapshot.next_ids.feed = last.id + 1;
    }
    snapshot
}
