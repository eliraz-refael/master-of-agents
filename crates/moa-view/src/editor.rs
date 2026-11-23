//! A one-line editor: the footer's prompts (a label and the text typed after it) and the
//! console's input line (B7). Its cursor is a grapheme index; the footer's prompts never move
//! it, so they type, erase and show at the end.

use unicode_segmentation::UnicodeSegmentation;

use crate::keys::Key;
use crate::text::{self, clean, width};

/// A line being typed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Editor {
    pub label: String,
    pub text: String,
    /// The cursor: how many graphemes of `text` are before it.
    pub at: usize,
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
    /// The editor with `prefill` typed, the cursor at its end.
    pub fn new(label: impl Into<String>, prefill: impl Into<String>) -> Editor {
        let mut editor = Editor {
            label: label.into(),
            text: String::new(),
            at: 0,
        };
        editor.set(prefill.into());
        editor
    }

    /// Replaces the text, the cursor at its end.
    pub fn set(&mut self, text: String) {
        self.at = text.graphemes(true).count();
        self.text = text;
    }

    /// The byte where the cursor stands (its end when `at` is past it).
    /// Whether the cursor is after the last grapheme.
    pub fn at_end(&self) -> bool {
        self.at == self.text.graphemes(true).count()
    }

    fn cursor(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(self.at)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Puts `s` before the cursor, the cursor after it.
    fn insert(&mut self, s: &str) {
        let i = self.cursor();
        self.text.insert_str(i, s);
        self.at = self.text[..i + s.len()].graphemes(true).count();
    }

    /// Keeps the first `keep` bytes of what is before the cursor, and all after it.
    fn erase_before(&mut self, keep: usize) {
        let i = self.cursor();
        self.text.replace_range(keep..i, "");
        self.at = self.text[..keep].graphemes(true).count();
    }

    /// Takes a key: Enter answers with the text trimmed, Esc cancels, Backspace erases the
    /// character before the cursor, Ctrl-u everything before it, Ctrl-w the word before it
    /// (the spike's `ask`); any other printable character is typed at the cursor and every
    /// other key does nothing. Only [`step`](Editor::step) moves the cursor otherwise.
    pub fn key(&mut self, key: Key) -> Edited {
        match key {
            Key::Enter => return Edited::Done(self.text.trim().to_string()),
            Key::Esc => return Edited::Cancelled,
            Key::Backspace => {
                let i = self.cursor();
                let keep = self.text[..i]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.erase_before(keep);
            }
            Key::Ctrl('u') => self.erase_before(0),
            Key::Ctrl('w') => {
                let before = self.text[..self.cursor()].trim_end();
                let keep = before.rfind(' ').map_or(0, |i| i + 1);
                self.erase_before(keep);
            }
            Key::Char(c) if !c.is_control() => self.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
        Edited::Typing
    }

    /// Moves the cursor (B7): Left and Right a grapheme, Home and End to the ends. True when
    /// `key` is one of those, whether or not the cursor could go.
    pub fn step(&mut self, key: Key) -> bool {
        let end = self.text.graphemes(true).count();
        self.at = match key {
            Key::Left => self.at.min(end).saturating_sub(1),
            Key::Right => (self.at + 1).min(end),
            Key::Home => 0,
            Key::End => end,
            _ => return false,
        };
        true
    }

    /// Takes pasted text, at the cursor: a line break, any other control character or a
    /// line or paragraph separator becomes a space and a bidi control is dropped. The rest is
    /// kept as pasted, as typed text is; [`Editor::shown`] cleans it for drawing.
    pub fn paste(&mut self, text: &str) {
        self.insert(&text::strip_controls(&text.replace("\r\n", "\n")));
    }

    /// `<label> <text>` in at most `width` cells, cleaned for drawing; when too long, its end
    /// is kept, where the typing is.
    pub fn shown(&self, width: usize) -> String {
        text::tail(&clean(&format!("{} {}", self.label, self.text)), width)
    }

    /// The text as it fits `room` cells with the cursor in sight, cleaned for drawing, and
    /// the cursor's column in it: when the text before the cursor doesn't fit with a cell to
    /// spare, its start goes.
    pub fn view(&self, room: usize) -> (String, usize) {
        let shown: Vec<String> = self.text.graphemes(true).map(clean).collect();
        let at = self.at.min(shown.len());
        let mut before: usize = shown[..at].iter().map(|g| width(g)).sum();
        let mut start = 0;
        while start < at && before + 1 > room {
            before -= width(&shown[start]);
            start += 1;
        }
        let mut out = String::new();
        let mut used = 0;
        for g in &shown[start..] {
            used += width(g);
            if used > room {
                break;
            }
            out.push_str(g);
        }
        (out, before)
    }
}
