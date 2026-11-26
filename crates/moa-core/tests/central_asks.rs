//! Asks to moa (A9) and moa's own asks under the watchdog (A10): an ask to moa goes through
//! its channel as a hidden prompt of its own, first among the automatic ones, and its turn's
//! end is the answer; while moa waits on an ask of its own, its watchdog holds.

mod common;

use common::asks::*;
use common::central::{
    body_of, caps, hear_at, play_at, prompts_sent, sent, started, toml, you_commit, you_row,
};
use common::timers;
use moa_core::UtcMs;
use moa_core::central::opening::BRIEFED_KEY;
use moa_core::central::wire::{AcpCmd, AcpIn, StopReason, Update};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TimerKey, handle_checked};
use moa_core::model::{Ask, AskStatus, AskTarget, Report, ReportKind};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Snapshot};
use moa_core::tmux::cmd::Cmd;
use serde_json::json;

/// What moa's agent is told of Kite's ask #1 (A13).
const KITES_ASK: &str = "(moa · Kite asks you, ask #1, not typed by the user)\n\
                         Is the domain renewed?\n\n\
                         Your reply this turn goes back to Kite as the answer.";

/// Kite's request, waiting for moa's answer.
const KITE_CONN: u64 = 2;

/// Kite, Harbor and Beacon live, moa on `acct-a` with today opened, the link up.
fn rig_from(mut snap: Snapshot) -> Rig {
    snap.meta
        .push((BRIEFED_KEY.into(), now(T0).local_day.to_string()));
    rig_unopened(snap)
}

/// [`rig_from`], today not opened yet.
fn rig_unopened(snap: Snapshot) -> Rig {
    let (mut r, _) = Rig::started_with(snap, &toml("acct-a"), T0);
    r.link_up(T0);
    r
}

fn rig() -> Rig {
    rig_from(snapshot())
}

fn you() -> CallerInfo {
    CallerInfo::default()
}

/// Your prompt `text` at `ms`, on connection 9, and moa's agent starting and opening a
/// thread for it.
fn your_prompt(r: &mut Rig, ms: i64, text: &str) -> Vec<Effect> {
    let op = Op::ConsolePrompt {
        text: text.into(),
        wait: false,
    };
    let fx = r.op(9, you(), op, ms);
    play_at(&mut r.st, now(ms), fx)
}

fn hear(r: &mut Rig, ms: i64, input: AcpIn) -> Vec<Effect> {
    hear_at(&mut r.st, now(ms), input)
}

/// moa says `text` in its turn under way, at `ms`.
fn says(r: &mut Rig, ms: i64, text: &str) -> Vec<Effect> {
    let turn = r.st.central().turn.clone().expect("a turn");
    let input = AcpIn::Update {
        generation: r.st.central().generation,
        thread: turn.thread,
        update: Update::Message {
            id: None,
            text: text.into(),
        },
    };
    hear(r, ms, input)
}

/// moa's turn under way ends at `ms`, as `result` says.
fn turn_ends(r: &mut Rig, ms: i64, result: Result<StopReason, String>) -> Vec<Effect> {
    let prompt = r.st.central().turn.as_ref().expect("a turn").prompt;
    let input = AcpIn::TurnEnded {
        generation: r.st.central().generation,
        prompt,
        result,
    };
    let fx = hear(r, ms, input);
    play_at(&mut r.st, now(ms), fx)
}

/// Kite asks moa "Is the domain renewed?" at T0 on [`KITE_CONN`].
fn kite_asks_moa(r: &mut Rig) -> Vec<Effect> {
    r.ask(KITE_CONN, from(KITE), "moa", "Is the domain renewed?", 540)
}

/// A rig whose moa answers Kite's ask #1: your prompt's turn was under way when Kite asked,
/// and ended at T0 + 10 s, which sent the ask. The effects of that end.
fn answering() -> (Rig, Vec<Effect>) {
    let mut r = rig();
    your_prompt(&mut r, T0, "Tidy the Harbor folder");
    kite_asks_moa(&mut r);
    let fx = turn_ends(&mut r, T0 + 10 * SEC, Ok(StopReason::EndTurn));
    assert_eq!(sent(&fx), KITES_ASK, "{fx:?}");
    (r, fx)
}

/// The one reply to Kite's request.
fn kite_hears(fx: &[Effect]) -> Result<Done, OpError> {
    let theirs: Vec<_> = replies(fx)
        .into_iter()
        .filter(|(conn, _, _)| *conn == ConnId(KITE_CONN))
        .collect();
    match &theirs[..] {
        [(_, _, result)] => result.clone(),
        other => panic!("not one reply to Kite: {other:?}"),
    }
}

fn harbors_report(at: i64) -> Report {
    Report {
        id: 1,
        session_id: sid(HARBOR),
        kind: ReportKind::TurnEnded,
        queued_at: UtcMs(at),
        last_at: UtcMs(at),
        turns: vec!["Harbor tidied the folder.".into()],
    }
}

// ---------- an ask to moa ----------

/// A9 (a–c): an ask to moa waits while moa is busy, then goes as a hidden prompt of its own,
/// carrying only the ask, ahead of the reports queued.
#[test]
fn an_ask_to_moa_waits_for_its_turn() {
    let mut r = rig();
    let fx = your_prompt(&mut r, T0, "Tidy the Harbor folder");
    assert_eq!(prompts_sent(&fx).len(), 1, "{fx:?}");
    // A report is queued meanwhile.
    r.stopped(T0 + 5 * SEC, HARBOR, "Harbor tidied the folder.");
    assert_eq!(r.st.reports().count(), 1);
    let fx = kite_asks_moa(&mut r);
    assert!(
        replies(&fx).is_empty() && prompts_sent(&fx).is_empty(),
        "{fx:?}"
    );
    assert_eq!(r.row(1).status, AskStatus::Pending);

    let fx = turn_ends(&mut r, T0 + 10 * SEC, Ok(StopReason::EndTurn));
    // The ask alone: no reports block, though one is queued.
    assert_eq!(sent(&fx), KITES_ASK);
    assert_eq!(r.st.reports().count(), 1);
    let you = you_row(&fx);
    assert!(you.automatic);
    assert_eq!(
        body_of(you),
        json!({ "text": KITES_ASK, "ask": { "id": 1, "from": "Kite" } })
    );
    // One commit: the card, the row, and the ask received.
    let commit = you_commit(&fx);
    let card = commit
        .iter()
        .find_map(|c| match c {
            Change::Feed(row) if row.kind == "card" => Some(row),
            _ => None,
        })
        .expect("the ask's card");
    assert_eq!(
        body_of(card),
        json!({ "what": "ask", "text": "Kite · #1: Is the domain renewed?" })
    );
    assert_eq!(card.thread_id, you.thread_id);
    assert!(card.id < you.id, "the card comes before its turn");
    let received = commit.iter().find_map(|c| match c {
        Change::Ask(a) => Some(a),
        _ => None,
    });
    assert_eq!(
        received,
        Some(&Ask {
            status: AskStatus::Received,
            claimed_at: Some(UtcMs(T0 + 10 * SEC)),
            ..ask_row(
                1,
                to(KITE),
                "Kite",
                AskTarget::Moa,
                "Is the domain renewed?",
                T0
            )
        })
    );
    assert_eq!(r.row(1).status, AskStatus::Received);
    // Nothing is typed into a pane, and Kite still waits.
    assert!(typed(&fx).is_empty() && replies(&fx).is_empty(), "{fx:?}");
}

/// A9 (d): the turn's words, joined, are the answer, and Kite's request hears them.
#[test]
fn moas_answer_goes_back() {
    let (mut r, _) = answering();
    says(&mut r, T0 + 12 * SEC, "Renewed in May.");
    let fx = turn_ends(&mut r, T0 + 15 * SEC, Ok(StopReason::EndTurn));
    let row = r.row(1);
    assert_eq!(row.status, AskStatus::Answered);
    assert_eq!(row.reply.as_deref(), Some("Renewed in May."));
    assert_eq!(row.answered_at, Some(UtcMs(T0 + 15 * SEC)));
    let done = kite_hears(&fx).expect("the answer");
    assert_eq!(done.text, "Renewed in May.");
    assert_eq!(
        done.data,
        json!({ "id": 1, "from": "Kite", "to": "moa", "status": "answered" })
    );
    // An ask's turn queues no next prompt of its own.
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
}

/// A9 (d): a turn that ends any other way fails the ask, with the turn's own words.
#[test]
fn an_early_stop_fails_the_ask() {
    let (mut r, _) = answering();
    says(&mut r, T0 + 12 * SEC, "Looking");
    let fx = turn_ends(&mut r, T0 + 15 * SEC, Ok(StopReason::Cancelled));
    assert_eq!(r.row(1).status, AskStatus::Failed);
    assert_eq!(
        r.row(1).reply.as_deref(),
        Some("moa's turn ended early: cancelled")
    );
    assert_eq!(
        kite_hears(&fx),
        Err(OpError(
            "ask #1 failed: moa's turn ended early: cancelled".into()
        ))
    );

    let (mut r, _) = answering();
    let fx = turn_ends(&mut r, T0 + 15 * SEC, Err("overloaded".into()));
    assert_eq!(
        r.row(1).reply.as_deref(),
        Some("moa's turn failed: overloaded")
    );
    assert_eq!(
        kite_hears(&fx),
        Err(OpError(
            "ask #1 failed: moa's turn failed: overloaded".into()
        ))
    );
}

/// An empty answer is said as one, as a session's is.
#[test]
fn an_empty_answer_says_so() {
    let (mut r, _) = answering();
    let fx = turn_ends(&mut r, T0 + 15 * SEC, Ok(StopReason::EndTurn));
    assert_eq!(r.row(1).reply.as_deref(), Some("(no text)"));
    assert_eq!(kite_hears(&fx).unwrap().text, "(no text)");
}

/// A9 (e): an ask moa took whose turn is gone (a restart of the daemon, moa's agent gone
/// mid-turn) fails `moa restarted before answering`.
#[test]
fn an_orphan_ask_to_moa_fails() {
    // The daemon starts with one received and one sent; a pending one waits for its turn.
    let mut snap = snapshot();
    let ask = |id, status| Ask {
        status,
        claimed_at: Some(UtcMs(T0 - MIN)),
        ..ask_row(id, to(KITE), "Kite", AskTarget::Moa, "Is it?", T0 - 2 * MIN)
    };
    snap.asks = vec![
        ask(1, AskStatus::Received),
        ask(2, AskStatus::Sent),
        Ask {
            claimed_at: None,
            ..ask(3, AskStatus::Pending)
        },
    ];
    snap.next_ids.ask = 4;
    snap.meta
        .push((BRIEFED_KEY.into(), now(T0).local_day.to_string()));
    let (r, fx) = Rig::started_with(snap, &toml("acct-a"), T0);
    for id in [1, 2] {
        assert_eq!(r.row(id).status, AskStatus::Failed, "{fx:?}");
        assert_eq!(
            r.row(id).reply.as_deref(),
            Some("moa restarted before answering")
        );
    }
    // The pending one goes to moa, whose agent starts for it.
    assert_eq!(r.row(3).status, AskStatus::Pending);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Acp(AcpCmd::Start { .. }))),
        "{fx:?}"
    );

    // moa's agent goes in the middle of the ask's turn.
    let (mut r, _) = answering();
    let generation = r.st.central().generation;
    let fx = hear(
        &mut r,
        T0 + 12 * SEC,
        AcpIn::Exited {
            generation,
            why: "it crashed".into(),
        },
    );
    assert_eq!(r.row(1).status, AskStatus::Failed);
    assert_eq!(
        r.row(1).reply.as_deref(),
        Some("moa restarted before answering")
    );
    assert_eq!(
        kite_hears(&fx),
        Err(OpError(
            "ask #1 failed: moa restarted before answering".into()
        ))
    );
}

/// A9 (a): an ask and the day's opening both due, the ask goes first, on its own; the
/// opening goes after it, with the reports.
#[test]
fn asks_go_before_the_brief() {
    let mut snap = snapshot();
    snap.reports = vec![harbors_report(T0)];
    snap.next_ids.report = 2;
    let mut r = rig_unopened(snap);
    // You are here: the opening goes, but its thread can't be opened, which holds every
    // automatic prompt for a gap.
    let page = Op::ConsolePage { before: 10, n: 5 };
    let fx = r.op(9, you(), page, T0);
    let generation = r.st.central().generation;
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Acp(AcpCmd::Start { .. }))),
        "{fx:?}"
    );
    hear(&mut r, T0, started(generation, caps(true, true, true)));
    let refused = AcpIn::Opened {
        generation,
        result: Err("boom".into()),
    };
    let fx = hear(&mut r, T0, refused);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    kite_asks_moa(&mut r);
    // Once the gap is over, both are due: the ask goes, alone.
    let gap = r.st.config().moa.wake_gap_ms;
    let fx = handle_checked(&mut r.st, Input::Timer(TimerKey::Wake), now(T0 + gap));
    let fx = play_at(&mut r.st, now(T0 + gap), fx);
    assert_eq!(sent(&fx), KITES_ASK);
    assert!(body_of(you_row(&fx)).get("opening").is_none(), "{fx:?}");
    // The opening goes after it, carrying the report.
    let fx = turn_ends(&mut r, T0 + gap + 10 * SEC, Ok(StopReason::EndTurn));
    let body = body_of(you_row(&fx));
    assert_eq!(body["opening"], json!(true), "{body}");
    assert_eq!(body["reports"], json!(1), "{body}");
    assert!(body.get("ask").is_none(), "{body}");
    assert!(sent(&fx).contains("Harbor tidied the folder."));
}

/// `/auto off` stops the wake-ups, not an ask: someone waits for it.
#[test]
fn auto_off_still_answers_asks() {
    let mut snap = snapshot();
    snap.meta.push(("moa.auto".into(), "off".into()));
    let mut r = rig_from(snap);
    let fx = kite_asks_moa(&mut r);
    let fx = play_at(&mut r.st, now(T0), fx);
    assert_eq!(sent(&fx), KITES_ASK);
}

// ---------- moa asks ----------

/// moa's `moactl ask` on connection 5 at `ms`: `to`, waiting 540 s.
fn moa_asks(r: &mut Rig, ms: i64, to: &str) -> Vec<Effect> {
    let op = Op::Ask {
        to: to.into(),
        text: "Which branch?".into(),
        timeout_s: 540,
    };
    r.op(5, moa(), op, ms)
}

/// `Caller::Moa` asks a session: the ask is moa's, labelled `moa`, and typed into its tab.
#[test]
fn moa_can_ask_a_session() {
    let mut r = rig();
    let fx = moa_asks(&mut r, T0, "Kite");
    let saved = saved_asks(&fx);
    assert_eq!(saved[0].from_session, Some(AskTarget::Moa));
    assert_eq!(saved[0].from_name.as_deref(), Some("moa"));
    assert_eq!(r.row(1).status, AskStatus::Sent);
    let text = framed(1, "moa", "Which branch?");
    assert_eq!(
        typed(&fx),
        [
            Cmd::set_buffer("moa-ask-1", &text),
            Cmd::paste_buffer("moa-ask-1", "%1".parse().unwrap()),
        ]
    );
}

/// The `Watchdog` timer as `fx` leaves it: `Some(Some(at))` set, `Some(None)` cancelled,
/// `None` untouched.
fn watchdog_timer(fx: &[Effect]) -> Option<Option<UtcMs>> {
    timers(fx, TimerKey::Watchdog).last().map(|e| match e {
        Effect::SetTimer { at, .. } => Some(*at),
        _ => None,
    })
}

fn fire_watchdog(r: &mut Rig, ms: i64) -> Vec<Effect> {
    handle_checked(&mut r.st, Input::Timer(TimerKey::Watchdog), now(ms))
}

fn cancels(fx: &[Effect]) -> bool {
    fx.iter()
        .any(|e| matches!(e, Effect::Acp(AcpCmd::Cancel { .. })))
}

/// Review Focus 4 (A10): while moa waits on its own ask, no watchdog cancel; it counts again
/// from the ask's end. Kite answers at 520 s; Harbor never does, and its ask is dropped at
/// 540 s.
#[test]
fn the_watchdog_waits_for_moas_ask() {
    let mut r = rig();
    let watchdog = r.st.config().moa.watchdog_ms;
    let fx = your_prompt(&mut r, T0, "Ask Kite which branch");
    assert_eq!(watchdog_timer(&fx), Some(Some(UtcMs(T0 + watchdog))));
    let asked = T0 + 10 * SEC;
    let fx = moa_asks(&mut r, asked, "Kite");
    assert_eq!(r.row(1).status, AskStatus::Sent);
    assert_eq!(watchdog_timer(&fx), Some(None), "{fx:?}");
    r.started(asked + 2 * SEC, KITE, &framed(1, "moa", "Which branch?"));
    assert_eq!(r.row(1).status, AskStatus::Received);
    // Long past the watchdog's span: nothing is cancelled.
    let fx = fire_watchdog(&mut r, T0 + watchdog + MIN);
    assert!(!cancels(&fx), "{fx:?}");
    assert!(!matches!(watchdog_timer(&fx), Some(Some(_))), "{fx:?}");
    // Kite answers at 520 s: moa hears it, and the watchdog counts from then.
    let answer = asked + 520 * SEC;
    let fx = r.stopped(answer, KITE, "main");
    assert_eq!(
        replies(&fx)[0].2.as_ref().map(|d| d.text.as_str()),
        Ok("main")
    );
    assert_eq!(watchdog_timer(&fx), Some(Some(UtcMs(answer + watchdog))));
    let fx = fire_watchdog(&mut r, answer + watchdog);
    assert!(cancels(&fx), "{fx:?}");

    // Harbor is busy and never takes it: dropped at 540 s, the watchdog counts from then.
    let mut r = rig();
    your_prompt(&mut r, T0, "Ask Harbor which branch");
    let fx = moa_asks(&mut r, asked, "Harbor");
    assert_eq!(r.row(1).status, AskStatus::Pending);
    assert_eq!(watchdog_timer(&fx), Some(None), "{fx:?}");
    let fx = fire_watchdog(&mut r, T0 + watchdog + MIN);
    assert!(!cancels(&fx), "{fx:?}");
    let dropped = asked + 540 * SEC;
    let fx = r.fire(dropped);
    assert_eq!(r.row(1).status, AskStatus::Dropped);
    assert_eq!(
        replies(&fx),
        [(
            ConnId(5),
            ReqId(2),
            Err(OpError(
                "Harbor stayed busy for 9m; ask #1 was dropped".into()
            ))
        )]
    );
    assert_eq!(watchdog_timer(&fx), Some(Some(UtcMs(dropped + watchdog))));
}

/// The watchdog holds only while moa waits: a session's ask doesn't hold it, nor does an ask
/// of moa's whose request went (its `moactl` killed).
#[test]
fn the_watchdog_holds_only_while_moa_waits() {
    let mut r = rig();
    let watchdog = r.st.config().moa.watchdog_ms;
    your_prompt(&mut r, T0, "Tidy the Harbor folder");
    let fx = r.ask(3, from(BEACON), "Kite", "Which branch?", 540);
    assert_eq!(watchdog_timer(&fx), None, "{fx:?}");
    let fx = moa_asks(&mut r, T0 + 10 * SEC, "Harbor");
    assert_eq!(watchdog_timer(&fx), Some(None), "{fx:?}");
    let gone = T0 + 20 * SEC;
    let fx = handle_checked(&mut r.st, Input::ClientGone(ConnId(5)), now(gone));
    assert_eq!(watchdog_timer(&fx), Some(Some(UtcMs(gone + watchdog))));
    // The ask goes on, pending.
    assert_eq!(r.row(2).status, AskStatus::Pending);
    // Beacon waits on moa's ask too: that holds nothing of moa's.
    let fx = r.op(
        6,
        from(BEACON),
        Op::Reply {
            id: 2,
            timeout_s: 540,
        },
        gone + SEC,
    );
    assert!(replies(&fx).is_empty(), "it waits: {fx:?}");
    assert!(!matches!(watchdog_timer(&fx), Some(None)), "{fx:?}");
    let fx = fire_watchdog(&mut r, gone + watchdog);
    assert!(cancels(&fx), "{fx:?}");
}

/// Review Focus 4, the other "never": Kite takes moa's ask and never answers. The watchdog
/// holds until moa's wait ends with "no reply yet", then counts from then.
#[test]
fn the_watchdog_waits_out_a_taken_ask() {
    let mut r = rig();
    let watchdog = r.st.config().moa.watchdog_ms;
    your_prompt(&mut r, T0, "Ask Kite which branch");
    let asked = T0 + 10 * SEC;
    moa_asks(&mut r, asked, "Kite");
    r.started(asked + 2 * SEC, KITE, &framed(1, "moa", "Which branch?"));
    assert_eq!(r.row(1).status, AskStatus::Received);
    let fx = fire_watchdog(&mut r, T0 + watchdog + MIN);
    assert!(!cancels(&fx), "{fx:?}");
    let waited = asked + 540 * SEC;
    let fx = r.fire(waited);
    assert_eq!(
        replies(&fx)[0].2.as_ref().map(|d| d.text.as_str()),
        Ok("no reply yet from Kite; `moactl reply 1` waits again")
    );
    assert_eq!(r.row(1).status, AskStatus::Received);
    assert_eq!(watchdog_timer(&fx), Some(Some(UtcMs(waited + watchdog))));
    let fx = fire_watchdog(&mut r, waited + watchdog);
    assert!(cancels(&fx), "{fx:?}");
}

// ---------- moa can't answer ----------

/// A9, A2: with no account moa's agent can run on, an ask to moa is refused at once.
#[test]
fn an_ask_to_moa_that_cant_run_is_refused() {
    let mut r = Rig::linked(snapshot());
    let fx = kite_asks_moa(&mut r);
    assert_eq!(the_error(&fx), "moa isn't running");
    assert!(saved_asks(&fx).is_empty(), "{fx:?}");
}

/// A9: moa's agent fails to start while an ask waits for it: the ask is dropped at once, and
/// a new one is refused, until your prompt starts moa again.
#[test]
fn an_ask_to_moa_that_fails_to_start_is_dropped() {
    let mut r = rig();
    let fx = kite_asks_moa(&mut r);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Acp(AcpCmd::Start { .. }))),
        "{fx:?}"
    );
    let generation = r.st.central().generation;
    let fx = hear(
        &mut r,
        T0 + SEC,
        AcpIn::Exited {
            generation,
            why: "no such program".into(),
        },
    );
    assert_eq!(r.row(1).status, AskStatus::Dropped);
    assert_eq!(r.row(1).reply.as_deref(), Some("moa isn't running"));
    assert_eq!(
        kite_hears(&fx),
        Err(OpError("moa isn't running; ask #1 was dropped".into()))
    );
    let fx = r.op(
        KITE_CONN,
        from(KITE),
        Op::Ask {
            to: "moa".into(),
            text: "Is it?".into(),
            timeout_s: 540,
        },
        T0 + 2 * SEC,
    );
    assert_eq!(the_error(&fx), "moa isn't running");
}
