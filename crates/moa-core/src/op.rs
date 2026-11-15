//! What a client can ask the core to do, and what it gets back.

use serde::{Deserialize, Serialize};

/// A client connection, as the daemon numbers them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConnId(pub u64);

/// A request on a connection.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ReqId(pub u64);

/// One verb with its arguments. On the wire: `{"method": "group.add", "params": {...}}`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Op {
    #[serde(rename = "group.add")]
    GroupAdd { path: String },
    #[serde(rename = "group.rename")]
    GroupRename { old: String, new: String },
    #[serde(rename = "group.rm")]
    GroupRm { path: String },
    #[serde(rename = "session.ls")]
    SessionLs {
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "session.set")]
    SessionSet {
        q: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        group: Option<String>,
    },
    #[serde(rename = "session.archive")]
    SessionArchive { qs: Vec<String> },
}

impl Op {
    /// Every method name, in variant order. A test ties this to the variants.
    pub const METHODS: &'static [&'static str] = &[
        "group.add",
        "group.rename",
        "group.rm",
        "session.ls",
        "session.set",
        "session.archive",
    ];

    /// This op's method name.
    pub fn method(&self) -> &'static str {
        match self {
            Op::GroupAdd { .. } => "group.add",
            Op::GroupRename { .. } => "group.rename",
            Op::GroupRm { .. } => "group.rm",
            Op::SessionLs { .. } => "session.ls",
            Op::SessionSet { .. } => "session.set",
            Op::SessionArchive { .. } => "session.archive",
        }
    }
}

/// A successful reply: the line the CLI prints and the data a script reads.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Done {
    pub text: String,
    pub data: serde_json::Value,
}

/// A refusal, as one human line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpError(pub String);
