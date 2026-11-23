//! Which thread moa's prompt goes to (A8): one a local day per account. Today's thread is
//! reopened when moa's agent starts again, and a new day or account opens a new one, told
//! the system text. Nothing happens at midnight: the day is looked at on the next prompt.

use jiff::civil::Date;

use super::prompt::system_text;
use super::wire::{Caps, ThreadHow};
use crate::Now;
use crate::config::AccountName;
use crate::effect::Effect;
use crate::state::State;

/// Where a prompt goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum How {
    /// The thread moa's agent has open.
    Use(String),
    /// Today's earlier thread, reopened without its history.
    Resume(String),
    /// Today's earlier thread, its history replayed.
    Load(String),
    /// A new thread.
    New,
}

/// The open thread when it is `account`'s and `today`'s; else `today`'s newest thread of
/// `account`, reopened as `caps` allow; else a new one.
pub fn choose(state: &State, account: &AccountName, today: Date, caps: Caps) -> How {
    let ours = |account_of: &str, day: Date| account_of == account.as_str() && day == today;
    if let Some(open) = &state.central().thread
        && ours(&open.row.account, open.row.day)
    {
        return How::Use(open.row.thread_id.clone());
    }
    let earlier = state
        .threads()
        .filter(|t| ours(&t.account, t.day))
        .max_by(|a, b| (a.started_at, &a.thread_id).cmp(&(b.started_at, &b.thread_id)));
    match earlier {
        Some(t) if caps.resume => How::Resume(t.thread_id.clone()),
        Some(t) if caps.load => How::Load(t.thread_id.clone()),
        _ => How::New,
    }
}

/// How a new thread is opened (A9): told the system text as its system prompt when the
/// agent takes one. Otherwise the constitution and the scope go into the agent's folder's
/// `CLAUDE.md`, and the brief is returned to ride on the thread's first prompt. What
/// couldn't be read for the text is logged.
pub(crate) fn new_thread(
    state: &State,
    now: &Now,
    caps: Caps,
) -> (ThreadHow, Option<String>, Vec<Effect>) {
    let text = system_text(state, now);
    let logs = text.warnings.into_iter().map(Effect::Log).collect();
    if caps.system_prompt {
        let how = ThreadHow::New {
            system_prompt: Some(text.full),
            claude_md: None,
        };
        (how, None, logs)
    } else {
        let how = ThreadHow::New {
            system_prompt: None,
            claude_md: Some(text.claude_md),
        };
        (how, Some(text.brief), logs)
    }
}
