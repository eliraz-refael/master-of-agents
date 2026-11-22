//! moa's conversation in the core (A6–A8): a prompt starts moa's agent, which opens today's
//! thread (reopened, or new with the system text), and the prompt goes once its row is saved.

mod common;

use common::central::{
    acps, caps, ended, feed_rows, first_turn, hear, hear_at, moa, moa_with, opened, phase, prompt,
    prompt_at, prompts_sent, published_state, saved, started, text_of, thread, thread_rows,
};
use common::{at, changes, replies, send};
use jiff::ToSpan;
use jiff::civil::Date;
use moa_core::UtcMs;
use moa_core::central::launch::spec;
use moa_core::central::prompt::system_text;
use moa_core::central::threads::{How, choose};
use moa_core::central::wire::{AcpCmd, AcpIn, Opened, ThreadHow};
use moa_core::central::{Phase, Waiter};
use moa_core::config::AccountName;
use moa_core::effect::{Effect, Input, Publish, RefuseWrites, check_order, handle_checked};
use moa_core::model::MoaThread;
use moa_core::op::{ConnId, NoParams, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Topic};
use moa_core::state::Snapshot;
use moa_testkit::brain::TypicalBrain;
use proptest::prelude::*;
use serde_json::json;

const BUSY: &str = "moa is still answering; wait for it, or cancel it";
const REOPEN_FAILED: &str = "today's earlier thread couldn't be reopened, so this is a new one";
const DAY_MS: i64 = 24 * 3_600_000;

#[test]
fn a_first_prompt_starts_the_agent_then_a_thread() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let now = b.now();

    let fx = prompt(&b, &mut st, 1, "  hello moa  ");
    let (_, want) = spec(&st).unwrap();
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Start {
            generation: 1,
            spec: want
        }]
    );
    // The reply waits for the prompt's row (F8): there is no thread to write it in yet.
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(*phase(&st), Phase::Starting);

    let fx = hear(&b, &mut st, started(1, caps(true, true, true)));
    let text = system_text(&st, &now);
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Open {
            generation: 1,
            how: ThreadHow::New {
                system_prompt: Some(text.full),
                claude_md: None,
            },
            cwd: st.deps().agent_dir.clone(),
        }]
    );
    assert_eq!(*phase(&st), Phase::Ready);

    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    let persists = fx
        .iter()
        .filter(|e| matches!(e, Effect::Persist(_)))
        .count();
    assert_eq!(persists, 1, "{fx:?}");
    assert_eq!(
        thread_rows(&fx),
        [&thread("th-1", "acct-a", now.local_day, now.wall.0)]
    );
    let rows = feed_rows(&fx);
    assert_eq!(rows.len(), 1);
    let you = rows[0];
    assert_eq!((you.kind.as_str(), you.thread_id.as_str()), ("you", "th-1"));
    assert_eq!(text_of(you), "hello moa");
    assert!(!you.automatic);
    let message_id = you.message_id.clone().unwrap();
    assert!(uuid::Uuid::parse_str(&message_id).is_ok());
    let changes_at = fx.iter().position(|e| matches!(e, Effect::Persist(_)));
    match &replies(&fx)[..] {
        [(ReqId(1), Ok(done))] => {
            assert_eq!(done.text, "moa has it");
            assert_eq!(
                done.data,
                json!({"thread": "th-1", "message_id": message_id})
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Prompt {
            generation: 1,
            thread: "th-1".into(),
            prompt: 1,
            text: "hello moa".into(),
        }]
    );
    let prompt_at = fx.iter().position(|e| matches!(e, Effect::Acp(_)));
    assert!(changes_at.unwrap() < prompt_at.unwrap());
    assert_eq!(*phase(&st), Phase::Thinking);
    let turn = st.central().turn.as_ref().unwrap();
    assert_eq!((turn.prompt, turn.message_id.as_str()), (1, &*message_id));
}

#[test]
fn the_prompt_goes_only_after_its_row() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let fx = prompt(&b, &mut st, 1, "hello");
    let start = fx.iter().find(|e| matches!(e, Effect::Acp(_))).unwrap();
    assert!(start.runs_at_once() && !start.is_heard());
    let fx = hear(&b, &mut st, started(1, caps(false, false, true)));
    let open = fx.iter().find(|e| matches!(e, Effect::Acp(_))).unwrap();
    assert!(open.runs_at_once() && !open.is_heard());

    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert!(check_order(&fx).is_ok());
    let sent = fx
        .iter()
        .find(|e| matches!(e, Effect::Acp(AcpCmd::Prompt { .. })))
        .unwrap();
    assert!(!sent.runs_at_once());
    assert!(sent.is_heard());
    // A prompt placed before its row breaks the order the executor checks.
    let mut wrong: Vec<Effect> = fx
        .iter()
        .filter(|e| !matches!(e, Effect::Acp(_)))
        .cloned()
        .collect();
    wrong.insert(0, sent.clone());
    assert!(check_order(&wrong).is_err());
}

#[test]
fn a_restart_the_same_day_resumes() {
    let b = TypicalBrain::new();
    let now = b.now();
    let earlier = now.wall.0 - 3_600_000;
    let mut st = moa_with(
        &b,
        "acct-a",
        vec![thread("th-0", "acct-a", now.local_day, earlier)],
    );
    prompt(&b, &mut st, 1, "hello");
    let fx = hear(&b, &mut st, started(1, caps(true, true, true)));
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Open {
            generation: 1,
            how: ThreadHow::Resume {
                thread: "th-0".into()
            },
            cwd: st.deps().agent_dir.clone(),
        }]
    );
    assert!(!st.central().replaying);
    let fx = hear(&b, &mut st, opened(1, "th-0", Opened::Resumed));
    assert!(thread_rows(&fx).is_empty(), "{fx:?}");
    let rows = feed_rows(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].thread_id, "th-0");
    assert_eq!(st.threads().count(), 1);
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Prompt { thread, .. }] if thread == "th-0"
    ));
}

#[test]
fn without_resume_it_loads() {
    let b = TypicalBrain::new();
    let now = b.now();
    let mut st = moa_with(
        &b,
        "acct-a",
        vec![thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000)],
    );
    prompt(&b, &mut st, 1, "hello");
    let fx = hear(&b, &mut st, started(1, caps(true, false, true)));
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open { how: ThreadHow::Load { thread }, .. }] if thread == "th-0"
    ));
    // The load's replay comes before its answer: it is checked against the feed's keys.
    assert!(st.central().replaying);
    let fx = hear(&b, &mut st, opened(1, "th-0", Opened::Loaded));
    assert!(!st.central().replaying);
    assert!(thread_rows(&fx).is_empty());
    assert_eq!(prompts_sent(&fx), ["hello"]);
}

#[test]
fn without_either_it_starts_a_new_thread() {
    let b = TypicalBrain::new();
    let now = b.now();
    let mut st = moa_with(
        &b,
        "acct-a",
        vec![thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000)],
    );
    prompt(&b, &mut st, 1, "hello");
    let fx = hear(&b, &mut st, started(1, caps(false, false, true)));
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open {
            how: ThreadHow::New {
                system_prompt: Some(_),
                claude_md: None
            },
            ..
        }]
    ));
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert_eq!(
        thread_rows(&fx),
        [&thread("th-1", "acct-a", now.local_day, now.wall.0)]
    );
}

#[test]
fn a_new_day_starts_a_thread_with_the_brief() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let today = b.now();
    let yesterday = at(UtcMs(today.wall.0 - DAY_MS));
    first_turn(&mut st, &yesterday, "th-1", false);
    hear_at(&mut st, yesterday.clone(), ended(1, 1));
    assert_eq!(*phase(&st), Phase::Ready);

    let fx = prompt_at(&mut st, today.clone(), 2, "good morning", false);
    // moa's agent still runs: only a new thread is opened.
    let full = system_text(&st, &today).full;
    assert!(full.contains("# Brief (Sun 15 Nov"), "{full}");
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Open {
            generation: 1,
            how: ThreadHow::New {
                system_prompt: Some(full),
                claude_md: None,
            },
            cwd: st.deps().agent_dir.clone(),
        }]
    );
    assert!(replies(&fx).is_empty());
    let fx = hear_at(&mut st, today.clone(), opened(1, "th-2", Opened::New));
    assert_eq!(
        thread_rows(&fx),
        [&thread("th-2", "acct-a", today.local_day, today.wall.0)]
    );
    assert_eq!(feed_rows(&fx)[0].thread_id, "th-2");
    assert_eq!(prompts_sent(&fx), ["good morning"]);
}

#[test]
fn a_new_account_starts_a_new_thread() {
    let b = TypicalBrain::new();
    let now = b.now();
    // Today's thread is acct-a's; moa now runs on acct-b.
    let mut st = moa_with(
        &b,
        "acct-b",
        vec![thread("th-a", "acct-a", now.local_day, now.wall.0 - 60_000)],
    );
    let fx = prompt(&b, &mut st, 1, "hello");
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Start { spec, .. }]
            if spec.env.contains(&("CLAUDE_CONFIG_DIR".into(),
                b.home.join(".claude-acct-b").display().to_string()))
    ));
    let fx = hear(&b, &mut st, started(1, caps(true, true, true)));
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open {
            how: ThreadHow::New { .. },
            ..
        }]
    ));
    let fx = hear(&b, &mut st, opened(1, "th-b", Opened::New));
    assert_eq!(
        thread_rows(&fx),
        [&thread("th-b", "acct-b", now.local_day, now.wall.0)]
    );
}

#[test]
fn without_system_prompt_the_brief_rides_the_first_prompt() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let now = b.now();
    prompt(&b, &mut st, 1, "hello");
    let fx = hear(&b, &mut st, started(1, caps(false, false, false)));
    let text = system_text(&st, &now);
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Open {
            generation: 1,
            how: ThreadHow::New {
                system_prompt: None,
                claude_md: Some(text.claude_md.clone()),
            },
            cwd: st.deps().agent_dir.clone(),
        }]
    );
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    let sent = prompts_sent(&fx);
    assert_eq!(sent, [format!("{}\n\n---\n\nhello", text.brief)]);
    assert!(sent[0].starts_with("# Brief ("));
    assert_eq!(text_of(feed_rows(&fx)[0]), "hello");

    hear(&b, &mut st, ended(1, 1));
    let fx = prompt(&b, &mut st, 2, "and again");
    assert!(thread_rows(&fx).is_empty());
    assert_eq!(prompts_sent(&fx), ["and again"]);
    assert_eq!(text_of(feed_rows(&fx)[0]), "and again");
}

#[test]
fn a_reopen_that_fails_starts_a_new_thread_and_says_so() {
    let b = TypicalBrain::new();
    let now = b.now();
    let mut st = moa_with(
        &b,
        "acct-a",
        vec![thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000)],
    );
    prompt(&b, &mut st, 1, "hello");
    hear(&b, &mut st, started(1, caps(true, true, true)));
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Opened {
            generation: 1,
            result: Err("no such session".into()),
        },
    );
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open {
            how: ThreadHow::New {
                system_prompt: Some(_),
                ..
            },
            ..
        }]
    ));
    assert!(replies(&fx).is_empty());
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert_eq!(thread_rows(&fx).len(), 1);
    let rows = feed_rows(&fx);
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["system", "you"]);
    assert_eq!(text_of(rows[0]), REOPEN_FAILED);
    assert!(rows[0].id < rows[1].id);
    assert!(rows.iter().all(|r| r.thread_id == "th-1"));
    assert_eq!(prompts_sent(&fx), ["hello"]);
}

#[test]
fn a_failed_new_thread_refuses_the_prompt() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt_at(&mut st, b.now(), 1, "hello", true);
    hear(&b, &mut st, started(1, caps(false, false, true)));
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Opened {
            generation: 1,
            result: Err("it broke\nat line 2".into()),
        },
    );
    let line = "moa couldn't start a thread: it broke";
    assert_eq!(
        replies(&fx),
        [(ReqId(1), Err(OpError(line.into())))],
        "{fx:?}"
    );
    assert!(
        fx.contains(&Effect::Publish(Publish::Event(CoreEvent::Error {
            message: line.into()
        })))
    );
    assert!(changes(&fx).is_empty());
    assert!(acps(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Ready);
    assert!(st.central().pending.is_none());
    // moa's agent still runs: the next prompt tries a thread again.
    let fx = prompt(&b, &mut st, 2, "again");
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open {
            how: ThreadHow::New { .. },
            ..
        }]
    ));
}

#[test]
fn a_second_prompt_while_busy_is_refused() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let busy = |fx: &[Effect], req| {
        assert_eq!(replies(fx), [(ReqId(req), Err(OpError(BUSY.into())))]);
        assert!(acps(fx).is_empty());
        assert!(changes(fx).is_empty());
    };
    prompt(&b, &mut st, 1, "first");
    // Starting.
    busy(&prompt(&b, &mut st, 2, "second"), 2);
    // Started, its thread still opening.
    hear(&b, &mut st, started(1, caps(false, false, true)));
    assert_eq!(*phase(&st), Phase::Ready);
    busy(&prompt(&b, &mut st, 3, "second"), 3);
    // Thinking.
    hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert_eq!(*phase(&st), Phase::Thinking);
    busy(&prompt(&b, &mut st, 4, "second"), 4);
    // The turn ended: the next one goes.
    hear(&b, &mut st, ended(1, 1));
    assert_eq!(prompts_sent(&prompt(&b, &mut st, 5, "second")), ["second"]);
}

#[test]
fn no_account_is_refused_before_anything_starts() {
    let b = TypicalBrain::new();
    let mut st = moa_with(&b, "", vec![]);
    let fx = prompt(&b, &mut st, 1, "hello");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(
                "moa has no account yet: set moa_account for the default brain in moa.toml".into()
            ))
        )]
    );
    assert!(acps(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Off);
    assert_eq!(st.central().generation, 0);
    assert!(st.central().pending.is_none());
}

#[test]
fn empty_and_oversized_prompts_are_refused() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    for (text, why) in [
        ("", "there's nothing to send"),
        (" \n\t ", "there's nothing to send"),
    ] {
        let fx = prompt(&b, &mut st, 1, text);
        assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(why.into())))]);
        assert!(acps(&fx).is_empty());
    }
    let over = "é".repeat(100_001);
    let fx = prompt(&b, &mut st, 2, &over);
    assert_eq!(
        replies(&fx),
        [(
            ReqId(2),
            Err(OpError("a prompt is at most 100000 characters".into()))
        )]
    );
    assert!(acps(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Off);
    // Characters are counted, not bytes; the spaces around are trimmed first.
    let most = format!("  {}  ", "é".repeat(100_000));
    let fx = prompt(&b, &mut st, 3, &most);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]));
}

#[test]
fn stale_generations_are_dropped() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt(&b, &mut st, 1, "hello");
    assert!(hear(&b, &mut st, started(2, caps(true, true, true))).is_empty());
    assert!(
        hear(
            &b,
            &mut st,
            AcpIn::Exited {
                generation: 0,
                why: "old".into()
            }
        )
        .is_empty()
    );
    assert_eq!(*phase(&st), Phase::Starting);

    // The start fails; the next prompt is generation 2, and 1's words are dropped.
    hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: "no such file".into(),
        },
    );
    prompt(&b, &mut st, 2, "again");
    assert_eq!(st.central().generation, 2);
    assert!(hear(&b, &mut st, started(1, caps(true, true, true))).is_empty());
    assert!(hear(&b, &mut st, opened(1, "th-old", Opened::New)).is_empty());
    assert_eq!(*phase(&st), Phase::Starting);
    let fx = hear(&b, &mut st, started(2, caps(true, true, true)));
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Open { generation: 2, .. }]
    ));
}

#[test]
fn a_failed_start_says_why() {
    let b = TypicalBrain::new();
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
    assert_eq!(*phase(&st), Phase::Failed(why.into()));
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(format!("moa's agent couldn't start: {why}")))
        )]
    );
    let row = published_state(&fx).unwrap();
    assert_eq!(row["phase"], "failed");
    assert_eq!(row["why"], why);
    assert!(st.central().pending.is_none());
    // A prompt starts it again.
    let fx = prompt(&b, &mut st, 2, "again");
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Start { generation: 2, .. }]
    ));
}

#[test]
fn each_phase_change_publishes_the_state_row() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let fx = prompt(&b, &mut st, 1, "hello");
    assert_eq!(published_state(&fx).unwrap()["phase"], "starting");
    let fx = hear(&b, &mut st, started(1, caps(false, false, true)));
    assert_eq!(published_state(&fx).unwrap()["phase"], "ready");
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    let row = published_state(&fx).unwrap();
    assert_eq!(
        (&row["phase"], &row["thread"]),
        (&json!("thinking"), &json!("th-1"))
    );
    let fx = hear(&b, &mut st, ended(1, 1));
    assert_eq!(published_state(&fx).unwrap()["phase"], "ready");
    assert_eq!(st.model().topics[&Topic::Feed]["state"]["phase"], "ready");
}

#[test]
fn cancel_goes_only_to_a_turn() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let fx = send(&b, &mut st, 1, Op::ConsoleCancel(NoParams));
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError("moa isn't answering anything".into()))
        )]
    );
    first_turn(&mut st, &b.now(), "th-1", false);
    let fx = send(&b, &mut st, 2, Op::ConsoleCancel(NoParams));
    assert_eq!(
        acps(&fx),
        [&AcpCmd::Cancel {
            generation: 1,
            thread: "th-1".into()
        }]
    );
    assert!(matches!(&replies(&fx)[..], [(ReqId(2), Ok(d))] if d.text == "cancelling moa's turn"));
}

#[test]
fn a_reload_resaves_an_unsaved_thread_and_forgets_an_unsent_prompt() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let now = b.now();
    let before = b.snapshot();
    prompt_at(&mut st, now.clone(), 1, "hello", true);
    // No system prompt: the brief rides the thread's first prompt.
    hear(&b, &mut st, started(1, caps(false, false, false)));
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert_eq!(thread_rows(&fx).len(), 1);
    assert!(prompts_sent(&fx)[0].starts_with("# Brief ("));
    assert_eq!(*phase(&st), Phase::Thinking);

    // The commit carrying the thread and the prompt's row failed, and the held prompt with
    // it: the store has neither row. A reload saves nothing (the daemon drops its writes).
    let fx = handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(before),
            refuse_writes: None,
        },
        now.clone(),
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(
                "moa couldn't save your prompt; nothing was sent".into()
            ))
        )]
    );
    assert!(acps(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Ready);
    assert!(st.central().turn.is_none());
    assert!(st.thread("th-1").is_none());

    // The thread is still open, unsaved: the next prompt saves it with its own row, in one
    // commit, and is its first, carrying the brief.
    let fx = prompt(&b, &mut st, 2, "again");
    let persists: Vec<_> = fx
        .iter()
        .filter(|e| matches!(e, Effect::Persist(_)))
        .collect();
    assert_eq!(persists.len(), 1, "{fx:?}");
    assert_eq!(
        thread_rows(&fx),
        [&thread("th-1", "acct-a", now.local_day, now.wall.0)]
    );
    let rows = feed_rows(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind.as_str(), text_of(rows[0])),
        ("you", json!("again"))
    );
    let sent = prompts_sent(&fx);
    assert!(sent[0].starts_with("# Brief (") && sent[0].ends_with("\n\n---\n\nagain"));

    // Once saved, the thread's later prompts carry only themselves.
    hear(&b, &mut st, ended(1, 2));
    let fx = prompt(&b, &mut st, 3, "third");
    assert!(thread_rows(&fx).is_empty());
    assert_eq!(prompts_sent(&fx), ["third"]);
}

#[test]
fn a_refused_first_prompt_keeps_the_reopen_line() {
    let b = TypicalBrain::new();
    let now = b.now();
    let threads = vec![thread("th-0", "acct-a", now.local_day, now.wall.0 - 60_000)];
    let mut st = moa_with(&b, "acct-a", threads.clone());
    prompt_at(&mut st, now.clone(), 1, "hello", false);
    hear(&b, &mut st, started(1, caps(true, true, true)));
    // The store refuses writes for a minute.
    let until = UtcMs(now.wall.0 + 60_000);
    handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(Snapshot {
                threads,
                ..b.snapshot()
            }),
            refuse_writes: Some(RefuseWrites {
                until,
                reason: "the disk is full".into(),
            }),
        },
        now.clone(),
    );
    hear(
        &b,
        &mut st,
        AcpIn::Opened {
            generation: 1,
            result: Err("no such session".into()),
        },
    );
    let fx = hear(&b, &mut st, opened(1, "th-1", Opened::New));
    assert!(changes(&fx).is_empty());
    assert!(acps(&fx).is_empty());
    assert!(matches!(&replies(&fx)[..], [(ReqId(1), Err(_))]));
    assert_eq!(*phase(&st), Phase::Ready);

    // Writes are back: the next prompt is the new thread's first, with the line before it.
    let later = at(UtcMs(until.0 + 1));
    let fx = prompt_at(&mut st, later.clone(), 2, "again", false);
    assert_eq!(thread_rows(&fx).len(), 1);
    let rows = feed_rows(&fx);
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["system", "you"]);
    assert_eq!(text_of(rows[0]), REOPEN_FAILED);
    assert_eq!(prompts_sent(&fx), ["again"]);
}

#[test]
fn a_restart_after_an_exit_mid_turn_answers_the_old_waiter_once() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt_at(&mut st, b.now(), 1, "hello", true);
    hear(&b, &mut st, started(1, caps(false, false, true)));
    hear(&b, &mut st, opened(1, "th-1", Opened::New));
    // The waiter hears when the agent goes (T8 wording ruling).
    let fx = hear(
        &b,
        &mut st,
        AcpIn::Exited {
            generation: 1,
            why: "it crashed".into(),
        },
    );
    let stopped = "moa's agent stopped: it crashed; your next prompt starts it again";
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(stopped.into())))]);
    // Its turn's end never came: the next prompt starts a new run, and drops the old turn
    // without answering anyone twice.
    let fx = prompt(&b, &mut st, 2, "again");
    assert!(matches!(
        acps(&fx)[..],
        [AcpCmd::Start { generation: 2, .. }]
    ));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(st.central().turn.is_none());
}

#[test]
fn a_reload_keeps_a_saved_prompt() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt_at(&mut st, b.now(), 1, "hello", true);
    hear(&b, &mut st, started(1, caps(false, false, true)));
    hear(&b, &mut st, opened(1, "th-1", Opened::New));
    let snapshot = saved(&b, &st);
    let fx = handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: None,
        },
        b.now(),
    );
    assert!(changes(&fx).is_empty());
    assert!(replies(&fx).is_empty());
    assert_eq!(*phase(&st), Phase::Thinking);
    let turn = st.central().turn.as_ref().unwrap();
    assert_eq!(
        turn.waiter,
        Some(Waiter {
            conn: ConnId(1),
            req: ReqId(1),
            wait: true
        })
    );
}

/// The typical day, moved `offset` days.
fn day(offset: i64) -> Date {
    shared().now().local_day + offset.days()
}

/// A thread row for the property: account `a` (0 or 1), `offset` days from the typical day.
fn row_strategy() -> impl Strategy<Value = (usize, i64, i64)> {
    (0usize..2, -1i64..=1, 0i64..4)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the rows, the open thread, the account, the day and the agent's caps: a
    /// thread is used only when it is this account's and today's, an earlier one is
    /// reopened only when it is today's newest of this account, and only as the caps allow.
    #[test]
    fn choose_is_exhaustive(
        rows in prop::collection::vec(row_strategy(), 0..6),
        open in any::<bool>(),
        account in 0usize..2,
        today in -1i64..=1,
        load in any::<bool>(),
        resume in any::<bool>(),
    ) {
        let accounts = ["acct-a", "acct-b"];
        let b = shared();
        let base = b.now();
        let threads: Vec<MoaThread> = rows
            .iter()
            .enumerate()
            .map(|(i, (a, d, s))| thread(&format!("th-{i}"), accounts[*a], day(*d), base.wall.0 + s))
            .collect();
        let mut st = moa_with(b, "acct-a", threads);
        if open {
            // moa runs on acct-a, with a new thread opened on the typical day.
            first_turn(&mut st, &base, "th-open", false);
        }
        let account = AccountName::parse(accounts[account]).unwrap();
        let today = day(today);
        let caps = caps(load, resume, false);
        let how = choose(&st, &account, today, caps);

        let open_row = st.central().thread.as_ref().map(|o| o.row.clone());
        let usable = open_row
            .as_ref()
            .filter(|r| r.account == account.as_str() && r.day == today);
        let newest = st
            .threads()
            .filter(|t| t.account == account.as_str() && t.day == today)
            .max_by_key(|t| (t.started_at, t.thread_id.clone()));
        match (&how, usable, newest) {
            (How::Use(id), Some(r), _) => prop_assert_eq!(id, &r.thread_id),
            (How::Use(_), None, _) => prop_assert!(false, "used a thread that isn't usable"),
            (_, Some(_), _) => prop_assert!(false, "a usable thread wasn't used: {:?}", how),
            (How::Resume(id), None, Some(t)) => {
                prop_assert!(caps.resume);
                prop_assert_eq!(id, &t.thread_id);
            }
            (How::Load(id), None, Some(t)) => {
                prop_assert!(caps.load && !caps.resume);
                prop_assert_eq!(id, &t.thread_id);
            }
            (How::New, None, Some(_)) => prop_assert!(!caps.load && !caps.resume),
            (How::New, None, None) => {}
            (How::Resume(_) | How::Load(_), None, None) => {
                prop_assert!(false, "reopened a thread that isn't today's of this account")
            }
        }
    }
}

/// One typical brain for every case of the property: only its snapshot is read.
fn shared() -> &'static TypicalBrain {
    static BRAIN: std::sync::OnceLock<TypicalBrain> = std::sync::OnceLock::new();
    BRAIN.get_or_init(TypicalBrain::new)
}
