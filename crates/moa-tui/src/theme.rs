//! moa's palette (spec §6.14) and what each tone of a view is drawn in (V12).
//!
//! The frame is drawn in the terminal's default colour, which tmux sets per pane (blue on the
//! active one), so only it follows focus; everything else has a colour of its own.

use moa_view::line::Tone;
use ratatui::style::{Color, Modifier, Style};

/// A colour of the palette.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Token {
    /// Body text.
    Text,
    /// Emphasis: the selection's text.
    Bright,
    /// Secondary text.
    Dim,
    /// Inactive frames and rules.
    Line,
    /// Cards and pills: the hover mark's background.
    Surface,
    /// The selection bar.
    Sel,
    /// Background accents: the text on a pill.
    Dark,
    Blue,
    Cyan,
    Green,
    Yellow,
    Orange,
    Red,
    Magenta,
}

impl Token {
    /// Its colour, as (red, green, blue): spec §6.14 exactly.
    pub const fn hex(self) -> (u8, u8, u8) {
        match self {
            Token::Text => (0xd8, 0xde, 0xe9),
            Token::Bright => (0xec, 0xef, 0xf4),
            Token::Dim => (0x8c, 0x9a, 0xb3),
            Token::Line => (0x4c, 0x56, 0x6a),
            Token::Surface => (0x3b, 0x42, 0x52),
            Token::Sel => (0x5e, 0x81, 0xac),
            Token::Dark => (0x2e, 0x34, 0x40),
            Token::Blue => (0x88, 0xc0, 0xd0),
            Token::Cyan => (0x8f, 0xbc, 0xbb),
            Token::Green => (0xa3, 0xbe, 0x8c),
            Token::Yellow => (0xeb, 0xcb, 0x8b),
            Token::Orange => (0xd0, 0x87, 0x70),
            Token::Red => (0xd5, 0x77, 0x80),
            Token::Magenta => (0xb4, 0x8e, 0xad),
        }
    }
}

/// How colours reach the terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    /// 24-bit: the palette exactly.
    True,
    /// The nearest xterm-256 colour of each token.
    Ansi256,
}

/// The colour mode: 24-bit inside tmux (it converts for clients without RGB) or when
/// `COLORTERM` is `truecolor` or `24bit`; else 256 colours.
pub fn detect(in_tmux: bool, colorterm: Option<&str>) -> ColorMode {
    let truecolor = colorterm.is_some_and(|c| {
        let c = c.trim();
        c.eq_ignore_ascii_case("truecolor") || c.eq_ignore_ascii_case("24bit")
    });
    if in_tmux || truecolor {
        ColorMode::True
    } else {
        ColorMode::Ansi256
    }
}

/// The 6 × 6 × 6 cube's steps.
const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest xterm-256 colour to `rgb`, from the cube or the grey ramp (the spike's
/// `xterm256`; a tie goes to the first candidate, and to the cube between the two).
pub fn nearest_256(rgb: (u8, u8, u8)) -> u8 {
    let (r, g, b) = rgb;
    let nearest_step = |c: u8| {
        let mut best = 0;
        for (i, step) in STEPS.iter().enumerate() {
            if step.abs_diff(c) < STEPS[best].abs_diff(c) {
                best = i;
            }
        }
        best
    };
    let cube = [nearest_step(r), nearest_step(g), nearest_step(b)];
    let cube_rgb = cube.map(|i| STEPS[i]);
    // The grey nearest the channels' mean; `3 × level` against the sum keeps it exact.
    let sum = i32::from(r) + i32::from(g) + i32::from(b);
    let level = |i: i32| 8 + 10 * i;
    let mut grey = 0;
    for i in 1..24 {
        if (3 * level(i) - sum).abs() < (3 * level(grey) - sum).abs() {
            grey = i;
        }
    }
    let grey_v = u8::try_from(level(grey)).expect("a grey level fits a byte");
    let dist = |c: [u8; 3]| -> i32 {
        c.iter()
            .zip([r, g, b])
            .map(|(x, y)| (i32::from(*x) - i32::from(y)).pow(2))
            .sum()
    };
    if dist([grey_v; 3]) < dist(cube_rgb) {
        232 + u8::try_from(grey).expect("a grey index fits a byte")
    } else {
        let [r, g, b] = cube.map(|i| u8::try_from(i).expect("a cube index fits a byte"));
        16 + 36 * r + 6 * g + b
    }
}

/// The colours a panel draws in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub mode: ColorMode,
}

impl Theme {
    /// A token in this theme's mode.
    pub fn color(&self, token: Token) -> Color {
        let (r, g, b) = token.hex();
        match self.mode {
            ColorMode::True => Color::Rgb(r, g, b),
            ColorMode::Ansi256 => Color::Indexed(nearest_256((r, g, b))),
        }
    }

    /// A tone's colours (V12), in bold when `bold`: the view decides what is bold.
    pub fn style(&self, tone: Tone, bold: bool) -> Style {
        let fg = |token| Style::new().fg(self.color(token));
        let pill = |token| fg(Token::Dark).bg(self.color(token));
        let style = match tone {
            Tone::Frame => Style::new().fg(Color::Reset),
            Tone::Text => fg(Token::Text),
            Tone::Dim => fg(Token::Dim),
            Tone::Head | Tone::Group | Tone::Work | Tone::Soon => fg(Token::Blue),
            Tone::Warn | Tone::You => fg(Token::Yellow),
            Tone::Ask => fg(Token::Orange),
            Tone::Ok => fg(Token::Green),
            Tone::Fail | Tone::P0 => fg(Token::Red),
            Tone::Quiet => fg(Token::Cyan),
            Tone::Brain => fg(Token::Magenta),
            Tone::Late => pill(Token::Red),
            Tone::Today => pill(Token::Yellow),
        };
        if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }

    /// The selection bar: `bright` on `sel`.
    pub fn selected(&self) -> Style {
        Style::new()
            .fg(self.color(Token::Bright))
            .bg(self.color(Token::Sel))
    }

    /// The hover mark (V11): the `surface` background only, so the row keeps its colours.
    pub fn hovered(&self) -> Style {
        Style::new().bg(self.color(Token::Surface))
    }
}
