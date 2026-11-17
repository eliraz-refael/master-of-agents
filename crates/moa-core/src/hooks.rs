//! Hook events in the core: which session one is for, whether it is new, what it changes and
//! what is written down. `core.rs` dispatches here. A session starting or ending brings its
//! lifecycle and conversation rotation; a pane that exits ends its session.

use serde_json::json;

use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine};
use crate::hook::{HookEnvelope, HookKind, StartSource};
use crate::live::cut_middle;
use crate::model::{
    Activity, ActivityWho, Conversation, Event, EventId, EventKind, Lifecycle, Location, Session,
    SessionId,
};
use crate::op::{ConnId, Done, ReqId};
use crate::peers::peers_note;
use crate::state::{State, Table};
use crate::tmux::focus::trusted_panes;
use crate::tmux::ids::PaneId;

/// What [`apply`] did with an event.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Applied {
    /// Not for this brain or not for a known session; the reason is a fixed sentence for the
    /// log, empty when the event is expected and not worth a line.
    Dropped(&'static str),
    /// Already applied once.
    Duplicate,
    /// Older than the session's last activity, or an end that leaves the session live:
    /// written to history, nothing else.
    HistoryOnly,
    Applied,
}

/// The effects of one hook event, and its answer when the sender waits for one.
pub(crate) fn run(
    state: &mut State,
    now: &Now,
    reply: Option<(ConnId, ReqId)>,
    env: &HookEnvelope,
) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let applied = apply(state, &mut tx, env);
    let mut fx = Vec::new();
    if let Applied::Dropped(reason) = applied
        && !reason.is_empty()
    {
        fx.push(Effect::Log(LogLine {
            level: LogLevel::Info,
            message: reason.to_string(),
        }));
    }
    fx.extend(post_commit(state, tx));
    if let Some((conn, req)) = reply {
        // Only a start has anything to say; slice 7 fills the note.
        let note = match env.kind {
            HookKind::SessionStarted { .. } => resolve(state, env)
                .ok()
                .and_then(|id| peers_note(state, id)),
            _ => None,
        };
        fx.push(Effect::Reply {
            conn,
            req,
            result: Ok(Done {
                text: String::new(),
                data: json!({ "context": note }),
            }),
        });
    }
    fx
}

/// Which session an event is for, or why it is dropped.
fn resolve(state: &State, env: &HookEnvelope) -> Result<SessionId, &'static str> {
    if env
        .brain
        .as_deref()
        .is_some_and(|b| b != state.brain().as_str())
    {
        return Err("a hook event for another brain was dropped");
    }
    if env.slot.as_deref() == Some("moa") {
        return Err("");
    }
    env.slot
        .as_deref()
        .and_then(|s| s.parse::<SessionId>().ok())
        .filter(|id| state.session(*id).is_some())
        .or_else(|| state.session_of_conversation(&env.conversation_id))
        .ok_or("a hook event for an unknown session was dropped")
}

pub(crate) fn apply(state: &mut State, tx: &mut Tx, env: &HookEnvelope) -> Applied {
    let id = match resolve(state, env) {
        Ok(id) => id,
        Err(reason) => return Applied::Dropped(reason),
    };
    let key = env.dedupe_key();
    if state.recent_hook_set.contains(&key) {
        return Applied::Duplicate;
    }
    let before = state
        .session(id)
        .expect("resolved to a known session")
        .clone();
    let mut changes = Vec::new();

    if let Some(row) = history_row(state, tx, &before, env) {
        changes.push(row);
    }

    // An older event keeps its history row and changes nothing else. So does an end that
    // is only the conversation changing (`/clear`, a resume).
    let older = before.activity_at.is_some_and(|t| env.at < t);
    let rotating = matches!(&env.kind, HookKind::SessionEnded { reason } if reason == "clear" || reason == "resume");
    if older || rotating {
        return commit(state, tx, key, changes, Applied::HistoryOnly);
    }

    let after = transition(&before, env);
    if let Some(project) = after
        .project_id
        .and_then(|p| state.projects.get(&p))
        .filter(|p| p.last_activity_on != Some(tx.today))
    {
        let mut project = project.clone();
        project.last_activity_on = Some(tx.today);
        changes.push(Change::Project(project));
    }
    if let Some(text) = activity_text(&after, &env.kind) {
        changes.push(tx.activity_row(
            state,
            ActivityWho::Session,
            text,
            Some(("session", id.to_string())),
        ));
    }
    if let HookKind::SessionStarted { source } = &env.kind {
        changes.push(Change::Conversation(rotated(state, id, env, *source)));
    }
    changes.insert(0, Change::Session(after));
    commit(state, tx, key, changes, Applied::Applied)
}

/// Commits, and only then remembers the key: a refused write leaves the event to arrive again.
fn commit(
    state: &mut State,
    tx: &mut Tx,
    key: (u32, u32, i64),
    changes: Vec<Change>,
    applied: Applied,
) -> Applied {
    // Nothing to write is nothing that can be refused.
    if !changes.is_empty() && tx.commit(state, changes).is_err() {
        return Applied::Dropped("a hook event was not saved");
    }
    state.remember_hook(key);
    applied
}

/// The `events` row, for every kind but the two that are only state.
fn history_row(
    state: &State,
    tx: &mut Tx,
    session: &Session,
    env: &HookEnvelope,
) -> Option<Change> {
    let (text, detail) = match &env.kind {
        HookKind::NeedsCleared | HookKind::CwdChanged { .. } => return None,
        HookKind::SessionStarted { source } => {
            let word = source_word(*source);
            (word.to_string(), Some(json!({ "source": word })))
        }
        HookKind::SessionEnded { reason } => (reason.clone(), Some(json!({ "reason": reason }))),
        HookKind::TurnStarted { prompt } => (cut_middle(prompt), None),
        HookKind::TurnEnded { last_message } => (cut_middle(last_message), None),
        HookKind::TurnFailed { error } => (error.clone(), Some(json!({ "error": error }))),
        HookKind::NeedsPermission { tool } => (tool.clone(), Some(json!({ "tool": tool }))),
        HookKind::NeedsAnswer { what } => (what.clone(), None),
    };
    Some(Change::Event(Event {
        id: EventId(tx.next_id(state, Table::Event)),
        at: env.at,
        session_id: Some(session.id),
        conversation_id: Some(env.conversation_id.clone()),
        account: session.account.clone(),
        cwd: env.cwd.clone().or_else(|| session.cwd.clone()),
        kind: EventKind(env.kind.event_kind().to_string()),
        text: Some(text),
        detail,
    }))
}

/// The conversation a start belongs to: a new one for an id the session hasn't had, or the
/// known one with its `last_seen_at` moved.
fn rotated(state: &State, id: SessionId, env: &HookEnvelope, source: StartSource) -> Conversation {
    if let Some(known) = state
        .conversations_of(id)
        .find(|c| c.conversation_id == env.conversation_id)
    {
        let mut c = known.clone();
        c.last_seen_at = env.at;
        return c;
    }
    Conversation {
        agent_kind: env.agent,
        conversation_id: env.conversation_id.clone(),
        session_id: id,
        store_dir: env.store_dir.clone(),
        spike_account: None,
        reason: source.reason(),
        first_seen_at: env.at,
        last_seen_at: env.at,
        transcript_path: env.transcript_path.clone(),
        transcript_present: None,
        transcript_checked_at: None,
    }
}

/// A pane exited: the session bound to it, if the binding can be believed (T11) and the
/// session is still live, ends and loses the binding. Nothing else checks that a process is
/// alive. The caller sends what `tx` holds.
pub(crate) fn pane_gone(state: &mut State, tx: &mut Tx, pane: PaneId) {
    let Some((_, id)) = trusted_panes(state).get(&pane).copied() else {
        return;
    };
    let Some(session) = state.session(id).filter(|s| s.lifecycle == Lifecycle::Live) else {
        return;
    };
    let mut after = session.clone();
    after.lifecycle = Lifecycle::Ended;
    let mut changes = vec![Change::DeletePaneBinding(id)];
    if let Some(text) = activity_text(
        &after,
        &HookKind::SessionEnded {
            reason: String::new(),
        },
    ) {
        changes.push(tx.activity_row(
            state,
            ActivityWho::Session,
            text,
            Some(("session", id.to_string())),
        ));
    }
    changes.insert(0, Change::Session(after));
    // A refused write leaves the pane bound and the session live; the next event retries.
    let _ = tx.commit(state, changes);
}

fn source_word(source: StartSource) -> &'static str {
    match source {
        StartSource::Startup => "startup",
        StartSource::Resume => "resume",
        StartSource::Clear => "clear",
        StartSource::Compact => "compact",
        StartSource::Fork => "fork",
    }
}

/// The session after this event. Every event moves the times; the kind decides the rest.
fn transition(before: &Session, env: &HookEnvelope) -> Session {
    let mut s = before.clone();
    s.activity_at = Some(env.at);
    s.last_seen_at = Some(env.at);
    if let Some(cwd) = &env.cwd {
        s.cwd = Some(cwd.clone());
    }
    match &env.kind {
        HookKind::SessionStarted { .. } => {
            s.lifecycle = Lifecycle::Live;
            s.asleep_at = None;
            s.location = Location::Here;
            s.activity = Activity::Idle;
            s.resume_failed = false;
        }
        HookKind::SessionEnded { .. } => s.lifecycle = Lifecycle::Ended,
        HookKind::TurnStarted { prompt } => {
            let prompt = cut_middle(prompt);
            s.activity = Activity::Working;
            s.last_prompt = Some(prompt.clone());
            s.activity_why = Some(prompt);
        }
        HookKind::TurnEnded { last_message } => {
            s.activity = Activity::Idle;
            s.last_turn_end_at = Some(env.at);
            s.activity_why = Some(cut_middle(last_message));
        }
        HookKind::TurnFailed { error } => {
            s.activity = Activity::Failed;
            s.activity_why = Some(error.clone());
        }
        HookKind::NeedsPermission { tool } => {
            s.activity = Activity::NeedsPermission;
            s.activity_why = Some(format!("permission: {tool}"));
        }
        HookKind::NeedsAnswer { what } => {
            s.activity = Activity::NeedsAnswer;
            s.activity_why = Some(what.clone());
        }
        HookKind::NeedsCleared => {
            if matches!(
                s.activity,
                Activity::NeedsPermission | Activity::NeedsAnswer
            ) {
                s.activity = Activity::Working;
                s.activity_why = s.last_prompt.clone();
            }
        }
        HookKind::CwdChanged { cwd } => s.cwd = Some(cwd.clone()),
    }
    s
}

/// The activity feed's line for an event, as the spike's `a_event` trigger wrote it:
/// `<folder> <what>`, the folder being the last part of the session's cwd.
fn activity_text(session: &Session, kind: &HookKind) -> Option<String> {
    let what = match kind {
        HookKind::TurnStarted { prompt } => {
            let line: String = prompt.replace('\n', " ").chars().take(90).collect();
            format!("› {line}")
        }
        HookKind::SessionEnded { .. } => "○ ended".to_string(),
        HookKind::TurnEnded { .. } => "✓ turn done".to_string(),
        HookKind::TurnFailed { .. } => "✗ failed".to_string(),
        HookKind::NeedsPermission { .. } | HookKind::NeedsAnswer { .. } => {
            "⚠ needs you".to_string()
        }
        _ => return None,
    };
    let folder = session
        .cwd
        .as_deref()
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("");
    Some(format!("{folder} {what}"))
}
