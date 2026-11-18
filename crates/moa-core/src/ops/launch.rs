//! Agents in new tabs of moa's terminal: `open` (and later `resume` and restart) through one
//! window builder (R11), the answers once tmux says how each tab turned out, and `focus`.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::cut;
use super::projects::project_for;
use crate::Now;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine, TmuxToken};
use crate::hooks::{asleep, lost_agent};
use crate::model::{
    Activity, AgentKind, Conversation, ConversationReason, Group, Lifecycle, Location, PaneBinding,
    Session, SessionId, normalize_group_path,
};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::route::{Route, route};
use crate::state::State;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::focus::bound_pane;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{
    LaunchKind, Launching, Link, NO_TERMINAL, OPENED_FORMAT, OpenEnd, Settled, ready,
    set_pane_option,
};

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
const NO_RESUME: &str = "resume isn't in this build yet";

/// The conversation a launch runs: a new one under this id, or an old one resumed.
pub(crate) enum Conv<'a> {
    Fresh(&'a str),
    // Comes with `moactl resume`.
    #[allow(dead_code)]
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

fn warn(message: String) -> Effect {
    Effect::Log(LogLine {
        level: LogLevel::Warn,
        message,
    })
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
        return (Ok(launched_reply(launching.kind, s)), None);
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
    let went = went_to(state, &s);
    (Ok(launched_reply(launching.kind, s)), Some(went))
}

/// A launch's answer, with the id and title for scripts.
fn launched_reply(kind: LaunchKind, s: Session) -> Done {
    let title = s.title.unwrap_or_default();
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

/// `went to <title>`: the title, else the handle.
fn went_to(state: &State, s: &Session) -> Done {
    let title = s
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| state.handle_of(s.id));
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
/// Nothing is saved, so the commands go out on their own, before the reply.
pub(crate) fn focus(state: &mut State, conn: ConnId, req: ReqId, q: &str) -> Vec<Effect> {
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
        // Resuming it comes with `moactl resume`.
        _ => vec![reply(Err(OpError(NO_RESUME.into())))],
    }
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
