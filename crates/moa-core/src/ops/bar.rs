//! `bar.set`: the console's button bar. moa sets one list (`moactl bar`) and you override it
//! (`/bar`); both live in `meta`, and the `feed` topic's `bar` row shows yours, else moa's.

use serde_json::json;

use crate::change::Change;
use crate::console::{self, Parsed};
use crate::core::Tx;
use crate::op::{Done, Op, OpError};
use crate::publish::BAR_KEY_PREFIX;
use crate::state::State;

/// The most buttons a bar holds.
pub const MAX_COMMANDS: usize = 8;
/// The most characters in one button's command.
pub const MAX_LENGTH: usize = 40;

/// Whether `line` may be a button: a command the console runs (checked by the console's own
/// parser, without running it), plus `/help` and `/new`. Not `/bar` itself.
fn check(line: &str, state: &State) -> Result<(), String> {
    if line.chars().count() > MAX_LENGTH {
        return Err(format!(
            "{line}: a button's command is at most {MAX_LENGTH} characters"
        ));
    }
    match console::parse(line, None, state.deps()) {
        Ok(Parsed::Run(Op::BarSet { .. })) => Err(format!("{line}: a button can't set the bar")),
        Ok(_) => Ok(()),
        Err(why) => Err(format!("{line}: {why}")),
    }
}

pub(crate) fn set(
    state: &mut State,
    tx: &mut Tx,
    commands: Vec<String>,
    user: bool,
) -> Result<Done, OpError> {
    if commands.len() > MAX_COMMANDS {
        return Err(OpError(format!(
            "a bar holds at most {MAX_COMMANDS} buttons"
        )));
    }
    for line in &commands {
        check(line, state).map_err(OpError)?;
    }
    let by = if user { "you" } else { "moa" };
    let key = format!("{BAR_KEY_PREFIX}{by}");
    let value = (!commands.is_empty())
        .then(|| serde_json::to_string(&commands).expect("a list of strings serializes"));
    let done = Done {
        text: if commands.is_empty() {
            format!("cleared {by} bar")
        } else {
            format!("the bar is set: {}", commands.join(" "))
        },
        data: json!({ "by": by, "commands": commands }),
    };
    // Already as asked: nothing to save.
    if state.meta(&key) == value.as_deref() {
        return Ok(done);
    }
    tx.commit(state, vec![Change::Meta { key, value }])?;
    Ok(done)
}
