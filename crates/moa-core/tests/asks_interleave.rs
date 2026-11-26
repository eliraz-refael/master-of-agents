//! Asks racing for peers (Review Focus 2): whatever three sessions and moa ask each other,
//! whenever the sessions' turns start and end, the timer fires, a client goes, a write is
//! refused, or the core reloads or the daemon restarts from the rows on disk, an ask moves
//! only as A1 says, one at a time per peer, and each request hears at most once.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use moa_core::UtcMs;
use moa_core::asks::{ASK_RECEIPT_MS, framed};
use moa_core::effect::{Effect, Input, RefuseWrites, TimerKey, TmuxIn, handle_checked};
use moa_core::hook::HookKind;
use moa_core::model::{Ask, AskId, AskStatus, AskTarget};
use moa_core::op::{ConnId, Op};
use moa_core::state::CallerInfo;
use moa_core::tmux::notify::Notification;
use moa_store::Store;
use proptest::prelude::*;
use tempfile::TempDir;

use common::asks::*;

const SESSIONS: [&str; 3] = [KITE, HARBOR, BEACON];
const NO_RECEIPT: &str = "no receipt (a dialog or copy mode had the pane)";

#[derive(Clone, Debug)]
enum Step {
    /// `from` (a session, or moa for 3) asks session `to`, waiting `wait_s`, on a new
    /// connection.
    Ask { from: usize, to: usize, wait_s: i64 },
    /// `reply` waits on ask `ask` on a new connection.
    Reply { ask: i64, wait_s: i64 },
    /// Session `who`'s turn starts, its prompt carrying ask `marker`'s marker when given.
    Prompt { who: usize, marker: Option<i64> },
    /// Session `who`'s turn starts with the ask typed into it: the one `sent` to it, else
    /// the last that failed for want of a receipt (or a prompt of the user's, with none).
    Take(usize),
    /// Session `who`'s turn ends, its message empty or not.
    Stop { who: usize, empty: bool },
    /// Session `who`'s turn fails.
    Fail(usize),
    /// The asks timer fires when it is armed (or now, when it isn't).
    Timer,
    /// Time passes.
    Advance(i64),
    /// The `n`th connection still waiting goes.
    Gone(usize),
    /// The next input's writes are refused.
    RefuseNext,
    /// The core loads the rows on disk again.
    Reload,
    /// The daemon restarts from the rows on disk: every connection goes.
    Restart,
    /// Session `who`'s tab exits: it goes to sleep.
    Exit(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0..4usize, 0..3usize, prop::sample::select(vec![1i64, 30, 540]))
            .prop_map(|(from, to, wait_s)| Step::Ask { from, to, wait_s }),
        1 => (1..8i64, prop::sample::select(vec![1i64, 60]))
            .prop_map(|(ask, wait_s)| Step::Reply { ask, wait_s }),
        3 => (0..3usize, prop::option::weighted(0.7, 1..8i64))
            .prop_map(|(who, marker)| Step::Prompt { who, marker }),
        3 => (0..3usize).prop_map(Step::Take),
        3 => (0..3usize, prop::bool::weighted(0.2)).prop_map(|(who, empty)| Step::Stop { who, empty }),
        1 => (0..3usize).prop_map(Step::Fail),
        3 => Just(Step::Timer),
        2 => prop::sample::select(vec![SEC, 5 * SEC, 20 * SEC, MIN, 10 * MIN, 20 * MIN])
            .prop_map(Step::Advance),
        1 => (0..4usize).prop_map(Step::Gone),
        1 => Just(Step::RefuseNext),
        1 => Just(Step::Reload),
        1 => Just(Step::Restart),
        1 => (0..3usize).prop_map(Step::Exit),
    ]
}

/// The core over a real store, its link up, and what was heard so far.
struct World {
    _dir: TempDir,
    store: Store,
    r: Rig,
    now: i64,
    conn: u64,
    /// The requests that wait, by connection and request.
    waiting: BTreeSet<(u64, u64)>,
    /// Every request heard, by connection and request.
    heard: BTreeSet<(u64, u64)>,
    /// Each ask as last seen.
    last: BTreeMap<AskId, Ask>,
    /// Where the asks timer is armed.
    armed: Option<i64>,
    refuse_next: bool,
    /// Whether the last input's writes were refused.
    refused: bool,
}

impl World {
    fn new() -> Result<World, TestCaseError> {
        let dir = tempfile::Builder::new()
            .prefix("moa-asks")
            .tempdir_in("/tmp")
            .unwrap();
        let mut store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups"))
            .expect("a new store");
        let seed = snapshot();
        let mut rows: Vec<moa_core::change::Change> = Vec::new();
        rows.extend(
            seed.sessions
                .into_iter()
                .map(moa_core::change::Change::Session),
        );
        rows.extend(
            seed.conversations
                .into_iter()
                .map(moa_core::change::Change::Conversation),
        );
        rows.extend(
            seed.pane_bindings
                .into_iter()
                .map(moa_core::change::Change::PaneBinding),
        );
        store.apply(&rows).expect("the seed is saved");
        let r = Rig::new(store.load_snapshot().unwrap());
        let mut w = World {
            _dir: dir,
            store,
            r,
            now: T0,
            conn: 0,
            waiting: BTreeSet::new(),
            heard: BTreeSet::new(),
            last: BTreeMap::new(),
            armed: None,
            refuse_next: false,
            refused: false,
        };
        w.now += 1;
        let fx = w.r.link_up(w.now);
        w.execute(fx)?;
        Ok(w)
    }

    /// The next input's moment, a millisecond on; its writes refused when the step before
    /// said so.
    fn next(&mut self) -> Result<(), TestCaseError> {
        self.now += 1;
        self.refused = std::mem::take(&mut self.refuse_next);
        if self.refused {
            let reload = Input::Reload {
                snapshot: Box::new(self.store.load_snapshot().unwrap()),
                refuse_writes: Some(RefuseWrites {
                    until: UtcMs(self.now + 1),
                    reason: "the disk is full".into(),
                }),
            };
            let fx = handle_checked(&mut self.r.st, reload, now(self.now));
            self.execute(fx)?;
        }
        Ok(())
    }

    fn step(&mut self, s: Step) -> Result<(), TestCaseError> {
        self.next()?;
        let at = self.now;
        let fx = match s {
            Step::Ask { from, to, wait_s } => {
                let caller = if from == 3 {
                    moa()
                } else {
                    common::asks::from(SESSIONS[from])
                };
                let op = Op::Ask {
                    to: label(to).into(),
                    text: format!("question {at}"),
                    timeout_s: wait_s,
                };
                self.op(caller, op)
            }
            Step::Reply { ask, wait_s } => self.op(
                common::asks::from(KITE),
                Op::Reply {
                    id: ask,
                    timeout_s: wait_s,
                },
            ),
            Step::Prompt { who, marker } => {
                let prompt = match marker {
                    Some(id) => {
                        let label = self
                            .r
                            .st
                            .ask(AskId(id))
                            .and_then(|a| a.from_name.clone())
                            .unwrap_or_else(|| "Kite".into());
                        format!("[moa · message from {label} · ask #{id}] question")
                    }
                    None => "Look at the tide tables".into(),
                };
                self.r.started(at, SESSIONS[who], &prompt)
            }
            Step::Take(who) => {
                let me = Some(AskTarget::Session(sid(SESSIONS[who])));
                let mine: Vec<&Ask> = self.r.st.asks().filter(|a| a.to_session == me).collect();
                let typed = mine
                    .iter()
                    .find(|a| a.status == AskStatus::Sent)
                    .or_else(|| {
                        mine.iter()
                            .rev()
                            .find(|a| a.reply.as_deref() == Some(NO_RECEIPT))
                    })
                    .map(|a| framed(&self.r.st, a))
                    .unwrap_or_else(|| "Look at the tide tables".into());
                self.r.started(at, SESSIONS[who], &typed)
            }
            Step::Fail(who) => {
                let kind = HookKind::TurnFailed {
                    error: "rate limited".into(),
                };
                self.r.hook(at, SESSIONS[who], kind)
            }
            Step::Stop { who, empty } => {
                let message = if empty {
                    String::new()
                } else {
                    format!("answer {at}")
                };
                self.r.stopped(at, SESSIONS[who], &message)
            }
            Step::Timer => {
                self.now = self.armed.unwrap_or(self.now).max(self.now);
                self.r.fire(self.now)
            }
            Step::Advance(ms) => {
                self.now += ms;
                self.r.fire(self.now)
            }
            Step::Gone(n) => {
                let conns: BTreeSet<u64> = self.waiting.iter().map(|(c, _)| *c).collect();
                let Some(conn) = conns.iter().nth(n % conns.len().max(1)).copied() else {
                    return Ok(());
                };
                self.waiting.retain(|(c, _)| *c != conn);
                handle_checked(&mut self.r.st, Input::ClientGone(ConnId(conn)), now(at))
            }
            Step::Exit(who) => {
                let exited = format!("moa:hook pane-exited %{}", who + 1);
                let input = Input::Tmux(TmuxIn::Notification(Notification::Message(exited)));
                handle_checked(&mut self.r.st, input, now(at))
            }
            Step::RefuseNext => {
                self.refuse_next = true;
                return Ok(());
            }
            Step::Reload => {
                let reload = Input::Reload {
                    snapshot: Box::new(self.store.load_snapshot().unwrap()),
                    refuse_writes: None,
                };
                handle_checked(&mut self.r.st, reload, now(at))
            }
            Step::Restart => {
                self.waiting.clear();
                self.armed = None;
                let (seq, req) = (self.r.seq, self.r.req);
                let (r, fx) = Rig::started_at(self.store.load_snapshot().unwrap(), at);
                self.r = r;
                (self.r.seq, self.r.req) = (seq, req);
                self.execute(fx)?;
                self.check()?;
                self.now += 1;
                self.r.link_up(self.now)
            }
        };
        self.execute(fx)?;
        self.check()
    }

    /// `op` from `caller` on a new connection, at the moment.
    fn op(&mut self, caller: CallerInfo, op: Op) -> Vec<Effect> {
        self.conn += 1;
        let fx = self.r.op(self.conn, caller, op, self.now);
        self.waiting.insert((self.conn, self.r.req));
        fx
    }

    /// Runs `fx` as the daemon would: commits reach the disk, replies their clients, timers
    /// the scheduler. Each request is heard once, and an ask is typed only after the commit
    /// that claims it.
    fn execute(&mut self, fx: Vec<Effect>) -> Result<(), TestCaseError> {
        let mut claimed: BTreeSet<AskId> = BTreeSet::new();
        for e in fx {
            match e {
                Effect::Persist(changes) => {
                    for c in &changes {
                        if let moa_core::change::Change::Ask(a) = c
                            && a.status == AskStatus::Sent
                        {
                            claimed.insert(a.id);
                        }
                    }
                    self.store
                        .apply(&changes)
                        .expect("the disk takes the commit");
                }
                Effect::Reply { conn, req, .. } => {
                    let key = (conn.0, req.0);
                    prop_assert!(self.heard.insert(key), "{key:?} heard twice");
                    self.waiting.remove(&key);
                }
                Effect::SetTimer {
                    key: TimerKey::Asks,
                    at,
                } => self.armed = Some(at.0),
                Effect::CancelTimer(TimerKey::Asks) => self.armed = None,
                Effect::Tmux {
                    cmds, token: None, ..
                } => {
                    for c in cmds.iter().filter(|c| c.name() == "paste-buffer") {
                        let id: i64 = c.argv()[3]
                            .strip_prefix("moa-ask-")
                            .and_then(|n| n.parse().ok())
                            .expect("an ask's buffer");
                        prop_assert!(
                            claimed.contains(&AskId(id)),
                            "ask #{} typed without its claim",
                            id
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A1's moves only; one ask in flight per peer; no two agents waiting on each other; a
    /// typed ask taken or failed by its 20 s.
    fn check(&mut self) -> Result<(), TestCaseError> {
        use AskStatus::*;
        let asks: Vec<Ask> = self.r.st.asks().cloned().collect();
        for a in &asks {
            if let Some(before) = self.last.get(&a.id) {
                let revived = before.status == Failed
                    && before.reply.as_deref() == Some(NO_RECEIPT)
                    && a.status == Received;
                let moved = matches!(
                    (before.status, a.status),
                    (Pending, Sent | Dropped)
                        | (Sent, Received | Failed)
                        | (Received, Answered | Failed)
                );
                prop_assert!(
                    before.status == a.status || moved || revived,
                    "ask #{} went {} → {}",
                    a.id,
                    before.status,
                    a.status
                );
                if before.status == a.status && !moa_core::asks::is_open(a.status) {
                    prop_assert_eq!(&before.reply, &a.reply, "an ended ask's reply changed");
                }
            } else {
                // Saved, and claimed in the same input when its peer was free.
                prop_assert!(
                    matches!(a.status, Pending | Sent),
                    "new ask #{} is {}",
                    a.id,
                    a.status
                );
            }
            self.last.insert(a.id, a.clone());
        }
        let mut in_flight: BTreeMap<String, usize> = BTreeMap::new();
        for a in asks.iter().filter(|a| matches!(a.status, Sent | Received)) {
            *in_flight.entry(format!("{:?}", a.to_session)).or_default() += 1;
        }
        for (to, n) in &in_flight {
            prop_assert!(*n <= 1, "{} asks in flight to {:?}", n, to);
        }
        let open: Vec<&Ask> = asks
            .iter()
            .filter(|a| moa_core::asks::is_open(a.status))
            .collect();
        for a in &open {
            prop_assert!(
                !open
                    .iter()
                    .any(|b| b.from_session == a.to_session && b.to_session == a.from_session),
                "ask #{} and its peer's wait on each other",
                a.id
            );
        }
        if !self.refused {
            for a in asks.iter().filter(|a| a.status == Sent) {
                let by = a.claimed_at.unwrap().0 + ASK_RECEIPT_MS;
                prop_assert!(self.now < by, "ask #{} is still sent at its 20 s", a.id);
                prop_assert!(
                    self.armed.is_some_and(|at| at <= by),
                    "ask #{}'s 20 s has no timer: {:?}",
                    a.id,
                    self.armed
                );
            }
        }
        Ok(())
    }

    /// Time runs on until nothing waits: every request still connected has been heard.
    fn drain(&mut self) -> Result<(), TestCaseError> {
        for _ in 0..200 {
            if self.armed.is_none() {
                break;
            }
            self.step(Step::Timer)?;
        }
        prop_assert!(self.armed.is_none(), "the timer never settles");
        prop_assert!(self.waiting.is_empty(), "never heard: {:?}", self.waiting);
        Ok(())
    }
}

/// What an ask calls session `n`.
fn label(n: usize) -> &'static str {
    ["Kite", "Harbor", "Beacon"][n]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Review Focus 2: `asks_interleave_safely`.
    #[test]
    fn asks_interleave_safely(steps in prop::collection::vec(step(), 1..=60)) {
        let mut w = World::new()?;
        for s in steps {
            w.step(s)?;
        }
        w.drain()?;
    }
}
