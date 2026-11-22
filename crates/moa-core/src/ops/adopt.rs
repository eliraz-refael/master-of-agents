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
    Activity, AgentKind, Conversation, ConversationReason, Group, Inspected, Lifecycle, Location,
    PaneAgent, PaneBinding, Session, SessionId, normalize_group_path, safe_id,
};
use crate::op::{ADOPT_USAGE, ConnId, Done, OpError, ReqId};
use crate::reconcile::{
    Recheck, moa_session_of, tag_pane, tagged_for, untag_pane, untracked_agent_runs,
};
use crate::route::{Route, route};
use crate::sleep::put_to_sleep;
use crate::state::State;
use crate::tmux::ids::{Generation, PaneId};
use crate::tmux::link::{Asked, Link, NO_TERMINAL, ready};
use crate::tmux::panes::{PaneInfo, kept_bindings, list_panes, listed, of_server};
use crate::{Now, UtcMs};

pub(super) const WENT_AWAY: &str = "moa's terminal went away; try again";
const TERMINAL: &str = "that tab is a terminal, not an agent";
const NOT_AN_AGENT: &str = "that tab isn't running an agent moa recognizes";
const NO_ACCOUNT: &str = "moa doesn't know the account that agent runs under; add it to moa.toml";
const NO_CONVERSATION: &str = "no conversation it wrote yet; ask it one thing first, or pass --id";
const LOST: &str = "moa lost that session; try again";
const UNREAD: &str = "moa couldn't read what runs in that tab; try again in a moment";
const NO_START: &str =
    "moa can't read when that agent started, so it couldn't tell it from a later one in that tab";

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
    // The bindings that hold their tab, an untracked one unless its agent is proven gone
    // (R31): one whose agent left is let go below, as `ls` would.
    let (kept, left): (Vec<&PaneBinding>, Vec<&PaneBinding>) =
        kept_bindings(state, generation, panes)
            .into_iter()
            .partition(|b| {
                b.tracked || untracked_agent_runs(state, b, listed(b, panes)) != Recheck::Gone
            });
    // The tabs of the session's own bindings that were let go, to take moa's tags off.
    let left: Vec<(SessionId, PaneId)> = left
        .iter()
        .filter_map(|b| {
            let i = listed(b, panes)?;
            (i.pane != pane && tagged_for(i, b.session_id)).then_some((b.session_id, i.pane))
        })
        .collect();
    let kept: Vec<SessionId> = kept.iter().map(|b| b.session_id).collect();
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
    let stale: Vec<SessionId> = state
        .pane_bindings()
        .filter(|b| of_server(b, generation) && b.pane_id.parse() == Ok(pane))
        .map(|b| b.session_id)
        .collect();

    // Its agent. A terminal running none is just the user's shell; one that can't be read
    // proves nothing either way.
    let deps = state.deps().clone();
    let agent = match (deps.inspect)(info.pid, &info.start) {
        Inspected::Unknown => return Err(OpError(UNREAD.into())),
        found => found.into_agent(),
    }
    .filter(|a| a.kind == AgentKind::Claude)
    .ok_or_else(|| OpError(if info.term { TERMINAL } else { NOT_AN_AGENT }.into()))?;
    // An agent moa started for a session it knows is that session's, not one to adopt. Its
    // tab is on its way to being bound, or is bound already; or its binding was never saved,
    // and then the tab is bound to it as the next connection's reconcile would (R6 (b)).
    if let Some(own) = moa_session_of(state, &agent) {
        if state.tmux().opening(own) {
            return Err(OpError(STILL_OPENING.into()));
        }
        if kept.contains(&own) {
            return Err(OpError(format!(
                "that tab is already {}",
                state.handle_of(own)
            )));
        }
        let given = args.id.as_deref().filter(|c| !c.is_empty());
        if given.is_some_and(|c| !state.conversations_of(own).any(|o| o.conversation_id == c)) {
            return Err(OpError(format!(
                "that agent was started for {}; adopt it without --id",
                state.handle_of(own)
            )));
        }
        let untracked = info.adopted || info.term;
        if untracked && unprovable(&agent) {
            return Err(OpError(NO_START.into()));
        }
        let stale = stale.into_iter().filter(|s| *s != own).collect();
        let tab = Tab {
            pane,
            pid: info.pid,
            generation,
            socket,
            untracked,
            marked: info.adopted,
            stale,
            left,
        };
        return bind_own(state, tx, own, args, group_flag, tab);
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
    let given = args.id.clone().filter(|c| !c.is_empty());
    // An agent naming no conversation is proven at each later look by its start time
    // (R31): with none, the binding `--id` makes would be let go at once.
    if given.is_some() && unprovable(&agent) {
        return Err(OpError(NO_START.into()));
    }
    let named = given.or_else(|| agent.conversation.clone());
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
    // The conversation the agent was started on, when `--id` names another (a `/resume` or
    // `/clear` inside it since): its command line still names that one, and every later look
    // at the tab reads it, so it is kept as one of the session's conversations too (R31).
    // One another session has is that session's agent: `--id` doesn't move it.
    let started_on = agent
        .conversation
        .clone()
        .filter(|c| *c != conv)
        .map(|c| {
            let owner = state
                .conversation_of_agent(AgentKind::Claude, &c)
                .map(|o| o.session_id);
            match owner {
                Some(o) if Some(o) != known => Err(OpError(format!(
                    "that agent was started on {}; adopt it without --id",
                    state.handle_of(o)
                ))),
                Some(_) => Ok(None),
                None if !safe_id(&c) => Err(OpError(UNSAFE_ID.into())),
                None => Ok(Some(c)),
            }
        })
        .transpose()?
        .flatten();

    // The session: the conversation's, woken here, or a new one.
    let store_text = acct_store.clone();
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
    if let Some(c) = started_on {
        // Seen before the adopted one, so that one stays current.
        let before = UtcMs(tx.wall.0.saturating_sub(1));
        let at = agent.started_at.map_or(before, |t| t.min(before));
        changes.push(Change::Conversation(Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: c,
            session_id: id,
            store_dir: Some(store_text),
            spike_account: None,
            reason: ConversationReason::Adopt,
            first_seen_at: at,
            last_seen_at: at,
            transcript_path: None,
            transcript_present: None,
            transcript_checked_at: None,
        }));
    }
    let title = session.title.clone().filter(|t| !t.trim().is_empty());
    let group = session.group_path.clone();
    let tab = Tab {
        pane,
        pid: info.pid,
        generation,
        socket,
        untracked: true,
        marked: info.adopted,
        stale,
        left,
    };
    let shown = title.clone().unwrap_or_else(|| state.handle_of(id));
    let row = format!("adopted {shown} ({name}, {cwd})");
    bind(state, tx, &session, changes, tab, row)?;

    Ok(Done {
        text: format!(
            "adopted {conv} ({name}) in {cwd} as {} [{}]",
            title.as_deref().unwrap_or("-"),
            group.as_deref().unwrap_or("-")
        ),
        data: json!({ "id": id, "title": title_of(state, &session) }),
    })
}

/// Whether an untracked binding of `agent` could never be proven again (R31): it names no
/// conversation, and its start time can't be read.
fn unprovable(agent: &PaneAgent) -> bool {
    agent.conversation.is_none() && agent.started_at.is_none()
}

/// The tab an adopt binds, as its listing showed it.
struct Tab {
    pane: PaneId,
    pid: i64,
    generation: Generation,
    socket: String,
    /// Its binding is untracked: an agent moa didn't start, or one in a tab marked adopted
    /// or a terminal, which may outlive it (R7).
    untracked: bool,
    /// It carries the adopted mark already.
    marked: bool,
    /// The sessions of bindings that name the tab but no longer hold it, put to sleep.
    stale: Vec<SessionId>,
    /// Tabs of untracked bindings whose agent left, each with its session: the bound
    /// session's lose moa's tags.
    left: Vec<(SessionId, PaneId)>,
}

/// One commit: `session` first, then `changes`, its group when new, the stale bindings'
/// sessions put to sleep, its binding to `tab` and the activity row `row`. Then the tab's
/// tags (adopted first when untracked), and a tab the session's agent left loses them.
fn bind(
    state: &mut State,
    tx: &mut Tx,
    session: &Session,
    mut changes: Vec<Change>,
    tab: Tab,
    row: String,
) -> Result<(), OpError> {
    let id = session.id;
    // The session goes first: the conversation and the binding point at it.
    changes.insert(0, Change::Session(session.clone()));
    if let Some(g) = session
        .group_path
        .clone()
        .filter(|g| !state.groups.contains_key(g))
    {
        changes.push(Change::Group(Group {
            path: g,
            collapsed: false,
            created_at: tx.wall,
        }));
    }
    for other in tab.stale.into_iter().filter(|s| *s != id) {
        changes.extend(put_to_sleep(state, tx, other));
    }
    changes.push(Change::PaneBinding(PaneBinding {
        session_id: id,
        server_pid: tab.generation.pid,
        server_start: tab.generation.start,
        socket: tab.socket,
        pane_id: tab.pane.to_string(),
        pane_pid: tab.pid,
        bound_at: tx.wall,
        tracked: !tab.untracked,
    }));
    tx.ui(state, changes, row)?;
    tag_pane(state, tx, tab.pane, id, tab.untracked && !tab.marked);
    for (_, pane) in tab.left.into_iter().filter(|(s, _)| *s == id) {
        untag_pane(state, tx, pane);
    }
    Ok(())
}

/// The tab of moa's own agent for session `own`, whose binding was never saved (its commit
/// was refused), bound to it as reconcile would (R6 (b)): tracked, unless the tab is marked
/// adopted or is a terminal. The session is live here again; `--title` and `--group` apply,
/// and `--id`, one of the session's conversations, becomes its current one, as adopt makes
/// it.
fn bind_own(
    state: &mut State,
    tx: &mut Tx,
    own: SessionId,
    args: &AdoptArgs,
    group_flag: Option<String>,
    tab: Tab,
) -> Result<Done, OpError> {
    let s = state
        .session(own)
        .cloned()
        .ok_or_else(|| OpError(LOST.into()))?;
    let title_flag = args.title.clone().filter(|t| !t.trim().is_empty());
    let session = Session {
        title: title_flag.clone().or(s.title.clone()),
        title_locked: s.title_locked || title_flag.is_some(),
        group_path: group_flag.or(s.group_path.clone()),
        lifecycle: Lifecycle::Live,
        asleep_at: None,
        location: Location::Here,
        ..s
    };
    let current = args
        .id
        .as_deref()
        .filter(|c| !c.is_empty())
        .and_then(|c| state.conversations_of(own).find(|o| o.conversation_id == c))
        .map(|c| {
            Change::Conversation(Conversation {
                last_seen_at: tx.wall,
                ..c.clone()
            })
        });
    let title = title_of(state, &session);
    let pane = tab.pane;
    let tracks = if tab.untracked { "" } else { "; moa tracks it" };
    bind(
        state,
        tx,
        &session,
        current.into_iter().collect(),
        tab,
        format!("bound {title} to tab {pane}"),
    )?;
    Ok(Done {
        text: format!("bound {title} to tab {pane}{tracks}"),
        data: json!({ "id": own, "title": title }),
    })
}
