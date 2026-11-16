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

/// A block's identity: the `%begin` time, command number and flags.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guard {
    time: u64,
    number: u64,
    flags: u32,
}

/// `%<word> T N F` with exactly three numeric fields, else `None`.
fn guard(line: &str, word: &str) -> Option<Guard> {
    let mut it = line.split(' ');
    if it.next()? != word {
        return None;
    }
    let time = it.next()?.parse().ok()?;
    let number = it.next()?.parse().ok()?;
    let flags = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Guard {
        time,
        number,
        flags,
    })
}

struct Block {
    guard: Guard,
    lines: Vec<String>,
    bytes: usize,
}

pub struct Parser {
    buf: Vec<u8>,
    block: Option<Block>,
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
            block: None,
        }
    }

    /// Feeds bytes and returns the frames they complete. Chunk boundaries don't matter.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, ParseError> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut start = 0;
        while let Some(off) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let end = start + off;
            if off > MAX_LINE {
                return Err(ParseError::LineTooLong);
            }
            let line = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
            start = end + 1;
            Self::line(&mut self.block, line, off + 1, &mut frames)?;
        }
        self.buf.drain(..start);
        if self.buf.len() > MAX_LINE {
            return Err(ParseError::LineTooLong);
        }
        Ok(frames)
    }

    fn line(
        block: &mut Option<Block>,
        line: String,
        size: usize,
        frames: &mut Vec<Frame>,
    ) -> Result<(), ParseError> {
        let Some(open) = block else {
            if let Some(g) = guard(&line, "%begin") {
                *block = Some(Block {
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
            if guard(&line, word) == Some(open.guard) {
                let done = block.take().expect("block is open");
                frames.push(Frame::Reply {
                    flags: done.guard.flags,
                    error,
                    lines: done.lines,
                });
                return Ok(());
            }
        }
        // Before tmux 3.8 a notification can land inside a block.
        if let Some(name) = line.strip_prefix('%')
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
