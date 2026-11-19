//! A view's model: styled lines, each carrying the item it stands for (V2), so another client
//! can lay the same items out its own way.

use std::fmt::Write as _;

use moa_core::UtcMs;
use moa_core::model::{ProjectId, SessionId, SuggestionId, TaskId};

/// What a span means; `moa-tui` maps each to a colour (V12).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tone {
    /// The terminal's default colour, as the frame is drawn (V12): the picture's rule.
    Frame,
    Text,
    Dim,
    Head,
    Group,
    Warn,
    Ask,
    Ok,
    Fail,
    Work,
    Quiet,
    Brain,
    You,
    P0,
    Late,
    Today,
    Soon,
}

impl Tone {
    /// Its name in a model snapshot.
    pub fn word(self) -> &'static str {
        match self {
            Tone::Frame => "frame",
            Tone::Text => "text",
            Tone::Dim => "dim",
            Tone::Head => "head",
            Tone::Group => "group",
            Tone::Warn => "warn",
            Tone::Ask => "ask",
            Tone::Ok => "ok",
            Tone::Fail => "fail",
            Tone::Work => "work",
            Tone::Quiet => "quiet",
            Tone::Brain => "brain",
            Tone::You => "you",
            Tone::P0 => "p0",
            Tone::Late => "late",
            Tone::Today => "today",
            Tone::Soon => "soon",
        }
    }
}

/// A run of text in one tone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Span {
    pub text: String,
    pub tone: Tone,
    pub bold: bool,
    /// Drawn with its colours swapped: the strip's flash.
    pub reversed: bool,
}

impl Span {
    pub fn new(text: impl Into<String>, tone: Tone) -> Span {
        Span::styled(text, tone, false)
    }

    /// The span in bold.
    pub fn bold(text: impl Into<String>, tone: Tone) -> Span {
        Span::styled(text, tone, true)
    }

    /// The span, in bold when `bold`.
    pub fn styled(text: impl Into<String>, tone: Tone, bold: bool) -> Span {
        Span {
            text: text.into(),
            tone,
            bold,
            reversed: false,
        }
    }
}

/// What a selectable line stands for. Keys act on it, never on a row index (V8).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Item {
    Session(SessionId),
    /// A tree node, by its path: a real group, a project's node or `Loose`.
    Group(String),
    Task(TaskId),
    Project(ProjectId),
    Suggestion(SuggestionId),
}

impl Item {
    /// Its kind's name in a model snapshot.
    pub fn kind(&self) -> &'static str {
        match self {
            Item::Session(_) => "session",
            Item::Group(_) => "group",
            Item::Task(_) => "task",
            Item::Project(_) => "project",
            Item::Suggestion(_) => "suggestion",
        }
    }
}

/// One line of a view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    pub spans: Vec<Span>,
    /// The item this line selects; `None` for a heading, a blank, a hint or a wrapped line.
    pub item: Option<Item>,
    /// A wrapped line of the item above: it scrolls and clicks with it.
    pub more: bool,
    /// Drawn against the right edge, when it fits.
    pub chip: Option<Span>,
    /// It holds a spinner, a blink or a flash: the panel ticks while it is visible (V10).
    pub animated: bool,
    /// The earliest moment its text changes (an age's next step, a flash's end), if any.
    pub changes_at: Option<UtcMs>,
}

impl Line {
    /// A line of these spans, standing for nothing.
    pub fn new(spans: Vec<Span>) -> Line {
        Line {
            spans,
            item: None,
            more: false,
            chip: None,
            animated: false,
            changes_at: None,
        }
    }

    /// A line of one span.
    pub fn plain(text: impl Into<String>, tone: Tone) -> Line {
        Line::new(vec![Span::new(text, tone)])
    }

    /// A wrapped line of the item above.
    pub fn wrapped(spans: Vec<Span>) -> Line {
        Line {
            more: true,
            ..Line::new(spans)
        }
    }

    /// The line, standing for `item`.
    pub fn with_item(mut self, item: Item) -> Line {
        self.item = Some(item);
        self
    }

    /// The line, with `chip` against the right edge.
    pub fn with_chip(mut self, chip: Span) -> Line {
        self.chip = Some(chip);
        self
    }

    /// Whether a selection can rest on it: it stands for an item and isn't a wrapped line.
    pub fn selectable(&self) -> bool {
        self.item.is_some() && !self.more
    }

    /// Its text, spans joined.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

fn push_span(out: &mut String, span: &Span) {
    let bold = if span.bold { "*" } else { "" };
    let reversed = if span.reversed { "!" } else { "" };
    let _ = write!(out, "[{}{bold}{reversed}]{}", span.tone.word(), span.text);
}

/// The model snapshot form (V19): one line per `Line`, each span as `[tone]text` (`*` after
/// the tone for bold, then `!` for reversed), the chip in `⟨⟩`, then the item's kind in `‹›`
/// (`‹more›` for a wrapped line).
pub fn debug_lines(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            push_span(&mut out, span);
        }
        if let Some(chip) = &line.chip {
            out.push_str(" ⟨");
            push_span(&mut out, chip);
            out.push('⟩');
        }
        if let Some(item) = &line.item {
            let _ = write!(out, "  ‹{}›", item.kind());
        } else if line.more {
            out.push_str("  ‹more›");
        }
        out.push('\n');
    }
    out
}
