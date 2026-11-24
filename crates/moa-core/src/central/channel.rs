//! The one channel (K3, K4): what goes to moa's agent, from whom ([`Source`]), and when. Your
//! prompts go through it, and in 6c its only automatic source is the wake-up, which carries
//! the session reports once they settle; your prompts carry them too. What a prompt carries
//! is chosen at its commit ([`super::reports::batch`]), and that commit deletes it: each
//! report reaches moa once.

use super::{Phase, launch, send};
use crate::Now;
use crate::UtcMs;
use crate::core::fold_in;
use crate::effect::{Effect, TimerKey, rearm};
use crate::state::State;

/// The `meta` key `/auto` sets: `off` stops the wake-ups, and reports ride on your prompts.
pub const AUTO_KEY: &str = "moa.auto";

/// Who a prompt to moa's agent comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// You, from the console.
    You,
    /// A wake-up, with the session reports.
    Wake,
}

/// Whether moa reads session reports on its own: `meta` `moa.auto` isn't `off`.
pub fn auto_on(state: &State) -> bool {
    state.meta(AUTO_KEY) != Some("off")
}

/// The wake-up's check at the end of every input: a wake that is due goes, and the `Wake`
/// timer is armed for the next one, or cancelled when none can come. While writes are
/// refused a wake waits for the refusal's end too; when moa's agent can't be started, it
/// waits for the next input that changes something.
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    if fired {
        state.central.wake_armed = None;
    }
    let mut next = due(state);
    let refused = state
        .refuse_writes()
        .filter(|_| state.writes_refused(now.wall))
        .map(|r| r.until);
    if let Some(until) = refused {
        // Nothing else ends a refusal from the core's side.
        next = next.map(|at| at.max(until));
    } else if next.is_some_and(|at| at <= now.wall) {
        // A running agent needs no start, so only a stopped one's launch can hold a wake.
        next = match state.central.running() || launch::spec(state).is_ok() {
            false => None,
            true => {
                let more = send(state, now, Source::Wake, String::new(), None);
                fold_in(fx, more);
                due(state)
            }
        };
    }
    fx.extend(rearm(
        &mut state.central.wake_armed,
        TimerKey::Wake,
        next,
        fired,
    ));
}

/// When the next wake-up is due (K4): once the reports settle (`wake_settle` after the
/// newest), or once the oldest has waited `wake_gap`, whichever comes first, and never
/// sooner than `wake_gap` after the last wake. `None` while nothing is queued, or while a
/// wake can't go: `/auto` is off, or moa's agent failed to start, is starting, has a prompt
/// under way, waits for your answer or is replaying a thread. A turn whose agent exited is
/// over for this: the wake's start takes it, as any prompt's does.
fn due(state: &State) -> Option<UtcMs> {
    let mut reports = state.reports().peekable();
    reports.peek()?;
    let moa = &state.central;
    let open = auto_on(state)
        && matches!(moa.phase, Phase::Off | Phase::Ready)
        && moa.pending.is_none()
        && moa.turn.as_ref().is_none_or(|t| t.exited.is_some())
        && moa.asking.is_empty()
        && moa.replaying.is_none();
    if !open {
        return None;
    }
    let (first, last) = reports.fold((i64::MAX, i64::MIN), |(first, last), r| {
        (first.min(r.queued_at.0), last.max(r.last_at.0))
    });
    let settings = &state.config().moa;
    let settled = last
        .saturating_add(settings.wake_settle_ms)
        .min(first.saturating_add(settings.wake_gap_ms));
    let gap = moa
        .last_wake
        .map_or(i64::MIN, |w| w.0.saturating_add(settings.wake_gap_ms));
    Some(UtcMs(settled.max(gap)))
}
