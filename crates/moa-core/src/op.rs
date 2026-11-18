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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    #[serde(rename = "session.find")]
    SessionFind { q: String },
    /// A fresh session in a new tab of moa's terminal. `path` is absolute; the reply comes
    /// once tmux says which pane the tab has. The tab comes to the screen unless `bg`.
    #[serde(rename = "session.open")]
    SessionOpen {
        path: PathBuf,
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        bg: bool,
    },
    /// Brings a session's tab to the screen. A write: a session not running is resumed.
    #[serde(rename = "session.focus")]
    SessionFocus { q: String },
    /// Relaunches a session's conversation in a new tab, under the account whose store it
    /// was recorded in (C11); one that runs is only brought to the screen. The tab comes to
    /// the screen unless `bg`.
    #[serde(rename = "session.resume")]
    SessionResume {
        q: String,
        #[serde(default)]
        bg: bool,
    },
    /// Ends a session and kills its tab, once moa can prove the tab is the session's.
    #[serde(rename = "session.close")]
    SessionClose { q: String },
    /// Puts sessions to sleep: their tabs close and they stay resumable. By name (`qs`), or
    /// every one idle `idle_ms` that could sleep on its own. A `dry_run` only says which.
    #[serde(rename = "session.sleep")]
    SessionSleep {
        #[serde(default)]
        qs: Vec<String>,
        #[serde(default)]
        idle_ms: Option<i64>,
        #[serde(default)]
        dry_run: bool,
    },
    /// A plain shell in a new tab of moa's terminal, in folder `path` (absolute). The tab is
    /// named after `title`, else the folder.
    #[serde(rename = "term.open")]
    TermOpen {
        path: PathBuf,
        #[serde(default)]
        title: Option<String>,
    },
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
    #[serde(rename = "brief")]
    Brief(NoParams),
    #[serde(rename = "attention")]
    Attention(NoParams),
    #[serde(rename = "suggest")]
    Suggest {
        text: String,
        /// Each is a moactl action, as typed: `task set 1 --status done`.
        #[serde(default)]
        actions: Vec<String>,
    },
    #[serde(rename = "suggestion.answer")]
    SuggestionAnswer {
        #[serde(default)]
        id: Option<i64>,
        answer: Answer,
    },
    #[serde(rename = "suggestion.ls")]
    SuggestionLs(NoParams),
    /// The front door's handshake: answers `{"running": bool}` once moa's terminal server is
    /// linked and has its `moa` session, or when there is no server. `tmux` is the
    /// absolute path of the tmux program the front door found.
    #[serde(rename = "tmux.ensure")]
    TmuxEnsure {
        #[serde(default)]
        tmux: Option<PathBuf>,
    },
    /// Panics when run, so tests can see the core survive it. Not on the wire: it is not in
    /// [`Op::METHODS`] or the schema, and a client can't send it.
    #[doc(hidden)]
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    DebugPanic,
}

/// How a suggestion is answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Yes,
    No,
    Later,
}

/// The params of an op that takes none. Missing, `null` and `{}` all decode (a unit variant
/// would reject `{}`); it encodes as `{}`. Anything else is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NoParams;

/// Derived, the schema would say `null`; the wire is an empty object (or missing, or `null`).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for NoParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NoParams".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": ["object", "null"],
            "properties": {},
            "additionalProperties": false
        })
    }
}

impl Serialize for NoParams {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        s.serialize_map(Some(0))?.end()
    }
}

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Empty {}
        // A missing `params` arrives as `None` (serde's missing-field deserializer).
        Option::<Empty>::deserialize(d).map(|_| NoParams)
    }
}

/// `task add`'s arguments, all the user's raw text.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Every method name on the wire, in variant order (`DebugPanic` is not one). A test ties
    /// this to the variants.
    pub const METHODS: &'static [&'static str] = &[
        "group.add",
        "group.rename",
        "group.rm",
        "session.ls",
        "session.set",
        "session.archive",
        "session.find",
        "session.open",
        "session.focus",
        "session.resume",
        "session.close",
        "session.sleep",
        "term.open",
        "task.ls",
        "task.add",
        "task.set",
        "note.add",
        "project.show",
        "project.add",
        "brief",
        "attention",
        "suggest",
        "suggestion.answer",
        "suggestion.ls",
        "tmux.ensure",
    ];

    /// Whether this op changes the brain. Exhaustive on purpose: a new op has to be
    /// classified (`moactl` refuses writes from an account `moa.toml` doesn't list).
    pub fn writes(&self) -> bool {
        match self {
            Op::GroupAdd { .. }
            | Op::GroupRename { .. }
            | Op::GroupRm { .. }
            | Op::SessionSet { .. }
            | Op::SessionArchive { .. }
            | Op::SessionOpen { .. }
            | Op::SessionFocus { .. }
            | Op::SessionResume { .. }
            | Op::SessionClose { .. }
            | Op::SessionSleep { .. }
            | Op::TaskAdd(_)
            | Op::TaskSet(_)
            | Op::NoteAdd { .. }
            | Op::ProjectAdd { .. }
            | Op::Suggest { .. }
            | Op::SuggestionAnswer { .. } => true,
            Op::SessionLs { .. }
            | Op::SessionFind { .. }
            | Op::TaskLs { .. }
            | Op::ProjectShow { .. }
            | Op::Brief(_)
            | Op::Attention(_)
            | Op::SuggestionLs(_)
            | Op::TmuxEnsure { .. }
            | Op::TermOpen { .. }
            | Op::DebugPanic => false,
        }
    }

    /// This op's method name.
    pub fn method(&self) -> &'static str {
        match self {
            Op::GroupAdd { .. } => "group.add",
            Op::GroupRename { .. } => "group.rename",
            Op::GroupRm { .. } => "group.rm",
            Op::SessionLs { .. } => "session.ls",
            Op::SessionSet { .. } => "session.set",
            Op::SessionArchive { .. } => "session.archive",
            Op::SessionFind { .. } => "session.find",
            Op::SessionOpen { .. } => "session.open",
            Op::SessionFocus { .. } => "session.focus",
            Op::SessionResume { .. } => "session.resume",
            Op::SessionClose { .. } => "session.close",
            Op::SessionSleep { .. } => "session.sleep",
            Op::TermOpen { .. } => "term.open",
            Op::TaskLs { .. } => "task.ls",
            Op::TaskAdd(_) => "task.add",
            Op::TaskSet(_) => "task.set",
            Op::NoteAdd { .. } => "note.add",
            Op::ProjectShow { .. } => "project.show",
            Op::ProjectAdd { .. } => "project.add",
            Op::Brief(_) => "brief",
            Op::Attention(_) => "attention",
            Op::Suggest { .. } => "suggest",
            Op::SuggestionAnswer { .. } => "suggestion.answer",
            Op::SuggestionLs(_) => "suggestion.ls",
            Op::TmuxEnsure { .. } => "tmux.ensure",
            Op::DebugPanic => "debug.panic",
        }
    }
}

/// A successful reply: the line the CLI prints and the data a script reads. A client decodes
/// it, so it round-trips.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Done {
    pub text: String,
    pub data: serde_json::Value,
}

/// A refusal, as one human line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpError(pub String);
