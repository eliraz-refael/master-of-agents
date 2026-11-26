//! Requests in, replies and notices out: one JSON object per line.

use std::collections::BTreeMap;

use moa_core::op::{NoParams, Op};
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, TmuxLinkData};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The longest line either side accepts, in bytes.
pub const MAX_LINE: usize = 8 << 20;

/// A request id, echoed in its reply.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
}

/// A client's greeting: who it is, which build, and what it says about its caller.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Hello {
    pub client: String,
    pub build: String,
    pub caller: CallerInfo,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HelloOk {
    pub brain: String,
    pub build: String,
    pub rev: u64,
    pub matched: bool,
    pub pid: u32,
    /// The daemon's program: the path it runs, watches and upgrades into (`MOA_EXE`). A
    /// daemon from before live updates sends none.
    #[serde(default)]
    pub exe: Option<String>,
}

/// `upgrade`'s params. An old client's bare `upgrade` is `if_stale: false`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeParams {
    /// Upgrade only when the daemon's program is no longer the build it runs; otherwise the
    /// reply names that program.
    #[serde(default)]
    pub if_stale: bool,
}

/// `upgrade`'s reply. A daemon from before live updates answers `{}` and stops instead: a
/// reply this can't be read from (no `upgrading`) is that.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeOk {
    /// The daemon upgrades in place, once moa is between turns; its pid stays.
    pub upgrading: bool,
    /// The upgrade waits for moa's turn to end.
    #[serde(default)]
    pub waiting_for_moa: bool,
    /// When it doesn't upgrade: the program that runs this brain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusOk {
    pub brain: String,
    pub build: String,
    pub pid: u32,
    pub launches: u64,
    pub sessions: BTreeMap<String, usize>,
    pub caller: String,
    /// The terminal server, while linked.
    #[serde(default)]
    pub tmux: Option<TmuxLinkData>,
    #[serde(default)]
    pub tmux_restarts: u32,
    /// How many panels moa started or restarted while this daemon ran.
    #[serde(default)]
    pub panel_starts: u32,
    /// `waiting for moa's turn` while an upgrade waits; `null` otherwise.
    #[serde(default)]
    pub upgrade: Option<String>,
}

/// Which topics a connection follows. `since` is the rev the client already holds.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Subscribe {
    pub topics: Vec<Topic>,
    #[serde(default)]
    pub since: Option<u64>,
}

/// A refusal on the wire. `message` is one human line.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

pub mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const FAILED: i64 = -32000;
    pub const BUILD_MISMATCH: i64 = -32001;
    pub const HELLO_FIRST: i64 = -32002;
    pub const TOO_LONG: i64 = -32003;
}

fn err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
    }
}

/// What a request asks for: a control method or an op.
#[derive(Clone, PartialEq, Debug)]
pub enum Call {
    Hello(Hello),
    Status,
    Shutdown,
    Upgrade { if_stale: bool },
    Subscribe(Subscribe),
    Op(Op),
}

/// The schema documents the wire form of every method, `{"method": m, "params": ...}`, not
/// the Rust enum's shape. An op's forms come from `Op`, which is already tagged that way.
impl JsonSchema for Call {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Call".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let form = |method: &str, params: schemars::Schema, required: bool| -> Value {
            json!({
                "type": "object",
                "properties": { "method": { "const": method }, "params": params },
                "required": if required { json!(["method", "params"]) } else { json!(["method"]) },
            })
        };
        let none = g.subschema_for::<NoParams>();
        let forms = vec![
            form("hello", g.subschema_for::<Hello>(), true),
            form("status", none.clone(), false),
            form("shutdown", none, false),
            form("upgrade", g.subschema_for::<UpgradeParams>(), false),
            form("subscribe", g.subschema_for::<Subscribe>(), true),
            g.subschema_for::<Op>().to_value(),
        ];
        schemars::json_schema!({ "anyOf": forms })
    }
}

/// A line the server read.
// Decoded and matched at once, one per line; boxing would only complicate every match.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Debug)]
pub enum Incoming {
    Request {
        id: Id,
        call: Call,
    },
    /// The agent hook's message. `id` is `None` for a notification (no reply is due) and
    /// `Some` when the hook sent a request, which expects a reply.
    Hook {
        id: Option<Id>,
        params: Value,
    },
    /// A well-formed line the server has nothing to do with: a notification other than `hook`.
    Ignored,
}

/// Reads one line (no trailing newline).
///
/// - A notification (no `id`) is `Hook` for `hook` and `Ignored` for any other method, known
///   or not: unknown methods are additive (spec 6.4), and a known verb sent without an id
///   has nowhere to send its answer, so it is dropped rather than run.
/// - A request whose `id` is `null`, or neither a number nor a string, is `INVALID_REQUEST`.
/// - `hook` with an id is `Hook` with that id kept, so the server can reply. Slice 4 sends
///   the hook's SessionStart as a request, allowed before `hello`; nothing here forbids that.
pub fn decode_incoming(line: &str) -> Result<Incoming, (Option<Id>, RpcError)> {
    let value: Value =
        serde_json::from_str(line).map_err(|_| (None, err(code::PARSE, "that isn't JSON")))?;
    if value.is_array() {
        return Err((None, err(code::INVALID_REQUEST, "batches aren't supported")));
    }
    let not_rpc = |id: Option<Id>| {
        (
            id,
            err(code::INVALID_REQUEST, "that isn't a JSON-RPC 2.0 message"),
        )
    };
    let Some(obj) = value.as_object() else {
        return Err(not_rpc(None));
    };
    let id = match obj.get("id") {
        None => None,
        Some(v) => match serde_json::from_value::<Id>(v.clone()) {
            Ok(id) => Some(id),
            Err(_) => {
                return Err((
                    None,
                    err(
                        code::INVALID_REQUEST,
                        "a request id is a number or a string",
                    ),
                ));
            }
        },
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(not_rpc(id));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Err(not_rpc(id));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    if method == "hook" {
        return Ok(Incoming::Hook { id, params });
    }
    let Some(id) = id else {
        return Ok(Incoming::Ignored);
    };
    let bad = |e: serde_json::Error| {
        let first = e.to_string();
        let first = first.lines().next().unwrap_or_default().to_string();
        (
            Some(id.clone()),
            err(
                code::INVALID_PARAMS,
                format!("the parameters for {method} don't fit: {first}"),
            ),
        )
    };
    let empty = |params: Value| serde_json::from_value::<NoParams>(params).map_err(bad);
    let call = match method {
        "hello" => Call::Hello(serde_json::from_value(params).map_err(bad)?),
        "subscribe" => Call::Subscribe(serde_json::from_value(params).map_err(bad)?),
        "status" => empty(params).map(|_| Call::Status)?,
        "shutdown" => empty(params).map(|_| Call::Shutdown)?,
        // Missing and `null` params are the bare `upgrade` an old client sends.
        "upgrade" => {
            let p: UpgradeParams = match params {
                Value::Null => UpgradeParams::default(),
                params => serde_json::from_value(params).map_err(bad)?,
            };
            Call::Upgrade {
                if_stale: p.if_stale,
            }
        }
        m if Op::METHODS.contains(&m) => {
            Call::Op(serde_json::from_value(json!({"method": m, "params": params})).map_err(bad)?)
        }
        m => {
            return Err((
                Some(id),
                err(
                    code::METHOD_NOT_FOUND,
                    format!("moa doesn't know the method {m}"),
                ),
            ));
        }
    };
    Ok(Incoming::Request { id, call })
}

/// Whether a connection whose build differs from the daemon's may make this call.
pub fn allowed_on_mismatch(call: &Call) -> bool {
    matches!(
        call,
        Call::Hello(_) | Call::Status | Call::Upgrade { .. } | Call::Shutdown
    )
}

fn line(v: Value) -> String {
    // JSON text escapes newlines, so this is always one line.
    v.to_string()
}

/// A success reply, as one line without its newline.
pub fn encode_result(id: &Id, result: &impl Serialize) -> String {
    let result = serde_json::to_value(result).unwrap_or(Value::Null);
    line(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// An error reply; `id` is `None` when the request's id could not be read.
pub fn encode_error(id: Option<&Id>, e: &RpcError) -> String {
    line(json!({"jsonrpc": "2.0", "id": id, "error": e}))
}

/// A server push with no id: `patch`, `snapshot` or `event`.
pub fn encode_notice(method: &str, params: &impl Serialize) -> String {
    let params = serde_json::to_value(params).unwrap_or(Value::Null);
    line(json!({"jsonrpc": "2.0", "method": method, "params": params}))
}

/// A line the client read.
#[derive(Clone, PartialEq, Debug)]
pub enum ServerLine {
    Result { id: Id, value: Value },
    Error { id: Option<Id>, error: RpcError },
    Notice { method: String, params: Value },
}

/// Reads one server line (the client side).
pub fn decode_server_line(line: &str) -> Result<ServerLine, RpcError> {
    let v: Value = serde_json::from_str(line).map_err(|_| err(code::PARSE, "that isn't JSON"))?;
    let not_rpc = || err(code::INVALID_REQUEST, "that isn't a JSON-RPC 2.0 message");
    let obj = v.as_object().ok_or_else(not_rpc)?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(not_rpc());
    }
    let id = obj
        .get("id")
        .and_then(|i| serde_json::from_value::<Id>(i.clone()).ok());
    if let Some(method) = obj.get("method").and_then(Value::as_str) {
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return Ok(ServerLine::Notice {
            method: method.to_string(),
            params,
        });
    }
    if let Some(e) = obj.get("error") {
        let error = serde_json::from_value(e.clone()).map_err(|_| not_rpc())?;
        return Ok(ServerLine::Error { id, error });
    }
    match (id, obj.get("result")) {
        (Some(id), Some(value)) => Ok(ServerLine::Result {
            id,
            value: value.clone(),
        }),
        _ => Err(not_rpc()),
    }
}
