use std::time::Duration;

use jiff::tz::TimeZone;

use super::*;
use crate::config::{BrainName, Config};
use crate::effect::RefuseWrites;
use crate::state::Snapshot;

const T: i64 = 1_790_000_000_000;
const HARBOR: &str = "3f2a91c4-0000-4000-8000-0000000000a2";

fn now(ms: i64) -> Now {
    Now::at(UtcMs(ms), Duration::ZERO, TimeZone::UTC)
}

/// Ask `id` from moa to a session moa no longer has, typed into its pane.
fn sent(id: i64) -> Ask {
    Ask {
        id: AskId(id),
        at: UtcMs(T - 60_000),
        from_session: Some(AskTarget::Moa),
        from_name: Some("moa".into()),
        to_session: Some(AskTarget::Session(HARBOR.parse().unwrap())),
        text: "Is the domain renewed?".into(),
        status: AskStatus::Sent,
        reply: None,
        claimed_at: Some(UtcMs(T - 50_000)),
        answered_at: None,
    }
}

fn state_with(asks: Vec<Ask>) -> State {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    st.load(Snapshot {
        asks,
        ..Snapshot::default()
    });
    st
}

fn replies(fx: &[Effect]) -> Vec<(ConnId, ReqId, Result<Done, OpError>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply { conn, req, result } => Some((*conn, *req, result.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn end_answers_every_request_waiting_once() {
    let mut st = state_with(vec![sent(12), sent(13)]);
    park(&mut st, ConnId(1), ReqId(1), AskId(12), UtcMs(T + 1_000));
    park(&mut st, ConnId(2), ReqId(5), AskId(12), UtcMs(T + 9_000));
    park(&mut st, ConnId(3), ReqId(1), AskId(13), UtcMs(T + 1_000));
    let mut tx = Tx::new(&now(T));
    let fx = end(
        &mut st,
        &mut tx,
        AskId(12),
        AskStatus::Answered,
        "yes".into(),
    );
    assert_eq!(tx.changes.len(), 1);
    let row = st.ask(AskId(12)).unwrap();
    assert_eq!(row.status, AskStatus::Answered);
    assert_eq!(row.reply.as_deref(), Some("yes"));
    assert_eq!(row.answered_at, Some(UtcMs(T)));
    let heard = replies(&fx);
    assert_eq!(heard.len(), 2, "{fx:?}");
    for (conn, _, result) in &heard {
        let done = result.as_ref().unwrap();
        assert_eq!(done.text, "yes", "{conn:?}");
        assert_eq!(
            done.data,
            json!({"id": 12, "from": "moa", "to": "3f2a91c4", "status": "answered"})
        );
    }
    assert_eq!(heard[0].0, ConnId(1));
    assert_eq!(heard[1].0, ConnId(2));
    // The other ask's request still waits.
    assert_eq!(st.asks_mem.parked.len(), 1);
    assert_eq!(st.asks_mem.parked[0].ask, AskId(13));
    // An ask ends once.
    let fx = end(
        &mut st,
        &mut tx,
        AskId(12),
        AskStatus::Failed,
        "stale".into(),
    );
    assert!(fx.is_empty());
    assert_eq!(tx.changes.len(), 1);
    assert_eq!(st.ask(AskId(12)).unwrap().status, AskStatus::Answered);
}

#[test]
fn a_refused_end_replies_nothing() {
    let mut st = state_with(vec![sent(12)]);
    st.refuse_writes = Some(RefuseWrites {
        until: UtcMs(T + 1),
        reason: "the disk is full".into(),
    });
    park(&mut st, ConnId(1), ReqId(1), AskId(12), UtcMs(T + 1_000));
    let mut tx = Tx::new(&now(T));
    let fx = end(
        &mut st,
        &mut tx,
        AskId(12),
        AskStatus::Answered,
        "yes".into(),
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(tx.changes.is_empty());
    assert_eq!(st.ask(AskId(12)).unwrap().status, AskStatus::Sent);
    assert_eq!(st.asks_mem.parked.len(), 1);
}

#[test]
fn an_ended_ask_says_why() {
    let st = state_with(Vec::new());
    let ended = |status, reason: &str| Ask {
        status,
        reply: Some(reason.into()),
        answered_at: Some(UtcMs(T + 480_000)),
        ..sent(12)
    };
    let said = |a: Ask| answer_of(&st, &a).unwrap().unwrap_err().0;
    assert_eq!(
        said(ended(
            AskStatus::Failed,
            "no receipt (a dialog or copy mode had the pane)"
        )),
        "ask #12 failed: no receipt (a dialog or copy mode had the pane)"
    );
    assert_eq!(
        said(ended(AskStatus::Dropped, STAYED_BUSY)),
        "3f2a91c4 stayed busy for 9m; ask #12 was dropped"
    );
    assert_eq!(
        said(ended(AskStatus::Dropped, "Harbor ended")),
        "Harbor ended; ask #12 was dropped"
    );
    let short = Ask {
        answered_at: Some(UtcMs(T - 30_000)),
        ..ended(AskStatus::Dropped, STAYED_BUSY)
    };
    assert_eq!(
        said(short),
        "3f2a91c4 stayed busy for 30s; ask #12 was dropped"
    );
    assert!(answer_of(&st, &sent(12)).is_none());
}

#[test]
fn cleaning_keeps_lines_and_tabs() {
    assert_eq!(clean(" \u{1b}a\u{7f}\u{85}\r\nb\tc\u{0} "), "a\nb\tc");
    assert_eq!(clean("\u{1b}\r\n"), "");
}

/// The brief's step: `reply 12` waits on a sent ask, and `end(…, answered, "yes")` answers
/// it `yes`.
#[test]
fn end_answers_a_waiting_reply() {
    let mut st = state_with(vec![sent(12)]);
    let fx = reply(&mut st, &now(T), ConnId(4), ReqId(2), 12, 30);
    assert!(fx.is_empty(), "{fx:?}");
    let mut tx = Tx::new(&now(T + 1_000));
    let fx = end(
        &mut st,
        &mut tx,
        AskId(12),
        AskStatus::Answered,
        "yes".into(),
    );
    match &replies(&fx)[..] {
        [(ConnId(4), ReqId(2), Ok(done))] => assert_eq!(done.text, "yes"),
        other => panic!("{other:?}"),
    }
    assert!(st.asks_mem.parked.is_empty());
}

#[test]
fn moas_state_in_peers() {
    use crate::central::Phase;
    let mut st = state_with(Vec::new());
    for (phase, word) in [
        (Phase::Off, "off"),
        (Phase::Failed("x".into()), "off"),
        (Phase::Ready, "ready"),
        (Phase::Starting, "busy"),
        (Phase::Thinking, "busy"),
    ] {
        st.central.phase = phase;
        assert_eq!(moa_state(&st), word);
    }
}

#[test]
fn a_marker_names_its_ask() {
    let marker = |p: &str| marker_id(p);
    assert_eq!(
        marker("[moa · message from Kite · ask #12] Is the domain renewed?"),
        Some(AskId(12))
    );
    // Anywhere in the prompt.
    assert_eq!(
        marker("Pasted:\n> [moa · message from Kite · ask #7] hi"),
        Some(AskId(7))
    );
    // A label is cut at its last `· ask #`.
    assert_eq!(
        marker("[moa · message from a · ask #3 b · ask #12] hi"),
        Some(AskId(12))
    );
    // The first marker that is one.
    assert_eq!(
        marker("[moa · message from Kite · ask #x] [moa · message from Kite · ask #4] hi"),
        Some(AskId(4))
    );
    for not in [
        "",
        "Is the domain renewed?",
        "[moa · message from Kite · ask #12 hi",
        "[moa · message from Kite · ask #] hi",
        "[moa · message from Kite · ask #-3] hi",
        "[moa · message from Kite · ask #1 2] hi",
        "[moa · message from Kite] hi",
        "moa · message from Kite · ask #12] hi",
        "[moa · message from Kite · ask #99999999999999999999] hi",
    ] {
        assert_eq!(marker(not), None, "{not}");
    }
}

#[test]
fn framed_is_the_marker_then_the_text() {
    let st = state_with(Vec::new());
    let a = Ask {
        from_name: Some("Kite".into()),
        ..sent(12)
    };
    let text = framed(&st, &a);
    assert_eq!(
        text,
        "[moa · message from Kite · ask #12] Is the domain renewed?\n\n\
         (Your reply this turn goes back to Kite as the answer.)"
    );
    assert_eq!(marker_id(&text), Some(AskId(12)));
}
