//! Reconcile: once per connection to moa's terminal server, after the start drain (R2), the
//! pane bindings are checked against one listing of the server's panes (R3). A binding that
//! no longer holds its pane goes, and so does its session's liveness: what a `kill-server`,
//! a reboot or a crash left comes back asleep, and nothing starts on its own (spec §6.13,
//! U7). A tab that runs a conversation moa knows is bound (R6), and the panes imported from
//! the spike are verified or cleared (P18).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Now;
use crate::change::Change;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel};
use crate::hooks::asleep;
use crate::model::{AgentKind, Lifecycle, Location, PaneAgent, PaneBinding, Session, SessionId};
use crate::state::State;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Answer, Asked, Link, log, set_pane_option, unset_pane_option};
use crate::tmux::panes::{PaneInfo, Tag, holds, list_panes, parse_panes};

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
    let inspect = state.deps().inspect;
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
    for (pane, mut bs) in holding {
        bs.sort_by_key(|b| std::cmp::Reverse((b.bound_at, b.session_id)));
        let b = bs[0];
        // 2. An adopted agent leaves its shell tab open when it exits (U14).
        if !b.tracked {
            let info = listed[&pane];
            let agent = inspect(info.pid, &info.start);
            if !agent_runs(state, b.session_id, agent.as_ref(), Some(&info.cwd)) {
                if tagged_for(info, b.session_id) {
                    to_untag.push(pane);
                }
                continue;
            }
        }
        kept.insert(b.session_id);
    }

    // 3. Rebinding, in listing order.
    let waiting: BTreeSet<SessionId> = state.tmux().waiting_opens().collect();
    let mut bound: BTreeMap<SessionId, PaneBinding> = BTreeMap::new();
    // Each new binding's tab, its session, and whether it is marked adopted too.
    let mut to_tag: Vec<(PaneId, SessionId, bool)> = Vec::new();
    for info in panes {
        if info.dead || held.contains(&info.pane) {
            continue;
        }
        // A terminal moa opened is where an agent is typed by hand. Its binding is never
        // tracked: a tracked tab is killed without its agent read again, and this one is the
        // user's shell.
        let untracked = info.adopted || info.term;
        let free = |id: SessionId, bound: &BTreeMap<SessionId, PaneBinding>| {
            state.session(id).is_some()
                && !kept.contains(&id)
                && !bound.contains_key(&id)
                && !waiting.contains(&id)
        };
        let (id, tracked) = match info.tag {
            // A tag that isn't a session id counts as another session's (R4).
            Tag::Other => continue,
            // (a) The tab says whose it is. An adopted agent's tab outlives the agent, so
            // its agent is checked first; a terminal's too.
            Tag::Session(id) => {
                if !free(id, &bound) {
                    continue;
                }
                if untracked {
                    let agent = inspect(info.pid, &info.start);
                    if !agent_runs(state, id, agent.as_ref(), Some(&info.cwd)) {
                        continue;
                    }
                }
                // A terminal with a session tag but no adopted mark (one moa didn't set:
                // `tag_pane` marks first, `untag_pane` unmarks last) is marked now.
                if info.term && !info.adopted {
                    to_tag.push((info.pane, id, true));
                }
                (id, !untracked)
            }
            Tag::Unset => {
                let agent = inspect(info.pid, &info.start);
                let spike = spike_candidate(state, info.pane, &socket);
                // (b) The agent names a known session, by `MOA_SESSION` or conversation.
                let named = agent.as_ref().and_then(|a| {
                    a.moa_session
                        .as_deref()
                        .and_then(|s| s.parse::<SessionId>().ok())
                        .filter(|id| state.session(*id).is_some())
                        .or_else(|| {
                            let conv = a.conversation.as_deref()?;
                            Some(state.conversation_of_agent(a.kind, conv)?.session_id)
                        })
                });
                let id = match named {
                    Some(id) => id,
                    // (c) The spike's pane for a session, running its agent.
                    None => match spike
                        .filter(|id| agent_runs(state, *id, agent.as_ref(), Some(&info.cwd)))
                    {
                        Some(id) => id,
                        None => continue,
                    },
                };
                if !free(id, &bound) {
                    continue;
                }
                let has_moa_session = agent.as_ref().is_some_and(|a| a.moa_session.is_some());
                let tracked = !untracked && (has_moa_session || spike == Some(id));
                // An untracked binding's tab is marked adopted, as `adopt` marks one: its
                // agent may leave it open, so the next reconcile checks the agent before
                // binding it again by its tag (R7, R31).
                to_tag.push((info.pane, id, !tracked && !info.adopted));
                (id, tracked)
            }
        };
        bound.insert(
            id,
            PaneBinding {
                session_id: id,
                server_pid: generation.pid,
                server_start: generation.start,
                socket: socket.clone(),
                pane_id: info.pane.to_string(),
                pane_pid: info.pid,
                bound_at: now.wall,
                tracked,
            },
        );
    }

    // 4. The spike's panes on this server: bound above when they still run their agent.
    let spike_gone: Vec<SessionId> = state
        .spike_panes()
        .filter(|p| on_this_server(p.tmux.as_deref(), &socket))
        .map(|p| p.session_id)
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
        } else if !kept.contains(&s.id) && sleeps(state, s, &waiting, Some(&socket)) {
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
    fx.extend(post_commit(state, tx));
    fx
}

/// Tags `pane` with session `id` once `tx`'s commit is saved (R15), and marks it adopted
/// first when `adopted`: a link lost between the two then leaves an adopted tab with no
/// session, never one that looks tracked. The topology knows the tag at once (R18 reads it).
pub(crate) fn tag_pane(state: &mut State, tx: &mut Tx, pane: PaneId, id: SessionId, adopted: bool) {
    if adopted {
        tx.tmux(set_pane_option(pane, "@moa_adopted", "1"));
    }
    tx.tmux(set_pane_option(pane, "@moa_session", &id.to_string()));
    state.tmux.tag_sent(pane, id);
}

/// Takes moa's tags off `pane` once `tx`'s commit is saved (R15): its adopted agent left,
/// its session went to sleep, and the shell tab is the user's again, so an agent typed in it
/// next shows as one moa doesn't track (R23). The session tag goes first, as [`tag_pane`]
/// set it last.
pub(crate) fn untag_pane(state: &mut State, tx: &mut Tx, pane: PaneId) {
    tx.tmux(unset_pane_option(pane, "@moa_session"));
    tx.tmux(unset_pane_option(pane, "@moa_adopted"));
    state.tmux.untag_sent(pane);
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

/// Whether `agent` is session `id`'s agent (R31): a Claude running one of the session's
/// conversations, or naming none on the store of the session's current conversation (R32:
/// no `CLAUDE_CONFIG_DIR` is `<home>/.claude`). With `cwd` (reconcile and `ls` have the
/// listed folder), a Claude naming no conversation must also be in the session's folder or
/// one inside the other (the spike's `related`). A kill passes `None`.
pub(crate) fn agent_runs(
    state: &State,
    id: SessionId,
    agent: Option<&PaneAgent>,
    cwd: Option<&str>,
) -> bool {
    let Some(agent) = agent.filter(|a| a.kind == AgentKind::Claude) else {
        return false;
    };
    if let Some(conv) = &agent.conversation {
        return state
            .conversations_of(id)
            .any(|c| c.agent_kind == agent.kind && c.conversation_id == *conv);
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
