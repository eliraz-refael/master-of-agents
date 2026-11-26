//! Asks between agents (slice 7, A1–A4, A11, A13): `ask` saves a `pending` row and waits for
//! the answer, `reply` waits again, `peers` lists who can be asked, and `asks` lists the
//! caller's asks; A5–A8: an ask to a session is typed into its pane when it is free, taken by
//! the marker in its next prompt, and answered by the full message that ends that turn. The
//! tests before the link is up see an ask stay `pending`.

mod common;

use moa_core::UtcMs;
use moa_core::asks::{ASK_TEXT_MAX, ASK_WAIT_DEFAULT_MS, STALE_AFTER_MS};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TmuxIn, handle_checked};
use moa_core::hook::{HookKind, StartSource};
use moa_core::model::{Activity, Ask, AskId, AskStatus, AskTarget, Lifecycle, Location, Session};
use moa_core::op::{ConnId, Done, NoParams, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Snapshot};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::PaneId;
use moa_core::tmux::notify::Notification;
use serde_json::Value;

use common::asks::*;

// ---------- ask ----------

#[test]
fn an_ask_waits_pending() {
    let mut r = Rig::new(snapshot());
    let fx = r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let saved = saved_asks(&fx);
    assert_eq!(
        saved,
        [Ask {
            id: AskId(1),
            at: UtcMs(T0),
            from_session: Some(to(KITE)),
            from_name: Some("Kite".into()),
            to_session: Some(to(HARBOR)),
            text: "Is the domain renewed?".into(),
            status: AskStatus::Pending,
            reply: None,
            claimed_at: None,
            answered_at: None,
        }]
    );
    assert_eq!(r.row(1).status, AskStatus::Pending);
    // Its wait is the one deadline.
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 540 * SEC)));
    // moa asks too: its label is `moa`.
    let fx = r.ask(2, moa(), "beacon", "Which branch?", 60);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let saved = saved_asks(&fx);
    assert_eq!(saved[0].from_session, Some(AskTarget::Moa));
    assert_eq!(saved[0].from_name.as_deref(), Some("moa"));
    assert_eq!(saved[0].to_session, Some(to(BEACON)));
    // The earlier deadline wins.
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 60 * SEC)));
}

#[test]
fn a_session_asks_moa() {
    let mut r = Rig::new(snapshot());
    let fx = r.ask(1, from(KITE), "moa", "Which account is Harbor on?", 540);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(saved_asks(&fx)[0].to_session, Some(AskTarget::Moa));
}

#[test]
fn an_ask_refuses() {
    // Harbor's ask to Kite, open in each of its open statuses.
    let deadlock = |status: AskStatus| {
        let mut snap = snapshot();
        let mut theirs = ask_row(9, to(HARBOR), "Harbor", to(KITE), "Which branch?", T0 - MIN);
        theirs.status = status;
        snap.asks = vec![theirs];
        snap.next_ids.ask = 10;
        snap
    };
    let waiting = || "Harbor is waiting on your answer to ask #9; answer that first".to_string();
    let at_most = format!("an ask is at most {ASK_TEXT_MAX} characters; point to a file instead");
    let long = "x".repeat(ASK_TEXT_MAX + 1);
    let cases: Vec<(Snapshot, CallerInfo, &str, &str, i64, String)> = vec![
        (
            snapshot(),
            from(KITE),
            "Kite",
            "hi",
            540,
            "you can't ask yourself".into(),
        ),
        (
            snapshot(),
            moa(),
            "moa",
            "hi",
            540,
            "you can't ask yourself".into(),
        ),
        (
            snapshot(),
            shell(),
            "Harbor",
            "hi",
            540,
            "ask works from a session or moa; type into the session yourself".into(),
        ),
        (
            snapshot(),
            from(KITE),
            "Lantern",
            "hi",
            540,
            "Lantern isn't running; `moactl resume 5d8e2f10` starts it, then ask again".into(),
        ),
        (
            deadlock(AskStatus::Pending),
            from(KITE),
            "Harbor",
            "hi",
            540,
            waiting(),
        ),
        (
            deadlock(AskStatus::Sent),
            from(KITE),
            "Harbor",
            "hi",
            540,
            waiting(),
        ),
        (
            deadlock(AskStatus::Received),
            from(KITE),
            "Harbor",
            "hi",
            540,
            waiting(),
        ),
        (snapshot(), from(KITE), "Harbor", &long, 540, at_most),
        (
            snapshot(),
            from(KITE),
            "Harbor",
            " \u{1b}\r\0\u{7f} \n\t ",
            540,
            "an ask can't be empty".into(),
        ),
        (
            snapshot(),
            from(KITE),
            "Harbor",
            "hi",
            0,
            "a timeout is 1 to 3600 seconds".into(),
        ),
        (
            snapshot(),
            from(KITE),
            "Harbor",
            "hi",
            3601,
            "a timeout is 1 to 3600 seconds".into(),
        ),
        (
            snapshot(),
            from(KITE),
            "Nobody",
            "hi",
            540,
            "no session matches 'Nobody'".into(),
        ),
    ];
    for (snap, caller, target, text, timeout, want) in cases {
        let mut r = Rig::new(snap);
        let fx = r.ask(1, caller, target, text, timeout);
        assert_eq!(the_error(&fx), want, "asking {target}");
        assert!(!persisted(&fx), "asking {target}: {fx:?}");
        // Nothing waits: no timer.
        assert_eq!(set_at(&fx), None, "asking {target}");
    }
}

#[test]
fn an_ask_at_its_limits_is_saved() {
    // 8000 characters of emoji are 8000 characters, and a wait of an hour is allowed.
    let mut r = Rig::new(snapshot());
    let text = "🌊".repeat(ASK_TEXT_MAX);
    let fx = r.ask(1, from(KITE), "Harbor", &text, 3600);
    assert!(replies(&fx).is_empty(), "{:?}", replies(&fx));
    assert_eq!(saved_asks(&fx)[0].text, text);
    let fx = r.ask(2, from(KITE), "Beacon", "hi", 1);
    assert!(replies(&fx).is_empty(), "{:?}", replies(&fx));
}

#[test]
fn names_prefer_live_sessions() {
    // Two ended Lanterns: none live, and more than one.
    let mut snap = snapshot();
    let mut beacon = session(BEACON, "Lantern", "beacon", Activity::Idle);
    beacon.lifecycle = Lifecycle::Ended;
    snap.sessions.retain(|s| s.id != sid(BEACON));
    snap.sessions.push(beacon);
    let r = Rig::new(snap);
    assert_eq!(
        r.st.peer("Lantern"),
        Err(OpError("no live session matches 'Lantern'".into()))
    );
    // An ended Kite, and the live one.
    let mut snap = snapshot();
    let mut old = session(LANTERN, "Kite", "kite", Activity::Idle);
    old.lifecycle = Lifecycle::Ended;
    snap.sessions.retain(|s| s.id != sid(LANTERN));
    snap.sessions.push(old);
    let r = Rig::new(snap);
    assert_eq!(r.st.peer("kite"), Ok(to(KITE)));
    assert_eq!(r.st.peer("moa"), Ok(AskTarget::Moa));
    // Only an ended one: it is the target, refused later as not running.
    let mut snap = snapshot();
    snap.sessions.retain(|s| s.id != sid(KITE));
    snap.pane_bindings.retain(|b| b.session_id != sid(KITE));
    let r = Rig::new(snap);
    assert_eq!(r.st.peer("lantern"), Ok(to(LANTERN)));
    // Two live ones are listed.
    let mut snap = snapshot();
    snap.sessions
        .iter_mut()
        .find(|s| s.id == sid(BEACON))
        .unwrap()
        .title = Some("Kite".into());
    let r = Rig::new(snap);
    let err = r.st.peer("Kite").unwrap_err().0;
    assert!(
        err.starts_with("2 sessions match 'Kite'; be more specific:"),
        "{err}"
    );
    assert!(
        err.contains("7c41d0e2") && err.contains("b9e0c3d5"),
        "{err}"
    );
}

#[test]
fn live_and_free() {
    let mut snap = snapshot();
    let mut sent = ask_row(3, to(KITE), "Kite", to(BEACON), "Which branch?", T0 - MIN);
    sent.status = AskStatus::Sent;
    snap.asks = vec![sent];
    let r = Rig::new(snap);
    assert!(r.st.is_live(sid(KITE)) && r.st.is_live(sid(HARBOR)));
    assert!(!r.st.is_live(sid(LANTERN)));
    // Kite is idle; Harbor works; Beacon has an ask in flight.
    assert!(r.st.is_free(sid(KITE)));
    assert!(!r.st.is_free(sid(HARBOR)));
    assert!(!r.st.is_free(sid(BEACON)));
    assert!(!r.st.is_free(sid(LANTERN)));
}

/// [`snapshot`], Beacon changed by `change`.
fn with_beacon(change: impl Fn(&mut Session)) -> Snapshot {
    let mut snap = snapshot();
    change(
        snap.sessions
            .iter_mut()
            .find(|s| s.id == sid(BEACON))
            .unwrap(),
    );
    snap
}

/// Each of A3's conditions alone makes Beacon not live.
#[test]
fn each_condition_makes_a_session_not_live() {
    assert!(Rig::new(snapshot()).st.is_live(sid(BEACON)));
    let cases: Vec<(&str, Snapshot)> = vec![
        (
            "archived",
            with_beacon(|s| s.archived_at = Some(UtcMs(T0 - MIN))),
        ),
        (
            "elsewhere",
            with_beacon(|s| s.location = Location::Elsewhere),
        ),
        ("ended", with_beacon(|s| s.lifecycle = Lifecycle::Ended)),
        (
            "asleep",
            with_beacon(|s| s.asleep_at = Some(UtcMs(T0 - MIN))),
        ),
        ("unbound", {
            let mut snap = snapshot();
            snap.pane_bindings.retain(|b| b.session_id != sid(BEACON));
            snap
        }),
    ];
    for (why, snap) in cases {
        assert!(!Rig::new(snap).st.is_live(sid(BEACON)), "{why}");
    }
}

#[test]
fn ask_text_is_cleaned() {
    let mut r = Rig::new(snapshot());
    let text =
        "  \u{1b}[201~Is it\r\0 renewed;#{pane_id}\"$(touch /tmp/x)\u{7f}\u{9b}?\n\tok \\ \n";
    let fx = r.ask(1, from(KITE), "Harbor", text, 540);
    assert!(replies(&fx).is_empty(), "{:?}", replies(&fx));
    assert_eq!(
        saved_asks(&fx)[0].text,
        "[201~Is it renewed;#{pane_id}\"$(touch /tmp/x)?\n\tok \\"
    );
}

// ---------- waiting ----------

#[test]
fn a_pending_ask_is_dropped_at_its_wait() {
    let mut r = Rig::new(snapshot());
    r.ask(4, from(KITE), "Harbor", "Is the domain renewed?", 540);
    // A timer that fires early changes nothing, and is armed again.
    let fx = r.fire(T0 + 100 * SEC);
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 540 * SEC)));
    let fx = r.fire(T0 + 540 * SEC);
    let saved = saved_asks(&fx);
    assert_eq!(saved.len(), 1, "{fx:?}");
    assert_eq!(saved[0].status, AskStatus::Dropped);
    assert_eq!(saved[0].reply.as_deref(), Some("stayed busy"));
    assert_eq!(
        replies(&fx),
        [(
            ConnId(4),
            ReqId(1),
            Err(OpError(
                "Harbor stayed busy for 9m; ask #1 was dropped".into()
            ))
        )]
    );
    assert!(cancelled(&fx), "{fx:?}");
    // Asked again, it says so at once.
    let fx = r.op(
        5,
        from(KITE),
        Op::Reply {
            id: 1,
            timeout_s: 540,
        },
        T0 + 600 * SEC,
    );
    assert_eq!(
        the_error(&fx),
        "Harbor stayed busy for 9m; ask #1 was dropped"
    );
    assert_eq!(set_at(&fx), None);
}

#[test]
fn an_open_ask_says_no_reply_yet() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    // Delivered and taken, by hand (a `sent` one would fail at 20 s for want of a receipt).
    let mut sent = r.row(1);
    sent.status = AskStatus::Received;
    sent.claimed_at = Some(UtcMs(T0 + SEC));
    r.reload(T0 + SEC, vec![sent.clone()]);
    let until = T0 + ASK_WAIT_DEFAULT_MS;
    let fx = r.fire(until);
    assert!(!persisted(&fx), "{fx:?}");
    let no_reply = "no reply yet from Harbor; `moactl reply 1` waits again";
    match &replies(&fx)[..] {
        [(ConnId(1), ReqId(1), Ok(done))] => assert_eq!(done.text, no_reply),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.row(1), sent);
    assert!(cancelled(&fx));
    // `reply` waits again; the timer is armed at its wait.
    let fx = r.op(
        2,
        from(KITE),
        Op::Reply {
            id: 1,
            timeout_s: 30,
        },
        until,
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(set_at(&fx), Some(UtcMs(until + 30 * SEC)));
    // The row turns answered (here by hand): the wait hears the answer.
    let mut answered = sent;
    answered.status = AskStatus::Answered;
    answered.reply = Some("yes".into());
    answered.answered_at = Some(UtcMs(until + SEC));
    let fx = r.reload(until + SEC, vec![answered]);
    match &replies(&fx)[..] {
        [(ConnId(2), _, Ok(done))] => {
            assert_eq!(done.text, "yes");
            assert_eq!(done.data["id"], 1);
            assert_eq!(done.data["status"], "answered");
        }
        other => panic!("{other:?}"),
    }
    // Nothing waits: no timer is armed (a reload forgets the one armed before it, as the
    // other timers' are; one that fires finds nothing to do).
    assert_eq!(set_at(&fx), None);
    assert!(
        r.fire(until + 30 * SEC)
            .iter()
            .all(|e| !matches!(e, Effect::Reply { .. }))
    );
}

#[test]
fn reply_needs_an_ask() {
    let mut r = Rig::new(snapshot());
    let fx = r.op(
        1,
        from(KITE),
        Op::Reply {
            id: 12,
            timeout_s: 540,
        },
        T0,
    );
    assert_eq!(the_error(&fx), "no ask #12");
    let fx = r.op(
        1,
        from(KITE),
        Op::Reply {
            id: 12,
            timeout_s: 0,
        },
        T0,
    );
    assert_eq!(the_error(&fx), "a timeout is 1 to 3600 seconds");
}

#[test]
/// A connection that goes drops its requests and nothing else: each pending ask keeps its own
/// wait, and is dropped at it with nobody to hear (R3, amended: an asker whose call was
/// killed mustn't have its question typed hours later).
fn a_gone_client_leaves_the_ask() {
    let mut r = Rig::new(snapshot());
    r.ask(7, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(8, from(BEACON), "Harbor", "Which branch?", 60);
    // Connection 8 goes: nothing is heard, and ask 2 keeps its 60 s.
    let fx = handle_checked(&mut r.st, Input::ClientGone(ConnId(8)), now(T0 + SEC));
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    assert_eq!(r.row(2).status, AskStatus::Pending);
    let fx = r.fire(T0 + 60 * SEC);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(2).status, AskStatus::Dropped);
    assert_eq!(r.row(2).reply.as_deref(), Some("stayed busy"));
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 540 * SEC)));
    // Then 7: its ask is dropped at its own wait too.
    let fx = handle_checked(&mut r.st, Input::ClientGone(ConnId(7)), now(T0 + 61 * SEC));
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    let fx = r.fire(T0 + 540 * SEC);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Dropped);
    assert!(cancelled(&fx), "{fx:?}");
}

/// After a restart an ask's own wait is forgotten: a pending one waits the default from when
/// it was saved.
#[test]
fn a_pending_ask_from_before_a_restart_waits_the_default() {
    let mut snap = snapshot();
    let old = ask_row(
        1,
        to(KITE),
        "Kite",
        to(HARBOR),
        "Is it?",
        T0 - ASK_WAIT_DEFAULT_MS,
    );
    let young = ask_row(2, to(BEACON), "Beacon", to(HARBOR), "Which?", T0 - MIN);
    snap.asks = vec![old, young];
    snap.next_ids.ask = 3;
    let (r, fx) = Rig::started_at(snap, T0);
    assert_eq!(r.row(1).status, AskStatus::Dropped);
    assert_eq!(r.row(2).status, AskStatus::Pending);
    assert_eq!(set_at(&fx), Some(UtcMs(T0 - MIN + ASK_WAIT_DEFAULT_MS)));
}

// ---------- peers and asks ----------

#[test]
fn peers_lists_live_sessions() {
    let mut snap = snapshot();
    snap.asks = vec![ask_row(
        1,
        to(BEACON),
        "Beacon",
        to(HARBOR),
        "Which branch?",
        T0 - 5 * MIN,
    )];
    snap.next_ids.ask = 2;
    let mut r = Rig::new(snap);
    let fx = r.op(1, from(KITE), Op::Peers(NoParams), T0);
    let done = match &replies(&fx)[..] {
        [(_, _, Ok(done))] => done.clone(),
        other => panic!("{other:?}"),
    };
    insta::assert_snapshot!(done.text);
    let rows = done.data.as_array().unwrap();
    let handles: Vec<&str> = rows.iter().map(|r| r["handle"].as_str().unwrap()).collect();
    assert_eq!(handles, ["moa", "b9e0c3d5", "3f2a91c4"]);
    let harbor = &rows[2];
    assert_eq!(harbor["id"], HARBOR);
    assert_eq!(harbor["label"], "Harbor");
    assert_eq!(harbor["state"], "working");
    assert_eq!(harbor["folder"], "~/private/harbor");
    assert_eq!(harbor["last_asked"], T0 - 5 * MIN);
    assert_eq!(harbor["agent_kind"], "claude");
    assert_eq!(harbor["cwd"], format!("{HOME}/private/harbor"));
    assert_eq!(rows[1]["last_asked"], Value::Null);
    assert_eq!(rows[0]["state"], "off");
    // moa sees every live session, and not itself.
    let fx = r.op(1, moa(), Op::Peers(NoParams), T0);
    let rows = match &replies(&fx)[..] {
        [(_, _, Ok(done))] => done.data.as_array().unwrap().clone(),
        other => panic!("{other:?}"),
    };
    let handles: Vec<&str> = rows.iter().map(|r| r["handle"].as_str().unwrap()).collect();
    assert_eq!(handles, ["b9e0c3d5", "3f2a91c4", "7c41d0e2"]);
}

#[test]
fn asks_lists_mine() {
    let mut snap = snapshot();
    let mut answered = ask_row(
        1,
        to(KITE),
        "Kite",
        to(HARBOR),
        "Is the domain renewed?",
        T0 - 90 * MIN,
    );
    answered.status = AskStatus::Answered;
    answered.reply = Some("yes".into());
    let other = ask_row(
        2,
        to(BEACON),
        "Beacon",
        to(HARBOR),
        "Which branch?",
        T0 - 8 * MIN,
    );
    let long = "Which of the three lamps did you rewire on Friday, and was the dimmer one of them?\nThanks";
    let to_kite = ask_row(3, AskTarget::Moa, "moa", to(KITE), long, T0 - 5 * MIN);
    snap.asks = vec![answered, other, to_kite];
    snap.next_ids.ask = 4;
    let mut r = Rig::new(snap);
    let fx = r.op(1, from(KITE), Op::Asks(NoParams), T0);
    let done = match &replies(&fx)[..] {
        [(_, _, Ok(done))] => done.clone(),
        other => panic!("{other:?}"),
    };
    insta::assert_snapshot!("asks_lists_mine", done.text);
    let ids: Vec<i64> = done
        .data
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [3, 1]);
    // From a shell, every ask.
    let fx = r.op(1, shell(), Op::Asks(NoParams), T0);
    let done = match &replies(&fx)[..] {
        [(_, _, Ok(done))] => done.clone(),
        other => panic!("{other:?}"),
    };
    insta::assert_snapshot!("asks_lists_every_ask", done.text);
}

/// Only the ask's own wait drops a pending ask (M2): a `reply` that runs out on it says no
/// reply yet, and the ask stays pending for its own wait.
#[test]
fn a_reply_wait_leaves_a_pending_ask() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    let fx = r.op(
        2,
        from(BEACON),
        Op::Reply {
            id: 1,
            timeout_s: 1,
        },
        T0,
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + SEC)));
    let fx = r.fire(T0 + SEC);
    assert!(!persisted(&fx), "{fx:?}");
    match &replies(&fx)[..] {
        [(ConnId(2), _, Ok(done))] => assert_eq!(
            done.text,
            "no reply yet from Harbor; `moactl reply 1` waits again"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.row(1).status, AskStatus::Pending);
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 540 * SEC)));
    // Kite's own wait still drops it.
    let fx = r.fire(T0 + 540 * SEC);
    assert_eq!(saved_asks(&fx)[0].status, AskStatus::Dropped);
    assert_eq!(replies(&fx).len(), 1, "{fx:?}");
}

/// A wait under a minute is said in seconds (M1).
#[test]
fn a_short_wait_is_said_in_seconds() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 30);
    let fx = r.fire(T0 + 30 * SEC);
    assert_eq!(
        the_error(&fx),
        "Harbor stayed busy for 30s; ask #1 was dropped"
    );
}

/// While the store refuses writes, an ask is refused: nothing is saved and nothing waits.
#[test]
fn an_ask_is_refused_while_writes_are() {
    let mut r = Rig::new(snapshot());
    r.reload_refusing(T0 - SEC, Vec::new(), Some(T0 + 600 * SEC));
    let fx = r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    assert_eq!(
        the_error(&fx),
        "moa can't save right now (the disk is full); nothing was changed"
    );
    assert!(!persisted(&fx), "{fx:?}");
    assert_eq!(set_at(&fx), None);
    // Nothing waits: a fire at its wait replies nothing.
    let fx = r.fire(T0 + 540 * SEC);
    assert!(replies(&fx).is_empty(), "{fx:?}");
}

/// A drop the store refuses leaves the ask pending, and its wait hears no reply yet.
#[test]
fn a_refused_drop_says_no_reply_yet() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    let row = r.row(1);
    r.reload_refusing(T0 + SEC, vec![row], Some(T0 + 600 * SEC));
    let fx = r.fire(T0 + 540 * SEC);
    assert!(!persisted(&fx), "{fx:?}");
    match &replies(&fx)[..] {
        [(ConnId(1), _, Ok(done))] => assert_eq!(
            done.text,
            "no reply yet from Harbor; `moactl reply 1` waits again"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.row(1).status, AskStatus::Pending);
}

// ---------- delivery, receipt and answer ----------

const NO_RECEIPT: &str = "no receipt (a dialog or copy mode had the pane)";

fn pane(p: &str) -> PaneId {
    p.parse().unwrap()
}

/// [`snapshot`], Harbor idle and in tab `%5`.
fn harbor_idle() -> Snapshot {
    let mut snap = snapshot();
    let harbor = snap
        .sessions
        .iter_mut()
        .find(|s| s.id == sid(HARBOR))
        .unwrap();
    harbor.activity = Activity::Idle;
    let b = snap
        .pane_bindings
        .iter_mut()
        .find(|b| b.session_id == sid(HARBOR))
        .unwrap();
    b.pane_id = "%5".into();
    snap
}

/// The asks each of `fx`'s commits saves, one list per commit.
fn ask_commits(fx: &[Effect]) -> Vec<Vec<Ask>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(
                c.iter()
                    .filter_map(|c| match c {
                        Change::Ask(a) => Some(a.clone()),
                        _ => None,
                    })
                    .collect::<Vec<Ask>>(),
            ),
            _ => None,
        })
        .filter(|asks| !asks.is_empty())
        .collect()
}

/// The commit in `fx` that saves session `id`, all of it.
fn session_commit(fx: &[Effect], id: &str) -> Vec<Change> {
    fx.iter()
        .find_map(|e| match e {
            Effect::Persist(c)
                if c.iter()
                    .any(|c| matches!(c, Change::Session(s) if s.id == sid(id))) =>
            {
                Some(c.clone())
            }
            _ => None,
        })
        .expect("a commit saves the session")
}

fn statuses(r: &Rig, ids: &[i64]) -> Vec<AskStatus> {
    ids.iter().map(|id| r.row(*id).status).collect()
}

#[test]
fn a_free_peer_gets_the_ask_typed() {
    let mut r = Rig::linked(harbor_idle());
    let fx = r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let commits = ask_commits(&fx);
    assert_eq!(commits.len(), 2, "the ask, then its claim: {commits:?}");
    let claimed = &commits[1];
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, AskStatus::Sent);
    assert_eq!(claimed[0].claimed_at, Some(UtcMs(T0)));
    let text = framed(1, "Kite", "Is the domain renewed?");
    assert_eq!(
        typed(&fx),
        [
            Cmd::set_buffer("moa-ask-1", &text),
            Cmd::paste_buffer("moa-ask-1", pane("%5")),
        ]
    );
    // The typing goes after the claim is saved.
    let saved_at = fx
        .iter()
        .rposition(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let typed_at = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { token: None, .. }))
        .unwrap();
    assert!(saved_at < typed_at, "{fx:?}");
    assert!(!launches(&fx), "{fx:?}");
    // Its Enter is the next deadline.
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 500)));
    let fx = r.fire(T0 + 499);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    let fx = r.fire(T0 + 500);
    assert_eq!(typed(&fx), [Cmd::enter(pane("%5"))]);
    assert!(!persisted(&fx) && !launches(&fx), "{fx:?}");
    // Then its receipt's.
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 20 * SEC)));
    // Only once.
    let fx = r.fire(T0 + 600);
    assert!(typed(&fx).is_empty(), "{fx:?}");
}

#[test]
fn an_ask_waits_for_the_link() {
    let mut r = Rig::new(harbor_idle());
    let fx = r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Pending);
    let fx = r.link_up(T0 + SEC);
    assert_eq!(r.row(1).status, AskStatus::Sent);
    assert_eq!(typed(&fx).len(), 2, "{fx:?}");
}

#[test]
fn a_busy_peer_gets_it_on_its_stop() {
    let mut r = Rig::linked(snapshot());
    let fx = r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Pending);
    let fx = r.stopped(T0 + 5 * SEC, HARBOR, "The tide tables are checked.");
    assert_eq!(r.row(1).status, AskStatus::Sent);
    assert_eq!(r.row(1).claimed_at, Some(UtcMs(T0 + 5 * SEC)));
    assert_eq!(
        typed(&fx),
        [
            Cmd::set_buffer("moa-ask-1", &framed(1, "Kite", "Is the domain renewed?")),
            Cmd::paste_buffer("moa-ask-1", pane("%2")),
        ]
    );
}

#[test]
fn one_ask_at_a_time() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    let fx = r.ask(2, from(BEACON), "Harbor", "Which branch?", 540);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    assert_eq!(statuses(&r, &[1, 2]), [AskStatus::Sent, AskStatus::Pending]);
    r.fire(T0 + 500);
    r.started(
        T0 + SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    assert_eq!(
        statuses(&r, &[1, 2]),
        [AskStatus::Received, AskStatus::Pending]
    );
    // Its answer frees Harbor: the next ask is typed in the same input.
    let fx = r.stopped(T0 + 9 * SEC, HARBOR, "Yes, until 2028.");
    assert_eq!(
        statuses(&r, &[1, 2]),
        [AskStatus::Answered, AskStatus::Sent]
    );
    match &replies(&fx)[..] {
        [(ConnId(1), _, Ok(done))] => assert_eq!(done.text, "Yes, until 2028."),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        typed(&fx),
        [
            Cmd::set_buffer("moa-ask-2", &framed(2, "Beacon", "Which branch?")),
            Cmd::paste_buffer("moa-ask-2", pane("%5")),
        ]
    );
}

#[test]
fn only_its_own_marker_is_a_receipt() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(2, from(KITE), "Beacon", "Which branch?", 540);
    assert_eq!(statuses(&r, &[1, 2]), [AskStatus::Sent, AskStatus::Sent]);
    r.fire(T0 + 500);
    let not_receipts = [
        // Another ask's marker, quoted: an ask to Beacon.
        framed(2, "Kite", "Which branch?"),
        // An ask that doesn't exist.
        framed(9, "Kite", "Is the domain renewed?"),
        // Almost a marker.
        "[moa · message from Kite · ask #1 Is the domain renewed?".to_string(),
        "[moa · message from Kite · ask #x1] Is the domain renewed?".to_string(),
        // The user's own prompt.
        "Look at the tide tables again".to_string(),
    ];
    let mut at = T0 + SEC;
    for prompt in not_receipts {
        let fx = r.started(at, HARBOR, &prompt);
        assert_eq!(
            statuses(&r, &[1, 2]),
            [AskStatus::Sent, AskStatus::Sent],
            "{prompt}"
        );
        assert!(saved_asks(&fx).is_empty(), "{prompt}: {fx:?}");
        // Its turn's end answers nothing.
        let fx = r.stopped(at + 100, HARBOR, "Done.");
        assert!(saved_asks(&fx).is_empty(), "{prompt}: {fx:?}");
        assert!(replies(&fx).is_empty(), "{prompt}: {fx:?}");
        at += SEC;
    }
    // A wrapped paste, the marker not first: a receipt.
    let wrapped = format!(
        "Pasted from the other tab:\n> {}",
        framed(1, "Kite", "Is the domain renewed?")
    );
    r.started(at, HARBOR, &wrapped);
    assert_eq!(
        statuses(&r, &[1, 2]),
        [AskStatus::Received, AskStatus::Sent]
    );
}

#[test]
fn the_answer_is_the_full_message() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    r.started(
        T0 + SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    let huge: String = (0..20_000).map(|i| format!("line {i:05}\n")).collect();
    assert!(huge.len() >= 200_000);
    let fx = r.stopped(T0 + 9 * SEC, HARBOR, &huge);
    match &replies(&fx)[..] {
        [(ConnId(1), _, Ok(done))] => {
            assert_eq!(done.text, huge);
            assert_eq!(done.data["status"], "answered");
            assert_eq!(done.data["from"], "Kite");
            assert_eq!(done.data["to"], "Harbor");
        }
        other => panic!("{other:?}"),
    }
    let row = r.row(1);
    assert_eq!(row.status, AskStatus::Answered);
    assert_eq!(row.reply.as_deref(), Some(huge.as_str()));
    assert_eq!(row.answered_at, Some(UtcMs(T0 + 9 * SEC)));
    // The answer is saved with the hook's own commit; its history row stays cut.
    let commit = session_commit(&fx, HARBOR);
    assert!(
        commit
            .iter()
            .any(|c| matches!(c, Change::Ask(a) if a.status == AskStatus::Answered)),
        "{commit:?}"
    );
    let event = commit
        .iter()
        .find_map(|c| match c {
            Change::Event(e) => e.text.clone(),
            _ => None,
        })
        .unwrap();
    assert!(event.chars().count() < 3_000, "{}", event.len());
    // An empty answer says so.
    r.ask(2, from(BEACON), "Harbor", "Which branch?", 540);
    r.fire(T0 + 10 * SEC);
    r.started(T0 + 11 * SEC, HARBOR, &framed(2, "Beacon", "Which branch?"));
    let fx = r.stopped(T0 + 12 * SEC, HARBOR, "");
    match &replies(&fx)[..] {
        [(ConnId(2), _, Ok(done))] => assert_eq!(done.text, "(no text)"),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.row(2).reply.as_deref(), Some("(no text)"));
}

#[test]
fn no_receipt_fails_at_20_s() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    let fx = r.fire(T0 + 20 * SEC - 1);
    assert!(!persisted(&fx) && replies(&fx).is_empty(), "{fx:?}");
    let fx = r.fire(T0 + 20 * SEC);
    let saved = saved_asks(&fx);
    assert_eq!(saved.len(), 1, "{fx:?}");
    assert_eq!(saved[0].status, AskStatus::Failed);
    assert_eq!(saved[0].reply.as_deref(), Some(NO_RECEIPT));
    assert_eq!(
        replies(&fx),
        [(
            ConnId(1),
            ReqId(1),
            Err(OpError(format!("ask #1 failed: {NO_RECEIPT}")))
        )]
    );
    // Nothing else waits.
    assert!(cancelled(&fx), "{fx:?}");
}

#[test]
fn a_late_receipt_revives_it() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    r.fire(T0 + 20 * SEC);
    assert_eq!(r.row(1).status, AskStatus::Failed);
    // The dialog closes, and the ask goes in.
    let fx = r.started(
        T0 + 25 * SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    let row = r.row(1);
    assert_eq!(row.status, AskStatus::Received, "{fx:?}");
    assert_eq!(row.reply, None);
    assert_eq!(row.answered_at, None);
    assert_eq!(row.claimed_at, Some(UtcMs(T0)));
    assert!(!r.st.is_free(sid(HARBOR)));
    // Kite waits again, and hears the answer.
    let fx = r.op(
        1,
        from(KITE),
        Op::Reply {
            id: 1,
            timeout_s: 540,
        },
        T0 + 26 * SEC,
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = r.stopped(T0 + 30 * SEC, HARBOR, "Yes.");
    match &replies(&fx)[..] {
        [(ConnId(1), _, Ok(done))] => assert_eq!(done.text, "Yes."),
        other => panic!("{other:?}"),
    }
    // Another failure isn't revived.
    let mut snap = harbor_idle();
    let mut stale = ask_row(1, to(KITE), "Kite", to(HARBOR), "Is it?", T0 - MIN);
    stale.status = AskStatus::Failed;
    stale.reply = Some("stale".into());
    stale.claimed_at = Some(UtcMs(T0 - MIN));
    snap.asks = vec![stale];
    snap.next_ids.ask = 2;
    let mut r = Rig::linked(snap);
    r.started(T0 + SEC, HARBOR, &framed(1, "Kite", "Is it?"));
    assert_eq!(r.row(1).status, AskStatus::Failed);
}

/// A late receipt never makes two agents wait on each other: once Harbor asks Kite, Kite's
/// failed ask to Harbor stays failed.
#[test]
fn a_late_receipt_never_deadlocks() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    r.fire(T0 + 20 * SEC);
    let fx = r.op(
        2,
        from(HARBOR),
        Op::Ask {
            to: "Kite".into(),
            text: "Which branch?".into(),
            timeout_s: 540,
        },
        T0 + 21 * SEC,
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(2).status, AskStatus::Sent);
    r.started(
        T0 + 25 * SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    assert_eq!(r.row(1).status, AskStatus::Failed);
}

#[test]
fn stale_asks_fail_before_a_new_claim() {
    let stale_after = 2 * ASK_WAIT_DEFAULT_MS;
    let mut snap = harbor_idle();
    let mut old = ask_row(1, to(KITE), "Kite", to(HARBOR), "Is it?", T0 - 30 * MIN);
    old.status = AskStatus::Received;
    old.claimed_at = Some(UtcMs(T0 - stale_after + MIN));
    let waiting = ask_row(
        2,
        to(BEACON),
        "Beacon",
        to(HARBOR),
        "Which branch?",
        T0 - MIN,
    );
    snap.asks = vec![old, waiting];
    snap.next_ids.ask = 3;
    // Not stale yet: Harbor isn't free, and the timer waits for it to be.
    let (mut r, fx) = Rig::started_at(snap, T0);
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + MIN)));
    let fx = r.link_up(T0);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        statuses(&r, &[1, 2]),
        [AskStatus::Received, AskStatus::Pending]
    );
    // Stale: it fails in the claim's own commit.
    let fx = r.fire(T0 + MIN);
    let commits = ask_commits(&fx);
    assert_eq!(commits.len(), 1, "{commits:?}");
    let by_id: Vec<(i64, AskStatus)> = commits[0].iter().map(|a| (a.id.0, a.status)).collect();
    assert_eq!(by_id, [(1, AskStatus::Failed), (2, AskStatus::Sent)]);
    assert_eq!(r.row(1).reply.as_deref(), Some("stale"));
    assert_eq!(typed(&fx).len(), 2, "{fx:?}");
}

/// Harbor (in `%5`) has Kite's ask in flight, Beacon's waiting, and its own to moa.
fn harbor_in_asks() -> Rig {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(2, from(BEACON), "Harbor", "Which branch?", 540);
    r.ask(3, from(HARBOR), "moa", "Which account am I on?", 540);
    assert_eq!(
        statuses(&r, &[1, 2, 3]),
        [AskStatus::Sent, AskStatus::Pending, AskStatus::Pending]
    );
    r
}

fn assert_ended_for_harbor(r: &Rig, fx: &[Effect]) {
    assert_eq!(
        statuses(r, &[1, 2, 3]),
        [AskStatus::Failed, AskStatus::Dropped, AskStatus::Pending]
    );
    assert_eq!(r.row(1).reply.as_deref(), Some("Harbor ended"));
    assert_eq!(r.row(2).reply.as_deref(), Some("Harbor isn't running"));
    let mut heard = replies(fx);
    heard.sort_by_key(|(c, _, _)| *c);
    assert_eq!(
        heard,
        [
            (
                ConnId(1),
                ReqId(1),
                Err(OpError("ask #1 failed: Harbor ended".into()))
            ),
            (
                ConnId(2),
                ReqId(2),
                Err(OpError("Harbor isn't running; ask #2 was dropped".into()))
            ),
        ]
    );
    // In the commit that ends Harbor.
    let commit = session_commit(fx, HARBOR);
    let ended: Vec<i64> = commit
        .iter()
        .filter_map(|c| match c {
            Change::Ask(a) => Some(a.id.0),
            _ => None,
        })
        .collect();
    assert_eq!(ended, [1, 2]);
}

#[test]
fn a_dead_peer_ends_its_asks() {
    // It quits.
    let mut r = harbor_in_asks();
    let fx = r.hook(
        T0 + 5 * SEC,
        HARBOR,
        HookKind::SessionEnded {
            reason: "prompt_input_exit".into(),
        },
    );
    assert_ended_for_harbor(&r, &fx);
    // Its tab goes: nothing more is typed into it.
    let mut r = harbor_in_asks();
    let exited = TmuxIn::Notification(Notification::Message("moa:hook pane-exited %5".into()));
    let fx = handle_checked(&mut r.st, Input::Tmux(exited), common::asks::now(T0 + 100));
    assert_ended_for_harbor(&r, &fx);
    let fx = r.fire(T0 + 500);
    assert!(typed(&fx).is_empty(), "{fx:?}");
}

#[test]
fn an_answering_stop_queues_no_report() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    r.started(
        T0 + SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    let fx = r.stopped(T0 + 9 * SEC, HARBOR, "Yes, until 2028.");
    let reports = |fx: &[Effect]| {
        common::changes(fx)
            .into_iter()
            .filter(|c| matches!(c, Change::Report(_)))
            .count()
    };
    assert_eq!(r.row(1).status, AskStatus::Answered);
    assert_eq!(reports(&fx), 0, "{fx:?}");
    // Its next turn, the user's, reports as any does.
    r.started(T0 + 20 * SEC, HARBOR, "Look at the tide tables again");
    let fx = r.stopped(T0 + 30 * SEC, HARBOR, "Checked.");
    assert_eq!(reports(&fx), 1, "{fx:?}");
}

// ---------- a received ask ends with its turn ----------

/// Harbor (in `%5`) has received Kite's ask #1, typed at T0, its turn started at T0 + 1 s.
fn harbor_answering() -> Rig {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    r.started(
        T0 + SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    assert_eq!(r.row(1).status, AskStatus::Received);
    r
}

fn the_failure(fx: &[Effect]) -> String {
    match &replies(fx)[..] {
        [(ConnId(1), _, Err(e))] => e.0.clone(),
        other => panic!("not Kite's failure: {other:?}"),
    }
}

fn reports(fx: &[Effect]) -> usize {
    common::changes(fx)
        .into_iter()
        .filter(|c| matches!(c, Change::Report(_)))
        .count()
}

#[test]
fn a_failed_turn_fails_its_ask() {
    let mut r = harbor_answering();
    let fx = r.hook(
        T0 + 5 * SEC,
        HARBOR,
        HookKind::TurnFailed {
            error: "rate limited".into(),
        },
    );
    assert_eq!(r.row(1).status, AskStatus::Failed);
    assert_eq!(the_failure(&fx), "ask #1 failed: Harbor's turn failed");
    // In the hook's own commit.
    assert!(
        session_commit(&fx, HARBOR)
            .iter()
            .any(|c| matches!(c, Change::Ask(a) if a.status == AskStatus::Failed))
    );
    // The user's next turn answers nothing, and reports as any does.
    r.started(T0 + 20 * SEC, HARBOR, "Look at the tide tables again");
    let fx = r.stopped(T0 + 30 * SEC, HARBOR, "Checked.");
    assert!(saved_asks(&fx).is_empty(), "{fx:?}");
    assert_eq!(reports(&fx), 1, "{fx:?}");
    assert_eq!(r.row(1).reply.as_deref(), Some("Harbor's turn failed"));
}

#[test]
fn a_new_prompt_fails_an_unanswered_ask() {
    let mut r = harbor_answering();
    // A prompt that still holds its marker keeps it.
    let fx = r.started(
        T0 + 2 * SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    assert!(saved_asks(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Received);
    // Interrupted, then a prompt of the user's: it moved on.
    let fx = r.started(T0 + 5 * SEC, HARBOR, "Look at the tide tables again");
    assert_eq!(r.row(1).status, AskStatus::Failed);
    assert_eq!(
        the_failure(&fx),
        "ask #1 failed: Harbor moved on before answering"
    );
    let fx = r.stopped(T0 + 30 * SEC, HARBOR, "Checked.");
    assert!(saved_asks(&fx).is_empty(), "{fx:?}");
    assert_eq!(reports(&fx), 1, "{fx:?}");
}

#[test]
/// Every new conversation in the tab (`/clear`, a resume, a new agent, a fork) leaves the ask
/// unanswered.
fn a_new_conversation_fails_an_unanswered_ask() {
    let sources = [
        StartSource::Clear,
        StartSource::Resume,
        StartSource::Startup,
        StartSource::Fork,
    ];
    for source in sources {
        let mut r = harbor_answering();
        let fx = r.hook(T0 + 5 * SEC, HARBOR, HookKind::SessionStarted { source });
        assert_eq!(r.row(1).status, AskStatus::Failed, "{source:?}");
        assert_eq!(
            the_failure(&fx),
            "ask #1 failed: Harbor moved on before answering",
            "{source:?}"
        );
    }
}

/// A compaction can come mid-turn, and the turn carries on: the ask stays received, and the
/// turn's Stop answers it.
#[test]
fn a_compaction_mid_ask_keeps_it() {
    let mut r = harbor_answering();
    let source = StartSource::Compact;
    let fx = r.hook(T0 + 5 * SEC, HARBOR, HookKind::SessionStarted { source });
    assert!(saved_asks(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Received);
    let fx = r.stopped(T0 + 9 * SEC, HARBOR, "Yes, until 2028.");
    assert_eq!(r.row(1).status, AskStatus::Answered);
    match &replies(&fx)[..] {
        [(ConnId(1), _, Ok(done))] => assert_eq!(done.text, "Yes, until 2028."),
        other => panic!("{other:?}"),
    }
}

/// A late receipt takes its ask only before the ask would be stale (M1).
#[test]
fn a_late_receipt_only_before_stale() {
    for (claimed, revived) in [
        (T0 - STALE_AFTER_MS + 1, true),
        (T0 - STALE_AFTER_MS, false),
    ] {
        let mut snap = harbor_idle();
        let mut failed = ask_row(1, to(KITE), "Kite", to(HARBOR), "Is it?", claimed - SEC);
        failed.status = AskStatus::Failed;
        failed.reply = Some(NO_RECEIPT.into());
        failed.claimed_at = Some(UtcMs(claimed));
        failed.answered_at = Some(UtcMs(claimed + 20 * SEC));
        snap.asks = vec![failed];
        snap.next_ids.ask = 2;
        let mut r = Rig::linked(snap);
        r.started(T0, HARBOR, &framed(1, "Kite", "Is it?"));
        let want = if revived {
            AskStatus::Received
        } else {
            AskStatus::Failed
        };
        assert_eq!(r.row(1).status, want, "claimed at {claimed}");
    }
}

// ---------- a peer that isn't running ----------

/// Harbor (in `%5`, reconciled) has received Kite's ask #1, and Beacon's #2 waits.
fn harbor_answering_reconciled() -> Rig {
    let mut r = Rig::reconciled(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(2, from(BEACON), "Harbor", "Which branch?", 540);
    r.fire(T0 + 500);
    r.started(
        T0 + SEC,
        HARBOR,
        &framed(1, "Kite", "Is the domain renewed?"),
    );
    assert_eq!(
        statuses(&r, &[1, 2]),
        [AskStatus::Received, AskStatus::Pending]
    );
    r
}

#[test]
fn a_closed_archived_or_slept_peer_ends_its_asks() {
    let ops = [
        ("close", Op::SessionClose { q: "Harbor".into() }),
        (
            "archive",
            Op::SessionArchive {
                qs: vec!["Harbor".into()],
            },
        ),
        (
            "sleep",
            Op::SessionSleep {
                qs: vec!["Harbor".into()],
                idle_ms: None,
                dry_run: false,
            },
        ),
    ];
    for (what, op) in ops {
        let mut r = harbor_answering_reconciled();
        let fx = r.op(9, CallerInfo::default(), op, T0 + 2 * SEC);
        assert_eq!(
            statuses(&r, &[1, 2]),
            [AskStatus::Failed, AskStatus::Dropped],
            "{what}: {fx:?}"
        );
        let mut heard: Vec<(ConnId, Result<Done, OpError>)> = replies(&fx)
            .into_iter()
            .filter(|(c, _, _)| c.0 != 9)
            .map(|(c, _, r)| (c, r))
            .collect();
        heard.sort_by_key(|(c, _)| *c);
        assert_eq!(
            heard,
            [
                (
                    ConnId(1),
                    Err(OpError("ask #1 failed: Harbor ended".into()))
                ),
                (
                    ConnId(2),
                    Err(OpError("Harbor isn't running; ask #2 was dropped".into()))
                ),
            ],
            "{what}"
        );
    }
}

/// Before the link is up, a peer that isn't running keeps its asks: the daemon's start
/// hasn't looked at the tabs yet.
#[test]
fn asks_to_a_gone_peer_end_once_the_link_is_up() {
    let mut snap = harbor_idle();
    snap.pane_bindings.retain(|b| b.session_id != sid(HARBOR));
    let mut sent = ask_row(1, to(KITE), "Kite", to(HARBOR), "Is it?", T0 - MIN);
    sent.status = AskStatus::Received;
    sent.claimed_at = Some(UtcMs(T0 - MIN));
    snap.asks = vec![sent];
    snap.next_ids.ask = 2;
    let mut r = Rig::new(snap);
    assert_eq!(r.row(1).status, AskStatus::Received);
    let fx = r.link_up(T0 + SEC);
    assert_eq!(r.row(1).status, AskStatus::Failed, "{fx:?}");
    assert_eq!(r.row(1).reply.as_deref(), Some("Harbor ended"));
}

// ---------- code review: held back, and deadlines while writes are refused ----------

/// A pane that took no receipt holds its peer back: the asks behind wait, untyped, until moa
/// hears from the peer again.
#[test]
fn no_receipt_holds_the_peer_back() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(2, from(BEACON), "Harbor", "Which branch?", 540);
    r.ask(3, moa(), "Harbor", "Which account?", 540);
    r.fire(T0 + 500);
    let fx = r.fire(T0 + 20 * SEC);
    assert_eq!(
        statuses(&r, &[1, 2, 3]),
        [AskStatus::Failed, AskStatus::Pending, AskStatus::Pending]
    );
    assert!(typed(&fx).is_empty(), "{fx:?}");
    assert!(!r.st.is_free(sid(HARBOR)));
    let fx = r.fire(T0 + 60 * SEC);
    assert!(typed(&fx).is_empty(), "{fx:?}");
    // Harbor is heard from: the next one goes.
    let fx = r.stopped(T0 + 90 * SEC, HARBOR, "Done with the dialog.");
    assert_eq!(
        statuses(&r, &[1, 2, 3]),
        [AskStatus::Failed, AskStatus::Sent, AskStatus::Pending]
    );
    assert_eq!(typed(&fx).len(), 2, "{fx:?}");
}

/// A receipt's 20 s that passes while writes are refused fails the ask once writes come back,
/// with no other input: the timer is armed for then.
#[test]
fn a_deadline_passed_while_writes_were_refused_still_acts() {
    let mut r = Rig::linked(harbor_idle());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.fire(T0 + 500);
    let row = r.row(1);
    assert_eq!(row.status, AskStatus::Sent);
    r.reload_refusing(T0 + SEC, vec![row], Some(T0 + 30 * SEC));
    let fx = r.fire(T0 + 20 * SEC);
    assert!(!persisted(&fx), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Sent);
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 30 * SEC)));
    let fx = r.fire(T0 + 30 * SEC);
    assert_eq!(r.row(1).status, AskStatus::Failed, "{fx:?}");
    assert_eq!(r.row(1).reply.as_deref(), Some(NO_RECEIPT));
}

/// So does a pending ask's own wait.
#[test]
fn a_drop_passed_while_writes_were_refused_still_acts() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 60);
    let row = r.row(1);
    r.reload_refusing(T0 + SEC, vec![row], Some(T0 + 90 * SEC));
    let fx = r.fire(T0 + 60 * SEC);
    assert_eq!(r.row(1).status, AskStatus::Pending);
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 90 * SEC)));
    r.fire(T0 + 90 * SEC);
    assert_eq!(r.row(1).status, AskStatus::Dropped);
}
