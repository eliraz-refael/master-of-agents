//! Slice 4a end to end: the real `moa` binaries, a TempBrain's own terminal server, `moactl
//! open` starting fake-claude in a tab, and its scripted hooks taking the real path: the
//! injected plugin runs `moa hook claude`, which reaches the daemon (or the spool), whose
//! core changes the session that `moa ls` shows. Payloads are made up.
//!
//! Quiet and idle after the turn is seen aren't driven here: Task 5's core tests cover them
//! (`crates/moa-core/tests/hooks.rs`).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moa_client::{Client, ClientOptions, OnMismatch};
use moa_core::config::BrainName;
use moa_core::publish::Topic;
use moa_core::state::CallerInfo;
use moa_platform::{build_id_of, spool};
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for, write_script};
use serde_json::{Map, Value, json};

use common::*;

/// Every wait's limit.
const LIMIT: Duration = Duration::from_secs(10);
/// More than any pipe holds, so a write this big ends only once the reader is reading.
const PAD: usize = 1 << 20;

/// Every `display_state` the daemon publishes for each session, in order, with repeats
/// collapsed: a subscription to the sessions topic, read on a thread of its own, so a
/// state is recorded however slowly the test itself polls.
struct Recorder {
    seen: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
}

impl Recorder {
    /// Subscribed once this returns, so nothing published after it is missed.
    fn start(tb: &TempBrain) -> Recorder {
        let seen: Arc<Mutex<BTreeMap<String, Vec<String>>>> = Arc::default();
        let opts = ClientOptions {
            paths: tb.paths(),
            brain: BrainName::default_brain(),
            client: "hooks-e2e",
            caller: CallerInfo::default(),
            autostart: None,
            on_mismatch: OnMismatch::Accept,
            build: build_id_of(Path::new(MOA)).unwrap(),
        };
        let (ready, subscribed) = mpsc::channel();
        let rows = Arc::clone(&seen);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let mut client = match Client::connect(opts).await {
                    Ok(c) => c,
                    Err(e) => return drop(ready.send(Err(e.to_string()))),
                };
                match client.subscribe(&[Topic::Sessions]).await {
                    Ok(snapshot) => {
                        let now = snapshot.topics.get(&Topic::Sessions);
                        record(&rows, now.map(Vec::as_slice).unwrap_or_default());
                        let _ = ready.send(Ok(()));
                    }
                    Err(e) => return drop(ready.send(Err(e.to_string()))),
                }
                // Until the daemon goes, when the TempBrain drops.
                while let Ok((method, params)) = client.next_notice().await {
                    let at = match method.as_str() {
                        "patch" => "/upsert/sessions",
                        "snapshot" => "/topics/sessions",
                        _ => continue,
                    };
                    if let Some(Value::Array(changed)) = params.pointer(at) {
                        record(&rows, changed);
                    }
                }
            });
        });
        let started = subscribed.recv_timeout(LIMIT).expect("no subscription");
        started.unwrap_or_else(|e| panic!("subscribe: {e}"));
        Recorder { seen }
    }

    /// What session `id` has shown so far.
    fn of(&self, id: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }
}

fn record(seen: &Mutex<BTreeMap<String, Vec<String>>>, rows: &[Value]) {
    let mut seen = seen.lock().unwrap();
    for row in rows {
        let (Some(id), Some(state)) = (row["key"].as_str(), row["display_state"].as_str()) else {
            continue;
        };
        let states = seen.entry(id.to_string()).or_default();
        if states.last().map(String::as_str) != Some(state) {
            states.push(state.to_string());
        }
    }
}

/// A root with the fake account, and `moa` on a terminal, linked to its daemon.
struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    _term: Terminal,
    tb: TempBrain,
}

fn rig() -> Rig {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    Rig { _term: term, tb }
}

/// A session `moactl open` started, and where.
struct Opened {
    id: String,
    dir: PathBuf,
}

impl Rig {
    /// `moactl open --bg <root>/work/<name>`, with `steps` as fake-claude's script there.
    /// In the background: a tab on screen is seen, and a turn's end there would show `idle`,
    /// not `done`.
    fn open(&self, name: &str, steps: &[FakeStep]) -> Opened {
        let dir = self.tb.root().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        write_script(&dir, steps);
        let o = run_exe(
            &self.tb,
            &moactl(&self.tb),
            &["--json", "open", "--bg", &dir.display().to_string()],
            &[("MOA_BRAIN", B)],
        );
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        let data: Value = serde_json::from_str(&out(&o)).unwrap();
        let id = data["id"].as_str().unwrap_or_else(|| panic!("{data}"));
        Opened {
            id: id.to_string(),
            dir: moa_platform::canonical(&dir),
        }
    }

    /// Waits until session `id` shows `want`.
    fn shows(&self, id: &str, want: &str) {
        let seen = wait_for(LIMIT, || {
            (ls_display(&self.tb, id).as_deref() == Some(want)).then_some(())
        });
        assert!(
            seen.is_some(),
            "never {want}: {:?}",
            ls_display(&self.tb, id)
        );
    }

    /// Waits until session `id` has `n` events of `kind`.
    fn has_events(&self, id: &str, kind: &str, n: usize) {
        let seen = wait_for(LIMIT, || (events(&self.tb, id, kind) == n).then_some(()));
        assert!(
            seen.is_some(),
            "{} {kind} events",
            events(&self.tb, id, kind)
        );
    }

    /// `moa hook claude` as session `s`'s pane runs it: the root's environment (`MOA_ROOT`
    /// among it) and the variables `open` gives the tab.
    fn hook(&self, s: &Opened) -> Command {
        let store = self.tb.root().join(".claude-a");
        let mut cmd = Command::new(MOA);
        cmd.args(["hook", "claude"])
            .env_clear()
            .envs(self.tb.env())
            .env("MOA_SESSION", &s.id)
            .env("MOA_BRAIN", B)
            .env("CLAUDE_CONFIG_DIR", store)
            .current_dir(&s.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Runs one hook to the end, as the agent does: it exits 0 and says nothing.
    fn run_hook(&self, s: &Opened, payload: &Value) {
        let mut child = self.hook(s).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        quiet(&child.wait_with_output().unwrap());
    }

    /// A hook that has stamped its time and waits for the last byte of `payload`.
    fn held_hook(&self, s: &Opened, payload: &Value) -> Held {
        // The pad is leading whitespace, not a field: the hook parses the payload and sends
        // it re-serialized, so the line on the wire stays small.
        let bytes = (" ".repeat(PAD) + &payload.to_string()).into_bytes();
        let (head, last) = bytes.split_at(bytes.len() - 1);
        let mut child = self.hook(s).spawn().unwrap();
        // The hook stamps its time first thing, then reads. A write bigger than the pipe
        // returns only once it is reading, so its time is already taken.
        child.stdin.as_mut().unwrap().write_all(head).unwrap();
        Held {
            child,
            last: last.to_vec(),
            stamped: now_ms(),
        }
    }
}

/// A hook process mid-payload.
struct Held {
    child: Child,
    last: Vec<u8>,
    /// No later than this, in ms, the hook stamped its time.
    stamped: i64,
}

impl Held {
    /// Waits for the clock to pass the hook's time, so anything that starts after this is
    /// newer by at least a millisecond (the core orders by ms).
    fn wait_until_older(&self) {
        let passed = wait_for(LIMIT, || (now_ms() > self.stamped).then_some(()));
        assert!(passed.is_some());
    }

    fn finish(mut self) {
        let mut stdin = self.child.stdin.take().unwrap();
        stdin.write_all(&self.last).unwrap();
        drop(stdin);
        quiet(&self.child.wait_with_output().unwrap());
    }
}

fn now_ms() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(d.as_millis()).unwrap()
}

fn quiet(o: &Output) {
    assert!(o.status.success(), "{:?}", o.status);
    assert!(o.stdout.is_empty(), "stdout: {}", out(o));
    assert!(o.stderr.is_empty(), "stderr: {}", err(o));
}

fn hook(event: &str, fields: Value) -> FakeStep {
    FakeStep::Hook {
        event: event.into(),
        fields: fields.as_object().cloned().unwrap_or_default(),
    }
}

/// Holds the script until the test makes `.gate-<n>` in the session's folder.
fn gate(n: usize) -> FakeStep {
    FakeStep::Wait {
        file: format!(".gate-{n}"),
    }
}

/// A Claude payload for session `s`, its conversation `conv`.
fn payload(event: &str, conv: &str, s: &Opened, fields: Value) -> Value {
    let mut p = Map::new();
    p.insert("hook_event_name".into(), event.into());
    p.insert("session_id".into(), conv.into());
    p.insert("cwd".into(), s.dir.display().to_string().into());
    if let Value::Object(more) = fields {
        p.extend(more);
    }
    Value::Object(p)
}

/// The conversation `open` started session `id` with.
fn conversation(tb: &TempBrain, id: &str) -> String {
    let row = ls_row(tb, id).expect("the session is listed");
    row["conversation"]
        .as_str()
        .unwrap_or_else(|| panic!("{row}"))
        .to_string()
}

#[test]
fn fake_agents_drive_every_display_state() {
    let r = rig();
    let rec = Recorder::start(&r.tb);
    let s = r.open(
        "a",
        &[
            hook("UserPromptSubmit", json!({})),
            gate(1),
            hook("PermissionRequest", json!({"tool_name": "Bash"})),
            gate(2),
            hook("PostToolUse", json!({})),
            gate(3),
            hook("Stop", json!({"last_assistant_message": "Done. Ship it?"})),
            gate(4),
            hook("UserPromptSubmit", json!({})),
            hook("StopFailure", json!({"error": "rate_limit"})),
            gate(5),
            hook("UserPromptSubmit", json!({})),
            hook("Stop", json!({"last_assistant_message": "All done."})),
            gate(6),
            FakeStep::Exit,
        ],
    );
    // Each state holds until the daemon has published it: the agent waits at a gate the
    // test opens only then, however late the daemon is.
    let held = [
        "working", "waiting", "working", "question", "failed", "done",
    ];
    for (n, want) in held.into_iter().enumerate() {
        let shown = wait_for(LIMIT, || {
            (rec.of(&s.id).last().map(String::as_str) == Some(want)).then_some(())
        });
        assert!(
            shown.is_some(),
            "never {want}: {:?}; spooled: {:?}",
            rec.of(&s.id),
            fs::read_to_string(r.tb.paths().spool(B)).ok()
        );
        fs::write(s.dir.join(format!(".gate-{}", n + 1)), "").unwrap();
    }
    let ended = wait_for(LIMIT, || {
        (rec.of(&s.id).last().map(String::as_str) == Some("ended")).then_some(())
    });
    assert!(ended.is_some(), "{:?}", rec.of(&s.id));
    // Every line reached the daemon: none was spooled while it was up.
    assert!(!r.tb.paths().spool(B).exists());
    let all = rec.of(&s.id);
    // The open publishes the new session as idle before its first prompt. The two prompts
    // fired right before a StopFailure or a Stop are working only for as long as one hook
    // takes, so the daemon may publish that or not. They have no gate after them, as the
    // brief's script has none; the assertion doesn't need one, since the gates already make
    // every state that matters published.
    //
    // `Exit` sends its clean SessionEnd and then exits, and its pane's exit can reach the
    // daemon first: the session is asleep until the SessionEnd ends it (R14).
    let flicker = |i: usize| {
        let next = all.get(i + 1).map(String::as_str);
        (all[i] == "working" && matches!(next, Some("failed" | "done")))
            || (all[i] == "asleep" && next == Some("ended"))
    };
    let shown: Vec<&str> = (0..all.len())
        .filter(|&i| !(i == 0 && all[i] == "idle") && !flicker(i))
        .map(|i| all[i].as_str())
        .collect();
    assert_eq!(
        shown,
        [
            "working", "waiting", "working", "question", "failed", "done", "ended"
        ],
        "{all:?}"
    );
}

/// No SessionEnd: the pane's exit puts it to sleep (H18, R14).
#[test]
fn a_dying_agent_sleeps() {
    let r = rig();
    let s = r.open("b", &[hook("SessionStart", json!({})), FakeStep::Die]);
    r.shows(&s.id, "asleep");
    assert_eq!(events(&r.tb, &s.id, "session.ended"), 0);
}

#[test]
fn hooks_while_down_are_spooled_and_drained() {
    let r = rig();
    let s = r.open("c", &[hook("SessionStart", json!({})), FakeStep::Hang]);
    r.has_events(&s.id, "session.started", 1);
    r.shows(&s.id, "idle");
    let conv = conversation(&r.tb, &s.id);
    shutdown(&r.tb);

    // Alternating, and ending on a Stop.
    for i in 0..5 {
        let p = if i % 2 == 0 {
            payload("Stop", &conv, &s, json!({"last_assistant_message": "Lit."}))
        } else {
            payload("UserPromptSubmit", &conv, &s, json!({"prompt": "light it"}))
        };
        r.run_hook(&s, &p);
    }
    let spool = r.tb.paths().spool(B);
    let text = fs::read_to_string(&spool).unwrap();
    assert_eq!(text.lines().count(), 5, "{text}");

    let o = run_exe(&r.tb, &moactl(&r.tb), &["ls"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    r.shows(&s.id, "done");
    assert_eq!(events(&r.tb, &s.id, "turn.ended"), 3);
    assert_eq!(events(&r.tb, &s.id, "turn.started"), 2);
    let left = fs::read_to_string(&spool).unwrap_or_default();
    assert!(left.is_empty(), "{left}");
    assert!(!spool::draining(&spool).exists());
}

#[test]
fn out_of_order_hooks_never_regress() {
    let r = rig();
    let s = r.open("d", &[hook("SessionStart", json!({})), FakeStep::Hang]);
    r.has_events(&s.id, "session.started", 1);
    let conv = conversation(&r.tb, &s.id);
    let prompt = payload("UserPromptSubmit", &conv, &s, json!({"prompt": "go"}));
    let stop = payload(
        "Stop",
        &conv,
        &s,
        json!({"last_assistant_message": "Gone."}),
    );

    // An older Stop, delivered after a newer prompt, is history only.
    let a = r.held_hook(&s, &stop);
    a.wait_until_older();
    r.run_hook(&s, &prompt);
    r.shows(&s.id, "working");
    a.finish();
    r.has_events(&s.id, "turn.ended", 1);
    assert_eq!(ls_display(&r.tb, &s.id).as_deref(), Some("working"));

    // Swapped: an older prompt after a newer Stop.
    let a = r.held_hook(&s, &prompt);
    a.wait_until_older();
    r.run_hook(&s, &stop);
    r.shows(&s.id, "done");
    a.finish();
    r.has_events(&s.id, "turn.started", 2);
    assert_eq!(ls_display(&r.tb, &s.id).as_deref(), Some("done"));
}

/// The peers note is `None` until slice 7, so the agent gets nothing; slice 7 changes this.
#[test]
fn session_start_context_reaches_the_agent() {
    let r = rig();
    let s = r.open("e", &[hook("SessionStart", json!({})), FakeStep::Hang]);
    let file = s.dir.join(".fake-claude.context");
    let context = wait_for(LIMIT, || fs::read_to_string(&file).ok());
    assert_eq!(context.as_deref(), Some(""));
}

#[test]
fn open_and_hooks_launch_nothing_in_the_daemon() {
    let r = rig();
    let launches = || status_json(&r.tb)["launches"].as_u64().unwrap();
    let before = launches();
    let s = r.open(
        "f",
        &[
            hook("SessionStart", json!({})),
            hook("UserPromptSubmit", json!({"prompt": "go"})),
            hook("PermissionRequest", json!({"tool_name": "Bash"})),
            hook("PostToolUse", json!({"tool_name": "Bash"})),
            hook("Stop", json!({"last_assistant_message": "Gone."})),
            FakeStep::Exit,
        ],
    );
    r.shows(&s.id, "ended");
    r.has_events(&s.id, "session.ended", 1);
    assert_eq!(launches(), before);
}
