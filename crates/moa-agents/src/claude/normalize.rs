//! Claude's hook payloads, normalized into the agent-independent `HookEnvelope` (the C10
//! table). Anything moa doesn't subscribe to, or can't attribute to a conversation, is `None`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use moa_core::UtcMs;
use moa_core::hook::{HOOK_LINE_V, HookEnvelope, HookKind, HookLine, StartSource};
use moa_core::live::cut_middle;
use serde_json::Value;

/// How much of a transcript's end `last_assistant_text` reads.
const TAIL_BYTES: u64 = 256 * 1024;

fn text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key)?.as_str()
}

/// What the hook means, or `None` when it never changes a session's state. `tail` reads the
/// last assistant text of a transcript, for a `Stop` that carries no message of its own.
fn kind_of(
    payload: &Value,
    event: &str,
    tail: &dyn Fn(&Path) -> Option<String>,
) -> Option<HookKind> {
    Some(match event {
        "SessionStart" => HookKind::SessionStarted {
            source: StartSource::from_word(text(payload, "source").unwrap_or("")),
        },
        "UserPromptSubmit" => HookKind::TurnStarted {
            prompt: cut_middle(text(payload, "prompt").unwrap_or("")),
        },
        "Stop" => {
            // A blank message is no message.
            let last = text(payload, "last_assistant_message")
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .or_else(|| tail(Path::new(text(payload, "transcript_path")?)))
                .unwrap_or_default();
            HookKind::TurnEnded {
                last_message: cut_middle(&last),
            }
        }
        "StopFailure" => HookKind::TurnFailed {
            error: text(payload, "error")
                .map(|e| e.replace('_', " "))
                .unwrap_or_else(|| "failed".to_string()),
        },
        "PermissionRequest" => HookKind::NeedsPermission {
            tool: text(payload, "tool_name").unwrap_or("a tool").to_string(),
        },
        "PreToolUse" => match text(payload, "tool_name")? {
            "AskUserQuestion" => HookKind::NeedsAnswer {
                what: payload
                    .pointer("/tool_input/questions/0/question")
                    .and_then(Value::as_str)
                    .unwrap_or("a question")
                    .to_string(),
            },
            "ExitPlanMode" => HookKind::NeedsAnswer {
                what: "plan approval".to_string(),
            },
            _ => return None,
        },
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => HookKind::NeedsCleared,
        // The idle ping and every other notification never mean "needs you".
        "Notification" if text(payload, "notification_type") == Some("elicitation_dialog") => {
            HookKind::NeedsAnswer {
                what: text(payload, "message").unwrap_or("a form").to_string(),
            }
        }
        "SessionEnd" => HookKind::SessionEnded {
            reason: text(payload, "reason").unwrap_or("other").to_string(),
        },
        "CwdChanged" => HookKind::CwdChanged {
            cwd: text(payload, "new_cwd")?.to_string(),
        },
        _ => return None,
    })
}

/// Turns one Claude hook line into an envelope. `None` means the line is not for the core:
/// another version, no conversation id, or an event that never changes state.
pub fn normalize(line: &HookLine, tail: &dyn Fn(&Path) -> Option<String>) -> Option<HookEnvelope> {
    if line.v != HOOK_LINE_V {
        return None;
    }
    let payload = &line.payload;
    let conversation_id = text(payload, "session_id")
        .filter(|id| !id.is_empty())
        .or(line.env.claude_session_id.as_deref())?
        .to_string();
    let kind = kind_of(payload, text(payload, "hook_event_name")?, tail)?;
    Some(HookEnvelope {
        agent: line.agent,
        slot: line.env.moa_session.clone(),
        pane: line.env.tmux_pane.clone(),
        brain: line.env.moa_brain.clone(),
        store_dir: line.env.claude_config_dir.clone(),
        conversation_id,
        kind,
        at: UtcMs(line.at_ns / 1_000_000),
        at_ns: line.at_ns,
        pid: line.pid,
        seq: line.seq,
        cwd: text(payload, "cwd").map(str::to_string),
        transcript_path: text(payload, "transcript_path").map(str::to_string),
    })
}

/// The last `text` item of the last assistant line, from the final 256 KiB of a transcript.
/// It never fails loudly: anything unreadable is `None`. Only a regular file is opened: the
/// path comes from a payload, and opening a FIFO would block the caller's thread.
pub fn last_assistant_text(transcript: &Path) -> Option<String> {
    if !std::fs::metadata(transcript).ok()?.is_file() {
        return None;
    }
    let mut file = File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    // One byte before the window too: a window that starts exactly on a line then keeps
    // that line, since the piece skipped below is just the end of the line before.
    let start = len.saturating_sub(TAIL_BYTES + 1);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES + 1).read_to_end(&mut bytes).ok()?;
    let window = String::from_utf8_lossy(&bytes);
    let mut lines = window.lines();
    if start > 0 {
        // The window began mid-line.
        lines.next();
    }
    lines
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|v| {
            let content = v.pointer("/message/content")?.as_array()?;
            content
                .iter()
                .rev()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("text"))?
                .get("text")?
                .as_str()
                .map(str::to_string)
        })
        .next_back()
}
