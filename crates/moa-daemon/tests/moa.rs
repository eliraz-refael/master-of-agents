//! The daemon hosts moa's agent: an in-process daemon on a `/tmp` root whose moa account runs
//! `stub-acp` (and names `fake-claude` as its Claude), prompted through the socket.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{Client, request};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, DaemonError, Ready, RunOptions, load_config, run};
use moa_platform::proc::{children, running};
use moa_platform::{Paths, ensure_private_dir, spawn};
use moa_store::Store;
use moa_store::actor::Fault;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use moa_testkit::{
    StubStep, TempBrain, fake_claude, stub_acp, stub_log, wait_for, write_stub_script,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

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

/// `moa.toml`: the default brain's moa runs on `acct-a`, whose agent is the stub.
fn moa_toml() -> String {
    format!(
        "[brains.default]\nmoa_account = \"acct-a\"\n\
         [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-acct-a\"\n\
         launcher = \"{}\"\nacp = \"{}\"\n",
        fake_claude().display(),
        stub_acp().display()
    )
}

/// A root whose agent folder holds the stub's script.
struct Rig {
    _tb: TempBrain,
    paths: Paths,
    time: Arc<FakeTime>,
    /// The agent folder: the stub's script, log and files are here.
    agent: PathBuf,
}

/// A running daemon.
struct Up {
    sock: PathBuf,
    core: CoreHandle,
    done: JoinHandle<Result<(), DaemonError>>,
}

impl Rig {
    fn new(steps: &[StubStep]) -> Rig {
        let tb = TempBrain::new(Some(&moa_toml()));
        let paths = tb.paths();
        let agent = paths.agent_dir("default");
        ensure_private_dir(&agent).expect("the agent folder");
        write_stub_script(&agent, steps);
        let now: Timestamp = TYPICAL_NOW.parse().unwrap();
        let time = FakeTime::new(UtcMs(now.as_millisecond()));
        Rig {
            _tb: tb,
            paths,
            time,
            agent,
        }
    }

    /// Starts the daemon and waits until it is ready.
    async fn start(&self) -> Up {
        let (tx, rx) = oneshot::channel();
        let done = tokio::spawn(run(RunOptions {
            paths: self.paths.clone(),
            brain: BrainName::default_brain(),
            config: load_config(&self.paths).expect("a config"),
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: self.time.clone(),
            alarm: Box::new(self.time.alarm()),
            zone: None,
            ready: Ready::Core(tx),
            events_capacity: 256,
            uid_override: None,
        }));
        let (sock, core) = match timeout(WAIT, rx).await {
            Ok(Ok(up)) => up,
            Ok(Err(_)) => panic!("run ended before ready: {:?}", done.await),
            Err(_) => panic!("run was not ready after 10 s"),
        };
        Up { sock, core, done }
    }

    /// The stub's log lines for `method`, oldest first.
    fn logged(&self, method: &str) -> Vec<Value> {
        stub_log(&self.agent)
            .into_iter()
            .filter(|l| l["method"] == method)
            .collect()
    }

    /// Waits for the stub to log `method`, and returns its last such line.
    fn wait_logged(&self, method: &str) -> Value {
        wait_for(WAIT, || self.logged(method).pop())
            .unwrap_or_else(|| panic!("the stub never logged {method}"))
    }

    fn open(&self, file: &str) {
        std::fs::write(self.agent.join(file), "").expect("the gate opens");
    }

    /// Waits for a feed row of `kind` that says `text`.
    fn wait_saved(&self, kind: &str, text: &str) {
        wait_for(WAIT, || {
            self.feed()
                .iter()
                .any(|(k, t)| k == kind && t == text)
                .then_some(())
        })
        .unwrap_or_else(|| panic!("no {kind} row says {text:?}"));
    }

    fn store(&self) -> Store {
        Store::open_read_only(&self.paths.db("default")).expect("the brain's db")
    }

    /// The `moa_threads` rows' ids.
    fn threads(&self) -> Vec<String> {
        let store = self.store();
        let mut stmt = store
            .conn()
            .prepare("SELECT thread_id FROM moa_threads ORDER BY started_at")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The feed's rows as `(kind, text)`, oldest first; a row without text has `""`.
    fn feed(&self) -> Vec<(String, String)> {
        let store = self.store();
        let mut stmt = store
            .conn()
            .prepare("SELECT kind, body FROM feed ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .map(|(kind, body)| {
                let body: Value = serde_json::from_str(&body).expect("a JSON body");
                (kind, body["text"].as_str().unwrap_or_default().to_string())
            })
            .collect()
    }
}

impl Up {
    fn client(&self) -> Client {
        let mut c = Client::connect(&self.sock);
        c.hello();
        c
    }

    /// Stops the daemon as `shutdown` does, and waits for `run` to return.
    async fn stop(self) {
        let mut c = self.client();
        c.call("shutdown", json!({}));
        let ended = timeout(WAIT, self.done)
            .await
            .expect("the daemon stops in time")
            .expect("no panic");
        assert!(ended.is_ok(), "{ended:?}");
    }
}

/// `console.prompt` with `wait`: the whole reply line.
fn prompt(c: &mut Client, text: &str) -> Value {
    c.call("console.prompt", json!({"text": text, "wait": true}))
}

/// The reply's text, failing on an error.
fn answer(reply: &Value) -> String {
    reply["result"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("an answer: {reply}"))
        .to_string()
}

/// The reply's error message, failing on a result.
fn refusal(reply: &Value) -> String {
    reply["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a refusal: {reply}"))
        .to_string()
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
