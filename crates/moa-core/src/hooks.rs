//! Hook events in the core: which session one is for, whether it is new, what it changes and
//! what is written down. `core.rs` dispatches here. A session starting or ending brings its
//! lifecycle and conversation rotation. A clean quit ends a session; any other end, a pane
//! that exits among them, puts it to sleep (R14). Any other event from a session that isn't
//! live is history until a look at the tabs finds its agent ([`crate::wake`]).

use serde_json::json;

use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine};
use crate::hook::{HookEnvelope, HookKind, StartSource};
use crate::live::{asks, cut_middle};
use crate::model::{
    Activity, ActivityWho, Conversation, Event, EventId, EventKind, Lifecycle, Location, Session,
    SessionId,
};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::peers::peers_note;
use crate::state::{EventWhat, State, Table};
use crate::tmux::focus::bound_session;
use crate::tmux::ids::PaneId;
use crate::tmux::link::LaunchKind;
use crate::{Now, UtcMs};

/// What [`apply`] did with an event.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Applied {
    /// Not for this brain or not for a known session; the reason is a fixed sentence for the
    /// log, empty when the event is expected and not worth a line.
    Dropped(&'static str),
    /// Already applied once.
    Duplicate,
    /// Older than the session's last activity, an end that leaves the session live, or one
    /// for a session already ended: written to history, nothing else.
    HistoryOnly,
    /// Newer than session `0`'s last activity, though it isn't live and has no tab: written
    /// to history, and applied only if a look at the tabs finds its agent ([`crate::wake`]).
    Unproven(SessionId),
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
    if let Applied::Unproven(id) = applied {
        fx.extend(crate::wake::look(state, id, env));
    }
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
    let slot = slot_of(env).filter(|id| state.session(*id).is_some());
    if let Some(id) = slot {
        return if is_own(state, id, env) {
            Ok(id)
        } else {
            Err("a hook event from another agent in a session's tab was dropped")
        };
    }
    state
        .conversation_of_agent(env.agent, &env.conversation_id)
        .map(|c| c.session_id)
        .ok_or("a hook event for an unknown session was dropped")
}

/// The session an event's `MOA_SESSION` names, if it is one.
fn slot_of(env: &HookEnvelope) -> Option<SessionId> {
    env.slot.as_deref().and_then(|s| s.parse().ok())
}

/// Whether an event under session `id`'s slot comes from the session's own agent. Anything
/// the agent runs inherits the tab's environment, so a second `claude` started inside it (a
/// `claude -p` in a tool) sends its events under the same slot. The tab's agent is the one
/// on one of the session's conversations, or moving to a new one (`/clear`, a resume, a
/// fork, a compaction), or starting for the first time: a launcher that ignores
/// `--session-id` starts under an id of its own. A first start is told by nothing having
/// been heard from the session since `open` made it: every applied event moves its
/// `last_seen_at`.
fn is_own(state: &State, id: SessionId, env: &HookEnvelope) -> bool {
    if state
        .conversation_of_agent(env.agent, &env.conversation_id)
        .is_some_and(|c| c.session_id == id)
    {
        return true;
    }
    match &env.kind {
        HookKind::SessionStarted {
            source: StartSource::Startup,
        } => state
            .session(id)
            .is_some_and(|s| s.last_seen_at == Some(s.created_at)),
        HookKind::SessionStarted { .. } => true,
        _ => false,
    }
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

    let applied = match after_of(state, id, &before, env) {
        After::History => commit(state, tx, key, changes, Applied::HistoryOnly),
        After::Unproven => {
            let applied = if state.binding(id).is_none() {
                Applied::Unproven(id)
            } else {
                Applied::HistoryOnly
            };
            commit(state, tx, key, changes, applied)
        }
        After::Apply(after) => {
            changes.extend(applied_changes(state, tx, id, &after, env));
            changes.insert(0, Change::Session(*after));
            commit(state, tx, key, changes, Applied::Applied)
        }
    };
    // Saved: a start is a new agent, one an empty look at the tabs didn't see
    // ([`crate::wake`]), unless it comes from a tab moa killed; and one that lifts the
    // restart's fence lifts it now. Like the key, these are kept only once the event is.
    if !matches!(applied, Applied::Dropped(_))
        && matches!(env.kind, HookKind::SessionStarted { .. })
    {
        if !from_a_killed_tab(state, id, env) {
            state.not_found.remove(&id);
        }
        if lifts_fence(state, id, env) {
            state.restarted.remove(&id);
        }
    }
    applied
}

/// Whether `env` lifts session `id`'s restart fence (R24): a start that doesn't name it,
/// sent after the restart was saved. That is a new agent started by hand (the killed one
/// can't start again), as adopting one is; an older start, or one from the killed tab, is
/// the killed agent's own.
fn lifts_fence(state: &State, id: SessionId, env: &HookEnvelope) -> bool {
    matches!(env.kind, HookKind::SessionStarted { .. })
        && slot_of(env) != Some(id)
        && !from_a_killed_tab(state, id, env)
        && state
            .restarted
            .get(&id)
            .is_some_and(|(at, _)| env.at >= *at)
}

/// Whether `env` comes from the tab moa last killed for session `id` (sleep, close or
/// restart: [`State::killed`]), while the session isn't bound to that tab, and unless the
/// link last connected to another server (up or down since), where that id may name a new
/// tab. Its agent dies with the tab, but a hook it sent between the commit and the kill
/// arrives after the commit, and may be stamped later than it. A kill can be lost (its batch
/// failed, or the link went), and then a reload, reconcile or `adopt` binds the session to
/// the tab again: its agent runs, and its events count again.
fn from_a_killed_tab(state: &State, id: SessionId, env: &HookEnvelope) -> bool {
    let Some(pane) = unbound_tab_of(state, id, env) else {
        return false;
    };
    let Some((killed_on, killed)) = state.killed.get(&id) else {
        return false;
    };
    *killed == pane && state.tmux().generation().is_none_or(|g| g == *killed_on)
}

/// The tab `env` says it comes from, when session `id` isn't bound to it.
fn unbound_tab_of(state: &State, id: SessionId, env: &HookEnvelope) -> Option<PaneId> {
    let pane = env.pane.as_deref()?.parse::<PaneId>().ok()?;
    let bound = state.binding(id).and_then(|b| b.pane_id.parse().ok());
    (bound != Some(pane)).then_some(pane)
}

/// Whether end `env` comes from the tab session `id`'s restart killed (R24), which isn't
/// its tab now: the killed agent's, which may carry `MOA_SESSION` (`adopt` bound moa's own
/// agent). Any other end, one that doesn't say its tab or one with no binding among them,
/// goes by the other rules.
fn from_an_old_tab(state: &State, id: SessionId, env: &HookEnvelope) -> bool {
    let Some(pane) = unbound_tab_of(state, id, env) else {
        return false;
    };
    matches!(env.kind, HookKind::SessionEnded { .. })
        && state
            .restarted
            .get(&id)
            .is_some_and(|(_, killed)| *killed == pane)
}

/// What an event does to its session past its history row.
enum After {
    /// Nothing more.
    History,
    /// Nothing yet: the session isn't live, and only a look at the tabs can show its agent
    /// runs.
    Unproven,
    /// The session becomes this.
    Apply(Box<Session>),
}

/// What `env` does to session `id`, now `before`.
///
/// An older event keeps its history row and changes nothing else. Older is than the
/// session's last activity, or than its going to sleep: a sleep keeps `activity_at` (R5),
/// and a hook the agent sent before it died can still arrive after. An end goes by its
/// kind first (R14): one that is only the conversation changing (`/clear`, a resume)
/// changes nothing either, a clean quit ends a session that is asleep however old it is,
/// and any other end of a session already ended is history only. A start brings the
/// session back. Any other event from a session that isn't live changes nothing until its
/// agent is found running (M7: an asleep or ended session shows as it was left). Once a
/// session is restarted (R24), an event that doesn't name it in `MOA_SESSION` isn't from
/// an agent moa started for it, whenever it arrives: the killed agent's end is history
/// only, and so is any other of its events, until a start sent after the restart that
/// doesn't name it (a new agent started by hand) lifts that fence ([`lifts_fence`]). An
/// end from a tab that isn't the session's (the killed agent's, `MOA_SESSION` or not) is
/// history only too. While the restart's tab opens, any end is history only, whatever it
/// names: tmux names the new tab long before the new agent could end, so only the killed
/// agent ends then. Any other event from a tab moa killed for the session (sleep, close,
/// restart) is the killed agent's, sent before it died, however late it is stamped: history
/// only, so its start can't bring back a session moa just put to sleep or closed. Its end
/// goes by R14 like any other: moa's kill makes it `other`, and a clean quit still wins.
fn after_of(state: &State, id: SessionId, before: &Session, env: &HookEnvelope) -> After {
    let older = before
        .activity_at
        .max(before.asleep_at)
        .is_some_and(|t| env.at < t);
    let ended = before.lifecycle == Lifecycle::Ended;
    let not_its_agent = state.restarted.contains_key(&id)
        && slot_of(env) != Some(id)
        && !lifts_fence(state, id, env);
    let restarting = state
        .tmux()
        .launches()
        .any(|(i, l)| i == id && l.kind == LaunchKind::Restart);
    let after = match &env.kind {
        _ if not_its_agent => None,
        HookKind::SessionEnded { .. } if restarting || from_an_old_tab(state, id, env) => None,
        HookKind::SessionEnded { reason } => match end_kind(reason) {
            EndKind::Clean if ended && before.asleep_at.is_some() => {
                Some(woken_to_end(before, env))
            }
            EndKind::Rotating => None,
            _ if older || ended => None,
            EndKind::Clean => Some(transition(before, env)),
            // Heard from, as every applied event is; the rest stays as it was left (R5).
            EndKind::Unexpected => Some(Session {
                last_seen_at: before.last_seen_at.max(Some(env.at)),
                ..asleep(before, env.at)
            }),
        },
        _ if from_a_killed_tab(state, id, env) => None,
        _ if older => None,
        HookKind::SessionStarted { .. } => Some(transition(before, env)),
        _ if ended => return After::Unproven,
        _ => Some(transition(before, env)),
    };
    after.map_or(After::History, |s| After::Apply(Box::new(s)))
}

/// The changes an applied event brings besides the session itself, `after`: the project's
/// day, the activity row, and a start's conversation.
fn applied_changes(
    state: &State,
    tx: &mut Tx,
    id: SessionId,
    after: &Session,
    env: &HookEnvelope,
) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Some(project) = after
        .project_id
        .and_then(|p| state.projects.get(&p))
        .filter(|p| p.last_activity_on != Some(tx.today))
    {
        let mut project = project.clone();
        project.last_activity_on = Some(tx.today);
        changes.push(Change::Project(project));
    }
    if let Some(text) = activity_text(after, &env.kind) {
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
    changes
}

/// An event a look at the tabs held back ([`Applied::Unproven`]), applied to session `id`
/// now that its agent was found and the session woken: as [`apply`] would, without the
/// history row it already has.
pub(crate) fn apply_held(
    state: &mut State,
    tx: &mut Tx,
    id: SessionId,
    env: &HookEnvelope,
) -> Result<(), OpError> {
    let Some(before) = state.session(id).cloned() else {
        return Ok(());
    };
    let After::Apply(after) = after_of(state, id, &before, env) else {
        return Ok(());
    };
    let mut changes = applied_changes(state, tx, id, &after, env);
    changes.insert(0, Change::Session(*after));
    tx.commit(state, changes)
}

/// Commits, and only then remembers the key. A refused write drops the event here; the
/// daemon spools hooks while writes are refused, so the core is not asked for them then.
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

/// The conversation a start belongs to: a new one for an id nobody has had, or the known one
/// with its `last_seen_at` moved. One another session had is running here now: it moves to
/// this session and keeps what it knows, unless the envelope says better. The store is
/// always the envelope's: the agent runs in it.
fn rotated(state: &State, id: SessionId, env: &HookEnvelope, source: StartSource) -> Conversation {
    // No variable is Claude's own default store, which moa launches without one (R11, R32).
    let store_dir = Some(
        state
            .store_of(env.store_dir.as_deref())
            .display()
            .to_string(),
    );
    if let Some(known) = state.conversation_of_agent(env.agent, &env.conversation_id) {
        let mut c = known.clone();
        c.session_id = id;
        c.last_seen_at = env.at;
        c.store_dir = store_dir;
        if env.transcript_path.is_some() {
            c.transcript_path = env.transcript_path.clone();
        }
        return c;
    }
    Conversation {
        agent_kind: env.agent,
        conversation_id: env.conversation_id.clone(),
        session_id: id,
        store_dir,
        spike_account: None,
        reason: source.reason(),
        first_seen_at: env.at,
        last_seen_at: env.at,
        transcript_path: env.transcript_path.clone(),
        transcript_present: None,
        transcript_checked_at: None,
    }
}

/// What a `SessionEnd`'s reason says about the agent (R14).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EndKind {
    /// Only the conversation changing (`/clear`, a resume): the agent goes on.
    Rotating,
    /// The user quit the agent: the session ends.
    Clean,
    /// Anything else, a crash, a kill or a reboot among them: moa can't tell it from a
    /// choice, so the session goes to sleep, to be resumed.
    Unexpected,
}

pub(crate) fn end_kind(reason: &str) -> EndKind {
    match reason {
        "clear" | "resume" => EndKind::Rotating,
        "prompt_input_exit" | "logout" => EndKind::Clean,
        _ => EndKind::Unexpected,
    }
}

/// The session put to sleep at `at`: ended, with the time it went to sleep. Its activity,
/// why and `activity_at` stay (R5), so it shows as it was left.
pub(crate) fn asleep(s: &Session, at: UtcMs) -> Session {
    Session {
        lifecycle: Lifecycle::Ended,
        asleep_at: Some(at),
        ..s.clone()
    }
}

/// An asleep session that a clean quit ends: the quit can arrive after the pane's exit put
/// it to sleep, and older than that, and it still wins (R14). The times only move forward.
fn woken_to_end(before: &Session, env: &HookEnvelope) -> Session {
    Session {
        lifecycle: Lifecycle::Ended,
        asleep_at: None,
        activity_at: before.activity_at.max(Some(env.at)),
        last_seen_at: before.last_seen_at.max(Some(env.at)),
        ..before.clone()
    }
}

/// A pane exited: the session bound to it on this server (T11) loses the binding, and goes
/// to sleep if still live (after a clean exit its `SessionEnd` has already ended it, and one
/// that comes later still ends it). Nothing else checks that a process is alive. A pane
/// nobody is bound to is remembered, since an `open`'s answer naming it may still be on its
/// way. The caller sends what `tx` holds.
///
/// `false` when the write was refused: the exit is reported once, so nothing retries it.
pub(crate) fn pane_gone(state: &mut State, tx: &mut Tx, pane: PaneId) -> bool {
    let Some(id) = bound_session(state, pane) else {
        state.tmux.exited_unbound(pane);
        return true;
    };
    lost_agent(state, tx, id, vec![Change::DeletePaneBinding(id)])
}

/// `also`, and when the session is still live, its agent gone with no clean quit: the
/// session goes to sleep, with no activity row (R14). The `SessionEnd` the agent sent on its
/// way out, which often arrives after the exit, then decides: a clean quit ends the session,
/// anything else is history only.
/// `false` when the write was refused. Then nothing changes: a bound pane stays bound, and
/// a session whose pane exited before its `open` was answered stays live with no binding.
/// Reconcile puts both to sleep.
pub(crate) fn lost_agent(state: &mut State, tx: &mut Tx, id: SessionId, also: Vec<Change>) -> bool {
    let mut changes = also;
    if let Some(session) = state.session(id).filter(|s| s.lifecycle == Lifecycle::Live) {
        changes.insert(0, Change::Session(asleep(session, tx.wall)));
    }
    changes.is_empty() || tx.commit(state, changes).is_ok()
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

/// What committing `after` over `before` (`None` for a new session) tells the console (B3),
/// from the lifecycle and the activity, never the display state (a long turn turns quiet on
/// its own): brought live, `Started`; ended other than by going to sleep, `Ended`; put to
/// sleep, `Asleep`; come to need you (a permission, a question, or a turn that ended asking),
/// `NeedsYou`; a turn that ended any other way, `Finished`, from working or from needing you
/// (a denied permission ends the turn with nothing clearing it first, and "needs you" mustn't
/// stay on a session that is done). At most one, the first of these.
pub(crate) fn event_of(before: Option<&Session>, after: &Session) -> Option<EventWhat> {
    let live = |s: &Session| s.lifecycle == Lifecycle::Live;
    let ended = |s: &Session| s.lifecycle == Lifecycle::Ended && s.asleep_at.is_none();
    let asleep = |s: &Session| s.lifecycle == Lifecycle::Ended && s.asleep_at.is_some();
    let needs_you = |s: &Session| {
        live(s)
            && match s.activity {
                Activity::NeedsPermission | Activity::NeedsAnswer => true,
                Activity::Idle => asks(s.activity_why.as_deref().unwrap_or("")),
                Activity::Working | Activity::Failed => false,
            }
    };
    if live(after) {
        return match before {
            None => Some(EventWhat::Started),
            Some(b) if !live(b) => Some(EventWhat::Started),
            Some(b) if needs_you(after) && !needs_you(b) => Some(EventWhat::NeedsYou),
            Some(b)
                if after.activity == Activity::Idle
                    && !needs_you(after)
                    && matches!(
                        b.activity,
                        Activity::Working | Activity::NeedsPermission | Activity::NeedsAnswer
                    ) =>
            {
                Some(EventWhat::Finished)
            }
            Some(_) => None,
        };
    }
    let before = before?;
    if ended(after) && !ended(before) {
        Some(EventWhat::Ended)
    } else if asleep(after) && !asleep(before) {
        Some(EventWhat::Asleep)
    } else {
        None
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
            s.resume_failed = false;
            // A compaction can come mid-turn, and the turn carries on. Any other start is a
            // fresh conversation: nothing said before it is a reply.
            if !matches!(
                &env.kind,
                HookKind::SessionStarted {
                    source: StartSource::Compact
                }
            ) {
                s.activity = Activity::Idle;
                s.activity_why = None;
            }
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
/// `<folder> <what>`, the folder being the last part of the session's cwd; just `<what>`
/// when it has none. An end that leaves the session asleep has none (R14).
fn activity_text(session: &Session, kind: &HookKind) -> Option<String> {
    let what = match kind {
        HookKind::TurnStarted { prompt } => {
            let line: String = prompt.replace('\n', " ").chars().take(90).collect();
            format!("› {line}")
        }
        HookKind::SessionEnded { .. } if session.asleep_at.is_some() => return None,
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
        .filter(|f| !f.is_empty());
    Some(match folder {
        Some(folder) => format!("{folder} {what}"),
        None => what,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::tests::session;

    #[test]
    fn a_new_session_is_started_only_when_live() {
        let live = session();
        let ended = Session {
            lifecycle: Lifecycle::Ended,
            ..session()
        };
        let asleep = Session {
            asleep_at: Some(UtcMs(1)),
            ..ended.clone()
        };
        assert_eq!(event_of(None, &live), Some(EventWhat::Started));
        assert_eq!(event_of(None, &ended), None);
        assert_eq!(event_of(None, &asleep), None);
    }
}
