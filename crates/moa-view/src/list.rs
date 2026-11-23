//! Scrolling and selection over a view's lines.

use crate::line::{Item, Line};

/// How many lines the wheel scrolls.
pub const WHEEL: usize = 3;

/// Where a list is scrolled and what it has selected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListState {
    /// The selected line's index: always a selectable line, `None` when there is none.
    pub sel: Option<usize>,
    /// The first line shown.
    pub top: usize,
    /// The view follows the selection. The wheel turns it off until the next key.
    pub tracking: bool,
}

impl Default for ListState {
    fn default() -> ListState {
        ListState {
            sel: None,
            top: 0,
            tracking: true,
        }
    }
}

/// The indexes of the lines a selection can rest on.
fn selectable(lines: &[Line]) -> Vec<usize> {
    (0..lines.len())
        .filter(|&i| lines[i].selectable())
        .collect()
}

/// The index in `among` nearest `to`; the one above on a tie.
fn nearest(among: &[usize], to: usize) -> Option<usize> {
    among.iter().copied().min_by_key(|&i| i.abs_diff(to))
}

/// The highest `top` that still fills the body.
fn max_top(lines: &[Line], body: usize) -> usize {
    lines.len().saturating_sub(body)
}

impl ListState {
    /// After a rebuild: the selection moves to the line with the item it had (`was`), else to
    /// the selectable line nearest where it was, else to the first (V8).
    pub fn reselect(&mut self, was: Option<&Item>, lines: &[Line]) {
        let same = was.and_then(|was| {
            lines
                .iter()
                .position(|l| l.selectable() && l.item.as_ref() == Some(was))
        });
        self.sel = same.or_else(|| nearest(&selectable(lines), self.sel.unwrap_or(0)));
    }

    /// Scrolls so the selected item shows whole while the view follows the selection: its
    /// wrapped lines and the headings above it (back to the item before), and for the last
    /// item every line after it (the key hints). An item taller than the body shows from its
    /// first line. Then `top` is kept within the lines, following or not.
    pub fn follow(&mut self, lines: &[Line], body: usize) {
        let body = body.max(1);
        if let (true, Some(sel)) = (self.tracking, self.current(lines)) {
            let mut start = lines[..sel]
                .iter()
                .rposition(Line::selectable)
                .map_or(0, |i| i + 1);
            // Not the wrapped lines of the item above.
            while start < sel && lines[start].more {
                start += 1;
            }
            let last = !lines[sel + 1..].iter().any(Line::selectable);
            let mut end = if last {
                lines.len().saturating_sub(1)
            } else {
                sel
            };
            while end + 1 < lines.len() && lines[end + 1].more {
                end += 1;
            }
            let fits = (end + 1).saturating_sub(body);
            if self.top > start {
                self.top = start.max(fits);
            }
            if end >= self.top.saturating_add(body) {
                self.top = fits;
            }
            self.top = self.top.min(sel);
        }
        self.top = self.top.min(max_top(lines, body));
    }

    /// `j`/`k`: the next or previous item. With nothing to select, a line of scroll (V17).
    pub fn step(&mut self, lines: &[Line], down: bool) {
        self.tracking = true;
        let selectable = selectable(lines);
        let Some(sel) = self.current(lines) else {
            self.start_or_scroll(lines, &selectable, 1, down);
            return;
        };
        let next = if down {
            selectable.iter().copied().find(|&i| i > sel)
        } else {
            selectable.iter().rev().copied().find(|&i| i < sel)
        };
        self.sel = next.or(Some(sel));
    }

    /// `g`/`G`: the first or last item. With nothing to select, the top or the end.
    pub fn ends(&mut self, lines: &[Line], last: bool) {
        self.tracking = true;
        let selectable = selectable(lines);
        if selectable.is_empty() {
            self.top = if last {
                lines.len().saturating_sub(1)
            } else {
                0
            };
        }
        self.sel = if last {
            selectable.last().copied()
        } else {
            selectable.first().copied()
        };
    }

    /// PgUp/PgDn: the item about a body's height of lines away, stopping at the first and
    /// last. With nothing to select, a body's height of scroll.
    pub fn page(&mut self, lines: &[Line], body: usize, down: bool) {
        self.tracking = true;
        let body = body.max(1);
        let selectable = selectable(lines);
        let Some(sel) = self.current(lines) else {
            self.start_or_scroll(lines, &selectable, body, down);
            return;
        };
        self.sel = if down {
            let to = sel.saturating_add(body);
            selectable
                .iter()
                .copied()
                .find(|&i| i >= to)
                .or(selectable.last().copied())
        } else {
            let to = sel.checked_sub(body);
            to.and_then(|to| selectable.iter().rev().copied().find(|&i| i <= to))
                .or(selectable.first().copied())
        };
    }

    /// The wheel: [`WHEEL`] lines of scroll. A selection scrolled out of view comes along to the
    /// nearest visible item, since keys act on what can be seen; the view stays where it was
    /// scrolled until the next key.
    pub fn wheel(&mut self, lines: &[Line], body: usize, down: bool) {
        let body = body.max(1);
        self.top = if down {
            self.top.saturating_add(WHEEL).min(max_top(lines, body))
        } else {
            self.top.min(max_top(lines, body)).saturating_sub(WHEEL)
        };
        let shown: Vec<usize> = selectable(lines)
            .into_iter()
            .filter(|&i| self.top <= i && i < self.top + body)
            .collect();
        if !self.current(lines).is_some_and(|sel| shown.contains(&sel)) {
            // The nearest visible item; a stale selection with none in view is dropped.
            self.sel = self
                .sel
                .and_then(|sel| nearest(&shown, sel))
                .or(self.current(lines));
        }
        self.tracking = false;
    }

    /// The selectable line drawn at body row `y`: a wrapped line counts as its item's; a
    /// heading, a blank or a row outside the body or below the last line is `None`.
    pub fn row_at(&self, lines: &[Line], y: usize, body: usize) -> Option<usize> {
        if y >= body {
            return None;
        }
        let mut i = self.top.checked_add(y)?;
        if i >= lines.len() {
            return None;
        }
        while i > 0 && lines[i].more {
            i -= 1;
        }
        lines[i].selectable().then_some(i)
    }

    /// The selected line's item.
    pub fn selected<'a>(&self, lines: &'a [Line]) -> Option<&'a Item> {
        let line = lines.get(self.sel?)?;
        if line.selectable() {
            line.item.as_ref()
        } else {
            None
        }
    }

    /// The selection, when it is a selectable line of `lines`. After the rows change and
    /// before `reselect`, `sel` can be stale: past the end, or on a line that isn't an item.
    fn current(&self, lines: &[Line]) -> Option<usize> {
        self.sel
            .filter(|&i| lines.get(i).is_some_and(Line::selectable))
    }

    /// A key with no selection (or a stale one): it selects the first item, or with none,
    /// moves `top` by `by` lines, no further than the last line (`follow` keeps it within the
    /// body).
    fn start_or_scroll(&mut self, lines: &[Line], selectable: &[usize], by: usize, down: bool) {
        self.sel = selectable.first().copied();
        if self.sel.is_some() {
            return;
        }
        let last = lines.len().saturating_sub(1);
        self.top = if down {
            self.top.saturating_add(by).min(last)
        } else {
            self.top.min(last).saturating_sub(by)
        };
    }
}
