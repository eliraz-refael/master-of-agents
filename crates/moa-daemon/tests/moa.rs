//! The daemon hosts moa's agent: an in-process daemon on a `/tmp` root whose moa account runs
//! `stub-acp` (and names `fake-claude` as its Claude), prompted through the socket.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use common::rig::{Rig, WAIT, answer, refusal};
use common::{Client, request};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::model::{FeedEntry, MoaThread, ThreadKind};
use moa_platform::proc::{children, running};
use moa_platform::spawn;
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::{StubStep, fake_claude, wait_for};
use serde_json::{Value, json};

fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

fn end() -> StubStep {
    StubStep::End { stop: None }
}

fn gate(file: &str) -> StubStep {
    StubStep::Wait { file: file.into() }
}

/// `console.prompt` with `wait`: the whole reply line.
fn prompt(c: &mut Client, text: &str) -> Value {
    c.call("console.prompt", json!({"text": text, "wait": true}))
}

fn kinds(feed: &[(String, String)]) -> Vec<&str> {
    feed.iter().map(|(k, _)| k.as_str()).collect()
}

/// Waits for orphans `pids` to exit (not `pid_alive`: an orphan can stay a zombie).
fn gone(pids: &[u32]) {
    wait_for(WAIT, || pids.iter().all(|&p| !running(p)).then_some(()))
        .unwrap_or_else(|| panic!("{pids:?} are still there"));
}

/// The stub's child's pid, once it has written it.
fn child_pid(agent: &Path) -> u32 {
    let file = agent.join(".stub-acp.child");
    wait_for(WAIT, || {
        std::fs::read_to_string(&file).ok()?.trim().parse().ok()
    })
    .expect("the stub started its child")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_lands_in_the_feed() {
    let rig = Rig::new(&[say("Hello from ", "m1"), say("the stub", "m1"), end()]);
    let up = rig.start().await;
    let mut c = up.client();

    let reply = prompt(&mut c, "light the Lantern");
    assert_eq!(answer(&reply), "Hello from the stub");
    let thread = reply["result"]["data"]["thread"].as_str().unwrap();
    assert_eq!(reply["result"]["data"]["stop"], "end_turn");

    assert_eq!(rig.threads(), [thread]);
    assert_eq!(
        rig.feed(),
        [
            ("you".to_string(), "light the Lantern".to_string()),
            ("moa".to_string(), "Hello from the stub".to_string()),
        ]
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_prompts_launch_one_adapter() {
    let rig = Rig::new(&[say("one", "m1"), end(), say("two", "m2"), end()]);
    let up = rig.start().await;
    let mut c = up.client();
    let launched = spawn::launches();

    assert_eq!(answer(&prompt(&mut c, "first")), "one");
    assert_eq!(answer(&prompt(&mut c, "second")), "two");
    assert_eq!(spawn::launches(), launched + 1);
    assert_eq!(rig.logged("initialize").len(), 1);
    assert_eq!(rig.logged("session/new").len(), 1);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_the_same_day_resumes() {
    let rig = Rig::new(&[say("one", "m1"), end(), say("two", "m2"), end()]);
    let up = rig.start().await;
    let reply = prompt(&mut up.client(), "first");
    let thread = reply["result"]["data"]["thread"]
        .as_str()
        .unwrap()
        .to_string();
    up.stop().await;

    let up = rig.start().await;
    assert_eq!(answer(&prompt(&mut up.client(), "second")), "two");
    let resumed = rig.logged("session/resume");
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0]["params"]["sessionId"], thread.as_str());
    assert_eq!(rig.logged("session/new").len(), 1);
    assert_eq!(rig.threads(), [thread]);
    assert_eq!(
        kinds(&rig.feed()),
        ["you", "moa", "you", "moa"],
        "nothing replayed"
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_mid_turn_says_so_and_the_next_prompt_resumes() {
    let rig = Rig::new(&[say("half", "m1"), StubStep::Die, say("back", "m2"), end()]);
    let up = rig.start().await;
    let mut c = up.client();

    // However the host notices (the exit, or the connection's end failing the turn), the
    // user hears that moa's agent stopped, in plain words.
    let lost = refusal(&prompt(&mut c, "first"));
    let why = lost
        .strip_prefix("moa's agent stopped: ")
        .and_then(|rest| rest.strip_suffix("; your next prompt starts it again"))
        .unwrap_or_else(|| panic!("{lost}"));
    assert!(
        ["moa's agent exited", "moa's agent closed its connection"].contains(&why),
        "{lost}"
    );
    let feed = rig.feed();
    let errors: Vec<_> = feed.iter().filter(|(k, _)| k == "error").collect();
    assert_eq!(errors.len(), 1, "{feed:?}");
    assert_eq!(errors[0].1, lost);

    assert_eq!(answer(&prompt(&mut c, "again")), "back");
    let thread = rig.threads();
    assert_eq!(thread.len(), 1);
    let resumed = rig.logged("session/resume");
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0]["params"]["sessionId"], thread[0].as_str());
    assert_eq!(rig.logged("initialize").len(), 2, "started again");
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hang_can_be_cancelled() {
    let rig = Rig::new(&[say("thinking", "m1"), StubStep::Hang]);
    let up = rig.start().await;
    let mut asker = up.client();
    let id = asker.id();
    asker.write(&request(
        id,
        "console.prompt",
        json!({"text": "take your time", "wait": true}),
    ));
    // Its first words are saved: the stub's turn has begun, and waits for the cancel.
    rig.wait_saved("moa", "thinking");

    let mut other = up.client();
    let cancelled = other.call("console.cancel", json!({}));
    assert_eq!(answer(&cancelled), "cancelling moa's turn");

    let reply = loop {
        let v = asker.read().expect("the prompt's answer");
        if v.get("id") == Some(&json!(id)) {
            break v;
        }
    };
    assert_eq!(answer(&reply), "moa's turn ended early: cancelled");
    assert_eq!(rig.logged("session/cancel").len(), 1);
    let nothing = other.call("console.cancel", json!({}));
    assert_eq!(refusal(&nothing), "moa isn't answering anything");
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_daemon_ends_the_adapter_and_its_children() {
    let rig = Rig::new(&[StubStep::Spawn, StubStep::HangHard]);
    let up = rig.start().await;
    let mut c = up.client();
    let id = c.id();
    c.write(&request(
        id,
        "console.prompt",
        json!({"text": "go", "wait": true}),
    ));
    let child = child_pid(&rig.agent);
    // The stub is this process's child (the daemon runs here); its child is the stub's.
    let stub = children(std::process::id())
        .expect("this process's children")
        .into_iter()
        .find(|&p| children(p).is_ok_and(|kids| kids.contains(&child)))
        .expect("the stub is running");
    assert!(running(stub) && running(child));

    up.stop().await;
    gone(&[stub, child]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_commit_replays_moas_updates() {
    let rig = Rig::new(&[say("Hel", "m1"), gate("gate"), say("lo", "m1"), end()]);
    let up = rig.start().await;
    let sock = up.sock.clone();
    let asked = std::thread::spawn(move || {
        let mut c = Client::connect(&sock);
        c.hello();
        prompt(&mut c, "spell it")
    });
    rig.wait_saved("moa", "Hel");
    let failures = up.core.stats().commit_failures.load(Ordering::SeqCst);

    // The commit that carries the second piece fails; the reload brings it back.
    up.core.store_fault(Fault::FailNextCommit);
    rig.open("gate");
    let reply = asked.join().expect("the prompt's thread");
    assert_eq!(answer(&reply), "Hello");
    assert_eq!(
        up.core.stats().commit_failures.load(Ordering::SeqCst),
        failures + 1
    );
    assert_eq!(
        rig.feed(),
        [
            ("you".to_string(), "spell it".to_string()),
            ("moa".to_string(), "Hello".to_string()),
        ]
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_adapter_gets_the_specs_env() {
    let rig = Rig::new(&[end()]);
    let up = rig.start().await;
    prompt(&mut up.client(), "hello");

    let init = rig.wait_logged("initialize");
    let env = &init["env"];
    assert_eq!(env["MOA_SESSION"], "moa");
    assert_eq!(env["MOA_BRAIN"], "default");
    assert_eq!(env["CLAUDE_AGENT_ACP_PROTOCOL"], "v1");
    assert_eq!(
        env["CLAUDE_CODE_EXECUTABLE"],
        fake_claude().display().to_string()
    );
    // The account's store, spelled as `moa.toml`'s folders are: canonical (`/tmp` is a link on
    // macOS).
    let store = moa_platform::canonical(rig.paths.home()).join(".claude-acct-a");
    assert_eq!(env["CLAUDE_CONFIG_DIR"], store.display().to_string());
    let bin = rig.agent.join("bin").display().to_string();
    let path = env["PATH"].as_str().expect("a PATH");
    assert!(path.starts_with(&format!("{bin}:")), "{path}");
    let cwd = PathBuf::from(init["cwd"].as_str().expect("a cwd"));
    assert_eq!(
        moa_platform::canonical(&cwd),
        moa_platform::canonical(&rig.agent)
    );
    assert!(rig.agent.join(".claude/settings.json").is_file());
    up.stop().await;
}

/// F11: the longest prompt reaches moa's agent whole; one character more is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_of_100000_characters_round_trips() {
    let rig = Rig::new(&[end()]);
    let up = rig.start().await;
    let mut c = up.client();
    // Characters, not bytes: a four-byte emoji counts as one.
    let longest = format!("🏮{}Lantern", "é".repeat(100_000 - 8));
    assert_eq!(longest.chars().count(), 100_000);

    answer(&prompt(&mut c, &longest));
    let sent = rig.wait_logged("session/prompt");
    assert_eq!(sent["params"]["prompt"][0]["text"], longest.as_str());

    let too_long = format!("{longest}!");
    assert_eq!(
        refusal(&prompt(&mut c, &too_long)),
        "a prompt is at most 100000 characters"
    );
    assert_eq!(rig.logged("session/prompt").len(), 1);
    up.stop().await;
}

/// The adapter outruns the core: a 5,000-piece answer is all read, saved and answered within
/// 10 s of its last piece.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_5000_chunks_is_answered_in_time() {
    let rig = Rig::new(&[StubStep::Burst { n: 5000 }, end()]);
    let up = rig.start().await;
    let mut c = up.client();
    let id = c.id();
    c.write(&request(
        id,
        "console.prompt",
        json!({"text": "count", "wait": true}),
    ));
    rig.wait_logged("burst.done");
    let last = Instant::now();
    let reply = loop {
        let v = c.read().expect("the prompt's answer");
        if v.get("id") == Some(&json!(id)) {
            break v;
        }
    };
    assert!(last.elapsed() < WAIT, "answered after {:?}", last.elapsed());

    let words: String = (0..5000).map(|i| format!("{i} ")).collect();
    assert_eq!(answer(&reply), words);
    let feed = rig.feed();
    assert_eq!(kinds(&feed), ["you", "moa"]);
    assert_eq!(feed[1].1, words);
    up.stop().await;
}

impl Rig {
    /// Puts `n` feed rows (ids 1 to `n`) in the brain before the daemon starts: the first
    /// `split` in thread `th-a`, the rest in `th-b`.
    fn seed_feed(&self, n: i64, split: i64) {
        let store = Store::open(&self.paths.db("default"), &self.paths.backups("default"))
            .expect("the brain's db");
        let day = jiff::civil::date(2026, 11, 20);
        let mut changes: Vec<Change> = ["th-a", "th-b"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                Change::Thread(MoaThread {
                    thread_id: id.to_string(),
                    account: "default".into(),
                    day,
                    kind: ThreadKind::Daily,
                    started_at: UtcMs(1_000 + i as i64),
                })
            })
            .collect();
        changes.extend((1..=n).map(|id| {
            Change::Feed(FeedEntry {
                id,
                thread_id: if id <= split { "th-a" } else { "th-b" }.into(),
                at: UtcMs(2_000 + id),
                kind: "moa".into(),
                message_id: None,
                automatic: false,
                body: json!({"text": format!("row {id}")}).to_string(),
                state: None,
            })
        }));
        let mut store = store;
        store.apply(&changes).expect("the rows are saved");
    }
}

/// The ids of a `console.page` reply's rows.
fn page_ids(reply: &Value) -> Vec<i64> {
    reply["result"]["data"]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("rows: {reply}"))
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_is_the_rows_before_oldest_first() {
    let rig = Rig::new(&[end()]);
    rig.seed_feed(250, 125);
    let up = rig.start().await;
    let mut c = up.client();

    let reply = c.call("console.page", json!({"before": 201, "n": 100}));
    assert_eq!(page_ids(&reply), (101..=200).collect::<Vec<_>>());
    assert_eq!(answer(&reply), "100 older rows");
    assert_eq!(reply["result"]["data"]["rows"][0]["thread"], "th-a");
    assert_eq!(
        reply["result"]["data"]["rows"][0]["body"],
        json!({"text": "row 101"})
    );

    // `n` defaults to 100, and the oldest rows come back short.
    let reply = c.call("console.page", json!({"before": 11}));
    assert_eq!(page_ids(&reply), (1..=10).collect::<Vec<_>>());
    let reply = c.call("console.page", json!({"before": 1}));
    assert!(page_ids(&reply).is_empty());

    for n in [0, 201] {
        let reply = c.call("console.page", json!({"before": 201, "n": n}));
        assert_eq!(refusal(&reply), "n is 1 to 200");
    }
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_spans_threads() {
    let rig = Rig::new(&[end()]);
    rig.seed_feed(250, 125);
    let up = rig.start().await;

    let reply = up
        .client()
        .call("console.page", json!({"before": 131, "n": 10}));
    assert_eq!(page_ids(&reply), (121..=130).collect::<Vec<_>>());
    let threads: Vec<_> = reply["result"]["data"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["thread"].as_str().unwrap())
        .collect();
    assert_eq!(threads[..5], ["th-a"; 5]);
    assert_eq!(threads[5..], ["th-b"; 5]);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_starts_a_thread_the_same_day() {
    let rig = Rig::new(&[
        say("one", "m1"),
        end(),
        say("two", "m2"),
        end(),
        say("three", "m3"),
        end(),
    ]);
    let up = rig.start().await;
    let mut c = up.client();

    let first = prompt(&mut c, "first");
    let first = first["result"]["data"]["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let told = c.call("console.new", json!({}));
    assert_eq!(answer(&told), "the next prompt starts a new thread");

    let second = prompt(&mut c, "second");
    let second = second["result"]["data"]["thread"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);
    assert_eq!(rig.threads(), [first, second.clone()]);
    let kind_of = |id: &str| -> String {
        rig.store()
            .conn()
            .query_row(
                "SELECT kind FROM moa_threads WHERE thread_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(kind_of(&second), "manual");

    // The prompt after that uses it.
    let third = prompt(&mut c, "third");
    assert_eq!(third["result"]["data"]["thread"], second.as_str());
    assert_eq!(rig.threads().len(), 2);
    up.stop().await;
}
