use moa_core::op::Op;
use moa_testkit::brain::{TypicalBrain, run};

#[test]
fn typical_brain_starts() {
    let brain = TypicalBrain::new();
    let mut state = brain.state();
    assert_eq!(state.sessions().count(), 9);
    let done = run(&mut state, Op::GroupAdd { path: "X".into() }, &brain.now());
    assert!(done.is_ok(), "{done:?}");
}
