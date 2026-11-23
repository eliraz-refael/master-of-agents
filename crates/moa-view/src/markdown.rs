//! moa's answers as lines (B12): CommonMark with tables, task lists and strikethrough, in the
//! console's tones. Inline code that names something to press becomes a button (B13), which
//! wraps as one unit.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::line::{Button, Line, Span, Tone};
use crate::text::{self, clean, width};

/// What a quote draws in front of each line.
const QUOTE: &str = "│ ";

/// A list's bullets, by its depth.
const BULLETS: [&str; 3] = ["•", "◦", "▪"];

/// `text` (Markdown) as lines at most `width` cells wide. `find` turns an inline code's text
/// into a button, or `None` for code that stays code; each button is pushed on `buttons` and
/// its span holds its index there.
pub fn render(
    text: &str,
    width: u16,
    buttons: &mut Vec<Button>,
    find: &dyn Fn(&str) -> Option<Button>,
) -> Vec<Line> {
    if width == 0 {
        return Vec::new();
    }
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut out = Render {
        width: usize::from(width),
        buttons,
        find,
        lines: Vec::new(),
        holders: Vec::new(),
        lists: Vec::new(),
        inline: Vec::new(),
        tones: Vec::new(),
        bold: 0,
        image: 0,
        code: None,
        table: None,
        gap: false,
    };
    for event in Parser::new_ext(text, options) {
        out.event(event);
    }
    out.flush(&[]);
    out.lines
}

/// What holds the lines inside it, drawing in front of each.
enum Holder {
    /// [`QUOTE`] on every line.
    Quote,
    /// A list item: its marker on its first line, as many spaces on the rest.
    Item { marker: Span, used: bool },
}

/// A table's cells as they come.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
    /// Rows from the table's head, at the top of `rows`.
    head: usize,
}

struct Render<'a> {
    width: usize,
    buttons: &'a mut Vec<Button>,
    find: &'a dyn Fn(&str) -> Option<Button>,
    lines: Vec<Line>,
    /// The quotes and list items open, outermost first.
    holders: Vec<Holder>,
    /// The lists open, innermost last, with an ordered one's next number.
    lists: Vec<Option<u64>>,
    /// The open block's text, in runs split at hard breaks.
    inline: Vec<Vec<Span>>,
    /// The tone text takes: Dim in a quote, Head in a top heading.
    tones: Vec<Tone>,
    /// Strong and heading levels open: text inside any is bold.
    bold: usize,
    /// Images open: their alt text isn't drawn.
    image: usize,
    /// A code block's text, while one is open.
    code: Option<String>,
    table: Option<Table>,
    /// A block ended: the next one starts after a blank line.
    gap: bool,
}

impl Render<'_> {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) if self.code.is_some() => {
                if let Some(code) = &mut self.code {
                    code.push_str(&t);
                }
            }
            Event::Text(t) | Event::InlineHtml(t) | Event::InlineMath(t) => self.text(&t),
            Event::Html(t) | Event::DisplayMath(t) => {
                // A block's own line breaks stay.
                for (i, row) in t.trim_end_matches('\n').split('\n').enumerate() {
                    if i > 0 {
                        self.inline.push(Vec::new());
                    }
                    self.text(row);
                }
                self.inline.push(Vec::new());
            }
            Event::Code(t) => self.code_span(&t),
            Event::FootnoteReference(t) => self.text(&format!("[^{t}]")),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.inline.push(Vec::new()),
            Event::Rule => {
                self.block();
                let rule = "─".repeat(self.room(0));
                self.line(Vec::new(), vec![Span::new(rule, Tone::Dim)], false);
                self.gap = true;
            }
            Event::TaskListMarker(done) => {
                if let Some(Holder::Item {
                    marker,
                    used: false,
                }) = self.holders.last_mut()
                {
                    marker.text = if done { "☑ " } else { "☐ " }.to_string();
                }
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            // Footnotes, definition lists and metadata aren't enabled: plain blocks if ever.
            Tag::Paragraph
            | Tag::HtmlBlock
            | Tag::MetadataBlock(_)
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => self.block(),
            Tag::Heading { level, .. } => {
                self.block();
                let tone = if level == HeadingLevel::H1 {
                    Tone::Head
                } else {
                    self.tone()
                };
                self.tones.push(tone);
                self.bold += 1;
            }
            Tag::BlockQuote(_) => {
                self.block();
                self.holders.push(Holder::Quote);
                self.tones.push(Tone::Dim);
            }
            Tag::CodeBlock(_) => {
                self.block();
                self.code = Some(String::new());
            }
            Tag::List(first) => {
                self.block();
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush(&[]);
                self.gap = false;
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n = n.saturating_add(1);
                        marker
                    }
                    _ => format!("{} ", BULLETS[depth % BULLETS.len()]),
                };
                self.holders.push(Holder::Item {
                    marker: Span::new(marker, Tone::Dim),
                    used: false,
                });
            }
            Tag::Table(_) => {
                self.block();
                self.table = Some(Table::default());
            }
            Tag::TableHead => self.bold += 1,
            Tag::TableRow | Tag::TableCell => {}
            Tag::Strong => self.bold += 1,
            Tag::Emphasis
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. } => {}
            Tag::Image { .. } => {
                if self.image == 0 {
                    self.push(Span::new("[image]", Tone::Dim));
                }
                self.image += 1;
            }
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph
            | TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_)
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {
                self.flush(&[]);
                self.gap = true;
            }
            TagEnd::Heading(_) => {
                let lead = Span::styled("▌ ", self.tone(), true);
                self.flush(&[lead]);
                self.tones.pop();
                self.bold = self.bold.saturating_sub(1);
                self.gap = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush(&[]);
                self.holders.pop();
                self.tones.pop();
                self.gap = true;
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let room = self.room(0);
                let rows = code.strip_suffix('\n').unwrap_or(&code);
                for row in rows.split('\n') {
                    let row = text::cut(&clean(&row.replace('\t', "    ")), room);
                    self.line(Vec::new(), vec![Span::new(row, Tone::Dim)], false);
                }
                self.gap = true;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.gap = true;
            }
            TagEnd::Item => {
                self.flush(&[]);
                // An empty item still shows its marker.
                if let Some(Holder::Item { used: false, .. }) = self.holders.last() {
                    self.line(Vec::new(), Vec::new(), false);
                }
                self.holders.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline).concat();
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if tag == TagEnd::TableHead {
                    self.bold = self.bold.saturating_sub(1);
                }
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                    if tag == TagEnd::TableHead {
                        table.head = table.rows.len();
                    }
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.draw_table(table);
                }
                self.gap = true;
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link => {}
            TagEnd::Image => self.image = self.image.saturating_sub(1),
        }
    }

    fn tone(&self) -> Tone {
        self.tones.last().copied().unwrap_or(Tone::Text)
    }

    /// A span in the open block, in this run.
    fn push(&mut self, span: Span) {
        if self.inline.is_empty() {
            self.inline.push(Vec::new());
        }
        if let Some(run) = self.inline.last_mut() {
            run.push(span);
        }
    }

    /// Text from the answer: cleaned, in the open style, with `✓ ✗ ⚠ ●` in their own tones.
    fn text(&mut self, t: &str) {
        if self.image > 0 {
            return;
        }
        let (tone, bold) = (self.tone(), self.bold > 0);
        let mut plain = String::new();
        for c in clean(t).chars() {
            let glyph = match c {
                '✓' => Tone::Ok,
                '✗' => Tone::Fail,
                '⚠' => Tone::Warn,
                '●' => Tone::Work,
                _ => {
                    plain.push(c);
                    continue;
                }
            };
            if !plain.is_empty() {
                self.push(Span::styled(std::mem::take(&mut plain), tone, bold));
            }
            self.push(Span::styled(c, glyph, bold));
        }
        if !plain.is_empty() {
            self.push(Span::styled(plain, tone, bold));
        }
    }

    /// Inline code: a button when `find` makes it one, else code in the Work tone.
    fn code_span(&mut self, t: &str) {
        if self.image > 0 {
            return;
        }
        let code = clean(t);
        let bold = self.bold > 0;
        let span = (self.find)(&code)
            .and_then(|button| button_span(self.buttons, button, bold))
            .unwrap_or_else(|| Span::styled(code, Tone::Work, bold));
        self.push(span);
    }

    /// A block starts: what the open item said before it is drawn, and a blank line comes
    /// first when a block ended just before.
    fn block(&mut self) {
        self.flush(&[]);
        if std::mem::take(&mut self.gap) && !self.lines.is_empty() {
            let mut spans = self.prefix(false);
            while let Some(last) = spans.last_mut() {
                last.text.truncate(last.text.trim_end().len());
                if !last.text.is_empty() {
                    break;
                }
                spans.pop();
            }
            self.lines.push(Line::new(spans));
        }
    }

    /// What the open quotes and items draw in front of a line: an unused item's marker when
    /// `mark` (it's used then), else its width in spaces.
    fn prefix(&mut self, mark: bool) -> Vec<Span> {
        let mut spans = Vec::new();
        for holder in &mut self.holders {
            match holder {
                Holder::Quote => spans.push(Span::new(QUOTE, Tone::Dim)),
                Holder::Item { marker, used } => {
                    if mark && !*used {
                        *used = true;
                        spans.push(marker.clone());
                    } else {
                        spans.push(Span::new(" ".repeat(width(&marker.text)), Tone::Text));
                    }
                }
            }
        }
        spans
    }

    /// The cells a line's own text has, after the prefix and a lead `lead` cells wide.
    fn room(&self, lead: usize) -> usize {
        let prefix: usize = self
            .holders
            .iter()
            .map(|holder| match holder {
                Holder::Quote => width(QUOTE),
                Holder::Item { marker, .. } => width(&marker.text),
            })
            .sum();
        self.width - (prefix + lead).min(self.indent_cap())
    }

    /// Indents never take more than half the line: deeper ones lose their outer columns.
    fn indent_cap(&self) -> usize {
        self.width / 2
    }

    /// One line: the prefix, then `lead`, then `spans`; a wrapped line when `more`.
    fn line(&mut self, lead: Vec<Span>, spans: Vec<Span>, more: bool) {
        let mut front = self.prefix(true);
        front.extend(lead);
        let mut line: Vec<Span> = Vec::new();
        for span in tail_spans(front, self.indent_cap())
            .into_iter()
            .chain(spans)
        {
            match line.last_mut() {
                _ if span.text.is_empty() => {}
                Some(last) if last.joins(&span) => last.text.push_str(&span.text),
                _ => line.push(span),
            }
        }
        self.lines.push(if more {
            Line::wrapped(line)
        } else {
            Line::new(line)
        });
    }

    /// The open block's text, wrapped under the prefix and `lead` (on its first line; spaces
    /// on the rest). A button wider than the room is cut, keeping its brackets.
    fn flush(&mut self, lead: &[Span]) {
        let mut runs = std::mem::take(&mut self.inline);
        while runs.last().is_some_and(Vec::is_empty) {
            runs.pop();
        }
        if runs.is_empty() {
            return;
        }
        let lead_width: usize = lead.iter().map(|s| width(&s.text)).sum();
        let room = self.room(lead_width);
        let mut first = true;
        for run in runs {
            let run: Vec<Span> = run.into_iter().map(|s| self.fit_button(s, room)).collect();
            for (i, spans) in text::wrap_spans(&run, room).into_iter().enumerate() {
                let lead = if first {
                    lead.to_vec()
                } else {
                    vec![Span::new(" ".repeat(lead_width), Tone::Text)]
                };
                first = false;
                self.line(lead, spans, i > 0);
            }
        }
    }

    /// A button's span as it fits `room` cells: whole, or its label cut with `…`.
    fn fit_button(&self, span: Span, room: usize) -> Span {
        match span.button.and_then(|i| self.buttons.get(usize::from(i))) {
            Some(button) if width(&span.text) > room && room > 2 => {
                span.with_text(format!("[{}]", text::ellipsize(&button.label, room - 2)))
            }
            _ => span,
        }
    }

    /// A table, box-drawn when every column fits; else one bullet per row, cells split by
    /// ` · `.
    fn draw_table(&mut self, table: Table) {
        let cols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![0; cols];
        for row in &table.rows {
            for (c, cell) in row.iter().enumerate() {
                widths[c] = widths[c].max(text::spans_width(cell));
            }
        }
        if widths.iter().map(|w| w + 3).sum::<usize>() < self.room(0) {
            self.boxed(&table, &widths);
            return;
        }
        let bullet = BULLETS[self.lists.len() % BULLETS.len()];
        for row in table.rows {
            let mut spans = Vec::new();
            for (c, cell) in row.into_iter().enumerate() {
                if c > 0 {
                    spans.push(Span::new(" · ", Tone::Dim));
                }
                spans.extend(cell);
            }
            self.holders.push(Holder::Item {
                marker: Span::new(format!("{bullet} "), Tone::Dim),
                used: false,
            });
            self.inline = vec![spans];
            self.flush(&[]);
            self.holders.pop();
        }
    }

    fn boxed(&mut self, table: &Table, widths: &[usize]) {
        let border = |left: &str, join: &str, right: &str| {
            let cells: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            vec![Span::new(
                format!("{left}{}{right}", cells.join(join)),
                Tone::Dim,
            )]
        };
        self.line(Vec::new(), border("┌", "┬", "┐"), false);
        for (r, row) in table.rows.iter().enumerate() {
            if r == table.head && r > 0 {
                self.line(Vec::new(), border("├", "┼", "┤"), false);
            }
            let mut spans = Vec::new();
            for (c, w) in widths.iter().enumerate() {
                let cell = row.get(c).map(Vec::as_slice).unwrap_or_default();
                spans.push(Span::new("│", Tone::Dim));
                spans.push(Span::new(" ", Tone::Text));
                spans.extend_from_slice(cell);
                let pad = w - text::spans_width(cell) + 1;
                spans.push(Span::new(" ".repeat(pad), Tone::Text));
            }
            spans.push(Span::new("│", Tone::Dim));
            self.line(Vec::new(), spans, false);
        }
        self.line(Vec::new(), border("└", "┴", "┘"), false);
    }
}

/// `button` pushed on `buttons` (its label cleaned), and the span that draws it: `[label]` in
/// the Work tone, bold when `bold`, holding its index. `None`, and nothing pushed, once the
/// index would pass `u16::MAX`.
pub fn button_span(buttons: &mut Vec<Button>, button: Button, bold: bool) -> Option<Span> {
    let index = u16::try_from(buttons.len()).ok()?;
    let label = clean(&button.label);
    let span = Span {
        button: Some(index),
        ..Span::styled(format!("[{label}]"), Tone::Work, bold)
    };
    buttons.push(Button {
        label,
        command: button.command,
    });
    Some(span)
}

/// The last `w` cells of `spans`: what's left of an indent once its outer columns go.
fn tail_spans(spans: Vec<Span>, w: usize) -> Vec<Span> {
    let mut left = w;
    let mut out = Vec::new();
    for span in spans.into_iter().rev() {
        let sw = width(&span.text);
        if sw <= left {
            left -= sw;
            out.push(span);
        } else {
            let text = text::tail(&span.text, left);
            if !text.is_empty() {
                out.push(span.with_text(text));
            }
            break;
        }
    }
    out.reverse();
    out
}
