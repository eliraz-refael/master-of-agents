//! The control-mode byte parser: bytes from tmux in, `Frame`s out. No IO.

use moa_core::tmux::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::notify::Notification;

/// The longest line we hold while waiting for its newline.
pub const MAX_LINE: usize = 8 << 20;
/// The most bytes we accept in one reply block.
pub const MAX_BLOCK: usize = 64 << 20;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Frame {
    /// A finished command block. `lines` are decoded as lossy UTF-8.
    Reply {
        flags: u32,
        error: bool,
        lines: Vec<String>,
    },
    Notification(Notification),
    /// A line outside any block that doesn't start with `%`.
    Stray,
}

/// After an error the parser is not usable: the client drops the connection.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("a line from tmux is too long")]
    LineTooLong,
    #[error("a reply from tmux is too long")]
    BlockTooLong,
}

/// Every notification name tmux sends (R2 section 1.2), without the leading `%` and
/// without the block markers. Also the names accepted as notifications inside a block.
const KNOWN: &[&str] = &[
    "output",
    "extended-output",
    "pause",
    "continue",
    "layout-change",
    "window-add",
    "window-close",
    "window-renamed",
    "unlinked-window-add",
    "unlinked-window-close",
    "unlinked-window-renamed",
    "window-pane-changed",
    "session-window-changed",
    "session-changed",
    "client-session-changed",
    "client-detached",
    "sessions-changed",
    "session-renamed",
    "pane-mode-changed",
    "paste-buffer-changed",
    "paste-buffer-deleted",
    "subscription-changed",
    "message",
    "config-error",
    "exit",
];

fn known(name: &str) -> Option<&'static str> {
    KNOWN.iter().copied().find(|k| *k == name)
}

/// A block's identity: the raw `%begin` time, command number and flags. The closing
/// marker must repeat the same text, so `+5` or `05` never closes block `5`.
#[derive(Clone, PartialEq, Eq)]
struct Guard {
    time: String,
    number: String,
    flags: String,
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `%<word> T N F` with exactly three numeric fields, else `None`.
fn guard(line: &str, word: &str) -> Option<Guard> {
    let mut it = line.split(' ');
    if it.next()? != word {
        return None;
    }
    let (time, number, flags) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || !digits(time) || !digits(number) || !digits(flags) {
        return None;
    }
    time.parse::<u64>().ok()?;
    number.parse::<u64>().ok()?;
    flags.parse::<u32>().ok()?;
    Some(Guard {
        time: time.to_string(),
        number: number.to_string(),
        flags: flags.to_string(),
    })
}

struct Block {
    guard: Guard,
    flags: u32,
    lines: Vec<String>,
    bytes: usize,
}

/// Turns tmux's control-mode bytes into frames.
///
/// Inside a reply block, only the exact closing marker (same time, number and flags as
/// the `%begin`) ends it. With `set_notifications_in_blocks(true)`, the default, a line
/// whose first word is a known notification name is also read as a notification, because
/// before tmux 3.8 a notification can land inside a block. From 3.8 tmux holds them back
/// until after the block, so the caller switches this off once it sees a 3.8 or newer
/// version (the version is not known before the first reply).
///
/// Caller invariant: while the rule is on, a reply line that begins with `%` and a known
/// name is read as a notification, so moa must not request output whose lines can start
/// with `%` from a server older than 3.8 (multi-line `display-message -p` values are the
/// known case).
pub struct Parser {
    buf: Vec<u8>,
    /// How many bytes at the front of `buf` are known to hold no newline.
    scanned: usize,
    block: Option<Block>,
    notifications_in_blocks: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            buf: Vec::new(),
            scanned: 0,
            block: None,
            notifications_in_blocks: true,
        }
    }

    /// Turns the in-block notification rule on or off (see the type docs).
    pub fn set_notifications_in_blocks(&mut self, on: bool) {
        self.notifications_in_blocks = on;
    }

    /// Feeds bytes and returns the frames they complete. Chunk boundaries don't matter.
    /// On an error, frames already parsed in this call are dropped, and the parser must
    /// not be used again: the client drops the connection.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, ParseError> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut start = 0;
        loop {
            let from = start.max(self.scanned);
            let Some(off) = self.buf[from..].iter().position(|b| *b == b'\n') else {
                break;
            };
            let end = from + off;
            if end - start > MAX_LINE {
                return Err(ParseError::LineTooLong);
            }
            let line = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
            let size = end - start + 1;
            start = end + 1;
            self.line(line, size, &mut frames)?;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        if self.buf.len() > MAX_LINE {
            return Err(ParseError::LineTooLong);
        }
        Ok(frames)
    }

    fn line(
        &mut self,
        line: String,
        size: usize,
        frames: &mut Vec<Frame>,
    ) -> Result<(), ParseError> {
        let Some(open) = &mut self.block else {
            if let Some(g) = guard(&line, "%begin") {
                self.block = Some(Block {
                    flags: g.flags.parse().unwrap_or(0),
                    guard: g,
                    lines: Vec::new(),
                    bytes: 0,
                });
            } else if line.starts_with('%') {
                frames.push(Frame::Notification(notification(&line)));
            } else {
                frames.push(Frame::Stray);
            }
            return Ok(());
        };
        for (word, error) in [("%end", false), ("%error", true)] {
            if guard(&line, word).as_ref() == Some(&open.guard) {
                let done = self.block.take().expect("block is open");
                frames.push(Frame::Reply {
                    flags: done.flags,
                    error,
                    lines: done.lines,
                });
                return Ok(());
            }
        }
        if self.notifications_in_blocks
            && let Some(name) = line.strip_prefix('%')
            && known(name.split(' ').next().unwrap_or("")).is_some()
        {
            frames.push(Frame::Notification(notification(&line)));
            return Ok(());
        }
        open.bytes += size;
        if open.bytes > MAX_BLOCK {
            return Err(ParseError::BlockTooLong);
        }
        open.lines.push(line);
        Ok(())
    }
}

/// Parses a line that starts with `%`. A known name with arguments that don't parse
/// (`%window-add x`) gives `Other(name)`, never a panic; an unknown name gives
/// `Unknown(name)`. The text of `%message` is kept verbatim.
pub fn notification(line: &str) -> Notification {
    let body = line.strip_prefix('%').unwrap_or(line);
    let (name, rest) = body.split_once(' ').unwrap_or((body, ""));
    let Some(name) = known(name) else {
        return Notification::Unknown(name.to_string());
    };
    typed(name, rest).unwrap_or_else(|| {
        // Only the name is logged, never the arguments.
        tracing::debug!(name, "notification arguments did not parse");
        Notification::Other(name)
    })
}

fn typed(name: &'static str, rest: &str) -> Option<Notification> {
    let mut args = rest.split(' ');
    let mut arg = || args.next();
    Some(match name {
        "window-pane-changed" => Notification::WindowPaneChanged {
            window: arg()?.parse::<WindowId>().ok()?,
            pane: arg()?.parse::<PaneId>().ok()?,
        },
        "session-window-changed" => Notification::SessionWindowChanged {
            session: arg()?.parse::<TmuxSessionId>().ok()?,
            window: arg()?.parse::<WindowId>().ok()?,
        },
        "client-session-changed" => Notification::ClientSessionChanged {
            client: client(arg()?)?,
            session: arg()?.parse::<TmuxSessionId>().ok()?,
        },
        "client-detached" => Notification::ClientDetached {
            client: client(arg()?)?,
        },
        "session-changed" => Notification::SessionChanged {
            session: arg()?.parse::<TmuxSessionId>().ok()?,
        },
        "sessions-changed" => Notification::SessionsChanged,
        "window-add" | "unlinked-window-add" => Notification::WindowAdd {
            window: arg()?.parse::<WindowId>().ok()?,
        },
        "window-close" | "unlinked-window-close" => Notification::WindowClose {
            window: arg()?.parse::<WindowId>().ok()?,
        },
        "layout-change" => Notification::LayoutChange {
            window: arg()?.parse::<WindowId>().ok()?,
        },
        "message" => Notification::Message(rest.to_string()),
        "exit" => Notification::Exit,
        _ => return None,
    })
}

fn client(s: &str) -> Option<ClientName> {
    (!s.is_empty()).then(|| ClientName(s.to_string()))
}
