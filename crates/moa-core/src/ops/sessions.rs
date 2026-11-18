//! `session ls|set|archive|close`. `open` is in [`super::launch`].

use serde_json::json;

use super::launch::title_of;
use crate::Now;
use crate::change::Change;
use crate::core::Tx;
use crate::hooks::asleep;
use crate::live::{display_state, is_asleep, spike_state};
use crate::model::{Group, Lifecycle, Session, SessionId, normalize_group_path};
use crate::names::{is_live_spike, newest_first, place, title_or_prompt};
use crate::op::{Done, OpError};
use crate::reconcile::agent_runs;
use crate::state::State;
use crate::time::{days_ago, local_text};
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::PaneId;
use crate::tmux::link::ready;

/// How much of the last prompt stands in for a missing title.
const TITLE_CUT: usize = 40;

const STILL_OPENING: &str = "it's still opening; try again in a moment";

pub(crate) fn ls(state: &State, now: &Now, all: bool) -> Result<Done, OpError> {
    let recent = days_ago(now, 2);
    let mut shown: Vec<_> = state
        .sessions()
        .filter(|s| {
            all || is_live_spike(s) || is_asleep(s) || s.last_seen_at.is_some_and(|t| t >= recent)
        })
        .collect();
    newest_first(&mut shown);
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for s in shown {
        let alive = if is_asleep(s) {
            "asleep"
        } else {
            spike_state(s)
        };
        let handle = state.handle_of(s.id);
        let group = place(state, s);
        let title = title_or_prompt(s, TITLE_CUT);
        let account = s.account.as_deref().unwrap_or("");
        let cwd = s.cwd.as_deref().unwrap_or("-");
        let last = s.last_seen_at.map(|t| local_text(t, &now.tz));
        let (display, why) = display_state(s, now.wall);
        lines.push(format!(
            "{handle}  {alive:<9} {account:<12} {group:<22} {title:<34} {cwd}  last {}",
            last.as_deref().unwrap_or("-")
        ));
        data.push(json!({
            "id": s.id,
            "conversation": state.current_conversation(s.id).map(|c| &c.conversation_id),
            "alive": alive,
            "state": spike_state(s),
            "display": display.word(),
            "why": why,
            "account": s.account,
            "group": (group != "-").then_some(&group),
            "title": title,
            "cwd": s.cwd,
            "last": last,
        }));
    }
    Ok(Done {
        text: lines.join("\n"),
        data: serde_json::Value::Array(data),
    })
}

pub(crate) fn set(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    q: &str,
    title: Option<&str>,
    group: Option<&str>,
) -> Result<Done, OpError> {
    if title.is_none() && group.is_none() {
        return Err(OpError("set needs --title or --group".into()));
    }
    if title.is_some_and(|t| t.trim().is_empty()) {
        return Err(OpError("--title can't be empty".into()));
    }
    let group = match group.map(normalize_group_path) {
        Some(g) if g.is_empty() => return Err(OpError("a group needs a name".into())),
        other => other,
    };
    let id = state.resolve(q)?;
    let mut session = state
        .session(id)
        .cloned()
        .ok_or_else(|| OpError(format!("no session matches '{q}'")))?;
    let shown = title
        .map(str::to_string)
        .or_else(|| session.title.clone().filter(|t| !t.is_empty()))
        .unwrap_or_else(|| state.handle_of(id));
    let mut changes = Vec::new();
    if let Some(t) = title {
        session.title = Some(t.to_string());
        session.title_locked = true;
    }
    let text = match &group {
        Some(g) => {
            if !state.groups.contains_key(g) {
                changes.push(Change::Group(Group {
                    path: g.clone(),
                    collapsed: false,
                    created_at: now.wall,
                }));
            }
            session.group_path = Some(g.clone());
            format!("session {shown} → {g}")
        }
        None => format!("session {shown} renamed"),
    };
    changes.push(Change::Session(session.clone()));
    tx.ui(state, changes, text)?;
    Ok(Done {
        text: "ok".into(),
        data: serde_json::to_value(&session).unwrap_or_default(),
    })
}

pub(crate) fn archive(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    qs: &[String],
) -> Result<Done, OpError> {
    if qs.is_empty() {
        return Err(OpError("archive needs a session".into()));
    }
    // Every name first: a bad one changes nothing.
    let mut ids: Vec<SessionId> = Vec::new();
    for q in qs {
        let id = state.resolve(q)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut changes = Vec::new();
    let mut archived = Vec::new();
    let mut said = Vec::new();
    for id in ids {
        let Some(s) = state.session(id) else {
            continue;
        };
        // Archiving twice keeps the first time.
        if s.archived_at.is_some() {
            continue;
        }
        said.push(format!(
            "archived {}",
            s.title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| state.handle_of(id))
        ));
        let mut s = s.clone();
        s.archived_at = Some(now.wall);
        changes.push(Change::Session(s));
        archived.push(id);
    }
    // One commit: a refusal leaves every session as it was.
    tx.ui_each(state, changes, said)?;
    Ok(Done {
        text: format!("archived {}", archived.len()),
        data: json!({ "archived": archived }),
    })
}

/// `close Q`: the session ends (not asleep: the user meant it, R14), and its tab is killed
/// once that is saved (R15), when moa can prove the tab is the session's (R31). A session
/// that isn't running is still marked ended, as the spike did.
pub(crate) fn close(state: &mut State, tx: &mut Tx, now: &Now, q: &str) -> Result<Done, OpError> {
    ready(state)?;
    let id = state.resolve(q)?;
    // Its pane isn't known yet.
    if state.tmux.launching(id).is_some() {
        return Err(OpError(STILL_OPENING.into()));
    }
    let s = state
        .session(id)
        .cloned()
        .ok_or_else(|| OpError(format!("no session matches '{q}'")))?;
    let data = json!({ "id": id });
    let pane = match kill_checked(state, id) {
        Ok(pane) => pane,
        Err(gone) => {
            // The adopted agent left its shell tab, which stays the user's: the session goes
            // to sleep, as any unexpected end does, and loses the binding. That is saved, so
            // the answer is success-shaped.
            let mut changes = vec![Change::DeletePaneBinding(id)];
            if s.lifecycle == Lifecycle::Live {
                changes.insert(0, Change::Session(asleep(&s, tx.wall)));
            }
            tx.commit(state, changes)?;
            return Ok(Done { text: gone.0, data });
        }
    };
    let text = format!("closed {}", title_of(state, &s));
    let ended = Session {
        lifecycle: Lifecycle::Ended,
        asleep_at: None,
        activity_at: Some(now.wall),
        ..s
    };
    let mut changes = vec![Change::Session(ended)];
    if state.binding(id).is_some() {
        changes.push(Change::DeletePaneBinding(id));
    }
    tx.ui(state, changes, text)?;
    if let Some(pane) = pane {
        tx.tmux(Cmd::new("kill-pane").args(["-t".to_string(), pane.to_string()]));
    }
    Ok(Done {
        text: "closed".into(),
        data,
    })
}

/// The pane to kill for session `id`, checked again first (R31): `None` when it isn't
/// running here. A tracked binding's pane is the agent's, since it dies with it. An
/// untracked (adopted) one may be a shell the agent has left: its process is read again,
/// and a pane that no longer runs the session's agent is refused. A kill has no listing, so
/// no folder is compared. The caller puts a refused session to sleep and deletes its
/// binding, in its own commit.
pub(crate) fn kill_checked(state: &State, id: SessionId) -> Result<Option<PaneId>, OpError> {
    let (Some(pane), Some(b)) = (bound_pane(state, id), state.binding(id)) else {
        return Ok(None);
    };
    if b.tracked {
        return Ok(Some(pane));
    }
    let inspect = state.deps().inspect;
    if agent_runs(state, id, inspect(b.pane_pid, &[]).as_ref(), None) {
        return Ok(Some(pane));
    }
    let title = state
        .session(id)
        .map(|s| title_of(state, s))
        .unwrap_or_else(|| state.handle_of(id));
    Err(OpError(format!(
        "{title}'s agent isn't running in that tab any more"
    )))
}
