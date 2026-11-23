//! What moa's agent is told when a thread starts (A9): its constitution, what this brain is
//! for, and the brief.

use std::path::Path;

use crate::Now;
use crate::effect::{LogLevel, LogLine};
use crate::ops::read::brief;
use crate::ops::term::with_tilde;
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
    /// One warning for each part that fell back because it couldn't be had: a file there
    /// but unreadable, or the brief. The caller logs them. A missing file says nothing.
    pub warnings: Vec<LogLine>,
}

/// The text a new thread starts with. A file that can't be read is left out as a missing
/// one is, with a warning.
pub fn system_text(state: &State, now: &Now) -> SystemText {
    let deps = state.deps();
    let mut warnings = Vec::new();
    let mut warn = |message: String| {
        warnings.push(LogLine {
            level: LogLevel::Warn,
            message,
        })
    };
    let mut read = |path: &Path, instead: &str| match (deps.read_text)(path) {
        Ok(text) => text,
        Err(e) => {
            let file = with_tilde(&path.display().to_string(), &deps.home, deps.canonical);
            warn(format!(
                "{file} can't be read ({e}); moa's agent is told {instead}"
            ));
            None
        }
    };
    let constitution = read(&deps.constitution_file, "the built-in constitution");
    let scope = read(&deps.scope_file, "this brain has no scope file");
    let constitution = constitution.as_deref().unwrap_or(CONSTITUTION);
    let scope = scope.as_deref().unwrap_or("(no scope file)");
    let claude_md = format!(
        "{}\n\n# This brain: {}\n\n{}",
        constitution.trim_end(),
        state.brain(),
        scope.trim_end()
    );
    let brief = match brief(state, now) {
        Ok(done) => done.text,
        Err(e) => {
            warn(format!(
                "the brief couldn't be made ({}); moa's agent starts without it",
                e.0
            ));
            String::new()
        }
    };
    SystemText {
        full: format!("{claude_md}\n\n{brief}"),
        claude_md,
        brief,
        warnings,
    }
}
