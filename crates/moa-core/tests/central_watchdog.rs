//! The watchdog (K6): a turn of moa's agent with no news for 5 minutes is cancelled, and one
//! still under way 30 s after that is cut short: moa's agent is restarted and today's thread
//! reopened. A `card` row in the turn's thread says what happened (K7, K11).

mod common;

use common::central::{
    acps, caps, feed_rows, hear_at, moa_at, opened, phase, prompt_at, prompts_sent,
    published_state, started, thread_rows,
};
use common::{replies, timers};
use moa_core::central::Phase;
use moa_core::central::wire::{
    AcpCmd, AcpIn, Caps, Opened, PermKind, PermOption, StopReason, ThreadHow, Update,
};
use moa_core::effect::{Effect, Input, TimerKey, handle_checked};
use moa_core::model::FeedEntry;
use moa_core::op::{ConnId, Op, OpError, PermAnswer, ReqId};
use moa_core::state::State;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;

const CANCELLED: &str = "moa's turn had no news for 5 min, so moa cancelled it.";
const KILLED: &str = " It still hadn't stopped 30 s later, so moa's agent was restarted";
const HUNG: &str = "moa's turn hung, so moa's agent was restarted";

fn at(ms: i64) -> Now {
    common::at(UtcMs(ms))
}

/// moa on `acct-a`, with `settings` (a `[moa]` table, or `""`); when it started.
fn moa_with(b: &TypicalBrain, settings: &str) -> (State, i64) {
    let t = b.now().wall.0;
    (moa_at(b, "acct-a", settings, b.snapshot(), b.now()).0, t)
}

/// moa's first turn, sent at `t` in thread `th-1` (request 1, `wait` as given), its agent
/// offering `offers`: what sending it said.
fn send(st: &mut State, t: i64, wait: bool, offers: Caps) -> Vec<Effect> {
    let generation = st.central().generation + 1;
    prompt_at(st, at(t), 1, "hello", wait);
    hear_at(st, at(t), started(generation, offers));
    let fx = hear_at(st, at(t), opened(generation, "th-1", Opened::New));
    assert_eq!(*phase(st), Phase::Thinking, "{fx:?}");
    fx
}

/// moa at the default settings, its first turn sent at its start: the state and that time.
fn turn(b: &TypicalBrain, wait: bool) -> (State, i64) {
    let (mut st, t) = moa_with(b, "");
    send(&mut st, t, wait, caps(true, true, true));
    (st, t)
}

/// moa's agent says `update` in `th-1` at `ms`.
fn say(st: &mut State, ms: i64, update: Update) -> Vec<Effect> {
    let input = AcpIn::Update {
        generation: st.central().generation,
        thread: "th-1".into(),
        update,
    };
    hear_at(st, at(ms), input)
}

fn words(text: &str) -> Update {
    Update::Message {
        id: None,
        text: text.into(),
    }
}

fn tool(id: &str, status: &str) -> Update {
    Update::Tool {
        id: id.into(),
        title: format!("Read {id}"),
        kind: "read".into(),
        status: status.into(),
        input: None,
    }
}

/// A permission request in `th-1` that waits for you.
fn ask(st: &mut State, ms: i64, req: u64) -> Vec<Effect> {
    let option = |id: &str, kind| PermOption {
        id: id.into(),
        kind,
        name: format!("Name of {id}"),
    };
    let input = AcpIn::Permission {
        generation: st.central().generation,
        req,
        thread: "th-1".into(),
        title: "Run git push".into(),
        options: vec![
            option("allow-1", PermKind::AllowOnce),
            option("not-now", PermKind::RejectOnce),
        ],
    };
    hear_at(st, at(ms), input)
}

/// The turn under way ends at `ms` as `stop` says.
fn end(st: &mut State, ms: i64, stop: StopReason) -> Vec<Effect> {
    let input = AcpIn::TurnEnded {
        generation: st.central().generation,
        prompt: st.central().turn.as_ref().expect("a turn").prompt,
        result: Ok(stop),
    };
    hear_at(st, at(ms), input)
}

/// The `Watchdog` timer fires at `ms`.
fn fire(st: &mut State, ms: i64) -> Vec<Effect> {
    handle_checked(st, Input::Timer(TimerKey::Watchdog), at(ms))
}

/// The `Watchdog` timer as `fx` leaves it: `Some(Some(at))` set, `Some(None)` cancelled,
/// `None` untouched.
fn dog(fx: &[Effect]) -> Option<Option<UtcMs>> {
    timers(fx, TimerKey::Watchdog).last().map(|e| match e {
        Effect::SetTimer { at, .. } => Some(*at),
        _ => None,
    })
}

/// The `card` rows the state holds, oldest first.
fn cards(st: &State) -> Vec<FeedEntry> {
    st.feed_tail()
        .filter(|r| r.kind == "card")
        .cloned()
        .collect()
}

/// The one card's body.
fn the_card(st: &State) -> Value {
    match &cards(st)[..] {
        [card] => serde_json::from_str(&card.body).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn card_text(st: &State) -> String {
    the_card(st)["text"].as_str().unwrap().to_string()
}

/// The turn of [`turn`] cancelled at `t + 5 min` and still under way at `t + 5.5 min`: the
/// restart's effects.
fn restarted(st: &mut State, t: i64) -> Vec<Effect> {
    fire(st, t + 5 * MIN);
    fire(st, t + 5 * MIN + 30 * SEC)
}

#[test]
fn no_news_for_5_min_cancels() {
    let b = TypicalBrain::new();
    let (mut st, t) = moa_with(&b, "");
    let fx = send(&mut st, t, false, caps(true, true, true));
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 300 * SEC))));
    // Each word of the turn is news.
    let fx = say(&mut st, t + 100 * SEC, words("Looking"));
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 400 * SEC))));
    // A timer that fires before its deadline is armed again, and does nothing.
    let fx = fire(&mut st, t + 300 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 400 * SEC))));

    let fx = fire(&mut st, t + 400 * SEC);
    let cancel = AcpCmd::Cancel {
        generation: 1,
        thread: "th-1".into(),
    };
    assert_eq!(acps(&fx), [&cancel]);
    let card = &cards(&st)[0];
    assert_eq!(card.thread_id, "th-1");
    assert_eq!(card.at, UtcMs(t + 400 * SEC));
    assert_eq!(
        the_card(&st),
        json!({"what": "watchdog", "text": CANCELLED})
    );
    assert_eq!(feed_rows(&fx).len(), 1);
    // The turn gets 30 s to stop; news doesn't move that.
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 430 * SEC))));
    let fx = say(&mut st, t + 410 * SEC, words(" still"));
    assert_eq!(dog(&fx), None);
    assert_eq!(*phase(&st), Phase::Thinking);
}

#[test]
fn a_turn_that_ends_after_the_cancel_is_left_alone() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    fire(&mut st, t + 5 * MIN);
    let fx = end(&mut st, t + 5 * MIN + 10 * SEC, StopReason::Cancelled);
    assert_eq!(dog(&fx), Some(None));
    let fx = fire(&mut st, t + 5 * MIN + 30 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Ready);
    assert_eq!(card_text(&st), CANCELLED);

    // The next turn is watched afresh.
    let fx = prompt_at(&mut st, at(t + 6 * MIN), 2, "again", false);
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 11 * MIN))));
}

#[test]
fn still_stuck_30_s_later_restarts() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, true);
    say(&mut st, t, tool("read-1", "in_progress"));
    let fx = restarted(&mut st, t);
    let generation = st.central().generation;
    assert_eq!(generation, 2);
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Start { generation: 2, .. }]),
        "{fx:?}"
    );
    // Whoever waits hears it at once; the running tool is cancelled, and the card says so.
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(HUNG.into())))]);
    let tool_row = st.feed_tail().find(|r| r.kind == "tool").unwrap();
    assert_eq!(tool_row.state.as_deref(), Some("cancelled"));
    assert_eq!(card_text(&st), format!("{CANCELLED}{KILLED}."));
    assert_eq!(*phase(&st), Phase::Starting);
    assert!(st.central().turn.is_none());
    assert_eq!(dog(&fx), Some(None));
    // The old run's late words are dropped.
    let late = AcpIn::TurnEnded {
        generation: 1,
        prompt: 1,
        result: Ok(StopReason::Cancelled),
    };
    let fx = hear_at(&mut st, at(t + 6 * MIN), late);
    assert!(feed_rows(&fx).is_empty(), "{fx:?}");

    // Started, it reopens today's thread, with no prompt.
    let fx = hear_at(&mut st, at(t + 6 * MIN), started(2, caps(true, true, true)));
    let cwd = st.deps().agent_dir.clone();
    let open = AcpCmd::Open {
        generation: 2,
        how: ThreadHow::Resume {
            thread: "th-1".into(),
        },
        cwd,
    };
    assert_eq!(acps(&fx), [&open]);
    assert_eq!(*phase(&st), Phase::Starting);
    let fx = hear_at(&mut st, at(t + 6 * MIN), opened(2, "th-1", Opened::Resumed));
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Ready);
    let open = st
        .central()
        .thread
        .as_ref()
        .expect("today's thread is open");
    assert_eq!(open.row.thread_id, "th-1");
    assert_eq!(published_state(&fx).unwrap()["phase"], "ready");
    assert_eq!(
        card_text(&st),
        format!("{CANCELLED}{KILLED} and today's thread reopened.")
    );
    assert_eq!(cards(&st)[0].thread_id, "th-1");

    // The next prompt goes to it at once.
    let fx = prompt_at(&mut st, at(t + 7 * MIN), 2, "go on", false);
    assert_eq!(prompts_sent(&fx), ["go on"]);
}

#[test]
fn a_restart_without_resume_loads() {
    let b = TypicalBrain::new();
    let (mut st, t) = moa_with(&b, "");
    send(&mut st, t, false, caps(true, false, true));
    restarted(&mut st, t);
    let fx = hear_at(
        &mut st,
        at(t + 6 * MIN),
        started(2, caps(true, false, true)),
    );
    assert!(
        matches!(
            &acps(&fx)[..],
            [AcpCmd::Open { how: ThreadHow::Load { thread }, .. }] if thread == "th-1"
        ),
        "{fx:?}"
    );
    // The replay brings back what the feed has already: nothing is written twice.
    let replay = AcpIn::Update {
        generation: 2,
        thread: "th-1".into(),
        update: Update::UserEcho,
    };
    hear_at(&mut st, at(t + 6 * MIN), replay);
    hear_at(&mut st, at(t + 6 * MIN), opened(2, "th-1", Opened::Loaded));
    assert_eq!(*phase(&st), Phase::Ready);
    assert_eq!(st.central().thread.as_ref().unwrap().row.thread_id, "th-1");
    assert!(st.central().replaying.is_none());
    assert_eq!(
        card_text(&st),
        format!("{CANCELLED}{KILLED} and today's thread reopened.")
    );
}

#[test]
fn a_restart_that_cant_reopen_says_so() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    restarted(&mut st, t);
    hear_at(&mut st, at(t + 6 * MIN), started(2, caps(true, true, true)));
    let gone = AcpIn::Opened {
        generation: 2,
        result: Err("no such thread".into()),
    };
    let fx = hear_at(&mut st, at(t + 6 * MIN), gone);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Ready);
    assert!(st.central().thread.is_none());
    assert_eq!(
        card_text(&st),
        format!(
            "{CANCELLED}{KILLED}. Today's thread couldn't be reopened; your next prompt starts \
             a new one."
        )
    );

    // The next prompt opens a new thread, whose first line says why.
    let fx = prompt_at(&mut st, at(t + 7 * MIN), 2, "go on", false);
    assert!(
        matches!(
            &acps(&fx)[..],
            [AcpCmd::Open {
                how: ThreadHow::New { .. },
                ..
            }]
        ),
        "{fx:?}"
    );
    let fx = hear_at(&mut st, at(t + 7 * MIN), opened(2, "th-2", Opened::New));
    assert_eq!(prompts_sent(&fx), ["go on"]);
    assert_eq!(thread_rows(&fx).len(), 1);
    let first = feed_rows(&fx)[0];
    assert_eq!(first.kind, "system");
    assert!(first.body.contains("couldn't be reopened"), "{first:?}");
}

#[test]
fn a_failed_restart_says_so_too() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    restarted(&mut st, t);
    let exit = AcpIn::Exited {
        generation: 2,
        why: "it crashed".into(),
    };
    hear_at(&mut st, at(t + 6 * MIN), exit);
    assert!(matches!(phase(&st), Phase::Failed(_)));
    assert!(st.central().restarting.is_none());
    assert!(
        card_text(&st)
            .ends_with("Today's thread couldn't be reopened; your next prompt starts a new one.")
    );
}

#[test]
fn a_restart_clears_the_cards() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    fire(&mut st, t + 5 * MIN);
    // Asked after the cancel, it doesn't hold the restart off: the turn should have stopped.
    let fx = ask(&mut st, t + 5 * MIN + 10 * SEC, 3);
    assert_eq!(dog(&fx), None);
    assert_eq!(st.central().asking.len(), 1);
    let fx = fire(&mut st, t + 5 * MIN + 30 * SEC);
    // The old agent is gone: nobody hears the request cancelled.
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Start { generation: 2, .. }]),
        "{fx:?}"
    );
    let asked = st.feed_tail().find(|r| r.kind == "permission").unwrap();
    assert_eq!(asked.state.as_deref(), Some("cancelled"));
    assert!(st.central().asking.is_empty());
    assert!(st.central().answered.is_empty());
    assert_eq!(published_state(&fx).unwrap()["asking"], json!([]));
}

#[test]
fn the_watchdog_waits_while_you_decide() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    let fx = ask(&mut st, t + MIN, 3);
    assert_eq!(dog(&fx), Some(None));
    let fx = fire(&mut st, t + 5 * MIN);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(dog(&fx), Some(None));

    // Answered 20 minutes later, the turn gets 5 minutes from the answer.
    let answer = Op::ConsolePermission {
        id: None,
        answer: PermAnswer::Once,
    };
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(5),
        caller: Default::default(),
        op: answer,
    };
    let answered = t + 21 * MIN;
    let fx = handle_checked(&mut st, input, at(answered));
    assert_eq!(dog(&fx), Some(Some(UtcMs(answered + 5 * MIN))));
    let fx = fire(&mut st, answered + 5 * MIN);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Cancel { .. }]), "{fx:?}");
}

#[test]
fn a_cancel_of_yours_gives_the_turn_a_fresh_5_min() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    ask(&mut st, t + MIN, 3);
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(5),
        caller: Default::default(),
        op: Op::ConsoleCancel(moa_core::op::NoParams),
    };
    let fx = handle_checked(&mut st, input, at(t + 30 * MIN));
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 35 * MIN))));
}

#[test]
fn a_late_timer_rearms() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    // The Mac slept: the timer fires 2 h late.
    let late = t + 5 * MIN + 2 * 60 * MIN;
    let fx = fire(&mut st, late);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert!(feed_rows(&fx).is_empty(), "{fx:?}");
    assert_eq!(dog(&fx), Some(Some(UtcMs(late + 5 * MIN))));
    // Up to a minute late is on time.
    let fx = fire(&mut st, late + 6 * MIN);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Cancel { .. }]), "{fx:?}");
    // The kill's timer re-arms the same way.
    let fx = fire(&mut st, late + 6 * MIN + 2 * 60 * MIN);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        dog(&fx),
        Some(Some(UtcMs(late + 6 * MIN + 2 * 60 * MIN + 30 * SEC)))
    );
}

#[test]
fn the_limits_are_settings() {
    let b = TypicalBrain::new();
    let settings = "[moa]\nwatchdog_seconds = 2\nwatchdog_kill_seconds = 120\n";
    let (mut st, t) = moa_with(&b, settings);
    let fx = send(&mut st, t, false, caps(true, true, true));
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 2 * SEC))));
    let fx = fire(&mut st, t + 2 * SEC);
    assert_eq!(dog(&fx), Some(Some(UtcMs(t + 2 * SEC + 2 * MIN))));
    assert_eq!(
        card_text(&st),
        "moa's turn had no news for 2 s, so moa cancelled it."
    );
    fire(&mut st, t + 2 * SEC + 2 * MIN);
    assert!(
        card_text(&st).contains("It still hadn't stopped 2 min later"),
        "{}",
        card_text(&st)
    );
}

#[test]
fn no_watchdog_when_idle() {
    let b = TypicalBrain::new();
    let (mut st, t) = moa_with(&b, "");
    let fx = prompt_at(&mut st, at(t), 1, "hello", false);
    assert_eq!(dog(&fx), None);
    let fx = hear_at(&mut st, at(t), started(1, caps(true, true, true)));
    assert_eq!(dog(&fx), None);
    hear_at(&mut st, at(t), opened(1, "th-1", Opened::New));
    let fx = end(&mut st, t + MIN, StopReason::EndTurn);
    assert_eq!(dog(&fx), Some(None));
    // Nothing more, whatever comes.
    let fx = fire(&mut st, t + 10 * MIN);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(dog(&fx), Some(None));
    assert!(cards(&st).is_empty());
    let fx = say(&mut st, t + 11 * MIN, words("late"));
    assert_eq!(dog(&fx), None);
}

#[test]
fn an_exit_mid_turn_stops_the_watch() {
    let b = TypicalBrain::new();
    let (mut st, t) = turn(&b, false);
    let exit = AcpIn::Exited {
        generation: 1,
        why: "it crashed".into(),
    };
    let fx = hear_at(&mut st, at(t + MIN), exit);
    assert_eq!(dog(&fx), Some(None));
    let fx = fire(&mut st, t + 10 * MIN);
    assert!(acps(&fx).is_empty(), "{fx:?}");
}
