//! The sessions tree, on the Lantern fixture.

use moa_core::publish::Topic;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, session};
use moa_view::line::{Item, Line, debug_lines};
use moa_view::text;
use moa_view::views::{self, ViewUi};

fn tree(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    views::tree::build(data, ctx, &ViewUi::default(), width)
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

/// The line holding `needle`.
fn line_with<'a>(lines: &'a [Line], needle: &str) -> &'a Line {
    lines
        .iter()
        .find(|l| l.text().contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle:?}:\n{}", debug_lines(lines)))
}

#[test]
fn tree_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("tree_model", debug_lines(&tree(&data, &ctx, 80)));
}

#[test]
fn tree_header_counts() {
    let (data, ctx) = lantern();
    let lines = tree(&data, &ctx, 80);
    assert_eq!(
        lines[0].text(),
        "10 sessions · 8 live · 1 asleep · ⠸ 1 working · 1 waiting on you"
    );
    assert_eq!(lines[0].item, None);
    assert!(lines[0].animated, "its spinner ticks");
}

#[test]
fn groups_nest_with_guides_and_empty_groups_show() {
    let (data, ctx) = lantern();
    let lines = tree(&data, &ctx, 80);
    let starts = [
        "10 sessions",
        "▾ Private  8 · 7 live ⠸",
        "├─ ▾ Lantern  3 · 3 live ⠸",
        "│  ├─ ! Fix login",
        "│  ├─ ⠸ Rust scaffold",
        "│  └─ ◌ Import the whole archive",
        "├─ ▾ Kite  3 · 2 live",
        "│  ├─ ? Release notes",
        "│  ├─ ✓ 設計レビュー",
        "│  └─ ○ Closed thing 👨 ",
        "├─ ▾ Harbor  2 · 2 live",
        "│  ├─ ● Dev server 👍 ",
        "│  └─ ● Hand started",
        "└─ ▾ Ideas  0",
        "▾ Work  1 · 1 live",
        "└─ ▾ Harbor  1 · 1 live",
        "   └─ ✗ Migrate DB",
        "▾ Loose  1",
        "└─ 💤Old spike",
    ];
    let got = texts(&lines);
    assert_eq!(got.len(), starts.len(), "{got:#?}");
    for (line, start) in got.iter().zip(starts) {
        assert!(line.starts_with(start), "{line:?} doesn't start {start:?}");
    }
    // A group line names its node by path, a session line its session.
    assert_eq!(
        lines[2].item,
        Some(Item::Group("Private/Lantern".to_string()))
    );
    assert_eq!(lines[15].item, Some(Item::Group("Work/Harbor".to_string())));
    assert_eq!(lines[17].item, Some(Item::Group("Loose".to_string())));
    assert_eq!(lines[3].item, Some(Item::Session(session(1))));
    assert!(
        !got.iter().any(|l| l.contains("Gone")),
        "archived is left out"
    );
}

#[test]
fn a_folded_node_hides_its_subtree() {
    let (mut data, ctx) = lantern();
    data.brain.as_mut().expect("a brain row").folded =
        vec!["Private/Kite".to_string(), "Loose".to_string()];
    let lines = tree(&data, &ctx, 80);
    let got = texts(&lines);
    assert!(got.iter().any(|l| l.starts_with("├─ ▸ Kite  3 · 2 live")));
    assert!(got.iter().any(|l| l.starts_with("▸ Loose  1")));
    for gone in ["Release notes", "設計レビュー", "Closed thing", "Old spike"] {
        assert!(!got.iter().any(|l| l.contains(gone)), "{gone} still shows");
    }
    assert!(got.iter().any(|l| l.contains("Fix login")));
}

#[test]
fn columns_follow_the_width_left_after_the_guides() {
    let (data, ctx) = lantern();
    let at = |width: u16| line_with(&tree(&data, &ctx, width), "Fix login").text();
    let narrow = at(77);
    assert!(narrow.starts_with("│  ├─ "), "Fix login sits 6 cells deep");
    assert!(narrow.contains("acct- "), "{narrow:?}");
    assert!(!narrow.contains("private/lantern"), "{narrow:?}");
    assert_eq!(text::width(&narrow), 77);

    let wide = at(117);
    assert!(wide.contains("acct- "), "{wide:?}");
    assert!(wide.contains("~/private/lantern"), "{wide:?}");
    assert_eq!(text::width(&wide), 117);

    let sixty = at(57);
    assert!(sixty.contains("acct- ") && !sixty.contains("private/lantern"));

    let forty = at(37);
    assert!(!forty.contains("acct-") && !forty.contains("private/lantern"));
    assert!(forty.ends_with("  1m"), "the age stays: {forty:?}");
}

#[test]
fn asleep_takes_the_glyph_and_its_space() {
    let (mut data, ctx) = lantern();
    // Hand started joins Old spike under Loose, both one level deep.
    let mut row = serde_json::to_value(&data.sessions[&session(8)]).expect("a row");
    row["key"] = serde_json::json!(session(8).to_string());
    row["group_path"] = serde_json::Value::Null;
    data.upsert(Topic::Sessions, vec![row]);
    let lines = tree(&data, &ctx, 80);
    let column = |title: &str| {
        let line = line_with(&lines, title).text();
        let at = line.find(title).expect("the title");
        text::width(&line[..at])
    };
    assert!(
        line_with(&lines, "Old spike")
            .text()
            .contains("💤Old spike")
    );
    assert!(
        line_with(&lines, "Hand started")
            .text()
            .contains("● Hand started")
    );
    assert_eq!(column("Old spike"), column("Hand started"));
}

#[test]
fn a_session_line_animates_and_changes_with_its_age() {
    let (data, ctx) = lantern();
    let lines = tree(&data, &ctx, 80);
    assert!(line_with(&lines, "Rust scaffold").animated);
    assert!(line_with(&lines, "Fix login").animated);
    assert!(!line_with(&lines, "Release notes").animated);
    // Seen a minute ago: "1m" turns "2m" a minute later.
    let seen = data.sessions[&session(1)]
        .session
        .last_seen_at
        .expect("seen");
    assert_eq!(
        line_with(&lines, "Fix login").changes_at,
        Some(moa_core::UtcMs(seen.0 + 2 * 60_000))
    );
}

#[test]
fn no_width_or_title_breaks_a_line() {
    let (mut data, ctx) = lantern();
    let mut row = serde_json::to_value(&data.sessions[&session(1)]).expect("a row");
    row["key"] = serde_json::json!(session(1).to_string());
    row["title"] = serde_json::json!("a\u{1b}[31mred\ttitle");
    row["group_path"] = serde_json::json!("A//B/");
    data.upsert(Topic::Sessions, vec![row]);
    // A group nested past any reason.
    let mut deep = serde_json::to_value(&data.sessions[&session(2)]).expect("a row");
    deep["key"] = serde_json::json!(session(2).to_string());
    deep["group_path"] = serde_json::json!(vec!["g"; 100_000].join("/"));
    data.upsert(Topic::Sessions, vec![deep]);
    for width in [0, 1, 5, 10, 33, 34, 75, 76, 200] {
        for line in tree(&data, &ctx, width) {
            assert!(!line.text().chars().any(char::is_control), "{line:?}");
        }
    }
}
