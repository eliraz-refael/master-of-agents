//! moa's answers as lines (B12): each Markdown construct at widths 40 and 80, hanging
//! indents, tables that fit and don't, buttons that never split, and no line wider than its
//! width for any Markdown.

use moa_view::line::{Button, Line, debug_lines};
use moa_view::markdown::render;
use moa_view::text::width;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

/// No code is a button.
fn no_buttons(_: &str) -> Option<Button> {
    None
}

/// Every code span is a button showing its text and pressing `/<text>`.
fn every_code(code: &str) -> Option<Button> {
    Some(Button {
        label: code.to_string(),
        command: format!("/{code}"),
    })
}

/// A command (`/…`) is a button; other code stays code.
fn commands(code: &str) -> Option<Button> {
    code.starts_with('/').then(|| Button {
        label: code.to_string(),
        command: code.to_string(),
    })
}

fn lines_at(md: &str, width: u16, find: &dyn Fn(&str) -> Option<Button>) -> Vec<Line> {
    render(md, width, &mut Vec::new(), find)
}

/// The lines' model at widths 40 and 80, one after the other.
fn both(md: &str) -> String {
    let mut out = String::new();
    for w in [40, 80] {
        out.push_str(&format!("── {w} ──\n"));
        out.push_str(&debug_lines(&lines_at(md, w, &commands)));
    }
    out
}

/// A line's width: its spans' widths summed, as a terminal draws them span by span.
fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|s| width(&s.text)).sum()
}

fn assert_fits(lines: &[Line], w: u16) {
    for line in lines {
        assert!(
            line_width(line) <= usize::from(w) && width(&line.text()) <= usize::from(w),
            "wider than {w}: {:?}",
            line.text()
        );
    }
}

#[test]
fn headings() {
    insta::assert_snapshot!(both(
        "# The plan for Lantern today, with a heading long enough to wrap\n\n\
         ## Kite\n\n### Harbor `/brief`\n\nText under it."
    ));
}

#[test]
fn bullets_by_depth() {
    insta::assert_snapshot!(both(
        "- one\n  - two\n    - three\n      - four\n- back out"
    ));
}

#[test]
fn numbered() {
    insta::assert_snapshot!(both(
        "3. three\n4. four, a longer item that wraps under its own text at forty\n\n\
         and then\n\n1. nine\n2. ten\n3. x\n4. x\n5. x\n6. x\n7. x\n8. x\n9. x\n10. ten"
    ));
}

#[test]
fn task_boxes() {
    insta::assert_snapshot!(both(
        "- [x] ship the scaffold for Lantern\n- [ ] review Kite\n  - [ ] nested"
    ));
}

#[test]
fn quotes() {
    insta::assert_snapshot!(both(
        "> a quote long enough to wrap at forty columns, **bold** inside\n>\n> > nested\n\n\
         after"
    ));
}

#[test]
fn fenced_code() {
    insta::assert_snapshot!(both(
        "```rust\nfn main() {\n\tprintln!(\"a line far wider than forty columns, cut and never wrapped\");\n}\n```\n\
         after\n\n    indented code"
    ));
}

#[test]
fn rules() {
    insta::assert_snapshot!(both("above\n\n---\n\nbelow\n\n- in a list\n\n  ***"));
}

#[test]
fn inline_styles() {
    insta::assert_snapshot!(both(
        "**bold** and ~~struck~~ and *emphasis* and `inline code` and \
         [a link](https://example.com) and ![alt text](x.png) and <b>html</b>"
    ));
}

#[test]
fn html_blocks() {
    insta::assert_snapshot!(both("<div>\nfirst\nsecond\n</div>\n\nafter"));
}

#[test]
fn coloured_glyphs() {
    insta::assert_snapshot!(both("✓ built ✗ failed ⚠ careful ● working **✓ bold**"));
}

#[test]
fn buttons() {
    insta::assert_snapshot!(both(
        "Run `/brief` or `/attention`, not `moactl ls --all`.\n\n- see `/ls`"
    ));
}

#[test]
fn hanging_indents() {
    let lines = lines_at(
        "- a bullet long enough to wrap twice at forty columns, under its text\n\
         - [ ] a task long enough to wrap twice at forty columns, under its text\n\
         12. a numbered item long enough to wrap at forty columns, under its text",
        40,
        &no_buttons,
    );
    insta::assert_snapshot!(debug_lines(&lines));
    let texts: Vec<String> = lines.iter().map(Line::text).collect();
    assert!(texts[0].starts_with("• a bullet"), "{texts:?}");
    assert!(texts[1].starts_with("  ") && !texts[1].starts_with("   "));
    let task = texts.iter().position(|t| t.starts_with("☐ ")).unwrap();
    assert!(texts[task + 1].starts_with("  ") && !texts[task + 1].starts_with("   "));
    let numbered = texts.iter().position(|t| t.starts_with("12. ")).unwrap();
    assert!(texts[numbered + 1].starts_with("    ") && !texts[numbered + 1].starts_with("     "));
    assert!(lines[1].more && !lines[0].more);
}

const TABLE: &str = "| Project | State | Next |\n|---|---|---|\n\
                     | Lantern | ✓ green, all checks | ship it, then `/brief` |\n\
                     | Kite | ⚠ waiting on you | review |";

#[test]
fn a_wide_table_becomes_bullets() {
    let lines = lines_at(TABLE, 40, &commands);
    insta::assert_snapshot!(debug_lines(&lines));
    assert!(lines[0].text().starts_with("• Project · State · Next"));
    assert!(lines.iter().all(|l| !l.text().contains('┌')));
}

#[test]
fn a_fitting_table_is_boxed() {
    let lines = lines_at(TABLE, 80, &commands);
    insta::assert_snapshot!(debug_lines(&lines));
    assert!(lines[0].text().starts_with('┌'));
    assert!(lines.last().unwrap().text().starts_with('└'));
    let wide: Vec<usize> = lines.iter().map(line_width).collect();
    assert!(wide.windows(2).all(|w| w[0] == w[1]), "{wide:?}");
}

#[test]
fn buttons_never_split() {
    let md = "Press `/brief`, `/attention` or `/ls` now, then `/help` and `/brief` again; \
              `/ls`,`/help`.\n\n- `/attention` `/brief` `/ls`\n  - `/help` in a nested `/ls`\n\n\
              > quoted `/brief` and `/ls`\n\n# heading `/ls`";
    for w in 10..=60u16 {
        let mut buttons = Vec::new();
        let lines = render(md, w, &mut buttons, &every_code);
        assert_fits(&lines, w);
        assert!(!buttons.is_empty());
        for (i, button) in buttons.iter().enumerate() {
            let spans: Vec<_> = lines
                .iter()
                .flat_map(|l| &l.spans)
                .filter(|s| s.button == Some(i as u16))
                .collect();
            assert_eq!(spans.len(), 1, "button {i} at {w}: {spans:?}");
            // Whole whenever it fits beside the deepest indent here (4 cells); cut only
            // when it may not.
            let whole = format!("[{}]", button.label);
            let cut = spans[0].text.ends_with("…]") && width(&whole) > usize::from(w) - 4;
            assert!(spans[0].text == whole || cut, "at {w}: {:?}", spans[0].text);
        }
    }
    // Wider than the whole line, it's cut with `…` and keeps its brackets.
    let mut buttons = Vec::new();
    let lines = render("`/a-very-long-command-name`", 12, &mut buttons, &every_code);
    assert_eq!(debug_lines(&lines), "[work#0][/a-very-l…]\n");
    assert_eq!(buttons[0].label, "/a-very-long-command-name");
}

#[test]
fn an_unterminated_fence_is_code_to_the_end() {
    let lines = lines_at(
        "before\n\n```\ncode\n# not a heading\n- not a list",
        40,
        &no_buttons,
    );
    insta::assert_snapshot!(debug_lines(&lines));
    let texts: Vec<String> = lines.iter().map(Line::text).collect();
    assert_eq!(
        texts,
        ["before", "", "code", "# not a heading", "- not a list"]
    );
}

#[test]
fn data_text_is_cleaned() {
    let lines = lines_at("a\u{1b}[31m b\u{202e}c `x\u{7f}`", 40, &no_buttons);
    assert_eq!(lines[0].text(), "a [31m bc x");
    let lines = lines_at("```\nx\u{1b}y\u{200f}z\n```", 40, &no_buttons);
    assert_eq!(lines[0].text(), "x yz");
}

/// A piece of text that has broken terminals or measured wrong.
fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9 /.*_`~\\[\\]()|#>!-]{0,12}",
        2 => select(vec![
            "設計レビュー".to_string(),
            "👨‍👩‍👧".to_string(),
            "👍🏽".to_string(),
            "⚠️".to_string(),
            "🇮🇱".to_string(),
            "שלום עולם".to_string(),
            "مرحبا".to_string(),
            "\u{202e}rtl\u{202c}".to_string(),
            "\u{200d}".to_string(),
            "e\u{301}".to_string(),
            "\t".to_string(),
            "\u{1b}[1m".to_string(),
            "`/brief`".to_string(),
            "`moactl focus x`".to_string(),
            "**bold**".to_string(),
            "~~gone~~".to_string(),
            "[link](u)".to_string(),
            "![img](u)".to_string(),
            "<br>".to_string(),
            " ".to_string(),
            "✓ ✗ ⚠ ●".to_string(),
            "a_very_long_word_with_no_spaces_at_all_that_is_wider_than_forty_columns".to_string(),
        ]),
    ]
}

fn words() -> impl Strategy<Value = String> {
    vec(piece(), 0..8).prop_map(|parts| parts.concat())
}

/// One block of Markdown.
fn block() -> impl Strategy<Value = String> {
    prop_oneof![
        words(),
        (1..=6usize, words()).prop_map(|(n, t)| format!("{} {t}", "#".repeat(n))),
        // a list item up to 8 deep, bulleted, numbered or a task
        (0..8usize, 0..4usize, words()).prop_map(|(depth, kind, t)| {
            let marker = ["-", "1.", "- [ ]", "- [x]"][kind];
            format!("{}{marker} {t}", "    ".repeat(depth))
        }),
        (1..5usize, 1..4usize, words()).prop_map(|(n, cols, t)| {
            let row = |c: &str| format!("|{}", format!(" {c} |").repeat(cols));
            let mut out = format!("{}\n|{}\n", row("h"), "---|".repeat(cols));
            for _ in 0..n {
                out.push_str(&row(&t));
                out.push('\n');
            }
            out
        }),
        (words(), any::<bool>()).prop_map(|(t, closed)| {
            format!("```\n{t}\n{t}{t}{t}\n{}", if closed { "```" } else { "" })
        }),
        (1..4usize, words()).prop_map(|(n, t)| format!("{} {t}", ">".repeat(n))),
        Just("---".to_string()),
        Just(String::new()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any Markdown at any width from 40 to 200: no panic, no line wider than the width,
    /// and every button's index points at one of the buttons.
    #[test]
    fn markdown_never_overflows(
        blocks in vec(block(), 0..12),
        joins in vec(select(vec!["\n", "\n\n"]), 12),
        w in 40..=200u16,
    ) {
        let md: String = blocks
            .iter()
            .zip(&joins)
            .flat_map(|(b, j)| [b.as_str(), j])
            .collect();
        let mut buttons = Vec::new();
        let lines = render(&md, w, &mut buttons, &every_code);
        assert_fits(&lines, w);
        for span in lines.iter().flat_map(|l| &l.spans) {
            prop_assert!(span.button.is_none_or(|b| usize::from(b) < buttons.len()));
            prop_assert!(!span.text.chars().any(char::is_control), "{:?}", span.text);
        }
    }
}

/// A 10,000-line answer, a list 8 deep and a wide table, at width 40.
#[test]
fn odd_answers_fit() {
    let long = "a line of moa's answer with `/brief` in it\n".repeat(10_000);
    let deep: String = (0..8)
        .map(|d| format!("{}10. deep item {d} with some words\n", "    ".repeat(d)))
        .collect();
    let wide = format!(
        "| {} |\n|{}\n| x |\n",
        ["column"; 12].join(" | "),
        "---|".repeat(12)
    );
    for md in [long.as_str(), &deep, &wide] {
        let mut buttons = Vec::new();
        let lines = render(md, 40, &mut buttons, &every_code);
        assert_fits(&lines, 40);
        assert!(!lines.is_empty());
    }
}
