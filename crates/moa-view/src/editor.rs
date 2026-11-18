//! The footer's one-line editor: a prompt's label and the text typed after it.

use unicode_segmentation::UnicodeSegmentation;

use crate::keys::Key;
use crate::text::{self, clean};

/// A prompt being answered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Editor {
    pub label: String,
    pub text: String,
}

/// Where an answer stands after a key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edited {
    Typing,
    /// Enter: the text, trimmed.
    Done(String),
    /// Esc.
    Cancelled,
}

impl Editor {
    pub fn new(label: impl Into<String>, prefill: impl Into<String>) -> Editor {
        Editor {
            label: label.into(),
            text: prefill.into(),
        }
    }

    /// Takes a key: Enter answers with the text trimmed, Esc cancels, Backspace erases a
    /// character, Ctrl-u clears, Ctrl-w drops the last word (the spike's `ask`); any other
    /// printable character is typed and every other key does nothing.
    pub fn key(&mut self, key: Key) -> Edited {
        match key {
            Key::Enter => return Edited::Done(self.text.trim().to_string()),
            Key::Esc => return Edited::Cancelled,
            Key::Backspace => {
                let keep = self
                    .text
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.text.truncate(keep);
            }
            Key::Ctrl('u') => self.text.clear(),
            Key::Ctrl('w') => {
                let text = self.text.trim_end();
                let keep = text.rfind(' ').map_or(0, |i| i + 1);
                self.text.truncate(keep);
            }
            Key::Char(c) if !c.is_control() => self.text.push(c),
            _ => {}
        }
        Edited::Typing
    }

    /// Takes pasted text: a line break, any other control character or a line or paragraph
    /// separator becomes a space and a bidi control is dropped. The rest is kept as pasted,
    /// as typed text is; [`Editor::shown`] cleans it for drawing.
    pub fn paste(&mut self, text: &str) {
        self.text
            .push_str(&text::strip_controls(&text.replace("\r\n", "\n")));
    }

    /// `<label> <text>` in at most `width` cells, cleaned for drawing; when too long, its end
    /// is kept, where the typing is.
    pub fn shown(&self, width: usize) -> String {
        let line = clean(&format!("{} {}", self.label, self.text));
        let mut used = 0;
        let mut start = line.len();
        for (i, g) in line.grapheme_indices(true).rev() {
            used += text::width(g);
            if used > width {
                break;
            }
            start = i;
        }
        line[start..].to_string()
    }
}
