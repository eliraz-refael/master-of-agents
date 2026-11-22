//! The footer's prompts: what each one asks, and what its answer sends. The new-session chain
//! (folder, title, account) is here, so every view's `n` and `N` share it (V7, V21).

use std::path::PathBuf;

use moa_core::folders::typed_folder;
use moa_core::model::Project;
use moa_core::op::Op;
use moa_core::terminals;

use super::{CANCELLED, Out, Panel};
use crate::ctx::Ctx;
use crate::data::Data;
use crate::text::{clean, tilde};
use crate::views::tree;

/// What a prompt's answer is for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Ask {
    /// `n`: the folder, then the title, then the account.
    Folder {
        group: Option<String>,
        account: String,
    },
    Title {
        group: Option<String>,
        path: PathBuf,
        account: String,
    },
    Account {
        group: Option<String>,
        path: PathBuf,
        title: Option<String>,
    },
    NewGroup,
    Move {
        q: String,
    },
    RenameSession {
        q: String,
    },
    RenameGroup {
        old: String,
    },
    /// The terminals view's `r`, `n` and `t`: the terminal's tab id.
    TermName {
        target: String,
    },
    TermNote {
        target: String,
    },
    TermTimeout {
        target: String,
    },
}

/// The account prompt's label: the brain's accounts to choose from.
pub(super) fn account_label(data: &Data) -> String {
    let accounts = data.brain.as_ref().map(|b| b.accounts.join(" / "));
    match accounts {
        Some(list) if !list.is_empty() => format!("account ({list}):"),
        _ => "account:".to_string(),
    }
}

/// The account a new session starts under: `want` when the brain lists it, else the brain's
/// own default, else its first account.
pub(super) fn default_account(data: &Data, want: Option<&str>) -> String {
    let Some(brain) = &data.brain else {
        return want.unwrap_or_default().to_string();
    };
    want.filter(|w| brain.accounts.iter().any(|a| a == w))
        .or(brain.open_account.as_deref())
        .or(brain.accounts.first().map(String::as_str))
        .unwrap_or_default()
        .to_string()
}

impl Panel {
    /// `n` and `N` on a row with a project (V21): a new session in the project's folder,
    /// after V7's folder checks. `n` opens it under the brain's default account; `N` asks the
    /// account first.
    pub(super) fn open_in_project(
        &mut self,
        project: Option<&Project>,
        choose: bool,
        data: &Data,
        ctx: &Ctx,
    ) -> Vec<Out> {
        let Some(project) = project else {
            return self.say("this row has no project");
        };
        let Some(path) = &project.path else {
            return self.say(&format!("no folder for {}", clean(&project.name)));
        };
        let path = match typed_folder(&tilde(path, &ctx.home), &ctx.home, ctx.is_dir) {
            Ok(path) => path,
            Err(e) => return self.say(&e),
        };
        if !choose {
            return vec![Out::Send(Op::SessionOpen {
                path,
                account: None,
                title: None,
                group: None,
                prompt: None,
                bg: false,
            })];
        }
        let account = default_account(data, None);
        let ask = Ask::Account {
            group: None,
            path,
            title: None,
        };
        self.ask(account_label(data), account, ask)
    }

    /// A prompt's answer, trimmed: the next prompt, an op, or why neither.
    pub(super) fn answered(&mut self, ask: Ask, text: String, data: &Data, ctx: &Ctx) -> Vec<Out> {
        let send = |op| vec![Out::Send(op)];
        match ask {
            Ask::Folder { group, account } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                // V7's checks, the same `Ctrl-a n` makes in the core.
                match typed_folder(&text, &ctx.home, ctx.is_dir) {
                    Ok(path) => self.ask(
                        "title:".to_string(),
                        String::new(),
                        Ask::Title {
                            group,
                            path,
                            account,
                        },
                    ),
                    Err(e) => self.say(&e),
                }
            }
            Ask::Title {
                group,
                path,
                account,
            } => {
                let title = (!text.is_empty()).then_some(text);
                let ask = Ask::Account { group, path, title };
                self.ask(account_label(data), account, ask)
            }
            Ask::Account { group, path, title } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                send(Op::SessionOpen {
                    path,
                    account: Some(text),
                    title,
                    group,
                    prompt: None,
                    bg: false,
                })
            }
            Ask::NewGroup => match tree::tidy(&text) {
                Some(path) => send(Op::GroupAdd { path }),
                None => self.say(CANCELLED),
            },
            Ask::Move { q } => match tree::tidy(&text) {
                Some(group) => send(Op::SessionSet {
                    q,
                    title: None,
                    group: Some(group),
                }),
                None => self.say(CANCELLED),
            },
            Ask::RenameSession { q } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                send(Op::SessionSet {
                    q,
                    title: Some(text),
                    group: None,
                })
            }
            Ask::RenameGroup { old } => {
                let (parent, leaf) = old.rsplit_once('/').unwrap_or(("", &old));
                // Tidied as the core will, so `leaf/` is no rename either.
                let Some(text) = tree::tidy(&text).filter(|t| t != leaf) else {
                    return self.say(CANCELLED);
                };
                let new = if parent.is_empty() {
                    text
                } else {
                    format!("{parent}/{text}")
                };
                send(Op::GroupRename { old, new })
            }
            Ask::TermName { target } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                send(Op::TermName { target, name: text })
            }
            // An empty note clears it.
            Ask::TermNote { target } => send(Op::TermNote { target, note: text }),
            // The CLI's parser: a bad answer says why in the footer.
            Ask::TermTimeout { target } => match terminals::parse_after(&text) {
                Ok(after_ms) => send(Op::TermTimeout { target, after_ms }),
                Err(e) => self.say(&e),
            },
        }
    }
}
