//! The `moa:` mailbox: what tmux key bindings, menu items and hooks send the core,
//! as `display-message` text that comes back as a `%message`.

use std::str::FromStr;

use super::ids::{ClientName, PaneId, TmuxSessionId, WindowId};

/// What a moa key or menu item asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyAction {
    Console,
    Tree,
    Plan,
    Agent,
    Find,
    FindOther,
    New,
    Terminal,
    /// A layout, 1 to 4.
    Layout(u8),
    Repair,
    Help,
}

impl KeyAction {
    pub const ALL: [KeyAction; 14] = [
        KeyAction::Console,
        KeyAction::Tree,
        KeyAction::Plan,
        KeyAction::Agent,
        KeyAction::Find,
        KeyAction::FindOther,
        KeyAction::New,
        KeyAction::Terminal,
        KeyAction::Layout(1),
        KeyAction::Layout(2),
        KeyAction::Layout(3),
        KeyAction::Layout(4),
        KeyAction::Repair,
        KeyAction::Help,
    ];

    /// The word used in mailbox lines.
    pub fn word(self) -> String {
        match self {
            KeyAction::Console => "console".into(),
            KeyAction::Tree => "tree".into(),
            KeyAction::Plan => "plan".into(),
            KeyAction::Agent => "agent".into(),
            KeyAction::Find => "find".into(),
            KeyAction::FindOther => "find-other".into(),
            KeyAction::New => "new".into(),
            KeyAction::Terminal => "terminal".into(),
            KeyAction::Layout(n) => format!("layout-{n}"),
            KeyAction::Repair => "repair".into(),
            KeyAction::Help => "help".into(),
        }
    }

    pub fn from_word(w: &str) -> Option<KeyAction> {
        KeyAction::ALL.into_iter().find(|a| a.word() == w)
    }

    /// The key that triggers it, after moa's prefix.
    pub fn key(self) -> &'static str {
        match self {
            KeyAction::Console => "c",
            KeyAction::Tree => "t",
            KeyAction::Plan => "p",
            KeyAction::Agent => "g",
            KeyAction::Find => "/",
            KeyAction::FindOther => "s",
            KeyAction::New => "n",
            KeyAction::Terminal => "$",
            KeyAction::Layout(1) => "1",
            KeyAction::Layout(2) => "2",
            KeyAction::Layout(3) => "3",
            KeyAction::Layout(_) => "4",
            KeyAction::Repair => "r",
            KeyAction::Help => "?",
        }
    }

    /// The menu text.
    pub fn label(self) -> &'static str {
        match self {
            KeyAction::Console => "Console",
            KeyAction::Tree => "Sessions tree",
            KeyAction::Plan => "Big picture",
            KeyAction::Agent => "Agent spot",
            KeyAction::Find => "Find a session…",
            KeyAction::FindOther => "Find a session, full tab…",
            KeyAction::New => "New session…",
            KeyAction::Terminal => "Terminal here",
            KeyAction::Layout(1) => "Layout: cockpit",
            KeyAction::Layout(2) => "Layout: focus (agent)",
            KeyAction::Layout(3) => "Layout: classic",
            KeyAction::Layout(_) => "Layout: triage (status)",
            KeyAction::Repair => "Repair panels",
            KeyAction::Help => "Keys and help",
        }
    }

    /// The notice shown until the action's slice lands.
    pub fn not_yet(self) -> &'static str {
        match self {
            KeyAction::Console => "the console isn't in this build yet",
            KeyAction::Tree => "the sessions tree isn't in this build yet",
            KeyAction::Plan => "the big picture isn't in this build yet",
            KeyAction::Agent => "the agent spot isn't in this build yet",
            KeyAction::Find | KeyAction::FindOther => "finding a session isn't in this build yet",
            KeyAction::New => "new sessions aren't in this build yet",
            // Never shown since slice 5a: the core opens the terminal (`term_key`).
            KeyAction::Terminal => "terminals aren't in this build yet",
            KeyAction::Layout(_) => "layouts aren't in this build yet",
            KeyAction::Repair => "repair isn't in this build yet",
            KeyAction::Help => "help isn't in this build yet",
        }
    }
}

/// What a tmux hook reports.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HookMsg {
    PaneExited(PaneId),
    PaneDied(PaneId),
    Layout(WindowId),
    ClientAttached(ClientName),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mailbox {
    Key {
        action: KeyAction,
        /// `None` from a menu, which has no pane.
        pane: Option<PaneId>,
        session: TmuxSessionId,
        client: ClientName,
    },
    Hook(HookMsg),
    Prompt {
        token: u64,
        text: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MailboxError {
    /// Not a `moa:` line.
    NotMoa,
    /// A `moa:` line that doesn't parse.
    Bad,
}

fn id<T: FromStr>(s: &str) -> Result<T, MailboxError> {
    s.parse().map_err(|_| MailboxError::Bad)
}

fn client(s: &str) -> Result<ClientName, MailboxError> {
    if s.is_empty() {
        return Err(MailboxError::Bad);
    }
    Ok(ClientName(s.to_string()))
}

impl Mailbox {
    pub fn parse(message: &str) -> Result<Mailbox, MailboxError> {
        let rest = message.strip_prefix("moa:").ok_or(MailboxError::NotMoa)?;
        let (verb, rest) = rest.split_once(' ').ok_or(MailboxError::Bad)?;
        match verb {
            "key" => {
                let mut f = rest.splitn(4, ' ');
                let (Some(word), Some(pane), Some(session), Some(c)) =
                    (f.next(), f.next(), f.next(), f.next())
                else {
                    return Err(MailboxError::Bad);
                };
                Ok(Mailbox::Key {
                    action: KeyAction::from_word(word).ok_or(MailboxError::Bad)?,
                    pane: if pane == "-" { None } else { Some(id(pane)?) },
                    session: id(session)?,
                    client: client(c)?,
                })
            }
            "hook" => {
                let (name, arg) = rest.split_once(' ').ok_or(MailboxError::Bad)?;
                Ok(Mailbox::Hook(match name {
                    "pane-exited" => HookMsg::PaneExited(id(arg)?),
                    "pane-died" => HookMsg::PaneDied(id(arg)?),
                    "layout" => HookMsg::Layout(id(arg)?),
                    "client-attached" => HookMsg::ClientAttached(client(arg)?),
                    _ => return Err(MailboxError::Bad),
                }))
            }
            "prompt" => {
                // tmux may trim the space after the token when the text is empty.
                let (token, text) = rest.split_once(' ').unwrap_or((rest, ""));
                // `u64::from_str` would accept a leading '+'.
                if !token.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(MailboxError::Bad);
                }
                Ok(Mailbox::Prompt {
                    token: id(token)?,
                    // tmux 3.4 stores a `$` before a letter as `\$`.
                    text: text.replace(r"\$", "$"),
                })
            }
            _ => Err(MailboxError::Bad),
        }
    }
}
