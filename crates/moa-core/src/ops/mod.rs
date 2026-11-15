//! One handler per verb. Every handler validates its input, builds its changes, then makes
//! one `tx.commit`: an `Err` has applied nothing.

pub mod groups;
pub mod projects;
pub mod read;
pub mod sessions;
pub mod tasks;

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
        Op::SessionFind { q } => read::find(state, now, &q),
        Op::TaskLs { project, all } => tasks::ls(state, now, project.as_deref(), all),
        Op::TaskAdd(a) => tasks::add(state, tx, now, &a),
        Op::TaskSet(a) => tasks::set(state, tx, now, &a),
        Op::NoteAdd {
            text,
            project,
            kind,
        } => tasks::note(state, tx, now, &text, project.as_deref(), kind.as_deref()),
        Op::ProjectShow { name } => projects::show(state, now, &name),
        Op::ProjectAdd {
            name,
            path,
            summary,
        } => projects::add(state, tx, now, &name, &path, summary.as_deref()),
        Op::Brief => read::brief(state, now),
        Op::Attention => read::attention(state, now),
    }
}

/// An activity row written by the brain, as the spike's triggers did.
pub(crate) fn brain_row(
    state: &State,
    tx: &mut Tx,
    now: &Now,
    text: String,
) -> crate::change::Change {
    use crate::model::{ActivityId, ActivityRow, ActivityWho};
    crate::change::Change::Activity(ActivityRow {
        id: ActivityId(tx.next_id(state, crate::state::Table::Activity)),
        at: now.wall,
        who: ActivityWho::Brain,
        text,
        ref_kind: None,
        ref_id: None,
    })
}

/// The project named `name`, ignoring case and surrounding spaces (names are stored
/// trimmed).
pub(crate) fn project_named<'a>(state: &'a State, name: &str) -> Option<&'a crate::model::Project> {
    let want = name.trim().to_lowercase();
    state.projects().find(|p| p.name.to_lowercase() == want)
}
