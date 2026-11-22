//! What moa's agent is told when a thread starts (A9): its constitution, what this brain is
//! for, and the brief.

use crate::Now;
use crate::ops::read::brief;
use crate::state::State;

/// The constitution moa is built with; `Deps::constitution_file` replaces it.
pub const CONSTITUTION: &str = include_str!("constitution.md");

/// What a new thread is told, in its two parts and whole.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SystemText {
    /// `claude_md`, a blank line, then `brief`.
    pub full: String,
    /// The constitution and the brain's scope: what holds for every turn.
    pub claude_md: String,
    /// The brief: how things stand now.
    pub brief: String,
}

/// The text a new thread starts with. A file that can't be read counts as missing.
pub fn system_text(state: &State, now: &Now) -> SystemText {
    let deps = state.deps();
    let read = |p| (deps.read_text)(p).ok().flatten();
    let constitution = read(&deps.constitution_file);
    let constitution = constitution.as_deref().unwrap_or(CONSTITUTION);
    let scope = read(&deps.scope_file);
    let scope = scope.as_deref().unwrap_or("(no scope file)");
    let claude_md = format!(
        "{}\n\n# This brain: {}\n\n{}",
        constitution.trim_end(),
        state.brain(),
        scope.trim_end()
    );
    let brief = brief(state, now).map(|d| d.text).unwrap_or_default();
    SystemText {
        full: format!("{claude_md}\n\n{brief}"),
        claude_md,
        brief,
    }
}
