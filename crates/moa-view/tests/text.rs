//! Text: cleaning, display width, cuts, ages and dates.

use std::path::Path;

use jiff::civil::date;
use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::panel::View;
use moa_view::text::{
    CHROME, SPIN, ago, clean, cut, ellipsize, fit, fit_right, next_ago_change, short_account,
    short_path, when, width, wrap,
};
use proptest::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

#[test]
fn clean_replaces_control_characters() {
    assert_eq!(clean("a\x1b[31mb\nc\td\u{7f}"), "a [31mb c d ");
    assert_eq!(clean("x\u{9b}y"), "x y", "a C1 control too");
    assert_eq!(
        clean("a\u{2028}b\u{2029}c"),
        "a b c",
        "line and paragraph separators"
    );
    assert_eq!(
        clean("a\u{202e}b\u{2067}c\u{2069}\u{200f}d"),
        "abcd",
        "bidi controls"
    );
}

#[test]
fn clean_simplifies_emoji_sequences() {
    assert_eq!(clean("⚠️ ok"), "⚠ ok");
    assert_eq!(clean("a\u{fe0e}b"), "ab");
    assert_eq!(clean("👍🏽"), "👍");
    assert_eq!(clean("👨‍👩‍👧"), "👨");
    assert_eq!(clean("🇮🇱"), "IL");
    // A subdivision flag: a black flag and tag characters.
    assert_eq!(
        clean("\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}"),
        "\u{1f3f4}"
    );
    let e = "e\u{301}";
    assert_eq!(clean(e), e);
    assert_eq!(width(e), 1);
    // A halfwidth katakana with its sound mark: ratatui measures it a cell wider than
    // `width`, so it becomes its first code point, one cell wide either way.
    assert_eq!(clean("ｶﾞ"), "ｶ");
    assert_eq!(clean("ﾊﾟ"), "ﾊ");
    assert_eq!(width(&clean("ｶﾞ")), 1);
}

#[test]
fn the_chrome_glyph_set_has_one_width_each() {
    let ones = "⚠ ! ? ✓ ✗ ◐ ◌ ● ○ ★ ▸ ▾ ◷ ✦ ⧗ ◆ ▤ ☀ ≡ ▦ ◉ ◫ ⌨ ⏎ ↵ · … ─ │ ├ └ ╭ ╮ ╰ ╯";
    let mut expected: Vec<(&str, usize)> = ones.split(' ').map(|g| (g, 1)).collect();
    expected.extend(SPIN.iter().map(|g| (*g, 1)));
    expected.push(("💤", 2));
    let mut listed = CHROME.to_vec();
    listed.sort();
    expected.sort();
    assert_eq!(listed, expected);
    for (glyph, w) in CHROME {
        assert_eq!(width(glyph), *w, "{glyph}");
        assert_eq!(clean(glyph), *glyph, "{glyph} is clean");
    }
    // Every glyph moa itself draws is in the set.
    let known = |g: &str| CHROME.iter().any(|(c, _)| *c == g);
    for s in DisplayState::ALL {
        assert!(s.glyph().is_empty() || known(s.glyph()), "{s:?}");
    }
    // A label starts with its glyph, or is the view's word; the picker's and help's are plain
    // words (`find a session`, `keys and help`).
    for v in View::ALL {
        let first = v.label().split(' ').next().unwrap_or("");
        let words = matches!(v, View::Picker | View::Help);
        assert!(first == v.word() || words || known(first), "{v:?}");
    }
}

#[test]
fn fit_cuts_by_display_width() {
    let cases = [
        ("💤 x".to_string(), 5, "💤 x "),
        ("💤 x".to_string(), 3, "💤 "),
        ("設計レビュー".to_string(), 7, "設計レ "),
        (clean("👍🏽 ok"), 6, "👍 ok "),
    ];
    for (s, w, want) in cases {
        assert_eq!(fit(&s, w), want);
        assert_eq!(width(&fit(&s, w)), w);
    }
    assert_eq!(width("💤 x"), 4);
    assert_eq!(cut("設計レビュー", 7), "設計レ");
    assert_eq!(fit_right("設計", 6), "  設計");
    assert_eq!(fit_right("設計レビュー", 5), " 設計");
    assert_eq!(fit("", 0), "");
}

#[test]
fn wrap_never_returns_nothing_and_breaks_long_words() {
    assert_eq!(wrap("", 10), vec![String::new()]);
    assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
    assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    for line in wrap("設計レビュー 設計レビュー", 5) {
        assert!(width(&line) <= 5, "{line}");
    }
    assert!(!wrap("anything", 0).is_empty());
    assert_eq!(
        wrap(" hello world", 5),
        vec!["hello", "world"],
        "no empty first line"
    );
    assert_eq!(wrap("   ", 4), vec![String::new()]);
}

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z ]{1,4}",
        Just("設".to_string()),
        Just("レビュー".to_string()),
        Just("💤".to_string()),
        Just("👍🏽".to_string()),
        Just("👨‍👩‍👧".to_string()),
        Just("⚠️".to_string()),
        Just("🇮🇱".to_string()),
        Just("e\u{301}".to_string()),
    ]
}

proptest! {
    #[test]
    fn fit_never_splits_a_wide_char(
        parts in proptest::collection::vec(piece(), 0..12),
        w in 0usize..40,
    ) {
        let s = clean(&parts.concat());
        let c = cut(&s, w);
        prop_assert!(width(&c) <= w);
        prop_assert!(s.starts_with(&c));
        let boundaries: Vec<usize> = s
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([s.len()])
            .collect();
        prop_assert!(boundaries.contains(&c.len()));
        prop_assert_eq!(width(&fit(&s, w)), w);
        prop_assert_eq!(width(&fit_right(&s, w)), w);
    }
}

#[test]
fn ago_words_and_their_next_change() {
    let now = UtcMs(1_800_000_000_000);
    let back = |ms: i64| UtcMs(now.0 - ms);
    let cases = [
        (30_000, "now", 60_000),
        (5 * 60_000 + 10_000, "5m", 6 * 60_000),
        (3 * 3_600_000, "3h", 4 * 3_600_000),
        (2 * 86_400_000, "2d", 3 * 86_400_000),
    ];
    for (age, word, next_age) in cases {
        let from = back(age);
        assert_eq!(ago(Some(from), now), word);
        let next = next_ago_change(from, now);
        assert_eq!(next.0 - from.0, next_age, "{word}");
        assert_ne!(ago(Some(from), next), word);
        assert_eq!(ago(Some(from), UtcMs(next.0 - 1)), word);
    }
    // 30 s old changes 30 s from now.
    assert_eq!(next_ago_change(back(30_000), now), UtcMs(now.0 + 30_000));
    assert_eq!(ago(None, now), "");
    assert_eq!(ago(Some(UtcMs(now.0 + 5_000)), now), "now", "the future");
    assert_eq!(ago(Some(back(59 * 60_000)), now), "59m");
    assert_eq!(ago(Some(back(60 * 60_000)), now), "1h");
}

#[test]
fn when_words() {
    let today = date(2026, 11, 13);
    assert_eq!(when(date(2026, 11, 11), today), "2d overdue");
    assert_eq!(when(date(2026, 11, 13), today), "today");
    assert_eq!(when(date(2026, 11, 14), today), "tomorrow");
    assert_eq!(when(date(2026, 11, 17), today), "Tue");
    assert_eq!(when(date(2026, 11, 19), today), "Thu");
    assert_eq!(when(date(2026, 11, 20), today), "20 Nov");
    assert_eq!(when(date(2026, 12, 3), today), "3 Dec");
    assert_eq!(moa_view::text::days_left(date(2026, 11, 11), today), -2);
}

#[test]
fn short_paths_and_accounts() {
    let home = Path::new("/h");
    assert_eq!(short_path("/h/private/lantern", home), "~/private/lantern");
    assert_eq!(short_path("/a/b/c/d", home), "c/d");
    assert_eq!(short_path("/h", home), "~");
    assert_eq!(short_path("/hx/a", home), "/hx/a");
    assert_eq!(short_path("/h/a/b/c", home), "b/c");
    assert_eq!(short_account("claude-acct-b"), "acct-b");
    assert_eq!(short_account("acct-a"), "acct-a");
}

#[test]
fn ellipsize_cuts_to_the_width_and_ends_in_an_ellipsis() {
    assert_eq!(ellipsize("short", 5), "short");
    assert_eq!(ellipsize("short", 9), "short");
    assert_eq!(ellipsize("shorter", 0), "");
    assert_eq!(ellipsize("shorter", 1), "…");
    assert_eq!(ellipsize("shorter", 4), "sho…");
    // A wide character at the edge is left out whole, so the line ends a cell short.
    let wide = "漢".repeat(20);
    assert_eq!(ellipsize(&wide, 28), format!("{}…", "漢".repeat(13)));
    assert_eq!(width(&ellipsize(&wide, 28)), 27);
    // The cut doesn't leave a space before the ellipsis.
    assert_eq!(
        ellipsize("moa's layout · Ctrl-a r repairs", 14),
        "moa's layout…"
    );
}
