//! What the core and the host of moa's agent say to each other, in moa's own types: the host
//! turns the agent's protocol into these, so nothing past it names the protocol's library.
//! Every message carries the agent's generation, one per start, so the core can tell a stale
//! message from a live one. (It is `generation`, not `gen`: Rust 2024 reserves `gen`.)

use std::path::PathBuf;

/// What the agent offers, from its answer to `initialize`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Caps {
    /// It can replay a thread (`session/load`).
    pub load: bool,
    /// It can reopen a thread without replaying it (`session/resume`).
    pub resume: bool,
    /// It takes a system prompt when a thread is opened.
    pub system_prompt: bool,
}

/// How to start the agent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdapterSpec {
    /// The agent's program (an absolute path).
    pub program: PathBuf,
    /// Set in the agent's environment, over this process's own.
    pub env: Vec<(String, String)>,
    /// Removed from the agent's environment.
    pub unset: Vec<String>,
    /// The agent dir: the agent runs there, and the host writes its files there.
    pub cwd: PathBuf,
    /// The text of `<cwd>/.claude/settings.json`.
    pub settings: String,
    /// What `<cwd>/bin/moactl` links to.
    pub moactl: PathBuf,
}

/// How to open a thread.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ThreadHow {
    /// A new thread. The system prompt goes with it when the agent takes one; otherwise the
    /// text goes into the thread's folder's `CLAUDE.md`. Any `CLAUDE.md` there is removed
    /// when `claude_md` is `None`.
    New {
        system_prompt: Option<String>,
        claude_md: Option<String>,
    },
    /// An earlier thread, reopened without its history.
    Resume { thread: String },
    /// An earlier thread, its history replayed as updates first.
    Load { thread: String },
}

/// How a thread was opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opened {
    New,
    Resumed,
    Loaded,
}

/// The core's commands to the host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AcpCmd {
    Start {
        generation: u64,
        spec: AdapterSpec,
    },
    Open {
        generation: u64,
        how: ThreadHow,
        cwd: PathBuf,
    },
    /// `prompt` is the core's own number for the turn; `TurnEnded` carries it back.
    Prompt {
        generation: u64,
        thread: String,
        prompt: u64,
        text: String,
    },
    Cancel {
        generation: u64,
        thread: String,
    },
    /// Answers permission request `req`: the chosen option's id, or `None` to cancel it.
    Permission {
        generation: u64,
        req: u64,
        choice: Option<String>,
    },
    Stop {
        generation: u64,
    },
}

impl AcpCmd {
    /// The run of the agent it is for.
    pub fn generation(&self) -> u64 {
        match self {
            AcpCmd::Start { generation, .. }
            | AcpCmd::Open { generation, .. }
            | AcpCmd::Prompt { generation, .. }
            | AcpCmd::Cancel { generation, .. }
            | AcpCmd::Permission { generation, .. }
            | AcpCmd::Stop { generation } => *generation,
        }
    }

    /// Whether it waits for the commit before it goes: a prompt, whose `you` row is saved
    /// first, and your answer to a permission request, whose row says so first. A cancelled
    /// request's `None` goes at once: the protocol needs it, and nothing waits on its row.
    pub fn held(&self) -> bool {
        matches!(
            self,
            AcpCmd::Prompt { .. }
                | AcpCmd::Permission {
                    choice: Some(_),
                    ..
                }
        )
    }
}

/// What the host tells the core.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AcpIn {
    Started {
        generation: u64,
        caps: Caps,
    },
    /// The thread's id and how it was opened, or why it couldn't be.
    Opened {
        generation: u64,
        result: Result<(String, Opened), String>,
    },
    Update {
        generation: u64,
        thread: String,
        update: Update,
    },
    /// The agent asks before it goes on; answered by `AcpCmd::Permission` with this `req`.
    Permission {
        generation: u64,
        req: u64,
        thread: String,
        title: String,
        options: Vec<PermOption>,
    },
    /// Turn `prompt` ended: how, or why it failed (the agent went away, say).
    TurnEnded {
        generation: u64,
        prompt: u64,
        result: Result<StopReason, String>,
    },
    /// The agent is gone. Said once per generation.
    Exited {
        generation: u64,
        why: String,
    },
}

impl AcpIn {
    /// The run of the agent that said it.
    pub fn generation(&self) -> u64 {
        match self {
            AcpIn::Started { generation, .. }
            | AcpIn::Opened { generation, .. }
            | AcpIn::Update { generation, .. }
            | AcpIn::Permission { generation, .. }
            | AcpIn::TurnEnded { generation, .. }
            | AcpIn::Exited { generation, .. } => *generation,
        }
    }
}

/// One update from the agent, in a thread.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Update {
    /// A piece of moa's answer. Content that isn't text is `[image]`, `[audio]` or
    /// `[resource]`.
    Message { id: Option<String>, text: String },
    /// A piece of moa's thinking.
    Thought { id: Option<String>, text: String },
    /// A tool call starts. `kind` and `status` are the protocol's names (`read`, `pending`);
    /// `input` is the tool's input as JSON.
    Tool {
        id: String,
        title: String,
        kind: String,
        status: String,
        input: Option<String>,
    },
    /// A tool call changed.
    ToolUpdate {
        id: String,
        title: Option<String>,
        status: Option<String>,
        /// The tool's input as JSON, when the update gives it: Claude's adapter starts a
        /// call with `{}` and fills it in here.
        input: Option<String>,
    },
    /// The whole plan, as it is now.
    Plan { entries: Vec<PlanEntry> },
    /// The agent repeating what was said to it.
    UserEcho,
    /// Anything else, by the protocol's name for it (`usage_update`, ...).
    Other(&'static str),
}

/// One step of moa's plan, with the protocol's names for its status and priority.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
    pub priority: String,
}

/// One answer a permission request offers: answered by its `id`, chosen by its `kind`, and
/// shown by its `name`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PermOption {
    pub id: String,
    pub kind: PermKind,
    pub name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    /// A kind this build doesn't know.
    Other,
}

impl PermKind {
    /// The protocol's name for it (`allow_once`, ...); `other` for a kind this build doesn't
    /// know.
    pub fn name(self) -> &'static str {
        match self {
            PermKind::AllowOnce => "allow_once",
            PermKind::AllowAlways => "allow_always",
            PermKind::RejectOnce => "reject_once",
            PermKind::RejectAlways => "reject_always",
            PermKind::Other => "other",
        }
    }
}

/// Why a turn ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// A reason this build doesn't know, by its name.
    Other(String),
}

impl StopReason {
    /// The protocol's name for it (`end_turn`, `max_tokens`, ...).
    pub fn name(&self) -> &str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::MaxTurnRequests => "max_turn_requests",
            StopReason::Refusal => "refusal",
            StopReason::Cancelled => "cancelled",
            StopReason::Other(name) => name,
        }
    }
}
