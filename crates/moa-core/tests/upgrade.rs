//! The daemon's upgrade in the core (L4): it waits for moa's turn to end, goes out once, and
//! after that no new turn of moa's starts.

mod common;

use common::central::{ended, first_turn, moa, prompt};
use common::replies;
use moa_core::effect::{Effect, Input, handle_checked};
use moa_core::op::OpError;
use moa_core::state::State;
use moa_core::upgrade::{UPGRADING, WAITING, Why, status};
use moa_testkit::brain::TypicalBrain;

fn upgrade(b: &TypicalBrain, st: &mut State, why: Why) -> Vec<Effect> {
    handle_checked(st, Input::Upgrade(why), b.now())
}

fn execs(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::Exec)).count()
}

/// Wanted mid-turn, the upgrade waits (and says so); a second want changes nothing; the
/// turn's end lets it go, once.
#[test]
fn an_upgrade_waits_for_moas_turn_then_goes_once() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    first_turn(&mut st, &b.now(), "th-1", false);
    assert_eq!(status(&st), None);

    assert_eq!(execs(&upgrade(&b, &mut st, Why::Rebuilt)), 0);
    assert_eq!(status(&st), Some(WAITING));
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 0);

    let generation = st.central().generation;
    let fx = handle_checked(&mut st, Input::Acp(ended(generation, 1)), b.now());
    assert_eq!(execs(&fx), 1, "{fx:?}");
    assert_eq!(status(&st), None, "it has gone out");
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 0, "once");
}

/// M1: once the exec has gone out, a prompt is refused rather than starting a turn the
/// wind-down would cut off.
#[test]
fn a_prompt_after_the_exec_is_refused() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 1);
    let fx = prompt(&b, &mut st, 2, "are you there?");
    let got = replies(&fx);
    assert_eq!(got.len(), 1, "{fx:?}");
    assert_eq!(got[0].1, Err(OpError(UPGRADING.into())));
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Acp(_))),
        "moa's agent isn't started: {fx:?}"
    );
}
