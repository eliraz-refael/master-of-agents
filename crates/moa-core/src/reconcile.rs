//! Reconcile: once per connection to moa's terminal server, after the start drain (R2), the
//! pane bindings are checked against one listing of the server's panes (R3). A binding that
//! no longer holds its pane goes, and so does its session's liveness: what a `kill-server`,
//! a reboot or a crash left comes back asleep, and nothing starts on its own (spec §6.13,
//! U7). A tab that runs a conversation moa knows is bound (R6), and the panes imported from
//! the spike are verified or cleared (P18).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::hooks::asleep;
use crate::model::{
    AgentKind, Conversation, Inspected, Lifecycle, Location, PaneAgent, PaneBinding, Session,
    SessionId,
};
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::ids::{Generation, PaneId};
use crate::tmux::link::{Answer, Asked, Link, log, set_pane_option, unset_pane_option};
use crate::tmux::panes::{PaneInfo, Tag, holds, list_panes, parse_panes};
use crate::{Now, UtcMs};

/// How many reconciles of one connection may start and never finish (a panic) before moa
/// stops asking until the next connection.
pub(crate) const UNFINISHED_KEPT: u32 = 3;

/// Asks for this connection's listing when a reconcile is owed: the start drain has run,
/// the link is up, its panes aren't reconciled, no listing is on its way, and writes are
/// accepted. Run as each input starts, so a refused or failed reconcile is asked again by
/// the first input after it (R10), never by the input that failed it.
///
/// A reconcile that panicked [`UNFINISHED_KEPT`] times on this connection isn't asked
/// again: each panic reloads the core, and the reload would ask at once. One warn says so;
/// the ops on tabs keep refusing until the next connection.
pub(crate) fn ask_if_due(state: &mut State, now: &Now) -> Option<Effect> {
    let tmux = state.tmux();
    if !tmux.drained
        || tmux.reconciled()
        || tmux.asking(&Asked::Reconcile)
        || !matches!(tmux.link, Link::Up { .. })
        || state.writes_refused(now.wall)
    {
        return None;
    }
    if tmux.unfinished_reconciles >= UNFINISHED_KEPT {
        if tmux.reconcile_given_up {
            return None;
        }
        state.tmux.reconcile_given_up = true;
        return Some(log(
            LogLevel::Warn,
            format!(
                "reconciling the tabs failed {UNFINISHED_KEPT} times; moa tries again at the next connection"
            ),
        ));
    }
    state.tmux.ask(Asked::Reconcile, list_panes())
}

/// What the answers to reconcile's pane queries lead to. `replied` is whether the input was
/// tmux's reply: otherwise the link went before it came, and the next connection asks again.
pub(crate) fn answered(
    state: &mut State,
    now: &Now,
    answers: Vec<Answer>,
    replied: bool,
) -> Vec<Effect> {
    let mut fx = Vec::new();
    for answer in answers {
        match answer {
            Ok(lists) if lists.len() == 2 => {
                fx.extend(run(state, now, &parse_panes(&lists[0], &lists[1])));
            }
            _ if replied => fx.push(log(LogLevel::Warn, "listing the tabs failed".to_string())),
            _ => {}
        }
    }
    fx
}

/// Reconciles this connection's panes with the bindings, in one commit (R4–R7):
/// 1. a binding is kept when it holds its pane (R4); one pane keeps one binding, the newest;
/// 2. an untracked kept binding is kept while its agent still runs there (R31);
/// 3. a live tab that no kept binding holds is bound to the session it runs (R6);
/// 4. the spike's panes on this server are forgotten, verified or not;
/// 5. a live session left without a binding goes to sleep (R5), except one somewhere else,
///    one whose launch is waiting, and one whose spike pane is on another server.
///
/// A tab whose processes can't be read proves nothing (R31): its session, when its tag or
/// spike pane names one, is left as it is; one that names none could be any live session
/// with no binding, and none of those goes to sleep this time.
///
/// A refused commit leaves the panes unreconciled, to be asked again (R10). A run that
/// panics is counted, as one that never finished ([`ask_if_due`]).
pub(crate) fn run(state: &mut State, now: &Now, panes: &[PaneInfo]) -> Vec<Effect> {
    state.tmux.unfinished_reconciles += 1;
    let fx = reconcile(state, now, panes);
    state.tmux.unfinished_reconciles = 0;
    fx
}

fn reconcile(state: &mut State, now: &Now, panes: &[PaneInfo]) -> Vec<Effect> {
    let Link::Up {
        generation, socket, ..
    } = &state.tmux().link
    else {
        return Vec::new();
    };
    let (generation, socket) = (*generation, socket.clone());
    let listed: BTreeMap<PaneId, &PaneInfo> = panes.iter().map(|p| (p.pane, p)).collect();

    // 1. The bindings that hold their pane, newest first per pane. `holds` keeps only the
    // binding of the session a pane's tag names, so two can share a pane only when it has
    // no tag (R4).
    let mut holding: BTreeMap<PaneId, Vec<&PaneBinding>> = BTreeMap::new();
    for b in state.pane_bindings() {
        let info = b.pane_id.parse().ok().and_then(|p| listed.get(&p).copied());
        if let Some(info) = info.filter(|i| holds(b, generation, Some(i))) {
            holding.entry(info.pane).or_default().push(b);
        }
    }
    // The panes a binding held: none of them is bound again below, even when step 2 lets
    // its binding go (its agent was just found gone).
    let held: BTreeSet<PaneId> = holding.keys().copied().collect();
    let mut kept: BTreeSet<SessionId> = BTreeSet::new();
    // The tabs an adopted agent left: their tags come off once the commit is saved.
    let mut to_untag: Vec<PaneId> = Vec::new();
    // The kept bindings' tabs that show no tag (one lost with the link): tagged again, and
    // marked adopted too when that is missing from an untracked one's.
    let mut retag: Vec<(PaneId, SessionId, bool)> = Vec::new();
    // The tabs tagged for a session with no binding whose agent isn't the session's: their
    // tags come off, as the untag a lost link dropped would have taken them.
    let mut stale_tags: Vec<PaneId> = Vec::new();
    // The tabs whose processes couldn't be read; the sessions they name, left as they are;
    // and whether one named none.
    let mut unread = 0usize;
    let mut left_alone: BTreeSet<SessionId> = BTreeSet::new();
    let mut unread_unnamed = false;
    for (pane, mut bs) in holding {
        bs.sort_by_key(|b| std::cmp::Reverse((b.bound_at, b.session_id)));
        let b = bs[0];
        let info = listed[&pane];
        // 2. An adopted agent leaves its shell tab open when it exits (U14). One that
        // can't be read keeps its binding.
        if !b.tracked {
            match untracked_agent_runs(state, b, Some(info)) {
                Recheck::Runs => {}
                Recheck::Unknown => unread += 1,
                Recheck::Gone => {
                    if tagged_for(info, b.session_id) {
                        to_untag.push(pane);
                    }
                    continue;
                }
            }
        }
        if info.tag == Tag::Unset {
            retag.push((pane, b.session_id, !b.tracked && !info.adopted));
        }
        kept.insert(b.session_id);
    }

    // 3. Rebinding, in listing order.
    let waiting: BTreeSet<SessionId> = state.tmux().waiting_opens().collect();
    let mut bound: BTreeMap<SessionId, PaneBinding> = BTreeMap::new();
    // Each new binding's tab, its session, and whether it is marked adopted too.
    let mut to_tag: Vec<(PaneId, SessionId, bool)> = Vec::new();
    // The conversations a tab was bound by that become their session's current one.
    let mut made_current: Vec<Conversation> = Vec::new();
    let free = |id: SessionId, bound: &BTreeMap<SessionId, PaneBinding>| {
        state.session(id).is_some()
            && !kept.contains(&id)
            && !bound.contains_key(&id)
            && !waiting.contains(&id)
    };
    for info in panes {
        if info.dead || held.contains(&info.pane) {
            continue;
        }
        match match_tab(state, now.wall, info, &socket, |id| free(id, &bound)) {
            TabMatch::Nobody => {}
            TabMatch::Stale => stale_tags.push(info.pane),
            TabMatch::Unread(named) => {
                unread += 1;
                match named {
                    Some(id) => {
                        left_alone.insert(id);
                    }
                    None => unread_unnamed = true,
                }
            }
            TabMatch::Bind {
                id,
                tracked,
                tag,
                current,
            } => {
                if let Some(adopted) = tag {
                    to_tag.push((info.pane, id, adopted));
                }
                made_current.extend(current);
                bound.insert(
                    id,
                    new_binding(id, generation, &socket, info, now.wall, tracked),
                );
            }
        }
    }

    // 4. The spike's panes on this server: bound above when they still run their agent. One
    // that couldn't be read stays, for the next look.
    let spike_gone: Vec<SessionId> = state
        .spike_panes()
        .filter(|p| on_this_server(p.tmux.as_deref(), &socket))
        .map(|p| p.session_id)
        .filter(|id| !left_alone.contains(id))
        .collect();

    // 5. Asleep, or live again.
    let mut sessions: Vec<Session> = Vec::new();
    let mut slept = 0usize;
    for s in state.sessions() {
        if bound.contains_key(&s.id) {
            sessions.push(Session {
                lifecycle: Lifecycle::Live,
                asleep_at: None,
                location: Location::Here,
                ..s.clone()
            });
        } else if !kept.contains(&s.id)
            && !left_alone.contains(&s.id)
            && !(unread_unnamed && state.binding(s.id).is_none())
            && sleeps(state, s, &waiting, Some(&socket))
        {
            sessions.push(asleep(s, now.wall));
            slept += 1;
        }
    }

    let mut changes: Vec<Change> = state
        .pane_bindings()
        .map(|b| b.session_id)
        .filter(|id| !kept.contains(id) && !bound.contains_key(id))
        .map(Change::DeletePaneBinding)
        .collect();
    changes.extend(sessions.into_iter().map(Change::Session));
    let (n_kept, n_bound) = (kept.len(), bound.len());
    changes.extend(bound.into_values().map(Change::PaneBinding));
    changes.extend(made_current.into_iter().map(Change::Conversation));
    changes.extend(spike_gone.into_iter().map(Change::DeleteSpikePane));

    let mut tx = Tx::new(now);
    if !changes.is_empty() && tx.commit(state, changes).is_err() {
        return vec![log(
            LogLevel::Warn,
            "reconciling the tabs wasn't saved; moa asks again".to_string(),
        )];
    }
    // A new binding's tab is tagged with its session, so the next reconcile knows it.
    for (pane, id, adopted) in to_tag {
        tag_pane(state, &mut tx, pane, id, adopted);
    }
    for pane in to_untag {
        untag_pane(state, &mut tx, pane);
    }
    state.tmux.set_reconciled();
    let mut fx = vec![log(
        LogLevel::Info,
        format!("reconciled the tabs: {n_kept} kept, {n_bound} bound, {slept} put to sleep"),
    )];
    if unread > 0 {
        fx.push(log(
            LogLevel::Debug,
            format!("{unread} tabs couldn't be read; their sessions are left as they are"),
        ));
    }
    fx.extend(post_commit(state, tx));
    // What these tags stand for is saved already (a kept binding, or none at all): they go
    // out whether or not anything was saved now.
    let mut cmds: Vec<Cmd> = Vec::new();
    for (pane, id, adopted) in retag {
        state.tmux.tag_sent(pane, id);
        cmds.extend(tag_cmds(pane, id, adopted));
    }
    for pane in stale_tags {
        state.tmux.untag_sent(pane);
        cmds.extend(untag_cmds(pane));
    }
    if !cmds.is_empty()
        && let Link::Up { conn, .. } = state.tmux().link
    {
        fx.push(Effect::Tmux {
            conn,
            cmds,
            token: None,
        });
    }
    fx
}

/// What a listed tab that no kept binding holds says about whose agent runs in it (R6).
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum TabMatch {
    /// No free session's: no agent moa knows, another session's tag, or a session that
    /// isn't free.
    Nobody,
    /// Free session `id`'s agent runs in it: it is bound, `tracked` or not. `tag` is set when
    /// the tab's tags have to be put on, with whether it is marked adopted too; `current` is
    /// the conversation it was found by, when that becomes its session's current one.
    Bind {
        id: SessionId,
        tracked: bool,
        tag: Option<bool>,
        current: Option<Conversation>,
    },
    /// Tagged for a free session, marked adopted or a terminal, and its agent isn't the
    /// session's (an untag lost with the link): its tags come off.
    Stale,
    /// Its processes couldn't be read. The session its tag or spike pane names, if any, is
    /// left as it is.
    Unread(Option<SessionId>),
}

/// Whose agent runs in listed tab `info`, which no kept binding holds (R6, R7), among the
/// sessions `free` allows:
/// - (a) the tab's tag names the session. An adopted agent's tab outlives the agent, so
///   its agent is checked first ([`tag_proves`]); a terminal's too. A terminal with a session
///   tag but no adopted mark (one moa didn't set: `tag_pane` marks first, `untag_pane`
///   unmarks last) is marked now;
/// - (b) its agent names a known session, by `MOA_SESSION` or conversation. By
///   conversation, the one it runs becomes its session's current one, as an agent's start
///   on it would make it: a restart then resumes that one;
/// - (c) it is the spike's pane for a session, running its agent. The spike started that
///   tab with the agent as its process, so its start time isn't asked for.
///
/// A terminal moa opened is where an agent is typed by hand. Its binding is never tracked: a
/// tracked tab is killed without its agent read again, and this one is the user's shell.
/// An untracked binding of an agent that names no conversation is proven again by its start
/// time (R31): with none to read, it isn't bound, since the next look would let it go.
pub(crate) fn match_tab(
    state: &State,
    now: UtcMs,
    info: &PaneInfo,
    socket: &str,
    free: impl Fn(SessionId) -> bool,
) -> TabMatch {
    let inspect = state.deps().inspect;
    let untracked = info.adopted || info.term;
    match info.tag {
        // A tag that isn't a session id counts as another session's (R4).
        Tag::Other => TabMatch::Nobody,
        Tag::Session(id) => {
            if !free(id) {
                return TabMatch::Nobody;
            }
            if untracked {
                let agent = match inspect(info.pid, &info.start) {
                    Inspected::Unknown => return TabMatch::Unread(Some(id)),
                    found => found.into_agent(),
                };
                if !tag_proves(state, id, agent.as_ref(), &info.cwd, now) {
                    return TabMatch::Stale;
                }
            }
            TabMatch::Bind {
                id,
                tracked: !untracked,
                tag: (info.term && !info.adopted).then_some(true),
                current: None,
            }
        }
        Tag::Unset => {
            let spike = spike_candidate(state, info.pane, socket);
            let agent = match inspect(info.pid, &info.start) {
                Inspected::Unknown => return TabMatch::Unread(spike),
                found => found.into_agent(),
            };
            let by_moa = agent.as_ref().and_then(|a| moa_session_of(state, a));
            let by_conv = agent.as_ref().and_then(|a| {
                let conv = a.conversation.as_deref()?;
                state.conversation_of_agent(a.kind, conv)
            });
            let named = by_moa.or(by_conv.map(|c| c.session_id));
            let Some(id) = named.or_else(|| {
                spike.filter(|id| agent_runs(state, *id, agent.as_ref(), Some(&info.cwd), None))
            }) else {
                return TabMatch::Nobody;
            };
            if !free(id) {
                return TabMatch::Nobody;
            }
            let has_moa_session = agent.as_ref().is_some_and(|a| a.moa_session.is_some());
            let tracked = !untracked && (has_moa_session || spike == Some(id));
            if !tracked
                && agent
                    .as_ref()
                    .is_some_and(|a| a.conversation.is_none() && a.started_at.is_none())
            {
                return TabMatch::Nobody;
            }
            let current = by_conv
                .filter(|c| by_moa.is_none() && c.session_id == id)
                .filter(|c| {
                    state
                        .current_conversation(id)
                        .is_some_and(|cur| cur.conversation_id != c.conversation_id)
                })
                .map(|c| Conversation {
                    last_seen_at: now.max(c.last_seen_at),
                    ..c.clone()
                });
            // An untracked binding's tab is marked adopted, as `adopt` marks one: its agent
            // may leave it open, so the next reconcile checks the agent before binding it
            // again by its tag (R7, R31).
            TabMatch::Bind {
                id,
                tracked,
                tag: Some(!tracked && !info.adopted),
                current,
            }
        }
    }
}

/// Whether `agent`, in a tab tagged for session `id` and marked adopted or a terminal, is
/// the session's (R31). While the session is live, any agent of its that started by now
/// ([`agent_runs`]). Once it isn't, only the agent's conversation or its `MOA_SESSION`
/// proves it: a plain `claude` typed in the shell after the session's agent left runs on the
/// same store in the same folder, and when moa noticed the agent leave says nothing of when
/// it did.
fn tag_proves(
    state: &State,
    id: SessionId,
    agent: Option<&PaneAgent>,
    cwd: &str,
    now: UtcMs,
) -> bool {
    if state
        .session(id)
        .is_some_and(|s| s.lifecycle == Lifecycle::Live)
    {
        return agent_runs(state, id, agent, Some(cwd), Some(now));
    }
    agent.is_some_and(|a| {
        a.kind == AgentKind::Claude
            && (moa_session_of(state, a) == Some(id)
                || (a.conversation.is_some() && agent_runs(state, id, Some(a), Some(cwd), None)))
    })
}

/// Session `id`'s binding to listed tab `info` of the server `generation` at `socket`, made
/// at `now`.
pub(crate) fn new_binding(
    id: SessionId,
    generation: Generation,
    socket: &str,
    info: &PaneInfo,
    now: UtcMs,
    tracked: bool,
) -> PaneBinding {
    PaneBinding {
        session_id: id,
        server_pid: generation.pid,
        server_start: generation.start,
        socket: socket.to_string(),
        pane_id: info.pane.to_string(),
        pane_pid: info.pid,
        bound_at: now,
        tracked,
    }
}

/// Tags `pane` with session `id` once `tx`'s commit is saved (R15), and marks it adopted
/// first when `adopted`: a link lost between the two then leaves an adopted tab with no
/// session, never one that looks tracked. The topology knows the tag at once (R18 reads it).
pub(crate) fn tag_pane(state: &mut State, tx: &mut Tx, pane: PaneId, id: SessionId, adopted: bool) {
    for cmd in tag_cmds(pane, id, adopted) {
        tx.tmux(cmd);
    }
    state.tmux.tag_sent(pane, id);
}

/// What tags `pane` with session `id`: the adopted mark first when `adopted`, then the
/// session tag.
fn tag_cmds(pane: PaneId, id: SessionId, adopted: bool) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    if adopted {
        cmds.push(set_pane_option(pane, "@moa_adopted", "1"));
    }
    cmds.push(set_pane_option(pane, "@moa_session", &id.to_string()));
    cmds
}

/// Takes moa's tags off `pane` once `tx`'s commit is saved (R15): its adopted agent left,
/// its session went to sleep, and the shell tab is the user's again, so an agent typed in it
/// next shows as one moa doesn't track (R23). The session tag goes first, as [`tag_pane`]
/// set it last.
pub(crate) fn untag_pane(state: &mut State, tx: &mut Tx, pane: PaneId) {
    for cmd in untag_cmds(pane) {
        tx.tmux(cmd);
    }
    state.tmux.untag_sent(pane);
}

/// What takes moa's tags off `pane`: the session tag, then the adopted mark.
fn untag_cmds(pane: PaneId) -> [Cmd; 2] {
    [
        unset_pane_option(pane, "@moa_session"),
        unset_pane_option(pane, "@moa_adopted"),
    ]
}

/// Whether listed tab `info` carries the tags of session `id`'s adoption: its session tag,
/// or the adopted mark with no session tag (a link lost between the two).
pub(crate) fn tagged_for(info: &PaneInfo, id: SessionId) -> bool {
    match info.tag {
        Tag::Session(t) => t == id,
        Tag::Unset => info.adopted,
        Tag::Other => false,
    }
}

/// The link found no server (R2, R4): no pane is left, so every binding goes, and every
/// live session goes to sleep, bound or not, except as in [`run`]'s step 5. The spike's
/// panes on this server go too. Only once the start drain has run; the panes stay
/// unreconciled, since there's no connection.
pub(crate) fn no_server(state: &mut State, now: &Now) -> Vec<Effect> {
    if !state.tmux().drained {
        return Vec::new();
    }
    let socket = state.tmux().last_socket().map(str::to_string);
    let waiting: BTreeSet<SessionId> = state.tmux().waiting_opens().collect();
    let mut changes: Vec<Change> = state
        .pane_bindings()
        .map(|b| Change::DeletePaneBinding(b.session_id))
        .collect();
    let dropped = changes.len();
    let sleepers: Vec<Session> = state
        .sessions()
        .filter(|s| sleeps(state, s, &waiting, socket.as_deref()))
        .map(|s| asleep(s, now.wall))
        .collect();
    let slept = sleepers.len();
    changes.extend(sleepers.into_iter().map(Change::Session));
    changes.extend(
        state
            .spike_panes()
            .filter(|p| on_server(p.tmux.as_deref(), socket.as_deref()))
            .map(|p| Change::DeleteSpikePane(p.session_id)),
    );
    if changes.is_empty() {
        return Vec::new();
    }
    let mut tx = Tx::new(now);
    if tx.commit(state, changes).is_err() {
        return vec![log(
            LogLevel::Warn,
            "the sessions of a terminal server that's gone weren't put to sleep; the next connection does it".to_string(),
        )];
    }
    let mut fx = vec![log(
        LogLevel::Info,
        format!("no terminal server: {dropped} bindings dropped, {slept} put to sleep"),
    )];
    fx.extend(post_commit(state, tx));
    fx
}

/// Whether a live session left without a binding goes to sleep (R5): not one somewhere
/// else, not one whose launch is waiting, and not one whose spike pane is on another
/// server, which may still run its agent.
fn sleeps(state: &State, s: &Session, waiting: &BTreeSet<SessionId>, socket: Option<&str>) -> bool {
    s.lifecycle == Lifecycle::Live
        && s.location != Location::Elsewhere
        && !waiting.contains(&s.id)
        && !state
            .spike_panes()
            .any(|p| p.session_id == s.id && !on_server(p.tmux.as_deref(), socket))
}

/// The session whose spike pane is `pane` on this server.
fn spike_candidate(state: &State, pane: PaneId, socket: &str) -> Option<SessionId> {
    state
        .spike_panes()
        .find(|p| p.pane_id.parse() == Ok(pane) && on_this_server(p.tmux.as_deref(), socket))
        .map(|p| p.session_id)
}

/// Whether a spike pane whose `tmux` column is `spike_tmux` is on the server at `socket`:
/// it names no server, or the last parts of the two socket paths are the same (the spike's
/// `on_this_server`).
pub(crate) fn on_this_server(spike_tmux: Option<&str>, socket: &str) -> bool {
    match spike_tmux.filter(|t| !t.is_empty()) {
        None => true,
        Some(t) => last_part(t) == last_part(socket),
    }
}

/// [`on_this_server`], when the server's socket may be unknown (the link never connected):
/// then only a spike pane that names no server is on it.
fn on_server(spike_tmux: Option<&str>, socket: Option<&str>) -> bool {
    match socket {
        Some(socket) => on_this_server(spike_tmux, socket),
        None => spike_tmux.is_none_or(str::is_empty),
    }
}

fn last_part(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The session this brain knows that `agent`'s `MOA_SESSION` names: moa started it for
/// that session.
pub(crate) fn moa_session_of(state: &State, agent: &PaneAgent) -> Option<SessionId> {
    agent
        .moa_session
        .as_deref()
        .and_then(|s| s.parse::<SessionId>().ok())
        .filter(|id| state.session(*id).is_some())
}

/// How much earlier than its binding an agent's start time may read and still count as
/// started by then: Linux gives a process's start in ticks since a boot time it keeps in
/// whole seconds.
const START_SLACK_MS: i64 = 2_000;

/// What a second look at an untracked binding's tab found (R31).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Recheck {
    /// The session's agent still runs there.
    Runs,
    /// Proven: no agent runs there, or one that isn't the session's.
    Gone,
    /// What runs there couldn't be read. Nothing is proven: the session and its binding are
    /// left as they are, and the next look decides.
    Unknown,
}

/// Whether the agent in untracked binding `b`'s tab is still its session's (R31): read
/// again, from `tab` when there is a listing (with its folder), else from the binding's
/// pid (a kill has no listing). An agent naming no conversation must have started by the
/// time the binding was made: one typed in the same shell after the adopted one quit runs on
/// the same store in the same folder, and isn't the session's.
pub(crate) fn untracked_agent_runs(
    state: &State,
    b: &PaneBinding,
    tab: Option<&PaneInfo>,
) -> Recheck {
    let inspect = state.deps().inspect;
    let found = match tab {
        Some(i) => inspect(i.pid, &i.start),
        None => inspect(b.pane_pid, &[]),
    };
    if found == Inspected::Unknown {
        return Recheck::Unknown;
    }
    let cwd = tab.map(|i| i.cwd.as_str());
    if agent_runs(state, b.session_id, found.agent(), cwd, Some(b.bound_at)) {
        Recheck::Runs
    } else {
        Recheck::Gone
    }
}

/// Whether `agent` is session `id`'s agent (R31): a Claude running one of the session's
/// conversations, or naming none on the store of the session's current conversation (R32:
/// no `CLAUDE_CONFIG_DIR` is `<home>/.claude`). With `cwd` (reconcile and `ls` have the
/// listed folder), a Claude naming no conversation must also be in the session's folder or
/// one inside the other (the spike's `related`). A kill passes `None`. With `started_by`, a
/// Claude naming no conversation must also have started by then (a start time it can't
/// read proves nothing).
pub(crate) fn agent_runs(
    state: &State,
    id: SessionId,
    agent: Option<&PaneAgent>,
    cwd: Option<&str>,
    started_by: Option<UtcMs>,
) -> bool {
    let Some(agent) = agent.filter(|a| a.kind == AgentKind::Claude) else {
        return false;
    };
    if let Some(conv) = &agent.conversation {
        return state
            .conversations_of(id)
            .any(|c| c.agent_kind == agent.kind && c.conversation_id == *conv);
    }
    if let Some(by) = started_by
        && agent
            .started_at
            .is_none_or(|t| t.0 > by.0.saturating_add(START_SLACK_MS))
    {
        return false;
    }
    let Some(current) = state.current_conversation(id) else {
        return false;
    };
    if state.store_of(agent.store_dir.as_deref()) != state.store_of(current.store_dir.as_deref()) {
        return false;
    }
    let Some(cwd) = cwd else {
        return true;
    };
    let own = state.session(id).and_then(|s| s.cwd.as_deref());
    own.is_some_and(|own| {
        let canonical = state.deps().canonical;
        related(&canonical(Path::new(cwd)), &canonical(Path::new(own)))
    })
}

/// The same place: equal, or one inside the other (an agent may `cd` into a subfolder).
fn related(a: &Path, b: &Path) -> bool {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    let (a, b) = (a.trim_end_matches('/'), b.trim_end_matches('/'));
    let inside = |x: &str, y: &str| x.strip_prefix(y).is_some_and(|r| r.starts_with('/'));
    !a.is_empty() && !b.is_empty() && (a == b || inside(a, b) || inside(b, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spike_pane_is_on_this_server_by_its_sockets_last_part() {
        assert!(on_this_server(None, "/run/tmux-default"));
        assert!(on_this_server(Some(""), "moa"));
        assert!(on_this_server(Some("/tmp/tmux-501/moa"), "moa"));
        assert!(on_this_server(Some("/tmp/tmux-501/moa"), "/x/y/moa"));
        assert!(!on_this_server(Some("/tmp/tmux-501/moa-other"), "moa"));
        assert!(!on_this_server(
            Some("/tmp/tmux-501/moa"),
            "/run/tmux-default"
        ));
        // With no known server, only a pane that names none is on it.
        assert!(on_server(None, None));
        assert!(!on_server(Some("/tmp/tmux-501/moa"), None));
    }

    #[test]
    fn related_folders_are_equal_or_one_inside_the_other() {
        let r = |a: &str, b: &str| related(Path::new(a), Path::new(b));
        assert!(r("/w/a", "/w/a/"));
        assert!(r("/w/a/sub", "/w/a"));
        assert!(r("/w/a", "/w/a/sub"));
        assert!(!r("/w/ab", "/w/a"));
        assert!(!r("/", "/w"));
        assert!(!r("", "/w"));
    }
}
