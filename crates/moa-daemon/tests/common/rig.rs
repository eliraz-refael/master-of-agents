//! An in-process daemon on a `/tmp` root whose moa account runs `stub-acp` (and names
//! `fake-claude` as its Claude), and what a test reads back from it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::config::BrainName;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, DaemonError, Ready, RunOptions, load_config, run};
use moa_platform::{Paths, ensure_private_dir};
use moa_store::Store;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use moa_testkit::{StubStep, TempBrain, stub_log, stub_moa_toml, wait_for, write_stub_script};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::Client;

pub const WAIT: Duration = Duration::from_secs(10);

/// A root whose agent folder holds the stub's script.
pub struct Rig {
    pub tb: TempBrain,
    pub paths: Paths,
    pub time: Arc<FakeTime>,
    /// The agent folder: the stub's script, log and files are here.
    pub agent: PathBuf,
}

/// A running daemon.
pub struct Up {
    pub sock: PathBuf,
    pub core: CoreHandle,
    pub done: JoinHandle<Result<(), DaemonError>>,
}

impl Rig {
    pub fn new(steps: &[StubStep]) -> Rig {
        let tb = TempBrain::new(Some(&stub_moa_toml()));
        let paths = tb.paths();
        let agent = paths.agent_dir("default");
        ensure_private_dir(&agent).expect("the agent folder");
        write_stub_script(&agent, steps);
        let now: Timestamp = TYPICAL_NOW.parse().unwrap();
        let time = FakeTime::new(UtcMs(now.as_millisecond()));
        Rig {
            tb,
            paths,
            time,
            agent,
        }
    }

    /// Starts the daemon and waits until it is ready.
    pub async fn start(&self) -> Up {
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
    pub fn logged(&self, method: &str) -> Vec<Value> {
        stub_log(&self.agent)
            .into_iter()
            .filter(|l| l["method"] == method)
            .collect()
    }

    /// Waits for the stub to log `method`, and returns its last such line.
    pub fn wait_logged(&self, method: &str) -> Value {
        wait_for(WAIT, || self.logged(method).pop())
            .unwrap_or_else(|| panic!("the stub never logged {method}"))
    }

    pub fn open(&self, file: &str) {
        std::fs::write(self.agent.join(file), "").expect("the gate opens");
    }

    /// Waits for a feed row of `kind` that says `text`.
    pub fn wait_saved(&self, kind: &str, text: &str) {
        wait_for(WAIT, || {
            self.feed()
                .iter()
                .any(|(k, t)| k == kind && t == text)
                .then_some(())
        })
        .unwrap_or_else(|| panic!("no {kind} row says {text:?}"));
    }

    pub fn store(&self) -> Store {
        Store::open_read_only(&self.paths.db("default")).expect("the brain's db")
    }

    /// The `moa_threads` rows' ids.
    pub fn threads(&self) -> Vec<String> {
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
    pub fn feed(&self) -> Vec<(String, String)> {
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
    pub fn client(&self) -> Client {
        let mut c = Client::connect(&self.sock);
        c.hello();
        c
    }

    /// Stops the daemon as `shutdown` does, and waits for `run` to return.
    pub async fn stop(self) {
        let mut c = self.client();
        c.call("shutdown", json!({}));
        let ended = timeout(WAIT, self.done)
            .await
            .expect("the daemon stops in time")
            .expect("no panic");
        assert!(ended.is_ok(), "{ended:?}");
    }
}

/// The reply's text, failing on an error.
pub fn answer(reply: &Value) -> String {
    reply["result"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("an answer: {reply}"))
        .to_string()
}

/// The reply's error message, failing on a result.
pub fn refusal(reply: &Value) -> String {
    reply["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a refusal: {reply}"))
        .to_string()
}
