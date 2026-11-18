//! Agents in new tabs of moa's terminal: `open`, `resume` (and later restart) through one
//! window builder (R11), the answers once tmux says how each tab turned out, and `focus`.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::cut;
use super::projects::project_for;
use crate::Now;
use crate::change::Change;
use crate::config::{AccountConfig, AccountName};
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, TmuxToken};
use crate::hooks::{asleep, lost_agent};
use crate::model::{
    Activity, AgentKind, Conversation, ConversationReason, Group, Lifecycle, Location, PaneBinding,
    Session, SessionId, TranscriptCheck, normalize_group_path, project_slot,
};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::reconcile::on_this_server;
use crate::route::{Route, route};
use crate::state::State;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::focus::{bound_pane, bound_session};
use crate::tmux::ids::PaneId;
use crate::tmux::link::{
    Answer, Asked, LaunchKind, Launching, Link, NO_TERMINAL, OPENED_FORMAT, OpenEnd, Settled, log,
    opened_pane, ready, set_pane_option,
};
use crate::tmux::panes::{PaneInfo, list_panes};

/// How much of a title names its tab, as the spike cut it.
const WINDOW_NAME_CUT: usize = 24;

const NOT_FULL: &str = "open needs a full path";
const NO_ACCOUNT: &str = "no account to open with: set open_account in moa.toml";
const ONLY_CLAUDE: &str = "only Claude accounts can open sessions for now";
const NO_TAB: &str = "couldn't open a tab for it; see moa's log";
const WENT_AWAY: &str = "moa's terminal went away before it opened";
const NOT_SAVED: &str = "moa couldn't save that session; nothing was opened, try again";
const LOST: &str = "moa lost that session; a tab may have opened for it, see moa's log";
const UNTRACKED: &str =
    "moa couldn't save which tab it runs in; it may show as untracked until moa restarts";
const ELSEWHERE: &str = "it's running in another app right now; close it there first";
const OTHER_TERMINAL: &str =
    "it's running in another moa terminal; use it there, or close it there first";
const NO_CONVERSATION: &str =
    "moa doesn't know which conversation it had; start it fresh with moactl open";
const NOT_LISTED: &str =
    "the account its conversation was recorded in isn't in moa.toml; add it there to resume";
const NO_FOLDER: &str = "moa doesn't know which folder it ran in; start it fresh with moactl open";
const UNSAFE_ID: &str = "that conversation's id isn't one moa can resume";
const TERM_NOT_FULL: &str = "term needs a full path";
const NO_SHELL: &str = "couldn't open a terminal; see moa's log";

/// The conversation a launch runs: a new one under this id, or an old one resumed.
pub(crate) enum Conv<'a> {
    Fresh(&'a str),
    Resume(&'a str),
}

/// What one launch runs, and where.
pub(crate) struct LaunchSpec<'a> {
    pub id: SessionId,
    pub title: &'a str,
    pub folder: &'a str,
    pub store: &'a Path,
    pub launcher: &'a Path,
    pub conv: Conv<'a>,
    pub prompt: Option<&'a str>,
}

/// How a launch gives Claude its store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfigDirMode {
    /// `CLAUDE_CONFIG_DIR=<store>`, whatever the store: 4a's rule, kept working by a test
    /// in case the ruling changes.
    #[allow(dead_code)]
    Set,
    /// The same, except for Claude's own default store (`<home>/.claude`), which runs with
    /// the variable unset: Claude keys its login on it.
    UnsetForDefault,
}

/// R11, the human's ruling: Claude's own default store runs without the variable.
pub(crate) const CONFIG_DIR_MODE: ConfigDirMode = ConfigDirMode::UnsetForDefault;

/// The `-e` arguments that give a launch its store, and the words that go before the
/// launcher: none, or `env -u CLAUDE_CONFIG_DIR` for the default store, unset explicitly
/// because the terminal server's own environment may hold a value. `canonical` spells both
/// folders the one way (`Deps::canonical`) before they are compared.
pub(crate) fn launch_env(
    store: &Path,
    home: &Path,
    mode: ConfigDirMode,
    canonical: fn(&Path) -> PathBuf,
) -> (Vec<String>, Vec<String>) {
    let default = mode == ConfigDirMode::UnsetForDefault
        && canonical(store) == canonical(&home.join(".claude"));
    if default {
        let prefix = ["/usr/bin/env", "-u", "CLAUDE_CONFIG_DIR"];
        (Vec::new(), prefix.map(str::to_string).to_vec())
    } else {
        let env = vec![
            "-e".to_string(),
            format!("CLAUDE_CONFIG_DIR={}", store.display()),
        ];
        (env, Vec::new())
    }
}

/// The tokened `new-window` that starts `spec` in a new tab of moa's terminal, on
/// connection `conn`. Its answer comes back as `TmuxToken::Open(spec.id)`.
pub(crate) fn new_window(state: &State, conn: u64, spec: &LaunchSpec) -> Effect {
    let env = |k: &str, v: &str| ["-e".to_string(), format!("{k}={v}")];
    let deps = state.deps();
    let (store_env, prefix) = launch_env(spec.store, &deps.home, CONFIG_DIR_MODE, deps.canonical);
    let plugin = deps.session_plugin.display().to_string();
    let (flag, conv) = match spec.conv {
        Conv::Fresh(c) => ("--session-id", c),
        Conv::Resume(c) => ("--resume", c),
    };
    // tmux expands `-n` and `-c` as formats: cut first, so a doubled `#` isn't split and
    // the name shows at most 24 characters. `-e` values and the argv stay as they are.
    let mut cmd = Cmd::new("new-window")
        .args(["-d", "-P", "-F", OPENED_FORMAT, "-n"])
        .arg(literal_format(&cut(spec.title, WINDOW_NAME_CUT)))
        .arg("-c")
        .arg(literal_format(spec.folder))
        .args(["-t", "=moa:"])
        .args(store_env)
        .args(env("MOA_SESSION", &spec.id.to_string()))
        .args(env("MOA_BRAIN", state.brain().as_str()))
        .args(env("CLAUDE_CODE_PLUGIN_DIRS", &plugin))
        .args(prefix)
        .arg(spec.launcher.display().to_string())
        .args([flag, conv, "--name", spec.title]);
    if let Some(p) = spec.prompt.filter(|p| !p.is_empty()) {
        cmd = cmd.args(["--", p]);
    }
    Effect::Tmux {
        conn,
        cmds: vec![cmd],
        token: Some(TmuxToken::Open(spec.id)),
    }
}

/// Brings the tab of `pane` to the screen: its window, then the pane in it.
pub(crate) fn focus_cmds(pane: PaneId) -> [Cmd; 2] {
    let target = pane.to_string();
    [
        Cmd::new("select-window").args(["-t", target.as_str()]),
        Cmd::new("select-pane").args(["-t", target.as_str()]),
    ]
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
    let launcher = launcher_of(&name, &acct)?;
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
        store_dir: Some(store),
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

    let spec = LaunchSpec {
        id,
        title: &title,
        folder: &path,
        store: &acct.store,
        launcher: &launcher,
        conv: Conv::Fresh(&conv),
        prompt: a.prompt,
    };
    Ok((id, new_window(state, conn, &spec)))
}

/// The launcher of a Claude account (rule 6), or why it has none.
fn launcher_of(name: &str, acct: &AccountConfig) -> Result<PathBuf, OpError> {
    if acct.kind != AgentKind::Claude {
        return Err(OpError(ONLY_CLAUDE.into()));
    }
    acct.launcher.clone().ok_or_else(|| {
        OpError(format!(
            "can't find claude for {name}: set its launcher in moa.toml"
        ))
    })
}

/// A resume whose checks passed (R13 steps 1–5): what it launches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Checked {
    pub id: SessionId,
    pub conv: String,
    pub folder: String,
    pub store: PathBuf,
    pub launcher: PathBuf,
    pub title: String,
}

/// Where a resume's checks leave it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Resume {
    /// It runs in this pane: it is only brought to the screen (step 1).
    Running(SessionId, PaneId),
    /// Its tab is opening: the caller waits for it.
    Waiting(SessionId),
    /// It can be launched, once the tabs show none running its conversation (step 6).
    Check(Checked),
}

/// A resume waiting for its pane query (R13 step 6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ResumeAsk {
    pub conn: ConnId,
    pub req: ReqId,
    pub checked: Checked,
    pub kind: LaunchKind,
    /// The tab comes to the screen once it is bound.
    pub focus: bool,
    /// `focus` asked: the caller hears where it went, as a focus waiter does.
    pub via_focus: bool,
}

/// The listed account whose store the conversation was recorded in (R32: no store is
/// Claude's own `<home>/.claude`), compared as canonical folders: `open` records a store as
/// moa.toml spells it, a hook's rotation as the canonical path.
pub(crate) fn account_of(
    state: &State,
    conv: &Conversation,
) -> Result<(AccountName, AccountConfig), OpError> {
    let store = state.store_of(conv.store_dir.as_deref());
    state
        .account_of_store(&store)
        .and_then(|name| {
            let acct = state.config().accounts.get(&name)?.clone();
            Some((name, acct))
        })
        .ok_or_else(|| OpError(NOT_LISTED.into()))
}

/// The folder a resume runs in (R13 step 4): of the session's `cwd` and `start_cwd`, the one
/// whose Claude project slot holds the conversation's transcript, since Claude finds a
/// conversation by the folder it began in; else `start_cwd`, else `cwd`. Claude names the
/// slot after its real folder, so a folder matches in its recorded spelling or its
/// `canonical` one; the recorded one is launched.
pub(crate) fn resume_folder(
    s: &Session,
    conv: &Conversation,
    canonical: fn(&Path) -> PathBuf,
) -> Option<String> {
    let known = |f: &Option<String>| f.clone().filter(|f| !f.is_empty());
    let (cwd, start) = (known(&s.cwd), known(&s.start_cwd));
    let slot = conv
        .transcript_path
        .as_deref()
        .map(Path::new)
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|f| f.to_string_lossy().into_owned());
    let holds = |f: &Option<String>| {
        let (Some(f), Some(slot)) = (f.as_deref(), slot.as_deref()) else {
            return false;
        };
        let f = Path::new(f);
        project_slot(f) == slot || project_slot(&canonical(f)) == slot
    };
    if holds(&start) {
        return start;
    }
    if holds(&cwd) {
        return cwd;
    }
    start.or(cwd)
}

/// `resume Q` (C11), R13 steps 1–5, once the tabs are reconciled (R10). A missing transcript
/// commits `resume_failed` with its refusal (rule 3), so the caller says what `tx` holds
/// whatever comes back, after `logs`.
pub(crate) fn resume(
    state: &mut State,
    tx: &mut Tx,
    logs: &mut Vec<Effect>,
    q: &str,
    kind: LaunchKind,
) -> Result<Resume, OpError> {
    ready(state)?;
    let id = state.resolve(q)?;
    check(state, tx, logs, id, kind)
}

/// Steps 1–5 for session `id`, the link up and reconciled. A restart skips step 1: its own
/// tab is the one it replaces.
fn check(
    state: &mut State,
    tx: &mut Tx,
    logs: &mut Vec<Effect>,
    id: SessionId,
    kind: LaunchKind,
) -> Result<Resume, OpError> {
    // 1. Running, or about to.
    if state.tmux.launching(id).is_some() {
        return Ok(Resume::Waiting(id));
    }
    if kind != LaunchKind::Restart
        && let Some(pane) = bound_pane(state, id)
    {
        return Ok(Resume::Running(id, pane));
    }
    let Some(s) = state.session(id).cloned() else {
        return Err(OpError(LOST.into()));
    };
    // 2. Running somewhere moa can't reach, or never known.
    if s.location == Location::Elsewhere {
        return Err(OpError(ELSEWHERE.into()));
    }
    let socket = match &state.tmux().link {
        Link::Up { socket, .. } => socket.clone(),
        _ => return Err(OpError(NO_TERMINAL.into())),
    };
    if state
        .spike_panes()
        .any(|p| p.session_id == id && !on_this_server(p.tmux.as_deref(), &socket))
    {
        return Err(OpError(OTHER_TERMINAL.into()));
    }
    let Some(conv) = state.current_conversation(id).cloned() else {
        return Err(OpError(NO_CONVERSATION.into()));
    };
    // 3. The account the conversation was recorded in.
    let (account, acct) = account_of(state, &conv)?;
    let name = account.to_string();
    if conv.agent_kind != AgentKind::Claude {
        return Err(OpError(ONLY_CLAUDE.into()));
    }
    let launcher = launcher_of(&name, &acct)?;
    // 4. The folder it began in, there, and this brain's.
    let deps = state.deps().clone();
    let folder =
        resume_folder(&s, &conv, deps.canonical).ok_or_else(|| OpError(NO_FOLDER.into()))?;
    if !(deps.is_dir)(Path::new(&folder)) {
        return Err(OpError(format!("its folder {folder} is gone")));
    }
    let routed = (deps.canonical)(Path::new(&folder));
    match route(state.config(), Some(&account), &routed, Some(state.brain())) {
        Route::Brain(b) if b != *state.brain() => {
            return Err(OpError(format!(
                "that folder belongs to the {b} brain; resume it there"
            )));
        }
        Route::Brain(_) => {}
        Route::NotAvailable => {
            return Err(OpError(format!("{name} can't run sessions in that folder")));
        }
    }
    // 5. Its transcript, in that account's store.
    match (deps.transcript)(&acct.store, &conv.conversation_id) {
        TranscriptCheck::Present(_) => {}
        TranscriptCheck::UnsafeId => return Err(OpError(UNSAFE_ID.into())),
        TranscriptCheck::Missing | TranscriptCheck::NotChecked => {
            let store = (deps.canonical)(&acct.store);
            let elsewhere = state.config().accounts.iter().find(|(_, a)| {
                a.kind == AgentKind::Claude
                    && (deps.canonical)(&a.store) != store
                    && matches!(
                        (deps.transcript)(&a.store, &conv.conversation_id),
                        TranscriptCheck::Present(_)
                    )
            });
            let text = match elsewhere {
                Some((other, _)) => format!(
                    "its conversation is in {other}'s folder, not {name}'s; moa won't resume it under the wrong account"
                ),
                None => format!(
                    "its conversation isn't in {name}'s folder any more, so it can't be resumed"
                ),
            };
            // The next open asks "start fresh or pick a conversation". A refused write
            // leaves the refusal, which still holds.
            let failed = Session {
                resume_failed: true,
                ..s
            };
            if tx.commit(state, vec![Change::Session(failed)]).is_err() {
                logs.push(warn(format!(
                    "the resume_failed mark of session {id} was not saved"
                )));
            }
            return Err(OpError(text));
        }
    }
    let title = named(&s)
        .or_else(|| {
            Path::new(&folder)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "agent".to_string());
    Ok(Resume::Check(Checked {
        id,
        conv: conv.conversation_id,
        folder,
        store: acct.store,
        launcher,
        title,
    }))
}

/// `resume Q`, and `focus Q` of a session that isn't running (R12): checked, then one pane
/// query; [`resumed`] goes on with its answer. `focus` brings the tab to the screen once it
/// is bound; `via_focus` answers as `focus` does.
pub(crate) fn start_resume(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    q: &str,
    focus: bool,
    via_focus: bool,
) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let mut fx = Vec::new();
    let checked = resume(state, &mut tx, &mut fx, q, LaunchKind::Resume);
    fx.extend(post_commit(state, tx));
    fx.extend(go_on(
        state,
        conn,
        req,
        LaunchKind::Resume,
        focus,
        via_focus,
        checked,
    ));
    fx
}

/// What a resume does where its checks left it, short of the launch: answered, joined to the
/// launch it waits for, or a pane query asked.
fn go_on(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    kind: LaunchKind,
    focus: bool,
    via_focus: bool,
    checked: Result<Resume, OpError>,
) -> Vec<Effect> {
    let reply = |result| Effect::Reply { conn, req, result };
    match checked {
        Err(e) => vec![reply(Err(e))],
        Ok(Resume::Waiting(id)) => {
            if let Some(launching) = state.tmux.launching(id) {
                launching.waiters.push((conn, req));
                launching.focus |= focus;
            }
            Vec::new()
        }
        Ok(Resume::Running(id, pane)) => {
            let Some(s) = state.session(id) else {
                return vec![reply(Err(OpError(LOST.into())))];
            };
            let went = went_to(state, s);
            if !focus {
                let title = went.data["title"].as_str().unwrap_or_default();
                let text = format!("{title} is already running");
                return vec![reply(Ok(Done { text, ..went }))];
            }
            let mut fx = Vec::new();
            if let Link::Up { conn: link, .. } = state.tmux().link {
                fx.push(Effect::Tmux {
                    conn: link,
                    cmds: focus_cmds(pane).to_vec(),
                    token: None,
                });
            }
            fx.push(reply(Ok(went)));
            fx
        }
        Ok(Resume::Check(checked)) => {
            let ask = ResumeAsk {
                conn,
                req,
                checked,
                kind,
                focus,
                via_focus,
            };
            match state.tmux.ask(Asked::Resume(ask), list_panes()) {
                Some(query) => vec![query],
                None => vec![reply(Err(OpError(NO_TERMINAL.into())))],
            }
        }
    }
}

/// A resume's pane query came back. The tabs were listed while other inputs ran, so steps
/// 1–5 are run again first. Then step 6: a tab no binding holds whose agent runs the
/// conversation refuses it; a listing that failed goes on, since steps 1–5 still guard it.
/// Then R13a's one commit, and the window.
pub(crate) fn resumed(
    state: &mut State,
    now: &Now,
    ask: ResumeAsk,
    panes: Result<Vec<PaneInfo>, ()>,
) -> Vec<Effect> {
    let ResumeAsk {
        conn,
        req,
        checked,
        kind,
        focus,
        via_focus,
    } = ask;
    let reply = |result| Effect::Reply { conn, req, result };
    let mut tx = Tx::new(now);
    let mut fx = Vec::new();
    let again = ready(state).and_then(|_| check(state, &mut tx, &mut fx, checked.id, kind));
    fx.extend(post_commit(state, tx));
    let checked = match again {
        // Unchanged: the listing answers for it.
        Ok(Resume::Check(c)) if c == checked => c,
        // Running, opening or refused by now, or its conversation or folder moved: it goes
        // on as a new resume would (a moved one looks at the tabs again).
        other => {
            fx.extend(go_on(state, conn, req, kind, focus, via_focus, other));
            return fx;
        }
    };
    let Link::Up { conn: link, .. } = state.tmux().link else {
        fx.push(reply(Err(OpError(NO_TERMINAL.into()))));
        return fx;
    };
    let id = checked.id;
    match panes {
        Ok(panes) => {
            let inspect = state.deps().inspect;
            let running = panes.iter().find(|p| {
                !p.dead
                    && !p.term
                    && bound_session(state, p.pane).is_none()
                    && inspect(p.pid, &p.start).is_some_and(|a| {
                        a.kind == AgentKind::Claude
                            && a.conversation.as_deref() == Some(checked.conv.as_str())
                    })
            });
            if let Some(p) = running {
                let pane = p.pane;
                fx.push(reply(Err(OpError(format!(
                    "it's already running in tab {pane}; adopt it with moactl adopt {pane}"
                )))));
                return fx;
            }
        }
        Err(()) => fx.push(warn(format!(
            "listing the tabs failed; resuming session {id} without looking for its agent"
        ))),
    }

    // R13a.
    let Some(s) = state.session(id).cloned() else {
        fx.push(reply(Err(OpError(LOST.into()))));
        return fx;
    };
    let mut tx = Tx::new(now);
    let live = Session {
        lifecycle: Lifecycle::Live,
        asleep_at: None,
        location: Location::Here,
        resume_failed: false,
        activity: Activity::Idle,
        activity_why: None,
        activity_at: Some(now.wall),
        last_seen_at: Some(now.wall),
        // The folder it runs in, which the reply names.
        cwd: Some(checked.folder.clone()),
        ..s
    };
    let text = format!("resumed {}", checked.title);
    if let Err(e) = tx.ui(state, vec![Change::Session(live)], text) {
        fx.push(reply(Err(e)));
        return fx;
    }
    let (answer, waiters) = if via_focus {
        (None, vec![(conn, req)])
    } else {
        (Some((conn, req)), Vec::new())
    };
    state.tmux.wait_open(
        id,
        Launching {
            reply: answer,
            kind,
            focus,
            waiters,
            launched_at: tx.wall,
        },
    );
    let spec = LaunchSpec {
        id,
        title: &checked.title,
        folder: &checked.folder,
        store: &checked.store,
        launcher: &checked.launcher,
        conv: Conv::Resume(&checked.conv),
        prompt: None,
    };
    let window = new_window(state, link, &spec);
    fx.extend(post_commit(state, tx));
    fx.push(window);
    fx
}

fn warn(message: String) -> Effect {
    log(LogLevel::Warn, message)
}

/// The answers to launches whose tab tmux reported on, or that the link went away from:
/// each pane bound and tagged (and brought to the screen, when asked), or its session
/// ended or put back to sleep, in one commit; then the replies.
pub(crate) fn opened(state: &mut State, now: &Now, settled: Vec<Settled>) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    let mut logs = Vec::new();
    let mut after = Vec::new();
    for Settled { id, launching, end } in settled {
        let (result, went) = match end {
            OpenEnd::Bound { pane, pid } => bind(
                state, &mut tx, id, pane, pid, &launching, &mut logs, &mut after,
            ),
            OpenEnd::Failed(why) => {
                // tmux's first line and ids only: no record contents.
                logs.push(warn(match why {
                    Some(line) => format!("tmux couldn't open a tab for session {id}: {line}"),
                    None => format!("tmux's answer about the tab for session {id} didn't parse"),
                }));
                end_launch(state, &mut tx, id, launching.kind, true, &mut logs);
                (Err(OpError(NO_TAB.into())), None)
            }
            OpenEnd::Gone => {
                end_launch(state, &mut tx, id, launching.kind, false, &mut logs);
                (Err(OpError(WENT_AWAY.into())), None)
            }
        };
        // A caller that went has nobody to tell; its tab was still bound or ended above.
        if let Some((conn, req)) = launching.reply {
            after.push(Effect::Reply {
                conn,
                req,
                result: result.clone(),
            });
        }
        // A focus waiter hears where it went, or else what the launch heard.
        let went = went.map(Ok).unwrap_or(result);
        after.extend(
            launching
                .waiters
                .into_iter()
                .map(|(conn, req)| Effect::Reply {
                    conn,
                    req,
                    result: went.clone(),
                }),
        );
    }
    let mut fx = logs;
    fx.extend(post_commit(state, tx));
    fx.extend(after);
    fx
}

/// The launches whose session a reload didn't find: their commit failed, so their new
/// window was dropped with it and no tab opened. Each is answered so.
pub(crate) fn lost_opens(state: &mut State) -> Vec<Effect> {
    let lost: Vec<SessionId> = state
        .tmux
        .waiting_opens()
        .filter(|id| state.session(*id).is_none())
        .collect();
    lost.into_iter()
        .filter_map(|id| state.tmux.forget_open(id))
        .flat_map(|l| l.reply.into_iter().chain(l.waiters))
        .map(|(conn, req)| Effect::Reply {
            conn,
            req,
            result: Err(OpError(NOT_SAVED.into())),
        })
        .collect()
}

/// A bound launch's reply, and what its focus waiters hear, when that differs.
type Answers = (Result<Done, OpError>, Option<Done>);

/// Binds the session to its new pane and tags the pane with it, then brings it to the
/// screen when asked (after the commit, in `after`). The binding is stamped with the
/// launch's own time.
#[allow(clippy::too_many_arguments)]
fn bind(
    state: &mut State,
    tx: &mut Tx,
    id: SessionId,
    pane: PaneId,
    pid: i64,
    launching: &Launching,
    logs: &mut Vec<Effect>,
    after: &mut Vec<Effect>,
) -> Answers {
    // `settle` reports a pane only while the link it came on is up.
    let Link::Up {
        conn,
        generation,
        socket,
        ..
    } = &state.tmux().link
    else {
        end_launch(state, tx, id, launching.kind, false, logs);
        return (Err(OpError(WENT_AWAY.into())), None);
    };
    let (conn, generation, socket) = (*conn, *generation, socket.clone());
    // The new window is sent only once the session is saved, and a reload that loses it
    // answers the launch then ([`lost_opens`]), so this shouldn't happen. If it does, a tab
    // has opened that nothing tracks: the reply says so.
    let Some(s) = state.session(id).cloned() else {
        logs.push(warn(format!(
            "session {id} got a tab but isn't in the brain"
        )));
        return (Err(OpError(LOST.into())), None);
    };
    if state.tmux.take_exited(pane) {
        // Its agent exited before this answer came: it goes to sleep as `pane_gone` would
        // put it, and no binding is made. The tab did open, so the reply is the one an exit
        // right after this answer would have got; the session shows as asleep, or ended if
        // its clean quit came first. A refused write leaves it live with no binding at all:
        // reconcile puts such sessions to sleep.
        if !lost_agent(state, tx, id, Vec::new()) {
            logs.push(warn(format!("the sleep of session {id} was not saved")));
        }
        return (Ok(launched_reply(state, launching.kind, s)), None);
    }
    let binding = PaneBinding {
        session_id: id,
        server_pid: generation.pid,
        server_start: generation.start,
        socket,
        pane_id: pane.to_string(),
        pane_pid: pid,
        bound_at: launching.launched_at,
        tracked: true,
    };
    if tx
        .commit(state, vec![Change::PaneBinding(binding)])
        .is_err()
    {
        // The tab runs, untagged: reconcile binds it by its agent at the next connection.
        logs.push(warn(format!("the tab of session {id} was not saved")));
        return (Err(OpError(UNTRACKED.into())), None);
    }
    let mut cmds = vec![set_pane_option(pane, "@moa_session", &id.to_string())];
    if launching.focus {
        cmds.extend(focus_cmds(pane));
    }
    after.push(Effect::Tmux {
        conn,
        cmds,
        token: None,
    });
    state.tmux.tag_sent(pane, id);
    let went = went_to(state, &s);
    (Ok(launched_reply(state, launching.kind, s)), Some(went))
}

/// A launch's answer, with the id and title (else the handle) for scripts. A resume's
/// commit made `cwd` the folder it was launched in.
fn launched_reply(state: &State, kind: LaunchKind, s: Session) -> Done {
    let title = title_of(state, &s);
    let cwd = s.cwd.unwrap_or_default();
    let text = match kind {
        LaunchKind::Open => format!(
            "opened {title} with {} in {cwd}",
            s.account.unwrap_or_default()
        ),
        LaunchKind::Resume => format!("resumed {title} in {cwd}"),
        LaunchKind::Restart => format!("restarted {title} under moa"),
    };
    Done {
        text,
        data: json!({ "id": s.id, "title": title }),
    }
}

/// A session's title, unless it has none: empty or only spaces is none.
fn named(s: &Session) -> Option<String> {
    s.title.clone().filter(|t| !t.trim().is_empty())
}

/// What a session is called in a launch's answers: its title, else its handle.
pub(crate) fn title_of(state: &State, s: &Session) -> String {
    named(s).unwrap_or_else(|| state.handle_of(s.id))
}

/// `went to <title>`: the title, else the handle.
fn went_to(state: &State, s: &Session) -> Done {
    let title = title_of(state, s);
    Done {
        text: format!("went to {title}"),
        data: json!({ "id": s.id, "title": title }),
    }
}

/// A launch that got no tab. An `open`'s session ends, marked so when tmux refused it; a
/// resumed or restarted one goes back to sleep, still resumable (R13a).
fn end_launch(
    state: &mut State,
    tx: &mut Tx,
    id: SessionId,
    kind: LaunchKind,
    failed: bool,
    logs: &mut Vec<Effect>,
) {
    let Some(s) = state.session(id).filter(|s| s.lifecycle == Lifecycle::Live) else {
        return;
    };
    let s = match kind {
        LaunchKind::Open => {
            let mut s = s.clone();
            s.lifecycle = Lifecycle::Ended;
            s.resume_failed |= failed;
            s
        }
        LaunchKind::Resume | LaunchKind::Restart => asleep(s, tx.wall),
    };
    if tx.commit(state, vec![Change::Session(s)]).is_err() {
        logs.push(warn(format!("the end of session {id} was not saved")));
    }
}

/// `focus Q`: once the tabs are reconciled, a running session's tab comes to the screen; a
/// session whose tab is opening is brought there once it is bound, and answered then.
/// Nothing is saved, so the commands go out on their own, before the reply. Any other
/// session is resumed, and answered once its tab is there (R12).
pub(crate) fn focus(
    state: &mut State,
    now: &Now,
    conn: ConnId,
    req: ReqId,
    q: &str,
) -> Vec<Effect> {
    let reply = |result| Effect::Reply { conn, req, result };
    let link = match ready(state) {
        Ok(link) => link,
        Err(e) => return vec![reply(Err(e))],
    };
    let id = match state.resolve(q) {
        Ok(id) => id,
        Err(e) => return vec![reply(Err(e))],
    };
    if let Some(launching) = state.tmux.launching(id) {
        launching.waiters.push((conn, req));
        launching.focus = true;
        return Vec::new();
    }
    match (bound_pane(state, id), state.session(id)) {
        (Some(pane), Some(s)) => vec![
            Effect::Tmux {
                conn: link,
                cmds: focus_cmds(pane).to_vec(),
                token: None,
            },
            reply(Ok(went_to(state, s))),
        ],
        _ => start_resume(state, now, conn, req, &id.to_string(), true, true),
    }
}

/// `term [PATH]` (R16): a plain shell in a new tab, in folder `path`. The window runs no
/// command, so it is the user's shell with no moa environment, and nothing is bound or
/// saved. [`term_opened`] tags it once tmux says which pane it has.
pub(crate) fn term(
    state: &mut State,
    conn: ConnId,
    req: ReqId,
    path: &Path,
    title: Option<&str>,
) -> Vec<Effect> {
    let reply = |text: &str| Effect::Reply {
        conn,
        req,
        result: Err(OpError(text.into())),
    };
    if !path.is_absolute() {
        return vec![reply(TERM_NOT_FULL)];
    }
    let folder = path.display().to_string();
    let deps = state.deps();
    // The title, else `~` for home, else the folder's name.
    let named = title
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .or_else(|| (with_tilde(&folder, &deps.home, deps.canonical) == "~").then(|| "~".into()))
        .or_else(|| path.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "~".to_string());
    // Cut before `literal_format`, as a launch's name is.
    let name = cut(&format!("⌨ {named}"), WINDOW_NAME_CUT);
    let cmd = Cmd::new("new-window")
        .args(["-d", "-P", "-F", OPENED_FORMAT, "-n"])
        .arg(literal_format(&name))
        .arg("-c")
        .arg(literal_format(&folder))
        .args(["-t", "=moa:"]);
    let asked = Asked::Term {
        reply: Some((conn, req)),
        name,
        path: folder,
    };
    // Asked only while the link is up.
    match state.tmux.ask(asked, vec![cmd]) {
        Some(query) => vec![query],
        None => vec![reply(NO_TERMINAL)],
    }
}

/// tmux answered a terminal's new window: its tab is tagged as a terminal with its label
/// (literal, so a `#[…]` in it doesn't style the border), then brought to the screen.
/// Nothing is saved, so the commands go out on their own, before the reply. A caller that
/// went has nobody to tell, and the tab is still tagged.
pub(crate) fn term_opened(
    state: &State,
    reply: Option<(ConnId, ReqId)>,
    name: &str,
    path: &str,
    answer: Answer,
) -> Vec<Effect> {
    let pane = answer.ok().and_then(|lists| match lists.as_slice() {
        [lines] => match lines.as_slice() {
            [line] => opened_pane(line),
            _ => None,
        },
        _ => None,
    });
    let mut fx = Vec::new();
    let result = match (pane, &state.tmux().link) {
        (Some((pane, _)), Link::Up { conn, .. }) => {
            let mut cmds = vec![
                set_pane_option(pane, "@moa_term", "1"),
                set_pane_option(pane, "@moa_label", &literal_format(name)),
            ];
            cmds.extend(focus_cmds(pane));
            fx.push(Effect::Tmux {
                conn: *conn,
                cmds,
                token: None,
            });
            let deps = state.deps();
            Ok(Done {
                text: format!(
                    "opened a terminal in {} (tab {name})",
                    with_tilde(path, &deps.home, deps.canonical)
                ),
                data: json!({ "pane": pane.to_string(), "name": name }),
            })
        }
        _ => {
            fx.push(warn(
                "tmux didn't open a terminal's tab, or its answer didn't parse".into(),
            ));
            Err(OpError(NO_SHELL.into()))
        }
    };
    if let Some((conn, req)) = reply {
        fx.push(Effect::Reply { conn, req, result });
    }
    fx
}

/// `path` with `home` (as given, or `canonical`) written `~`.
fn with_tilde(path: &str, home: &Path, canonical: fn(&Path) -> PathBuf) -> String {
    let path = Path::new(path);
    for home in [home.to_path_buf(), canonical(home)] {
        match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => return "~".into(),
            Ok(rest) => return format!("~/{}", rest.display()),
            Err(_) => {}
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(p: &Path) -> PathBuf {
        p.to_path_buf()
    }

    #[test]
    fn launch_env_modes() {
        let home = Path::new("/h");
        let default = Path::new("/h/.claude");
        let set = (
            vec!["-e".to_string(), "CLAUDE_CONFIG_DIR=/h/.claude".to_string()],
            Vec::<String>::new(),
        );
        assert_eq!(launch_env(default, home, ConfigDirMode::Set, same), set);
        let unset = (
            Vec::<String>::new(),
            vec![
                "/usr/bin/env".to_string(),
                "-u".to_string(),
                "CLAUDE_CONFIG_DIR".to_string(),
            ],
        );
        assert_eq!(
            launch_env(default, home, ConfigDirMode::UnsetForDefault, same),
            unset
        );
        assert_eq!(
            launch_env(
                Path::new("/h/.claude-a"),
                home,
                ConfigDirMode::UnsetForDefault,
                same
            ),
            (
                vec![
                    "-e".to_string(),
                    "CLAUDE_CONFIG_DIR=/h/.claude-a".to_string()
                ],
                Vec::new()
            )
        );
        assert_eq!(CONFIG_DIR_MODE, ConfigDirMode::UnsetForDefault);
    }

    /// The store is compared as `Deps::canonical` spells it: another spelling of the
    /// default store is still the default.
    #[test]
    fn launch_env_compares_canonical_folders() {
        fn resolve(p: &Path) -> PathBuf {
            PathBuf::from(p.display().to_string().replace("/./", "/"))
        }
        let (env, prefix) = launch_env(
            Path::new("/h/./.claude"),
            Path::new("/h"),
            ConfigDirMode::UnsetForDefault,
            resolve,
        );
        assert!(env.is_empty(), "{env:?}");
        assert_eq!(prefix[0], "/usr/bin/env");
    }
}
