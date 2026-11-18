//! `adopt %N` (R21): an agent the user started in a tab by hand, taken on as a session moa
//! doesn't track. One listing of the tabs, then the checks, then one commit and the tab's
//! tags.

use std::path::Path;

use serde_json::json;

use super::launch::{UNSAFE_ID, title_of};
use super::projects::project_for;
use super::sessions::STILL_OPENING;
use crate::change::Change;
use crate::core::{Tx, finish_op};
use crate::effect::Effect;
use crate::model::{
    Activity, AgentKind, Conversation, ConversationReason, Group, Lifecycle, Location, PaneBinding,
    Session, SessionId, normalize_group_path, safe_id,
};
use crate::op::{ADOPT_USAGE, ConnId, Done, OpError, ReqId};
use crate::reconcile::tag_pane;
use crate::route::{Route, route};
use crate::sleep::put_to_sleep;
use crate::state::State;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Asked, Link, NO_TERMINAL, ready};
use crate::tmux::panes::{PaneInfo, kept_bindings, list_panes};
use crate::Now;

const WENT_AWAY: &str = "moa's terminal went away; try again";
const TERMINAL: &str = "that tab is a terminal, not an agent";
const NOT_AN_AGENT: &str = "that tab isn't running an agent moa recognizes";
const NO_ACCOUNT: &str = "moa doesn't know the account that agent runs under; add it to moa.toml";
const NO_CONVERSATION: &str = "no conversation it wrote yet; ask it one thing first, or pass --id";
const LOST: &str = "moa lost that session; try again";

/// What `adopt` was asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AdoptArgs {
    pub pane: PaneId,
    pub title: Option<String>,
    pub group: Option<String>,
    pub id: Option<String>,
}

/// `adopt PANE`: once the tabs are reconciled (R10), one listing of them is asked;
/// [`adopted`] goes on with its answer.
pub(crate) fn start(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    pane: &str,
    title: Option<String>,
    group: Option<String>,
    id: Option<String>,
) -> Vec<Effect> {
    let reply = |text: &str| Effect::Reply {
        conn,
        req,
        result: Err(OpError(text.into())),
    };
    let Ok(pane) = pane.parse::<PaneId>() else {
        return vec![reply(ADOPT_USAGE)];
    };
    if let Err(e) = ready(state) {
        return vec![reply(&e.0)];
    }
    let args = AdoptArgs {
        pane,
        title,
        group,
        id,
    };
    match state
        .tmux
        .ask(Asked::Adopt { conn, req, args }, list_panes())
    {
        Some(query) => vec![query],
        None => vec![reply(NO_TERMINAL)],
    }
}

/// An adopt's listing came back, or failed (the link went before tmux answered).
pub(crate) fn adopted(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    args: &AdoptArgs,
    panes: Result<Vec<PaneInfo>, ()>,
) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let result = match panes {
        Ok(panes) => adopt(state, &mut tx, now, args, &panes),
        Err(()) => Err(OpError(WENT_AWAY.into())),
    };
    finish_op(state, tx, conn, req, result)
}

/// Whether `folder` (canonical) is one of this brain's (R22): the brain that owns it is this
/// one, or no brain owns it and this one lists no folders.
pub(crate) fn brains_folder(state: &State, folder: &Path) -> bool {
    match state.config().brain_for_folder(folder) {
        Some(owner) => owner == *state.brain(),
        None => state
            .config()
            .brain(state.brain())
            .is_none_or(|b| b.folders.is_empty()),
    }
}

/// R21, against `panes`, the listing asked for it: the tab, its agent, the folder, account
/// and brain, then the conversation. Then one commit: the session (a known conversation's,
/// else a new one), its untracked binding, a stale binding of the tab freed (its session put
/// to sleep), and the activity row. Then the tab's tags, adopted first.
pub(crate) fn adopt(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    args: &AdoptArgs,
    panes: &[PaneInfo],
) -> Result<Done, OpError> {
    ready(state)?;
    let Link::Up {
        generation, socket, ..
    } = &state.tmux().link
    else {
        return Err(OpError(NO_TERMINAL.into()));
    };
    let (generation, socket) = (*generation, socket.clone());
    let pane = args.pane;
    let group_flag = match args.group.as_deref().map(normalize_group_path) {
        Some(g) if g.is_empty() => return Err(OpError("a group needs a name".into())),
        other => other,
    };

    // The tab: listed, alive, and no binding holds it (R4). A terminal moa opened is where
    // an agent is typed by hand, so it may be one; its agent is read below.
    let info = panes
        .iter()
        .find(|p| p.pane == pane && !p.dead)
        .ok_or_else(|| OpError(format!("there's no tab {pane}")))?;
    let kept: Vec<SessionId> = kept_bindings(state, generation, panes)
        .iter()
        .map(|b| b.session_id)
        .collect();
    if let Some(b) = state
        .pane_bindings()
        .find(|b| kept.contains(&b.session_id) && b.pane_id.parse() == Ok(pane))
    {
        return Err(OpError(format!(
            "that tab is already {}",
            state.handle_of(b.session_id)
        )));
    }
    // A binding of this server that names the tab but no longer holds it.
    let this_server =
        |b: &PaneBinding| b.server_pid == generation.pid && b.server_start == generation.start;
    let stale: Vec<SessionId> = state
        .pane_bindings()
        .filter(|b| this_server(b) && b.pane_id.parse() == Ok(pane))
        .map(|b| b.session_id)
        .collect();

    // Its agent. A terminal running none is just the user's shell.
    let deps = state.deps().clone();
    let agent = (deps.inspect)(info.pid, &info.start)
        .filter(|a| a.kind == AgentKind::Claude)
        .ok_or_else(|| OpError(if info.term { TERMINAL } else { NOT_AN_AGENT }.into()))?;
    // An agent moa started for a session it knows is that session's, not one to adopt: its
    // tab is on its way to being bound, or is bound already.
    if let Some(own) = agent
        .moa_session
        .as_deref()
        .and_then(|s| s.parse::<SessionId>().ok())
        .filter(|id| state.session(*id).is_some())
    {
        if state.tmux().opening(own) {
            return Err(OpError(STILL_OPENING.into()));
        }
        return Err(OpError(format!(
            "that tab is already {}",
            state.handle_of(own)
        )));
    }

    // The brain's folder (R22), a listed account's store (R32), and this brain's route.
    let cwd = info.cwd.clone();
    let folder = (deps.canonical)(Path::new(&cwd));
    if !brains_folder(state, &folder) {
        return Err(OpError(format!("{cwd} isn't in this brain's folders")));
    }
    let store = state.store_of(agent.store_dir.as_deref());
    // The account, and its store as moa.toml spells it (`open` records it so).
    let (account, acct_store) = state
        .account_of_store(&store)
        .and_then(|name| {
            let acct_store = state
                .config()
                .accounts
                .get(&name)?
                .store
                .display()
                .to_string();
            Some((name, acct_store))
        })
        .ok_or_else(|| OpError(NO_ACCOUNT.into()))?;
    let name = account.to_string();
    match route(state.config(), Some(&account), &folder, Some(state.brain())) {
        Route::Brain(b) if b != *state.brain() => {
            return Err(OpError(format!(
                "that folder belongs to the {b} brain; adopt it there"
            )));
        }
        Route::Brain(_) => {}
        Route::NotAvailable => {
            return Err(OpError(format!("{name} can't run sessions in that folder")));
        }
    }

    // The conversation: given, else the agent's own, else the newest it wrote since it
    // started that no kept tab's session holds. With no start time, "since it started" can't
    // be told, and an older free conversation would be taken for its own: none is looked for.
    let held: Vec<String> = kept
        .iter()
        .flat_map(|id| state.conversations_of(*id))
        .map(|c| c.conversation_id.clone())
        .collect();
    let named = args
        .id
        .clone()
        .filter(|c| !c.is_empty())
        .or_else(|| agent.conversation.clone());
    if named.as_deref().is_some_and(|c| !safe_id(c)) {
        return Err(OpError(UNSAFE_ID.into()));
    }
    let conv = named
        .or_else(|| {
            let since = agent.started_at?;
            (deps.newest_conversation)(&store, &folder, &held, Some(since))
        })
        .ok_or_else(|| OpError(NO_CONVERSATION.into()))?;
    let known = state
        .conversation_of_agent(AgentKind::Claude, &conv)
        .map(|c| c.session_id);
    if let Some(owner) = known.filter(|id| kept.contains(id)) {
        return Err(OpError(format!(
            "that conversation is already running in another tab ({})",
            state.handle_of(owner)
        )));
    }
    // A resume of it is about to open it in a tab of its own.
    if known.is_some_and(|id| state.tmux().opening(id)) {
        return Err(OpError(STILL_OPENING.into()));
    }

    // The session: the conversation's, woken here, or a new one.
    let title_flag = args.title.clone().filter(|t| !t.trim().is_empty());
    let mut changes = Vec::new();
    let session = match known {
        Some(id) => {
            let s = state
                .session(id)
                .cloned()
                .ok_or_else(|| OpError(LOST.into()))?;
            // The adopted conversation is the session's current one from now on, so `ls`
            // shows it and `restart` resumes it: the current one is the latest an agent
            // started on (`State::current_conversation`).
            if let Some(c) = state.conversation_of_agent(AgentKind::Claude, &conv) {
                changes.push(Change::Conversation(Conversation {
                    last_seen_at: tx.wall,
                    ..c.clone()
                }));
            }
            Session {
                title: title_flag.clone().or(s.title.clone()),
                title_locked: s.title_locked || title_flag.is_some(),
                group_path: group_flag.clone().or(s.group_path.clone()),
                cwd: Some(cwd.clone()),
                lifecycle: Lifecycle::Live,
                asleep_at: None,
                location: Location::Here,
                resume_failed: false,
                // It sends no hooks: what it was doing before says nothing about now.
                activity: Activity::Idle,
                activity_why: None,
                activity_at: Some(tx.wall),
                last_seen_at: Some(tx.wall),
                ..s
            }
        }
        None => {
            let given = title_flag
                .clone()
                .or_else(|| agent.title.clone().filter(|t| !t.trim().is_empty()));
            let title = given
                .clone()
                .or_else(|| {
                    Path::new(&cwd)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "agent".to_string());
            let id = SessionId((deps.new_uuid)(now.wall));
            changes.push(Change::Conversation(Conversation {
                agent_kind: AgentKind::Claude,
                conversation_id: conv.clone(),
                session_id: id,
                store_dir: Some(acct_store),
                spike_account: None,
                reason: ConversationReason::Adopt,
                first_seen_at: tx.wall,
                last_seen_at: tx.wall,
                transcript_path: None,
                transcript_present: None,
                transcript_checked_at: None,
            }));
            Session {
                id,
                agent_kind: AgentKind::Claude,
                account: Some(name.clone()),
                cwd: Some(cwd.clone()),
                start_cwd: Some(cwd.clone()),
                project_id: project_for(state, &folder),
                title: Some(title),
                title_locked: given.is_some(),
                group_path: group_flag.clone().or_else(|| {
                    agent
                        .group
                        .as_deref()
                        .map(normalize_group_path)
                        .filter(|g| !g.is_empty())
                }),
                lifecycle: Lifecycle::Live,
                activity: Activity::Idle,
                activity_at: Some(tx.wall),
                activity_why: None,
                last_prompt: None,
                last_turn_end_at: None,
                // Nothing in it is unseen yet.
                seen_at: Some(tx.wall),
                asleep_at: None,
                archived_at: None,
                location: Location::Here,
                resume_failed: false,
                created_at: tx.wall,
                last_seen_at: Some(tx.wall),
            }
        }
    };
    let id = session.id;
    let title = session.title.clone().filter(|t| !t.trim().is_empty());
    let group = session.group_path.clone();
    // The session goes first: the conversation and the binding point at it.
    changes.insert(0, Change::Session(session.clone()));
    if let Some(g) = group.clone().filter(|g| !state.groups.contains_key(g)) {
        changes.push(Change::Group(Group {
            path: g,
            collapsed: false,
            created_at: tx.wall,
        }));
    }
    for other in stale.into_iter().filter(|s| *s != id) {
        changes.extend(put_to_sleep(state, tx, other));
    }
    changes.push(Change::PaneBinding(PaneBinding {
        session_id: id,
        server_pid: generation.pid,
        server_start: generation.start,
        socket,
        pane_id: pane.to_string(),
        pane_pid: info.pid,
        bound_at: tx.wall,
        tracked: false,
    }));
    let shown = title.clone().unwrap_or_else(|| state.handle_of(id));
    tx.ui(state, changes, format!("adopted {shown} ({name}, {cwd})"))?;
    tag_pane(state, tx, pane, id, true);

    Ok(Done {
        text: format!(
            "adopted {conv} ({name}) in {cwd} as {} [{}]",
            title.as_deref().unwrap_or("-"),
            group.as_deref().unwrap_or("-")
        ),
        data: json!({ "id": id, "title": title_of(state, &session) }),
    })
}
