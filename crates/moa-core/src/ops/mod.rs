//! One handler per verb. Every handler validates its input, builds its changes, then makes
//! one `tx.commit`: an `Err` has applied nothing.

pub mod groups;
pub mod sessions;

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
        Op::GroupRename { old, new } => groups::rename(state, tx, now, &old, &new),
        Op::GroupRm { path } => groups::rm(state, tx, &path),
        Op::SessionLs { all } => sessions::ls(state, now, all),
        Op::SessionSet { q, title, group } => {
            sessions::set(state, tx, now, &q, title.as_deref(), group.as_deref())
        }
        Op::SessionArchive { qs } => sessions::archive(state, tx, now, &qs),
    }
}
