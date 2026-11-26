//! moa's own agent played by hand: its `moa.toml`, its prompts, and what it says back.

use jiff::civil::Date;
use moa_core::central::Phase;
use moa_core::central::opening::BRIEFED_KEY;
use moa_core::central::wire::{AcpCmd, AcpIn, Caps, Opened, StopReason};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, Publish, RefuseWrites, TimerKey, handle_checked};
use moa_core::model::{FeedEntry, MoaThread, ThreadKind};
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::publish::Topic;
use moa_core::state::{Deps, Snapshot, State};
use moa_core::{Now, UtcMs};
use moa_store::Store;
use moa_testkit::brain::TypicalBrain;
use serde_json::Value;

use super::{changes, started_at};

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
    moa_at(b, account, "", snapshot, b.now()).0
}

/// [`moa_from`] with `settings` (TOML, such as a `[moa]` table) after the `moa.toml`,
/// started at `now`; what its start said too. moa opened `now`'s day already, unless
/// `snapshot` says which day it did: the user's presence opens nothing here (D2).
pub fn moa_at(
    b: &TypicalBrain,
    account: &str,
    settings: &str,
    mut snapshot: Snapshot,
    now: Now,
) -> (State, Vec<Effect>) {
    opened_today(&mut snapshot, &now);
    let deps = Deps {
        home: b.home.clone(),
        ..Deps::default()
    };
    started_at(
        b,
        &format!("{}{settings}", toml(account)),
        deps,
        snapshot,
        now,
    )
}

/// moa opened `now`'s day already, unless `snapshot` says which day it did: the user's
/// presence opens nothing (D2), and prompts carry what they did before the opening.
pub fn opened_today(snapshot: &mut Snapshot, now: &Now) {
    if !snapshot.meta.iter().any(|(k, _)| k == BRIEFED_KEY) {
        let today = now.local_day.to_string();
        snapshot.meta.push((BRIEFED_KEY.to_string(), today));
    }
}

/// The daemon's disk: the typical brain's own store, which holds only the commits a test
/// executes. A restart loads what it holds, as the daemon's start does.
pub struct Disk {
    store: Store,
    /// What `moa.toml` says after [`toml`]`("acct-a")`.
    settings: String,
}

impl Disk {
    pub fn new(b: &TypicalBrain, settings: &str) -> Disk {
        Disk {
            store: b.store(),
            settings: settings.into(),
        }
    }

    /// One commit reaches the disk.
    pub fn apply(&mut self, changes: &[Change]) {
        self.store
            .apply(changes)
            .expect("the disk takes the commit");
    }

    pub fn snapshot(&self) -> Snapshot {
        self.store.load_snapshot().expect("the disk loads")
    }

    /// The daemon starts again at `now`, with moa's account `acct-a`, from what the disk
    /// holds; what its start said too.
    pub fn restart(&self, b: &TypicalBrain, now: Now) -> (State, Vec<Effect>) {
        moa_at(b, "acct-a", &self.settings, self.snapshot(), now)
    }

    /// The `meta` value of `key` on the disk now.
    pub fn meta(&self, key: &str) -> Option<String> {
        self.store
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

    /// The bodies of every `you` row saved, read with SQL: every one, not a tail.
    pub fn you_bodies(&self) -> Vec<String> {
        let mut rows = self
            .store
            .conn()
            .prepare("SELECT body FROM feed WHERE kind = 'you' ORDER BY id")
            .unwrap();
        rows.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
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
    let briefed = st
        .meta(BRIEFED_KEY)
        .map(|d| (BRIEFED_KEY.to_string(), d.to_string()));
    snapshot.meta.extend(briefed);
    snapshot
}

/// From now on, until a minute from now, the store refuses writes; everything saved so far
/// is kept.
pub fn refuse_writes(b: &TypicalBrain, st: &mut State) {
    let snapshot = saved(b, st);
    handle_checked(
        st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(b.now().wall.0 + 60_000),
                reason: "the disk is full".into(),
            }),
        },
        b.now(),
    );
}

/// The commits `fx` saves.
pub fn commits(fx: &[Effect]) -> Vec<&Vec<Change>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect()
}

/// The `Wake` timer as `fx` leaves it: `Some(Some(at))` set, `Some(None)` cancelled, `None`
/// untouched.
pub fn wake_timer(fx: &[Effect]) -> Option<Option<UtcMs>> {
    super::timers(fx, TimerKey::Wake).last().map(|e| match e {
        Effect::SetTimer { at, .. } => Some(*at),
        _ => None,
    })
}

/// The `Wake` timer fires at `ms`.
pub fn fire(st: &mut State, ms: i64) -> Vec<Effect> {
    handle_checked(st, Input::Timer(TimerKey::Wake), super::at(UtcMs(ms)))
}

/// moa's agent answers at `ms` what `fx` asks of it: it starts, and opens a new thread, as
/// each is asked for. Every effect, `fx`'s first.
pub fn play(st: &mut State, ms: i64, fx: Vec<Effect>) -> Vec<Effect> {
    play_at(st, super::at(UtcMs(ms)), fx)
}

/// [`play`] at `now`.
pub fn play_at(st: &mut State, now: Now, mut fx: Vec<Effect>) -> Vec<Effect> {
    let mut i = 0;
    while i < fx.len() {
        let heard = match &fx[i] {
            Effect::Acp(AcpCmd::Start { generation, .. }) => {
                Some(started(*generation, caps(true, true, true)))
            }
            Effect::Acp(AcpCmd::Open { generation, .. }) => Some(opened(
                *generation,
                &format!("thread-{generation}"),
                Opened::New,
            )),
            _ => None,
        };
        if let Some(input) = heard {
            let more = hear_at(st, now.clone(), input);
            fx.extend(more);
        }
        i += 1;
    }
    fx
}

/// The turn under way ends at `ms`.
pub fn end_turn(st: &mut State, ms: i64) -> Vec<Effect> {
    let generation = st.central().generation;
    let prompt = st.central().turn.as_ref().expect("a turn").prompt;
    hear_at(st, super::at(UtcMs(ms)), ended(generation, prompt))
}

/// The one prompt `fx` sends.
pub fn sent(fx: &[Effect]) -> String {
    match prompts_sent(fx)[..] {
        [text] => text.to_string(),
        ref other => panic!("{} prompts sent", other.len()),
    }
}

/// The `you` row `fx` saves.
pub fn you_row(fx: &[Effect]) -> &FeedEntry {
    feed_rows(fx)
        .into_iter()
        .find(|r| r.kind == "you")
        .expect("a you row")
}

/// The one commit of `fx` that saves a `you` row.
pub fn you_commit(fx: &[Effect]) -> &Vec<Change> {
    let commits: Vec<&Vec<Change>> = commits(fx)
        .into_iter()
        .filter(|c| {
            c.iter()
                .any(|c| matches!(c, Change::Feed(r) if r.kind == "you"))
        })
        .collect();
    match commits[..] {
        [c] => c,
        _ => panic!("{} commits save a you row", commits.len()),
    }
}

pub fn body_of(row: &FeedEntry) -> Value {
    serde_json::from_str(&row.body).unwrap()
}
