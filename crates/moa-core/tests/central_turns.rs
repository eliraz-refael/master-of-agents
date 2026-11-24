//! moa's turns in the core (A5–A7): everything moa says lands in the feed as it comes, the
//! turn's end answers whoever waits, and permission requests are refused until moa can ask.

mod common;

use common::central::{
    acps, caps, ended, feed_rows, first_turn, hear, moa, moa_from, moa_with, opened, phase,
    prompt_at, published_state, saved, started, text_of, thread,
};
use common::{changes, replies, send, send_on};
use moa_core::UtcMs;
use moa_core::central::Phase;
use moa_core::central::wire::{
    AcpCmd, AcpIn, Opened, PermKind, PermOption, PlanEntry, StopReason, ThreadHow, Update,
};
use moa_core::effect::{Effect, Input, LogLevel, RefuseWrites, handle_checked};
use moa_core::model::FeedEntry;
use moa_core::op::{ConnId, Done, NoParams, Op, OpError, ReqId};
use moa_core::state::{Snapshot, State};
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

const REFUSED: &str = "and was refused: moa can't ask you yet";

/// moa with its first turn under way in thread `th-1`; request 1 waits for its end when
/// `wait`.
fn turn(b: &TypicalBrain, wait: bool) -> State {
    let mut st = moa(b);
    first_turn(&mut st, &b.now(), "th-1", wait);
    st
}

/// moa's agent says `update` in thread `th-1`.
fn say(b: &TypicalBrain, st: &mut State, update: Update) -> Vec<Effect> {
    say_in(b, st, "th-1", update)
}

fn say_in(b: &TypicalBrain, st: &mut State, thread: &str, update: Update) -> Vec<Effect> {
    let input = AcpIn::Update {
        generation: st.central().generation,
        thread: thread.into(),
        update,
    };
    hear(b, st, input)
}

fn message(id: Option<&str>, text: &str) -> Update {
    Update::Message {
        id: id.map(Into::into),
        text: text.into(),
    }
}

fn thought(id: Option<&str>, text: &str) -> Update {
    Update::Thought {
        id: id.map(Into::into),
        text: text.into(),
    }
}

fn tool(id: &str, status: &str, input: Option<&str>) -> Update {
    Update::Tool {
        id: id.into(),
        title: format!("Read {id}"),
        kind: "read".into(),
        status: status.into(),
        input: input.map(Into::into),
    }
}

fn tool_update(id: &str, title: Option<&str>, status: Option<&str>) -> Update {
    Update::ToolUpdate {
        id: id.into(),
        title: title.map(Into::into),
        status: status.map(Into::into),
        input: None,
    }
}

fn plan(entries: &[(&str, &str)]) -> Update {
    Update::Plan {
        entries: entries
            .iter()
            .map(|(content, status)| PlanEntry {
                content: (*content).into(),
                status: (*status).into(),
                priority: "medium".into(),
            })
            .collect(),
    }
}

/// Turn 1 of the live run ends with `result`.
fn end(b: &TypicalBrain, st: &mut State, result: Result<StopReason, String>) -> Vec<Effect> {
    let input = AcpIn::TurnEnded {
        generation: st.central().generation,
        prompt: 1,
        result,
    };
    hear(b, st, input)
}

fn option(id: &str, kind: PermKind) -> PermOption {
    PermOption {
        id: id.into(),
        kind,
    }
}

fn permission(req: u64, title: &str, options: Vec<PermOption>) -> AcpIn {
    AcpIn::Permission {
        generation: 1,
        req,
        thread: "th-1".into(),
        title: title.into(),
        options,
    }
}

/// The rows of `thread` the state holds, but the prompts' own, oldest first.
fn rows(st: &State, thread: &str) -> Vec<FeedEntry> {
    st.feed_tail()
        .filter(|r| r.thread_id == thread && r.kind != "you")
        .cloned()
        .collect()
}

fn body(row: &FeedEntry) -> Value {
    serde_json::from_str(&row.body).unwrap()
}

/// `(kind, text)` of each row, for the text kinds.
fn texts(rows: &[FeedEntry]) -> Vec<(String, Value)> {
    rows.iter().map(|r| (r.kind.clone(), text_of(r))).collect()
}

fn persists(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::Persist(_)))
        .count()
}

/// The log lines at `level` in `fx`.
fn logs(fx: &[Effect], level: LogLevel) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) if l.level == level => Some(l.message.as_str()),
            _ => None,
        })
        .collect()
}

/// The you row's `message_id` of the turn under way.
fn you_id(st: &State) -> String {
    st.central().turn.as_ref().unwrap().message_id.clone()
}

/// The reply to request 1, done.
fn done(fx: &[Effect]) -> Done {
    match &replies(fx)[..] {
        [(ReqId(1), Ok(done))] => done.clone(),
        other => panic!("{other:?}"),
    }
}

/// From now on, until a minute from now, the store refuses writes; everything saved so far
/// is kept.
fn refuse_writes(b: &TypicalBrain, st: &mut State) {
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

#[test]
fn chunks_with_one_message_id_make_one_row() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    let fx = say(&b, &mut st, message(Some("m-1"), "Hel"));
    let first = feed_rows(&fx)[0].clone();
    let fx = say(&b, &mut st, message(Some("m-1"), "lo"));
    // The row is written again, whole: an upsert of the same id.
    assert_eq!(persists(&fx), 1);
    let again = feed_rows(&fx)[0];
    assert_eq!(again.id, first.id);
    assert_eq!(text_of(again), "Hello");

    say(&b, &mut st, message(Some("m-2"), "Bye"));
    let rows = rows(&st, "th-1");
    assert_eq!(
        texts(&rows),
        [("moa".into(), json!("Hello")), ("moa".into(), json!("Bye"))]
    );
    let keys: Vec<_> = rows.iter().map(|r| r.message_id.as_deref()).collect();
    assert_eq!(keys, [Some("m-1"), Some("m-2")]);
}

#[test]
fn chunks_without_ids_merge_until_a_tool_breaks_them() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    say(&b, &mut st, message(None, "a"));
    say(&b, &mut st, message(None, "b"));
    say(&b, &mut st, thought(None, "hm"));
    say(&b, &mut st, thought(None, "m"));
    say(&b, &mut st, message(None, "c"));
    say(&b, &mut st, tool("t1", "pending", None));
    say(&b, &mut st, message(None, "d"));
    let rows = rows(&st, "th-1");
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["moa", "thought", "moa", "tool", "moa"]);
    let text: Vec<Value> = rows
        .iter()
        .filter(|r| r.kind != "tool")
        .map(text_of)
        .collect();
    assert_eq!(text, [json!("ab"), json!("hmm"), json!("c"), json!("d")]);
    assert!(
        rows.iter()
            .filter(|r| r.kind != "tool")
            .all(|r| r.message_id.is_none())
    );
}

#[test]
fn a_tool_call_row_follows_its_updates() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    say(
        &b,
        &mut st,
        tool("t1", "pending", Some(r#"{"path":"/r/a.md"}"#)),
    );
    let row = &rows(&st, "th-1")[0];
    assert_eq!(row.message_id.as_deref(), Some("tool:t1"));
    assert_eq!(row.state.as_deref(), Some("pending"));
    assert_eq!(
        body(row),
        json!({"title": "Read t1", "kind": "read", "input": r#"{"path":"/r/a.md"}"#})
    );

    say(&b, &mut st, tool_update("t1", None, Some("in_progress")));
    assert_eq!(rows(&st, "th-1")[0].state.as_deref(), Some("in_progress"));
    let fx = say(
        &b,
        &mut st,
        tool_update("t1", Some("Read a.md"), Some("completed")),
    );
    assert_eq!(feed_rows(&fx)[0].id, row.id);
    let rows = rows(&st, "th-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state.as_deref(), Some("completed"));
    assert_eq!(body(&rows[0])["title"], "Read a.md");
    assert_eq!(body(&rows[0])["input"], r#"{"path":"/r/a.md"}"#);

    // A tool without input says so.
    say(&b, &mut st, tool("t2", "pending", None));
    assert_eq!(body(&rows_of_kind(&st, "tool")[1])["input"], Value::Null);
}

/// Claude's adapter starts a call with `{}` and gives the input in a later update: the row
/// takes it, cut as a call's is.
#[test]
fn a_tool_update_fills_in_the_input() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    say(&b, &mut st, tool("t1", "pending", Some("{}")));
    let input = json!({"command": "moactl ls"}).to_string();
    say(
        &b,
        &mut st,
        Update::ToolUpdate {
            id: "t1".into(),
            title: Some("moactl ls".into()),
            status: None,
            input: Some(input.clone()),
        },
    );
    let row = &rows_of_kind(&st, "tool")[0];
    assert_eq!(body(row)["title"], "moactl ls");
    assert_eq!(body(row)["input"], input);

    let long = json!({"command": "x".repeat(3_000)}).to_string();
    say(
        &b,
        &mut st,
        Update::ToolUpdate {
            id: "t1".into(),
            title: None,
            status: None,
            input: Some(long),
        },
    );
    let cut = body(&rows_of_kind(&st, "tool")[0])["input"]
        .as_str()
        .unwrap()
        .chars()
        .count();
    assert!(cut <= 2_000, "{cut}");
}

fn rows_of_kind(st: &State, kind: &str) -> Vec<FeedEntry> {
    rows(st, "th-1")
        .into_iter()
        .filter(|r| r.kind == kind)
        .collect()
}

#[test]
fn a_plan_is_one_row_updated_in_place() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    say(&b, &mut st, plan(&[("Read the notes", "pending")]));
    say(&b, &mut st, message(None, "on it"));
    say(
        &b,
        &mut st,
        plan(&[("Read the notes", "completed"), ("Answer", "in_progress")]),
    );
    let plans = rows_of_kind(&st, "plan");
    assert_eq!(plans.len(), 1);
    assert!(plans[0].message_id.is_none());
    assert_eq!(
        body(&plans[0]),
        json!({"entries": [
            {"content": "Read the notes", "status": "completed", "priority": "medium"},
            {"content": "Answer", "status": "in_progress", "priority": "medium"},
        ]})
    );
}

#[test]
fn user_echo_and_bookkeeping_are_not_stored() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    for update in [Update::UserEcho, Update::Other("usage_update")] {
        let fx = say(&b, &mut st, update);
        assert!(changes(&fx).is_empty(), "{fx:?}");
        let debug = logs(&fx, LogLevel::Debug);
        assert_eq!(debug.len(), 1, "{fx:?}");
    }
    assert!(rows(&st, "th-1").is_empty());
    // The log names the kind, never its content.
    let fx = say(&b, &mut st, Update::Other("usage_update"));
    assert!(logs(&fx, LogLevel::Debug)[0].contains("usage_update"));
}

#[test]
fn the_turn_end_answers_the_waiter_with_moas_words() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    let message_id = you_id(&st);
    say(&b, &mut st, thought(None, "thinking it over"));
    say(&b, &mut st, message(Some("m-1"), "Hello"));
    say(&b, &mut st, tool("t1", "completed", None));
    say(&b, &mut st, message(Some("m-2"), "World"));
    let fx = end(&b, &mut st, Ok(StopReason::EndTurn));
    let done = done(&fx);
    assert_eq!(done.text, "Hello\n\nWorld");
    assert_eq!(
        done.data,
        json!({"thread": "th-1", "message_id": message_id, "stop": "end_turn"})
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Ready);
    assert!(st.central().turn.is_none());
}

#[test]
fn an_early_stop_says_why() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    say(&b, &mut st, message(None, "Half an ans"));
    let fx = end(&b, &mut st, Ok(StopReason::MaxTokens));
    let line = "moa's turn ended early: max_tokens";
    let rows = feed_rows(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind.as_str(), text_of(rows[0])),
        ("system", json!(line))
    );
    let done = done(&fx);
    assert_eq!(done.text, line);
    assert_eq!(done.data["stop"], "max_tokens");
    assert_eq!(*phase(&st), Phase::Ready);
}

#[test]
fn cancel_sends_cancel_and_the_end_is_cancelled() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    say(&b, &mut st, tool("t1", "pending", None));
    say(&b, &mut st, tool("t2", "in_progress", None));
    say(&b, &mut st, tool("t3", "completed", None));
    say(&b, &mut st, tool("t4", "failed", None));
    let fx = send_on(&b, &mut st, 2, 2, Op::ConsoleCancel(NoParams));
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Cancel {
            generation: 1,
            thread: "th-1".into()
        }]
    );
    assert!(matches!(&replies(&fx)[..], [(ReqId(2), Ok(d))] if d.text == "cancelling moa's turn"));

    let fx = end(&b, &mut st, Ok(StopReason::Cancelled));
    let states: Vec<Option<String>> = rows_of_kind(&st, "tool")
        .into_iter()
        .map(|r| r.state)
        .collect();
    assert_eq!(
        states,
        [
            Some("cancelled".into()),
            Some("cancelled".into()),
            Some("completed".into()),
            Some("failed".into())
        ]
    );
    let line = "moa's turn ended early: cancelled";
    assert_eq!(text_of(rows_of_kind(&st, "system").last().unwrap()), line);
    // One commit: the open tools and the line.
    assert_eq!(persists(&fx), 1);
    assert_eq!(done(&fx).text, line);
    assert_eq!(*phase(&st), Phase::Ready);
}

#[test]
fn cancel_with_no_turn_says_so() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let nothing = Err(OpError("moa isn't answering anything".into()));
    let fx = send(&b, &mut st, 1, Op::ConsoleCancel(NoParams));
    assert_eq!(replies(&fx), [(ReqId(1), nothing.clone())]);
    assert!(acps(&fx).is_empty());
    // Nor once the turn is over.
    first_turn(&mut st, &b.now(), "th-1", false);
    end(&b, &mut st, Ok(StopReason::EndTurn));
    let fx = send(&b, &mut st, 2, Op::ConsoleCancel(NoParams));
    assert_eq!(replies(&fx), [(ReqId(2), nothing)]);
    assert!(acps(&fx).is_empty());
}

#[test]
fn a_permission_is_refused_by_kind() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    let options = vec![
        option("allow-1", PermKind::AllowOnce),
        option("never", PermKind::RejectAlways),
        option("not-now", PermKind::RejectOnce),
    ];
    let fx = hear(&b, &mut st, permission(7, "Run git push", options));
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Permission {
            generation: 1,
            req: 7,
            choice: Some("not-now".into())
        }]
    );
    let line = format!("moa wanted to Run git push, {REFUSED}");
    let rows = feed_rows(&fx);
    assert_eq!(
        (rows[0].kind.as_str(), text_of(rows[0])),
        ("system", json!(line))
    );

    // A second one in the turn, its options in another order and no title: by kind again.
    let options = vec![
        option("not-now-2", PermKind::RejectOnce),
        option("allow-2", PermKind::AllowAlways),
    ];
    let fx = hear(&b, &mut st, permission(8, "", options));
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Permission {
            generation: 1,
            req: 8,
            choice: Some("not-now-2".into())
        }]
    );
    let systems = rows_of_kind(&st, "system");
    assert_eq!(systems.len(), 2);
    assert_eq!(
        text_of(&systems[1]),
        format!("moa wanted to a tool call, {REFUSED}")
    );
    assert_eq!(*phase(&st), Phase::Thinking);
}

#[test]
fn no_reject_once_cancels_the_turn() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    let options = vec![
        option("allow-1", PermKind::AllowOnce),
        option("allow-all", PermKind::AllowAlways),
        option("never", PermKind::RejectAlways),
    ];
    let fx = hear(&b, &mut st, permission(3, "Edit notes.md", options));
    assert_eq!(
        acps(&fx),
        [
            &AcpCmd::Cancel {
                generation: 1,
                thread: "th-1".into()
            },
            &AcpCmd::Permission {
                generation: 1,
                req: 3,
                choice: None
            }
        ]
    );
    let rows = feed_rows(&fx);
    assert_eq!(
        text_of(rows[0]),
        format!("moa wanted to Edit notes.md, {REFUSED}; its turn was cancelled")
    );

    // The cancelled end adds no second line, and the waiter still hears why.
    let fx = end(&b, &mut st, Ok(StopReason::Cancelled));
    assert_eq!(rows_of_kind(&st, "system").len(), 1);
    assert_eq!(done(&fx).text, "moa's turn ended early: cancelled");
}

#[test]
fn an_exit_mid_turn_says_so() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    say(&b, &mut st, message(None, "Half"));
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: "it crashed".into(),
        },
    );
    let line = "moa's agent stopped: it crashed; your next prompt starts it again";
    let written = feed_rows(&fx);
    assert_eq!(written.len(), 1);
    assert_eq!(
        (written[0].kind.as_str(), text_of(written[0])),
        ("error", json!(line))
    );
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(line.into())))]);
    assert_eq!(*phase(&st), Phase::Off);
    assert_eq!(published_state(&fx).unwrap()["phase"], "off");

    // The run's last words may come after its exit: they are still written, and its end
    // finds the waiter answered.
    let fx = say(&b, &mut st, message(None, " an answer"));
    assert_eq!(persists(&fx), 1);
    let fx = end(&b, &mut st, Err("the connection closed".into()));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(st.central().turn.is_none());
    assert_eq!(*phase(&st), Phase::Off);
    assert_eq!(
        texts(&rows(&st, "th-1")),
        [
            ("moa".into(), json!("Half")),
            ("error".into(), json!(line)),
            ("moa".into(), json!(" an answer")),
        ]
    );

    // The next prompt starts moa again, and nobody is answered twice.
    let fx = prompt_at(&mut st, b.now(), 2, "again", false);
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Start { generation: 2, .. }]
    ));
    assert!(replies(&fx).is_empty());
}

#[test]
fn an_exit_mid_turn_cancels_its_running_tools() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    say(&b, &mut st, tool("t1", "pending", None));
    say(&b, &mut st, tool("t2", "in_progress", None));
    say(&b, &mut st, tool("t3", "completed", None));
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: "it crashed".into(),
        },
    );
    // One commit: the running tools, then the line.
    assert_eq!(persists(&fx), 1);
    let kinds: Vec<&str> = feed_rows(&fx).iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["tool", "tool", "error"]);
    let states: Vec<Option<String>> = rows_of_kind(&st, "tool")
        .into_iter()
        .map(|r| r.state)
        .collect();
    assert_eq!(
        states,
        [
            Some("cancelled".into()),
            Some("cancelled".into()),
            Some("completed".into())
        ]
    );
}

#[test]
fn a_failed_turn_says_why() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    let fx = end(&b, &mut st, Err("Internal error: overloaded".into()));
    let line = "moa's turn failed: Internal error: overloaded";
    let rows = feed_rows(&fx);
    assert_eq!(
        (rows[0].kind.as_str(), text_of(rows[0])),
        ("error", json!(line))
    );
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(line.into())))]);
    assert_eq!(*phase(&st), Phase::Ready);
}

#[test]
fn a_failed_turn_cancels_its_running_tools() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    say(&b, &mut st, tool("t1", "in_progress", None));
    say(&b, &mut st, tool("t2", "completed", None));
    end(&b, &mut st, Err("Internal error: overloaded".into()));
    let states: Vec<Option<String>> = rows_of_kind(&st, "tool")
        .into_iter()
        .map(|r| r.state)
        .collect();
    assert_eq!(states, [Some("cancelled".into()), Some("completed".into())]);
}

#[test]
fn a_tool_started_after_an_exit_is_cancelled_by_the_late_end() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: "it crashed".into(),
        },
    );
    // The run's last words, overtaken by its exit.
    say(&b, &mut st, tool("t1", "pending", None));
    end(&b, &mut st, Ok(StopReason::Cancelled));
    let tools = rows_of_kind(&st, "tool");
    assert_eq!(tools[0].state.as_deref(), Some("cancelled"));
}

#[test]
fn a_client_gone_mid_turn_leaves_the_turn() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(1)), b.now());
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Thinking);
    assert_eq!(st.central().turn.as_ref().unwrap().waiter, None);

    let fx = say(&b, &mut st, message(None, "still here"));
    assert_eq!(persists(&fx), 1);
    let fx = end(&b, &mut st, Ok(StopReason::EndTurn));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Ready);
    assert_eq!(
        texts(&rows(&st, "th-1")),
        [("moa".into(), json!("still here"))]
    );
}

/// A row of thread `th-0`, saved earlier.
fn earlier(id: i64, kind: &str, key: Option<&str>, body: Value) -> FeedEntry {
    FeedEntry {
        id,
        thread_id: "th-0".into(),
        at: UtcMs(0),
        kind: kind.into(),
        message_id: key.map(Into::into),
        automatic: false,
        body: body.to_string(),
        state: None,
    }
}

#[test]
fn a_replay_writes_only_the_loaded_thread() {
    let b = TypicalBrain::new();
    let now = b.now();
    let mut st = moa_with(
        &b,
        "acct-a",
        vec![
            thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000),
            thread("th-1", "acct-a", now.local_day, now.wall.0 - 120_000),
        ],
    );
    prompt_at(&mut st, now.clone(), 1, "again", false);
    hear(&b, &mut st, started(1, caps(true, false, true)));
    assert_eq!(st.central().replaying.as_deref(), Some("th-0"));
    let other = say_in(&b, &mut st, "th-1", message(Some("m-1"), "another thread"));
    assert!(feed_rows(&other).is_empty(), "{other:?}");
    let loaded = say_in(&b, &mut st, "th-0", message(Some("m-1"), "this thread"));
    assert_eq!(feed_rows(&loaded).len(), 1);
}

#[test]
fn load_never_duplicates_rows() {
    let b = TypicalBrain::new();
    let now = b.now();
    let tail = vec![
        earlier(1, "you", Some("u-1"), json!({"text": "hi"})),
        earlier(2, "moa", Some("m-1"), json!({"text": "old answer"})),
        earlier(
            3,
            "tool",
            Some("tool:t1"),
            json!({"title": "Read t1", "kind": "read", "input": null}),
        ),
    ];
    let keys = ["u-1", "m-1", "tool:t1"].map(|k| ("th-0".to_string(), k.to_string()));
    let mut snapshot = Snapshot {
        threads: vec![thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000)],
        feed_tail: tail,
        feed_keys: keys.into(),
        ..b.snapshot()
    };
    snapshot.next_ids.feed = 4;
    let mut st = moa_from(&b, "acct-a", snapshot);

    prompt_at(&mut st, now.clone(), 1, "again", false);
    // Only load: today's thread is replayed.
    let fx = hear(&b, &mut st, started(1, caps(true, false, true)));
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open {
            how: ThreadHow::Load { .. },
            ..
        }]
    ));
    assert!(st.central().replaying.is_some());

    let replay = [
        Update::UserEcho,
        message(Some("m-1"), "old answer"),
        tool("t1", "pending", None),
        tool_update("t1", None, Some("completed")),
        message(None, "an id-less piece"),
        thought(None, "an id-less thought"),
        plan(&[("a step", "completed")]),
        message(Some("m-2"), "a new "),
        message(Some("m-2"), "answer"),
        tool("t2", "pending", None),
        tool_update("t2", None, Some("completed")),
    ];
    let mut written = Vec::new();
    for update in replay {
        let fx = say_in(&b, &mut st, "th-0", update);
        written.extend(feed_rows(&fx).into_iter().map(|r| r.id));
    }
    written.dedup();
    assert_eq!(written, [4, 5], "only the new keys are written");

    let fx = hear(&b, &mut st, opened(1, "th-0", Opened::Loaded));
    assert!(st.central().replaying.is_none());
    assert_eq!(feed_rows(&fx).len(), 1, "only the prompt's row");

    let all: Vec<FeedEntry> = st
        .feed_tail()
        .filter(|r| r.thread_id == "th-0")
        .cloned()
        .collect();
    let keys: Vec<Option<&str>> = all.iter().map(|r| r.message_id.as_deref()).collect();
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(keys.len(), unique.len(), "{keys:?}");
    let m2 = all
        .iter()
        .find(|r| r.message_id.as_deref() == Some("m-2"))
        .unwrap();
    assert_eq!(text_of(m2), "a new answer");
    let t2 = all
        .iter()
        .find(|r| r.message_id.as_deref() == Some("tool:t2"))
        .unwrap();
    assert_eq!(t2.state.as_deref(), Some("completed"));
}

#[test]
fn refused_writes_still_end_the_turn() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    refuse_writes(&b, &mut st);
    for update in [
        message(Some("m-1"), "Hello"),
        message(Some("m-1"), " there"),
        tool("t1", "pending", None),
    ] {
        let fx = say(&b, &mut st, update);
        assert!(changes(&fx).is_empty(), "{fx:?}");
    }
    assert_eq!(st.central().unsaved, 3);

    let fx = end(&b, &mut st, Ok(StopReason::EndTurn));
    assert_eq!(persists(&fx), 0);
    assert_eq!(done(&fx).text, "Hello there");
    assert_eq!(
        logs(&fx, LogLevel::Warn),
        ["3 of moa's updates couldn't be saved"]
    );
    assert_eq!(st.central().unsaved, 0);
    assert_eq!(*phase(&st), Phase::Ready);
}

#[test]
fn a_turns_rows_after_a_reload_are_the_stores() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    say(&b, &mut st, message(Some("m-1"), "Hel"));
    say(&b, &mut st, message(None, "x"));
    let before = saved(&b, &st);
    // These commits fail: the store has neither, and the daemon reloads.
    say(&b, &mut st, message(Some("m-1"), "lo"));
    say(&b, &mut st, message(None, "y"));
    say(&b, &mut st, tool("t1", "pending", None));
    handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(before),
            refuse_writes: None,
        },
        b.now(),
    );
    // The lost updates are stepped again: nothing doubles, and the tool's row is new again.
    say(&b, &mut st, message(Some("m-1"), "lo"));
    say(&b, &mut st, message(None, "y"));
    say(&b, &mut st, tool("t1", "pending", None));
    let rows = rows(&st, "th-1");
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["moa", "moa", "tool"]);
    assert_eq!(text_of(&rows[0]), "Hello");
    assert_eq!(text_of(&rows[1]), "xy");
    let fx = end(&b, &mut st, Ok(StopReason::EndTurn));
    assert_eq!(done(&fx).text, "Hello\n\nxy");
}

#[test]
fn odd_text_round_trips() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, true);
    let long: String = "שלום 🦀 中文 ".chars().cycle().take(100_000).collect();
    let odd = [
        "emoji 🦀🔥".to_string(),
        "中文字符と日本語".into(),
        "a family: 👩‍👩‍👧‍👦, a flag: 🏳️‍🌈".into(),
        "RTL: שלום עולם, مرحبا".into(),
        r#"quotes "double" 'single' `back` \ backslash"#.into(),
        "lines\nand\r\ncarriage\n\n".into(),
        "replaced: \u{FFFD}\u{FFFD}".into(),
        long,
    ];
    for (i, text) in odd.iter().enumerate() {
        let id = format!("m-{i}");
        let fx = say(&b, &mut st, message(Some(&id), text));
        assert_eq!(text_of(feed_rows(&fx)[0]), json!(text), "{i}");
    }
    let rows = rows(&st, "th-1");
    let got: Vec<Value> = rows.iter().map(text_of).collect();
    let want: Vec<Value> = odd.iter().map(|t| json!(t)).collect();
    assert_eq!(got, want);
    let fx = end(&b, &mut st, Ok(StopReason::EndTurn));
    assert_eq!(done(&fx).text, odd.join("\n\n"));
}

#[test]
fn a_huge_tool_input_is_cut() {
    let b = TypicalBrain::new();
    let mut st = turn(&b, false);
    let input = json!({ "content": "é".repeat(1_000_000) }).to_string();
    say(&b, &mut st, tool("t1", "pending", Some(&input)));
    let row = &rows(&st, "th-1")[0];
    let cut = body(row)["input"].as_str().unwrap().to_string();
    assert_eq!(cut.chars().count(), 2_000);
    assert!(input.starts_with(&cut));
}

#[test]
fn the_state_row_follows_the_turn() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let shown = |st: &State| st.model().topics[&moa_core::publish::Topic::Feed]["state"].clone();
    assert_eq!(shown(&st)["phase"], "off");
    let mut seen = Vec::new();
    let mut step = |fx: Vec<Effect>, st: &State| {
        seen.push(published_state(&fx).unwrap()["phase"].clone());
        assert_eq!(shown(st)["phase"], *seen.last().unwrap());
    };
    let fx = prompt_at(&mut st, b.now(), 1, "hello", false);
    step(fx, &st);
    let fx = hear(&b, &mut st, started(1, caps(false, false, true)));
    step(fx, &st);
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    step(fx, &st);
    let fx = hear(&b, &mut st, ended(1, 1));
    step(fx, &st);
    assert_eq!(seen, ["starting", "ready", "thinking", "ready"]);

    // A start that fails leaves moa failed, saying why.
    let mut st = moa(&b);
    prompt_at(&mut st, b.now(), 1, "hello", false);
    let why = "moa's agent didn't start within 30 s";
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: why.into(),
        },
    );
    let row = published_state(&fx).unwrap();
    assert_eq!(
        (&row["phase"], &row["why"]),
        (&json!("failed"), &json!(why))
    );
    assert_eq!(shown(&st), row);
}
