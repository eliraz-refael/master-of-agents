//! `moa hook claude` as the agent runs it: a payload on stdin, a listener standing in for the
//! brain's daemon. Payloads are made up.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moa_testkit::TempBrain;
use serde_json::{Value, json};

const MAX_LINE: usize = 8 << 20;

/// A stand-in daemon: accepts connections at `brain`'s socket and hands each first line to
/// the receiver, answering it with `reply` when there is one. `reply: None` and `hang` leave
/// the connection open without an answer.
fn listen(tb: &TempBrain, brain: &str, reply: Option<&'static str>) -> Receiver<String> {
    let socket = tb.paths().socket(brain).expect("a socket path");
    fs::create_dir_all(socket.parent().expect("a run dir")).expect("the run dir");
    let listener = UnixListener::bind(&socket).expect("bind");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let _ = tx.send(line);
                match reply {
                    Some(text) => {
                        let _ = writeln!(reader.get_mut(), "{text}");
                    }
                    // Hold the connection until the hook gives up.
                    None => {
                        let _ = reader.read_line(&mut String::new());
                    }
                }
            });
        }
    });
    rx
}

fn hook_command(tb: &TempBrain, extra: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_moa"));
    cmd.args(["hook", "claude"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_SESSION", "s-1")
        .env("MOA_BRAIN", "default")
        .envs(extra.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run_hook(mut cmd: Command, stdin: &[u8]) -> Output {
    let mut child = cmd.spawn().expect("moa hook starts");
    let mut pipe = child.stdin.take().expect("stdin");
    let input = stdin.to_vec();
    // A big input is written from a thread, so a hook that stops reading can't block us.
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().expect("moa hook ends");
    let _ = writer.join();
    out
}

fn payload(event: &str) -> Value {
    json!({
        "hook_event_name": event,
        "session_id": "conv-1",
        "cwd": "/work/Lantern",
        "transcript_path": "/work/Lantern/t.jsonl",
        "prompt": "hello",
    })
}

fn quiet(out: &Output) {
    assert!(out.status.success(), "{:?}", out.status);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

fn now_ns() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn recv(rx: &Receiver<String>) -> Value {
    let line = rx.recv_timeout(Duration::from_secs(5)).expect("a line");
    serde_json::from_str(&line).expect("one JSON line")
}

#[test]
fn delivers_one_line_to_the_brains_socket() {
    let tb = TempBrain::new(None);
    let rx = listen(&tb, "default", None);
    let input = payload("UserPromptSubmit");
    let before = now_ns();
    let out = run_hook(hook_command(&tb, &[]), input.to_string().as_bytes());
    let after = now_ns();
    quiet(&out);
    assert!(out.stdout.is_empty());
    let line = recv(&rx);
    assert_eq!(line["jsonrpc"], "2.0");
    assert_eq!(line["method"], "hook");
    assert!(line.get("id").is_none(), "a notification: {line}");
    assert_eq!(line["params"]["payload"], input);
    assert_eq!(line["params"]["env"]["moa_session"], "s-1");
    assert_eq!(line["params"]["agent"], "claude");
    let at = line["params"]["at_ns"].as_i64().unwrap();
    assert!((before..=after).contains(&at), "{before} {at} {after}");
    assert!(rx.try_recv().is_err(), "only one line");
}

#[test]
fn session_start_prints_the_context() {
    let tb = TempBrain::new(None);
    let rx = listen(
        &tb,
        "default",
        Some(r#"{"jsonrpc":"2.0","id":1,"result":{"context":"hi"}}"#),
    );
    let out = run_hook(
        hook_command(&tb, &[]),
        payload("SessionStart").to_string().as_bytes(),
    );
    quiet(&out);
    assert_eq!(recv(&rx)["id"], 1);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"hi\"}}\n"
    );
}

#[test]
fn session_start_without_context_prints_nothing() {
    let tb = TempBrain::new(None);
    let _rx = listen(
        &tb,
        "default",
        Some(r#"{"jsonrpc":"2.0","id":1,"result":{"context":null}}"#),
    );
    let out = run_hook(
        hook_command(&tb, &[]),
        payload("SessionStart").to_string().as_bytes(),
    );
    quiet(&out);
    assert!(out.stdout.is_empty());
}

#[test]
fn session_start_gives_up_after_a_second() {
    let tb = TempBrain::new(None);
    let _rx = listen(&tb, "default", None);
    // The first launch of a fresh binary can take most of a second; that isn't what's timed.
    quiet(&run_hook(hook_command(&tb, &[]), b""));
    let start = Instant::now();
    let out = run_hook(
        hook_command(&tb, &[]),
        payload("SessionStart").to_string().as_bytes(),
    );
    let took = start.elapsed();
    quiet(&out);
    assert!(out.stdout.is_empty());
    assert!(took < Duration::from_millis(1500), "{took:?}");
}

#[test]
fn with_the_daemon_down_the_line_is_spooled() {
    let tb = TempBrain::new(None);
    let input = payload("Stop");
    let out = run_hook(hook_command(&tb, &[]), input.to_string().as_bytes());
    quiet(&out);
    assert!(out.stdout.is_empty());
    let spool = fs::read_to_string(tb.paths().spool("default")).expect("a spool file");
    let lines: Vec<&str> = spool.lines().collect();
    assert_eq!(lines.len(), 1, "{spool}");
    let hook: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(hook["v"], 1);
    assert_eq!(hook["payload"], input);
    assert_eq!(hook["env"]["moa_brain"], "default");
    assert!(hook.get("method").is_none(), "the bare HookLine: {hook}");
}

/// A folder under the root, created, so its canonical form is the same for the config and
/// for the hook.
fn dir(root: &Path, name: &str) -> String {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    moa_platform::canonical(&path).display().to_string()
}

#[test]
fn without_moa_brain_the_route_picks_the_socket() {
    let tb = TempBrain::new(None);
    let root = tb.root();
    let private = dir(root, "private");
    let private2 = dir(root, "private2");
    let store = dir(root, "acct-a");
    let toml = format!(
        r#"
[brains.work]
folders = ["{private}"]

[accounts.acct-a]
kind = "claude"
store = "{store}"
brain = "default"
except = ["{private}"]
"#
    );
    fs::create_dir_all(tb.paths().config_dir()).unwrap();
    fs::write(tb.paths().config_file(), toml).unwrap();
    let default_rx = listen(&tb, "default", None);
    let work_rx = listen(&tb, "work", None);
    let hook = |cwd: &str| {
        let mut input = payload("UserPromptSubmit");
        input["cwd"] = json!(format!("{cwd}/x"));
        let mut cmd = hook_command(&tb, &[("CLAUDE_CONFIG_DIR", &store)]);
        cmd.env_remove("MOA_BRAIN");
        quiet(&run_hook(cmd, input.to_string().as_bytes()));
    };
    fs::create_dir_all(format!("{private2}/x")).unwrap();
    fs::create_dir_all(format!("{private}/x")).unwrap();
    hook(&private2);
    assert_eq!(
        recv(&default_rx)["params"]["payload"]["cwd"],
        format!("{private2}/x")
    );
    assert!(work_rx.try_recv().is_err());
    hook(&private);
    assert_eq!(
        recv(&work_rx)["params"]["payload"]["cwd"],
        format!("{private}/x")
    );
}

#[test]
fn a_moa_brain_that_is_no_brain_name_sends_nothing() {
    let tb = TempBrain::new(None);
    let out = run_hook(
        hook_command(&tb, &[("MOA_BRAIN", "../Nope")]),
        payload("Stop").to_string().as_bytes(),
    );
    quiet(&out);
    assert!(!tb.paths().spool("default").exists());
}

#[test]
fn garbage_stdin_exits_0() {
    let tb = TempBrain::new(None);
    let rx = listen(&tb, "default", None);
    for input in ["", "not json", "[1,2]"] {
        let out = run_hook(hook_command(&tb, &[]), input.as_bytes());
        quiet(&out);
        assert!(out.stdout.is_empty());
    }
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    assert!(!tb.paths().spool("default").exists());
}

#[test]
fn a_huge_payload_is_cut_to_fit() {
    let tb = TempBrain::new(None);
    let rx = listen(&tb, "default", None);
    let mut input = payload("PostToolUse");
    input["tool_response"] = json!("x".repeat(9 << 20));
    let out = run_hook(hook_command(&tb, &[]), input.to_string().as_bytes());
    quiet(&out);
    let line = rx.recv_timeout(Duration::from_secs(10)).expect("a line");
    assert!(line.len() < MAX_LINE, "{}", line.len());
    let line: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(line["params"]["payload"]["moa_cut"], true);
    assert_eq!(line["params"]["payload"]["hook_event_name"], "PostToolUse");
}

#[test]
fn hook_costs_under_15_ms() {
    let tb = TempBrain::new(None);
    let rx = listen(&tb, "default", None);
    let input = payload("UserPromptSubmit").to_string();
    let mut times = Vec::new();
    for _ in 0..21 {
        let start = Instant::now();
        let out = run_hook(hook_command(&tb, &[]), input.as_bytes());
        times.push(start.elapsed());
        quiet(&out);
        recv(&rx);
    }
    times.sort();
    let median = times[times.len() / 2];
    eprintln!(
        "moa hook median: {median:?} (min {:?}, max {:?})",
        times[0], times[20]
    );
    assert!(median < Duration::from_millis(15), "median {median:?}");
}
