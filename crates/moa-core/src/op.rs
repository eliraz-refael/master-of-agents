//! What a client can ask the core to do, and what it gets back.

use std::path::PathBuf;

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
    #[serde(rename = "task.ls")]
    TaskLs {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "task.add")]
    TaskAdd(TaskAdd),
    #[serde(rename = "task.set")]
    TaskSet(TaskSet),
    #[serde(rename = "note.add")]
    NoteAdd {
        text: String,
        #[serde(default)]
        project: Option<String>,
        /// `None` is `fact`.
        #[serde(default)]
        kind: Option<String>,
    },
    #[serde(rename = "project.show")]
    ProjectShow { name: String },
    #[serde(rename = "project.add")]
    ProjectAdd {
        name: String,
        path: PathBuf,
        #[serde(default)]
        summary: Option<String>,
    },
}

/// `task add`'s arguments, all the user's raw text.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskAdd {
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub today: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub blocked_on: Option<String>,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

/// `task set`'s arguments. `Some("")` clears `due`, `priority`, `session`, `note` and
/// `blocked_on`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskSet {
    pub id: i64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub blocked_on: Option<String>,
    #[serde(default)]
    pub today: bool,
    #[serde(default)]
    pub not_today: bool,
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
        "task.ls",
        "task.add",
        "task.set",
        "note.add",
        "project.show",
        "project.add",
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
            Op::TaskLs { .. } => "task.ls",
            Op::TaskAdd(_) => "task.add",
            Op::TaskSet(_) => "task.set",
            Op::NoteAdd { .. } => "note.add",
            Op::ProjectShow { .. } => "project.show",
            Op::ProjectAdd { .. } => "project.add",
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
