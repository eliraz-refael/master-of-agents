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

use crate::asks;
use crate::core::fold_in;
use crate::effect::{Effect, LogLevel, LogLine};
use crate::state::State;
use crate::{Now, UtcMs};

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

/// At a reload: an exec that went out may have been lost with the input that sent it (a
/// panic after it), so it goes out again at this input's end ([`run`]); a second one is
/// harmless, while a lost one would leave every prompt refused with no exec coming.
pub(crate) fn reload(state: &mut State) {
    if let Some(want) = &mut state.upgrade {
        want.sent = false;
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
