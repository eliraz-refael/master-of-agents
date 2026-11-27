//! The config files read again while the daemon runs (L8, §6.15.4). The daemon's shell
//! watches them and says which changed; the core reads no file of its own.
//!
//! - **`moa.toml`** ([`config`]): the shell reads it as at start and hands over the config
//!   whole, or why it can't be had. One that can't, or that drops the brain this daemon
//!   runs, is refused whole: moa keeps the last good config and says so ([`kept_the_last`]).
//!   A file that is gone keeps the last one too ([`MOA_TOML_GONE`]): no file is the default
//!   config only at start. A good one replaces it, the views hear of what it changed, and a
//!   `[ui]` change has the shell write moa's tmux config again and source it
//!   ([`Effect::TmuxConf`]). Everything else reads the config when it needs it, so it follows
//!   from the next input: the next `open`, the next wake-up, the deadlines armed at the end of
//!   this one. What runs already keeps what it was started with: a session's agent, and moa's
//!   own, whose account and program apply from its next start (a reload never restarts it).
//! - **`layouts.json`** ([`layouts`]): read again and published; a broken one keeps the last
//!   good layouts, with an error line ([`layouts_kept`]), as every other read of it does
//!   (`ops::layout::refresh`).
//! - **The brain's scope file and moa's constitution** ([`instructions`]): moa's agent is told
//!   them when a thread opens, so they apply from its next thread, never mid-thread
//!   ([`INSTRUCTIONS_CHANGED`]).

use crate::Now;
use crate::config::{Config, skipped_warning};
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, Publish};
use crate::ops;
use crate::publish::{CoreEvent, Topic};
use crate::state::State;
use crate::tmux::link::log;

/// What the console says when the scope file or the constitution was saved.
pub const INSTRUCTIONS_CHANGED: &str =
    "moa's instructions changed; they apply from its next thread (/new starts one)";

/// What the console says of a `moa.toml` moa refused: `error`, without the file's name it
/// starts with (`moa.toml: …`, `moa.toml line 3: …`), since the line names the file.
pub fn kept_the_last(error: &str) -> String {
    let why = error
        .strip_prefix("moa.toml")
        .map_or(error, |rest| rest.trim_start_matches(':').trim_start());
    format!("moa.toml has an error, so moa kept the last one: {why}")
}

/// What the console says when `moa.toml` was removed while moa runs.
pub const MOA_TOML_GONE: &str = "moa.toml is gone, so moa kept the last one";

/// Why the shell has no config to hand over.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unread {
    /// It can't be read, or doesn't parse: the reader's line.
    Error(String),
    /// There is no `moa.toml` any more.
    Gone,
}

/// What the console says of a `layouts.json` that doesn't read as layouts.
pub fn layouts_kept(error: &str) -> String {
    format!("layouts.json has an error, so moa kept the last one: {error}")
}

/// `Input::Config`: `moa.toml` as the shell read it.
pub(crate) fn config(state: &mut State, read: Result<Config, Unread>) -> Vec<Effect> {
    let brain = state.brain();
    let config = read.and_then(|c| match c.brain(brain) {
        Some(_) => Ok(c),
        None => Err(Unread::Error(format!("it has no brain named {brain}"))),
    });
    let config = match config {
        Ok(config) => config,
        Err(why) => {
            let message = match why {
                Unread::Error(why) => kept_the_last(&why),
                Unread::Gone => MOA_TOML_GONE.to_string(),
            };
            return vec![
                // The error may name folders: the log keeps to the fact.
                log(
                    LogLevel::Warn,
                    "moa.toml can't be had; moa kept the last one".into(),
                ),
                Effect::Publish(Publish::Event(CoreEvent::Error { message })),
            ];
        }
    };
    if &config == state.config() {
        return Vec::new();
    }
    let ui_changed = config.ui != state.config().ui;
    let mut fx = vec![log(LogLevel::Info, "moa.toml was read again".into())];
    fx.extend(skipped_warning(&config).map(|message| log(LogLevel::Warn, message)));
    state.set_config(config);
    fx.extend(
        state
            .publish_all_changed()
            .map(|m| Effect::Publish(Publish::Model(m))),
    );
    if ui_changed {
        fx.push(Effect::TmuxConf);
    }
    fx
}

/// `Input::LayoutsChanged`: `layouts.json` read again and published; one that doesn't read
/// keeps the last good layouts, with its error.
pub(crate) fn layouts(state: &mut State) -> Vec<Effect> {
    ops::layout::refresh(state);
    let mut fx = Vec::new();
    if let Some(why) = &state.layouts.file_error {
        fx.push(log(
            LogLevel::Warn,
            "layouts.json has an error; moa kept the last one".into(),
        ));
        fx.push(Effect::Publish(Publish::Event(CoreEvent::Error {
            message: layouts_kept(why),
        })));
    }
    // The last good layouts, and the file's error while it has one.
    fx.extend(
        state
            .publish_changed(Topic::Layouts)
            .map(|m| Effect::Publish(Publish::Model(m))),
    );
    fx
}

/// `Input::InstructionsChanged`: the line that says when they apply, saved as your `ui`
/// lines are. Nothing reaches moa's agent: its thread under way keeps what it was told.
pub(crate) fn instructions(state: &mut State, now: &Now) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    match tx.ui_each(state, Vec::new(), vec![INSTRUCTIONS_CHANGED.to_string()]) {
        Ok(()) => post_commit(state, tx),
        Err(_) => vec![log(
            LogLevel::Warn,
            "moa's instructions changed, and the line that says so couldn't be saved".into(),
        )],
    }
}
