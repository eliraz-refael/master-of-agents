//! The empty slot: what the agent spot shows with no agent in it (the spike's `EMPTY_SLOT`),
//! dim and unframed, with no keys of its own.

use crate::line::{Line, Tone};
use crate::text::cut;

/// The words, a line each; the empty one is a blank line.
const WORDS: [&str; 4] = [
    "no agent here yet",
    "",
    "press Enter on a session in the tree",
    "to bring it into this spot",
];

/// The slot's lines, `width` cells wide.
pub fn build(width: u16) -> Vec<Line> {
    WORDS
        .iter()
        .map(|words| match *words {
            "" => Line::new(Vec::new()),
            words => Line::plain(cut(words, usize::from(width)), Tone::Dim),
        })
        .collect()
}
