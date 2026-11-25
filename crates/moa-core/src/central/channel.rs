//! The one channel (K3, K4): what goes to moa's agent, from whom ([`Source`]), and when. Your
//! prompts go through it, and so do moa's automatic ones: the day's opening once you are here
//! (D3), the wake-up, which carries the session reports once they settle, and your yes to a
//! suggestion with nothing to run (D5).
//! Each automatic source goes only while [`may_send`] says so. What a prompt carries is
//! chosen at its commit ([`super::reports::batch`], [`State::yeses`]), and that commit marks
//! it delivered: each report and each yes reaches moa once.

use serde_json::json;

use super::{Phase, launch, opening, send};
use crate::Now;
use crate::UtcMs;
use crate::change::Change;
use crate::core::{Tx, fold_in};
use crate::effect::{Effect, TimerKey, rearm};
use crate::op::{Done, OpError};
use crate::state::State;

/// The `meta` key `/auto` sets: `off` stops the wake-ups, and reports ride on your prompts.
pub const AUTO_KEY: &str = "moa.auto";

/// Who a prompt to moa's agent comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// You, from the console.
    You,
    /// The day's opening, the first time you are here today (D3).
    Brief,
    /// A wake-up, with the session reports.
    Wake,
    /// Your yes to a suggestion with nothing to run, which waits for moa (D5).
    Yes,
}

/// Whether moa reads session reports on its own: `meta` `moa.auto` isn't `off`.
pub fn auto_on(state: &State) -> bool {
    state.meta(AUTO_KEY) != Some("off")
}

/// `console.auto` (`/auto`): the wake-ups on or off, as `on` says, else flipped. It takes
/// effect at once: [`run`] arms or cancels the `Wake` timer at the end of this input. Set as
/// it is, nothing is saved.
pub(crate) fn set_auto(state: &mut State, tx: &mut Tx, on: Option<bool>) -> Result<Done, OpError> {
    let on = on.unwrap_or(!auto_on(state));
    if on != auto_on(state) {
        let value = (!on).then(|| "off".to_string());
        let key = AUTO_KEY.to_string();
        tx.commit(state, vec![Change::Meta { key, value }])?;
    }
    let text = match on {
        true => "moa reads session reports on its own",
        false => {
            "moa won't read session reports on its own; they ride on your next prompt (/auto on)"
        }
    };
    Ok(Done {
        text: text.into(),
        data: json!({ "auto": on }),
    })
}

/// Whether an automatic prompt may go now (D3's gates), whatever its source: moa's agent is
/// [`free`], writes aren't refused, and it runs or can be started. `/auto` isn't one of them:
/// it stops the wake-ups only, which test it on top.
pub fn may_send(state: &State) -> bool {
    free(state)
        && !state.writes_refused(state.now)
        && (state.central.running() || launch::spec(state).is_ok())
}

/// moa's agent is free for an automatic prompt: it is off or ready (not failed to start, not
/// starting), with no prompt under way, no live turn, no request waiting for your answer, no
/// thread replaying and no restart reopening one. A turn whose agent exited is over for this:
/// the prompt's start takes it, as any prompt's does.
fn free(state: &State) -> bool {
    let moa = &state.central;
    // A request waits for you only within a live turn, which shuts the gate too; `asking`
    // shuts it on its own as well.
    debug_assert!(
        moa.asking.is_empty() || moa.turn.as_ref().is_some_and(|t| t.exited.is_none()),
        "a permission request waits outside a live turn"
    );
    matches!(moa.phase, Phase::Off | Phase::Ready)
        && moa.pending.is_none()
        && moa.turn.as_ref().is_none_or(|t| t.exited.is_some())
        && moa.asking.is_empty()
        && moa.replaying.is_none()
        && moa.restarting.is_none()
}

/// The channel's check at the end of every input: an automatic prompt that is due goes when
/// [`may_send`] says it may, and the `Wake` timer is armed for the next one, or cancelled
/// when none can come. While writes are refused it waits for the refusal's end too; when
/// moa's agent can't be started, it waits for the next input that changes something (a yes,
/// for your next prompt, which carries it).
pub(crate) fn run(state: &mut State, now: &Now, fired: bool, fx: &mut Vec<Effect>) {
    if fired {
        state.central.wake_armed = None;
    }
    opening::expire(state, now);
    let mut next = due(state, now);
    let refused = state
        .refuse_writes()
        .filter(|_| state.writes_refused(now.wall))
        .map(|r| r.until);
    if let Some(until) = refused {
        // Nothing else ends a refusal from the core's side.
        next = next.map(|(source, at)| (source, at.max(until)));
    } else if let Some((source, _)) = next.filter(|(_, at)| *at <= now.wall) {
        next = match may_send(state) {
            false => None,
            true => {
                let more = send(state, now, source, String::new(), None);
                fold_in(fx, more);
                due(state, now)
            }
        };
    }
    fx.extend(rearm(
        &mut state.central.wake_armed,
        TimerKey::Wake,
        next.map(|(_, at)| at),
        fired,
    ));
}

/// The next automatic prompt, and when it is due: the day's opening (while due today) and a
/// waiting yes at once, the opening first, a wake-up when [`wake_due`] says, and none sooner than a gap
/// after an automatic prompt failed ([`super::Moa::failed_at`]). The sooner goes; whichever
/// it is carries what the others would. `None` while none waits, or moa's agent isn't
/// [`free`].
fn due(state: &State, now: &Now) -> Option<(Source, UtcMs)> {
    if !free(state) {
        return None;
    }
    let gap = state.config().moa.wake_gap_ms;
    let held = state
        .central
        .failed_at
        .map_or(i64::MIN, |f| f.0.saturating_add(gap));
    let at_once = UtcMs(i64::MIN);
    let brief = opening::due_today(state, now).then_some((Source::Brief, at_once));
    let yes = state.yeses().next().map(|_| (Source::Yes, at_once));
    let wake = wake_due(state).map(|at| (Source::Wake, at));
    // The first of the soonest goes.
    let next = brief
        .into_iter()
        .chain(yes)
        .chain(wake)
        .min_by_key(|(_, at)| *at);
    next.map(|(source, at)| (source, UtcMs(at.0.max(held))))
}

/// When the next wake-up is due (K4): once the reports settle (`wake_settle` after the
/// newest), or once the oldest has waited `wake_gap`, whichever comes first, and never
/// sooner than `wake_gap` after the last wake. `None` while nothing is queued, or `/auto` is
/// off.
fn wake_due(state: &State) -> Option<UtcMs> {
    let mut reports = state.reports().peekable();
    reports.peek()?;
    if !auto_on(state) {
        return None;
    }
    let moa = &state.central;
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
