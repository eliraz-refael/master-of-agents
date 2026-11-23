//! Where a panel's parts go on its screen: the spike's layout, as rectangles.

use moa_core::panel::View;

/// The panel's size in cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Size {
    pub w: u16,
    pub h: u16,
}

/// A rectangle of cells. A part that doesn't fit is `Rect::default()`: no cells at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    /// The rectangle, or the empty one when it has no cells.
    fn new(x: u16, y: u16, w: u16, h: u16) -> Rect {
        if w == 0 || h == 0 {
            Rect::default()
        } else {
            Rect { x, y, w, h }
        }
    }

    /// Whether cell (`x`, `y`) is in it.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && u32::from(x) < u32::from(self.x) + u32::from(self.w)
            && u32::from(y) < u32::from(self.y) + u32::from(self.h)
    }
}

/// A view's layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// A rounded frame with the view's title round the whole screen.
    pub framed: bool,
    /// The picture's tab row.
    pub tabs: Option<Rect>,
    /// The picker's head, fixed above its list: the query line and a rule. The console's:
    /// its head line (B9).
    pub head: Option<Rect>,
    /// The list.
    pub body: Rect,
    /// The picture's activity strip; the rule above it is the line just above the rect.
    /// `None` when the screen is too short for it.
    pub strip: Option<Rect>,
    /// The console's input line, under its feed (B7).
    pub input: Option<Rect>,
    /// The console's button bar, under its input line (B5).
    pub bar: Option<Rect>,
    /// The console's `/` palette, directly above its input line (B11). Empty while it is
    /// closed.
    pub palette: Option<Rect>,
    /// One line for the editor, a question, a message or the key hints.
    pub footer: Rect,
}

/// The picture's activity strip: its height in lines (the spike's `STRIP`).
pub const STRIP: u16 = 7;

/// The layout of `view` at `size`; `framed` is whether it draws a frame (`View::framed`).
/// A framed view keeps the spike's padding inside the frame:
/// the body at x 2, y 1, `w - 3` wide; one blank line, then the footer on the last inner line.
/// The picture has no frame: its tabs on line 0, the body, a rule, the 7-line strip, and the
/// footer on the last line; a screen too short for the strip leaves it out. Anything else
/// without a frame is laid out like the empty slot: the spike's margins, the body at x 2,
/// y 1, and the footer on the last line. The picker's head takes the first [`HEAD`] lines of
/// its body, framed or not. The console's body is, from the top: its head line, the feed, its
/// input line and its bar, a line each but the feed (an open palette takes rows above the
/// input line, from the feed); a body too short for all of them keeps
/// the input first, then the head line, then the bar. Parts too small to have cells are empty.
pub fn geometry(view: View, framed: bool, size: Size) -> Geometry {
    let mut g = layout(view, framed, size);
    let b = g.body;
    if view == View::Picker {
        let head = b.h.min(HEAD);
        g.head = Some(Rect::new(b.x, b.y, b.w, head));
        g.body = Rect::new(b.x, b.y + head, b.w, b.h - head);
    }
    if view == View::Console {
        let input = b.h.min(1);
        let head = b.h.saturating_sub(1).min(1);
        let bar = b.h.saturating_sub(2).min(1);
        let feed = b.h - input - head - bar;
        g.head = Some(Rect::new(b.x, b.y, b.w, head));
        g.body = Rect::new(b.x, b.y + head, b.w, feed);
        g.input = Some(Rect::new(b.x, b.y + head + feed, b.w, input));
        g.bar = Some(Rect::new(b.x, b.y + head + feed + input, b.w, bar));
        g.palette = Some(Rect::default());
    }
    g
}

impl Geometry {
    /// The console's bar `rows` high, its extra rows taken from the bottom of the feed, which
    /// keeps at least one row while it has one. Its other views, and a console too short for
    /// its bar, are as they are.
    pub fn with_bar_rows(mut self, rows: u16) -> Geometry {
        let (Some(bar), Some(input)) = (self.bar, self.input) else {
            return self;
        };
        if bar.h == 0 || rows <= bar.h {
            return self;
        }
        let more = (rows - bar.h).min(self.body.h.saturating_sub(1));
        let b = self.body;
        self.body = Rect::new(b.x, b.y, b.w, b.h - more);
        self.input = Some(Rect::new(input.x, input.y - more, input.w, input.h));
        self.bar = Some(Rect::new(bar.x, bar.y - more, bar.w, bar.h + more));
        self
    }
}

impl Geometry {
    /// The console's palette `rows` high, directly above its input line, its rows taken from
    /// the bottom of the feed, which keeps at least one row while it has one (F16). Its other
    /// views are as they are.
    pub fn with_palette_rows(mut self, rows: u16) -> Geometry {
        if self.palette.is_none() {
            return self;
        }
        let b = self.body;
        let more = rows.min(b.h.saturating_sub(1));
        self.body = Rect::new(b.x, b.y, b.w, b.h - more);
        self.palette = Some(Rect::new(b.x, b.y + b.h - more, b.w, more));
        self
    }
}

/// The picker's head: its query line and the rule under it.
pub const HEAD: u16 = 2;

/// Every part but the picker's head.
fn layout(view: View, framed: bool, size: Size) -> Geometry {
    let Size { w, h } = size;
    if view == View::Picture {
        let tabs = Rect::new(0, 0, w, h.min(1));
        let footer = if h >= 2 {
            Rect::new(0, h - 1, w, 1)
        } else {
            Rect::default()
        };
        // Tabs, at least a line of body, the rule, the strip and the footer.
        let (body_h, strip) = if h > STRIP + 3 {
            (h - STRIP - 3, Some(Rect::new(0, h - 1 - STRIP, w, STRIP)))
        } else {
            (h.saturating_sub(2), None)
        };
        return Geometry {
            framed: false,
            tabs: Some(tabs),
            head: None,
            body: Rect::new(0, 1, w, body_h),
            strip,
            input: None,
            bar: None,
            palette: None,
            footer,
        };
    }
    if !framed {
        let footer = if h >= 2 {
            Rect::new(2, h - 1, w.saturating_sub(2), 1)
        } else {
            Rect::default()
        };
        return Geometry {
            framed: false,
            tabs: None,
            head: None,
            body: Rect::new(2, 1, w.saturating_sub(2), h.saturating_sub(2)),
            strip: None,
            input: None,
            bar: None,
            palette: None,
            footer,
        };
    }
    let inner = h.saturating_sub(2);
    let inner_w = w.saturating_sub(3);
    let footer = if inner >= 1 {
        Rect::new(2, inner, inner_w, 1)
    } else {
        Rect::default()
    };
    Geometry {
        framed: true,
        tabs: None,
        head: None,
        body: Rect::new(2, 1, inner_w, inner.saturating_sub(2)),
        strip: None,
        input: None,
        bar: None,
        palette: None,
        footer,
    }
}
