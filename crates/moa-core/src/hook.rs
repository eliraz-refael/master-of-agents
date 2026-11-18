//! The hook vocabulary: the raw line `moa hook` writes, and the envelope the core applies.
//! Pure types, no IO. An agent's own normalizer (`moa_agents::claude::normalize`) turns the
//! first into the second.

use serde::{Deserialize, Serialize};

use crate::UtcMs;
use crate::model::{AgentKind, ConversationReason, EventKind};

/// The version of `HookLine`. A line with another version is dropped, never guessed at.
pub const HOOK_LINE_V: u32 = 1;

/// The environment `moa hook` captured next to the payload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HookEnv {
    pub moa_session: Option<String>,
    pub moa_brain: Option<String>,
    pub claude_config_dir: Option<String>,
    pub claude_session_id: Option<String>,
    pub tmux: Option<String>,
    pub tmux_pane: Option<String>,
}

/// One raw hook, as `moa hook` sends it to the daemon or spools it. `at_ns` is the wall clock
/// in nanoseconds; `pid` and `seq` say which process sent it and in what order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookLine {
    pub v: u32,
    pub agent: AgentKind,
    pub at_ns: i64,
    pub pid: u32,
    pub seq: u32,
    pub env: HookEnv,
    pub payload: serde_json::Value,
}

/// How a conversation began, in the agent's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartSource {
    Startup,
    Resume,
    Clear,
    Compact,
    Fork,
}

impl StartSource {
    /// Unknown words count as `Startup`.
    pub fn from_word(word: &str) -> StartSource {
        match word {
            "resume" => StartSource::Resume,
            "clear" => StartSource::Clear,
            "compact" => StartSource::Compact,
            "fork" => StartSource::Fork,
            _ => StartSource::Startup,
        }
    }

    /// Why a new conversation was added. `compact` with a new id is a context reset, as
    /// `/clear` is.
    pub fn reason(self) -> ConversationReason {
        match self {
            StartSource::Startup => ConversationReason::Start,
            StartSource::Resume => ConversationReason::Resume,
            StartSource::Clear | StartSource::Compact => ConversationReason::Clear,
            StartSource::Fork => ConversationReason::Fork,
        }
    }
}

/// What a hook means, agent-independent.
#[derive(Clone, Debug, PartialEq)]
pub enum HookKind {
    SessionStarted { source: StartSource },
    SessionEnded { reason: String },
    TurnStarted { prompt: String },
    TurnEnded { last_message: String },
    TurnFailed { error: String },
    NeedsPermission { tool: String },
    NeedsAnswer { what: String },
    NeedsCleared,
    CwdChanged { cwd: String },
}

impl HookKind {
    /// The `EventKind` constant this kind is recorded as.
    pub fn event_kind(&self) -> &'static str {
        match self {
            HookKind::SessionStarted { .. } => EventKind::SESSION_STARTED,
            HookKind::SessionEnded { .. } => EventKind::SESSION_ENDED,
            HookKind::TurnStarted { .. } => EventKind::TURN_STARTED,
            HookKind::TurnEnded { .. } => EventKind::TURN_ENDED,
            HookKind::TurnFailed { .. } => EventKind::TURN_FAILED,
            HookKind::NeedsPermission { .. } => EventKind::NEEDS_PERMISSION,
            HookKind::NeedsAnswer { .. } => EventKind::NEEDS_ANSWER,
            HookKind::NeedsCleared => EventKind::NEEDS_CLEARED,
            HookKind::CwdChanged { .. } => EventKind::CWD_CHANGED,
        }
    }
}

/// One hook, normalized: who it is for, what it means and when it happened.
#[derive(Clone, Debug, PartialEq)]
pub struct HookEnvelope {
    pub agent: AgentKind,
    /// `MOA_SESSION`: the moa session the agent was launched for, if any.
    pub slot: Option<String>,
    /// `TMUX_PANE`: the tab the agent runs in (`%N`), when it runs in tmux.
    pub pane: Option<String>,
    /// `MOA_BRAIN`.
    pub brain: Option<String>,
    /// `CLAUDE_CONFIG_DIR`: the store the conversation is recorded in.
    pub store_dir: Option<String>,
    pub conversation_id: String,
    pub kind: HookKind,
    pub at: UtcMs,
    pub at_ns: i64,
    pub pid: u32,
    pub seq: u32,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
}

impl HookEnvelope {
    /// Names one sent hook, so a hook that arrives twice (live and again from the spool) is
    /// applied once.
    pub fn dedupe_key(&self) -> (u32, u32, i64) {
        (self.pid, self.seq, self.at_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_sources_map_to_reasons() {
        let cases = [
            ("startup", ConversationReason::Start),
            ("resume", ConversationReason::Resume),
            ("clear", ConversationReason::Clear),
            ("compact", ConversationReason::Clear),
            ("fork", ConversationReason::Fork),
            ("something-new", ConversationReason::Start),
        ];
        for (word, reason) in cases {
            assert_eq!(StartSource::from_word(word).reason(), reason, "{word}");
        }
    }

    #[test]
    fn kinds_name_their_event() {
        let s = String::new;
        let cases = [
            (
                HookKind::SessionStarted {
                    source: StartSource::Startup,
                },
                EventKind::SESSION_STARTED,
            ),
            (
                HookKind::SessionEnded { reason: s() },
                EventKind::SESSION_ENDED,
            ),
            (
                HookKind::TurnStarted { prompt: s() },
                EventKind::TURN_STARTED,
            ),
            (
                HookKind::TurnEnded { last_message: s() },
                EventKind::TURN_ENDED,
            ),
            (HookKind::TurnFailed { error: s() }, EventKind::TURN_FAILED),
            (
                HookKind::NeedsPermission { tool: s() },
                EventKind::NEEDS_PERMISSION,
            ),
            (HookKind::NeedsAnswer { what: s() }, EventKind::NEEDS_ANSWER),
            (HookKind::NeedsCleared, EventKind::NEEDS_CLEARED),
            (HookKind::CwdChanged { cwd: s() }, EventKind::CWD_CHANGED),
        ];
        for (kind, event) in cases {
            assert_eq!(kind.event_kind(), event);
            assert!(EventKind(event.to_string()).is_known());
        }
    }

    #[test]
    fn the_dedupe_key_is_pid_seq_and_time() {
        let e = HookEnvelope {
            agent: AgentKind::Claude,
            slot: None,
            pane: None,
            brain: None,
            store_dir: None,
            conversation_id: "c".into(),
            kind: HookKind::NeedsCleared,
            at: UtcMs(5),
            at_ns: 5_000_000,
            pid: 7,
            seq: 3,
            cwd: None,
            transcript_path: None,
        };
        assert_eq!(e.dedupe_key(), (7, 3, 5_000_000));
    }
}
