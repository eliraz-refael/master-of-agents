//! The palette (spec §6.14), the colour mode, and the 256-colour fallback (V12).

use moa_tui::theme::{ColorMode, Token, detect, nearest_256};

#[test]
fn the_palette_is_exact() {
    let palette = [
        (Token::Text, (0xd8, 0xde, 0xe9)),
        (Token::Bright, (0xec, 0xef, 0xf4)),
        (Token::Dim, (0x8c, 0x9a, 0xb3)),
        (Token::Line, (0x4c, 0x56, 0x6a)),
        (Token::Surface, (0x3b, 0x42, 0x52)),
        (Token::Sel, (0x5e, 0x81, 0xac)),
        (Token::Dark, (0x2e, 0x34, 0x40)),
        (Token::Blue, (0x88, 0xc0, 0xd0)),
        (Token::Cyan, (0x8f, 0xbc, 0xbb)),
        (Token::Green, (0xa3, 0xbe, 0x8c)),
        (Token::Yellow, (0xeb, 0xcb, 0x8b)),
        (Token::Orange, (0xd0, 0x87, 0x70)),
        (Token::Red, (0xd5, 0x77, 0x80)),
        (Token::Magenta, (0xb4, 0x8e, 0xad)),
    ];
    for (token, hex) in palette {
        assert_eq!(token.hex(), hex, "{token:?}");
    }
}

#[test]
fn nearest_256_matches_the_spike() {
    assert_eq!(nearest_256(Token::Text.hex()), 254);
    assert_eq!(nearest_256(Token::Red.hex()), 174);
    assert_eq!(nearest_256(Token::Blue.hex()), 110);
}

#[test]
fn detect_colour_mode() {
    assert_eq!(detect(true, None), ColorMode::True);
    assert_eq!(detect(false, Some("truecolor")), ColorMode::True);
    assert_eq!(detect(false, Some("24bit")), ColorMode::True);
    assert_eq!(detect(false, None), ColorMode::Ansi256);
}
