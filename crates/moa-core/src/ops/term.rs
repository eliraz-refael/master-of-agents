//! The terminal tabs: `term` and the `Ctrl-a $` key open a plain shell in a new tab.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::cut;
use super::launch::{WENT_AWAY, WINDOW_NAME_CUT, focus_cmds, warn};
use crate::effect::{Effect, LogLevel};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::state::State;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::focus::bound_session;
use crate::tmux::ids::{ClientName, PaneId};
use crate::tmux::link::{
    Answer, Asked, Link, NO_TERMINAL, OPENED_FORMAT, log, notice, opened_pane, set_pane_option,
};

const TERM_NOT_FULL: &str = "term needs a full path";
const NO_SHELL: &str = "couldn't open a terminal; see moa's log";

/// Whom a terminal's tab is told about: the `term` op's caller, by its reply, or the client
/// whose `Ctrl-a $` asked for it, by a notice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Tell {
    Reply(ConnId, ReqId),
    Notice(ClientName),
}

/// What tells `tell` how a terminal turned out: the reply, or a notice on the key's client
/// while the link is up (a key came over it; with the link gone, nobody can be shown one).
fn told(state: &State, tell: Option<Tell>, result: Result<Done, OpError>) -> Option<Effect> {
    match tell? {
        Tell::Reply(conn, req) => Some(Effect::Reply { conn, req, result }),
        Tell::Notice(client) => {
            // The same test `ask` makes, so the key's query and its notice agree.
            let conn = state.tmux().conn()?;
            let text = match result {
                Ok(done) => done.text,
                Err(OpError(text)) => text,
            };
            Some(Effect::Tmux {
                conn,
                cmds: vec![notice(&client, &text)],
                token: None,
            })
        }
    }
}

/// `Ctrl-a $` (V6): a terminal "here", by the key's pane. A panel's pane opens in its
/// `ui.here` (home until 5b gives it the agent spot's folder), an agent's in its session's
/// folder, and any other pane (an agent's whose session has no folder too) where tmux says
/// it is, asked on the link ([`here_answered`]).
/// A key from the menu has no pane: home.
pub(crate) fn term_key(state: &mut State, pane: Option<PaneId>, client: ClientName) -> Vec<Effect> {
    let Some(pane) = pane else {
        return term_here(state, client, None);
    };
    if let Some(panel) = state.panel_in(pane) {
        let here = panel.here.clone();
        return term_here(state, client, here.as_deref());
    }
    // An agent's tab whose session has no folder is asked about like any other tab.
    if let Some(cwd) = bound_session(state, pane).and_then(|id| state.session(id)?.cwd.clone()) {
        return term_here(state, client, Some(&cwd));
    }
    let cmd = Cmd::new("display-message").args([
        "-p".to_string(),
        "-t".to_string(),
        pane.to_string(),
        "#{pane_current_path}".to_string(),
    ]);
    // A key comes over the link, so it is up.
    state
        .tmux
        .ask(Asked::Here { client }, vec![cmd])
        .into_iter()
        .collect()
}

/// tmux said where a key's pane is: the terminal opens there. A pane that went is an error
/// answer, and opens at home. `replied` is whether tmux answered: otherwise the link went
/// first, and nothing opens.
pub(crate) fn here_answered(
    state: &mut State,
    client: ClientName,
    answer: Answer,
    replied: bool,
) -> Vec<Effect> {
    if !replied {
        return vec![log(
            LogLevel::Info,
            "moa's terminal went away before it said where to open a terminal".into(),
        )];
    }
    let folder = answer
        .ok()
        .and_then(|lists| match <[_; 1]>::try_from(lists) {
            Ok([lines]) => <[_; 1]>::try_from(lines).ok().map(|[line]| line),
            Err(_) => None,
        });
    term_here(state, client, folder.as_deref())
}

/// A key's terminal in `folder`: home when there is none, or it isn't a full path to a
/// folder that exists.
fn term_here(state: &mut State, client: ClientName, folder: Option<&str>) -> Vec<Effect> {
    let deps = state.deps();
    let path = folder
        .map(Path::new)
        .filter(|p| p.is_absolute() && (deps.is_dir)(p))
        .map_or_else(|| deps.home.clone(), Path::to_path_buf);
    term(state, Tell::Notice(client), &path, None)
}

/// `term [PATH]` (R16): a plain shell in a new tab, in folder `path`. The window runs no
/// command, so it is the user's shell with no moa environment, and nothing is bound or
/// saved. [`term_opened`] tags it once tmux says which pane it has, and tells `tell`.
pub(crate) fn term(state: &mut State, tell: Tell, path: &Path, title: Option<&str>) -> Vec<Effect> {
    if !path.is_absolute() {
        let refused = Err(OpError(TERM_NOT_FULL.into()));
        return told(state, Some(tell), refused).into_iter().collect();
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
        tell: Some(tell.clone()),
        name,
        path: folder,
    };
    // Asked only while the link is up.
    match state.tmux.ask(asked, vec![cmd]) {
        Some(query) => vec![query],
        None => {
            let refused = Err(OpError(NO_TERMINAL.into()));
            told(state, Some(tell), refused).into_iter().collect()
        }
    }
}

/// tmux answered a terminal's new window: its tab is tagged as a terminal with its label
/// (literal, so a `#[…]` in it doesn't style the border), then brought to the screen.
/// Nothing is saved, so the commands go out on their own, before the reply or notice. A
/// caller that went has nobody to tell, and the tab is still tagged. `replied` is whether
/// tmux answered: otherwise the link went before it did, and the answer says so as a
/// launch's does.
pub(crate) fn term_opened(
    state: &State,
    tell: Option<Tell>,
    name: &str,
    path: &str,
    answer: Answer,
    replied: bool,
) -> Vec<Effect> {
    if !replied {
        let mut fx = vec![log(
            LogLevel::Info,
            "moa's terminal went away before a terminal's tab opened".into(),
        )];
        fx.extend(told(state, tell, Err(OpError(WENT_AWAY.into()))));
        return fx;
    }
    let refused = answer.is_err();
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
            fx.push(warn(if refused {
                "tmux couldn't open a terminal's tab".into()
            } else {
                "tmux's answer about a terminal's tab didn't parse".into()
            }));
            Err(OpError(NO_SHELL.into()))
        }
    };
    fx.extend(told(state, tell, result));
    fx
}

/// `path` with `home` (as given, or `canonical`) written `~`.
pub(crate) fn with_tilde(path: &str, home: &Path, canonical: fn(&Path) -> PathBuf) -> String {
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
