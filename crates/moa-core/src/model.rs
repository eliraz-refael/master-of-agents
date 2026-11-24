//! The v1 domain model: ids, closed enums, open kinds and one row struct per table.
//! Plain data and parsing only. moa-core never generates ids.

use std::fmt;
use std::str::FromStr;

use jiff::civil::Date;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::time::UtcMs;

/// A text value was not one of the known values. Names the kind of value, never the value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("not a known {what}")]
pub struct UnknownValue {
    pub what: &'static str,
}

/// The text was not a session id. Carries no input.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("not a session id")]
pub struct BadSessionId;

// ---------- ids ----------

/// A session's id: a uuid, shown hyphenated and lowercase.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct SessionId(pub uuid::Uuid);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl FromStr for SessionId {
    type Err = BadSessionId;
    fn from_str(s: &str) -> Result<Self, BadSessionId> {
        uuid::Uuid::parse_str(s)
            .map(SessionId)
            .map_err(|_| BadSessionId)
    }
}

macro_rules! int_ids {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    )*};
}

int_ids!(
    ProjectId,
    TaskId,
    NoteId,
    SuggestionId,
    AskId,
    EventId,
    ActivityId,
    TermId
);

// ---------- closed enums ----------

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident, $test:ident, $what:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl FromStr for $name {
            type Err = UnknownValue;
            fn from_str(s: &str) -> Result<Self, UnknownValue> {
                match s {
                    $($text => Ok($name::$variant),)+
                    _ => Err(UnknownValue { what: $what }),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        #[cfg(test)]
        #[test]
        fn $test() {
            for v in $name::ALL {
                assert_eq!(v.as_str().parse::<$name>(), Ok(*v));
                assert_eq!(v.to_string(), v.as_str());
                let json = serde_json::to_string(v).unwrap();
                assert_eq!(serde_json::from_str::<$name>(&json).unwrap(), *v);
            }
            assert!("".parse::<$name>().is_err());
        }
    };
}

str_enum!(ProjectStatus, project_status_round_trips, "project status",
    { Idea => "idea", Active => "active", Paused => "paused", Done => "done" });
str_enum!(TaskStatus, task_status_round_trips, "task status", {
    Idea => "idea", Todo => "todo", Active => "active", Waiting => "waiting",
    Blocked => "blocked", Done => "done", Dropped => "dropped" });

impl TaskStatus {
    /// Not finished: neither done nor dropped.
    pub fn is_open(self) -> bool {
        !matches!(self, TaskStatus::Done | TaskStatus::Dropped)
    }
}
str_enum!(Who, who_round_trips, "who", { You => "you", Agent => "agent", Other => "other" });
str_enum!(Priority, priority_round_trips, "priority", { P0 => "p0", P1 => "p1", P2 => "p2" });
str_enum!(SuggestionStatus, suggestion_status_round_trips, "suggestion status",
    { Open => "open", Accepted => "accepted", Rejected => "rejected", Later => "later" });
str_enum!(AgentKind, agent_kind_round_trips, "agent kind", { Claude => "claude", Codex => "codex" });
str_enum!(Lifecycle, lifecycle_round_trips, "lifecycle", { Live => "live", Ended => "ended" });
str_enum!(Activity, activity_round_trips, "activity", {
    Working => "working", Idle => "idle", NeedsPermission => "needs_permission",
    NeedsAnswer => "needs_answer", Failed => "failed" });
str_enum!(Location, location_round_trips, "location", { Here => "here", Elsewhere => "elsewhere" });
str_enum!(AskStatus, ask_status_round_trips, "ask status", {
    Pending => "pending", Sent => "sent", Received => "received", Answered => "answered",
    Failed => "failed", Dropped => "dropped" });
str_enum!(ActivityWho, activity_who_round_trips, "activity who",
    { Brain => "brain", Session => "session", You => "you" });
str_enum!(ConversationReason, conversation_reason_round_trips, "conversation reason", {
    Start => "start", Clear => "clear", Fork => "fork", Resume => "resume", Import => "import",
    Adopt => "adopt" });
str_enum!(ThreadKind, thread_kind_round_trips, "thread kind", { Daily => "daily", Manual => "manual" });
str_enum!(
    /// What a session report says: a turn that ended, or a session that needs you.
    ReportKind, report_kind_round_trips, "report kind",
    { TurnEnded => "turn_ended", NeedsYou => "needs_you" });
str_enum!(
    /// Who opened a terminal: you, moa at your request, or nobody moa knows of (`found`: moa
    /// first saw it already open).
    OpenedBy, opened_by_round_trips, "opened by",
    { You => "you", Moa => "moa", Found => "found" });

// ---------- group paths ----------

/// Trims `/` and drops empty segments (`A//B/` becomes `A/B`).
pub fn normalize_group_path(raw: &str) -> String {
    raw.split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

// ---------- open kinds ----------

macro_rules! open_kind {
    ($(#[$meta:meta])* $name:ident, { $($konst:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            $(pub const $konst: &'static str = $text;)+
            pub const KNOWN: &'static [&'static str] = &[$($text),+];

            pub fn is_known(&self) -> bool {
                Self::KNOWN.contains(&self.0.as_str())
            }
        }
    };
}

open_kind!(
    /// The real data has kinds outside this list (such as `milestone`), so the value is kept as text.
    NoteKind,
    { FACT = "fact", PREFERENCE = "preference", ROUTINE = "routine",
      OBSERVATION = "observation", THREAD_SUMMARY = "thread-summary" }
);

open_kind!(
    /// Event kinds. `IDLE_PING` is Claude's "waiting for your input" notification: it never means "needs you".
    EventKind,
    { SESSION_STARTED = "session.started", SESSION_ENDED = "session.ended",
      TURN_STARTED = "turn.started", TURN_ENDED = "turn.ended", TURN_FAILED = "turn.failed",
      TURN_INTERRUPTED = "turn.interrupted", NEEDS_PERMISSION = "needs.permission",
      NEEDS_ANSWER = "needs.answer", NEEDS_CLEARED = "needs.cleared",
      CWD_CHANGED = "cwd.changed", MODEL_CHANGED = "model.changed", UI = "ui",
      IDLE_PING = "idle.ping" }
);

// ---------- transcripts ----------

/// The result of looking for an agent conversation's transcript file. `UnsafeId` means the
/// conversation id could not be used as a file name; `NotChecked` means no check was made.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TranscriptCheck {
    Present(std::path::PathBuf),
    Missing,
    UnsafeId,
    NotChecked,
}

/// Whether a conversation id is one moa can resume, and safe as a file name:
/// `^[A-Za-z0-9][A-Za-z0-9-]{0,63}$` (the rule `moa_agents::claude::find_transcript` checks
/// by). A leading `-` would read as a flag after `--resume` (R13 rule 5).
pub fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Claude's project folder name for a cwd: every character that isn't ASCII alphanumeric
/// becomes `-`. The core's copy of `moa_agents::claude::project_slot`, which a test keeps
/// equal to it: resume tells by it which folder a transcript belongs to.
pub fn project_slot(cwd: &std::path::Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// ---------- suggestions and asks ----------

pub const ACTION_VERBS: &[&str] = &[
    "task", "set", "archive", "close", "group", "note", "project",
];

/// One proposed action of a suggestion, such as verb `task` with its arguments.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SuggestionAction {
    pub verb: String,
    pub args: Vec<String>,
}

/// Who an ask is for: moa itself or one session. Stored as `"moa"` or the session uuid.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AskTarget {
    Moa,
    Session(SessionId),
}

impl fmt::Display for AskTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AskTarget::Moa => f.write_str("moa"),
            AskTarget::Session(id) => fmt::Display::fmt(id, f),
        }
    }
}

impl FromStr for AskTarget {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, UnknownValue> {
        if s == "moa" {
            return Ok(AskTarget::Moa);
        }
        s.parse::<SessionId>()
            .map(AskTarget::Session)
            .map_err(|_| UnknownValue { what: "ask target" })
    }
}

impl Serialize for AskTarget {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AskTarget {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------- rows (field names equal column names) ----------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: Option<String>,
    pub status: ProjectStatus,
    pub summary: Option<String>,
    pub last_activity_on: Option<Date>,
    pub created_at: UtcMs,
    pub updated_at: UtcMs,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub project_id: Option<ProjectId>,
    pub title: String,
    pub status: TaskStatus,
    pub blocked_on: Option<String>,
    pub today: Option<Date>,
    pub session_id: Option<SessionId>,
    pub note: Option<String>,
    pub who: Option<Who>,
    pub due: Option<Date>,
    pub priority: Option<Priority>,
    pub created_at: UtcMs,
    pub updated_at: UtcMs,
    pub done_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub project_id: Option<ProjectId>,
    pub kind: NoteKind,
    pub text: String,
    pub created_at: UtcMs,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: SuggestionId,
    pub text: String,
    pub status: SuggestionStatus,
    pub actions: Vec<SuggestionAction>,
    pub actions_raw: Option<String>,
    pub actions_invalid: Option<String>,
    pub result: Option<String>,
    pub created_at: UtcMs,
    pub resolved_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent_kind: AgentKind,
    pub account: Option<String>,
    pub cwd: Option<String>,
    pub start_cwd: Option<String>,
    pub project_id: Option<ProjectId>,
    pub title: Option<String>,
    pub title_locked: bool,
    pub group_path: Option<String>,
    pub lifecycle: Lifecycle,
    pub activity: Activity,
    pub activity_at: Option<UtcMs>,
    pub activity_why: Option<String>,
    pub last_prompt: Option<String>,
    pub last_turn_end_at: Option<UtcMs>,
    pub seen_at: Option<UtcMs>,
    pub asleep_at: Option<UtcMs>,
    pub archived_at: Option<UtcMs>,
    pub location: Location,
    pub resume_failed: bool,
    pub created_at: UtcMs,
    pub last_seen_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub agent_kind: AgentKind,
    pub conversation_id: String,
    pub session_id: SessionId,
    pub store_dir: Option<String>,
    pub spike_account: Option<String>,
    pub reason: ConversationReason,
    pub first_seen_at: UtcMs,
    pub last_seen_at: UtcMs,
    pub transcript_path: Option<String>,
    pub transcript_present: Option<bool>,
    pub transcript_checked_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PaneBinding {
    pub session_id: SessionId,
    pub server_pid: i64,
    pub server_start: i64,
    pub socket: String,
    pub pane_id: String,
    pub pane_pid: i64,
    pub bound_at: UtcMs,
    /// moa started the agent in this pane, so its hooks reach moa. `false` for an agent moa
    /// adopted: it shows alive or dead only, and never sleeps on its own.
    pub tracked: bool,
}

/// A shell in moa's tmux that isn't an agent or a panel. tmux reuses `%N` across servers, so
/// the server's generation is part of what names the pane. A closed terminal's row stays, as
/// history; only open ones are loaded.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Terminal {
    pub id: TermId,
    pub server_pid: i64,
    pub server_start: i64,
    pub pane_id: String,
    pub opened_by: OpenedBy,
    pub cwd: String,
    /// The session it belongs to: for a split, the agent whose tab it was split from.
    pub session_id: Option<SessionId>,
    pub opened_at: UtcMs,
    /// When you last touched it.
    pub touched_at: Option<UtcMs>,
    pub name: String,
    pub note: Option<String>,
    /// When moa closes it, if you set a timeout.
    pub timeout_at: Option<UtcMs>,
    pub shell: String,
    /// What runs in it now (tmux's current command), and since when moa first saw it.
    pub command: String,
    pub command_since: UtcMs,
    pub closed_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpikePane {
    pub session_id: SessionId,
    pub pane_id: String,
    pub tmux: Option<String>,
    pub imported_at: UtcMs,
}

/// The agent a pane runs, as its process and start command show it (`Deps::inspect`). Of
/// the process's environment only `CLAUDE_CONFIG_DIR` (`store_dir`) and the `MOA_*` keys are
/// kept.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneAgent {
    pub kind: AgentKind,
    pub store_dir: Option<String>,
    pub conversation: Option<String>,
    pub moa_session: Option<String>,
    pub moa_brain: Option<String>,
    pub title: Option<String>,
    pub group: Option<String>,
    /// When the agent's process started.
    pub started_at: Option<UtcMs>,
}

/// What a look at a tab's processes found (`Deps::inspect`). moa takes an agent for gone only
/// when that is proven: a read that failed proves nothing, and the next look decides.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Inspected {
    /// An agent runs in the tab.
    Agent(PaneAgent),
    /// No agent runs there: the tab's process and its children were all read (or are
    /// another user's, which no agent of ours is) and none is one, or the tab's process
    /// doesn't exist.
    Gone,
    /// A read that mattered failed (a process mid-exit, a zombie, a refused list).
    Unknown,
}

impl Inspected {
    /// The agent, when one was found.
    pub fn agent(&self) -> Option<&PaneAgent> {
        match self {
            Inspected::Agent(a) => Some(a),
            Inspected::Gone | Inspected::Unknown => None,
        }
    }

    /// The agent, when one was found; `Unknown` counts as none, as a refusal that needs an
    /// agent reads it.
    pub fn into_agent(self) -> Option<PaneAgent> {
        match self {
            Inspected::Agent(a) => Some(a),
            Inspected::Gone | Inspected::Unknown => None,
        }
    }
}

/// A read that found the agent, or proved there is none.
impl From<Option<PaneAgent>> for Inspected {
    fn from(agent: Option<PaneAgent>) -> Inspected {
        agent.map_or(Inspected::Gone, Inspected::Agent)
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub at: UtcMs,
    pub session_id: Option<SessionId>,
    pub conversation_id: Option<String>,
    pub account: Option<String>,
    pub cwd: Option<String>,
    pub kind: EventKind,
    pub text: Option<String>,
    pub detail: Option<serde_json::Value>,
}

/// A row of the `activity` table (named so it does not clash with the [`Activity`] enum).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ActivityRow {
    pub id: ActivityId,
    pub at: UtcMs,
    pub who: ActivityWho,
    pub text: String,
    pub ref_kind: Option<String>,
    pub ref_id: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Group {
    pub path: String,
    pub collapsed: bool,
    pub created_at: UtcMs,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ask {
    pub id: AskId,
    pub at: UtcMs,
    /// None when the sender is gone.
    pub from_session: Option<AskTarget>,
    pub from_name: Option<String>,
    /// None means the target is gone: a failed ask kept for history.
    pub to_session: Option<AskTarget>,
    pub text: String,
    pub status: AskStatus,
    pub reply: Option<String>,
    pub claimed_at: Option<UtcMs>,
    pub answered_at: Option<UtcMs>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MoaThread {
    pub thread_id: String,
    pub account: String,
    pub day: Date,
    pub kind: ThreadKind,
    pub started_at: UtcMs,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FeedEntry {
    pub id: i64,
    pub thread_id: String,
    pub at: UtcMs,
    pub kind: String,
    pub message_id: Option<String>,
    pub automatic: bool,
    pub body: String,
    pub state: Option<String>,
}

/// What one session has to tell moa's agent, kept until a wake-up carries it. A session has at
/// most one row of each kind.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    pub id: i64,
    pub session_id: SessionId,
    pub kind: ReportKind,
    /// When the first of its text was queued: its place in line, which later text keeps.
    pub queued_at: UtcMs,
    /// When the newest text was queued.
    pub last_at: UtcMs,
    /// The session's last messages, oldest first; a `needs_you` row holds only its newest.
    pub turns: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Nag {
    pub task_id: TaskId,
    pub due: Date,
    pub day: Date,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn task_status_parses_exactly_its_strings(s in "[a-z_]{0,12}") {
            prop_assert_eq!(s.parse::<TaskStatus>().is_ok(), TaskStatus::ALL.iter().any(|v| v.as_str() == s));
        }
    }

    #[test]
    fn unknown_value_names_the_enum_not_the_value() {
        assert_eq!(
            "someday".parse::<TaskStatus>().unwrap_err().to_string(),
            "not a known task status"
        );
    }

    #[test]
    fn open_kinds_keep_unknown_values() {
        assert!(!NoteKind("milestone".into()).is_known());
        assert!(NoteKind(NoteKind::FACT.into()).is_known());
        assert_eq!(NoteKind::THREAD_SUMMARY, "thread-summary");
        assert!(EventKind(EventKind::IDLE_PING.into()).is_known());
        assert_eq!(EventKind::IDLE_PING, "idle.ping");
        assert!(!EventKind("x.y".into()).is_known());
    }

    #[test]
    fn group_paths_normalize() {
        assert_eq!(normalize_group_path("A/B"), "A/B");
        assert_eq!(normalize_group_path("//A///B//"), "A/B");
        assert_eq!(normalize_group_path("/"), "");
    }

    #[test]
    fn rows_round_trip_through_json() {
        let g = Group {
            path: "Lantern/Tidepool".into(),
            collapsed: true,
            created_at: UtcMs(5),
        };
        let back: Group = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        let n = Nag {
            task_id: TaskId(3),
            due: jiff::civil::date(2026, 11, 20),
            day: jiff::civil::date(2026, 11, 16),
        };
        let j = serde_json::to_value(n).unwrap();
        assert_eq!(j["due"], "2026-11-20");
        assert_eq!(serde_json::from_value::<Nag>(j).unwrap(), n);
    }

    #[test]
    fn ask_target_text() {
        assert_eq!(AskTarget::Moa.to_string(), "moa");
        assert_eq!("moa".parse::<AskTarget>().unwrap(), AskTarget::Moa);
        let id = SessionId(uuid::Uuid::from_u128(0xabc));
        let t = AskTarget::Session(id);
        assert_eq!(t.to_string().parse::<AskTarget>().unwrap(), t);
        assert_eq!(serde_json::to_string(&t).unwrap(), format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<AskTarget>("\"moa\"").unwrap(),
            AskTarget::Moa
        );
        assert_eq!(
            "nope".parse::<AskTarget>().unwrap_err().to_string(),
            "not a known ask target"
        );
    }

    #[test]
    fn session_id_text_and_errors() {
        let id = SessionId(uuid::Uuid::from_u128(1));
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000001");
        assert_eq!(id.to_string().parse::<SessionId>().unwrap(), id);
        let err = "secret-text".parse::<SessionId>().unwrap_err().to_string();
        assert!(!err.contains("secret"));
    }

    #[test]
    fn suggestion_action_json_shape() {
        let a = SuggestionAction {
            verb: "task".into(),
            args: vec!["x".into()],
        };
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, r#"{"verb":"task","args":["x"]}"#);
        assert!(ACTION_VERBS.contains(&"project"));
    }

    #[test]
    fn rows_construct_with_expected_nullability() {
        let t = Task {
            id: TaskId(1),
            project_id: None,
            title: "t".into(),
            status: TaskStatus::Todo,
            blocked_on: None,
            today: None,
            session_id: None,
            note: None,
            who: None,
            due: None,
            priority: None,
            created_at: UtcMs(0),
            updated_at: UtcMs(0),
            done_at: None,
        };
        assert_eq!(t.status, TaskStatus::Todo);
    }
}
