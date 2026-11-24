//! Every conversion between the protocol library's types and moa's own (`central::wire`):
//! nothing else in moa names a library type.

use std::path::Path;

use agent_client_protocol::Error;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, ContentChunk, FileSystemCapabilities,
    Implementation, InitializeRequest, InitializeResponse, LoadSessionRequest, NewSessionRequest,
    NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason, SystemPrompt, SystemPromptMode, TextContent,
};
use moa_core::central::wire::{self, Caps, PermKind, PermOption, PlanEntry, Update};
use serde::Serialize;
use serde_json::Value;

/// moa's `initialize`: it reads and writes no files and runs no terminals for its agent.
pub(super) fn initialize() -> InitializeRequest {
    let fs = FileSystemCapabilities::new()
        .read_text_file(false)
        .write_text_file(false);
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(ClientCapabilities::new().fs(fs).terminal(false))
        .client_info(Implementation::new("moa", env!("CARGO_PKG_VERSION")))
}

pub(super) fn caps(init: &InitializeResponse) -> Caps {
    let agent = &init.agent_capabilities;
    Caps {
        load: agent.load_session,
        resume: agent.session_capabilities.resume.is_some(),
        system_prompt: agent.session_capabilities.system_prompt.is_some(),
    }
}

fn text(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(s))
}

pub(super) fn new_session(cwd: &Path, system_prompt: Option<&str>) -> NewSessionRequest {
    let new = NewSessionRequest::new(cwd);
    match system_prompt {
        Some(s) => {
            new.system_prompt(SystemPrompt::new(vec![text(s)]).mode(SystemPromptMode::Append))
        }
        None => new,
    }
}

pub(super) fn resume(thread: &str, cwd: &Path) -> ResumeSessionRequest {
    ResumeSessionRequest::new(thread.to_string(), cwd)
}

pub(super) fn load(thread: &str, cwd: &Path) -> LoadSessionRequest {
    LoadSessionRequest::new(thread.to_string(), cwd)
}

pub(super) fn prompt(thread: &str, said: &str) -> PromptRequest {
    PromptRequest::new(thread.to_string(), vec![text(said)])
}

pub(super) fn cancel(thread: &str) -> CancelNotification {
    CancelNotification::new(thread.to_string())
}

/// The text of a content block; one that isn't text is named, not carried.
fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.text.clone(),
        ContentBlock::Image(_) => "[image]".into(),
        ContentBlock::Audio(_) => "[audio]".into(),
        _ => "[resource]".into(),
    }
}

/// The protocol's name for a value (`"pending"`, `"read"`, `"high"`).
fn name_of(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn chunk_id(chunk: &ContentChunk) -> Option<String> {
    chunk.message_id.as_ref().map(ToString::to_string)
}

/// A new thread's id.
pub(super) fn new_thread(new: &NewSessionResponse) -> String {
    new.session_id.to_string()
}

/// An update and the thread it is in.
pub(super) fn notification(n: SessionNotification) -> (String, Update) {
    (n.session_id.to_string(), update(n.update))
}

fn update(update: SessionUpdate) -> Update {
    match update {
        SessionUpdate::AgentMessageChunk(c) => Update::Message {
            id: chunk_id(&c),
            text: text_of(&c.content),
        },
        SessionUpdate::AgentThoughtChunk(c) => Update::Thought {
            id: chunk_id(&c),
            text: text_of(&c.content),
        },
        SessionUpdate::UserMessageChunk(_) => Update::UserEcho,
        SessionUpdate::ToolCall(t) => Update::Tool {
            id: t.tool_call_id.to_string(),
            title: t.title,
            kind: name_of(&t.kind),
            status: name_of(&t.status),
            input: t.raw_input.map(|v| v.to_string()),
        },
        SessionUpdate::ToolCallUpdate(u) => Update::ToolUpdate {
            id: u.tool_call_id.to_string(),
            title: u.fields.title,
            status: u.fields.status.map(|s| name_of(&s)),
            input: u.fields.raw_input.map(|v| v.to_string()),
        },
        SessionUpdate::Plan(p) => Update::Plan {
            entries: p
                .entries
                .iter()
                .map(|e| PlanEntry {
                    content: e.content.clone(),
                    status: name_of(&e.status),
                    priority: name_of(&e.priority),
                })
                .collect(),
        },
        SessionUpdate::PlanUpdate(_) => Update::Other("plan_update"),
        SessionUpdate::PlanRemoved(_) => Update::Other("plan_removed"),
        SessionUpdate::AvailableCommandsUpdate(_) => Update::Other("available_commands_update"),
        SessionUpdate::CurrentModeUpdate(_) => Update::Other("current_mode_update"),
        SessionUpdate::ConfigOptionUpdate(_) => Update::Other("config_option_update"),
        SessionUpdate::SessionInfoUpdate(_) => Update::Other("session_info_update"),
        SessionUpdate::UsageUpdate(_) => Update::Other("usage_update"),
        SessionUpdate::Notice(_) => Update::Other("notice"),
        SessionUpdate::CompactionUpdate(_) => Update::Other("compaction_update"),
        SessionUpdate::CompactionSummaryChunk(_) => Update::Other("compaction_summary_chunk"),
        SessionUpdate::SubagentUpdate(_) => Update::Other("subagent_update"),
        SessionUpdate::SessionMessage(_) => Update::Other("session_message"),
        SessionUpdate::SessionMessageChunk(_) => Update::Other("session_message_chunk"),
        _ => Update::Other("unknown"),
    }
}

pub(super) fn stop_reason(stop: StopReason) -> wire::StopReason {
    match stop {
        StopReason::EndTurn => wire::StopReason::EndTurn,
        StopReason::MaxTokens => wire::StopReason::MaxTokens,
        StopReason::MaxTurnRequests => wire::StopReason::MaxTurnRequests,
        StopReason::Refusal => wire::StopReason::Refusal,
        StopReason::Cancelled => wire::StopReason::Cancelled,
        other => wire::StopReason::Other(name_of(&other)),
    }
}

fn perm_option(option: &PermissionOption) -> PermOption {
    let kind = match option.kind {
        PermissionOptionKind::AllowOnce => PermKind::AllowOnce,
        PermissionOptionKind::AllowAlways => PermKind::AllowAlways,
        PermissionOptionKind::RejectOnce => PermKind::RejectOnce,
        PermissionOptionKind::RejectAlways => PermKind::RejectAlways,
        _ => PermKind::Other,
    };
    PermOption {
        id: option.option_id.to_string(),
        kind,
    }
}

/// A permission request's thread, title and options.
pub(super) fn permission(req: &RequestPermissionRequest) -> (String, String, Vec<PermOption>) {
    let title = req.tool_call.fields.title.clone().unwrap_or_default();
    let options = req.options.iter().map(perm_option).collect();
    (req.session_id.to_string(), title, options)
}

/// The answer to a permission request: the chosen option, or cancelled.
pub(super) fn permission_answer(choice: Option<String>) -> RequestPermissionResponse {
    RequestPermissionResponse::new(match choice {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        None => RequestPermissionOutcome::Cancelled,
    })
}

/// An error as one line of text: its message, and its data when that is a string.
pub(super) fn error_text(e: &Error) -> String {
    match &e.data {
        Some(Value::String(data)) => format!("{}: {data}", e.message),
        _ => e.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{
        AudioContent, EmbeddedResource, EmbeddedResourceResource, ImageContent, ResourceLink,
        TextResourceContents,
    };

    use super::*;

    /// moa's agent says `block` as a piece of its answer, then as a piece of its thinking.
    fn said(block: ContentBlock) -> [Update; 2] {
        let answer = SessionUpdate::AgentMessageChunk(ContentChunk::new(block.clone()));
        let thinking = SessionUpdate::AgentThoughtChunk(ContentChunk::new(block));
        [answer, thinking].map(|u| notification(SessionNotification::new("th-1", u)).1)
    }

    #[test]
    fn blocks_that_arent_text_are_named() {
        let resource = TextResourceContents::new("the notes", "file:///r/notes.md");
        let embedded = EmbeddedResourceResource::TextResourceContents(resource);
        let blocks = [
            (
                ContentBlock::Image(ImageContent::new("iVBORw0KGgo=", "image/png")),
                "[image]",
            ),
            (
                ContentBlock::Audio(AudioContent::new("UklGRg==", "audio/wav")),
                "[audio]",
            ),
            (
                ContentBlock::ResourceLink(ResourceLink::new("notes", "file:///r/notes.md")),
                "[resource]",
            ),
            (
                ContentBlock::Resource(EmbeddedResource::new(embedded)),
                "[resource]",
            ),
            (text("plain words"), "plain words"),
        ];
        for (block, want) in blocks {
            let [answer, thinking] = said(block);
            assert_eq!(
                answer,
                Update::Message {
                    id: None,
                    text: want.into()
                }
            );
            assert_eq!(
                thinking,
                Update::Thought {
                    id: None,
                    text: want.into()
                }
            );
        }
    }
}
