//! One handler per verb. Every handler validates its input, builds its changes, then makes
//! one `tx.commit`: an `Err` has applied nothing.

pub mod adopt;
pub mod groups;
pub mod launch;
pub mod layout;
pub mod projects;
pub mod read;
pub mod sessions;
pub mod suggestions;
pub mod tasks;
pub mod term;

use crate::Now;
use crate::core::Tx;
use crate::effect::Effect;
use crate::op::{Done, Op, OpError};
use crate::state::{Caller, State};

/// Runs `op`. What moa's log is told besides goes in `logs`, whatever the answer.
pub(crate) fn run(
    state: &mut State,
    tx: &mut Tx,
    caller: &Caller,
    op: Op,
    now: &Now,
    logs: &mut Vec<Effect>,
) -> Result<Done, OpError> {
    match op {
        Op::GroupAdd { path } => groups::add(state, tx, now, &path),
        Op::GroupRename { old, new } => groups::rename(state, tx, now, &old, &new),
        Op::GroupRm { path } => groups::rm(state, tx, &path),
        Op::GroupFold { path, collapsed } => groups::fold(state, tx, &path, collapsed),
        Op::SessionLs { all } => sessions::ls(state, now, all),
        Op::SessionSet { q, title, group } => {
            sessions::set(state, tx, now, &q, title.as_deref(), group.as_deref())
        }
        Op::SessionArchive { qs } => sessions::archive(state, tx, now, &qs),
        Op::SessionClose { q } => sessions::close(state, tx, now, &q, logs),
        Op::SessionSleep {
            qs,
            idle_ms,
            dry_run,
        } => sessions::sleep(state, tx, now, &qs, idle_ms, dry_run, logs),
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
        Op::Brief(_) => read::brief(state, now),
        Op::Attention(_) => read::attention(state, now),
        Op::Suggest { text, actions } => suggestions::suggest(state, tx, now, &text, &actions),
        Op::SuggestionAnswer { id, answer } => {
            suggestions::answer(state, tx, caller, now, id, answer, logs)
        }
        Op::SuggestionLs(_) => suggestions::ls(state),
        Op::TermName { target, name } => term::name(state, tx, &target, &name),
        Op::TermNote { target, note } => term::note(state, tx, &target, &note),
        // `handle` answers these with the link; they are never run as ops.
        Op::TmuxEnsure { .. }
        | Op::SessionOpen { .. }
        | Op::SessionFocus { .. }
        | Op::SessionResume { .. }
        | Op::SessionAdopt { .. }
        | Op::SessionRestart { .. }
        | Op::TermOpen { .. }
        | Op::TermShow { .. }
        | Op::TermLs(_)
        | Op::UiPanel { .. }
        | Op::UiHere { .. }
        | Op::UiClose(_)
        | Op::UiSelectView { .. }
        | Op::LayoutList(_)
        | Op::LayoutShow { .. }
        | Op::LayoutSet { .. }
        | Op::LayoutApply { .. }
        | Op::Goto { .. } => Err(OpError("moa can't answer that here".into())),
        // The text is fixed: a panic message reaches the log.
        Op::DebugPanic => panic!("debug.panic was asked for"),
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

/// The first `n` characters of `text`.
pub(crate) fn cut(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
