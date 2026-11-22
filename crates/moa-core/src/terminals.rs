//! The terminals moa records (5d): every shell in moa's tmux that isn't an agent or a panel.
//! Each listing, once the link's tabs are reconciled, is compared with the open records
//! ([`listed`], T3): a new shell gets a record, a gone one's record closes, and a changed
//! program or folder is updated. moa closes one when you ask it to, or when the timeout you
//! set runs out ([`close`], [`run`], T8).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use jiff::{Span, SpanTotal, Unit};

use crate::change::Change;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Effect, LogLevel, TimerKey, rearm};
use crate::model::{ActivityWho, OpenedBy, SessionId, TermId, Terminal};
use crate::op::OpError;
use crate::ops::term::Tell;
use crate::sleep::kill_pane;
use crate::state::{State, Table};
use crate::time::DAY;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::focus::{bound_session, trusted_panes};
use crate::tmux::ids::{Generation, PaneId};
use crate::tmux::link::{Asked, Link, log, ready, set_pane_option};
use crate::tmux::topology::{HIDDEN, Topology, UNNAMED};
use crate::{Now, UtcMs};

/// The label of a shell moa marks a terminal without being asked to name it: one it finds,
/// and one the cockpit sends home.
pub(crate) const SHELL: &str = "⌨ shell";

/// What marks `pane` a terminal labelled `label` (`@moa_term`, then the literal
/// `@moa_label`), so the listing and the picker see it as one.
pub(crate) fn mark(pane: PaneId, label: &str) -> [Cmd; 2] {
    [
        set_pane_option(pane, "@moa_term", "1"),
        set_pane_option(pane, "@moa_label", &literal_format(label)),
    ]
}

/// Whether listed pane `pane` is a terminal: a tab (or a split) in a session other than
/// `_moa` that no session is tagged on or bound to, showing no panel and no `@moa_view`, and
/// whose program hasn't exited. The one place this rule lives.
pub fn is_terminal_pane(state: &State, pane: &PaneId) -> bool {
    let p = *pane;
    let t = &state.tmux().topology;
    t.session_name_of(p).is_some_and(|s| s != HIDDEN)
        && t.moa_session(p).is_none()
        && t.view_of(p).is_none()
        && !t.is_dead(p)
        && state.panel_in(p).is_none()
        && bound_session(state, p).is_none()
}

/// The open record of terminal `pane` on the server the link last connected to.
pub fn open_for<'a>(state: &'a State, pane: &PaneId) -> Option<&'a Terminal> {
    let pane = pane.to_string();
    open_records(state).find(|t| t.pane_id == pane)
}

/// The open records of the server the link last connected to, by pane (`%N`): one lookup
/// table for a whole listing, where [`open_for`] scans the records for each pane.
pub(crate) fn open_by_pane(state: &State) -> BTreeMap<&str, &Terminal> {
    open_records(state)
        .map(|t| (t.pane_id.as_str(), t))
        .collect()
}

/// The open terminal a `term.*` op's `target` names: its number (`7` or `#7`), its pane
/// (`%12`), or its name exactly, whatever the case. The one lookup every `term.*` op uses.
pub fn find<'a>(state: &'a State, target: &str) -> Result<&'a Terminal, OpError> {
    let target = target.trim();
    let none = || OpError(format!("no terminal {target}"));
    if target.starts_with('%') {
        return open_for(state, &target.parse().map_err(|_| none())?).ok_or_else(none);
    }
    let number = target.strip_prefix('#').unwrap_or(target);
    if let Ok(id) = number.parse::<i64>() {
        return open_records(state).find(|t| t.id.0 == id).ok_or_else(none);
    }
    let lower = target.to_lowercase();
    let mut named = open_records(state).filter(|t| t.name.to_lowercase() == lower);
    match (named.next(), named.next()) {
        (Some(t), None) => Ok(t),
        (None, _) => Err(none()),
        (Some(_), Some(_)) => Err(OpError(format!(
            "{} terminals are named {target}; use its number",
            2 + named.count()
        ))),
    }
}

/// Whether a terminal last touched at `last` (its open, when never touched) is idle: nobody
/// is looking at it (`active`) and it has gone `idle_ms` untouched at `now`. `0` never is.
pub fn is_idle(last: UtcMs, active: bool, idle_ms: i64, now: UtcMs) -> bool {
    idle_ms > 0 && !active && now.0.saturating_sub(last.0) >= idle_ms
}

/// Whether a user is looking at open record `t` now.
pub(crate) fn is_active(state: &State, t: &Terminal) -> bool {
    t.pane_id
        .parse()
        .is_ok_and(|pane| state.looked_at.contains(&pane))
}

/// The records of the server the link last connected to (none before it connected).
pub(crate) fn open_records(state: &State) -> impl Iterator<Item = &Terminal> {
    let generation = state.tmux().generation();
    state
        .terminals
        .values()
        .filter(move |t| generation.is_some_and(|g| on(t, g)))
}

/// Whether record `t` names a pane of server `generation`.
fn on(t: &Terminal, generation: Generation) -> bool {
    t.server_pid == generation.pid && t.server_start == generation.start
}

/// A new open record of `pane` on server `generation`, opened at `now` by `opened_by`. The
/// caller fills in its folder, name, shell and program.
pub(crate) fn opened(
    id: i64,
    generation: Generation,
    pane: PaneId,
    opened_by: OpenedBy,
    now: UtcMs,
) -> Terminal {
    Terminal {
        id: TermId(id),
        server_pid: generation.pid,
        server_start: generation.start,
        pane_id: pane.to_string(),
        opened_by,
        cwd: String::new(),
        session_id: None,
        opened_at: now,
        touched_at: None,
        name: String::new(),
        note: None,
        timeout_at: None,
        shell: String::new(),
        command: String::new(),
        command_since: now,
        closed_at: None,
    }
}

/// What this listing changes in the open records, at `now`; `first` when it is this
/// connection's first look, whose new shells were open before moa saw them (`found`; `you`
/// otherwise). Nothing until the tabs are reconciled (a `@moa_term` tag changes how
/// reconcile binds an agent), nor when this listing's pane list failed.
pub fn listed(state: &State, now: UtcMs, first: bool) -> Vec<Change> {
    diff(state, now, first).changes
}

/// What a listing changes ([`listed`]): the records, the shells newly recorded that carry no
/// terminal mark yet, and whether a new one was held back.
#[derive(Default)]
struct Diff {
    changes: Vec<Change>,
    unmarked: Vec<PaneId>,
    held: bool,
}

/// The server the link is up on, while its tabs are reconciled and this listing's pane
/// list came back: when the records may be compared with it.
fn comparable(state: &State) -> Option<Generation> {
    let tmux = state.tmux();
    match tmux.link {
        Link::Up { generation, .. } if tmux.reconciled() && tmux.topology.panes_listed() => {
            Some(generation)
        }
        _ => None,
    }
}

fn diff(state: &State, now: UtcMs, first: bool) -> Diff {
    let mut d = Diff::default();
    let Some(generation) = comparable(state) else {
        return d;
    };
    let tmux = state.tmux();
    let topology = &tmux.topology;
    // Each open record closes, but one whose pane is still a terminal, which takes in what
    // the listing says runs there. Another server's names other panes.
    let mut recorded: BTreeSet<PaneId> = BTreeSet::new();
    for t in state.terminals.values() {
        let pane = t.pane_id.parse().ok().filter(|_| on(t, generation));
        if let Some(pane) = pane {
            recorded.insert(pane);
            // Listed with no process: another pane's folder (one holding a newline) printed
            // a line for it too. Nothing is known of it this time.
            if topology.has_pane(pane) && topology.pid_of(&pane).is_none() {
                continue;
            }
            if is_terminal_pane(state, &pane) {
                d.changes.extend(updated(t, topology, pane, now));
                continue;
            }
        }
        // A pane that became an agent keeps its `@moa_term`: sleep reads it.
        d.changes.push(Change::Terminal(Terminal {
            closed_at: Some(now),
            timeout_at: None,
            ..t.clone()
        }));
    }
    // A launch's tab, a terminal's new one, or a panel moa started can be listed before
    // tmux says which it is and moa tags it: a new unmarked one waits for the next listing.
    // tmux answers in order, so by then it is tagged.
    let hold = tmux.waiting_opens().next().is_some()
        || tmux.asked().any(|a| {
            matches!(
                a,
                Asked::Term { .. } | Asked::Spawn { .. } | Asked::Adopt { .. }
            )
        });
    let trusted = trusted_panes(state);
    let opened_by = if first {
        OpenedBy::Found
    } else {
        OpenedBy::You
    };
    let mut next = state.next_id(Table::Terminal);
    for pane in topology.panes() {
        if recorded.contains(&pane) || !is_terminal_pane(state, &pane) {
            continue;
        }
        let (Some(pid), Some(command), Some(cwd)) = (
            topology.pid_of(&pane),
            topology.command_of(&pane),
            topology.cwd_of(&pane),
        ) else {
            continue;
        };
        let marked = topology.term_label(pane).is_some();
        if hold && !marked {
            d.held = true;
            continue;
        }
        if !marked {
            d.unmarked.push(pane);
        }
        // A split's session: the agent bound in the same tab, when there is just one.
        let window = topology.window_of(pane);
        let sessions: BTreeSet<SessionId> = trusted
            .iter()
            .filter(|(p, (w, _))| **p != pane && Some(*w) == window)
            .map(|(_, (_, id))| *id)
            .collect();
        let session_id = sessions.first().copied().filter(|_| sessions.len() == 1);
        d.changes.push(Change::Terminal(Terminal {
            cwd: cwd.to_string(),
            session_id,
            name: topology.term_name(pane).unwrap_or(UNNAMED).to_string(),
            shell: (state.deps().process_name)(pid).unwrap_or_else(|| command.to_string()),
            command: command.to_string(),
            ..opened(next, generation, pane, opened_by, now)
        }));
        next += 1;
    }
    d
}

/// Record `t`'s terminal, still one in `pane`, as the listing shows it at `now`: a changed
/// program sets since when it runs; a changed folder is the new one. A shell whose name
/// couldn't be read when it was recorded is the program first listed, as a new record's
/// is. `None` when nothing changed.
fn updated(t: &Terminal, topology: &Topology, pane: PaneId, now: UtcMs) -> Option<Change> {
    let (command, cwd) = (topology.command_of(&pane)?, topology.cwd_of(&pane)?);
    if t.command == command && t.cwd == cwd && !t.shell.is_empty() {
        return None;
    }
    let mut t = t.clone();
    if t.shell.is_empty() {
        t.shell = command.to_string();
    }
    if t.command != command {
        t.command = command.to_string();
        t.command_since = now;
    }
    t.cwd = cwd.to_string();
    Some(Change::Terminal(t))
}

/// The open terminals a user started or stopped looking at since the last tmux input, each
/// stamped `touched_at` `now`: one write when you switch to a terminal and one when you leave
/// it, never one per event. Typing on it counts, as it is on screen. The set looked at is
/// kept in `state` for the next input, and drives a row's `active`.
pub(crate) fn touched(state: &mut State, now: UtcMs) -> Vec<Change> {
    // moa losing tmux isn't a user leaving a terminal, and a connection's topology isn't
    // trusted until it is reconciled.
    if ready(state).is_err() {
        return Vec::new();
    }
    let current = state.tmux().topology.looked_at();
    let moved: Vec<PaneId> = current
        .symmetric_difference(&state.looked_at)
        .copied()
        .collect();
    state.looked_at = current;
    moved
        .into_iter()
        .filter_map(|pane| open_for(state, &pane))
        .map(|t| {
            Change::Terminal(Terminal {
                touched_at: Some(now),
                ..t.clone()
            })
        })
        .collect()
}

/// The records after a listing, or at the end of this connection's reconcile ([`listed`]),
/// in one commit. A shell newly recorded without a terminal mark is marked `⌨ shell` once
/// the commit is saved, so the picker lists it. The connection's first look is over once it
/// is saved with no new shell held back.
pub(crate) fn register(state: &mut State, now: &Now) -> Vec<Effect> {
    if comparable(state).is_none() {
        return Vec::new();
    }
    let Diff {
        changes,
        unmarked,
        held,
    } = diff(state, now.wall, state.tmux().terms_unseen);
    let mut tx = Tx::new(now);
    if !changes.is_empty() && tx.commit(state, changes).is_err() {
        return vec![log(
            LogLevel::Debug,
            "the terminals' records weren't saved; the next listing tries again".into(),
        )];
    }
    for pane in unmarked {
        for cmd in mark(pane, SHELL) {
            tx.tmux(cmd);
        }
        state.tmux.term_sent(pane, SHELL);
    }
    if !held {
        state.tmux.terms_unseen = false;
    }
    post_commit(state, tx)
}

/// How long a running program has after its Ctrl-c before its terminal is killed.
pub(crate) const STOP_WAIT_MS: i64 = 3_000;
/// The shortest and the longest timeout.
pub(crate) const TIMEOUT_MS: RangeInclusive<i64> = 1_000..=7 * DAY;
pub(crate) const TIMEOUT_FORM: &str = "a timeout is like 3h or 90m, up to 7 days";

/// A close moa started (T8), kept until a listing no longer shows its tab: of `pane` on
/// server `generation`, and when it is killed, while a running program has its 3 seconds
/// after the Ctrl-c.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Closing {
    pub generation: Generation,
    pub pane: PaneId,
    pub kill_at: Option<UtcMs>,
}

/// How long `text` asks a timeout to be (`3h`, `90m`, `2h30m`, `45s`, `7d`), in ms, from
/// 1 second to 7 days; `off` is none. The CLI and the view both read one this way.
pub fn parse_after(text: &str) -> Result<Option<i64>, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let ms = text
        .parse::<Span>()
        .ok()
        .and_then(|span| {
            span.total(SpanTotal::from(Unit::Millisecond).days_are_24_hours())
                .ok()
        })
        .filter(|ms| ms.fract() == 0.0)
        .map(|ms| ms as i64);
    match ms {
        Some(ms) if TIMEOUT_MS.contains(&ms) => Ok(Some(ms)),
        _ => Err(TIMEOUT_FORM.to_string()),
    }
}

/// Starts closing open record `t` (T8): one fresh question of what runs in its tab, which
/// decides how it closes once tmux answers ([`crate::ops::term::close_answered`]). The
/// close is held in `state.closing` meanwhile, so a second one is said to be under way.
/// `tell` is whom to tell; none for moa's own close. `None` while the link isn't up on
/// the record's server, or its record names no tab.
pub(crate) fn close(state: &mut State, tell: Option<Tell>, t: &Terminal) -> Option<Effect> {
    let generation = state.tmux().generation()?;
    let pane: PaneId = t.pane_id.parse().ok()?;
    if !on(t, generation) {
        return None;
    }
    let cmd = Cmd::new("display-message").args([
        "-p".to_string(),
        "-t".to_string(),
        pane.to_string(),
        "#{pane_current_command}".to_string(),
    ]);
    let query = state
        .tmux
        .ask(Asked::TermClose { tell, id: t.id }, vec![cmd])?;
    state.closing.insert(
        t.id,
        Closing {
            generation,
            pane,
            kill_at: None,
        },
    );
    Some(query)
}

/// Whether moa may send `pane` of server `generation` a close's Ctrl-c or kill now: the
/// link is up on that server with its tabs reconciled, and `pane` is still a terminal.
/// Checked as each one is sent, so another server's tab or an agent's never gets one.
pub(crate) fn may_close(state: &State, generation: Generation, pane: PaneId) -> bool {
    ready(state).is_ok()
        && state.tmux().generation() == Some(generation)
        && is_terminal_pane(state, &pane)
}

/// The closes whose tab this listing no longer shows are over.
pub(crate) fn closes_listed(state: &mut State) {
    let topology = &state.tmux.topology;
    if topology.panes_listed() {
        state.closing.retain(|_, c| topology.has_pane(c.pane));
    }
}

/// The earliest moment after `now` a terminal is due: a close's kill, or an open record's
/// timeout. Times already past are left out, as `sleep::deadline` does: due work that
/// can't run yet is tried again at the end of the next input.
pub(crate) fn deadline(state: &State, now: UtcMs) -> Option<UtcMs> {
    let kills = state.closing.values().filter_map(|c| c.kill_at);
    let timeouts = open_records(state).filter_map(|t| t.timeout_at);
    kills.chain(timeouts).filter(|at| *at > now).min()
}

/// Kills the terminals whose program had its 3 seconds, and closes those whose timeout ran
/// out; then arms the timer at the next deadline, or cancels it, as sleep's is. `handle`
/// runs it at the end of every input, beside `sleep::run`. Nothing is sent until the link
/// is up with its tabs reconciled.
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    if ready(state).is_ok() {
        kill_due(state, now.wall, fx);
        if !state.writes_refused(now.wall) {
            time_out(state, now, fx);
        }
    }
    let next = deadline(state, now.wall);
    fx.extend(rearm(
        &mut state.terminals_at,
        TimerKey::Terminals,
        next,
        fired,
    ));
}

/// The closes whose kill is due get it, each while its tab is still a terminal of the
/// server it was asked on; one that isn't any more is dropped, with nothing sent. A killed
/// one stays until the listing no longer shows its tab.
fn kill_due(state: &mut State, now: UtcMs, fx: &mut Vec<Effect>) {
    let Some(conn) = state.tmux().conn() else {
        return;
    };
    let due: Vec<(TermId, Closing)> = state
        .closing
        .iter()
        .filter(|(_, c)| c.kill_at.is_some_and(|at| at <= now))
        .map(|(id, c)| (*id, *c))
        .collect();
    let mut cmds = Vec::new();
    for (id, c) in due {
        if may_close(state, c.generation, c.pane) {
            cmds.push(kill_pane(c.pane));
            state.closing.insert(id, Closing { kill_at: None, ..c });
        } else {
            state.closing.remove(&id);
        }
    }
    if !cmds.is_empty() {
        fx.push(Effect::Tmux {
            conn,
            cmds,
            token: None,
        });
    }
}

/// The open terminals whose timeout ran out: each one's timeout is cleared together with
/// its line from moa, in one commit, then it closes as `term.close` closes it. One already
/// closing, one whose tab isn't a terminal any more (the listing closes its record), and
/// one whose row this build can't write wait.
fn time_out(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let due: Vec<(Terminal, Change)> = open_records(state)
        .filter(|t| t.timeout_at.is_some_and(|at| at <= now.wall))
        .filter(|t| !state.closing.contains_key(&t.id))
        .filter(|t| {
            t.pane_id
                .parse()
                .is_ok_and(|pane| is_terminal_pane(state, &pane))
        })
        .map(|t| {
            let cleared = Terminal {
                timeout_at: None,
                ..t.clone()
            };
            (t.clone(), Change::Terminal(cleared))
        })
        .filter(|(_, c)| {
            !c.row()
                .is_some_and(|(table, key)| state.is_unknown(table, &key))
        })
        .collect();
    if due.is_empty() {
        return;
    }
    let mut tx = Tx::new(now);
    let changes = due.iter().map(|(_, c)| c.clone()).collect();
    let texts = due
        .iter()
        .map(|(t, _)| format!("moa closed {}: its timeout ran out", t.name))
        .collect();
    if tx.ui_by(state, changes, texts, ActivityWho::Brain).is_err() {
        fx.push(log(
            LogLevel::Warn,
            "a terminal's timeout ran out but wasn't saved; moa tries again".into(),
        ));
        return;
    }
    let mut more = post_commit(state, tx);
    for (t, _) in &due {
        more.extend(close(state, None, t));
    }
    fold_in(fx, more);
}
