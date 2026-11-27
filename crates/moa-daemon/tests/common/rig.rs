//! An in-process daemon on a `/tmp` root whose moa account runs `stub-acp` (and names
//! `fake-claude` as its Claude), and what a test reads back from it.

use std::path::{Path, PathBuf};
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
use moa_daemon::{CoreHandle, DaemonError, Exec, Ready, RunOptions, load_config_with_text, run};
use moa_platform::{Inherited, Paths, ensure_private_dir};
use moa_store::Store;
use moa_testkit::brain::{TYPICAL_NOW, TYPICAL_TZ};
use moa_testkit::{
    FakeStep, StubStep, TempBrain, fake_claude, stub_acp, stub_log, stub_moa_toml, test_bin,
    wait_for, write_script, write_stub_script,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
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
    /// The daemon's program, when it is a copy of `stub-acp` ([`Rig::with_exe`]); else the
    /// test's own.
    pub exe: Option<PathBuf>,
    /// How long a new program has to say its build.
    pub probe_wait: moa_daemon::ProbeWait,
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
    /// Where the daemon hands its socket and lock when it upgrades ([`Up::handoff`]).
    pub handoffs: mpsc::Receiver<Inherited>,
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
            exe: None,
            probe_wait: moa_daemon::ProbeWait::default(),
        }
    }

    /// The daemon's program becomes a copy of `stub-acp` in the root, `bin/moa`, which
    /// answers `build-id` as moa does: [`Rig::rebuild`] replaces it with a new build.
    pub fn with_exe(mut self) -> Rig {
        let dir = self.tb.root().join("bin");
        std::fs::create_dir_all(&dir).expect("the program's folder");
        let exe = dir.join("moa");
        std::fs::copy(stub_acp(), &exe).expect("a copy of stub-acp");
        set_mtime(&exe, 0);
        self.exe = Some(exe);
        self
    }

    /// The daemon's program ([`Rig::with_exe`]).
    pub fn exe(&self) -> &Path {
        self.exe.as_deref().expect("a rig made with_exe")
    }

    /// Puts a new build in the program's place as an install does: a whole copy written
    /// beside it, its time `n` seconds on, then renamed over it.
    pub fn rebuild(&self, n: u64) {
        let new = self.exe().with_extension("new");
        std::fs::copy(stub_acp(), &new).expect("a copy of stub-acp");
        set_mtime(&new, n);
        std::fs::rename(&new, self.exe()).expect("the new build is put in place");
    }

    /// The program's next `build-id` answer comes only after `ms` milliseconds: a slow
    /// first launch (stub-acp reads and removes `<exe>.slow`).
    pub fn slow_next_build_id(&self, ms: u64) {
        std::fs::write(self.exe().with_extension("slow"), ms.to_string()).expect("the slow file");
    }

    /// Puts a half-written program in its place: an empty file, renamed over it.
    pub fn half_rebuild(&self) {
        use std::os::unix::fs::PermissionsExt;
        let new = self.exe().with_extension("new");
        std::fs::write(&new, "").expect("an empty file");
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .expect("it is runnable");
        std::fs::rename(&new, self.exe()).expect("it is put in place");
    }

    /// The build of the daemon's program as it is now.
    pub fn build(&self) -> String {
        match &self.exe {
            Some(exe) => moa_platform::build_id_of(exe).expect("the program's build"),
            None => moa_platform::build_id().to_string(),
        }
    }

    /// Starts the daemon and waits until it is ready.
    pub async fn start(&self) -> Up {
        self.start_from(None).await
    }

    /// Starts the next image of a daemon that upgraded, with what it handed over.
    pub async fn restart(&self, inherited: Inherited) -> Up {
        self.start_from(Some(inherited)).await
    }

    async fn start_from(&self, inherited: Option<Inherited>) -> Up {
        let (tx, rx) = oneshot::channel();
        let (handoff, handoffs) = mpsc::channel(1);
        let exe = match &self.exe {
            Some(exe) => exe.clone(),
            None => moa_daemon::daemon_exe().expect("the test's program"),
        };
        let (config, config_text) = load_config_with_text(&self.paths).expect("a config");
        let done = tokio::spawn(run(RunOptions {
            paths: self.paths.clone(),
            brain: BrainName::default_brain(),
            config,
            config_text: Some(config_text),
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: self.time.clone(),
            alarm: Box::new(self.time.alarm()),
            zone: None,
            ready: Ready::Core(tx),
            events_capacity: 256,
            uid_override: None,
            exe,
            build: self.build(),
            exec: Exec::Test(handoff),
            inherited,
            probe_wait: self.probe_wait,
        }));
        let (sock, core) = match timeout(WAIT, rx).await {
            Ok(Ok(up)) => up,
            Ok(Err(_)) => panic!("run ended before ready: {:?}", done.await),
            Err(_) => panic!("run was not ready after 10 s"),
        };
        Up {
            sock,
            core,
            done,
            handoffs,
        }
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

    /// Ask `id`'s `(status, reply)` as saved.
    pub fn ask(&self, id: i64) -> Option<(String, Option<String>)> {
        self.store()
            .conn()
            .query_row("SELECT status, reply FROM asks WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok()
    }

    /// Waits until ask `id` is saved as `status`.
    pub fn wait_ask(&self, id: i64, status: &str) {
        wait_for(WAIT, || (self.ask(id)?.0 == status).then_some(()))
            .unwrap_or_else(|| panic!("ask #{id} never became {status}: {:?}", self.ask(id)));
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
        let id = session_id(i);
        let dir = self.tb.root().join("work").join(title);
        std::fs::create_dir_all(&dir).expect("the agent's folder");
        write_script(&dir, steps);
        let conversation = conversation_id(i);
        let (pane, pid) = self.open_agent(id, &dir, &conversation);
        let mut rows = session_rows(id, &conversation);
        let Some(Change::Session(s)) = rows.first_mut() else {
            panic!("session_rows starts with the session's row");
        };
        s.title = Some(title.into());
        s.cwd = Some(dir.to_string_lossy().into_owned());
        s.start_cwd = s.cwd.clone();
        rows.push(self.bind(id, &pane, &pid));
        (Tab { id, pane, dir }, rows)
    }

    /// Starts `fake-claude` on conversation `conversation` in a new tab in `dir`, as session
    /// `id`'s agent with moa's session plugin, and tags the tab with `id`. Its pane and pid.
    fn open_agent(&self, id: SessionId, dir: &Path, conversation: &str) -> (String, String) {
        let root = self.tb.root();
        let plugin = root.join("plugin");
        // Every tab shares the one plugin: the first tab writes it.
        if !plugin.exists() {
            for (rel, text) in moa_agents::claude::session_plugin(&test_bin("moa")) {
                let path = plugin.join(rel);
                std::fs::create_dir_all(path.parent().unwrap()).expect("the plugin's folders");
                std::fs::write(path, text).expect("the plugin's files");
            }
        }
        let env = [
            format!("MOA_ROOT={}", root.display()),
            format!("MOA_SESSION={id}"),
            "MOA_BRAIN=default".to_string(),
            format!("CLAUDE_CODE_PLUGIN_DIRS={}", plugin.display()),
        ];
        let (dir, claude) = (dir.to_string_lossy(), fake_claude());
        let mut args = vec!["new-window", "-d", "-t", "=moa:", "-c", &dir];
        for var in &env {
            args.extend(["-e", var]);
        }
        let claude = claude.to_string_lossy();
        args.extend(["-P", "-F", "#{pane_id} #{pane_pid}"]);
        args.extend([&*claude, "--session-id", conversation]);
        let opened = self.tmux(&args);
        let (pane, pid) = opened.split_once(' ').expect("a pane and its pid");
        let tag = id.to_string();
        self.tmux(&["set-option", "-p", "-t", pane, "@moa_session", &tag]);
        (pane.to_string(), pid.to_string())
    }

    /// Session `id`'s binding to `pane` (pid `pid`) on the terminal server, tracked: moa
    /// started its agent.
    fn bind(&self, id: SessionId, pane: &str, pid: &str) -> Change {
        let server = self.tmux(&[
            "display-message",
            "-p",
            "#{pid} #{start_time} #{socket_path}",
        ]);
        let server: Vec<&str> = server.splitn(3, ' ').collect();
        Change::PaneBinding(PaneBinding {
            session_id: id,
            server_pid: server[0].parse().unwrap(),
            server_start: server[1].parse().unwrap(),
            socket: server[2].into(),
            pane_id: pane.into(),
            pane_pid: pid.parse().unwrap(),
            bound_at: UtcMs(1),
            tracked: true,
        })
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
    /// Waits for the daemon to upgrade: what it handed over, once its `run` has ended.
    pub async fn handoff(self) -> Inherited {
        let Up {
            done, mut handoffs, ..
        } = self;
        let inherited = timeout(WAIT, handoffs.recv())
            .await
            .expect("the daemon hands over within 10 s")
            .expect("the daemon handed over");
        let ended = timeout(WAIT, done)
            .await
            .expect("the old image ends")
            .expect("no panic");
        assert!(ended.is_ok(), "{ended:?}");
        assert!(
            matches!(
                handoffs.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ),
            "the daemon handed over once"
        );
        inherited
    }

    /// Whether the daemon has handed over yet.
    pub fn handed_over(&self) -> bool {
        !self.handoffs.is_empty()
    }

    pub fn client(&self) -> Client {
        let mut c = Client::connect(&self.sock);
        c.hello();
        c
    }

    /// A client that says it is session `id`, as `moactl` run in its tab does.
    pub fn session_client(&self, id: SessionId) -> Client {
        self.client_as(&id.to_string())
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
        self.client_as("moa")
    }

    /// A client whose `hello` names `moa_session` as its caller.
    fn client_as(&self, moa_session: &str) -> Client {
        let mut c = Client::connect(&self.sock);
        let hello = json!({
            "client": "test",
            "build": moa_platform::build_id(),
            "caller": {"moa_session": moa_session},
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

/// Sets `path`'s modification time to `n` seconds after a fixed moment: a build id changes
/// with it.
fn set_mtime(path: &Path, n: u64) {
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(at))
        .expect("the time is set");
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
