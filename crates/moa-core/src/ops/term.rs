//! The terminal tabs: `term` and the `Ctrl-a $` key open a plain shell in a new tab.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::launch::{WENT_AWAY, WINDOW_NAME_CUT, focus_cmds, warn};
use super::{adopt, cut};
use crate::change::Change;
use crate::cockpit::Scene;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::folders::tilde;
use crate::model::{OpenedBy, TermId, Terminal};
use crate::op::{ConnId, Done, OpError, ReqId};
use crate::sleep::kill_pane;
use crate::state::{State, Table};
use crate::terminals::{self, Closing, STOP_WAIT_MS, TIMEOUT_FORM, TIMEOUT_MS};
use crate::time::{span, until};
use crate::tmux::cmd::{Cmd, literal_format, one_line};
use crate::tmux::focus::bound_session;
use crate::tmux::ids::{ClientName, PaneId};
use crate::tmux::link::{
    Answer, Asked, NO_TERMINAL, OPENED_FORMAT, log, notice, opened_pane, pane_query, ready,
    set_pane_option,
};
use crate::{Now, UtcMs};

const TERM_NOT_FULL: &str = "term needs a full path";
const NO_SHELL: &str = "couldn't open a terminal; see moa's log";
const TERM_GONE: &str = "that terminal is gone";
const NAME_LENGTH: &str = "a terminal's name is 1 to 40 characters";
const NAME_FORM: &str = "a terminal's name can't be a number or start with %";
const NOTE_CUT: usize = 200;
/// What `term ls` says first while its answer is only what moa last recorded.
const OUT_OF_DATE: &str = "moa isn't connected to tmux right now; this list may be out of date";

/// Whom a terminal's tab is told about: the `term` op's caller, by its reply, or the client
/// whose `Ctrl-a $` asked for it, by a notice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Tell {
    Reply(ConnId, ReqId),
    Notice(ClientName),
}

/// What tells `tell` how a terminal turned out: the reply, or a notice on the key's client
/// while the link is up (a key came over it; with the link gone, nobody can be shown one).
pub(crate) fn told(
    state: &State,
    tell: Option<Tell>,
    result: Result<Done, OpError>,
) -> Option<Effect> {
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
/// `ui.here`, else in the folder of the agent in the spot (L11), else at home; an agent's
/// in its session's folder, and any other pane (an agent's whose session has no folder too)
/// where tmux says it is, asked on the link ([`here_answered`]).
/// A key from the menu has no pane: home.
pub(crate) fn term_key(state: &mut State, pane: Option<PaneId>, client: ClientName) -> Vec<Effect> {
    let Some(pane) = pane else {
        return term_here(state, client, None);
    };
    if let Some(panel) = state.panel_in(pane) {
        let here = panel.here.clone().or_else(|| spot_folder(state));
        return term_here(state, client, here.as_deref());
    }
    // An agent's tab whose session has no folder is asked about like any other tab.
    if let Some(cwd) = bound_session(state, pane).and_then(|id| state.session(id)?.cwd.clone()) {
        return term_here(state, client, Some(&cwd));
    }
    let cmd = pane_query(pane, "#{pane_current_path}");
    // A key comes over the link, so it is up.
    state
        .tmux
        .ask(Asked::Here { client }, vec![cmd])
        .into_iter()
        .collect()
}

/// The folder of the session whose agent is in the cockpit's spot: none with no cockpit,
/// or the empty slot there. Only a pane tagged `agent` in the cockpit counts, never a stale
/// tag elsewhere. A swap's `Ok` records both the tag and where it put the panes (L17), so
/// this is right before the next listing.
fn spot_folder(state: &State) -> Option<String> {
    let scene = Scene::read(&state.tmux.topology, |p| bound_session(state, p));
    let id = scene.spot()?.agent()?;
    state.session(id)?.cwd.clone()
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
    term_here(state, client, only_line(answer).as_deref())
}

/// The one line of an answer to one command, if that is what tmux answered.
fn only_line(answer: Answer) -> Option<String> {
    let [lines] = <[_; 1]>::try_from(answer.ok()?).ok()?;
    let [line] = <[_; 1]>::try_from(lines).ok()?;
    Some(line)
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
/// command, so it is the user's shell with no moa environment, and nothing is bound.
/// [`term_opened`] records and tags it once tmux says which pane it has, and tells `tell`.
pub(crate) fn term(state: &mut State, tell: Tell, path: &Path, title: Option<&str>) -> Vec<Effect> {
    if !path.is_absolute() {
        let refused = Err(OpError(TERM_NOT_FULL.into()));
        return told(state, Some(tell), refused).into_iter().collect();
    }
    let folder = path.display().to_string();
    let deps = state.deps();
    // The title, else `~` for home, else the folder's name, else the folder.
    let name = title
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            (with_tilde(&folder, &deps.home, deps.canonical).trim_end_matches('/') == "~")
                .then(|| "~".into())
        })
        .or_else(|| path.file_name().map(|f| f.to_string_lossy().into_owned()))
        // A folder with no name, like `/`, is named as it is written.
        .unwrap_or_else(|| folder.clone());
    // On one line: a newline in the label would print as a line of the terminals' list.
    let name = one_line(&name);
    let cmd = Cmd::new("new-window")
        .args(["-d", "-P", "-F", OPENED_FORMAT, "-n"])
        .arg(literal_format(&tab_label(&name)))
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

/// A terminal named `name`'s label, on its tab and its `@moa_label`: `⌨ <name>`, cut before
/// `literal_format`, as a launch's name is.
fn tab_label(name: &str) -> String {
    cut(&format!("⌨ {name}"), WINDOW_NAME_CUT)
}

/// tmux answered a terminal's new window, named `name`: it is recorded as yours, with that
/// name whole. Its tab is tagged as a terminal with its label (literal, so a `#[…]` in it
/// doesn't style the border), then brought to the screen; the topology has the terminal at
/// once, so the `terms` topic does. The commands go out after the record's commit, and
/// still do when it wasn't saved (the next listing records the terminal then), before the
/// reply or notice. A caller that went has nobody to tell, and the tab is still tagged.
/// `replied` is whether tmux answered: otherwise the link went before it did, and the
/// answer says so as a launch's does.
pub(crate) fn term_opened(
    state: &mut State,
    now: &Now,
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
    let pane = only_line(answer).as_deref().and_then(opened_pane);
    let mut fx = Vec::new();
    let result = match (pane, state.tmux().conn()) {
        (Some((pane, pid)), Some(conn)) => {
            // The topology has the terminal before the record's commit publishes the topic.
            let label = tab_label(name);
            state.tmux.term_sent(pane, &label);
            fx.extend(record_opened(state, now, pane, pid, name, path));
            let mut cmds = terminals::mark(pane, &label).to_vec();
            cmds.extend(focus_cmds(pane));
            fx.push(Effect::Tmux {
                conn,
                cmds,
                token: None,
            });
            let deps = state.deps();
            Ok(Done {
                text: format!(
                    "opened a terminal in {} (tab {label})",
                    with_tilde(path, &deps.home, deps.canonical)
                ),
                data: json!({ "pane": pane.to_string(), "name": label }),
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

/// The record of terminal `pane` (process `pid`) that moa just opened for you in folder
/// `path`, named `name`: its shell is the process's name (when that can't be read, the next
/// listing's program), and nothing else runs yet. A commit that wasn't saved saves nothing;
/// the next listing records the terminal.
fn record_opened(
    state: &mut State,
    now: &Now,
    pane: PaneId,
    pid: i64,
    name: &str,
    path: &str,
) -> Vec<Effect> {
    let Some(generation) = state.tmux().generation() else {
        return Vec::new();
    };
    // The store holds one open record per pane: one already there is kept.
    if terminals::open_for(state, &pane).is_some() {
        return Vec::new();
    }
    let shell = u32::try_from(pid)
        .ok()
        .and_then(state.deps().process_name)
        .unwrap_or_default();
    let mut tx = Tx::new(now);
    let id = tx.next_id(state, Table::Terminal);
    let t = Terminal {
        cwd: path.to_string(),
        name: name.to_string(),
        shell: shell.clone(),
        command: shell,
        ..terminals::opened(id, generation, pane, OpenedBy::You, now.wall)
    };
    if tx.commit(state, vec![Change::Terminal(t)]).is_err() {
        return vec![log(
            LogLevel::Debug,
            "a terminal's record wasn't saved; the next listing records it".into(),
        )];
    }
    post_commit(state, tx)
}

/// `term.show` (the picker's pick of a terminal): brings terminal `pane` to the screen, as
/// `term` did when it opened. Nothing is saved, so the select goes out on its own, before
/// the reply. A pane that isn't a terminal in the topology is gone, as is one running a
/// session's agent: it is that session, shown by `session.focus`.
pub(crate) fn show(state: &State, conn: ConnId, req: ReqId, pane: &str) -> Vec<Effect> {
    let reply = |result| Effect::Reply { conn, req, result };
    let Some(link) = state.tmux().conn() else {
        return vec![reply(Err(OpError(NO_TERMINAL.into())))];
    };
    let topology = &state.tmux().topology;
    let shown = pane
        .parse::<PaneId>()
        .ok()
        .and_then(|p| Some((p, topology.term_name(p)?)));
    let Some((pane, name)) = shown else {
        return vec![reply(Err(OpError(TERM_GONE.into())))];
    };
    vec![
        Effect::Tmux {
            conn: link,
            cmds: focus_cmds(pane).to_vec(),
            token: None,
        },
        reply(Ok(Done {
            text: format!("went to {name}"),
            data: json!({ "pane": pane.to_string(), "name": name }),
        })),
    ]
}

/// `term.ls`: the open terminals, once one fresh listing of the tabs has landed, so "what's
/// running" is current when you ask: that is one listing per request, however many
/// terminals there are. [`ls_answers`] answers it. With the link down nothing can be
/// asked, and the records answer at once, saying they may be out of date.
pub(crate) fn start_ls(state: &mut State, now: &Now, conn: ConnId, req: ReqId) -> Vec<Effect> {
    if state.tmux().conn().is_none() {
        return vec![Effect::Reply {
            conn,
            req,
            result: Ok(ls(state, now)),
        }];
    }
    state.term_ls.push((conn, req));
    state.tmux.refresh()
}

/// The `term.ls` requests that wait are answered once a listing that began after them
/// has landed (`listed`, with none more asked behind it), or at once when the link is
/// down: nothing will land then.
pub(crate) fn ls_answers(state: &mut State, now: &Now, listed: bool) -> Vec<Effect> {
    let down = state.tmux().conn().is_none();
    if state.term_ls.is_empty() || !(down || (listed && !state.tmux.refreshing())) {
        return Vec::new();
    }
    let done = ls(state, now);
    std::mem::take(&mut state.term_ls)
        .into_iter()
        .map(|(conn, req)| Effect::Reply {
            conn,
            req,
            result: Ok(done.clone()),
        })
        .collect()
}

/// The open terminals as `term.ls` tells them: a line each, and each one's row (the `terms`
/// topic's) with its pane and whether it is idle. As recorded: the listing, when there was
/// one, has been taken in.
fn ls(state: &State, now: &Now) -> Done {
    let deps = state.deps();
    let idle_ms = state.config().ui.terminal_idle_ms;
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for t in terminals::open_records(state) {
        let row = state.term_row(t);
        let idle = terminals::is_idle(
            t.touched_at.unwrap_or(t.opened_at),
            row.active,
            idle_ms,
            now.wall,
        );
        let program = if t.command == t.shell {
            "shell".to_string()
        } else {
            format!("{} ({})", t.command, span(now.wall.0 - t.command_since.0))
        };
        let touched = match t.touched_at {
            _ if row.active => "now".to_string(),
            Some(at) => span(now.wall.0 - at.0),
            None => "never".to_string(),
        };
        let mut line = format!(
            "#{}  {}  {}  {program}  touched {touched}  open {}",
            t.id.0,
            t.name,
            with_tilde(&t.cwd, &deps.home, deps.canonical),
            span(now.wall.0 - t.opened_at.0),
        );
        if idle {
            line.push_str("  idle");
        }
        if let Some(at) = t.timeout_at {
            line.push_str(&format!("  closes in {}", until(at, now.wall)));
        }
        if let Some(note) = &t.note {
            line.push_str(&format!("  — {note}"));
        }
        lines.push(line);
        let mut row = json!(row);
        row["pane"] = json!(t.pane_id);
        row["idle"] = json!(idle);
        data.push(row);
    }
    if lines.is_empty() {
        lines.push("no terminals open".to_string());
    }
    if state.tmux().conn().is_none() {
        lines.insert(0, OUT_OF_DATE.to_string());
    }
    Done {
        text: lines.join("\n"),
        data: json!(data),
    }
}

/// `term.name`: terminal `target`'s name, on its record, its `@moa_label` and, when it is
/// alone in its tab, the tab, as a new terminal's is named. Naming a split leaves the tab to
/// the agent it belongs to.
pub(crate) fn name(
    state: &mut State,
    tx: &mut Tx,
    target: &str,
    name: &str,
) -> Result<Done, OpError> {
    let name = one_line(name);
    let name = name.trim();
    if !(1..=40).contains(&name.chars().count()) {
        return Err(OpError(NAME_LENGTH.into()));
    }
    // `find` reads such a target as a number or a tab: the name could never be used.
    let number = name.strip_prefix('#').unwrap_or(name);
    if name.starts_with('%') || number.parse::<i64>().is_ok() {
        return Err(OpError(NAME_FORM.into()));
    }
    ready(state)?;
    let t = terminals::find(state, target)?.clone();
    let pane: PaneId = t.pane_id.parse().map_err(|_| OpError(TERM_GONE.into()))?;
    let id = t.id.0;
    tx.commit(
        state,
        vec![Change::Terminal(Terminal {
            name: name.to_string(),
            ..t
        })],
    )?;
    let label = tab_label(name);
    tx.tmux(set_pane_option(pane, "@moa_label", &literal_format(&label)));
    let topology = &state.tmux().topology;
    let alone = topology
        .window_of(pane)
        .is_some_and(|w| topology.panes_in(w).count() == 1);
    if alone {
        tx.tmux(Cmd::new("rename-window").args([
            "-t",
            &pane.to_string(),
            "--",
            &literal_format(&label),
        ]));
    }
    if state.tmux().conn().is_some() {
        state.tmux.term_sent(pane, &label);
    }
    Ok(Done {
        text: format!("named it {name}"),
        data: json!({ "id": id, "name": name }),
    })
}

/// `term.note`: terminal `target`'s note, cut at 200 characters with `…`; an empty one
/// clears it.
pub(crate) fn note(
    state: &mut State,
    tx: &mut Tx,
    target: &str,
    note: &str,
) -> Result<Done, OpError> {
    let note = one_line(note);
    let note = note.trim();
    let t = terminals::find(state, target)?.clone();
    let id = t.id.0;
    let note = match note {
        "" => None,
        n if n.chars().count() > NOTE_CUT => Some(format!("{}…", cut(n, NOTE_CUT - 1))),
        n => Some(n.to_string()),
    };
    let text = if note.is_some() {
        "noted"
    } else {
        "note cleared"
    };
    tx.commit(
        state,
        vec![Change::Terminal(Terminal {
            note: note.clone(),
            ..t
        })],
    )?;
    Ok(Done {
        text: text.into(),
        data: json!({ "id": id, "note": note }),
    })
}

/// `term.close`: closes terminal `target` gently (T8), once tmux says what runs in its tab
/// ([`close_answered`]). A close already under way is said so, and nothing is sent.
pub(crate) fn close(state: &mut State, conn: ConnId, req: ReqId, target: &str) -> Vec<Effect> {
    let reply = |result| Effect::Reply { conn, req, result };
    let t = match ready(state).and_then(|_| terminals::find(state, target)) {
        Ok(t) => t.clone(),
        Err(why) => return vec![reply(Err(why))],
    };
    if state.closing.contains_key(&t.id) {
        return vec![reply(Ok(Done {
            text: format!("already closing {}", t.name),
            data: json!({ "id": t.id.0 }),
        }))];
    }
    match terminals::close(state, Some(Tell::Reply(conn, req)), &t) {
        Some(query) => vec![query],
        None => vec![reply(Err(OpError(TERM_GONE.into())))],
    }
}

/// tmux said what runs in record `id`'s tab, for a close: that decides it. A tab that went,
/// or isn't a terminal any more, is gone. A shell at its prompt (the record's shell) is
/// killed at once; a running program gets Ctrl-c, and the kill 3 seconds later
/// ([`terminals::run`]). A close you asked for is saved as yours in the activity before
/// anything is sent; moa's own (`tell` none) said so when its timeout ran out. `replied` is
/// whether tmux answered: otherwise the link went first.
pub(crate) fn close_answered(
    state: &mut State,
    now: &Now,
    tell: Option<Tell>,
    id: TermId,
    answer: Answer,
    replied: bool,
) -> Vec<Effect> {
    // moa's own close has nobody to tell, and its activity already said it closed: the log
    // says it didn't.
    let end = |state: &mut State, why: &str| {
        state.closing.remove(&id);
        match tell.clone() {
            None => vec![log(
                LogLevel::Warn,
                format!(
                    "terminal #{} wasn't closed when its timeout ran out: {why}",
                    id.0
                ),
            )],
            tell => told(state, tell, Err(OpError(why.into())))
                .into_iter()
                .collect(),
        }
    };
    if !replied {
        return end(state, adopt::WENT_AWAY);
    }
    if let Err(OpError(why)) = ready(state) {
        return end(state, &why);
    }
    let found = state
        .closing
        .get(&id)
        .copied()
        .zip(state.terminals.get(&id));
    let (Some((c, t)), Some(command), Some(conn)) = (found, only_line(answer), state.tmux().conn())
    else {
        return end(state, TERM_GONE);
    };
    let t = t.clone();
    if !terminals::may_close(state, c.generation, c.pane) {
        return end(state, TERM_GONE);
    }
    let mut fx = Vec::new();
    if tell.is_some() {
        let mut tx = Tx::new(now);
        if let Err(OpError(why)) = tx.ui(state, Vec::new(), format!("closed terminal {}", t.name)) {
            return end(state, &why);
        }
        fx = post_commit(state, tx);
    }
    let pane = c.pane;
    let (cmd, kill_at, text) = if command == t.shell {
        (kill_pane(pane), None, format!("closed {}", t.name))
    } else {
        (
            Cmd::interrupt(pane),
            Some(UtcMs(now.wall.0 + STOP_WAIT_MS)),
            format!("stopping {command} in {}, then closing it", t.name),
        )
    };
    state.closing.insert(id, Closing { kill_at, ..c });
    fx.push(Effect::Tmux {
        conn,
        cmds: vec![cmd],
        token: None,
    });
    let done = Done {
        text,
        data: json!({ "id": id.0, "pane": pane.to_string() }),
    };
    fx.extend(told(state, tell, Ok(done)));
    fx
}

/// `term.timeout`: terminal `target` closes `after_ms` from now, and moa says so when it
/// does ([`terminals::run`]); none clears its timeout.
pub(crate) fn timeout(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    target: &str,
    after_ms: Option<i64>,
) -> Result<Done, OpError> {
    if after_ms.is_some_and(|ms| !TIMEOUT_MS.contains(&ms)) {
        return Err(OpError(TIMEOUT_FORM.into()));
    }
    let t = terminals::find(state, target)?.clone();
    let id = t.id.0;
    let timeout_at = after_ms.map(|ms| UtcMs(now.wall.0 + ms));
    tx.commit(state, vec![Change::Terminal(Terminal { timeout_at, ..t })])?;
    let text = match timeout_at {
        Some(at) => format!("closes in {}", until(at, now.wall)),
        None => "no timeout".into(),
    };
    Ok(Done {
        text,
        data: json!({ "id": id, "timeout_at": timeout_at.map(|at| at.0) }),
    })
}

/// `path` with `home` (as given, or `canonical`) written `~`.
pub(crate) fn with_tilde(path: &str, home: &Path, canonical: fn(&Path) -> PathBuf) -> String {
    let first = tilde(path, home);
    if first != path {
        return first;
    }
    tilde(path, &canonical(home))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS homes are `/var/...` and show up as `/private/var/...`: the canonical form is
    /// tried when the given one doesn't match.
    #[test]
    fn term_tries_the_canonical_home() {
        let canonical = |_: &Path| PathBuf::from("/private/h");
        let h = Path::new("/h");
        assert_eq!(with_tilde("/private/h/x", h, canonical), "~/x");
        assert_eq!(with_tilde("/h/x", h, canonical), "~/x");
        assert_eq!(with_tilde("/other", h, canonical), "/other");
        // A home with a trailing `/` is still home.
        assert_eq!(with_tilde("/h/", Path::new("/h/"), canonical), "~/");
    }
}
