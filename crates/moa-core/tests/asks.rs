//! Asks between agents (slice 7, A1–A4, A11, A13): `ask` saves a `pending` row and waits for
//! the answer, `reply` waits again, `peers` lists who can be asked, and `asks` lists the
//! caller's asks. Delivery into a pane is not here: an ask stays `pending` unless a test sets
//! its row by hand.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::asks::{ASK_TEXT_MAX, ASK_WAIT_DEFAULT_MS};
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, TimerKey, handle_checked};
use moa_core::model::{
    Activity, AgentKind, Ask, AskId, AskStatus, AskTarget, Conversation, ConversationReason,
    Lifecycle, Location, PaneBinding, Session, SessionId,
};
use moa_core::op::{ConnId, Done, NoParams, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Deps, Snapshot, State};
use moa_core::{Now, UtcMs};
use serde_json::Value;

use common::timers;

const HOME: &str = "/tmp/moa-asks-test/home";

const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
"#;

/// The moment every test starts at.
const T0: i64 = 1_790_000_000_000;
const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;

const KITE: &str = "00000000-0000-4000-8000-0000000000a1";
const HARBOR: &str = "00000000-0000-4000-8000-0000000000a2";
const BEACON: &str = "00000000-0000-4000-8000-0000000000a3";
/// Ended, and bound to no tab.
const LANTERN: &str = "00000000-0000-4000-8000-0000000000a4";

/// Each session's conversation: its handle is the first 8 characters.
fn conversation_of(id: &str) -> &'static str {
    match id {
        KITE => "7c41d0e2-0000-4000-8000-0000000000a1",
        HARBOR => "3f2a91c4-0000-4000-8000-0000000000a2",
        BEACON => "b9e0c3d5-0000-4000-8000-0000000000a3",
        LANTERN => "5d8e2f10-0000-4000-8000-0000000000a4",
        _ => "e0e0e0e0-0000-4000-8000-0000000000ff",
    }
}

fn sid(id: &str) -> SessionId {
    id.parse().unwrap()
}

fn now(ms: i64) -> Now {
    Now::at(UtcMs(ms), Duration::ZERO, TimeZone::UTC)
}

/// A live session titled `title` in `~/private/<folder>`, `activity` since a minute before
/// T0.
fn session(id: &str, title: &str, folder: &str, activity: Activity) -> Session {
    let cwd = format!("{HOME}/private/{folder}");
    Session {
        id: sid(id),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.clone()),
        start_cwd: Some(cwd),
        project_id: None,
        title: Some(title.into()),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity,
        activity_at: Some(UtcMs(T0 - MIN)),
        activity_why: Some("Looking at the tide tables".into()),
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(T0 - MIN)),
    }
}

fn conversation(id: &str) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conversation_of(id).into(),
        session_id: sid(id),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

fn binding(id: &str, pane: &str) -> PaneBinding {
    PaneBinding {
        session_id: sid(id),
        server_pid: 10,
        server_start: 1000,
        socket: "moa".into(),
        pane_id: pane.into(),
        pane_pid: 100,
        bound_at: UtcMs(1),
        tracked: true,
    }
}

/// Ask `id` from `from` to `to`, saved at `at`, `status`.
fn ask_row(id: i64, from: AskTarget, from_name: &str, to: AskTarget, text: &str, at: i64) -> Ask {
    Ask {
        id: AskId(id),
        at: UtcMs(at),
        from_session: Some(from),
        from_name: Some(from_name.into()),
        to_session: Some(to),
        text: text.into(),
        status: AskStatus::Pending,
        reply: None,
        claimed_at: None,
        answered_at: None,
    }
}

fn to(id: &str) -> AskTarget {
    AskTarget::Session(sid(id))
}

/// Kite (idle, `%1`), Harbor (working, `%2`) and Beacon (idle, `%3`), live; Lantern ended
/// with no tab.
fn snapshot() -> Snapshot {
    let mut lantern = session(LANTERN, "Lantern", "lantern", Activity::Idle);
    lantern.lifecycle = Lifecycle::Ended;
    let ids = [KITE, HARBOR, BEACON, LANTERN];
    Snapshot {
        sessions: vec![
            session(KITE, "Kite", "kite", Activity::Idle),
            session(HARBOR, "Harbor", "harbor", Activity::Working),
            session(BEACON, "Beacon", "beacon", Activity::Idle),
            lantern,
        ],
        conversations: ids.iter().map(|id| conversation(id)).collect(),
        pane_bindings: vec![
            binding(KITE, "%1"),
            binding(HARBOR, "%2"),
            binding(BEACON, "%3"),
        ],
        ..Snapshot::default()
    }
}

struct Rig {
    st: State,
    req: u64,
}

impl Rig {
    fn new(snapshot: Snapshot) -> Rig {
        let home = PathBuf::from(HOME);
        let config = Config::parse(TOML, &home).unwrap();
        let deps = Deps {
            home,
            ..Deps::default()
        };
        let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
        handle_checked(&mut st, Input::Startup(Box::new(snapshot)), now(T0));
        Rig { st, req: 0 }
    }

    /// `op` from `caller` on connection `conn`, at `ms`.
    fn op(&mut self, conn: u64, caller: CallerInfo, op: Op, ms: i64) -> Vec<Effect> {
        self.req += 1;
        let input = Input::Client {
            conn: ConnId(conn),
            req: ReqId(self.req),
            caller,
            op,
        };
        handle_checked(&mut self.st, input, now(ms))
    }

    /// `caller` asks `to` `text` on connection `conn` at T0, waiting `timeout_s`.
    fn ask(
        &mut self,
        conn: u64,
        caller: CallerInfo,
        to: &str,
        text: &str,
        timeout_s: i64,
    ) -> Vec<Effect> {
        let op = Op::Ask {
            to: to.into(),
            text: text.into(),
            timeout_s,
        };
        self.op(conn, caller, op, T0)
    }

    /// The asks timer fires at `ms`.
    fn fire(&mut self, ms: i64) -> Vec<Effect> {
        handle_checked(&mut self.st, Input::Timer(TimerKey::Asks), now(ms))
    }

    /// The core reloads at `ms` with the rows it has, these asks put in by hand.
    fn reload(&mut self, ms: i64, asks: Vec<Ask>) -> Vec<Effect> {
        let mut snap = snapshot();
        snap.asks = asks;
        snap.next_ids.ask = snap.asks.iter().map(|a| a.id.0 + 1).max().unwrap_or(1);
        let input = Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        };
        handle_checked(&mut self.st, input, now(ms))
    }

    fn row(&self, id: i64) -> Ask {
        let topic = &self.st.model().topics[&moa_core::publish::Topic::Asks];
        serde_json::from_value(topic[&id.to_string()].clone()).unwrap()
    }
}

fn from(id: &str) -> CallerInfo {
    CallerInfo {
        moa_session: Some(id.into()),
        ..CallerInfo::default()
    }
}

fn moa() -> CallerInfo {
    from("moa")
}

/// A shell in a folder: no session, not moa.
fn shell() -> CallerInfo {
    CallerInfo {
        cwd: Some(PathBuf::from(format!("{HOME}/private/kite"))),
        ..CallerInfo::default()
    }
}

fn replies(fx: &[Effect]) -> Vec<(ConnId, ReqId, Result<Done, OpError>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply { conn, req, result } => Some((*conn, *req, result.clone())),
            _ => None,
        })
        .collect()
}

fn the_error(fx: &[Effect]) -> String {
    match &replies(fx)[..] {
        [(_, _, Err(e))] => e.0.clone(),
        other => panic!("not one refusal: {other:?}"),
    }
}

fn saved_asks(fx: &[Effect]) -> Vec<Ask> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Ask(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

fn persisted(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Persist(_)))
}

fn set_at(fx: &[Effect]) -> Option<UtcMs> {
    timers(fx, TimerKey::Asks)
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::SetTimer { at, .. } => Some(*at),
            _ => None,
        })
}

fn cancelled(fx: &[Effect]) -> bool {
    timers(fx, TimerKey::Asks)
        .iter()
        .any(|e| matches!(e, Effect::CancelTimer(TimerKey::Asks)))
}

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
    let mut deadlock = snapshot();
    deadlock.asks = vec![ask_row(
        9,
        to(HARBOR),
        "Harbor",
        to(KITE),
        "Which branch?",
        T0 - MIN,
    )];
    deadlock.next_ids.ask = 10;
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
            deadlock,
            from(KITE),
            "Harbor",
            "hi",
            540,
            "Harbor is waiting on your answer to ask #9; answer that first".into(),
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
fn a_sent_ask_says_no_reply_yet() {
    let mut r = Rig::new(snapshot());
    r.ask(1, from(KITE), "Harbor", "Is the domain renewed?", 540);
    // Delivered, by hand.
    let mut sent = r.row(1);
    sent.status = AskStatus::Sent;
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
fn a_gone_client_leaves_the_ask() {
    let mut r = Rig::new(snapshot());
    r.ask(7, from(KITE), "Harbor", "Is the domain renewed?", 540);
    r.ask(8, from(BEACON), "Harbor", "Which branch?", 60);
    // Connection 8 goes: 7 still waits, and the timer moves to its wait.
    let fx = handle_checked(&mut r.st, Input::ClientGone(ConnId(8)), now(T0 + SEC));
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    assert_eq!(set_at(&fx), Some(UtcMs(T0 + 540 * SEC)));
    assert_eq!(r.row(2).status, AskStatus::Pending);
    // Then 7: nothing waits.
    let fx = handle_checked(&mut r.st, Input::ClientGone(ConnId(7)), now(T0 + 2 * SEC));
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    assert!(cancelled(&fx), "{fx:?}");
    let fx = r.fire(T0 + 540 * SEC);
    assert!(replies(&fx).is_empty() && !persisted(&fx), "{fx:?}");
    assert_eq!(r.row(1).status, AskStatus::Pending);
    assert_eq!(r.row(2).status, AskStatus::Pending);
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
        T0 - 30 * MIN,
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
