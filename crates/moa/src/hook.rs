//! `moa hook claude`: what Claude Code runs on every event, the payload on stdin. It sends
//! one JSON-RPC line to the brain's daemon, or spools it when the daemon is down. It uses std
//! only (no runtime, no client library, no child process), writes nothing to stderr and
//! always exits 0, so an agent never fails or waits on moa. Only SessionStart waits, at most
//! a second, for the daemon's `context`.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moa_core::config::{BrainName, Config, caller_store};
use moa_core::hook::{HOOK_LINE_V, HookEnv, HookLine};
use moa_core::model::AgentKind;
use moa_core::route::{Route, route};
use moa_platform::{Paths, canonical, net, spool};
use moa_proto::MAX_LINE;
use serde::Deserializer;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde_json::{Map, Value};

/// The most of stdin the hook reads: the wire's line limit, and one byte to tell "over".
pub const HOOK_STDIN_CAP: u64 = MAX_LINE as u64 + 1;
/// A payload bigger than this is cut down to its identifying fields (H12).
pub const HOOK_PAYLOAD_ROOM: usize = MAX_LINE - (64 << 10);

const CONNECT: Duration = Duration::from_millis(200);
const WRITE: Duration = Duration::from_millis(200);
const SESSION_START_WAIT: Duration = Duration::from_secs(1);

/// The payload fields a cut payload keeps: the identifying ones, and the short ones the
/// normalizer reads, so a cut `PermissionRequest` still names its tool.
const KEPT: [&str; 9] = [
    "hook_event_name",
    "session_id",
    "cwd",
    "transcript_path",
    "tool_name",
    "source",
    "reason",
    "notification_type",
    "new_cwd",
];

pub fn run(args: &[OsString]) -> ExitCode {
    let at_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0);
    if args.get(2).is_some_and(|a| a == "claude") {
        hook_claude(at_ns);
    }
    ExitCode::SUCCESS
}

fn hook_claude(at_ns: i64) {
    let mut input = Vec::new();
    if std::io::stdin()
        .lock()
        .take(HOOK_STDIN_CAP)
        .read_to_end(&mut input)
        .is_err()
    {
        return;
    }
    let Some(payload) = parse_payload(&input) else {
        return;
    };
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let env = HookEnv {
        moa_session: var("MOA_SESSION"),
        moa_brain: var("MOA_BRAIN"),
        claude_config_dir: var("CLAUDE_CONFIG_DIR"),
        claude_session_id: var("CLAUDE_CODE_SESSION_ID"),
        tmux: var("TMUX"),
        tmux_pane: var("TMUX_PANE"),
    };
    let Ok(paths) = Paths::from_env() else {
        return;
    };
    let Some(brain) = pick_brain(&paths, &env, &payload) else {
        return;
    };
    let Ok(socket) = paths.socket(brain.as_str()) else {
        return;
    };
    let session_start = payload["hook_event_name"] == "SessionStart";
    let hook = HookLine {
        v: HOOK_LINE_V,
        agent: AgentKind::Claude,
        at_ns,
        pid: std::process::id(),
        seq: 0,
        env,
        payload,
    };
    let Ok(hook_json) = serde_json::to_string(&hook) else {
        return;
    };
    // Only SessionStart is a request: its reply carries the context.
    let id = if session_start { r#""id":1,"# } else { "" };
    let line = format!("{{\"jsonrpc\":\"2.0\",{id}\"method\":\"hook\",\"params\":{hook_json}}}\n");
    match send(&socket, line.as_bytes()) {
        Some(stream) if session_start => {
            if let Some(text) = read_context(stream) {
                print_context(&text);
            }
        }
        Some(_) => {}
        None => {
            let _ = spool::append(&paths.spool(brain.as_str()), hook_json.as_bytes());
        }
    }
}

/// The payload as an object, cut down when it is too big for the wire. `None` for anything
/// that isn't a JSON object (or a too-big one whose event name can't be found).
fn parse_payload(input: &[u8]) -> Option<Value> {
    if input.len() <= HOOK_PAYLOAD_ROOM {
        return match serde_json::from_slice::<Value>(input) {
            Ok(v @ Value::Object(_)) => Some(v),
            _ => None,
        };
    }
    // Too big to forward whole, and perhaps cut off by the cap, so no whole parse: take the
    // kept fields as far as they come before the end.
    let mut kept = Map::new();
    let _ = serde_json::Deserializer::from_slice(input).deserialize_map(Pick(&mut kept));
    if !kept.contains_key("hook_event_name") {
        return None;
    }
    kept.insert("moa_cut".into(), Value::Bool(true));
    Some(Value::Object(kept))
}

/// Collects `KEPT` fields from an object, skipping the rest. The fields it has found stay
/// in the map even when the input ends early.
struct Pick<'a>(&'a mut Map<String, Value>);

impl<'de> Visitor<'de> for Pick<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if KEPT.contains(&key.as_str()) {
                let value = map.next_value::<Value>()?;
                self.0.insert(key, value);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// The brain this hook is for: `MOA_BRAIN` when set, else the one the folder and account
/// route to. `None` when there is nowhere to send it.
fn pick_brain(paths: &Paths, env: &HookEnv, payload: &Value) -> Option<BrainName> {
    if let Some(name) = &env.moa_brain {
        return BrainName::parse(name).ok();
    }
    let text = paths.read_config().ok()?;
    let config = Config::from_file_text(text.as_deref(), paths.home(), canonical).ok()?;
    let store = canonical(&caller_store(
        env.claude_config_dir.as_deref().map(OsStr::new),
        paths.home(),
    ));
    let cwd = canonical(Path::new(payload.get("cwd")?.as_str()?));
    match route(&config, config.account_for_store(&store), &cwd, None) {
        Route::Brain(b) => Some(b),
        Route::NotAvailable => None,
    }
}

/// Connects and writes the line; the stream when both worked.
fn send(socket: &Path, line: &[u8]) -> Option<UnixStream> {
    let mut stream = net::connect_within(socket, CONNECT).ok()?;
    stream.set_write_timeout(Some(WRITE)).ok()?;
    stream.write_all(line).ok()?;
    Some(stream)
}

/// The `context` of the daemon's reply, when it gives one in time.
fn read_context(stream: UnixStream) -> Option<String> {
    stream.set_read_timeout(Some(SESSION_START_WAIT)).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    let reply: Value = serde_json::from_str(&reply).ok()?;
    let text = reply.pointer("/result/context")?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

fn print_context(text: &str) {
    let text = Value::from(text);
    let _ = writeln!(
        std::io::stdout(),
        r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{text}}}}}"#
    );
}
