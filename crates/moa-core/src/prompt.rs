//! moa's questions in tmux's status line (S5): a `command-prompt`, built only by
//! [`Cmd::prompt`], whose answer comes back through the mailbox as `moa:prompt TOKEN TEXT`.
//! One question at a time, so `Ctrl-a n` (S6) checks the folder before it asks the title.

use std::path::PathBuf;

use crate::Now;
use crate::effect::{Effect, LogLevel};
use crate::folders::typed_folder;
use crate::op::OpError;
use crate::ops::launch::{OpenArgs, open_told};
use crate::ops::term::{Tell, told, with_tilde};
use crate::state::State;
use crate::tmux::cmd::Cmd;
use crate::tmux::ids::ClientName;
use crate::tmux::link::{Link, NOTHING_WAITS, log};
use crate::tmux::version;

/// How many prompts wait at most; a new one drops the oldest. Esc and Ctrl-c in tmux's
/// prompt send nothing, so a prompt put away waits until then.
pub(crate) const WAITING: usize = 16;

const FOLDER: &str = "new session · folder:";
const TITLE: &str = "title:";
/// What an empty folder answer says, as the tree's prompts do.
const CANCELLED: &str = "cancelled";

/// What a prompt's answer is for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Asking {
    /// `Ctrl-a n`'s folder.
    Folder { client: ClientName },
    /// Its title, for the folder the first answer named.
    Title { client: ClientName, folder: PathBuf },
}

/// What was typed, from a prompt answer's text as tmux sent it, on a tmux of `version`: 3.4
/// alone writes a typed `$` as `\$`, so only there is it taken back. The rule is the one
/// `moa.tmux` uses (`#{m:3.4*}` or `next-3.4*`); on later versions a typed `\$` stays.
pub fn typed(version: &str, text: &str) -> String {
    if version::is(version, &["3.4"]) {
        text.replace(r"\$", "$")
    } else {
        text.to_string()
    }
}

/// `Ctrl-a n` (S6): asks `client` for the new session's folder, prefilled with the brain's
/// first folder (`~/x/`, ready for typing), else `~/`. A folder holding `,`, `#` or `%` isn't
/// offered: tmux splits `-I` on `,`, and expands `#{…}` and strftime's `%` in it.
pub(crate) fn new_session(state: &mut State, client: ClientName) -> Vec<Effect> {
    let deps = state.deps();
    let first = state
        .config()
        .brain(state.brain())
        .and_then(|b| b.folders.into_iter().find(|f| !f.as_os_str().is_empty()));
    let prefill = first
        .map(|f| with_tilde(&f.display().to_string(), &deps.home, deps.canonical))
        .map(|f| format!("{}/", f.trim_end_matches('/')))
        .filter(|f| !f.contains([',', '#', '%']))
        .unwrap_or_else(|| "~/".to_string());
    let asking = Asking::Folder {
        client: client.clone(),
    };
    ask(state, &client, FOLDER, &prefill, asking)
}

/// Asks `label` in `client`'s status line, and waits for the answer for `asking`. A client's
/// older prompt stays: tmux ignores a second `command-prompt` while one is open, so the
/// answer that comes may be the older one's. With the link down nobody can be asked (a key
/// comes over it, so it is up).
fn ask(
    state: &mut State,
    client: &ClientName,
    label: &str,
    prefill: &str,
    asking: Asking,
) -> Vec<Effect> {
    let Link::Up { conn, ctl, .. } = &state.tmux().link else {
        return Vec::new();
    };
    let (conn, ctl) = (*conn, ctl.clone());
    state.last_prompt += 1;
    let token = state.last_prompt;
    state.prompts.insert(token, asking);
    // Tokens only grow: the first is the oldest.
    while state.prompts.len() > WAITING {
        state.prompts.pop_first();
    }
    vec![Effect::Tmux {
        conn,
        cmds: vec![Cmd::prompt(client, label, prefill, &ctl, token)],
        token: None,
    }]
}

/// The answer to prompt `token`, as tmux sent it, trimmed (an empty one comes with a
/// trailing space). It asks the next question or opens the session; a refusal is told by
/// notice, and that ends it.
pub(crate) fn answered(state: &mut State, now: &Now, token: u64, text: &str) -> Vec<Effect> {
    let Some(asking) = state.prompts.remove(&token) else {
        return vec![log(LogLevel::Info, NOTHING_WAITS.into())];
    };
    let text = match &state.tmux().link {
        Link::Up { version, .. } => typed(version, text),
        _ => text.to_string(),
    };
    let text = text.trim();
    match asking {
        // V7's checks, as the tree's `n` makes them: an empty answer cancels. Nothing is made:
        // a typo never leaves a folder behind.
        Asking::Folder { client } => {
            if text.is_empty() {
                return told(
                    state,
                    Some(Tell::Notice(client)),
                    Err(OpError(CANCELLED.into())),
                )
                .into_iter()
                .collect();
            }
            let deps = state.deps();
            match typed_folder(text, &deps.home, deps.is_dir) {
                Ok(folder) => {
                    let asking = Asking::Title {
                        client: client.clone(),
                        folder,
                    };
                    ask(state, &client, TITLE, "", asking)
                }
                Err(e) => told(state, Some(Tell::Notice(client)), Err(OpError(e)))
                    .into_iter()
                    .collect(),
            }
        }
        // As `moactl open FOLDER --title TITLE` opens it: the folder canonical, under the
        // brain's own account, at the top level, brought to the screen. An empty answer is
        // no title.
        Asking::Title { client, folder } => {
            let path = (state.deps().canonical)(&folder);
            let args = OpenArgs {
                path: &path,
                account: None,
                title: (!text.is_empty()).then_some(text),
                group: None,
                prompt: None,
            };
            open_told(state, now, &args, Tell::Notice(client), true)
        }
    }
}
