//! What a `Terminal` shows: its whole output replayed on a grid, as a terminal emulator
//! would, so a test can read a cell's character and colour where tmux drew it. Only what
//! tmux sends to an `xterm-256color` client is understood: cursor moves, erases, inserts
//! and deletes, the scroll region, `REP`, and the foreground colour. Anything else is
//! skipped.

use moa_testkit::Terminal;
use unicode_width::UnicodeWidthChar;

/// A cell's foreground colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fg {
    Default,
    Index(u8),
    Rgb(u8, u8, u8),
}

impl Fg {
    /// `#rrggbb`, as tmux's styles write it.
    pub fn hex(hex: &str) -> Fg {
        let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap();
        let [_, r, g, b] = n.to_be_bytes();
        Fg::Rgb(r, g, b)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Fg,
}

const BLANK: Cell = Cell {
    ch: ' ',
    fg: Fg::Default,
};

pub struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Cell>>,
    x: usize,
    y: usize,
    /// Past the last column: the next character wraps first.
    wrap: bool,
    fg: Fg,
    saved: (usize, usize),
    /// The scroll region's first and last rows.
    top: usize,
    bottom: usize,
    last: char,
}

impl Screen {
    /// What `term`, `cols` by `rows`, shows now.
    pub fn of(term: &Terminal, cols: usize, rows: usize) -> Screen {
        let mut s = Screen::blank(cols, rows);
        s.feed(&String::from_utf8_lossy(&term.output()));
        s
    }

    /// The cell at `at`, 1-based as a mouse report gives it.
    pub fn cell(&self, at: (u32, u32)) -> Cell {
        let (x, y) = at;
        self.cells[y as usize - 1][x as usize - 1]
    }

    /// Row `y` (1-based) as text.
    pub fn row(&self, y: usize) -> String {
        self.cells[y - 1].iter().map(|c| c.ch).collect()
    }

    /// Every row as text, for a failure message.
    pub fn text(&self) -> String {
        (1..=self.rows)
            .map(|y| self.row(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn feed(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut fin = None;
                        for c in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&c) {
                                fin = Some(c);
                                break;
                            }
                            params.push(c);
                        }
                        if let Some(fin) = fin {
                            self.csi(&params, fin);
                        }
                    }
                    // OSC, DCS, APC: up to BEL or ST.
                    Some(']' | 'P' | '_' | '^') => {
                        while let Some(c) = chars.next() {
                            if c == '\x07' {
                                break;
                            }
                            if c == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    Some('(' | ')' | '*' | '+' | '#') => {
                        chars.next();
                    }
                    Some('M') => {
                        if self.y == self.top {
                            self.scroll_down(1);
                        } else {
                            self.y = self.y.saturating_sub(1);
                        }
                    }
                    Some('D') => self.line_feed(),
                    Some('E') => {
                        self.x = 0;
                        self.line_feed();
                    }
                    Some('7') => self.saved = (self.x, self.y),
                    Some('8') => (self.x, self.y) = self.saved,
                    Some('c') => *self = Screen::blank(self.cols, self.rows),
                    _ => {}
                },
                '\r' => self.go(0, self.y),
                '\n' | '\x0b' | '\x0c' => self.line_feed(),
                '\x08' => self.go(self.x.saturating_sub(1), self.y),
                '\t' => self.go(((self.x / 8) + 1) * 8, self.y),
                c if c < ' ' || c == '\x7f' => {}
                c => self.print(c),
            }
        }
    }

    fn blank(cols: usize, rows: usize) -> Screen {
        Screen {
            cols,
            rows,
            cells: vec![vec![BLANK; cols]; rows],
            x: 0,
            y: 0,
            wrap: false,
            fg: Fg::Default,
            saved: (0, 0),
            top: 0,
            bottom: rows - 1,
            last: ' ',
        }
    }

    fn go(&mut self, x: usize, y: usize) {
        self.x = x.min(self.cols - 1);
        self.y = y.min(self.rows - 1);
        self.wrap = false;
    }

    fn print(&mut self, c: char) {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            return;
        }
        if self.wrap {
            self.x = 0;
            self.line_feed();
        }
        self.cells[self.y][self.x] = Cell { ch: c, fg: self.fg };
        if width == 2 && self.x + 1 < self.cols {
            self.cells[self.y][self.x + 1] = Cell {
                ch: ' ',
                fg: self.fg,
            };
        }
        self.last = c;
        if self.x + width >= self.cols {
            self.x = self.cols - 1;
            self.wrap = true;
        } else {
            self.x += width;
        }
    }

    fn line_feed(&mut self) {
        self.wrap = false;
        if self.y == self.bottom {
            self.scroll_up(1);
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            self.cells.remove(self.top);
            self.cells.insert(self.bottom, vec![BLANK; self.cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            self.cells.remove(self.bottom);
            self.cells.insert(self.top, vec![BLANK; self.cols]);
        }
    }

    fn csi(&mut self, params: &str, fin: char) {
        // Private modes (`?1049h`) and the like change nothing drawn.
        if params.starts_with(['?', '>', '<', '=']) {
            return;
        }
        let nums: Vec<usize> = params
            .split(';')
            .map(|p| p.split(':').next().unwrap_or("").parse().unwrap_or(0))
            .collect();
        let n = |i: usize| nums.get(i).copied().filter(|n| *n > 0).unwrap_or(1);
        let (x, y) = (self.x, self.y);
        match fin {
            'H' | 'f' => self.go(n(1) - 1, n(0) - 1),
            'A' => self.go(x, y.saturating_sub(n(0))),
            'B' => self.go(x, y + n(0)),
            'C' => self.go(x + n(0), y),
            'D' => self.go(x.saturating_sub(n(0)), y),
            'E' => self.go(0, y + n(0)),
            'F' => self.go(0, y.saturating_sub(n(0))),
            'G' | '`' => self.go(n(0) - 1, y),
            'd' => self.go(x, n(0) - 1),
            'J' => {
                let (from, to) = match nums[0] {
                    0 => (y * self.cols + x, self.rows * self.cols),
                    1 => (0, y * self.cols + x + 1),
                    _ => (0, self.rows * self.cols),
                };
                for i in from..to {
                    self.cells[i / self.cols][i % self.cols] = BLANK;
                }
            }
            'K' => {
                let (from, to) = match nums[0] {
                    0 => (x, self.cols),
                    1 => (0, x + 1),
                    _ => (0, self.cols),
                };
                self.cells[y][from..to].fill(BLANK);
            }
            'X' => {
                let to = (x + n(0)).min(self.cols);
                self.cells[y][x..to].fill(BLANK);
            }
            '@' => {
                for _ in 0..n(0).min(self.cols - x) {
                    self.cells[y].insert(x, BLANK);
                    self.cells[y].pop();
                }
            }
            'P' => {
                for _ in 0..n(0).min(self.cols - x) {
                    self.cells[y].remove(x);
                    self.cells[y].push(BLANK);
                }
            }
            'L' | 'M' if (self.top..=self.bottom).contains(&y) => {
                let top = self.top;
                self.top = y;
                if fin == 'L' {
                    self.scroll_down(n(0));
                } else {
                    self.scroll_up(n(0));
                }
                self.top = top;
            }
            'S' => self.scroll_up(n(0)),
            'T' => self.scroll_down(n(0)),
            'r' => {
                self.top = n(0) - 1;
                self.bottom = nums
                    .get(1)
                    .copied()
                    .filter(|n| *n > 0)
                    .map_or(self.rows - 1, |b| b - 1);
                self.go(0, 0);
            }
            'b' => {
                for _ in 0..n(0) {
                    self.print(self.last);
                }
            }
            'm' => self.sgr(params),
            _ => {}
        }
    }

    /// The foreground colour from an SGR: `38;2;r;g;b` or `38:2::r:g:b`, `38;5;n`, the
    /// basic and bright colours, `39`, and the resets.
    fn sgr(&mut self, params: &str) {
        let parts: Vec<Vec<u16>> = params
            .split(';')
            .map(|p| p.split(':').map(|n| n.parse().unwrap_or(0)).collect())
            .collect();
        let mut i = 0;
        while i < parts.len() {
            let p = &parts[i];
            match p[0] {
                0 | 39 => self.fg = Fg::Default,
                n @ 30..=37 => self.fg = Fg::Index((n - 30) as u8),
                n @ 90..=97 => self.fg = Fg::Index((n - 90 + 8) as u8),
                38 if p.len() > 1 => {
                    // The colon form: `38:5:n` or `38:2:[space]:r:g:b`.
                    let rest = &p[1..];
                    self.fg = match rest {
                        [5, n] => Fg::Index(*n as u8),
                        [2, _, r, g, b] | [2, r, g, b] => Fg::Rgb(*r as u8, *g as u8, *b as u8),
                        _ => self.fg,
                    };
                }
                38 => {
                    let at = |k: usize| parts.get(i + k).map_or(0, |p| p[0]);
                    match at(1) {
                        5 => {
                            self.fg = Fg::Index(at(2) as u8);
                            i += 2;
                        }
                        2 => {
                            self.fg = Fg::Rgb(at(2) as u8, at(3) as u8, at(4) as u8);
                            i += 4;
                        }
                        _ => {}
                    }
                }
                48 | 58 => {
                    // A background or underline colour: skip its arguments.
                    if p.len() == 1 {
                        let kind = parts.get(i + 1).map_or(0, |p| p[0]);
                        i += if kind == 5 {
                            2
                        } else if kind == 2 {
                            4
                        } else {
                            0
                        };
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}
