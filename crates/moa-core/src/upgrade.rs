//! The daemon's upgrade into a new build (L4): it waits until moa is between turns, then the
//! daemon re-execs itself ([`Effect::Exec`]).
//!
//! [`Input::Upgrade`](crate::Input::Upgrade) says one is wanted, because the program was
//! rebuilt or someone asked; a second one changes nothing. At the end of every input
//! ([`run`]), once moa isn't starting, thinking, restarting or holding a permission request,
//! every request still waiting on an ask hears A13's "no reply yet" (its `moactl reply`
//! waits again on the next image), and the exec goes out, once. While an upgrade is wanted,
//! moa's channel starts no automatic prompt, so the wait ends with the turn under way; once
//! the exec has gone out, a prompt of yours is refused ([`UPGRADING`]).
//!
//! The next image says what the upgrade needs of you, once ([`notice`], L7): when its build
//! isn't the one the last start saved (`meta` [`BUILD_KEY`]), the most demanding of the four
//! `NOTICE_*` texts goes in moa's thread and the activity feed, in the commit that saves the
//! new build.

use crate::asks;
use crate::central::{self, feed};
use crate::change::Change;
use crate::core::{Tx, fold_in, post_commit};
use crate::effect::{Down, Effect, LogLevel, LogLine};
use crate::model::{ActivityWho, AgentKind, Lifecycle};
use crate::state::State;
use crate::tmux::link::Link;
use crate::{Now, UtcMs};

/// The `meta` key holding the build of the daemon's last start (L7).
pub const BUILD_KEY: &str = "daemon.build";

/// What the start after an upgrade says, the most demanding first (§6.15.5): an option tmux
/// reads only when a terminal attaches changed.
pub const NOTICE_REATTACH: &str = "moa upgraded: re-attach your terminal";
/// The daemon's pid changed: the old one stopped, crashed, or predates upgrades in place.
pub const NOTICE_RESTARTED: &str = "moa upgraded: the daemon restarted itself";
/// An upgrade in place that needs nothing of you.
pub const NOTICE_NOTHING: &str = "moa upgraded: nothing to do";

/// The session plugin changed, and `n` Claude sessions were live at the start's reconcile:
/// they keep the hooks they started with. One is said in the singular.
pub fn notice_hooks(n: usize) -> String {
    match n {
        1 => {
            "moa upgraded: 1 running session uses the old hooks; it gets the new ones when resumed"
                .to_string()
        }
        n => format!(
            "moa upgraded: {n} running sessions use the old hooks; they get the new ones when resumed"
        ),
    }
}

/// What the daemon's shell knows of its start, for the notice (L7). The core reads no file:
/// the shell finds these out as it comes up and hands them in.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct UpgradeFacts {
    /// This image's build, saved as `meta` [`BUILD_KEY`].
    pub build: String,
    /// This image took over from the previous one in place (same pid).
    pub reexec: bool,
    /// This start rewrote the plugin sessions start with.
    pub plugin_changed: bool,
    /// This start changed an option of the tmux config that tmux reads only when a terminal
    /// attaches.
    pub reattach: bool,
}

/// The start's notice: its facts, whether it is settled (said, saved, or not owed), and
/// whether the log has said that a commit of it failed, which it says once per owed notice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Notice {
    facts: UpgradeFacts,
    done: bool,
    warned: bool,
}

impl Notice {
    pub(crate) fn new(facts: UpgradeFacts) -> Notice {
        Notice {
            facts,
            done: false,
            warned: false,
        }
    }
}

/// What `status` says while an upgrade waits.
pub const WAITING: &str = "waiting for moa's turn";
/// What a prompt hears once the upgrade has gone out: the wind-down would cut its turn off.
pub const UPGRADING: &str = "moa is upgrading; try again in a moment";

/// Why the daemon upgrades.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// Its program was replaced by a new build, which said its build when asked.
    Rebuilt,
    /// A client asked (`moa upgrade`, or a client of a newer build).
    Asked,
}

/// An upgrade that is wanted. Kept in memory: the next image starts without one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Want {
    pub why: Why,
    /// When it was first wanted.
    pub since: UtcMs,
    /// The exec has gone out.
    pub(crate) sent: bool,
}

/// `Input::Upgrade`: an upgrade is wanted from now, unless one is already.
pub(crate) fn want(state: &mut State, why: Why, now: &Now) -> Vec<Effect> {
    if state.upgrade.is_some() {
        return Vec::new();
    }
    state.upgrade = Some(Want {
        why,
        since: now.wall,
        sent: false,
    });
    vec![log(format!("an upgrade is wanted ({})", word(why)))]
}

/// Whether an upgrade is wanted (and moa's channel holds its automatic prompts).
pub(crate) fn wanted(state: &State) -> bool {
    state.upgrade.is_some()
}

/// Whether the exec has gone out: no new turn of moa's may start, as the wind-down would cut
/// it off.
pub(crate) fn exec_sent(state: &State) -> bool {
    state.upgrade.is_some_and(|w| w.sent)
}

/// At the end of every input, once this start's tabs are reconciled (or no reconcile is
/// coming: the link is down, or its reconciles were given up on) and writes are accepted,
/// the start's notice is settled, once:
/// - the build the last start saved is this one: nothing is said or written;
/// - no build is saved (a new root, or the first start of a build that saves it): it is
///   saved, and nothing is said, as a first start isn't an upgrade;
/// - another build: the notice ([`text`]) is a `system` line in moa's current thread, when
///   there is one, and an activity row always, in the commit that saves this build.
///
/// A refused commit leaves it owed, to be said by the next input that can save it; a commit
/// that fails otherwise is logged once.
pub(crate) fn notice(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let Some(facts) = state
        .notice
        .as_ref()
        .filter(|n| !n.done)
        .map(|n| n.facts.clone())
    else {
        return;
    };
    let tmux = state.tmux();
    // A reconcile given up on won't come on this connection either: the notice isn't held
    // until the next one.
    let settled =
        tmux.drained && (tmux.reconciled() || tmux.reconcile_given_up || tmux.link == Link::Down);
    if !settled || state.writes_refused(now.wall) {
        return;
    }
    let saved = state.meta(BUILD_KEY).map(str::to_string);
    if saved.as_deref() == Some(facts.build.as_str()) {
        settle(state);
        return;
    }
    let mut tx = Tx::new(now);
    let mut changes = vec![Change::Meta {
        key: BUILD_KEY.to_string(),
        value: Some(facts.build.clone()),
    }];
    let said = saved.map(|old| (old, text(state, &facts)));
    if let Some((_, text)) = &said {
        if let Some(thread) = central::card_thread(state) {
            let body = feed::text_body(text);
            let row = feed::row(
                state,
                &mut tx,
                now.wall,
                &thread,
                feed::kind::SYSTEM,
                None,
                body,
                None,
            );
            changes.push(Change::Feed(row));
        }
        changes.push(tx.activity_row(state, ActivityWho::Brain, text.clone(), None));
    }
    if tx.commit(state, changes).is_err() {
        // Tried again at every input; said once.
        if let Some(n) = &mut state.notice
            && !n.warned
        {
            n.warned = true;
            fx.push(Effect::Log(LogLine {
                level: LogLevel::Warn,
                message: "the upgrade's notice wasn't saved; moa says it at the next input"
                    .to_string(),
            }));
        }
        return;
    }
    settle(state);
    let line = match &said {
        Some((old, _)) => format!("upgraded from build {old} to {}", facts.build),
        None => format!("build {} saved", facts.build),
    };
    fx.push(log(line));
    fold_in(fx, post_commit(state, tx));
}

/// The notice the start after an upgrade says, the most demanding first:
/// - an attach-time tmux option changed, unless no tmux server was running (no terminal is
///   attached under the old options, and the next attach reads the new ones);
/// - the session plugin changed while Claude sessions run (the plugin is Claude Code's);
/// - the pid changed;
/// - nothing.
fn text(state: &State, facts: &UpgradeFacts) -> String {
    let tmux = state.tmux();
    let no_server = tmux.link == Link::Down && tmux.last_down == Some(Down::NoServer);
    let live = state
        .sessions()
        .filter(|s| s.lifecycle == Lifecycle::Live && s.agent_kind == AgentKind::Claude)
        .count();
    if facts.reattach && !no_server {
        NOTICE_REATTACH.to_string()
    } else if facts.plugin_changed && live > 0 {
        notice_hooks(live)
    } else if !facts.reexec {
        NOTICE_RESTARTED.to_string()
    } else {
        NOTICE_NOTHING.to_string()
    }
}

fn settle(state: &mut State) {
    if let Some(n) = &mut state.notice {
        n.done = true;
        n.warned = false;
    }
}

/// At a reload: an exec that went out may have been lost with the input that sent it (a
/// panic after it), so it goes out again at this input's end ([`run`]); a second one is
/// harmless, while a lost one would leave every prompt refused with no exec coming.
///
/// The notice too: the commit that said it may not have been saved. Settled again, it reads
/// the build the reload loaded, so a notice that was saved isn't said twice.
pub(crate) fn reload(state: &mut State) {
    if let Some(want) = &mut state.upgrade {
        want.sent = false;
    }
    if let Some(n) = &mut state.notice {
        n.done = false;
        n.warned = false;
    }
}

/// What `status` says about an upgrade: [`WAITING`] while it waits for moa's turn.
pub fn status(state: &State) -> Option<&'static str> {
    state.upgrade.filter(|w| !w.sent).map(|_| WAITING)
}

/// At the end of every input: once moa is between turns, every waiting ask request hears
/// "no reply yet", then the exec goes out, once. After it, a request that parks meanwhile
/// hears the same at once.
pub(crate) fn run(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let Some(want) = state.upgrade else {
        return;
    };
    if want.sent {
        fold_in(fx, asks::answer_all_waiting(state));
        return;
    }
    if state.central.busy() || state.central.restarting.is_some() {
        return;
    }
    state.upgrade = Some(Want { sent: true, ..want });
    fold_in(fx, asks::answer_all_waiting(state));
    let waited = (now.wall.0 - want.since.0).max(0) / 1_000;
    fx.push(log(format!(
        "upgrading ({}) after {waited} s",
        word(want.why)
    )));
    fx.push(Effect::Exec);
}

fn word(why: Why) -> &'static str {
    match why {
        Why::Rebuilt => "the program was rebuilt",
        Why::Asked => "asked",
    }
}

fn log(message: String) -> Effect {
    Effect::Log(LogLine {
        level: LogLevel::Info,
        message,
    })
}
