//! Sleep (R18–R20): an idle session goes to sleep on its own at its deadline (its tab is
//! closed, the session stays resumable), and `sleep` puts sessions to sleep by name or by
//! how long they've been idle. On the typical brain with its link played by hand.

mod common;

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Effect, Input, Publish, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::live::{DisplayState, display_state, is_asleep};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Ask, AskId, AskStatus, AskTarget, Conversation,
    ConversationReason, Lifecycle, Location, PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{Op, OpError, ReqId};
use moa_core::publish::CoreEvent;
use moa_core::sleep::why_not;
use moa_core::state::{Deps, LoadNote, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::notify::Notification;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
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

fn tz() -> TimeZone {
    TimeZone::posix(TYPICAL_TZ).unwrap()
}

fn at(wall: UtcMs) -> Now {
    Now::at(wall, Duration::ZERO, tz())
}

thread_local! {
    /// Whether S's adopted agent still runs in its shell tab.
    static AGENT_RUNS: Cell<bool> = const { Cell::new(true) };
}

/// S's agent in its tab (pid 101), started by hand: no `MOA_SESSION`, while it runs.
fn s_by_hand(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 101 && AGENT_RUNS.with(Cell::get)).then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: Some(C.into()),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    })
}

/// The cockpit `@1` (pane `%0`), which the user is looking at, and S's tab `%1` in `@2`
/// behind it.
fn base_panes() -> Vec<String> {
    vec![
        "P %0 @1 $0 1 1 0 1 ".into(),
        format!("P %1 @2 $0 1 0 0 0 {S}"),
    ]
}

/// The user's terminal on session `$0`, and moa's own control client.
const CLIENTS: &[&str] = &["C 0 $0 /dev/ttys1", "C 1 $1 client-9"];

fn lists(panes: &[String]) -> TmuxIn {
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: vec![
            lines(&["S $0 moa", "S $1 _moa"]),
            Ok(panes.to_vec()),
            lines(CLIENTS),
        ],
    }
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
        }
    }
}

struct Rig {
    b: TypicalBrain,
    st: State,
    /// Every effect of loading, linking and reconciling.
    setup: Vec<Effect>,
    due: UtcMs,
}

impl Rig {
    fn s(&self) -> &Session {
        self.st.session(s_id()).unwrap()
    }

    /// Why S can't sleep at `wall` under the default threshold.
    fn why(&self, wall: UtcMs) -> Option<&'static str> {
        why_not(&self.st, s_id(), wall, SIX)
    }

    fn input(&mut self, wall: UtcMs, input: Input) -> Vec<Effect> {
        handle_checked(&mut self.st, input, at(wall))
    }

    fn sleep_op(
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
}

fn kill(pane: &str) -> Cmd {
    Cmd::new("kill-pane").args(["-t", pane])
}

/// Every tmux command sent, untokened or not.
fn sent(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn kills(fx: &[Effect]) -> Vec<&Cmd> {
    sent(fx)
        .into_iter()
        .filter(|c| c.name() == "kill-pane")
        .collect()
}

fn sleep_timers(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SetTimer {
                    key: TimerKey::Sleep,
                    ..
                } | Effect::CancelTimer(TimerKey::Sleep)
            )
        })
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
        let me = Some(AskTarget::Session(s_id()));
        w.asks.push(Ask {
            id: AskId(900),
            at: UtcMs(1),
            from_session: if from { me } else { Some(AskTarget::Moa) },
            from_name: None,
            to_session: if to { me } else { Some(AskTarget::Moa) },
            text: "which boards?".into(),
            status: AskStatus::Received,
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
    w.panes = vec![
        "P %0 @1 $0 1 0 0 1 ".into(),
        format!("P %1 @1 $0 0 0 0 1 {S}"),
        "P %5 @3 $0 1 1 0 0 ".into(),
    ];
    cases.push(("in the cockpit", w, "in the cockpit"));
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
/// bound session's launch wait.
fn waiting_launch() -> Rig {
    let mut w = World::new();
    let (bound, info) = (w.snapshot(), w.info().unwrap());
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
    r
}

// --- on its own ---

#[test]
fn an_idle_session_sleeps_at_its_deadline() {
    let mut r = World::new().up();
    let due = r.due;
    assert_eq!(
        sleep_timers(&r.setup).last(),
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

    let killed = fx
        .iter()
        .position(
            |e| matches!(e, Effect::Tmux { conn: 1, cmds, token: None } if cmds == &[kill("%1")]),
        )
        .expect("the tab is killed");
    let cancelled = fx
        .iter()
        .position(|e| matches!(e, Effect::CancelTimer(TimerKey::Sleep)))
        .expect("no deadline is left");
    assert!(0 < killed && killed < cancelled, "{fx:?}");
    assert_eq!(kills(&fx).len(), 1);
    assert!(is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());

    // Nothing more happens later; a fired timer is never taken for an armed one.
    let fx = r.input(UtcMs(due.0 + HOUR), Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(sleep_timers(&fx), [&Effect::CancelTimer(TimerKey::Sleep)]);
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
        sleep_timers(&fx),
        [&Effect::SetTimer {
            key: TimerKey::Sleep,
            at: due
        }]
    );
    // Any other input leaves an unchanged deadline alone.
    let fx = r.input(UtcMs(due.0 - 30_000), Input::Timer(TimerKey::Clock));
    assert!(sleep_timers(&fx).is_empty(), "{fx:?}");
}

/// R19: due while on screen, it doesn't sleep, and no timer is armed for it. The input that
/// takes it off the screen puts it to sleep.
#[test]
fn an_overdue_session_on_screen_sleeps_when_it_leaves_the_screen() {
    let mut w = World::new();
    w.panes = vec![
        "P %0 @1 $0 1 0 0 1 ".into(),
        format!("P %1 @2 $0 1 1 0 0 {S}"),
    ];
    let mut r = w.up();
    let due = r.due;
    let fx = r.input(due, Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        sleep_timers(&fx),
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
    assert!(sleep_timers(&fx).is_empty(), "{fx:?}");
}

#[test]
fn zero_means_never() {
    let mut w = World::new();
    w.toml = TOML.replace(
        "open_account = \"acct-a\"\n",
        "open_account = \"acct-a\"\nsleep_after_hours = 0\n",
    );
    let mut r = w.up();
    assert!(sleep_timers(&r.setup).is_empty(), "{:?}", r.setup);
    let fx = r.input(UtcMs(r.due.0 + 100 * HOUR), Input::Timer(TimerKey::Sleep));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(sleep_timers(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
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
/// out of it is looked at again (the Task 4 minor).
#[test]
fn a_renamed_window_moves_the_cockpit() {
    let mut r = World::new().up();
    let due = r.due;
    let refreshes = |fx: &[Effect]| {
        fx.iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::Tmux {
                        token: Some(TmuxToken::Refresh),
                        ..
                    }
                )
            })
            .count()
    };
    // S's tab's window is renamed `moa` (the cockpit's own got another name first).
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Notification(Notification::Other("unlinked-window-renamed")),
    );
    assert_eq!(refreshes(&fx), 1, "{fx:?}");
    feed(
        &r.b,
        &mut r.st,
        lists(&[
            "P %0 @1 $0 1 1 0 0 ".into(),
            format!("P %1 @2 $0 1 0 0 1 {S}"),
        ]),
    );
    assert_eq!(r.why(due), Some("in the cockpit"));
    // And back.
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Notification(Notification::Other("window-renamed")),
    );
    assert_eq!(refreshes(&fx), 1, "{fx:?}");
    feed(&r.b, &mut r.st, lists(&base_panes()));
    assert_eq!(r.why(due), None);
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
    let killed = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { token: None, cmds, .. } if cmds == &[kill("%1")]))
        .unwrap();
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

    // A name that matches nothing changes nothing.
    let mut r = World::new().up();
    let fx = r.sleep_op(1, &[S, "no-such-session"], None, false);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx)[0].1.is_err());
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

fn nth_id(k: usize) -> SessionId {
    format!("00000000-0000-4000-8000-{:012}", k + 1)
        .parse()
        .unwrap()
}

fn nth_conv(pid: i64) -> String {
    format!("e0000000-0000-4000-8000-{pid:012}")
}

/// Every session's agent runs in its tab: pid `200 + k` runs conversation `nth_conv`.
fn agents(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (200..300).contains(&pid).then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: Some(nth_conv(pid)),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    })
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
            && self.window(k) != 1
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
                at: UtcMs(1),
                from_session: if p.ask == 2 { me } else { Some(AskTarget::Moa) },
                from_name: None,
                to_session: if p.ask == 2 { Some(AskTarget::Moa) } else { me },
                text: "?".into(),
                status: if p.ask == 3 {
                    AskStatus::Answered
                } else {
                    AskStatus::Sent
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
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(snap)), at(UtcMs(BASE)));
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
        // Never on screen, adopted, unheard from, in the cockpit, or done.
        let p = &m.sess[k];
        prop_assert!(p.tracked && p.heard && !p.cockpit);
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
        sess in prop::collection::vec(sess(), 1..5),
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
