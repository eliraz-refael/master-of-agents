//! The config files read again while the daemon runs (L8, §6.15.4). The daemon's shell
//! watches them and says which changed; the core reads no file of its own.
//!
//! - **`moa.toml`** ([`config`]): the shell reads it as at start and hands over the config
//!   whole, or why it can't be had. One that can't, or that drops the brain this daemon
//!   runs, is refused whole: moa keeps the last good config and says so ([`kept_the_last`]).
//!   A good one replaces it, the views hear of what it changed, and a `[ui]` change has the
//!   shell write moa's tmux config again and source it ([`Effect::TmuxConf`]). Everything
//!   else reads the config when it needs it, so it follows from the next input: the next
//!   `open`, the next wake-up, the deadlines armed at the end of this one. What runs already
//!   keeps what it was started with: a session's agent, and moa's own until it starts again.
//! - **`layouts.json`** ([`layouts`]): read again and published; a broken one keeps the last
//!   good layouts, with an error line ([`layouts_kept`]).
//! - **The brain's scope file and moa's constitution** ([`instructions`]): moa's agent is told
//!   them when a thread opens, so they apply from its next thread, never mid-thread
//!   ([`INSTRUCTIONS_CHANGED`]).

use crate::Now;
use crate::config::Config;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, LogLevel, LogLine, Publish};
use crate::ops;
use crate::publish::{CoreEvent, Topic};
use crate::state::State;

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

/// What the console says of a `layouts.json` that doesn't read as layouts.
pub fn layouts_kept(error: &str) -> String {
    format!("layouts.json has an error, so moa kept the last one: {error}")
}

/// `Input::Config`: `moa.toml` as the shell read it.
pub(crate) fn config(state: &mut State, read: Result<Config, String>) -> Vec<Effect> {
    let brain = state.brain();
    let config = read.and_then(|c| match c.brain(brain) {
        Some(_) => Ok(c),
        None => Err(format!("it has no brain named {brain}")),
    });
    let config = match config {
        Ok(config) => config,
        Err(why) => {
            return vec![
                // The error may name folders: the log keeps to the fact.
                log(
                    LogLevel::Warn,
                    "moa.toml has an error; moa kept the last one",
                ),
                Effect::Publish(Publish::Event(CoreEvent::Error {
                    message: kept_the_last(&why),
                })),
            ];
        }
    };
    if &config == state.config() {
        return Vec::new();
    }
    let ui_changed = config.ui != state.config().ui;
    state.set_config(config);
    let mut fx = vec![log(LogLevel::Info, "moa.toml was read again")];
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

/// `Input::LayoutsChanged`: `layouts.json` read again and, when it reads, published.
pub(crate) fn layouts(state: &mut State) -> Vec<Effect> {
    let read = ops::layout::read(state);
    if let Some(why) = &read.file_error {
        return vec![
            log(
                LogLevel::Warn,
                "layouts.json has an error; moa kept the last one",
            ),
            Effect::Publish(Publish::Event(CoreEvent::Error {
                message: layouts_kept(why),
            })),
        ];
    }
    state.layouts = read;
    state
        .publish_changed(Topic::Layouts)
        .map(|m| Effect::Publish(Publish::Model(m)))
        .into_iter()
        .collect()
}

/// `Input::InstructionsChanged`: the line that says when they apply, saved as your `ui`
/// lines are. Nothing reaches moa's agent: its thread under way keeps what it was told.
pub(crate) fn instructions(state: &mut State, now: &Now) -> Vec<Effect> {
    let mut tx = Tx::new(now);
    match tx.ui_each(state, Vec::new(), vec![INSTRUCTIONS_CHANGED.to_string()]) {
        Ok(()) => post_commit(state, tx),
        Err(_) => vec![log(
            LogLevel::Warn,
            "moa's instructions changed, and the line that says so couldn't be saved",
        )],
    }
}

fn log(level: LogLevel, message: &str) -> Effect {
    Effect::Log(LogLine {
        level,
        message: message.into(),
    })
}
