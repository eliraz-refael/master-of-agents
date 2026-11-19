//! A [`Screen`] drawn into a ratatui [`Buffer`]: the rounded frame and its title, the
//! picture's tabs, brand, rule and strip, the body's lines with the selection bar and the
//! hover mark, the chips, and the footer.
//!
//! Every string is placed by `moa_view::text`'s display width, so a wide glyph (💤, CJK) takes
//! two cells and is never split: the cell it covers is left as ratatui leaves it, empty, and a
//! glyph that doesn't fit whole isn't drawn. Nothing is drawn outside `area`.

use moa_view::geometry::Rect as Part;
use moa_view::line::{Line, Span, Tone};
use moa_view::panel::{Mark, Screen};
use moa_view::text;
use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::Theme;

/// Draws `screen` into `area` of `buf`: the screen's parts are laid out from `area`'s top
/// left corner, and every cell of `area` (inside `buf`) is drawn afresh.
pub fn draw(buf: &mut Buffer, area: Rect, screen: &Screen, theme: &Theme) {
    let mut canvas = Canvas::new(buf, area);
    canvas.clear();
    let g = &screen.geometry;
    if g.framed {
        canvas.frame(screen.title.as_deref());
    }
    if let Some(row) = g.tabs {
        draw_tabs(&mut canvas, row, screen, theme);
    }
    draw_body(&mut canvas, g.body, &screen.body, theme);
    if let Some(strip) = g.strip {
        // The rule is the line just above the strip.
        if let (Some(rule), Some(y)) = (&screen.rule, strip.y.checked_sub(1)) {
            canvas.spans(strip.x, y, strip.w, &rule.spans, |s| plain(theme, s));
        }
        for (line, y) in screen.strip.iter().zip(strip.y..strip.y + strip.h) {
            canvas.spans(strip.x, y, strip.w, &line.spans, |s| plain(theme, s));
        }
    }
    let footer = &screen.footer;
    if g.footer.h > 0 {
        let (f, style) = (g.footer, plain(theme, footer));
        canvas.put(f.x, f.y, f.w, &footer.text, style);
    }
}

/// A span's own style: its tone, bold and reversed as it says.
fn plain(theme: &Theme, span: &Span) -> Style {
    let style = theme.style(span.tone, span.bold);
    if span.reversed {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

/// A body span's style under its line's mark: on the selection bar, the bar's colours with
/// the span's bold; on a hovered line, its own colours on the hover background.
fn marked(theme: &Theme, mark: Mark, span: &Span) -> Style {
    match mark {
        Mark::Plain => plain(theme, span),
        Mark::Hovered => theme.hovered().patch(plain(theme, span)),
        Mark::Selected => {
            let style = theme.selected();
            if span.bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
    }
}

/// A chip's style: as a span of its line, except that a padded pill (late, today) keeps its
/// own colours on the selection bar.
fn chip_style(theme: &Theme, mark: Mark, chip: &Span) -> Style {
    let pill = matches!(chip.tone, Tone::Late | Tone::Today);
    if mark == Mark::Selected && pill {
        theme.selected().patch(plain(theme, chip))
    } else {
        marked(theme, mark, chip)
    }
}

/// The picture's tab row: the tabs in the heading's colour, the shown one in brackets, and
/// the brand against the right edge (the spike's `w - len(brand) - 1`).
fn draw_tabs(canvas: &mut Canvas<'_>, row: Part, screen: &Screen, theme: &Theme) {
    if row.h == 0 {
        return;
    }
    if let Some((names, shown)) = &screen.tabs {
        let tabs = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                if i == *shown {
                    format!("[{name}]")
                } else {
                    format!(" {name} ")
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        canvas.put(row.x, row.y, row.w, &tabs, theme.style(Tone::Head, true));
    }
    if let Some(brand) = &screen.brand {
        let w = text::width(&brand.text);
        let left = usize::from(row.w)
            .checked_sub(w + 1)
            .and_then(|x| u16::try_from(x).ok());
        if let Some(left) = left {
            canvas.put(
                row.x + left,
                row.y,
                row.w - left,
                &brand.text,
                plain(theme, brand),
            );
        }
    }
}

/// The body's lines: a marked line is a full-width bar (selected) or background (hovered),
/// its spans in their tones, and its chip against the right edge when it fits after the text
/// with a cell to spare.
fn draw_body(canvas: &mut Canvas<'_>, body: Part, lines: &[(Line, Mark)], theme: &Theme) {
    for ((line, mark), y) in lines.iter().zip(body.y..body.y + body.h) {
        let mark = *mark;
        match mark {
            Mark::Plain => {}
            Mark::Hovered => canvas.fill(body.x, y, body.w, theme.hovered()),
            Mark::Selected => canvas.fill(body.x, y, body.w, theme.selected()),
        }
        canvas.spans(body.x, y, body.w, &line.spans, |s| marked(theme, mark, s));
        if let Some(chip) = &line.chip {
            let chip_w = text::width(&chip.text);
            let text_w = text::width(line.text().trim_end());
            let fits = usize::from(body.w) > text_w + chip_w;
            if let (true, Ok(chip_w)) = (fits, u16::try_from(chip_w)) {
                let x = body.x + body.w - chip_w;
                canvas.put(x, y, chip_w, &chip.text, chip_style(theme, mark, chip));
            }
        }
    }
}

/// `area` of a buffer, written cell by cell, clipped to the buffer. Coordinates are relative
/// to `area`.
struct Canvas<'b> {
    buf: &'b mut Buffer,
    area: Rect,
    /// The cells that may be written: `area` inside the buffer.
    clip: Rect,
}

impl<'b> Canvas<'b> {
    fn new(buf: &'b mut Buffer, area: Rect) -> Canvas<'b> {
        let clip = area.intersection(buf.area);
        Canvas { buf, area, clip }
    }

    /// Every cell of the area, empty.
    fn clear(&mut self) {
        for y in self.clip.top()..self.clip.bottom() {
            for x in self.clip.left()..self.clip.right() {
                self.buf[(x, y)].reset();
            }
        }
    }

    /// The buffer's position of (`x`, `y`), when it may be written.
    fn at(&self, x: u16, y: u16) -> Option<(u16, u16)> {
        let ax = u16::try_from(u32::from(self.area.x) + u32::from(x)).ok()?;
        let ay = u16::try_from(u32::from(self.area.y) + u32::from(y)).ok()?;
        self.clip.contains((ax, ay).into()).then_some((ax, ay))
    }

    /// Cell (`x`, `y`) as `symbol` in `style` exactly.
    fn set(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        if let Some(p) = self.at(x, y) {
            let cell = &mut self.buf[p];
            cell.reset();
            cell.set_symbol(symbol).set_style(style);
        }
    }

    /// Cell (`x`, `y`) as a wide glyph covers it: empty, as ratatui leaves it.
    fn cover(&mut self, x: u16, y: u16) {
        if let Some(p) = self.at(x, y) {
            self.buf[p].reset();
        }
    }

    /// `w` cells from (`x`, `y`), blank in `style`.
    fn fill(&mut self, x: u16, y: u16, w: u16, style: Style) {
        for dx in 0..w {
            if let Some(cx) = x.checked_add(dx) {
                self.set(cx, y, " ", style);
            }
        }
    }

    /// `s` from (`x`, `y`) in at most `room` cells, by display width; a grapheme that
    /// doesn't fit whole ends it. Returns the cells used.
    fn put(&mut self, x: u16, y: u16, room: u16, s: &str, style: Style) -> u16 {
        let mut used: u16 = 0;
        for g in s.graphemes(true) {
            let w = text::width(g);
            if w == 0 {
                continue;
            }
            let fits = |w: &u16| used.checked_add(*w).is_some_and(|end| end <= room);
            let Some(w) = u16::try_from(w).ok().filter(fits) else {
                break;
            };
            let Some(gx) = x.checked_add(used) else {
                break;
            };
            match drawable(g, w) {
                // A glyph whose cells aren't all in the area would spill out of it.
                Some(symbol) if (0..w).all(|dx| self.at(gx.saturating_add(dx), y).is_some()) => {
                    self.set(gx, y, symbol, style);
                    for dx in 1..w {
                        self.cover(gx + dx, y);
                    }
                }
                _ => self.fill(gx, y, w, style),
            }
            used += w;
        }
        used
    }

    /// `spans` one after another from (`x`, `y`) in at most `room` cells, each in
    /// `style(span)`; a span cut short ends the line. Returns the cells used.
    fn spans(
        &mut self,
        x: u16,
        y: u16,
        room: u16,
        spans: &[Span],
        style: impl Fn(&Span) -> Style,
    ) -> u16 {
        let mut used = 0;
        for span in spans {
            let n = self.put(
                x.saturating_add(used),
                y,
                room - used,
                &span.text,
                style(span),
            );
            used += n;
            if usize::from(n) != text::width(&span.text) {
                break;
            }
        }
        used
    }

    /// The rounded frame round the whole area, in the terminal's default colour, with ` title `
    /// in bold on its top edge at column 2. Nothing under 4 × 2.
    fn frame(&mut self, title: Option<&str>) {
        let (w, h) = (self.area.width, self.area.height);
        if w < 4 || h < 2 {
            return;
        }
        let edge = Style::new().fg(Color::Reset);
        let (right, bottom) = (w - 1, h - 1);
        for x in 1..right {
            self.set(x, 0, "─", edge);
            self.set(x, bottom, "─", edge);
        }
        for y in 1..bottom {
            self.set(0, y, "│", edge);
            self.set(right, y, "│", edge);
        }
        self.set(0, 0, "╭", edge);
        self.set(right, 0, "╮", edge);
        self.set(0, bottom, "╰", edge);
        self.set(right, bottom, "╯", edge);
        if let Some(title) = title {
            // Between column 2 and the corner.
            let title = format!(" {title} ");
            self.put(2, 0, w - 3, &title, edge.add_modifier(Modifier::BOLD));
        }
    }
}

/// What to put in a grapheme's first cell, or `None` to draw it as `w` blanks: a grapheme
/// with a control character, or one ratatui would measure otherwise than moa (and so draw
/// out of step with the line), is drawn as its first code point when that has the same
/// width, else as blanks.
fn drawable(g: &str, w: u16) -> Option<&str> {
    if !g.contains(char::is_control) && g.cell_width() == w {
        return Some(g);
    }
    let first = g.chars().next()?;
    let first = &g[..first.len_utf8()];
    let same = !first.contains(char::is_control)
        && first.cell_width() == w
        && text::width(first) == usize::from(w);
    same.then_some(first)
}
