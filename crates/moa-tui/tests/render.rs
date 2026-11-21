//! Every view of the Lantern fixture drawn into a ratatui `TestBackend`: text snapshots at
//! five widths (V19), cell checks of the colours at 80 (V11, V12), wide glyphs (V9), and
//! sizes too small for anything (nothing drawn outside the pane).

use moa_core::panel::View;
use moa_core::publish::TermRow;
use moa_tui::draw::draw;
use moa_tui::theme::{ColorMode, Theme};
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::panel::{Mark, Mouse, Panel, Screen};
use moa_view::text;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const VIEWS: [(&str, View); 7] = [
    ("tree", View::Tree),
    ("plan", View::Plan),
    ("status", View::Status),
    ("today", View::Today),
    ("projects", View::Projects),
    ("activity", View::Activity),
    ("picture", View::Picture),
];

const WIDTHS: [u16; 5] = [40, 60, 80, 120, 200];
const HEIGHT: u16 = 16;

const TEXT: Color = Color::Rgb(0xd8, 0xde, 0xe9);
const BRIGHT: Color = Color::Rgb(0xec, 0xef, 0xf4);
const SURFACE: Color = Color::Rgb(0x3b, 0x42, 0x52);
const SEL: Color = Color::Rgb(0x5e, 0x81, 0xac);
const DARK: Color = Color::Rgb(0x2e, 0x34, 0x40);
const YELLOW: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
const DIM: Color = Color::Rgb(0x8c, 0x9a, 0xb3);
const RED: Color = Color::Rgb(0xd5, 0x77, 0x80);

fn theme() -> Theme {
    Theme {
        mode: ColorMode::True,
    }
}

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    fn new(view: View) -> T {
        let (data, ctx) = lantern();
        T {
            panel: Panel::new(view),
            data,
            ctx,
        }
    }

    /// The picker on the fixture with two terminals, `⌨ kite` and `⌨ harbor`.
    fn picker() -> T {
        let mut t = T::new(View::Picker);
        for (pane, name) in [("%5", "kite"), ("%7", "harbor")] {
            let row = TermRow {
                name: name.to_string(),
            };
            t.data.terms.insert(pane.to_string(), row);
        }
        t
    }

    fn screen(&mut self, w: u16, h: u16) -> Screen {
        self.panel.screen(&self.data, &self.ctx, Size { w, h })
    }

    /// The panel's screen at `w` × `h`, drawn into a `TestBackend` of that size.
    fn drawn(&mut self, w: u16, h: u16) -> Buffer {
        let screen = self.screen(w, h);
        drawn(&screen, w, h)
    }

    fn key(&mut self, k: Key, w: u16, h: u16) {
        self.panel.key(k, &self.data, &self.ctx, Size { w, h });
    }
}

fn drawn(screen: &Screen, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("a test terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw(frame.buffer_mut(), area, screen, &theme());
        })
        .expect("a test terminal draws");
    terminal.backend().buffer().clone()
}

/// Row `y`'s visible cells: each one's column and symbol, without the cells a wide glyph
/// covers.
fn cells(buf: &Buffer, y: u16) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut covered = 0;
    for x in 0..buf.area.width {
        let symbol = buf[(x, y)].symbol();
        if covered > 0 {
            covered -= 1;
            continue;
        }
        covered = text::width(symbol).saturating_sub(1);
        out.push((x, symbol.to_string()));
    }
    out
}

/// Row `y` as it shows.
fn row(buf: &Buffer, y: u16) -> String {
    cells(buf, y).into_iter().map(|(_, s)| s).collect()
}

/// The buffer as it shows, each row's trailing spaces trimmed.
fn screen_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        out.push_str(row(buf, y).trim_end());
        out.push('\n');
    }
    out
}

/// Where `needle` starts on screen: its first cell's column and its row.
fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    for y in 0..buf.area.height {
        let cells = cells(buf, y);
        let mut line = String::new();
        let mut starts = Vec::new();
        for (x, s) in &cells {
            starts.push((line.len(), *x));
            line.push_str(s);
        }
        if let Some(at) = line.find(needle) {
            let x = starts
                .iter()
                .find(|(byte, _)| *byte == at)
                .map(|(_, x)| *x)
                .expect("the needle starts at a cell");
            return (x, y);
        }
    }
    panic!("{needle:?} isn't on screen:\n{}", screen_text(buf));
}

// ---------- snapshots (V19) ----------

#[test]
fn every_view_at_every_width() {
    for (name, view) in VIEWS {
        for width in WIDTHS {
            let buf = T::new(view).drawn(width, HEIGHT);
            insta::assert_snapshot!(format!("{name}@{width}"), screen_text(&buf));
        }
    }
}

/// The picker, framed, with its query and rule fixed above the list: empty, then with
/// `kite` typed.
#[test]
fn the_picker_at_every_width() {
    for width in WIDTHS {
        let mut t = T::picker();
        let buf = t.drawn(width, HEIGHT);
        insta::assert_snapshot!(format!("picker@{width}"), screen_text(&buf));
        t.panel.paste("kite");
        let buf = t.drawn(width, HEIGHT);
        insta::assert_snapshot!(format!("picker-kite@{width}"), screen_text(&buf));
    }
}

/// Help, framed and titled, from its top, its long keys wrapped under their words (S10).
#[test]
fn help_at_every_width() {
    for width in WIDTHS {
        let buf = T::new(View::Help).drawn(width, HEIGHT);
        let text = screen_text(&buf);
        assert!(text.contains("keys and help"), "{text}");
        insta::assert_snapshot!(format!("help@{width}"), text);
    }
}

/// The empty slot draws unframed: its four dim lines, no title and no key hints.
#[test]
fn the_empty_slot_at_40_and_80() {
    for width in [40, 80] {
        let buf = T::new(View::EmptySlot).drawn(width, HEIGHT);
        let text = screen_text(&buf);
        insta::assert_snapshot!(format!("empty-slot@{width}"), text);
        assert!(!text.contains('╭'), "{text}");
        assert!(!text.contains("agent spot"), "no title: {text}");
    }
}

// ---------- cells at 80 (V12, V11) ----------

#[test]
fn the_frame_uses_the_default_colour() {
    let buf = T::new(View::Plan).drawn(80, HEIGHT);
    let (right, bottom) = (79, HEIGHT - 1);
    for (x, y, glyph) in [
        (0, 0, "╭"),
        (right, 0, "╮"),
        (0, bottom, "╰"),
        (right, bottom, "╯"),
        (1, 0, "─"),
        (right - 1, 0, "─"),
        (1, bottom, "─"),
    ] {
        assert_eq!(buf[(x, y)].symbol(), glyph, "({x}, {y})");
        assert_eq!(buf[(x, y)].fg, Color::Reset, "({x}, {y})");
    }
    for y in 1..bottom {
        for x in [0, right] {
            assert_eq!(buf[(x, y)].symbol(), "│", "({x}, {y})");
            assert_eq!(buf[(x, y)].fg, Color::Reset, "({x}, {y})");
        }
    }
    // The title sits on the top edge at column 2, bold, in the frame's colour.
    let (x, y) = find(&buf, " ◷ big picture ");
    assert_eq!((x, y), (2, 0));
    assert_eq!(buf[(3, 0)].fg, Color::Reset);
    assert!(buf[(3, 0)].modifier.contains(Modifier::BOLD));
    // Body text is `text`.
    let (x, y) = find(&buf, "Pick a cover");
    assert_eq!(buf[(x, y)].fg, TEXT);
}

#[test]
fn a_hovered_row_has_the_surface_background() {
    let mut t = T::new(View::Tree);
    t.screen(80, HEIGHT);
    // Fix login, a waiting session, is the body's fourth line: row 4.
    t.panel.mouse(
        Mouse::Move { x: 10, y: 4 },
        &t.data,
        &t.ctx,
        Size { w: 80, h: HEIGHT },
    );
    let screen = t.screen(80, HEIGHT);
    assert_eq!(screen.body[3].1, Mark::Hovered);
    let buf = drawn(&screen, 80, HEIGHT);
    assert!(row(&buf, 4).contains("Fix login"));
    for x in 2..79 {
        let cell = &buf[(x, 4)];
        assert_eq!(cell.bg, SURFACE, "({x}, 4)");
        if cell.symbol() != " " {
            assert_eq!(cell.fg, YELLOW, "({x}, 4) keeps its tone");
        }
    }
    // The frame and its padding aren't marked.
    assert_ne!(buf[(1, 4)].bg, SURFACE);
    assert_ne!(buf[(79, 4)].bg, SURFACE);
}

#[test]
fn the_selection_bar_spans_the_body() {
    let mut t = T::new(View::Tree);
    let screen = t.screen(80, HEIGHT);
    // The first selectable line, `▾ Private`, is the body's second: row 2.
    assert_eq!(screen.body[1].1, Mark::Selected);
    let buf = drawn(&screen, 80, HEIGHT);
    assert!(row(&buf, 2).contains("Private"));
    for x in 2..79 {
        let cell = &buf[(x, 2)];
        assert_eq!(cell.bg, SEL, "({x}, 2)");
        if cell.symbol() != " " {
            assert_eq!(cell.fg, BRIGHT, "({x}, 2)");
            assert!(
                cell.modifier.contains(Modifier::BOLD),
                "({x}, 2) keeps bold"
            );
        }
    }
    assert_ne!(buf[(1, 2)].bg, SEL);
    assert_ne!(buf[(79, 2)].bg, SEL);
}

#[test]
fn a_waiting_row_is_yellow_and_bold() {
    let buf = T::new(View::Tree).drawn(80, HEIGHT);
    let (x, y) = find(&buf, "! Fix login");
    for x in x..x + 11 {
        let cell = &buf[(x, y)];
        if cell.symbol() != " " {
            assert_eq!(cell.fg, YELLOW, "({x}, {y})");
            assert!(cell.modifier.contains(Modifier::BOLD), "({x}, {y})");
        }
    }
}

#[test]
fn an_overdue_chip_is_dark_on_red() {
    let buf = T::new(View::Plan).drawn(80, HEIGHT);
    let chip = " 1d overdue ";
    let (x, y) = find(&buf, chip);
    let end = x + text::width(chip) as u16;
    // Against the body's right edge, the column before the frame.
    assert_eq!(end, 79);
    for x in x..end {
        let cell = &buf[(x, y)];
        assert_eq!((cell.fg, cell.bg), (DARK, RED), "({x}, {y})");
        assert!(cell.modifier.contains(Modifier::BOLD), "({x}, {y})");
    }
}

#[test]
fn wide_glyphs_take_two_cells() {
    // Tall enough to show the tree's last line, Old spike.
    let h = 24;
    let buf = T::new(View::Tree).drawn(80, h);
    for glyph in ["💤", "👍"] {
        let (x, y) = find(&buf, glyph);
        assert_eq!(buf[(x, y)].symbol(), glyph);
        assert_eq!(buf[(x + 1, y)], Cell::EMPTY, "the cell {glyph} covers");
    }
    let (x, y) = find(&buf, "💤Old spike");
    assert_eq!(buf[(x + 2, y)].symbol(), "O");
    let (_, y) = find(&buf, "Dev server 👍");
    assert!(row(&buf, y).contains("Dev server 👍 "));
    // Every line is 80 cells and ends on the frame's edge in the last column; the age
    // before it is whole.
    for y in 0..h {
        assert_eq!(text::width(&row(&buf, y)), 80, "row {y}");
    }
    for y in 1..h - 1 {
        assert_eq!(buf[(79, y)].symbol(), "│", "row {y}");
    }
    let (_, y) = find(&buf, "💤Old spike");
    assert!(row(&buf, y).ends_with("8h│"), "{:?}", row(&buf, y));
}

/// The query's hits are yellow, bold and underlined (`Tone::Hit`); on the selection bar
/// they keep the bar's colours, bold and underlined, as the spike's did.
#[test]
fn hits_are_yellow_bold_and_underlined() {
    let mut t = T::picker();
    t.panel.paste("kite");
    let buf = t.drawn(80, HEIGHT);
    // The terminal's title is a whole hit; Release notes, selected, hits in its group.
    let (x, y) = find(&buf, "⌨ kite");
    for x in x + 2..x + 6 {
        let cell = &buf[(x, y)];
        assert_eq!(cell.fg, YELLOW, "({x}, {y})");
        assert!(
            cell.modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED),
            "({x}, {y})"
        );
    }
    // The word after it isn't a hit.
    let (x, y) = find(&buf, "terminal");
    assert!(!buf[(x, y)].modifier.contains(Modifier::UNDERLINED));
    let (x, y) = find(&buf, "Private/Kit");
    assert!(row(&buf, y).contains("Release notes"));
    for x in x + 8..x + 11 {
        let cell = &buf[(x, y)];
        assert_eq!((cell.fg, cell.bg), (BRIGHT, SEL), "({x}, {y})");
        assert!(
            cell.modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED),
            "({x}, {y})"
        );
    }
    assert!(!buf[(x, y)].modifier.contains(Modifier::UNDERLINED));
}

/// The head stays above the list as it scrolls: the query line, the dim rule, then rows.
#[test]
fn the_picker_head_stays_as_the_list_scrolls() {
    let mut t = T::picker();
    // Down to the last of the 12 rows (the picker has no End: it is typed nowhere).
    for _ in 0..11 {
        t.key(Key::Down, 80, HEIGHT);
    }
    let buf = t.drawn(80, HEIGHT);
    assert!(row(&buf, 1).starts_with("│ › "), "{}", screen_text(&buf));
    assert!(row(&buf, 1).contains("12/12 · 8 live"));
    assert!(row(&buf, 2).starts_with("│ ───"));
    assert_eq!(buf[(2, 2)].fg, DIM);
    assert!(
        row(&buf, 12).contains("Closed thing"),
        "{}",
        screen_text(&buf)
    );
    assert!(!screen_text(&buf).contains("Fix login"));
}

// ---------- sizes (Review Focus 3) ----------

#[test]
fn tiny_sizes_draw_without_panicking() {
    let outside = Style::new().fg(Color::Indexed(9)).bg(Color::Indexed(4));
    let mut filled = Cell::new("#");
    filled.set_style(outside);
    let more = [
        ("empty-slot", View::EmptySlot),
        ("picker", View::Picker),
        ("help", View::Help),
    ];
    for (name, view) in VIEWS.into_iter().chain(more) {
        for w in 0..=12 {
            for h in 0..=4 {
                let screen = T::new(view).screen(w, h);
                let mut buf = Buffer::filled(Rect::new(0, 0, 30, 12), filled.clone());
                let area = Rect::new(5, 3, w, h);
                draw(&mut buf, area, &screen, &theme());
                for y in 0..12 {
                    for x in 0..30 {
                        if !area.contains((x, y).into()) {
                            assert_eq!(buf[(x, y)], filled, "{name} at {w} × {h}: ({x}, {y})");
                        }
                    }
                }
            }
        }
    }
    // A resize to 3 × 1 with the editor open, then back: the typing is still there.
    let mut t = T::new(View::Tree);
    t.screen(80, HEIGHT);
    t.key(Key::Char('n'), 80, HEIGHT);
    for c in "xyz".chars() {
        t.key(Key::Char(c), 80, HEIGHT);
    }
    let footer = HEIGHT - 2;
    assert!(row(&t.drawn(80, HEIGHT), footer).contains("xyz"));
    t.drawn(3, 1);
    let buf = t.drawn(80, HEIGHT);
    assert!(row(&buf, footer).contains("xyz"), "{}", screen_text(&buf));
    assert!(t.screen(80, HEIGHT).footer.text.ends_with("xyz"));
}
