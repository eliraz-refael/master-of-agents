//! One handler per verb. Every handler validates its input, builds its changes, then makes
//! one `tx.commit`: an `Err` has applied nothing.

pub mod groups;

use crate::Now;
use crate::core::Tx;
use crate::op::{Done, Op, OpError};
use crate::state::{Caller, State};

pub(crate) fn run(
    state: &mut State,
    tx: &mut Tx,
    _caller: &Caller,
    op: Op,
    now: &Now,
) -> Result<Done, OpError> {
    match op {
        Op::GroupAdd { path } => groups::add(state, tx, now, &path),
    }
}
