//! Slices 6c's and 6d's scenarios, end to end through the real daemon with `stub-acp` as moa's
//! agent: a permission, a slow turn, a hang and the watchdog, a crash while asking, wake-up
//! batches, reports on your prompt and across a restart, and the account card (6c); the
//! opening brief, what's due, a plain yes, and the offer to close an archived session's
//! terminals (6d). Time moves only by `FakeTime::advance`.

mod common;

use std::time::Duration;

use common::hooks::{ago, session_rows};
use common::rig::{Rig, Up, WAIT, answer, end, gate, prompt, refusal, say, session_stop};
use common::{Client, create_server};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::central::opening::{BRIEFED_KEY, OPENING};
use moa_core::change::Change;
use moa_core::model::{PaneBinding, SessionId, Task, TaskId, TaskStatus, YES_DONE, YES_TOLD};
use moa_platform::{kill_term, spawn};
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use moa_testkit::{StubStep, stub_moa_toml_two_accounts, wait_for};
use serde_json::{Value, json};

/// K11: the watchdog's card once it has cancelled a turn.
const CANCELLED: &str = "moa's turn had no news for 5 min, so moa cancelled it.";

/// K11: the watchdog's card once it has restarted moa's agent and reopened today's thread.
const RESTARTED: &str = "moa's turn had no news for 5 min, so moa cancelled it. It still hadn't \
                         stopped 30 s later, so moa's agent was restarted and today's thread \
                         reopened.";

/// A user's terminal attaching, as tmux's mailbox says it: the user is here (D2).
const ATTACH: &str = "moa:hook client-attached /dev/ttys1";

fn minutes(n: u64) -> Duration {
    Duration::from_secs(60 * n)
}

/// The rig's day: the local date of `TYPICAL_NOW`, Sunday 15 November.
fn today() -> Date {
    let now: Timestamp = TYPICAL_NOW.parse().unwrap();
    now.to_zoned(TimeZone::posix(TYPICAL_TZ).unwrap()).date()
}

/// Waits until `meta` `moa.briefed` says `day`: the day moa opened last.
fn wait_briefed(rig: &Rig, day: Date) {
    let want = day.to_string();
    wait_for(WAIT, || (rig.meta(BRIEFED_KEY)? == want).then_some(())).unwrap_or_else(|| {
        panic!(
            "moa.briefed never became {want}: {:?}",
            rig.meta(BRIEFED_KEY)
        )
    });
}

/// The system prompt of each new thread moa's agent was asked to open, oldest first.
fn system_prompts(rig: &Rig) -> Vec<String> {
    rig.logged("session/new")
        .iter()
        .map(|l| {
            l["params"]["systemPrompt"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("a system prompt: {l}"))
                .to_string()
        })
        .collect()
}

/// Suggestion `id`'s saved `result`.
fn suggestion_result(rig: &Rig, id: i64) -> Option<String> {
    rig.store()
        .conn()
        .query_row("SELECT result FROM suggestions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .expect("the suggestion's row")
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

/// D2, review M2: paging back in the console, answered by the daemon from the store, reaches
/// the core as the user being here. On a day not opened yet, moa's agent is sent the opening.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paging_the_console_opens_the_day() {
    let rig = Rig::new(&[say("good morning", "m1"), end()]);
    rig.tb.opened_on(today().yesterday().unwrap());
    let up = rig.start().await;
    let mut c = up.client();
    let page = c.call("console.page", json!({"before": 10, "n": 5}));
    assert!(page["result"]["data"]["rows"].is_array(), "{page}");
    let sent = wait_prompts(&rig, 1);
    assert!(sent[0].starts_with(OPENING), "{}", sent[0]);
    assert_eq!(you_automatic(&rig), [true]);
    up.stop().await;
}

/// The roadmap's "done when" for the opening (D2, R8): moa opened the day, and has a thread.
/// Midnight itself sends nothing. The first attach after it opens a new thread, told the new
/// day's brief, whose first prompt opens the day; the day is saved as opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opening_brief_on_the_first_attach_after_midnight() {
    let rig = Rig::new(&[say("noted", "m1"), end(), say("good morning", "m2"), end()]);
    let up = rig.start().await;
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "light the Lantern")), "noted");
    // Today is opened already: the prompt goes as typed.
    assert_eq!(prompts_sent(&rig), ["light the Lantern"]);
    assert_eq!(rig.threads().len(), 1);
    assert_eq!(rig.meta(BRIEFED_KEY), Some(today().to_string()));

    // 12:00 to 00:01: past midnight, and nobody here.
    rig.time.advance(minutes(12 * 60 + 1));
    up.core.barrier().await;
    assert_eq!(prompts_sent(&rig).len(), 1, "midnight sent a prompt");

    up.mailbox(ATTACH);
    rig.wait_saved("moa", "good morning");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert!(sent[1].starts_with(OPENING), "{}", sent[1]);
    // A new day's thread, told the new day's brief.
    let systems = system_prompts(&rig);
    assert_eq!(systems.len(), 2, "{systems:#?}");
    assert!(
        systems[1].contains("# Brief (Mon 16 Nov 00:01)"),
        "{}",
        systems[1]
    );
    assert_eq!(rig.threads().len(), 2);
    wait_briefed(&rig, today().tomorrow().unwrap());
    assert_eq!(you_automatic(&rig), [false, true]);
    up.stop().await;
}

/// R8: the day opened, then the daemon restarted the same morning. The user's next attach
/// opens nothing, and their prompt goes as typed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_the_same_morning_doesnt_brief_again() {
    let rig = Rig::new(&[say("good morning", "m1"), end(), say("noted", "m2"), end()]);
    rig.tb.opened_on(today().yesterday().unwrap());
    let up = rig.start().await;
    up.mailbox(ATTACH);
    rig.wait_saved("moa", "good morning");
    assert!(prompts_sent(&rig)[0].starts_with(OPENING));
    wait_briefed(&rig, today());
    up.wait_state("phase", json!("ready"));
    up.stop().await;

    let up = rig.start().await;
    rig.time.advance(minutes(60));
    up.mailbox(ATTACH);
    up.core.barrier().await;
    let mut c = up.client();
    assert_eq!(answer(&prompt(&mut c, "light the Lantern")), "noted");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert_eq!(sent[1], "light the Lantern");
    assert_eq!(you_automatic(&rig), [true, false]);
    up.stop().await;
}

/// D2: a terminal turning active is the user being here too. After midnight, a
/// `client-active` line from the user's terminal opens the day.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_turning_active_after_midnight_opens_the_day() {
    let rig = Rig::new(&[say("good morning", "m1"), end()]);
    let up = rig.start().await;
    rig.time.advance(minutes(12 * 60 + 1));
    up.mailbox("moa:hook client-active /dev/ttys1");
    rig.wait_saved("moa", "good morning");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    assert!(sent[0].starts_with(OPENING), "{}", sent[0]);
    wait_briefed(&rig, today().tomorrow().unwrap());
    up.stop().await;
}

/// D2 (code review): moa's own control client attaching to the terminal server isn't the
/// user. Its `client-attached` line opens nothing, though moa's agent is free to be sent
/// the opening; a user's terminal attaching then opens the day.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moas_own_client_attaching_isnt_the_user() {
    let rig = Rig::new(&[end(), say("good morning", "m2"), end()]);
    rig.seed_sessions(1);
    // No day opened yet.
    rig.write(&[Change::Meta {
        key: BRIEFED_KEY.into(),
        value: None,
    }]);
    let up = rig.start().await;
    // A wake-up, which isn't the user, starts moa's agent: it is then free, with a thread,
    // so an opening due would go in the same commit as the line that made it due.
    up.hook(&session_stop(1, ago(0), "the Kite build is green"));
    up.wait_queued(1);
    rig.time.advance(Duration::from_secs(45));
    let sent = wait_prompts(&rig, 1);
    assert!(!sent[0].starts_with(OPENING), "{}", sent[0]);
    up.wait_state("phase", json!("ready"));

    create_server(&rig.tb, &rig.paths);
    let mut c = up.client();
    let linked = c.call("tmux.ensure", json!({}));
    assert_eq!(
        linked["result"]["data"],
        json!({"running": true}),
        "{linked}"
    );
    // The link's control client names itself in `@moa_ctl`.
    let ctl = wait_for(WAIT, || {
        let name = rig
            .tb
            .tmux("default", &["show-options", "-gqv", "@moa_ctl"])
            .ok()?;
        let name = name.trim().to_string();
        (!name.is_empty()).then_some(name)
    })
    .expect("the link's control client is named");

    up.mailbox(&format!("moa:hook client-attached {ctl}"));
    up.core.barrier().await;
    assert_eq!(rig.meta(BRIEFED_KEY), None);
    assert_eq!(prompts_sent(&rig).len(), 1);

    up.mailbox(ATTACH);
    rig.wait_saved("moa", "good morning");
    let sent = prompts_sent(&rig);
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert!(sent[1].starts_with(OPENING), "{}", sent[1]);
    wait_briefed(&rig, today());
    up.stop().await;
}

/// D4: a task due today rides on the day's first wake-up only, and a restart doesn't tell it
/// again: the commit that sent it saved that it was told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whats_due_is_told_once() {
    const DUE: &str = "Due by tomorrow (end your reply with these lines as they are):\n\
                       ⏰ Due today: Merge PR #3";
    let rig = Rig::new(&[end(), end(), end()]);
    rig.seed_sessions(3);
    rig.write(&[Change::Task(Task {
        id: TaskId(1),
        project_id: None,
        title: "Merge PR #3".into(),
        status: TaskStatus::Todo,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: Some(today()),
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    })]);
    // Wake-up `n`: session `n`'s report, 3 min after the last one's, goes once it settles
    // (45 s) and the gap (3 min) since the last wake-up has passed.
    let wake = |up: &Up, n: usize| {
        let i = u32::try_from(n).unwrap();
        let at = ago(-180_000 * (i64::from(i) - 1));
        up.hook(&session_stop(
            i,
            at,
            &format!("the Kite build {i} is green"),
        ));
        up.wait_queued(1);
        rig.time.advance(minutes(3));
        let sent = wait_prompts(&rig, n);
        up.wait_queued(0);
        up.wait_state("phase", json!("ready"));
        sent[n - 1].clone()
    };

    let up = rig.start().await;
    let first = wake(&up, 1);
    assert_eq!(sections(&first), 1, "{first}");
    assert!(first.ends_with(DUE), "{first}");
    let second = wake(&up, 2);
    assert_eq!(sections(&second), 1, "{second}");
    assert!(!second.contains('⏰'), "{second}");
    up.stop().await;

    let up = rig.start().await;
    let third = wake(&up, 3);
    assert_eq!(sections(&third), 1, "{third}");
    assert!(!third.contains('⏰'), "{third}");
    assert_eq!(you_automatic(&rig), [true, true, true]);
    up.stop().await;
}

/// D5, R5: moa suggests something with nothing to run; the user says yes in the console. The
/// yes reaches moa's agent in a prompt of its own, once, and is saved as told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_yes_reaches_moa() {
    let rig = Rig::new(&[say("on it", "m1"), end()]);
    let up = rig.start().await;
    let mut moa = up.moa_client();
    let suggested = moa.call("suggest", json!({"text": "Rename the Kite build"}));
    let id = suggested["result"]["data"]["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("a suggestion: {suggested}"));

    let mut c = up.client();
    let yes = c.call(
        "console.command",
        json!({"line": format!("/suggestion yes {id}")}),
    );
    assert_eq!(answer(&yes), "yes; moa acts on it when it's free");
    rig.wait_saved("moa", "on it");
    let sent = prompts_sent(&rig);
    assert_eq!(
        sent,
        [format!(
            "(moa · the user said yes to your suggestion(s), not typed by the user)\n\
             - #{id}: Rename the Kite build\n\
             Do what each one proposed."
        )]
    );
    assert_eq!(you_automatic(&rig), [true]);
    assert_eq!(suggestion_result(&rig, id).as_deref(), Some(YES_TOLD));
    up.stop().await;
}

/// D9, R4: a session's agent tab with a split beside it, on a real terminal server.
/// Archiving the session offers, in a suggestion, to close the split; the user's yes closes
/// it, and the agent's tab stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archiving_offers_to_close_terminals() {
    let rig = Rig::new(&[]);
    // A first start writes the server's config, which the front door then starts it with.
    rig.start().await.stop().await;
    create_server(&rig.tb, &rig.paths);
    let tmux = |args: &[&str]| {
        let out = rig
            .tb
            .tmux("default", args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"));
        out.trim_end().to_string()
    };
    let panes = || -> Vec<String> {
        let all = tmux(&["list-panes", "-a", "-F", "#{pane_id}"]);
        all.lines().map(str::to_string).collect()
    };

    // Lantern's agent runs in a tab of its own, as moa launched it: bound and tagged.
    let id: SessionId = "0195f3a2-2222-7000-8000-000000000001".parse().unwrap();
    let home = rig.paths.home().to_string_lossy().into_owned();
    let agent = tmux(&[
        "new-window",
        "-d",
        "-t",
        "=moa:",
        "-c",
        &home,
        "-P",
        "-F",
        "#{pane_id} #{pane_pid}",
    ]);
    let (agent, agent_pid) = agent.split_once(' ').unwrap();
    let agent = agent.to_string();
    tmux(&[
        "set-option",
        "-p",
        "-t",
        &agent,
        "@moa_session",
        &id.to_string(),
    ]);
    let server = tmux(&[
        "display-message",
        "-p",
        "#{pid} #{start_time} #{socket_path}",
    ]);
    let server: Vec<&str> = server.splitn(3, ' ').collect();
    let mut rows = session_rows(id, "a1b2c3d4-0000-4000-8000-000000000002");
    if let Some(Change::Session(s)) = rows.first_mut() {
        s.title = Some("Lantern".into());
    }
    rows.push(Change::PaneBinding(PaneBinding {
        session_id: id,
        server_pid: server[0].parse().unwrap(),
        server_start: server[1].parse().unwrap(),
        socket: server[2].into(),
        pane_id: agent.clone(),
        pane_pid: agent_pid.parse().unwrap(),
        bound_at: UtcMs(1),
        tracked: true,
    }));
    rig.write(&rows);

    let up = rig.start().await;
    let mut c = up.client();
    let linked = c.call("tmux.ensure", json!({}));
    assert_eq!(
        linked["result"]["data"],
        json!({"running": true}),
        "{linked}"
    );
    // The user splits the agent's tab: a terminal of Lantern's.
    let split = tmux(&[
        "split-window",
        "-d",
        "-t",
        &agent,
        "-c",
        &home,
        "-P",
        "-F",
        "#{pane_id}",
    ]);
    let term = wait_for(WAIT, || {
        let ls = c.call("term.ls", json!({}));
        ls["result"]["data"]
            .as_array()?
            .iter()
            .find(|t| t["pane"] == json!(split) && t["session"] == "Lantern")
            .cloned()
    })
    .unwrap_or_else(|| {
        panic!(
            "the split isn't Lantern's: {}",
            c.call("term.ls", json!({}))
        )
    });
    let term_id = term["id"].as_i64().unwrap_or_else(|| panic!("{term}"));

    let archived = c.call("session.archive", json!({"qs": [id.to_string()]}));
    assert_eq!(answer(&archived), "archived 1");
    let ls = c.call("suggestion.ls", json!({}));
    let offers = ls["result"]["data"]["suggestions"]
        .as_array()
        .unwrap_or_else(|| panic!("{ls}"));
    assert_eq!(offers.len(), 1, "{ls}");
    let offer = &offers[0];
    let name = term["name"].as_str().unwrap();
    assert_eq!(
        offer["text"],
        format!("Lantern is archived. Close its terminal {name} too?"),
        "{offer}"
    );
    assert_eq!(
        offer["actions"],
        json!([{"verb": "term", "args": ["close", term_id.to_string()]}]),
        "{offer}"
    );
    assert!(panes().contains(&split), "nothing closes before the yes");

    let yes = c.call(
        "console.command",
        json!({"line": format!("/suggestion yes {}", offer["id"])}),
    );
    assert_eq!(answer(&yes), "done");
    // The close starts once the yes is saved, and is saved as the user's first. A program
    // running there (`/bin/sh` can show as one) gets Ctrl-c, and the kill 3 s later.
    let saved =
        |sql: &str| -> bool { rig.store().conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    let closing =
        format!("SELECT count(*) > 0 FROM activity WHERE text = 'closed terminal {name}'");
    wait_for(WAIT, || saved(&closing).then_some(())).expect("the close is saved as the user's");
    // Time moves a second at a time until the split goes, whenever its kill is armed.
    wait_for(WAIT, || {
        let open = panes().contains(&split);
        if open {
            rig.time.advance(Duration::from_secs(1));
        }
        (!open).then_some(())
    })
    .unwrap_or_else(|| panic!("the split is still open: {:?}", panes()));
    let closed = format!("SELECT closed_at IS NOT NULL FROM terminals WHERE id = {term_id}");
    wait_for(WAIT, || saved(&closed).then_some(())).expect("the terminal's record is closed");
    assert!(panes().contains(&agent), "the agent's tab closed too");
    let offer = offer["id"].as_i64().unwrap();
    assert_eq!(suggestion_result(&rig, offer).as_deref(), Some(YES_DONE));
    up.stop().await;
}
