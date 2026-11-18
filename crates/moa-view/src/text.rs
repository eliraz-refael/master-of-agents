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
/// grapheme holding a ZWJ, a skin-tone modifier or a tag character becomes its first code
/// point; a regional indicator becomes its ASCII capital (a flag `🇮🇱` reads `IL`).
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for g in s.graphemes(true) {
        if g.chars().any(|c| c == ZWJ || is_skin_tone(c) || is_tag(c)) {
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

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
/// A day, in milliseconds.
pub const DAY: i64 = 24 * HOUR;

/// How long ago `from` was, as the spike's picture said it: `""` for none, `now` under a
/// minute (and for the future), then `{m}m`, `{h}h`, `{d}d`.
pub fn ago(from: Option<UtcMs>, now: UtcMs) -> String {
    let Some(from) = from else {
        return String::new();
    };
    let ms = now.0.saturating_sub(from.0);
    if ms < MINUTE {
        "now".to_string()
    } else if ms < HOUR {
        format!("{}m", ms / MINUTE)
    } else if ms < DAY {
        format!("{}h", ms / HOUR)
    } else {
        format!("{}d", ms / DAY)
    }
}

/// The first moment at or after `now` when [`ago`] of `from` reads differently.
pub fn next_ago_change(from: UtcMs, now: UtcMs) -> UtcMs {
    let ms = now.0.saturating_sub(from.0);
    let step = if ms < MINUTE {
        return UtcMs(from.0.saturating_add(MINUTE));
    } else if ms < HOUR {
        MINUTE
    } else if ms < DAY {
        HOUR
    } else {
        DAY
    };
    UtcMs(from.0.saturating_add((ms / step + 1).saturating_mul(step)))
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

/// `path` with the home folder as `~`.
pub fn tilde(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    }
}

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
