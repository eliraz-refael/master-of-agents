//! The account card (K8, C24): when moa's agent fails because of its account (a login,
//! billing, a spent usage limit), a `card` says so and offers `/account B` for each other
//! account moa can run on. Nothing else changes: only your `/account` switches it.
//!
//! Where the card goes (F18), as each test says: in the open thread; else in the newest saved
//! thread of any account; with no thread at all, its text goes out as an error, never a row.

mod common;

use common::central::{
    acps, caps, ended, feed_rows, first_turn, hear, moa, moa_at, moa_with, opened, phase, prompt,
    prompts_sent, refuse_writes, started, thread,
};
use common::{changes, replies, started_at};
use moa_core::central::Phase;
use moa_core::central::account::{ACCOUNT_TROUBLE, trouble};
use moa_core::central::launch::{self, ACCOUNT_KEY};
use moa_core::central::wire::{AcpCmd, AcpIn, AdapterSpec, Opened};
use moa_core::change::Change;
use moa_core::config::AccountName;
use moa_core::effect::{Effect, Input, Publish, handle_checked};
use moa_core::model::FeedEntry;
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::publish::CoreEvent;
use moa_core::state::{Deps, State};
use moa_testkit::brain::TypicalBrain;
use serde_json::Value;

const AUTH: &str = "Authentication required";

/// The card for an auth failure on `acct-a`, with `acct-b` to go on with.
const AUTH_CARD: &str = "moa's account acct-a failed: Authentication required.\n\n\
                         Nothing was switched. To go on with another account:\n\n\
                         `/account acct-b`";

/// A runnable account more, and one moa can't run on (no launcher): only the first is offered.
const MORE: &str = "[accounts.acct-c]\nkind = \"claude\"\nstore = \"~/.claude-acct-c\"\n\
                    launcher = \"/opt/claude/bin/claude\"\nacp = \"/opt/moa/bin/stub-acp\"\n\
                    [accounts.acct-d]\nkind = \"claude\"\nstore = \"~/.claude-acct-d\"\n";

/// The turn under way fails, as `why` says.
fn fail(b: &TypicalBrain, st: &mut State, why: &str) -> Vec<Effect> {
    let moa = st.central();
    let input = AcpIn::TurnEnded {
        generation: moa.generation,
        prompt: moa.turn.as_ref().expect("a turn").prompt,
        result: Err(why.into()),
    };
    hear(b, st, input)
}

/// moa's agent, still starting, goes, as `why` says.
fn exit(b: &TypicalBrain, st: &mut State, why: &str) -> Vec<Effect> {
    let generation = st.central().generation;
    let why = why.into();
    hear(b, st, AcpIn::Exited { generation, why })
}

/// The new thread asked for couldn't be opened, as `why` says.
fn not_opened(b: &TypicalBrain, st: &mut State, why: &str) -> Vec<Effect> {
    let generation = st.central().generation;
    let result = Err(why.into());
    hear(b, st, AcpIn::Opened { generation, result })
}

/// The account cards the state holds, oldest first: their thread and text.
fn cards(st: &State) -> Vec<(String, String)> {
    let body = |r: &FeedEntry| serde_json::from_str::<Value>(&r.body).unwrap();
    st.feed_tail()
        .filter(|r| r.kind == "card" && body(r)["what"] == "account")
        .map(|r| {
            (
                r.thread_id.clone(),
                body(r)["text"].as_str().unwrap().into(),
            )
        })
        .collect()
}

/// The account cards saved in `fx`.
fn cards_saved(fx: &[Effect]) -> usize {
    feed_rows(fx).iter().filter(|r| r.kind == "card").count()
}

/// The errors published in `fx`.
fn errors(fx: &[Effect]) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Event(CoreEvent::Error { message })) => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn logs(fx: &[Effect]) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l.message.as_str()),
            _ => None,
        })
        .collect()
}

/// `fx` saves a choice of account.
fn switched(fx: &[Effect]) -> bool {
    changes(fx)
        .iter()
        .any(|c| matches!(c, Change::Meta { key, .. } if key == ACCOUNT_KEY))
}

/// The starts of moa's agent in `fx`.
fn starts(fx: &[Effect]) -> Vec<&AdapterSpec> {
    acps(fx)
        .into_iter()
        .filter_map(|c| match c {
            AcpCmd::Start { spec, .. } => Some(spec),
            _ => None,
        })
        .collect()
}

fn on(st: &State, name: &str) -> AdapterSpec {
    launch::spec_on(st, &AccountName::parse(name).unwrap()).unwrap()
}

/// `/account name` from you, as request `req`.
fn switch(b: &TypicalBrain, st: &mut State, req: u64, name: &str) -> Vec<Effect> {
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(req),
        caller: Default::default(),
        op: Op::ConsoleAccount {
            name: Some(name.into()),
        },
    };
    let fx = handle_checked(st, input, b.now());
    assert!(replies(&fx)[0].1.is_ok(), "{fx:?}");
    fx
}

/// A prompt in the run under way: it goes at once, in the open thread.
fn next_turn(b: &TypicalBrain, st: &mut State, req: u64) {
    let fx = prompt(b, st, req, "again");
    assert_eq!(prompts_sent(&fx), ["again"], "{fx:?}");
}

/// moa on `acct-a` (moa.toml's), with `acct-b` too and `settings` after them, and the
/// typical brain's threads.
fn on_acct_a(b: &TypicalBrain, settings: &str) -> State {
    moa_at(b, "acct-a", settings, b.snapshot(), b.now()).0
}

#[test]
fn the_trouble_is_the_accounts() {
    for why in [
        "Authentication required",
        "Invalid API key · Please run /login",
        "OAuth token has expired",
        "Credit balance is too low",
        "Claude AI usage limit reached|1700000000",
        "You've hit your usage limit",
    ] {
        assert!(trouble(why), "{why}");
    }
    for why in ["connection reset", "internal error", ""] {
        assert!(!trouble(why), "{why}");
    }
    assert!(ACCOUNT_TROUBLE.iter().all(|p| *p == p.to_lowercase()));
}

/// The open thread holds the card.
#[test]
fn an_auth_failure_offers_other_accounts() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, "");
    first_turn(&mut st, &b.now(), "th-1", false);
    let fx = fail(&b, &mut st, AUTH);
    assert_eq!(cards(&st), [("th-1".into(), AUTH_CARD.into())]);
    assert_eq!(cards_saved(&fx), 1);
    // The card's row comes after the turn's error line.
    let kinds: Vec<&str> = st.feed_tail().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds[kinds.len() - 2..], ["error", "card"]);
    // Nothing else changes: no start, no switch.
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert!(!switched(&fx), "{fx:?}");
    assert_eq!(launch::account(&st).unwrap().as_str(), "acct-a");
    // The log says which account, never why.
    assert!(logs(&fx).iter().all(|l| !l.contains(AUTH)), "{fx:?}");
    // The next prompt goes on acct-a's run.
    next_turn(&b, &mut st, 2);
    assert_eq!(st.central().account.as_ref().unwrap().as_str(), "acct-a");
    assert_eq!(st.meta(ACCOUNT_KEY), None);
}

/// The open thread holds the card. Only the first line of why, 200 characters at most, and
/// every other account moa can run on, but none it can't.
#[test]
fn a_billing_failure_too() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, MORE);
    first_turn(&mut st, &b.now(), "th-1", false);
    let long = format!(
        "Credit balance is too low {}.\nsee the console",
        "x".repeat(300)
    );
    fail(&b, &mut st, &long);
    let first = format!("Credit balance is too low {}", "x".repeat(174));
    let card = format!(
        "moa's account acct-a failed: {first}.\n\n\
         Nothing was switched. To go on with another account:\n\n\
         `/account acct-b` `/account acct-c`"
    );
    assert_eq!(cards(&st), [("th-1".into(), card)]);
}

/// The open thread holds the card. A why that ends a sentence isn't ended twice.
#[test]
fn a_usage_limit_too() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, "");
    first_turn(&mut st, &b.now(), "th-1", false);
    fail(&b, &mut st, "You've hit your usage limit.");
    let card = AUTH_CARD.replace(AUTH, "You've hit your usage limit");
    assert_eq!(cards(&st), [("th-1".into(), card)]);
}

/// The open thread would hold the card, but writes are refused: its text is an error.
#[test]
fn a_card_that_cant_be_saved_is_an_error() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, "");
    first_turn(&mut st, &b.now(), "th-1", false);
    refuse_writes(&b, &mut st);
    let fx = fail(&b, &mut st, AUTH);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(errors(&fx), [AUTH_CARD]);
    assert!(logs(&fx).iter().all(|l| !l.contains(AUTH)), "{fx:?}");
}

/// The open thread would hold it, but there is no card.
#[test]
fn an_ordinary_failure_is_no_card() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, "");
    first_turn(&mut st, &b.now(), "th-1", false);
    let fx = fail(&b, &mut st, "connection reset");
    assert_eq!(cards(&st), []);
    assert!(errors(&fx).is_empty(), "{fx:?}");
}

/// The open thread holds the cards. One a run's account, until a turn ends well or the
/// account changes.
#[test]
fn once_per_account_until_a_turn_ends_well() {
    let b = TypicalBrain::new();
    let mut st = on_acct_a(&b, "");
    first_turn(&mut st, &b.now(), "th-1", false);
    fail(&b, &mut st, AUTH);
    next_turn(&b, &mut st, 2);
    let fx = fail(&b, &mut st, "Credit balance is too low");
    assert_eq!(cards_saved(&fx), 0, "{fx:?}");
    assert!(errors(&fx).is_empty(), "{fx:?}");
    assert_eq!(cards(&st).len(), 1);

    // A turn that ends well: the next failure says so again.
    next_turn(&b, &mut st, 3);
    let generation = st.central().generation;
    hear(&b, &mut st, ended(generation, 3));
    next_turn(&b, &mut st, 4);
    fail(&b, &mut st, AUTH);
    assert_eq!(cards(&st).len(), 2);

    // A switch away and back: the next failure on acct-a says so again.
    switch(&b, &mut st, 5, "acct-b");
    switch(&b, &mut st, 6, "acct-a");
    first_turn_on(&b, &mut st, 7, "th-2");
    fail(&b, &mut st, AUTH);
    assert_eq!(cards(&st).len(), 3);
    assert_eq!(cards(&st)[2], ("th-2".into(), AUTH_CARD.into()));

    // Another account's failure is its own, and offers the first.
    switch(&b, &mut st, 8, "acct-b");
    first_turn_on(&b, &mut st, 9, "th-3");
    fail(&b, &mut st, AUTH);
    let card = "moa's account acct-b failed: Authentication required.\n\n\
                Nothing was switched. To go on with another account:\n\n\
                `/account acct-a`";
    assert_eq!(cards(&st)[3], ("th-3".into(), card.into()));
}

/// A prompt, as request `req`, that starts moa's agent and opens new thread `id`.
fn first_turn_on(b: &TypicalBrain, st: &mut State, req: u64, id: &str) {
    let generation = st.central().generation + 1;
    prompt(b, st, req, "hello");
    hear(b, st, started(generation, caps(true, true, true)));
    hear(b, st, opened(generation, id, Opened::New));
    assert_eq!(*phase(st), Phase::Thinking);
}

/// moa on `acct-a` with two saved threads, neither open: yesterday's of `acct-a`, and the
/// newer, today's, of `acct-b`.
fn with_threads(b: &TypicalBrain) -> State {
    let now = b.now();
    let yesterday = now.local_day.yesterday().unwrap();
    let t = now.wall.0;
    let threads = vec![
        thread("th-a", "acct-a", yesterday, t - 86_400_000),
        thread("th-b", "acct-b", now.local_day, t - 60_000),
    ];
    moa_with(b, "acct-a", threads)
}

/// No thread is open: the newest saved thread of any account holds the card.
#[test]
fn a_start_that_fails_on_auth_offers_too() {
    let b = TypicalBrain::new();
    let mut st = with_threads(&b);
    prompt(&b, &mut st, 1, "hello");
    let why = "Not logged in · Please run /login";
    let fx = exit(&b, &mut st, why);
    assert!(matches!(phase(&st), Phase::Failed(_)));
    let card = AUTH_CARD.replace(AUTH, why);
    assert_eq!(cards(&st), [("th-b".into(), card)]);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert!(logs(&fx).iter().all(|l| !l.contains("logged")), "{fx:?}");
}

/// No thread is open: the newest saved thread of any account holds the card.
#[test]
fn a_new_thread_that_fails_on_auth_offers_too() {
    let b = TypicalBrain::new();
    let mut st = with_threads(&b);
    prompt(&b, &mut st, 1, "hello");
    let fx = hear(&b, &mut st, started(1, caps(true, true, true)));
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Open { .. }]),
        "a new thread: {fx:?}"
    );
    let fx = not_opened(&b, &mut st, "Invalid API key\nmore");
    let card = AUTH_CARD.replace(AUTH, "Invalid API key");
    assert_eq!(cards(&st), [("th-b".into(), card)]);
    assert!(acps(&fx).is_empty(), "{fx:?}");
}

/// No thread at all, on a brain that never had one: the text goes out as an error, not a
/// row, and the log says which account but not why.
#[test]
fn with_no_thread_the_card_is_an_error() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt(&b, &mut st, 1, "hello");
    let fx = exit(&b, &mut st, AUTH);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(errors(&fx), [AUTH_CARD]);
    let logged = logs(&fx);
    assert!(logged.iter().any(|l| l.contains("acct-a")), "{fx:?}");
    assert!(logged.iter().all(|l| !l.contains(AUTH)), "{fx:?}");

    // Said once for the account.
    prompt(&b, &mut st, 2, "hello");
    let fx = exit(&b, &mut st, AUTH);
    assert!(errors(&fx).is_empty(), "{fx:?}");
}

/// No thread at all, as in [`with_no_thread_the_card_is_an_error`]; a new thread that
/// couldn't open says why as it did, and the account's text after it.
#[test]
fn a_new_thread_with_no_thread_at_all() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    prompt(&b, &mut st, 1, "hello");
    hear(&b, &mut st, started(1, caps(true, true, true)));
    let fx = not_opened(&b, &mut st, AUTH);
    assert_eq!(
        errors(&fx),
        [
            format!("moa couldn't start a thread: {AUTH}").as_str(),
            AUTH_CARD
        ]
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
}

/// The open thread holds the card: moa.toml has no other account moa can run on.
#[test]
fn no_other_account_says_so() {
    let b = TypicalBrain::new();
    let toml = "[brains.default]\nfolders = [\"~/private\"]\nmoa_account = \"acct-a\"\n\
                [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-acct-a\"\n\
                launcher = \"/opt/claude/bin/claude\"\nacp = \"/opt/moa/bin/stub-acp\"\n\
                [accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.claude-acct-b\"\n";
    let deps = Deps {
        home: b.home.clone(),
        ..Deps::default()
    };
    let mut st = started_at(&b, toml, deps, b.snapshot(), b.now()).0;
    first_turn(&mut st, &b.now(), "th-1", false);
    fail(&b, &mut st, AUTH);
    let card = "moa's account acct-a failed: Authentication required.\n\n\
                Nothing was switched, and no other account is set up for moa in moa.toml.";
    assert_eq!(cards(&st), [("th-1".into(), card.into())]);
}

/// Each failure in its own place (none, then the newest saved thread, then the open one):
/// every start of moa's agent is on `acct-a`, and nothing is switched.
#[test]
fn nothing_switches_on_its_own() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    let acct_a = on(&st, "acct-a");
    let mut all = Vec::new();
    // A start that fails: no thread at all.
    all.extend(prompt(&b, &mut st, 1, "hello"));
    all.extend(exit(&b, &mut st, AUTH));
    // A new thread that couldn't open: no thread at all still.
    all.extend(prompt(&b, &mut st, 2, "hello"));
    let generation = st.central().generation;
    all.extend(hear(
        &b,
        &mut st,
        started(generation, caps(true, true, true)),
    ));
    all.extend(not_opened(&b, &mut st, "Credit balance is too low"));
    // A turn that fails: its thread is open.
    all.extend(prompt(&b, &mut st, 3, "hello"));
    all.extend(hear(&b, &mut st, opened(generation, "th-1", Opened::New)));
    all.extend(fail(&b, &mut st, "usage limit reached"));
    all.extend(prompt(&b, &mut st, 4, "and now?"));
    assert_eq!(starts(&all), [&acct_a, &acct_a]);
    assert_eq!(prompts_sent(&all), ["hello", "and now?"]);
    assert!(!switched(&all), "{all:?}");
    assert_eq!(st.meta(ACCOUNT_KEY), None);
    assert_eq!(st.central().account.as_ref().unwrap().as_str(), "acct-a");
    // Said once, for the start that failed; the new thread's error is its own.
    let said = [
        AUTH_CARD,
        "moa couldn't start a thread: Credit balance is too low",
    ];
    assert_eq!(errors(&all), said);
    assert_eq!(cards(&st), []);
}
