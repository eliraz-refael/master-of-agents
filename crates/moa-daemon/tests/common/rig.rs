//! An in-process daemon on a `/tmp` root whose moa account runs `stub-acp` (and names
//! `fake-claude` as its Claude), and what a test reads back from it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::config::BrainName;
use moa_core::effect::TmuxIn;
use moa_core::model::{PaneBinding, SessionId};
use moa_core::publish::Topic;
use moa_core::tmux::notify::Notification;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, DaemonError, Ready, RunOptions, load_config, run};
use moa_platform::{Paths, ensure_private_dir};
use moa_store::Store;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use moa_testkit::{
    FakeStep, StubStep, TempBrain, fake_claude, stub_log, stub_moa_toml, test_bin, wait_for,
    write_script, write_stub_script,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::Client;
use super::hooks::{hook_line, hook_notice, session_rows, spike_tab};

pub const WAIT: Duration = Duration::from_secs(10);

/// A root whose agent folder holds the stub's script.
pub struct Rig {
    pub tb: TempBrain,
    pub paths: Paths,
    pub time: Arc<FakeTime>,
    /// The agent folder: the stub's script, log and files are here.
    pub agent: PathBuf,
}

/// A session's agent tab on the rig's terminal server ([`Rig::agent_tab`]).
pub struct Tab {
    pub id: SessionId,
    /// The tab's pane, `%N`.
    pub pane: String,
    /// The agent's folder: its script, and the gates its `Wait` steps wait on.
    pub dir: PathBuf,
}

impl Tab {
    /// Opens the gate `file` the agent's script waits on.
    pub fn open(&self, file: &str) {
        std::fs::write(self.dir.join(file), "").expect("the gate opens");
    }
}

/// A running daemon.
pub struct Up {
    pub sock: PathBuf,
    pub core: CoreHandle,
    pub done: JoinHandle<Result<(), DaemonError>>,
}

impl Rig {
    pub fn new(steps: &[StubStep]) -> Rig {
        Rig::with_toml(&stub_moa_toml(), steps)
    }

    /// [`Rig::new`], with `toml` as its `moa.toml`.
    pub fn with_toml(toml: &str, steps: &[StubStep]) -> Rig {
        let tb = TempBrain::new(Some(toml));
        let paths = tb.paths();
        let agent = paths.agent_dir("default");
        ensure_private_dir(&agent).expect("the agent folder");
        write_stub_script(&agent, steps);
        let now: Timestamp = TYPICAL_NOW.parse().unwrap();
        // moa opened the day already: the user's presence opens nothing here (D2).
        tb.opened_on(now.to_zoned(TimeZone::posix(TYPICAL_TZ).unwrap()).date());
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

    /// Waits for a permission row in state `state` (`pending`, `cancelled`, ...).
    pub fn wait_permission(&self, state: &str) {
        wait_for(WAIT, || {
            self.rows()
                .iter()
                .any(|(kind, _, s)| kind == "permission" && s.as_deref() == Some(state))
                .then_some(())
        })
        .unwrap_or_else(|| panic!("no {state} permission row: {:?}", self.rows()));
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

    /// Saves `changes` in the brain, before the daemon starts.
    pub fn write(&self, changes: &[Change]) {
        let mut store = Store::open(&self.paths.db("default"), &self.paths.backups("default"))
            .expect("the brain's db");
        store.apply(changes).expect("the rows are saved");
    }

    /// Puts `n` live, idle sessions in the brain before the daemon starts, each with its
    /// conversation and a spike tab (so they stay live), and returns their ids, oldest first.
    /// [`session_stop`] sends one a `Stop`.
    pub fn seed_sessions(&self, n: u32) -> Vec<SessionId> {
        let ids: Vec<SessionId> = (1..=n).map(session_id).collect();
        let changes: Vec<Change> = (1..=n)
            .flat_map(|i| {
                let id = session_id(i);
                let mut rows = session_rows(id, &conversation_id(i));
                rows.push(spike_tab(id, &format!("%{i}")));
                rows
            })
            .collect();
        self.write(&changes);
        ids
    }

    /// Runs tmux `args` on the rig's terminal server ([`super::create_server`]): its output,
    /// trimmed.
    pub fn tmux(&self, args: &[&str]) -> String {
        let out = self
            .tb
            .tmux("default", args)
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"));
        out.trim_end().to_string()
    }

    /// Seeded session `i` ([`Rig::seed_sessions`]'s ids), titled `title`, as moa launches
    /// one: its agent, `fake-claude` playing `steps`, runs in a tab of its own on the terminal
    /// server ([`super::create_server`]), its hooks reach the daemon through the `moa` binary,
    /// and the tab is bound to it and tagged. Returns the tab, and the rows to save before the
    /// daemon starts ([`Rig::write`]).
    pub fn agent_tab(&self, i: u32, title: &str, steps: &[FakeStep]) -> (Tab, Vec<Change>) {
        let root = self.tb.root();
        let plugin = root.join("plugin");
        for (rel, text) in moa_agents::claude::session_plugin(&test_bin("moa")) {
            let path = plugin.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).expect("the plugin's folders");
            std::fs::write(path, text).expect("the plugin's files");
        }
        let id = session_id(i);
        let dir = root.join("work").join(title);
        std::fs::create_dir_all(&dir).expect("the agent's folder");
        write_script(&dir, steps);
        let env = |k: &str, v: &dyn std::fmt::Display| format!("{k}={v}");
        let (moa_root, moa_session) = (env("MOA_ROOT", &root.display()), env("MOA_SESSION", &id));
        let plugin = env("CLAUDE_CODE_PLUGIN_DIRS", &plugin.display());
        let (dir_arg, claude) = (dir.to_string_lossy(), fake_claude());
        let conversation = conversation_id(i);
        #[rustfmt::skip]
        let opened = self.tmux(&[
            "new-window", "-d", "-t", "=moa:", "-c", &dir_arg,
            "-e", &moa_root, "-e", &moa_session, "-e", "MOA_BRAIN=default", "-e", &plugin,
            "-P", "-F", "#{pane_id} #{pane_pid}",
            &claude.to_string_lossy(), "--session-id", &conversation,
        ]);
        let (pane, pid) = opened.split_once(' ').expect("a pane and its pid");
        self.tmux(&[
            "set-option",
            "-p",
            "-t",
            pane,
            "@moa_session",
            &id.to_string(),
        ]);
        let server = self.tmux(&[
            "display-message",
            "-p",
            "#{pid} #{start_time} #{socket_path}",
        ]);
        let server: Vec<&str> = server.splitn(3, ' ').collect();
        let mut rows = session_rows(id, &conversation);
        if let Some(Change::Session(s)) = rows.first_mut() {
            s.title = Some(title.into());
            s.cwd = Some(dir.to_string_lossy().into_owned());
            s.start_cwd = s.cwd.clone();
        }
        rows.push(Change::PaneBinding(PaneBinding {
            session_id: id,
            server_pid: server[0].parse().unwrap(),
            server_start: server[1].parse().unwrap(),
            socket: server[2].into(),
            pane_id: pane.into(),
            pane_pid: pid.parse().unwrap(),
            bound_at: UtcMs(1),
            tracked: true,
        }));
        let tab = Tab {
            id,
            pane: pane.into(),
            dir,
        };
        (tab, rows)
    }

    /// The `meta` row `key`'s value, if it is saved.
    pub fn meta(&self, key: &str) -> Option<String> {
        let store = self.store();
        store
            .conn()
            // Always one row: NULL when the key isn't saved. An error is the test's to see.
            .query_row(
                "SELECT (SELECT value FROM meta WHERE key = ?1)",
                [key],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("meta {key}: {e}"))
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

    /// The feed's rows as `(kind, body, state)`, oldest first: no ids, no times.
    pub fn rows(&self) -> Vec<(String, Value, Option<String>)> {
        let store = self.store();
        let mut stmt = store
            .conn()
            .prepare("SELECT kind, body, state FROM feed ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .map(|(kind, body, state)| {
            (
                kind,
                serde_json::from_str(&body).expect("a JSON body"),
                state,
            )
        })
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

    /// A client that says it is session `id`, as `moactl` run in its tab does.
    pub fn session_client(&self, id: SessionId) -> Client {
        let mut c = Client::connect(&self.sock);
        let hello = json!({
            "client": "test",
            "build": moa_platform::build_id(),
            "caller": {"moa_session": id.to_string()},
        });
        c.call("hello", hello);
        c
    }

    /// Links the daemon to its terminal server ([`super::create_server`]).
    pub fn link(&self) {
        let linked = self.client().call("tmux.ensure", json!({}));
        assert_eq!(
            linked["result"]["data"],
            json!({"running": true}),
            "{linked}"
        );
    }

    /// A client that says it is moa's own agent, as `moactl` run by moa's agent does.
    pub fn moa_client(&self) -> Client {
        let mut c = Client::connect(&self.sock);
        let hello = json!({
            "client": "test",
            "build": moa_platform::build_id(),
            "caller": {"moa_session": "moa"},
        });
        c.call("hello", hello);
        c
    }

    /// Sends hook `line` as a hook does: a notice, with no `hello`.
    pub fn hook(&self, line: &Value) {
        Client::connect(&self.sock).write(&hook_notice(line));
    }

    /// Sends `line` as tmux's mailbox does (`moa:hook client-attached /dev/ttys1`, ...): a
    /// message notification from the terminal server, with no link to one needed.
    pub fn mailbox(&self, line: &str) {
        self.core
            .tmux_input(TmuxIn::Notification(Notification::Message(line.into())));
    }

    /// The `feed` topic's `state` row, as published now; `null` while it isn't.
    pub fn moa_state(&self) -> Value {
        self.core
            .model()
            .borrow()
            .topics
            .get(&Topic::Feed)
            .and_then(|feed| feed.get("state"))
            .cloned()
            .unwrap_or_default()
    }

    /// Waits until the published `state` row's `key` is `want`. The model is published once
    /// a commit is released, a moment after the rows reach the disk, so a test that has seen
    /// the rows waits for it too.
    pub fn wait_state(&self, key: &str, want: Value) {
        wait_for(WAIT, || (self.moa_state()[key] == want).then_some(()))
            .unwrap_or_else(|| panic!("{key} never became {want}: {}", self.moa_state()));
    }

    /// Waits until the `state` row says `n` session reports are queued.
    pub fn wait_queued(&self, n: u64) {
        self.wait_state("queued", json!(n));
    }

    /// The handle of session `id`, as the `sessions` topic publishes it.
    pub fn handle(&self, id: SessionId) -> String {
        let model = self.core.model();
        let model = model.borrow();
        model
            .topics
            .get(&Topic::Sessions)
            .and_then(|sessions| sessions.get(&id.to_string()))
            .and_then(|session| session["handle"].as_str())
            .unwrap_or_else(|| panic!("session {id} isn't published"))
            .to_string()
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

/// An answer chunk of message `id`.
pub fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

/// The turn ends `end_turn`.
pub fn end() -> StubStep {
    StubStep::End { stop: None }
}

/// The turn waits for `file`: [`Rig::open`] opens it.
pub fn gate(file: &str) -> StubStep {
    StubStep::Wait { file: file.into() }
}

/// `console.prompt` with `wait`: the whole reply line.
pub fn prompt(c: &mut Client, text: &str) -> Value {
    c.call("console.prompt", json!({"text": text, "wait": true}))
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

/// The id of seeded session `i` ([`Rig::seed_sessions`]).
fn session_id(i: u32) -> SessionId {
    format!("0195f3a2-1111-7000-8000-{i:012}")
        .parse()
        .expect("a session id")
}

/// The conversation of seeded session `i`. Its first 8 characters, the session's handle, are
/// its own.
fn conversation_id(i: u32) -> String {
    format!("a1b2c3{i:02}-0000-4000-8000-000000000001")
}

/// A `Stop` of seeded session `i` ([`Rig::seed_sessions`]) at `at_ms`, whose turn's last
/// message is `said`: it queues a session report. Its `seq` is `i`, so two sessions' lines at
/// the same time aren't one line twice.
pub fn session_stop(i: u32, at_ms: i64, said: &str) -> Value {
    let session = session_id(i).to_string();
    let mut line = hook_line(&session, &conversation_id(i), "Stop", at_ms, i);
    line["payload"]["last_assistant_message"] = json!(said);
    line
}
