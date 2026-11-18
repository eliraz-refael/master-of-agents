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
    /// The list.
    pub body: Rect,
    /// The picture's activity strip; the rule above it is the line just above the rect.
    /// `None` when the screen is too short for it.
    pub strip: Option<Rect>,
    /// One line for the editor, a question, a message or the key hints.
    pub footer: Rect,
}

/// The picture's activity strip: its height in lines (the spike's `STRIP`).
pub const STRIP: u16 = 7;

/// The layout of `view` at `size`. A framed view keeps the spike's padding inside the frame:
/// the body at x 2, y 1, `w - 3` wide; one blank line, then the footer on the last inner line.
/// The picture has no frame: its tabs on line 0, the body, a rule, the 7-line strip, and the
/// footer on the last line; a screen too short for the strip leaves it out. Parts too small
/// to have cells are empty.
pub fn geometry(view: View, size: Size) -> Geometry {
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
            body: Rect::new(0, 1, w, body_h),
            strip,
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
        body: Rect::new(2, 1, inner_w, inner.saturating_sub(2)),
        strip: None,
        footer,
    }
}
