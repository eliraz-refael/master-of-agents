//! Slice 4a end to end: the real `moa` binaries, a TempBrain's own terminal server, `moactl
//! open` starting fake-claude in a tab, and its scripted hooks taking the real path: the
//! injected plugin runs `moa hook claude`, which reaches the daemon (or the spool), whose
//! core changes the session that `moa ls` shows. Payloads are made up.
//!
//! Quiet, idle after the turn is seen and asleep aren't driven here: Task 5's core tests
//! cover the first two (`crates/moa-core/tests/hooks.rs`), and asleep arrives in 4b.

mod common;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moa_platform::spool;
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for, write_script};
use serde_json::{Map, Value, json};

use common::*;

/// Every wait's limit.
const LIMIT: Duration = Duration::from_secs(10);
/// The scripts' pause between states.
const BEAT: u64 = 300;
/// More than any pipe holds, so a write this big ends only once the reader is reading.
const PAD: usize = 1 << 20;

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
    /// `moactl open <root>/work/<name>`, with `steps` as fake-claude's script there.
    fn open(&self, name: &str, steps: &[FakeStep]) -> Opened {
        let dir = self.tb.root().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        write_script(&dir, steps);
        let o = run_exe(
            &self.tb,
            &moactl(&self.tb),
            &["--json", "open", &dir.display().to_string()],
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

    /// How many `events` rows of `kind` session `id` has, read as `moa sql` reads them.
    fn events(&self, id: &str, kind: &str) -> usize {
        let query =
            format!("select count(*) from events where session_id = '{id}' and kind = '{kind}'");
        let o = run(&self.tb, &["sql", &query], &[]);
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        out(&o).lines().nth(1).unwrap().parse().unwrap()
    }

    /// Waits until session `id` has `n` events of `kind`.
    fn has_events(&self, id: &str, kind: &str, n: usize) {
        let seen = wait_for(LIMIT, || (self.events(id, kind) == n).then_some(()));
        assert!(seen.is_some(), "{} {kind} events", self.events(id, kind));
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
        let mut padded = payload.clone();
        padded["pad"] = json!("x".repeat(PAD));
        let bytes = padded.to_string().into_bytes();
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

fn sleep() -> FakeStep {
    FakeStep::Sleep { ms: BEAT }
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
    let s = r.open(
        "a",
        &[
            hook("UserPromptSubmit", json!({})),
            sleep(),
            hook("PermissionRequest", json!({"tool_name": "Bash"})),
            sleep(),
            hook("PostToolUse", json!({})),
            sleep(),
            hook("Stop", json!({"last_assistant_message": "Done. Ship it?"})),
            sleep(),
            hook("UserPromptSubmit", json!({})),
            hook("StopFailure", json!({"error": "rate_limit"})),
            sleep(),
            hook("UserPromptSubmit", json!({})),
            hook("Stop", json!({"last_assistant_message": "All done."})),
            sleep(),
            FakeStep::Exit,
        ],
    );
    for want in [
        "working", "waiting", "working", "question", "failed", "done", "ended",
    ] {
        r.shows(&s.id, want);
    }
}

/// No SessionEnd: the pane's exit ends it (H18).
#[test]
fn a_dying_agent_ends_its_session() {
    let r = rig();
    let s = r.open("b", &[hook("SessionStart", json!({})), FakeStep::Die]);
    r.shows(&s.id, "ended");
    assert_eq!(r.events(&s.id, "session.ended"), 0);
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
    assert_eq!(r.events(&s.id, "turn.ended"), 3);
    assert_eq!(r.events(&s.id, "turn.started"), 2);
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
