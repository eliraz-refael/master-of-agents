//! `session ls|set|archive|close|sleep`. `open` is in [`super::launch`].

use std::path::Path;

use serde_json::json;

use super::adopt::brains_folder;
use super::launch::title_of;
use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::live::{display_state, is_asleep, spike_state};
use crate::model::{
    AgentKind, Group, Lifecycle, PaneAgent, Session, SessionId, normalize_group_path,
};
use crate::names::{is_live_spike, newest_first, place, title_or_prompt};
use crate::op::{ConnId, Done, OpError, ReqId, SLEEP_USAGE};
use crate::reconcile::agent_runs;
use crate::sleep::{kill_pane, put_to_sleep, why_not};
use crate::state::State;
use crate::time::{days_ago, local_text};
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::{Generation, PaneId};
use crate::tmux::link::{Asked, Link, log, ready};
use crate::tmux::panes::{PaneInfo, Tag, holds, kept_bindings, list_panes, listed as listed_pane};

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
        // Adopted: moa sees only whether its agent runs (U14).
        let tracked = state.binding(s.id).is_none_or(|b| b.tracked);
        let mut line = format!(
            "{handle}  {alive:<9} {account:<12} {group:<22} {title:<34} {cwd}  last {}",
            last.as_deref().unwrap_or("-")
        );
        if !tracked && s.lifecycle == Lifecycle::Live {
            line.push_str(&format!("  adopted: moactl restart {handle} tracks it"));
        }
        lines.push(line);
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
            "tracked": tracked,
        }));
    }
    Ok(Done {
        text: lines.join("\n"),
        data: serde_json::Value::Array(data),
    })
}

/// `ls` with the tabs reconciled (R10): one listing of them is asked first; [`listed`]
/// answers with it.
pub(crate) fn start_ls(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    all: bool,
) -> Vec<Effect> {
    match state.tmux.ask(Asked::Ls { conn, req, all }, list_panes()) {
        Some(query) => vec![query],
        None => vec![Effect::Reply {
            conn,
            req,
            result: ls(state, now, all),
        }],
    }
}

/// An `ls`'s listing came back, or failed (tmux refused it, or the link went before it
/// answered): then the sessions are answered without the untracked list.
pub(crate) fn listed(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    all: bool,
    panes: Result<Vec<PaneInfo>, ()>,
) -> Vec<Effect> {
    let (mut fx, result) = match panes {
        Ok(panes) => ls_with(state, now, all, &panes),
        Err(()) => (Vec::new(), ls(state, now, all)),
    };
    fx.push(Effect::Reply { conn, req, result });
    fx
}

/// `ls` against `panes`, the listing asked for it (R23). First the adopted sessions whose
/// agent left its tab go to sleep, in one commit (R5, R31); then the reply is `ls`'s, plus a
/// row per agent moa doesn't track, each with the line that adopts it. With the tabs not
/// reconciled any more (a reload since it asked), the bindings can't be trusted: just `ls`.
pub(crate) fn ls_with(
    state: &mut State,
    now: &Now,
    all: bool,
    panes: &[PaneInfo],
) -> (Vec<Effect>, Result<Done, OpError>) {
    let generation = match (ready(state), &state.tmux().link) {
        (Ok(_), Link::Up { generation, .. }) => *generation,
        _ => return (Vec::new(), ls(state, now, all)),
    };
    let mut fx = Vec::new();
    let gone = agents_gone(state, generation, panes);
    if !gone.is_empty() {
        let mut tx = Tx::new(now);
        let changes: Vec<Change> = gone
            .iter()
            .flat_map(|id| put_to_sleep(state, &tx, *id))
            .collect();
        match tx.commit(state, changes) {
            Ok(()) => fx.extend(post_commit(state, tx)),
            Err(_) => fx.push(log(
                LogLevel::Warn,
                format!(
                    "{} adopted sessions whose agent left weren't put to sleep: nothing was saved",
                    gone.len()
                ),
            )),
        }
    }
    let rows = untracked(state, panes);
    let result = ls(state, now, all).map(|mut done| {
        for (info, _, account) in &rows {
            let (pane, cwd) = (info.pane, &info.cwd);
            if !done.text.is_empty() {
                done.text.push('\n');
            }
            done.text.push_str(&format!(
                "UNTRACKED tab {pane}: {account} in {cwd}  → moactl adopt {pane}"
            ));
            if let Some(data) = done.data.as_array_mut() {
                data.push(json!({ "untracked": pane.to_string(), "account": account, "cwd": cwd }));
            }
        }
        done
    });
    (fx, result)
}

/// The Claude agents in `panes` that moa doesn't track (R23): alive, not a terminal, held by
/// no binding and not tagged, with no `MOA_SESSION`, in the brain's folders (R22). Each with
/// its agent and its account: the listed account of its store (R32), else the store
/// folder's name.
pub(crate) fn untracked(state: &State, panes: &[PaneInfo]) -> Vec<(PaneInfo, PaneAgent, String)> {
    let Link::Up { generation, .. } = state.tmux().link else {
        return Vec::new();
    };
    let held: Vec<PaneId> = kept_bindings(state, generation, panes)
        .iter()
        .filter_map(|b| b.pane_id.parse().ok())
        .collect();
    let deps = state.deps();
    panes
        .iter()
        .filter(|p| !p.dead && !p.term && p.tag == Tag::Unset && !held.contains(&p.pane))
        .filter(|p| brains_folder(state, &(deps.canonical)(Path::new(&p.cwd))))
        .filter_map(|p| {
            let agent = (deps.inspect)(p.pid, &p.start)
                .filter(|a| a.kind == AgentKind::Claude && a.moa_session.is_none())?;
            let store = state.store_of(agent.store_dir.as_deref());
            let account = state.account_of_store(&store).map_or_else(
                || {
                    store.file_name().map_or_else(
                        || store.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    )
                },
                |a| a.to_string(),
            );
            Some((p.clone(), agent, account))
        })
        .collect()
}

/// The sessions whose untracked binding of this server (`generation`) has lost its agent, as
/// `panes` shows it (R23, R31): its tab is gone, or no longer runs the session's agent there.
/// Tracked bindings need no look: their tab dies with the agent.
fn agents_gone(state: &State, generation: Generation, panes: &[PaneInfo]) -> Vec<SessionId> {
    let inspect = state.deps().inspect;
    state
        .pane_bindings()
        .filter(|b| {
            !b.tracked && b.server_pid == generation.pid && b.server_start == generation.start
        })
        .filter(|b| {
            let info = listed_pane(b, panes);
            let runs = holds(b, generation, info)
                && info.is_some_and(|i| {
                    let agent = inspect(i.pid, &i.start);
                    agent_runs(state, b.session_id, agent.as_ref(), Some(&i.cwd))
                });
            !runs
        })
        .map(|b| b.session_id)
        .collect()
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
    // Its pane isn't known yet, or a resume of it would launch it after this.
    if state.tmux().opening(id) {
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
            let changes = put_to_sleep(state, tx, id);
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
        tx.tmux(kill_pane(pane));
    }
    Ok(Done {
        text: "closed".into(),
        data,
    })
}

/// `sleep Q…` or `sleep --idle H [--dry-run]` (R20): sessions go to sleep, their tabs
/// killed once that is saved (R15). By name, any running session goes, tracked or not (the
/// user asked; an adopted one after R31's check). By idle time, only those that could sleep
/// on their own (R18) after `idle_ms`. Names come first: with them, `idle_ms` is unused.
pub(crate) fn sleep(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    qs: &[String],
    idle_ms: Option<i64>,
    dry_run: bool,
) -> Result<Done, OpError> {
    if qs.is_empty() && idle_ms.is_none_or(|ms| ms < 0) {
        return Err(OpError(SLEEP_USAGE.into()));
    }
    ready(state)?;
    let ids: Vec<SessionId> = match idle_ms {
        Some(ms) if qs.is_empty() => state
            .sessions()
            .map(|s| s.id)
            .filter(|id| why_not(state, *id, now.wall, ms).is_none())
            .collect(),
        _ => {
            // Every name first: a bad one changes nothing.
            let mut ids = Vec::new();
            for q in qs {
                let id = state.resolve(q)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            // Its pane isn't known yet, or a resume of it would launch it after this.
            if ids.iter().any(|id| state.tmux().opening(*id)) {
                return Err(OpError(STILL_OPENING.into()));
            }
            ids
        }
    };
    // What isn't running here is said, and left as it is.
    let mut said = Vec::new();
    let mut running = Vec::new();
    for id in ids {
        let Some(s) = state.session(id) else {
            continue;
        };
        if s.lifecycle == Lifecycle::Live && bound_pane(state, id).is_some() {
            running.push(id);
        } else {
            let word = if is_asleep(s) {
                "asleep"
            } else {
                spike_state(s)
            };
            said.push(format!(
                "{} isn't running in moa ({word}); nothing to put to sleep",
                title_of(state, s)
            ));
        }
    }
    if dry_run {
        let mut lines: Vec<String> = running
            .iter()
            .filter_map(|id| state.session(*id))
            .map(|s| {
                format!(
                    "would sleep {}  {}",
                    state.handle_of(s.id),
                    title_of(state, s)
                )
            })
            .collect();
        lines.extend(said);
        if lines.is_empty() {
            lines.push("nothing to put to sleep".into());
        }
        return Ok(Done {
            text: lines.join("\n"),
            data: json!({ "would_sleep": running }),
        });
    }
    let mut changes = Vec::new();
    let mut panes = Vec::new();
    let mut slept = Vec::new();
    let mut names = Vec::new();
    for id in running {
        match kill_checked(state, id) {
            Ok(pane) => {
                panes.extend(pane);
                if let Some(s) = state.session(id) {
                    names.push(title_of(state, s));
                }
            }
            // An adopted agent that left its tab: asleep, and the tab left to the user.
            Err(gone) => said.push(gone.0),
        }
        // Asleep either way, so `asleep` lists it.
        slept.push(id);
        changes.extend(put_to_sleep(state, tx, id));
    }
    // One commit: a refusal leaves every session as it was.
    if !names.is_empty() {
        let s = if names.len() > 1 { "s" } else { "" };
        let text = format!(
            "put {} session{s} to sleep: {}",
            names.len(),
            names.join(", ")
        );
        tx.ui(state, changes, text)?;
    } else if !changes.is_empty() {
        tx.commit(state, changes)?;
    }
    for pane in panes {
        tx.tmux(kill_pane(pane));
    }
    if !names.is_empty() {
        said.push(format!(
            "asleep: {} (resumable: moactl resume ID)",
            names.join(", ")
        ));
    } else if said.is_empty() {
        said.push("nothing to put to sleep".into());
    }
    Ok(Done {
        text: said.join("\n"),
        data: json!({ "asleep": slept }),
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
