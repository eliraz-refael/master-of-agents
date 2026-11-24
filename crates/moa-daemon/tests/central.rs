//! Slice 6c's scenarios, end to end through the real daemon with `stub-acp` as moa's agent: a
//! permission, a slow turn, a hang and the watchdog, a crash while asking, wake-up batches,
//! reports on your prompt and across a restart, and the account card. Time moves only by
//! `FakeTime::advance`.

mod common;

use std::time::Duration;

use common::Client;
use common::hooks::ago;
use common::rig::{Rig, WAIT, answer, end, gate, prompt, refusal, say, session_stop};
use moa_platform::{kill_term, spawn};
use moa_testkit::{StubStep, stub_moa_toml_two_accounts, wait_for};
use serde_json::{Value, json};

/// K11: the watchdog's card once it has cancelled a turn.
const CANCELLED: &str = "moa's turn had no news for 5 min, so moa cancelled it.";

/// K11: the watchdog's card once it has restarted moa's agent and reopened today's thread.
const RESTARTED: &str = "moa's turn had no news for 5 min, so moa cancelled it. It still hadn't \
                         stopped 30 s later, so moa's agent was restarted and today's thread \
                         reopened.";

fn minutes(n: u64) -> Duration {
    Duration::from_secs(60 * n)
}

/// A permission request moa's agent makes, allowing once or refusing.
fn ask() -> StubStep {
    StubStep::Ask {
        title: "delete the Harbor".into(),
        options: vec![
            ("yes".into(), "allow_once".into()),
            ("no".into(), "reject_once".into()),
        ],
    }
}

/// The texts of the prompts the stub was sent, oldest first.
fn prompts_sent(rig: &Rig) -> Vec<String> {
    rig.logged("session/prompt")
        .iter()
        .map(|l| {
            l["params"]["prompt"][0]["text"]
                .as_str()
                .expect("a text prompt")
                .to_string()
        })
        .collect()
}

/// Waits for the stub to have been sent `n` prompts, and returns their texts.
fn wait_prompts(rig: &Rig, n: usize) -> Vec<String> {
    wait_for(WAIT, || {
        let sent = prompts_sent(rig);
        (sent.len() >= n).then_some(sent)
    })
    .unwrap_or_else(|| panic!("{n} prompts never went: {:?}", prompts_sent(rig)))
}

/// How many session reports a prompt's text carries: its `### ` headings.
fn sections(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("### ")).count()
}

/// The texts of the feed's `card` rows, oldest first.
fn cards(rig: &Rig) -> Vec<String> {
    let feed = rig.feed();
    feed.into_iter()
        .filter(|(k, _)| k == "card")
        .map(|(_, t)| t)
        .collect()
}

/// Whether each `you` row was automatic (a wake-up's), oldest first.
fn you_automatic(rig: &Rig) -> Vec<bool> {
    let store = rig.store();
    let mut stmt = store
        .conn()
        .prepare("SELECT automatic FROM feed WHERE kind = 'you' ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, bool>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_permission_waits_and_once_goes_on() {
    let rig = Rig::new(&[ask(), say("understood", "m1"), end()]);
    let up = rig.start().await;
    let sock = up.sock.clone();
    let asked = std::thread::spawn(move || {
        let mut c = Client::connect(&sock);
        c.hello();
        prompt(&mut c, "tidy up")
    });
    rig.wait_permission("pending");
    up.core.barrier().await;
    // The turn waits for you: its prompt has no answer, and moa's agent hears nothing.
    assert!(!asked.is_finished(), "the prompt was answered early");
    assert!(rig.logged("permission.outcome").is_empty());
    up.wait_state("phase", json!("thinking"));

    let reply = up
        .client()
        .call("console.permission", json!({"answer": "once"}));
    assert_eq!(answer(&reply), "allowed once");
    let outcome = rig.wait_logged("permission.outcome");
    assert_eq!(
        outcome["params"],
        json!({"outcome": "selected", "optionId": "yes"})
    );
    let reply = asked.join().expect("the prompt's thread");
    assert_eq!(answer(&reply), "understood");
    rig.wait_permission("allow_once");
    up.wait_state("asking", json!([]));
    up.stop().await;
}

/// A turn that says something within every 5 minutes is never the watchdog's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_turn_is_left_alone() {
    let rig = Rig::new(&[
        say("first", "m1"),
        gate("g1"),
        say("second", "m2"),
        gate("g2"),
        end(),
    ]);
    let up = rig.start().await;
    let mut c = up.client();
    let id = c.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_saved("moa", "first");

    rig.time.advance(minutes(4));
    rig.open("g1");
    rig.wait_saved("moa", "second");
    rig.time.advance(minutes(4));
    rig.open("g2");

    let reply = c.reply(id);
    assert_eq!(reply["result"]["data"]["stop"], "end_turn", "{reply}");
    assert!(rig.logged("session/cancel").is_empty());
    assert!(cards(&rig).is_empty(), "{:?}", rig.feed());
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hang_is_cancelled_by_the_watchdog() {
    let rig = Rig::new(&[StubStep::Hang]);
    let up = rig.start().await;
    let mut c = up.client();
    let id = c.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_logged("session/prompt");

    rig.time.advance(minutes(5));
    rig.wait_logged("session/cancel");
    let reply = c.reply(id);
    assert_eq!(answer(&reply), "moa's turn ended early: cancelled");
    assert_eq!(reply["result"]["data"]["stop"], "cancelled");
    rig.wait_saved("card", CANCELLED);
    assert_eq!(cards(&rig), [CANCELLED]);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hard_hang_restarts_moas_agent() {
    let rig = Rig::new(&[StubStep::HangHard, say("back", "m2"), end()]);
    let up = rig.start().await;
    let mut c = up.client();
    let id = c.send(
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    );
    rig.wait_logged("session/prompt");
    let launched = spawn::launches();
    let thread = rig.threads();
    assert_eq!(thread.len(), 1);

    rig.time.advance(minutes(5));
    rig.wait_logged("session/cancel");
    rig.wait_saved("card", CANCELLED);
    // Cancelled, the turn still runs: 30 s later moa's agent is restarted.
    rig.time.advance(Duration::from_secs(30));
    let reply = c.reply(id);
    assert_eq!(
        refusal(&reply),
        "moa's turn hung, so moa's agent was restarted"
    );
    rig.wait_saved("card", RESTARTED);
    assert_eq!(cards(&rig), [RESTARTED]);
    assert_eq!(rig.logged("initialize").len(), 2);
    let resumed = rig.logged("session/resume");
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0]["params"]["sessionId"], thread[0].as_str());
    assert_eq!(spawn::launches(), launched + 1);

    assert_eq!(answer(&prompt(&mut c, "again")), "back");
    assert_eq!(rig.threads(), thread);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_while_asking_clears_the_card() {
    // The stub waits on its request until it is answered, so no step of its script can end
    // it then: the test does, as a crash would.
    let rig = Rig::new(&[ask(), end()]);
    let up = rig.start().await;
    let sock = up.sock.clone();
    let asked = std::thread::spawn(move || {
        let mut c = Client::connect(&sock);
        c.hello();
        prompt(&mut c, "tidy up")
    });
    rig.wait_permission("pending");
    let stub = rig.wait_logged("initialize")["pid"]
        .as_u64()
        .expect("the stub's pid");
    kill_term(u32::try_from(stub).unwrap());

    let reply = asked.join().expect("the prompt's thread");
    assert!(
        refusal(&reply).starts_with("moa's agent stopped: "),
        "{reply}"
    );
    rig.wait_permission("cancelled");
    up.wait_state("asking", json!([]));
    assert!(rig.logged("permission.outcome").is_empty());
    up.stop().await;
}

/// Ten sessions finish a turn at once: the first wake-up, once they settle, carries 8 of them
/// (`wake_batch`), and the next, a gap later, the other 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_wake_moa_in_batches() {
    let rig = Rig::new(&[end(), end()]);
    let ids = rig.seed_sessions(10);
    let up = rig.start().await;
    for i in 1..=10 {
        up.hook(&session_stop(
            i,
            ago(0),
            &format!("the Kite build {i} is green"),
        ));
    }
    up.wait_queued(10);
    assert!(prompts_sent(&rig).is_empty());

    rig.time.advance(Duration::from_secs(45));
    let sent = wait_prompts(&rig, 1);
    assert_eq!(sections(&sent[0]), 8, "{}", sent[0]);
    up.wait_queued(2);
    up.core.barrier().await;
    assert_eq!(prompts_sent(&rig).len(), 1, "the gap holds the rest");

    rig.time.advance(Duration::from_secs(180));
    let sent = wait_prompts(&rig, 2);
    assert_eq!(sections(&sent[1]), 2, "{}", sent[1]);
    // Each session is in one wake-up, and only one: its heading names its handle.
    for id in ids {
        let heading = format!(" · session {} · ", up.handle(id));
        let carried: usize = sent.iter().map(|p| p.matches(&heading).count()).sum();
        assert_eq!(carried, 1, "{heading:?} in {sent:#?}");
    }
    up.wait_queued(0);
    assert_eq!(you_automatic(&rig), [true, true]);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_report_rides_on_your_next_prompt() {
    let rig = Rig::new(&[say("noted", "m1"), end()]);
    rig.seed_sessions(1);
    let up = rig.start().await;
    let mut c = up.client();
    let off = c.call("console.auto", json!({"on": false}));
    assert_eq!(off["result"]["data"]["auto"], false, "{off}");
    up.hook(&session_stop(1, ago(0), "the Beacon tests pass"));
    up.wait_queued(1);

    assert_eq!(answer(&prompt(&mut c, "light the Lantern")), "noted");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 1);
    assert_eq!(sections(&sent[0]), 1, "{}", sent[0]);
    assert!(sent[0].contains("> the Beacon tests pass"), "{}", sent[0]);
    assert!(sent[0].ends_with("light the Lantern"), "{}", sent[0]);
    assert_eq!(you_automatic(&rig), [false]);
    up.wait_queued(0);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_survive_a_daemon_restart() {
    let rig = Rig::new(&[end(), end()]);
    rig.seed_sessions(1);
    let up = rig.start().await;
    up.hook(&session_stop(1, ago(0), "the Beacon tests pass"));
    up.wait_queued(1);
    up.stop().await;

    let up = rig.start().await;
    up.wait_queued(1);
    let mut c = up.client();
    prompt(&mut c, "first");
    prompt(&mut c, "second");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 2);
    assert_eq!(sections(&sent[0]), 1, "{}", sent[0]);
    assert!(sent[0].contains("> the Beacon tests pass"), "{}", sent[0]);
    assert_eq!(sent[1], "second", "a report reaches moa once");
    up.wait_queued(0);
    up.stop().await;
}

/// A turn that fails for its account's sake says so in a card that offers the other account,
/// and nothing switches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_auth_failure_shows_the_account_card() {
    let fail = StubStep::Fail {
        message: "Authentication required".into(),
    };
    let rig = Rig::with_toml(&stub_moa_toml_two_accounts(), &[fail]);
    let up = rig.start().await;
    let mut c = up.client();

    let reply = prompt(&mut c, "light the Lantern");
    assert!(reply["error"].is_object(), "{reply}");
    let card = "moa's account acct-a failed: Authentication required.\n\nNothing was \
                switched. To go on with another account:\n\n`/account acct-b`";
    rig.wait_saved("card", card);
    let card_row: Vec<Value> = rig
        .rows()
        .into_iter()
        .filter(|(kind, _, _)| kind == "card")
        .map(|(_, body, _)| body)
        .collect();
    assert_eq!(card_row, [json!({"what": "account", "text": card})]);
    let on = c.call("console.account", json!({}));
    assert_eq!(answer(&on), "moa is on acct-a; others: acct-b");
    up.stop().await;
}
