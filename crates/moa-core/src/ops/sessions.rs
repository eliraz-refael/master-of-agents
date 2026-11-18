//! `session ls|set|archive|open`.

use std::path::Path;

use serde_json::json;

use super::cut;
use super::projects::project_for;
use crate::Now;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine, TmuxToken};
use crate::hooks::end_exited;
use crate::live::{display_state, is_asleep, spike_state};
use crate::model::{
    Activity, AgentKind, Conversation, ConversationReason, Group, Lifecycle, Location, PaneBinding,
    Session, SessionId, normalize_group_path,
};
use crate::names::{is_live_spike, newest_first, place, title_or_prompt};
use crate::op::{Done, OpError};
use crate::route::{Route, route};
use crate::state::State;
use crate::time::{days_ago, local_text};
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Link, OPENED_FORMAT, OpenEnd, Settled};

/// How much of the last prompt stands in for a missing title.
const TITLE_CUT: usize = 40;
/// How much of a title names its tab, as the spike cut it.
const WINDOW_NAME_CUT: usize = 24;

const NO_TERMINAL: &str = "moa isn't open in a terminal yet; run moa first";
const NOT_FULL: &str = "open needs a full path";
const NO_ACCOUNT: &str = "no account to open with: set open_account in moa.toml";
const ONLY_CLAUDE: &str = "only Claude accounts can open sessions for now";
const NO_TAB: &str = "couldn't open a tab for it; see moa's log";
const WENT_AWAY: &str = "moa's terminal went away before it opened";
const NOT_SAVED: &str = "moa couldn't save that session; nothing was opened, try again";
const LOST: &str = "moa lost that session; a tab may have opened for it, see moa's log";

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

/// What `open` was asked.
pub(crate) struct OpenArgs<'a> {
    pub path: &'a Path,
    pub account: Option<&'a str>,
    pub title: Option<&'a str>,
    pub group: Option<&'a str>,
    pub prompt: Option<&'a str>,
}

/// `open`: checks the request, commits the new session, and gives the tokened `new-window`
/// that starts it in a tab of moa's terminal, sent once the session is saved. The caller
/// holds the reply until tmux says how the tab turned out ([`opened`]), or a reload finds
/// the session wasn't saved ([`lost_opens`]).
pub(crate) fn open(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    a: &OpenArgs,
) -> Result<(SessionId, Effect), OpError> {
    let Link::Up { conn, .. } = state.tmux().link else {
        return Err(OpError(NO_TERMINAL.into()));
    };
    if !a.path.is_absolute() {
        return Err(OpError(NOT_FULL.into()));
    }
    let name = match a.account {
        Some(n) => n.to_string(),
        None => state
            .config()
            .brain(state.brain())
            .and_then(|b| b.open_account)
            .map(|n| n.to_string())
            .ok_or_else(|| OpError(NO_ACCOUNT.into()))?,
    };
    let no_such = || OpError(format!("there's no account called {name}"));
    let account = AccountName::parse(&name).map_err(|_| no_such())?;
    let acct = state
        .config()
        .accounts
        .get(&account)
        .ok_or_else(no_such)?
        .clone();
    if acct.kind != AgentKind::Claude {
        return Err(OpError(ONLY_CLAUDE.into()));
    }
    let launcher = acct.launcher.clone().ok_or_else(|| {
        OpError(format!(
            "can't find claude for {name}: set its launcher in moa.toml"
        ))
    })?;
    match route(state.config(), Some(&account), a.path, Some(state.brain())) {
        Route::Brain(b) if b != *state.brain() => {
            return Err(OpError(format!(
                "that folder belongs to the {b} brain; open it there"
            )));
        }
        Route::Brain(_) => {}
        Route::NotAvailable => {
            return Err(OpError(format!(
                "{name} can't open sessions in that folder"
            )));
        }
    }
    let group = match a.group.map(normalize_group_path) {
        Some(g) if g.is_empty() => return Err(OpError("a group needs a name".into())),
        other => other,
    };
    let given = a.title.filter(|t| !t.trim().is_empty());
    let title = given
        .map(str::to_string)
        .or_else(|| a.path.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "agent".to_string());

    let new_uuid = state.deps().new_uuid;
    let id = SessionId(new_uuid(now.wall));
    let conv = new_uuid(now.wall).to_string();
    let path = a.path.display().to_string();
    let store = acct.store.display().to_string();
    let session = Session {
        id,
        agent_kind: AgentKind::Claude,
        account: Some(name.clone()),
        cwd: Some(path.clone()),
        start_cwd: Some(path.clone()),
        project_id: project_for(state, a.path),
        title: Some(title.clone()),
        title_locked: given.is_some(),
        group_path: group.clone(),
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(now.wall),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        // Nothing in it is unseen yet. Without a `seen_at` a turn's end would never show
        // as done (`display_state`'s rule).
        seen_at: Some(now.wall),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: now.wall,
        last_seen_at: Some(now.wall),
    };
    let conversation = Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conv.clone(),
        session_id: id,
        store_dir: Some(store.clone()),
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: now.wall,
        last_seen_at: now.wall,
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    };
    let mut changes = vec![Change::Session(session), Change::Conversation(conversation)];
    if let Some(g) = group.filter(|g| !state.groups.contains_key(g)) {
        changes.push(Change::Group(Group {
            path: g,
            collapsed: false,
            created_at: now.wall,
        }));
    }
    tx.ui(state, changes, format!("opened {title} with {name}"))?;

    let env = |k: &str, v: &str| ["-e".to_string(), format!("{k}={v}")];
    let plugin = state.deps().session_plugin.display().to_string();
    // tmux expands `-n` and `-c` as formats: cut first, so a doubled `#` isn't split and
    // the name shows at most 24 characters. `-e` values and the argv stay as they are.
    let mut cmd = Cmd::new("new-window")
        .args(["-d", "-P", "-F", OPENED_FORMAT, "-n"])
        .arg(literal_format(&cut(&title, WINDOW_NAME_CUT)))
        .arg("-c")
        .arg(literal_format(&path))
        .args(["-t", "=moa:"])
        .args(env("CLAUDE_CONFIG_DIR", &store))
        .args(env("MOA_SESSION", &id.to_string()))
        .args(env("MOA_BRAIN", state.brain().as_str()))
        .args(env("CLAUDE_CODE_PLUGIN_DIRS", &plugin))
        .arg(launcher.display().to_string())
        .args(["--session-id", conv.as_str(), "--name", title.as_str()]);
    if let Some(p) = a.prompt.filter(|p| !p.is_empty()) {
        cmd = cmd.args(["--", p]);
    }
    Ok((
        id,
        Effect::Tmux {
            conn,
            cmds: vec![cmd],
            token: Some(TmuxToken::Open(id)),
        },
    ))
}

fn warn(message: String) -> Effect {
    Effect::Log(LogLine {
        level: LogLevel::Warn,
        message,
    })
}

/// The answers to `open`s whose tab tmux reported on, or that the link went away from:
/// each pane bound and tagged, or its session ended, in one commit; then the replies.
pub(crate) fn opened(state: &mut State, now: &Now, settled: Vec<Settled>) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let mut logs = Vec::new();
    let mut after = Vec::new();
    for Settled { id, reply, end } in settled {
        let result = match end {
            OpenEnd::Bound { pane, pid } => {
                bind(state, &mut tx, id, pane, pid, &mut logs, &mut after)
            }
            OpenEnd::Failed(why) => {
                // tmux's first line and ids only: no record contents.
                logs.push(warn(match why {
                    Some(line) => format!("tmux couldn't open a tab for session {id}: {line}"),
                    None => format!("tmux's answer about the tab for session {id} didn't parse"),
                }));
                end_open(state, &mut tx, id, true, &mut logs);
                Err(OpError(NO_TAB.into()))
            }
            OpenEnd::Gone => {
                end_open(state, &mut tx, id, false, &mut logs);
                Err(OpError(WENT_AWAY.into()))
            }
        };
        // A caller that went has nobody to tell; its tab was still bound or ended above.
        if let Some((conn, req)) = reply {
            after.push(Effect::Reply { conn, req, result });
        }
    }
    let mut fx = logs;
    fx.extend(post_commit(state, tx));
    fx.extend(after);
    fx
}

/// The `open`s whose session a reload didn't find: their commit failed, so their new window
/// was dropped with it and no tab opened. Each is answered so.
pub(crate) fn lost_opens(state: &mut State) -> Vec<Effect> {
    let lost: Vec<SessionId> = state
        .tmux
        .waiting_opens()
        .filter(|id| state.session(*id).is_none())
        .collect();
    lost.into_iter()
        .filter_map(|id| state.tmux.forget_open(id))
        .map(|(conn, req)| Effect::Reply {
            conn,
            req,
            result: Err(OpError(NOT_SAVED.into())),
        })
        .collect()
}

/// Binds the session to its new pane and tags the pane with it (after the commit, in
/// `after`), then the reply.
fn bind(
    state: &mut State,
    tx: &mut Tx,
    id: SessionId,
    pane: PaneId,
    pid: i64,
    logs: &mut Vec<Effect>,
    after: &mut Vec<Effect>,
) -> Result<Done, OpError> {
    // `settle` reports a pane only while the link it came on is up.
    let Link::Up {
        conn,
        generation,
        socket,
        ..
    } = &state.tmux().link
    else {
        end_open(state, tx, id, false, logs);
        return Err(OpError(WENT_AWAY.into()));
    };
    let (conn, generation, socket) = (*conn, *generation, socket.clone());
    // The new window is sent only once the session is saved, and a reload that loses it
    // answers the open then ([`lost_opens`]), so this shouldn't happen. If it does, a tab
    // has opened that nothing tracks: the reply says so.
    let Some(s) = state.session(id).cloned() else {
        logs.push(warn(format!(
            "session {id} got a tab but isn't in the brain"
        )));
        return Err(OpError(LOST.into()));
    };
    if state.tmux.take_exited(pane) {
        // Its agent exited before this answer came: it ends as `pane_gone` would end it,
        // and no binding is made. The tab did open, so the reply is the one an exit right
        // after this answer would have got; the session shows as ended. A refused write
        // leaves it live with no binding at all: reconcile (4b) ends such sessions.
        if !end_exited(state, tx, id, Vec::new()) {
            logs.push(warn(format!("the end of session {id} was not saved")));
        }
        return Ok(opened_reply(s));
    }
    let binding = PaneBinding {
        session_id: id,
        server_pid: generation.pid,
        server_start: generation.start,
        socket,
        pane_id: pane.to_string(),
        pane_pid: pid,
        bound_at: tx.wall,
        tracked: true,
    };
    if let Err(e) = tx.commit(state, vec![Change::PaneBinding(binding)]) {
        logs.push(warn(format!("the tab of session {id} was not saved")));
        return Err(e);
    }
    after.push(Effect::Tmux {
        conn,
        cmds: vec![Cmd::new("set-option").args([
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "@moa_session".to_string(),
            id.to_string(),
        ])],
        token: None,
    });
    Ok(opened_reply(s))
}

/// `opened <title> with <account> in <path>`, with the id and title for scripts.
fn opened_reply(s: Session) -> Done {
    let title = s.title.unwrap_or_default();
    Done {
        text: format!(
            "opened {title} with {} in {}",
            s.account.unwrap_or_default(),
            s.cwd.unwrap_or_default()
        ),
        data: json!({ "id": s.id, "title": title }),
    }
}

/// An `open` that got no tab: its session ends, marked so when tmux refused it.
fn end_open(state: &mut State, tx: &mut Tx, id: SessionId, failed: bool, logs: &mut Vec<Effect>) {
    let Some(s) = state.session(id).filter(|s| s.lifecycle == Lifecycle::Live) else {
        return;
    };
    let mut s = s.clone();
    s.lifecycle = Lifecycle::Ended;
    s.resume_failed |= failed;
    if tx.commit(state, vec![Change::Session(s)]).is_err() {
        logs.push(warn(format!("the end of session {id} was not saved")));
    }
}
