//! Text from data, cleaned and measured by display width (V9).
//!
//! Every string that comes from data goes through [`clean`] before it is measured or drawn:
//! an agent's title, prompt or why text must never put a control character on the terminal,
//! and a grapheme that tmux and ratatui would measure differently must never reach either.

use std::path::Path;

use jiff::civil::Date;
use moa_core::UtcMs;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The spinner's frames (the spike's); a frame is `SPIN[tick % 10]`.
pub const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Every glyph moa draws itself, with its width in cells. A test keeps `width` equal to it.
pub const CHROME: &[(&str, usize)] = &[
    // states and marks
    ("⚠", 1),
    ("!", 1),
    ("?", 1),
    ("✓", 1),
    ("✗", 1),
    ("◐", 1),
    ("◌", 1),
    ("●", 1),
    ("○", 1),
    ("★", 1),
    ("▸", 1),
    ("▾", 1),
    ("◷", 1),
    ("✦", 1),
    ("⧗", 1),
    ("💤", 2),
    // view labels
    ("◆", 1),
    ("▤", 1),
    ("☀", 1),
    ("≡", 1),
    ("▦", 1),
    ("◉", 1),
    ("◫", 1),
    // keys and punctuation
    ("⌨", 1),
    ("⏎", 1),
    ("↵", 1),
    ("·", 1),
    ("…", 1),
    // the frame
    ("─", 1),
    ("│", 1),
    ("├", 1),
    ("└", 1),
    ("╭", 1),
    ("╮", 1),
    ("╰", 1),
    ("╯", 1),
    // the spinner (`SPIN`)
    ("⠋", 1),
    ("⠙", 1),
    ("⠹", 1),
    ("⠸", 1),
    ("⠼", 1),
    ("⠴", 1),
    ("⠦", 1),
    ("⠧", 1),
    ("⠇", 1),
    ("⠏", 1),
];

const ZWJ: char = '\u{200D}';

fn is_skin_tone(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

fn is_tag(c: char) -> bool {
    ('\u{E0020}'..='\u{E007F}').contains(&c)
}

/// A halfwidth katakana sound mark (U+FF9E, U+FF9F): ratatui measures a grapheme holding one
/// a cell wider than `unicode-width` over the grapheme does.
fn is_halfwidth_mark(c: char) -> bool {
    matches!(c, '\u{FF9E}' | '\u{FF9F}')
}

fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE0E}' | '\u{FE0F}')
}

/// A bidi mark, embedding, override or isolate: it draws nothing, but it can reorder the
/// text drawn after it.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A control character (Cc) or a line or paragraph separator: it breaks the line.
fn is_line_breaking(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// Text with only what could break or reorder the line taken out: a control character or a
/// line or paragraph separator becomes a space and a bidi control is dropped. Everything else
/// (a flag, a skin tone, a variation selector) is kept as typed; [`clean`] is for drawing.
pub fn strip_controls(s: &str) -> String {
    s.chars()
        .filter(|&c| !is_bidi_control(c))
        .map(|c| if is_line_breaking(c) { ' ' } else { c })
        .collect()
}

/// One code point of cleaned text: a control or a line or paragraph separator is a space,
/// a variation selector or a bidi control goes, a regional indicator is its ASCII capital.
fn push_clean(out: &mut String, c: char) {
    if is_line_breaking(c) {
        out.push(' ');
    } else if is_variation_selector(c) || is_bidi_control(c) {
    } else if ('\u{1F1E6}'..='\u{1F1FF}').contains(&c) {
        out.push((b'A' + (c as u32 - 0x1F1E6) as u8) as char);
    } else {
        out.push(c);
    }
}

/// Text safe to draw and measure (V9): control characters (`\n`, `\t`, ESC, DEL, C1) and
/// U+2028/U+2029 become a space; U+FE0E, U+FE0F and the bidi controls are dropped; a
/// grapheme holding a ZWJ, a skin-tone modifier, a tag character or a halfwidth katakana
/// sound mark (U+FF9E, U+FF9F) becomes its first code point; a regional indicator becomes
/// its ASCII capital (a flag `🇮🇱` reads `IL`).
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for g in s.graphemes(true) {
        if g.chars()
            .any(|c| c == ZWJ || is_skin_tone(c) || is_tag(c) || is_halfwidth_mark(c))
        {
            match g.chars().next() {
                // A lone joiner or tag draws nothing.
                Some(c) if c == ZWJ || is_tag(c) => {}
                Some(c) => push_clean(&mut out, c),
                None => {}
            }
        } else {
            for c in g.chars() {
                push_clean(&mut out, c);
            }
        }
    }
    out
}

/// Display width in cells, summed over grapheme clusters.
pub fn width(s: &str) -> usize {
    s.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// The longest prefix of `s` that fits in `w` cells, ending at a grapheme boundary: a wide
/// character that would cross the edge is left out whole.
pub fn cut(s: &str, w: usize) -> String {
    let mut used = 0;
    let mut end = 0;
    for (i, g) in s.grapheme_indices(true) {
        let gw = UnicodeWidthStr::width(g);
        if used + gw > w {
            break;
        }
        used += gw;
        end = i + g.len();
    }
    s[..end].to_string()
}

/// The longest suffix of `s` that fits in `w` cells, starting at a grapheme boundary: where
/// the typing is, for text typed into a line too short for it.
pub fn tail(s: &str, w: usize) -> String {
    let mut used = 0;
    let mut start = s.len();
    for (i, g) in s.grapheme_indices(true).rev() {
        used += UnicodeWidthStr::width(g);
        if used > w {
            break;
        }
        start = i;
    }
    s[start..].to_string()
}

/// `s` as it fits `w` cells: one that is wider is cut at a grapheme boundary, loses the
/// spaces the cut left at its end, and ends in `…`. `w` of 0 leaves nothing.
pub fn ellipsize(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = cut(s, w - 1).trim_end().to_string();
    out.push('…');
    out
}

/// `s` in exactly `w` cells: cut, then padded with spaces on the right.
pub fn fit(s: &str, w: usize) -> String {
    let mut out = cut(s, w);
    let pad = w - width(&out);
    out.extend(std::iter::repeat_n(' ', pad));
    out
}

/// `s` in exactly `w` cells: cut, then padded with spaces on the left.
pub fn fit_right(s: &str, w: usize) -> String {
    let cut = cut(s, w);
    let pad = w - width(&cut);
    let mut out: String = std::iter::repeat_n(' ', pad).collect();
    out.push_str(&cut);
    out
}

/// `s` wrapped at word boundaries to `w` cells, first fit; a word longer than a line is
/// broken. Never empty: no text is one empty line. Leading spaces that don't fit beside the
/// first word go, rather than making an empty first line.
pub fn wrap(s: &str, w: usize) -> Vec<String> {
    let options = textwrap::Options::new(w.max(1)).break_words(true);
    let mut lines: Vec<String> = textwrap::wrap(s, options)
        .into_iter()
        .map(|l| l.into_owned())
        .collect();
    if lines.len() > 1 && lines[0].is_empty() {
        lines.remove(0);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// A day, in milliseconds.
pub use moa_core::time::DAY;
use moa_core::time::{HOUR, MINUTE, span};

/// How long ago `from` was, as the spike's picture said it: `""` for none, `now` under a
/// minute (and for the future), then `{m}m`, `{h}h`, `{d}d`.
pub fn ago(from: Option<UtcMs>, now: UtcMs) -> String {
    match from {
        Some(from) => span(now.0.saturating_sub(from.0)),
        None => String::new(),
    }
}

/// The first moment after `now` when [`ago`] of `from` reads differently. Worked out exactly
/// (in `i128`), so it is always after `now`; a change past the end of time is `i64::MAX`.
pub fn next_ago_change(from: UtcMs, now: UtcMs) -> UtcMs {
    let (from, now) = (i128::from(from.0), i128::from(now.0));
    let ms = now - from;
    let next = if ms < i128::from(MINUTE) {
        from + i128::from(MINUTE)
    } else {
        let step = i128::from(if ms < i128::from(HOUR) {
            MINUTE
        } else if ms < i128::from(DAY) {
            HOUR
        } else {
            DAY
        });
        from + (ms / step + 1) * step
    };
    UtcMs(i64::try_from(next).unwrap_or(i64::MAX))
}

/// Days from `today` to `due`: negative when it is overdue.
pub fn days_left(due: Date, today: Date) -> i64 {
    today
        .until(due)
        .map_or(0, |span| i64::from(span.get_days()))
}

/// A due date in words: `{n}d overdue`, `today`, `tomorrow`, the weekday (`Tue`) under a
/// week, else the day and month (`3 Dec`).
pub fn when(due: Date, today: Date) -> String {
    match days_left(due, today) {
        d if d < 0 => format!("{}d overdue", -d),
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        d if d < 7 => due.strftime("%a").to_string(),
        _ => due.strftime("%-d %b").to_string(),
    }
}

pub use moa_core::folders::tilde;

/// A folder as the spike showed it: the home folder as `~`, and only the last two
/// components when there are more than three.
pub fn short_path(path: &str, home: &Path) -> String {
    let path = tilde(path, home);
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 3 {
        parts[parts.len() - 2..].join("/")
    } else {
        path
    }
}

/// An account name without a leading `claude-` (V16).
pub fn short_account(account: &str) -> &str {
    account.strip_prefix("claude-").unwrap_or(account)
}
