//! Sleep (R18–R20): an idle session goes to sleep on its own at its deadline (its tab is
//! closed, the session stays resumable), and `sleep` puts sessions to sleep by name or by
//! how long they've been idle. On the typical brain with its link played by hand.

mod common;

use std::cell::Cell;
use std::path::PathBuf;

use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Down, Effect, Input, Publish, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::hook::{HookEnvelope, HookKind, StartSource};
use moa_core::live::{DisplayState, display_state, is_asleep};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Ask, AskId, AskStatus, AskTarget, Conversation,
    ConversationReason, Inspected, Lifecycle, Location, PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Topic};
use moa_core::sleep::{SLEEP_AFTER_USAGE, SLEEP_KEY, why_not};
use moa_core::state::{Deps, EventWhat, LoadNote, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use proptest::prelude::*;
use serde_json::json;

use common::*;

const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

const HOUR: i64 = 3_600_000;
/// The default threshold.
const SIX: i64 = 6 * HOUR;

/// The running session S, and its conversation.
const S: &str = "00000000-0000-4000-8000-0000000000d1";
const C: &str = "d0000000-0000-4000-8000-0000000000d1";
const TITLE: &str = "boards";
/// An ended session.
const E: &str = "00000000-0000-4000-8000-0000000000d2";

fn s_id() -> SessionId {
    S.parse().unwrap()
}

fn shown(p: &std::path::Path) -> String {
    p.display().to_string()
}

thread_local! {
    /// Whether S's adopted agent still runs in its shell tab.
    static AGENT_RUNS: Cell<bool> = const { Cell::new(true) };
    /// Whether S's tab's process can't be read.
    static UNREADABLE: Cell<bool> = const { Cell::new(false) };
}

/// S's agent in its tab (pid 101), started by hand: no `MOA_SESSION`, while it runs.
fn s_by_hand(pid: i64, _: &[String]) -> Inspected {
    if pid == 101 && UNREADABLE.with(Cell::get) {
        return Inspected::Unknown;
    }
    Inspected::from(
        (pid == 101 && AGENT_RUNS.with(Cell::get)).then(|| PaneAgent {
            kind: AgentKind::Claude,
            store_dir: None,
            conversation: Some(C.into()),
            moa_session: None,
            moa_brain: None,
            title: None,
            group: None,
            started_at: None,
        }),
    )
}

/// The cockpit `@1`, built (its console panel `%0`), which the user is looking at, and S's
/// tab `%1` in `@2` behind it.
fn base_panes() -> Vec<String> {
    vec![
        "P %0 @1 $0 1 1 0 1  0 console".into(),
        format!("P %1 @2 $0 1 0 0 0 {S}"),
    ]
}

/// The user's terminal on session `$0`, and moa's own control client.
const CLIENTS: &[&str] = &["C 0 $0 /dev/ttys1", "C 1 $1 client-9"];

fn lists(panes: &[String]) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: listing_of(panes),
    }
}

/// A listing's four answers: sessions `moa` and `_moa`, `panes`, [`CLIENTS`], and no
/// terminals.
fn listing_of(panes: &[String]) -> Vec<Result<Vec<String>, String>> {
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    vec![
        lines(&["S $0 moa", "S $1 _moa"]),
        Ok(panes.to_vec()),
        lines(CLIENTS),
        Ok(Vec::new()),
    ]
}

fn window_changed(window: u32) -> TmuxIn {
    TmuxIn::Notification(Notification::SessionWindowChanged {
        session: TmuxSessionId(0),
        window: WindowId(window),
    })
}

/// The world before loading: the typical brain plus S, live and idle in
/// `<home>/work/lantern`, bound to tab `%1` of this server at `B` (moa started it), heard
/// from at `B + 1s`, and idle since `T`.
struct World {
    b: TypicalBrain,
    toml: String,
    s: Session,
    binding: Option<PaneBinding>,
    others: Vec<Session>,
    asks: Vec<Ask>,
    deps: Deps,
    /// The topology refresh's pane lines.
    panes: Vec<String>,
    /// Whether the tabs are reconciled.
    reconcile: bool,
    /// S's row was loaded with a value this build doesn't know: no write to it is saved.
    unknown: bool,
}

impl World {
    fn new() -> World {
        let b = TypicalBrain::new();
        let lantern = b.home.join("work/lantern");
        let n = b.now().wall.0;
        let (bound, idle) = (UtcMs(n - 3 * HOUR), UtcMs(n - HOUR));
        let s = Session {
            id: s_id(),
            agent_kind: AgentKind::Claude,
            account: Some("acct-a".into()),
            cwd: Some(shown(&lantern)),
            start_cwd: Some(shown(&lantern)),
            project_id: None,
            title: Some(TITLE.into()),
            title_locked: false,
            group_path: None,
            lifecycle: Lifecycle::Live,
            activity: Activity::Idle,
            activity_at: Some(idle),
            activity_why: Some("All set.".into()),
            last_prompt: None,
            last_turn_end_at: Some(UtcMs(idle.0 - 1000)),
            seen_at: Some(idle),
            asleep_at: None,
            archived_at: None,
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(1),
            last_seen_at: Some(UtcMs(bound.0 + 1000)),
        };
        let binding = PaneBinding {
            session_id: s_id(),
            server_pid: generation().pid,
            server_start: generation().start,
            socket: "moa".into(),
            pane_id: "%1".into(),
            pane_pid: 101,
            bound_at: bound,
            tracked: true,
        };
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            ..Deps::default()
        };
        World {
            b,
            toml: TOML.into(),
            s,
            binding: Some(binding),
            others: Vec::new(),
            asks: Vec::new(),
            deps,
            panes: base_panes(),
            reconcile: true,
            unknown: false,
        }
    }

    /// S adopted: an untracked binding, its agent checked by `inspect`.
    fn adopted() -> World {
        let mut w = World::new();
        w.binding.as_mut().unwrap().tracked = false;
        w.deps.inspect = s_by_hand;
        w
    }

    /// S asleep, with no tab.
    fn asleep() -> World {
        let mut w = World::new();
        w.s.lifecycle = Lifecycle::Ended;
        w.s.asleep_at = w.s.activity_at;
        w.binding = None;
        w.panes.truncate(1);
        w
    }

    /// When S is due: `T + 6h`.
    fn due(&self) -> UtcMs {
        UtcMs(self.s.activity_at.unwrap().0 + SIX)
    }

    fn snapshot(&self) -> Snapshot {
        let conv = Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: C.into(),
            session_id: s_id(),
            store_dir: Some(shown(&self.b.home.join(".claude-a"))),
            spike_account: None,
            reason: ConversationReason::Start,
            first_seen_at: UtcMs(1),
            last_seen_at: UtcMs(1),
            transcript_path: None,
            transcript_present: None,
            transcript_checked_at: None,
        };
        let mut snap = self.b.snapshot();
        snap.sessions.push(self.s.clone());
        snap.sessions.extend(self.others.iter().cloned());
        snap.conversations.push(conv);
        snap.pane_bindings.extend(self.binding.clone());
        snap.asks.extend(self.asks.iter().cloned());
        if self.unknown {
            snap.unknown.push(LoadNote {
                table: "sessions",
                key: S.into(),
                reason: "a state this build doesn't know",
            });
        }
        snap
    }

    /// The reconcile listing's line for S's tab.
    fn info(&self) -> Option<String> {
        let cwd = self.s.cwd.clone().unwrap();
        self.binding.as_ref().map(|b| {
            let adopted = if b.tracked { "" } else { "1" };
            format!(
                "I\t{}\t{}\t0\t\t{adopted}\t{S}\t{cwd}",
                b.pane_id, b.pane_pid
            )
        })
    }

    /// Loaded, linked, drained and (unless not asked to) reconciled, with the topology in.
    fn up(self) -> Rig {
        let mut st = loaded_with(&self.b, &self.toml, self.deps.clone(), self.snapshot());
        moa_answers(&mut st, self.b.now());
        let mut setup = Vec::new();
        setup.extend(feed(
            &self.b,
            &mut st,
            TmuxIn::Connected {
                conn: 1,
                generation: generation(),
                version: "3.5a".into(),
                ctl: ClientName("client-9".into()),
                socket: "moa".into(),
            },
        ));
        setup.extend(handle_checked(&mut st, Input::StartDrained, self.b.now()));
        if self.reconcile {
            let info = self.info();
            let lines: Vec<&str> = info.iter().map(String::as_str).collect();
            setup.extend(listed(&mut st, &self.b, &lines, &[]));
            assert!(st.tmux().reconciled());
        }
        setup.extend(feed(&self.b, &mut st, lists(&self.panes)));
        let due = self.due();
        Rig {
            b: self.b,
            st,
            setup,
            due,
            panes: self.panes,
        }
    }
}

struct Rig {
    b: TypicalBrain,
    st: State,
    /// Every effect of loading, linking and reconciling.
    setup: Vec<Effect>,
    due: UtcMs,
    /// The pane lines tmux lists when the cockpit's queue asks.
    panes: Vec<String>,
}

/// A query the cockpit's queue asked in `e` (L17): its number, and whether it is a listing
/// (else a chain).
fn cockpit_asked(e: &Effect) -> Option<(u64, bool)> {
    match e {
        Effect::Tmux {
            cmds,
            token: Some(TmuxToken::Query(n)),
            ..
        } if *cmds == refresh_cmds() => Some((*n, true)),
        Effect::Tmux {
            cmds,
            token: Some(TmuxToken::Query(n)),
            ..
        } if cmds.len() == 1 && cmds[0].name() == "chain" => Some((*n, false)),
        _ => None,
    }
}

impl Rig {
    fn s(&self) -> &Session {
        self.st.session(s_id()).unwrap()
    }

    /// Why S can't sleep at `wall` under the default threshold.
    fn why(&self, wall: UtcMs) -> Option<&'static str> {
        why_not(&self.st, s_id(), wall, SIX)
    }

    /// `input` at `wall`, the cockpit's queue [`played`](Rig::played).
    fn input(&mut self, wall: UtcMs, input: Input) -> Vec<Effect> {
        let fx = handle_checked(&mut self.st, input, at(wall));
        self.played(wall, fx)
    }

    /// `sleep`, the cockpit's queue [`played`](Rig::played).
    fn sleep_op(
        &mut self,
        req: u64,
        qs: &[&str],
        idle_ms: Option<i64>,
        dry_run: bool,
    ) -> Vec<Effect> {
        let fx = self.sleep_unplayed(req, qs, idle_ms, dry_run);
        self.played(self.b.now().wall, fx)
    }

    fn sleep_unplayed(
        &mut self,
        req: u64,
        qs: &[&str],
        idle_ms: Option<i64>,
        dry_run: bool,
    ) -> Vec<Effect> {
        let op = Op::SessionSleep {
            qs: qs.iter().map(|q| q.to_string()).collect(),
            idle_ms,
            dry_run,
        };
        send(&self.b, &mut self.st, req, op)
    }

    /// tmux, for the cockpit's queue: with a cockpit, a sleep's kill is the queue's vacate
    /// (5b's L17). Each listing the queue asks in `fx` is answered with the rig's panes and
    /// each chain with `Ok`, as the terminal server would; `fx`, then what those answers did.
    fn played(&mut self, wall: UtcMs, mut fx: Vec<Effect>) -> Vec<Effect> {
        let mut i = 0;
        while i < fx.len() {
            if let Some((n, listing)) = cockpit_asked(&fx[i]) {
                let results = match listing {
                    true => listing_of(&self.panes),
                    false => vec![Ok(Vec::new())],
                };
                let answer = TmuxIn::Replies {
                    conn: 1,
                    token: TmuxToken::Query(n),
                    results,
                };
                let more = handle_checked(&mut self.st, Input::Tmux(answer), at(wall));
                fx.extend(more);
            }
            i += 1;
        }
        fx
    }
}

fn kill(pane: &str) -> Cmd {
    Cmd::new("kill-pane").args(["-t", pane])
}

/// Every tmux command sent, untokened or not, a chain's one by one.
fn sent(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter().flat_map(Cmd::parts)),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Where `fx` kills tab `pane`: a plain kill, or in a vacate's chain.
fn killed_at(fx: &[Effect], pane: &str) -> Option<usize> {
    fx.iter().position(|e| match e {
        Effect::Tmux { conn: 1, cmds, .. } => {
            cmds.iter().flat_map(Cmd::parts).any(|c| *c == kill(pane))
        }
        _ => false,
    })
}

/// Where `fx` asks the cockpit's queue's listing (L17).
fn vacate_listed_at(fx: &[Effect]) -> Option<usize> {
    fx.iter()
        .position(|e| cockpit_asked(e).is_some_and(|(_, listing)| listing))
}

fn kills(fx: &[Effect]) -> Vec<&Cmd> {
    sent(fx)
        .into_iter()
        .filter(|c| c.name() == "kill-pane")
        .collect()
}

/// The sessions an input put to sleep.
fn slept(fx: &[Effect]) -> Vec<SessionId> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Session(s) if s.lifecycle == Lifecycle::Ended && s.asleep_at.is_some() => {
                Some(s.id)
            }
            _ => None,
        })
        .collect()
}

fn reply_text(fx: &[Effect]) -> String {
    match &replies(fx)[..] {
        [(_, Ok(done))] => done.text.clone(),
        other => panic!("one answer: {other:?}"),
    }
}

// --- eligibility ---

/// R18, one condition at a time: each case flips one thing from the eligible base, and
/// `why_not` names it.
#[test]
fn sleep_eligibility_table() {
    let base = World::new().up();
    assert_eq!(base.why(base.due), None, "the base is eligible");

    let mut cases: Vec<(&str, World, &str)> = Vec::new();
    let mut w = World::new();
    w.panes = vec![
        "P %0 @1 $0 1 0 0 1 ".into(),
        format!("P %1 @2 $0 1 1 0 0 {S}"),
    ];
    cases.push(("visible", w, "on screen"));
    let mut w = World::new();
    w.s.last_turn_end_at = Some(UtcMs(w.s.seen_at.unwrap().0 + 1000));
    cases.push(("done", w, "not idle"));
    let mut w = World::new();
    w.s.activity_why = Some("Shall I order more?".into());
    cases.push(("a question", w, "not idle"));
    let mut w = World::new();
    w.s.activity = Activity::Working;
    cases.push(("working, and quiet", w, "not idle"));
    let mut w = World::new();
    w.s.activity = Activity::Failed;
    cases.push(("failed", w, "not idle"));
    for (to, from) in [(true, false), (false, true)] {
        let mut w = World::new();
        if from {
            w.toml = moa_runs(TOML);
        }
        let me = Some(AskTarget::Session(s_id()));
        w.asks.push(Ask {
            id: AskId(900),
            at: if from {
                UtcMs(w.b.now().wall.0 - MINUTE)
            } else {
                UtcMs(1)
            },
            from_session: if from { me } else { Some(AskTarget::Moa) },
            from_name: None,
            to_session: if to { me } else { Some(AskTarget::Moa) },
            text: "which boards?".into(),
            status: if from {
                AskStatus::Pending
            } else {
                AskStatus::Received
            },
            reply: None,
            claimed_at: None,
            answered_at: None,
        });
        cases.push((
            if to { "an ask to it" } else { "an ask from it" },
            w,
            "in an ask",
        ));
    }
    cases.push(("untracked", World::adopted(), "not tracked"));
    let mut w = World::new();
    w.s.last_seen_at = Some(w.binding.as_ref().unwrap().bound_at);
    cases.push(("never heard from", w, "never heard from"));
    let mut w = World::new();
    w.panes.truncate(1);
    cases.push(("not in the topology", w, "not listed"));
    let mut w = World::new();
    w.panes[1] = "P %1 @2 $0 1 0 0 0 ".into();
    cases.push(("untagged", w, "not tagged"));
    let mut w = World::new();
    w.s.archived_at = Some(UtcMs(2));
    cases.push(("archived", w, "archived"));
    let mut w = World::new();
    w.s.location = Location::Elsewhere;
    cases.push(("elsewhere", w, "elsewhere"));
    let mut w = World::new();
    w.reconcile = false;
    cases.push(("not reconciled", w, "not reconciled"));
    for (name, w, want) in cases {
        let r = w.up();
        assert_eq!(r.why(r.due), Some(want), "{name}");
    }

    // In the cockpit's spot, off screen: it can sleep, and the empty slot takes its place
    // (5b's L11).
    let mut w = World::new();
    w.panes = vec![
        "P %0 @1 $0 1 0 0 1 ".into(),
        format!("P %1 @1 $0 0 0 0 1 {S} 0 agent"),
        "P %5 @3 $0 1 1 0 0 ".into(),
    ];
    let r = w.up();
    assert_eq!(r.why(r.due), None, "in the spot");

    // Working, but not quiet yet.
    let mut w = World::new();
    w.s.activity = Activity::Working;
    let r = w.up();
    assert_eq!(r.why(r.s().activity_at.unwrap()), Some("not idle"));
    // `now < T + 6h`.
    assert_eq!(base.why(UtcMs(base.due.0 - 1)), Some("too soon"));
    // A binding of another server: the link reconnects to a new one.
    let mut r = World::new().up();
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: Generation {
                pid: 11,
                start: 2000,
            },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    assert_eq!(r.why(r.due), Some("no tab here"), "old generation");
    // The same server again, on a new connection: bound, but not reconciled yet.
    let mut r = World::new().up();
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    assert_eq!(r.why(r.due), Some("not reconciled"));
    // A launch of S is waiting.
    let r = waiting_launch();
    assert_eq!(r.why(r.due), Some("opening"));
}

/// S, eligible in every other way, with a resume of it still waiting for its tab: resumed
/// from asleep, then reloaded with its binding back, since no input from outside makes a
/// bound session's launch wait. The reload shows the resume's commit (R13a's times), or it
/// would answer the launch as lost; S is due six hours after it.
fn waiting_launch() -> Rig {
    let mut w = World::new();
    let launched = w.b.now().wall;
    let s = w.s.clone();
    w.s.activity_at = Some(launched);
    w.s.last_seen_at = Some(launched);
    let (bound, info) = (w.snapshot(), w.info().unwrap());
    w.s = s;
    w.s.lifecycle = Lifecycle::Ended;
    w.s.asleep_at = w.s.activity_at;
    w.binding = None;
    w.panes.truncate(1);
    let mut r = w.up();
    let fx = send(
        &r.b,
        &mut r.st,
        2,
        Op::SessionResume {
            q: S.into(),
            bg: true,
        },
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = listed(&mut r.st, &r.b, &[], &[]);
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::Tmux {
                token: Some(TmuxToken::Open(id)),
                ..
            } if *id == s_id()
        )),
        "{fx:?}"
    );
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(bound),
            refuse_writes: None,
        },
        r.b.now(),
    );
    listed(&mut r.st, &r.b, &[&info], &[]);
    feed(&r.b, &mut r.st, lists(&base_panes()));
    assert!(r.st.tmux().reconciled());
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    r.due = UtcMs(launched.0 + SIX);
    r
}

// --- on its own ---

#[test]
fn an_idle_session_sleeps_at_its_deadline() {
    let mut r = World::new().up();
    let due = r.due;
    assert_eq!(
        timers(&r.setup, TimerKey::Sleep).last(),
        Some(&&Effect::SetTimer {
            key: TimerKey::Sleep,
            at: due
        }),
        "{:?}",
        r.setup
    );
    let fx = r.input(due, Input::Timer(TimerKey::Sleep));

    let persists: Vec<usize> = fx
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Effect::Persist(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(persists, [0], "{fx:?}");
    let saved = changes(&fx);
    assert!(
        saved.iter().any(|c| matches!(c, Change::Session(s)
            if s.id == s_id()
                && s.lifecycle == Lifecycle::Ended
                && s.asleep_at == Some(due)
                && s.activity_at == r.s().activity_at)),
        "{fx:?}"
    );
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(id) if *id == s_id())),
        "{fx:?}"
    );
    let line = format!("💤 put 1 idle session to sleep: {TITLE} · resume from the tree");
    let rows: Vec<_> = saved
        .iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some((a.who, a.text.clone(), a.at)),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [(ActivityWho::Brain, line.clone(), due)]);
    assert!(
        fx.iter().any(|e| matches!(e,
            Effect::Publish(Publish::Event(CoreEvent::Ui { text })) if *text == line)),
        "{fx:?}"
    );

    // With the cockpit there, the kill is the queue's vacate: its listing, then its chain.
    let listed = vacate_listed_at(&fx).expect("the vacate's listing");
    let killed = killed_at(&fx, "%1").expect("the tab is killed");
    let cancelled = fx
        .iter()
        .position(|e| matches!(e, Effect::CancelTimer(TimerKey::Sleep)))
        .expect("no deadline is left");
    assert!(
        0 < listed && listed < cancelled && listed < killed,
        "{fx:?}"
    );
    assert_eq!(kills(&fx).len(), 1);
    assert!(is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());

    // Nothing more happens later; a fired timer is never taken for an armed one.
    let fx = r.input(UtcMs(due.0 + HOUR), Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        [&Effect::CancelTimer(TimerKey::Sleep)]
    );
}

#[test]
fn sleeping_at_the_deadline_is_an_asleep_event() {
    let mut r = World::new().up();
    let due = r.due;
    let fx = r.input(due, Input::Timer(TimerKey::Sleep));
    let events: Vec<_> = r.st.session_events().map(|e| (e.session, e.what)).collect();
    assert_eq!(
        events.last(),
        Some(&(s_id(), EventWhat::Asleep)),
        "{events:?}"
    );
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Publish(Publish::Model(m))
            if m.topics[&Topic::Feed].values().any(|row| row["what"] == "asleep"))),
        "{fx:?}"
    );
}

/// The scheduler forgets a deadline when it fires. Should the core's clock read earlier than
/// the deadline (a step back), the same deadline is armed again, not taken as still armed.
#[test]
fn a_timer_that_fires_early_is_armed_again() {
    let mut r = World::new().up();
    let due = r.due;
    let fx = r.input(UtcMs(due.0 - 60_000), Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        [&Effect::SetTimer {
            key: TimerKey::Sleep,
            at: due
        }]
    );
    // Any other input leaves an unchanged deadline alone.
    let fx = r.input(UtcMs(due.0 - 30_000), Input::Timer(TimerKey::Clock));
    assert!(timers(&fx, TimerKey::Sleep).is_empty(), "{fx:?}");
}

/// R19: due while on screen, it doesn't sleep, and no timer is armed for it. The input that
/// takes it off the screen puts it to sleep.
#[test]
fn an_overdue_session_on_screen_sleeps_when_it_leaves_the_screen() {
    let mut w = World::new();
    w.panes = vec![
        "P %0 @1 $0 1 0 0 1  0 console".into(),
        format!("P %1 @2 $0 1 1 0 0 {S}"),
    ];
    let mut r = w.up();
    let due = r.due;
    let fx = r.input(due, Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        [&Effect::CancelTimer(TimerKey::Sleep)],
        "no timer for S"
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Live);

    // An hour on, the user goes to the cockpit.
    let later = UtcMs(due.0 + HOUR);
    let fx = r.input(later, Input::Tmux(window_changed(1)));
    assert_eq!(slept(&fx), [s_id()], "{fx:?}");
    assert_eq!(kills(&fx), [&kill("%1")]);
    assert_eq!(r.s().asleep_at, Some(later));
    assert!(timers(&fx, TimerKey::Sleep).is_empty(), "{fx:?}");
}

#[test]
fn zero_means_never() {
    let mut w = World::new();
    w.toml = TOML.replace(
        "open_account = \"acct-a\"\n",
        "open_account = \"acct-a\"\nsleep_after_hours = 0\n",
    );
    let mut r = w.up();
    assert!(
        timers(&r.setup, TimerKey::Sleep).is_empty(),
        "{:?}",
        r.setup
    );
    let fx = r.input(UtcMs(r.due.0 + 100 * HOUR), Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(timers(&fx, TimerKey::Sleep).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// `/sleep` (`console.sleep_after`) with `value`, as request `req`.
fn sleep_after(r: &mut Rig, req: u64, value: Option<&str>) -> Vec<Effect> {
    let op = Op::ConsoleSleepAfter {
        value: value.map(str::to_string),
    };
    send(&r.b, &mut r.st, req, op)
}

/// `/sleep` changes the wait live (§6.13): the deadline moves with it. `off` is never, `on`
/// is moa.toml's again, and a bare `/sleep` says the wait and saves nothing.
#[test]
fn sleep_sets_the_threshold() {
    let mut r = World::new().up();
    let idle = r.s().activity_at.unwrap().0;
    let asleep = r.st.sessions().filter(|s| is_asleep(s)).count();
    let after =
        |wait: &str| format!("idle sessions go to sleep after {wait} · {asleep} asleep now");
    let saved = |value: Option<String>| {
        vec![Change::Meta {
            key: SLEEP_KEY.into(),
            value,
        }]
    };
    let set = |at: i64| {
        vec![Effect::SetTimer {
            key: TimerKey::Sleep,
            at: UtcMs(at),
        }]
    };

    let fx = sleep_after(&mut r, 1, Some("2"));
    assert_eq!(reply_text(&fx), after("2 h"));
    assert_eq!(
        changes(&fx),
        saved(Some((2 * HOUR).to_string()))
            .iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        set(idle + 2 * HOUR).iter().collect::<Vec<_>>()
    );

    // Said, not changed.
    let fx = sleep_after(&mut r, 2, None);
    assert_eq!(reply_text(&fx), after("2 h"));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(timers(&fx, TimerKey::Sleep).is_empty(), "{fx:?}");

    let fx = sleep_after(&mut r, 3, Some("1.5"));
    assert_eq!(reply_text(&fx), after("90 min"));
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        set(idle + 3 * HOUR / 2).iter().collect::<Vec<_>>()
    );

    let fx = sleep_after(&mut r, 4, Some("off"));
    assert_eq!(reply_text(&fx), "idle sessions stay up (/sleep on)");
    assert_eq!(
        changes(&fx),
        saved(Some("0".into())).iter().collect::<Vec<_>>()
    );
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        [&Effect::CancelTimer(TimerKey::Sleep)]
    );
    // Never, even long after.
    let fx = r.input(UtcMs(r.due.0 + 100 * HOUR), Input::Timer(TimerKey::Sleep));
    assert!(slept(&fx).is_empty(), "{fx:?}");

    let fx = sleep_after(&mut r, 5, Some("on"));
    assert_eq!(reply_text(&fx), after("6 h"));
    assert_eq!(changes(&fx), saved(None).iter().collect::<Vec<_>>());
    assert_eq!(
        timers(&fx, TimerKey::Sleep),
        set(r.due.0).iter().collect::<Vec<_>>()
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Live);

    // A bad value is refused, typed or sent, and changes nothing.
    let usage = Err(OpError(SLEEP_AFTER_USAGE.into()));
    for value in ["x", "-1", "nan", "inf", ""] {
        let fx = sleep_after(&mut r, 6, Some(value));
        assert_eq!(replies(&fx), [(ReqId(6), usage.clone())], "{value}");
        assert!(changes(&fx).is_empty(), "{fx:?}");
    }
    let line = Op::ConsoleCommand {
        line: "/sleep x".into(),
    };
    let fx = send(&r.b, &mut r.st, 7, line);
    assert_eq!(replies(&fx), [(ReqId(7), usage)]);
}

/// `/sleep`'s reply says the wait as the watchdog's card does ([`moa_core::time::words`]):
/// whole hours in hours, else minutes or seconds, so a tiny wait is never `0 h`.
#[test]
fn sleep_says_the_wait_in_words() {
    for (value, wait) in [
        ("1", "1 h"),
        ("24", "24 h"),
        ("2.5", "150 min"),
        ("0.5", "30 min"),
        ("0.001", "3 s"),
    ] {
        let mut r = World::new().up();
        let fx = sleep_after(&mut r, 1, Some(value));
        let text = reply_text(&fx);
        let want = format!("idle sessions go to sleep after {wait} · ");
        assert!(text.starts_with(&want), "{value}: {text}");
    }
}

/// R15: a sleep the store can't save kills nothing.
#[test]
fn sleep_with_writes_refused_kills_nothing() {
    // S's row can't be written.
    let mut w = World::new();
    w.unknown = true;
    let mut r = w.up();
    assert_eq!(r.why(r.due), None);
    let fx = r.input(r.due, Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(r.st.binding(s_id()).is_some());

    // Nothing can be written.
    let mut r = World::new().up();
    let mut snap = r.b.snapshot();
    snap.sessions.push(r.s().clone());
    snap.pane_bindings.extend(r.st.binding(s_id()).cloned());
    let due = r.due;
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(due.0 + HOUR),
                reason: "disk full".into(),
            }),
        },
        r.b.now(),
    );
    let fx = r.input(due, Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// The cockpit is the window named `moa` in the session `moa`: a window renamed into it or
/// out of it is looked at again (the Task 4 minor), straight from the notification, with no
/// listing (5b's L16).
#[test]
fn a_renamed_window_moves_the_cockpit() {
    let mut w = World::new();
    w.panes = base_panes();
    let mut r = w.up();
    let cockpit = |r: &Rig| r.st.tmux().topology.cockpit();
    let rename = |r: &mut Rig, window: u32, name: &str| {
        let n = Notification::WindowRenamed {
            window: WindowId(window),
            name: name.into(),
        };
        let fx = feed(&r.b, &mut r.st, TmuxIn::Notification(n));
        let refreshes = fx.iter().filter(|e| {
            matches!(
                e,
                Effect::Tmux {
                    token: Some(TmuxToken::Refresh),
                    ..
                }
            )
        });
        assert_eq!(refreshes.count(), 0, "{fx:?}");
    };
    // The cockpit's own window gets another name, and S's tab's window is renamed `moa`.
    assert_eq!(cockpit(&r), Some(WindowId(1)));
    rename(&mut r, 1, "Kite");
    rename(&mut r, 2, "moa");
    assert_eq!(cockpit(&r), Some(WindowId(2)));
    // And back.
    rename(&mut r, 2, "Tide tables");
    rename(&mut r, 1, "moa");
    assert_eq!(cockpit(&r), Some(WindowId(1)));
}

/// 5a's carry: with a cockpit, a sleep's kill waits in the cockpit's queue. The link drops
/// before it goes and comes back to the same terminal server; the kill then goes, after a
/// listing of its own, and a late start from the killed tab is still history only.
#[test]
fn a_late_start_from_the_killed_tab_stays_history_across_a_reconnect_to_the_same_server() {
    let mut r = World::new().up();
    let fx = r.sleep_unplayed(1, &[S], None, false);
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(vacate_listed_at(&fx).is_some(), "{fx:?}");
    assert!(is_asleep(r.s()));
    let slept_at = r.b.now().wall;
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    // The vacate's listing, on the new connection.
    let listing = fx.iter().find_map(|e| match e {
        Effect::Tmux {
            conn: 2,
            cmds,
            token: Some(TmuxToken::Query(n)),
        } if *cmds == refresh_cmds() => Some(*n),
        _ => None,
    });
    let n = listing.unwrap_or_else(|| panic!("no listing: {fx:?}"));
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 2,
            token: TmuxToken::Query(n),
            results: listing_of(&base_panes()),
        },
    );
    assert_eq!(kills(&fx), [&kill("%1")], "{fx:?}");

    let late = UtcMs(slept_at.0 + 1000);
    let fx = start_from(&mut r, "%1", late, UtcMs(late.0 + 1000), 101);
    let saved = changes(&fx);
    assert!(
        matches!(saved.as_slice(), [Change::Event(e)] if e.session_id == Some(s_id())),
        "{fx:?}"
    );
    assert!(is_asleep(r.s()), "{:?}", r.s());
}

/// I1: a sleep's or close's take-down waits out a lost link; back on the same server, the
/// reconnect's reconcile is answered first and sees the tab still tagged with S. It isn't
/// bound again: S stays asleep (or closed), and the vacate then takes the agent down.
#[test]
fn a_take_down_waiting_out_a_reconnect_isnt_undone_by_reconcile() {
    for close in [false, true] {
        let mut r = World::new().up();
        let fx = if close {
            send(&r.b, &mut r.st, 1, Op::SessionClose { q: S.into() })
        } else {
            r.sleep_unplayed(1, &[S], None, false)
        };
        assert!(kills(&fx).is_empty(), "{fx:?}");
        feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
        let fx = feed(
            &r.b,
            &mut r.st,
            TmuxIn::Connected {
                conn: 2,
                generation: generation(),
                version: "3.5a".into(),
                ctl: ClientName("client-9".into()),
                socket: "moa".into(),
            },
        );
        let asked = queries(&fx);
        let find = |listing: bool| {
            asked
                .iter()
                .find(|(_, cmds)| (*cmds == refresh_cmds()) == listing)
                .map(|(n, _)| *n)
                .unwrap_or_else(|| panic!("{asked:?}"))
        };
        let (reconcile, vacate) = (find(false), find(true));
        // The reconcile's look at the tabs first: %1 still runs S's agent, tagged with S.
        let cwd = r.s().cwd.clone().unwrap();
        let info = format!("I\t%1\t101\t0\t\t\t{S}\t{cwd}");
        feed(
            &r.b,
            &mut r.st,
            TmuxIn::Replies {
                conn: 2,
                token: TmuxToken::Query(reconcile),
                results: vec![Ok(vec![info]), Ok(Vec::new())],
            },
        );
        assert!(r.st.tmux().reconciled());
        assert!(r.st.binding(s_id()).is_none(), "bound again: close {close}");
        let still = |r: &Rig| match close {
            true => r.s().lifecycle == Lifecycle::Ended && r.s().asleep_at.is_none(),
            false => is_asleep(r.s()),
        };
        assert!(still(&r), "close {close}: {:?}", r.s());
        // Then the vacate's own listing: the agent is taken down.
        let fx = feed(
            &r.b,
            &mut r.st,
            TmuxIn::Replies {
                conn: 2,
                token: TmuxToken::Query(vacate),
                results: listing_of(&base_panes()),
            },
        );
        assert_eq!(kills(&fx), [&kill("%1")], "{fx:?}");
        assert!(still(&r), "close {close}: {:?}", r.s());
    }
}

// --- by name ---

#[test]
fn sleep_by_name() {
    let mut w = World::new();
    let mut ended = w.s.clone();
    ended.id = E.parse().unwrap();
    ended.title = Some("tides".into());
    ended.lifecycle = Lifecycle::Ended;
    w.others.push(ended);
    let mut r = w.up();
    let fx = r.sleep_op(1, &[S, E], None, false);
    assert_eq!(
        reply_text(&fx),
        format!(
            "tides isn't running in moa (ended); nothing to put to sleep\n\
             asleep: {TITLE} (resumable: moactl resume ID)"
        )
    );
    assert_eq!(slept(&fx), [s_id()]);
    assert_eq!(kills(&fx), [&kill("%1")]);
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let killed = killed_at(&fx, "%1").unwrap();
    assert!(persist < killed, "{fx:?}");
    assert!(is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());
    let [(_, Ok(done))] = &replies(&fx)[..] else {
        panic!("{fx:?}");
    };
    assert_eq!(done.data, json!({ "asleep": [S] }));

    // An adopted session whose agent is there: the user asked, so it sleeps.
    AGENT_RUNS.with(|c| c.set(true));
    let mut r = World::adopted().up();
    let fx = r.sleep_op(1, &[S], None, false);
    assert_eq!(
        reply_text(&fx),
        format!("asleep: {TITLE} (resumable: moactl resume ID)")
    );
    assert_eq!(kills(&fx), [&kill("%1")]);
    assert!(is_asleep(r.s()));

    // One whose agent left its tab: R31's line, asleep, and its tab left alone.
    let mut r = World::adopted().up();
    AGENT_RUNS.with(|c| c.set(false));
    let fx = r.sleep_op(1, &[S], None, false);
    AGENT_RUNS.with(|c| c.set(true));
    assert_eq!(
        reply_text(&fx),
        format!("{TITLE}'s agent isn't running in that tab any more")
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        unsets(&fx),
        untagged("%1").iter().collect::<Vec<_>>(),
        "{fx:?}"
    );
    assert!(is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());
    // It is asleep, so a script reading `asleep` sees it.
    let [(_, Ok(done))] = &replies(&fx)[..] else {
        panic!("{fx:?}");
    };
    assert_eq!(done.data, json!({ "asleep": [S] }));

    // One whose tab can't be read: nothing proves its agent is there or gone, so it is
    // left as it is, and the reply says why.
    let mut r = World::adopted().up();
    UNREADABLE.with(|c| c.set(true));
    let before = r.s().clone();
    let fx = r.sleep_op(1, &[S], None, false);
    UNREADABLE.with(|c| c.set(false));
    assert_eq!(
        reply_text(&fx),
        format!("moa couldn't read what runs in {TITLE}'s tab; try again in a moment")
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(unsets(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s(), &before);
    assert!(r.st.binding(s_id()).is_some());
    let [(_, Ok(done))] = &replies(&fx)[..] else {
        panic!("{fx:?}");
    };
    assert_eq!(done.data, json!({ "asleep": [] }));
    // moa's log says so, as restart's and resume's do.
    let said = format!("the tab of session {S} couldn't be read; it is left as it is");
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Log(l) if l.message == said)),
        "{fx:?}"
    );

    // A name that matches nothing changes nothing.
    let mut r = World::new().up();
    let fx = r.sleep_op(1, &[S, "no-such-session"], None, false);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx)[0].1.is_err());
}

/// A `SessionStart` for S from tab `pane`, naming S in `MOA_SESSION`, stamped `at` and
/// handled at `wall`.
fn start_from(r: &mut Rig, pane: &str, at: UtcMs, wall: UtcMs, pid: u32) -> Vec<Effect> {
    r.input(
        wall,
        Input::Hook {
            reply: None,
            envelope: HookEnvelope {
                agent: AgentKind::Claude,
                slot: Some(S.into()),
                pane: Some(pane.into()),
                brain: Some("default".into()),
                store_dir: Some(shown(&r.b.home.join(".claude-a"))),
                conversation_id: C.into(),
                kind: HookKind::SessionStarted {
                    source: StartSource::Startup,
                },
                at,
                at_ns: at.0 * 1_000_000,
                pid,
                seq: 1,
                cwd: None,
                transcript_path: None,
            },
        },
    )
}

/// The start of S's agent, sent between the sleep's commit and the kill of its tab, so
/// stamped after the commit and arriving after it: that agent is dying with its tab, so the
/// start is history only, and S stays asleep. A start from any other tab still brings S back.
#[test]
fn the_slept_agents_late_start_is_history_only() {
    for by_name in [true, false] {
        let mut r = World::new().up();
        let slept_at = if by_name { r.b.now().wall } else { r.due };
        let fx = if by_name {
            r.sleep_op(1, &[S], None, false)
        } else {
            r.input(slept_at, Input::Timer(TimerKey::Sleep))
        };
        assert_eq!(kills(&fx), [&kill("%1")], "{fx:?}");
        assert!(is_asleep(r.s()));

        let late = UtcMs(slept_at.0 + 1000);
        let fx = start_from(&mut r, "%1", late, UtcMs(late.0 + 1000), 101);
        let saved = changes(&fx);
        assert!(
            matches!(saved.as_slice(), [Change::Event(e)] if e.session_id == Some(s_id())),
            "{fx:?}"
        );
        assert!(is_asleep(r.s()), "{:?}", r.s());

        let later = UtcMs(late.0 + 2000);
        start_from(&mut r, "%8", later, UtcMs(later.0 + 1000), 808);
        assert_eq!(r.s().lifecycle, Lifecycle::Live, "{:?}", r.s());
        assert_eq!(r.s().asleep_at, None);
    }
}

/// A tab id names a tab only on its own server. Once the link is up on a new terminal
/// server, a start from a tab with the killed tab's id is a new agent there, and brings S
/// back; and so it does once the link to that new server drops.
#[test]
fn a_killed_tabs_id_on_a_new_server_counts_again() {
    for then_down in [false, true] {
        a_killed_tabs_id_on_a_new_server(then_down);
    }
}

fn a_killed_tabs_id_on_a_new_server(then_down: bool) {
    let mut r = World::new().up();
    let fx = r.sleep_op(1, &[S], None, false);
    assert_eq!(kills(&fx), [&kill("%1")], "{fx:?}");
    let slept_at = r.b.now().wall;
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: Generation {
                pid: 20,
                start: 2000,
            },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    if then_down {
        feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    }
    assert!(is_asleep(r.s()));

    let late = UtcMs(slept_at.0 + 1000);
    start_from(&mut r, "%1", late, UtcMs(late.0 + 1000), 101);
    assert_eq!(r.s().lifecycle, Lifecycle::Live, "{:?}", r.s());
    assert_eq!(r.s().asleep_at, None);
}

#[test]
fn sleep_idle_dry_run() {
    // S has been idle an hour.
    let mut r = World::new().up();
    let fx = r.sleep_op(1, &[], Some(HOUR), true);
    assert_eq!(reply_text(&fx), "would sleep d0000000  boards");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);

    // Two hours is more than it has been.
    let fx = r.sleep_op(2, &[], Some(2 * HOUR), true);
    assert_eq!(reply_text(&fx), "nothing to put to sleep");
    let fx = r.sleep_op(3, &[], Some(2 * HOUR), false);
    assert_eq!(reply_text(&fx), "nothing to put to sleep");
    assert!(changes(&fx).is_empty(), "{fx:?}");

    // Not a dry run: it goes.
    let fx = r.sleep_op(4, &[], Some(HOUR), false);
    assert_eq!(
        reply_text(&fx),
        format!("asleep: {TITLE} (resumable: moactl resume ID)")
    );
    assert_eq!(kills(&fx), [&kill("%1")]);
    assert!(is_asleep(r.s()));
}

/// A session whose tab is opening has no pane to kill yet.
#[test]
fn sleep_by_name_refuses_while_opening() {
    let mut r = World::asleep().up();
    let op = Op::SessionOpen {
        path: r.b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = the_open(&send(&r.b, &mut r.st, 1, op)).0;
    let fx = r.sleep_op(2, &[&id.to_string()], None, false);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(2),
            Err(OpError("it's still opening; try again in a moment".into()))
        )]
    );
}

/// R18 reads a tab's tag from the topology. The daemon sends a listing at once but holds the
/// tag until the commit is released, so a listing can be older than the tag: the tag moa
/// sent stays until a listing shows it, then the listings speak for it. A tag for a commit
/// that wasn't saved is neither sent nor assumed (R15).
#[test]
fn a_tag_moa_sent_outlives_an_older_listing() {
    let mut r = World::asleep().up();
    let op = Op::SessionOpen {
        path: r.b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = the_open(&send(&r.b, &mut r.st, 1, op)).0;
    let opened = |id| TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Open(id),
        results: vec![Ok(vec!["P %7 4242".into()])],
    };
    feed(&r.b, &mut r.st, opened(id));
    let later = UtcMs(r.b.now().wall.0 + 7 * HOUR);
    let why = |r: &Rig| why_not(&r.st, id, later, SIX);
    // A listing asked before the tag went out: the tab, untagged.
    let mut panes = base_panes();
    panes.truncate(1);
    panes.push("P %7 @3 $0 1 0 0 0 ".into());
    feed(&r.b, &mut r.st, lists(&panes));
    // Tagged and listed: only its silence keeps it awake.
    assert_eq!(why(&r), Some("never heard from"));
    // A listing that shows the tag confirms it; from then on the listings say.
    panes[1] = format!("P %7 @3 $0 1 0 0 0 {id}");
    feed(&r.b, &mut r.st, lists(&panes));
    assert_eq!(why(&r), Some("never heard from"));
    panes[1] = "P %7 @3 $0 1 0 0 0 ".into();
    feed(&r.b, &mut r.st, lists(&panes));
    assert_eq!(why(&r), Some("not tagged"));

    // A tag sent for a tab that is gone before any listing shows it isn't kept.
    let mut r = World::asleep().up();
    let op = Op::SessionOpen {
        path: r.b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = the_open(&send(&r.b, &mut r.st, 1, op)).0;
    feed(&r.b, &mut r.st, opened(id));
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(7)), Some(id));
    let mut panes = base_panes();
    panes.truncate(1);
    feed(&r.b, &mut r.st, lists(&panes));
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(7)), None);
    panes.push("P %7 @3 $0 1 0 0 0 ".into());
    feed(&r.b, &mut r.st, lists(&panes));
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(7)), None);

    // The binding's commit is refused: no tag goes out, and none is assumed.
    let mut r = World::asleep().up();
    let op = Op::SessionOpen {
        path: r.b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = the_open(&send(&r.b, &mut r.st, 1, op)).0;
    let mut snap = r.b.snapshot();
    snap.sessions.push(r.st.session(id).unwrap().clone());
    snap.conversations
        .extend(r.st.conversations_of(id).cloned());
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(later.0 + HOUR),
                reason: "disk full".into(),
            }),
        },
        r.b.now(),
    );
    let fx = feed(&r.b, &mut r.st, opened(id));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(
        !sent(&fx)
            .iter()
            .any(|c| c.argv().iter().any(|a| a == "@moa_session")),
        "{fx:?}"
    );
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(7)), None);
}

const MINUTE: i64 = 60_000;

/// `toml` (whose last table is the account `acct-a`) with moa running on `acct-a`, its
/// watchdog never firing within these tests: a session's ask to moa stays open while moa's
/// turn answers it ([`moa_answers`]). An ask moa can't answer would end at once (A9).
fn moa_runs(toml: &str) -> String {
    let toml = toml.replacen(
        "[brains.default]\n",
        "[brains.default]\nmoa_account = \"acct-a\"\n",
        1,
    );
    format!(
        "{toml}acp = \"{}\"\n\n[moa]\nwatchdog_seconds = 2592000\n",
        common::central::STUB
    )
}

/// moa's agent, started for the ask waiting for it, starts and opens its thread at `now`:
/// its turn answers the ask, which stays open. Nothing when moa isn't starting.
fn moa_answers(st: &mut State, now: Now) {
    use common::central::{caps, opened, started};
    use moa_core::central::Phase;
    use moa_core::central::wire::{AcpIn, Opened};
    if st.central().phase != Phase::Starting {
        return;
    }
    let generation = st.central().generation;
    let hear = |st: &mut State, input: AcpIn| handle_checked(st, Input::Acp(input), now.clone());
    hear(st, started(generation, caps(true, true, true)));
    hear(st, opened(generation, "thread-1", Opened::New));
    assert!(st.central().turn.is_some(), "moa answers the ask");
}

// --- the property ---

/// One session of the property's world, over R18's conditions.
#[derive(Clone, Debug)]
struct Sess {
    activity: Activity,
    question: bool,
    done: bool,
    tracked: bool,
    heard: bool,
    cockpit: bool,
    /// 0: none; 1: an open ask to it; 2: an open ask from it; 3: an answered ask to it.
    ask: u8,
    idle_hours: i64,
    archived: bool,
}

fn sess() -> impl Strategy<Value = Sess> {
    (
        prop_oneof![
            6 => Just(Activity::Idle),
            1 => Just(Activity::Working),
            1 => Just(Activity::NeedsAnswer),
            1 => Just(Activity::Failed),
        ],
        prop::bool::weighted(0.2),
        prop::bool::weighted(0.25),
        prop::bool::weighted(0.85),
        prop::bool::weighted(0.85),
        prop::bool::weighted(0.2),
        prop_oneof![6 => Just(0u8), 1 => Just(1u8), 1 => Just(2u8), 1 => Just(3u8)],
        0i64..12,
        prop::bool::weighted(0.05),
    )
        .prop_map(
            |(activity, question, done, tracked, heard, cockpit, ask, idle_hours, archived)| Sess {
                activity,
                question,
                done,
                tracked,
                heard,
                cockpit,
                ask,
                idle_hours,
                archived,
            },
        )
}

/// At most one session waits on moa: moa answers one ask at a time, and an ask waiting
/// behind it is dropped at its wait, which the model doesn't follow. Any other has an ask to
/// it instead, which keeps it awake as well.
fn one_waits_on_moa(mut sess: Vec<Sess>) -> Vec<Sess> {
    let mut seen = false;
    for p in sess.iter_mut().filter(|p| p.ask == 2) {
        if std::mem::replace(&mut seen, true) {
            p.ask = 1;
        }
    }
    sess
}

fn nth_id(k: usize) -> SessionId {
    format!("00000000-0000-4000-8000-{:012}", k + 1)
        .parse()
        .unwrap()
}

fn nth_conv(pid: i64) -> String {
    format!("e0000000-0000-4000-8000-{pid:012}")
}

/// Every session's agent runs in its tab: pid `200 + k` runs conversation `nth_conv`.
fn agents(pid: i64, _: &[String]) -> Inspected {
    Inspected::from((200..300).contains(&pid).then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: Some(nth_conv(pid)),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    }))
}

/// The property's world: no store, the default threshold, and tabs where the test says.
struct Model {
    sess: Vec<Sess>,
    /// The window the user is looking at.
    current: u32,
    reconciled: bool,
    listed: bool,
}

impl Model {
    /// Session `k`'s tab's window: the cockpit `@1`, else one of its own.
    fn window(&self, k: usize) -> u32 {
        if self.sess[k].cockpit {
            1
        } else {
            k as u32 + 2
        }
    }

    fn pane_lines(&self) -> Vec<String> {
        let wa = |w: u32| u8::from(w == self.current);
        let mut lines = vec![format!("P %0 @1 $0 1 {} 0 1 ", wa(1))];
        for k in 0..self.sess.len() {
            let w = self.window(k);
            lines.push(format!(
                "P %{} @{w} $0 {} {} 0 {} {}",
                k + 1,
                u8::from(w != 1),
                wa(w),
                u8::from(w == 1),
                nth_id(k)
            ));
        }
        lines
    }

    /// R18, written out: whether session `k` may sleep at `now`, by `rows`.
    fn eligible(&self, rows: &Rows, k: usize, now: UtcMs) -> bool {
        let (s, b) = &rows[k];
        let Some(b) = b else {
            return false;
        };
        // Asks don't change here: an open one is the one the session was made with.
        let open_ask = matches!(self.sess[k].ask, 1 | 2);
        let since = s.activity_at.unwrap_or(s.created_at);
        s.lifecycle == Lifecycle::Live
            && s.location == Location::Here
            && s.archived_at.is_none()
            && display_state(s, now).0 == DisplayState::Idle
            && b.tracked
            && b.server_pid == generation().pid
            && b.server_start == generation().start
            && b.pane_id == format!("%{}", k + 1)
            && self.listed
            && s.last_seen_at.is_some_and(|t| t > b.bound_at)
            && self.window(k) != self.current
            && !open_ask
            && now.0 - since.0 >= SIX
            && self.reconciled
    }
}

/// Each session's row and binding, as an input found or left them.
type Rows = Vec<(Session, Option<PaneBinding>)>;

fn rows(st: &State, n: usize) -> Rows {
    (0..n)
        .map(|k| {
            let id = nth_id(k);
            (st.session(id).unwrap().clone(), st.binding(id).cloned())
        })
        .collect()
}

const BASE: i64 = 1_789_000_000_000;

/// The property's `moa.toml` when moa answers an ask ([`moa_runs`] adds moa to it).
const PROPERTY_MOA: &str = "[brains.default]\n\n[accounts.acct-a]\nkind = \"claude\"\n\
                            store = \"~/.claude-a\"\nlauncher = \"/opt/claude\"\n";

fn property_state(sess: &[Sess]) -> State {
    let mut snap = Snapshot::default();
    for (k, p) in sess.iter().enumerate() {
        let id = nth_id(k);
        let pid = 200 + k as i64;
        let idle = UtcMs(BASE - p.idle_hours * HOUR);
        let bound = UtcMs(idle.0 - 2 * HOUR);
        snap.sessions.push(Session {
            id,
            agent_kind: AgentKind::Claude,
            account: None,
            cwd: Some("/w".into()),
            start_cwd: Some("/w".into()),
            project_id: None,
            title: Some(format!("s{k}")),
            title_locked: false,
            group_path: None,
            lifecycle: Lifecycle::Live,
            activity: p.activity,
            activity_at: Some(idle),
            activity_why: Some(if p.question { "Shall I?" } else { "Done." }.into()),
            last_prompt: None,
            last_turn_end_at: Some(idle),
            seen_at: Some(if p.done { UtcMs(idle.0 - 1) } else { idle }),
            asleep_at: None,
            archived_at: p.archived.then_some(UtcMs(1)),
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(bound.0 - HOUR),
            last_seen_at: Some(if p.heard { UtcMs(bound.0 + 1) } else { bound }),
        });
        snap.conversations.push(Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: nth_conv(pid),
            session_id: id,
            store_dir: None,
            spike_account: None,
            reason: ConversationReason::Start,
            first_seen_at: UtcMs(1),
            last_seen_at: UtcMs(1),
            transcript_path: None,
            transcript_present: None,
            transcript_checked_at: None,
        });
        snap.pane_bindings.push(PaneBinding {
            session_id: id,
            server_pid: generation().pid,
            server_start: generation().start,
            socket: "moa".into(),
            pane_id: format!("%{}", k + 1),
            pane_pid: pid,
            bound_at: bound,
            tracked: p.tracked,
        });
        if p.ask > 0 {
            let me = Some(AskTarget::Session(id));
            snap.asks.push(Ask {
                id: AskId(k as i64 + 1),
                at: if p.ask == 2 {
                    UtcMs(BASE - MINUTE)
                } else {
                    UtcMs(1)
                },
                from_session: if p.ask == 2 { me } else { Some(AskTarget::Moa) },
                from_name: None,
                to_session: if p.ask == 2 { Some(AskTarget::Moa) } else { me },
                text: "?".into(),
                status: match p.ask {
                    2 => AskStatus::Pending,
                    3 => AskStatus::Answered,
                    _ => AskStatus::Sent,
                },
                reply: None,
                claimed_at: None,
                answered_at: None,
            });
        }
    }
    let deps = Deps {
        inspect: agents,
        ..Deps::default()
    };
    // A session waiting on moa needs moa to answer it.
    let config = match sess.iter().any(|p| p.ask == 2) {
        true => Config::parse(&moa_runs(PROPERTY_MOA), &PathBuf::from("/tmp/moa-sleep")).unwrap(),
        false => Config::default(),
    };
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(snap)), at(UtcMs(BASE)));
    moa_answers(&mut st, at(UtcMs(BASE)));
    st
}

/// Runs `input` at `now`, then checks it: every session it put to sleep was eligible as it
/// ran (by the rows before it and the screen after it), and none it left is.
fn step(m: &Model, st: &mut State, input: Input, now: UtcMs) -> Result<(), TestCaseError> {
    let n = m.sess.len();
    let before = rows(st, n);
    let fx = handle_checked(st, input, at(now));
    for id in slept(&fx) {
        let k = (0..n).find(|k| nth_id(*k) == id).unwrap();
        prop_assert!(
            m.eligible(&before, k, now),
            "s{k} slept but wasn't eligible: {:?}",
            m.sess[k]
        );
        // Never on screen, adopted, unheard from, or done. In the cockpit, off screen, it
        // may (5b's L11).
        let p = &m.sess[k];
        prop_assert!(p.tracked && p.heard);
        prop_assert!(m.window(k) != m.current, "s{k} was on screen");
        let (s, _) = &before[k];
        prop_assert!(!matches!(s.last_turn_end_at.zip(s.seen_at), Some((end, seen)) if end > seen));
    }
    let after = rows(st, n);
    for k in 0..n {
        prop_assert!(
            !m.eligible(&after, k, now),
            "s{k} is eligible and still awake: {:?}",
            m.sess[k]
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever the sessions and whatever happens, nothing R18 rules out ever sleeps, and
    /// whatever it allows does once due.
    #[test]
    fn nothing_ineligible_ever_sleeps(
        sess in prop::collection::vec(sess(), 1..5).prop_map(one_waits_on_moa),
        inputs in prop::collection::vec((any::<bool>(), 0u32..7, 0i64..240), 0..12),
    ) {
        let mut st = property_state(&sess);
        let mut m = Model { sess, current: 1, reconciled: false, listed: false };
        let mut now = UtcMs(BASE);
        step(&m, &mut st, Input::Tmux(TmuxIn::Connected {
            conn: 1,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        }), now)?;
        step(&m, &mut st, Input::StartDrained, now)?;
        let n = st.tmux().queries().next().unwrap();
        let info: Vec<String> = (0..m.sess.len())
            .map(|k| {
                let adopted = if m.sess[k].tracked { "" } else { "1" };
                format!("I\t%{}\t{}\t0\t\t{adopted}\t{}\t/w", k + 1, 200 + k, nth_id(k))
            })
            .collect();
        m.reconciled = true;
        step(&m, &mut st, Input::Tmux(TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(info), Ok(Vec::new())],
        }), now)?;
        m.listed = true;
        step(&m, &mut st, Input::Tmux(lists(&m.pane_lines())), now)?;
        for (timer, window, minutes) in inputs {
            now = UtcMs(now.0 + minutes * 60_000);
            if timer {
                step(&m, &mut st, Input::Timer(TimerKey::Sleep), now)?;
            } else {
                // `@0` doesn't exist: the user is on a window with no session's tab.
                m.current = window;
                step(&m, &mut st, Input::Tmux(window_changed(window)), now)?;
            }
        }
    }
}
