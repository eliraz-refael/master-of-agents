//! `acp-tap`: records moa's agent. It runs `$MOA_ACP_TAP_TARGET` with its own arguments,
//! passes stdin and stdout through byte for byte (stderr is the agent's own), and appends each
//! line of either as a recording entry (`moa_testkit::recording`) to `$MOA_ACP_TAP_FILE`. It
//! exits as the agent did. It writes no header: `acp-scrub` does, from the agent's own
//! `initialize` answer. Run by hand only, or by a test in front of `stub-acp`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use moa_testkit::recording::{Dir, Entry};
use serde_json::Value;

const TARGET: &str = "MOA_ACP_TAP_TARGET";
const FILE: &str = "MOA_ACP_TAP_FILE";

type Log = Arc<Mutex<File>>;

fn fail(why: impl std::fmt::Display, code: i32) -> ! {
    eprintln!("acp-tap: {why}");
    std::process::exit(code)
}

/// Appends `line` (one line, its newline included or not) to the log as a `dir` entry.
fn record(log: &Log, dir: Dir, line: &[u8]) {
    if line.trim_ascii().is_empty() {
        return;
    }
    let Ok(msg) = serde_json::from_slice::<Value>(line) else {
        let way = match dir {
            Dir::In => "to",
            Dir::Out => "from",
        };
        eprintln!("acp-tap: a line {way} the agent isn't JSON; it isn't recorded");
        return;
    };
    let mut text = serde_json::to_string(&Entry { dir, msg }).expect("an entry is JSON");
    text.push('\n');
    let mut file = log.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = file.write_all(text.as_bytes()) {
        eprintln!("acp-tap: couldn't record a line: {e}");
    }
}

/// Copies `from` to `to` as it arrives, and records each line. A line is recorded before
/// the chunk that ends it is passed on, so it is in the log before the other side can
/// answer it.
fn pump(mut from: impl Read, mut to: impl Write, dir: Dir, log: Log) {
    let mut buf = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        pending.extend_from_slice(&buf[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            record(&log, dir, &line);
        }
        if to.write_all(&buf[..n]).and_then(|()| to.flush()).is_err() {
            break;
        }
    }
    record(&log, dir, &pending);
}

fn main() {
    let (Some(target), Some(file)) = (std::env::var_os(TARGET), std::env::var_os(FILE)) else {
        fail(
            format!("set {TARGET} (the agent to run) and {FILE} (where to record)"),
            2,
        );
    };
    // Private: until it's scrubbed, the recording holds whatever the agent said.
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&file)
        .unwrap_or_else(|e| fail(format!("{}: {e}", file.display()), 2));
    let log = Arc::new(Mutex::new(log));
    let mut agent = Command::new(&target)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| fail(format!("couldn't start {}: {e}", target.display()), 127));
    let (agent_in, agent_out) = (agent.stdin.take(), agent.stdout.take());
    let (Some(agent_in), Some(agent_out)) = (agent_in, agent_out) else {
        fail("no pipes to the agent", 2);
    };

    // Our stdin's end closes the agent's; it isn't waited for.
    let in_log = Arc::clone(&log);
    std::thread::spawn(move || pump(io::stdin(), agent_in, Dir::In, in_log));
    let out = std::thread::spawn(move || pump(agent_out, io::stdout(), Dir::Out, log));

    let status = agent
        .wait()
        .unwrap_or_else(|e| fail(format!("couldn't wait for the agent: {e}"), 2));
    let _ = out.join();
    let code = status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1);
    std::process::exit(code)
}
