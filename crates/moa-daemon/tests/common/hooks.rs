//! Hook lines and the sessions they are about: fixtures for tests that send hooks to a daemon.

use jiff::Timestamp;
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Session, SessionId,
    SpikePane,
};
use moa_testkit::brain::TYPICAL_NOW;
use serde_json::{Value, json};

/// The fake clock's now is the Typical brain's, in milliseconds; `ago` is before it.
pub fn ago(ms: i64) -> i64 {
    let now: Timestamp = TYPICAL_NOW.parse().unwrap();
    now.as_millisecond() - ms
}

/// A live, idle Claude session `id` in `/work/Lantern`, and its conversation `conversation`,
/// the one its hooks name.
pub fn session_rows(id: SessionId, conversation: &str) -> Vec<Change> {
    let session = Session {
        id,
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some("/work/Lantern".into()),
        start_cwd: Some("/work/Lantern".into()),
        project_id: None,
        title: None,
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(1)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(UtcMs(1)),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(1)),
    };
    let conversation = Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conversation.into(),
        session_id: id,
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    };
    vec![Change::Session(session), Change::Conversation(conversation)]
}

/// Session `id`'s tab `pane` on the spike's own server: the daemon's reconcile at start (no
/// server here) leaves the session live, as these tests are about hooks, not tabs.
pub fn spike_tab(id: SessionId, pane: &str) -> Change {
    Change::SpikePane(SpikePane {
        session_id: id,
        pane_id: pane.into(),
        tmux: Some("/tmp/tmux-501/spike".into()),
        imported_at: UtcMs(1),
    })
}

/// A raw hook line of session `session`, whose conversation is `conversation`, at `at_ms`.
pub fn hook_line(session: &str, conversation: &str, event: &str, at_ms: i64, seq: u32) -> Value {
    let mut payload = json!({"session_id": conversation, "hook_event_name": event});
    if event == "UserPromptSubmit" {
        payload["prompt"] = json!("light the Lantern");
    }
    json!({
        "v": 1,
        "agent": "claude",
        "at_ns": at_ms * 1_000_000,
        "pid": 100,
        "seq": seq,
        "env": {"moa_session": session, "moa_brain": "default", "claude_session_id": conversation},
        "payload": payload,
    })
}

/// `line` as the hook notice a hook sends.
pub fn hook_notice(line: &Value) -> String {
    json!({"jsonrpc": "2.0", "method": "hook", "params": line}).to_string()
}
