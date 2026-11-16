//! tmux's ids, printed and parsed in tmux's own spelling.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

/// The text was not an id of the kind asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("not a tmux id")]
pub struct BadTmuxId;

macro_rules! prefixed_ids {
    ($($name:ident $prefix:literal),* $(,)?) => {$(
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub u32);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }

        impl FromStr for $name {
            type Err = BadTmuxId;
            fn from_str(s: &str) -> Result<Self, BadTmuxId> {
                let digits = s.strip_prefix($prefix).ok_or(BadTmuxId)?;
                // `u32::from_str` would accept a leading '+'.
                if !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(BadTmuxId);
                }
                digits.parse().map($name).map_err(|_| BadTmuxId)
            }
        }
    )*};
}

prefixed_ids!(PaneId "%", WindowId "@", TmuxSessionId "$");

/// A tmux client's name: a tty path like `/dev/ttys1`, or `client-123` for control clients.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ClientName(pub String);

/// Which tmux server run an id belongs to: the server's pid and start time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct Generation {
    pub pid: i64,
    pub start: i64,
}
