//! The console's lines (B6): a made-up day of moa's conversation at widths 40, 80 and 120,
//! rows it can't read, the spinner of a running tool, and paged-in rows merged with the
//! topic's.

use moa_core::UtcMs;
use moa_core::panel::View;
use moa_core::publish::{FeedRow, Topic};
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{THREAD_TODAY, console_rows, lantern};
use moa_view::line::{Line, debug_lines};
use moa_view::text::width;
use moa_view::views::console::{Card, ConsoleUi, ConsoleView, build};
use moa_view::views::{self, ViewUi};
use serde_json::{Value, json};

const MINUTE: i64 = 60_000;

/// The fixture with the made-up day in its `feed` topic.
fn day() -> (Data, Ctx) {
    let (mut data, ctx) = lantern();
    console_rows(&mut data);
    (data, ctx)
}

/// The day's panel memory: a card for `/ls` at 09:40 and an error at 09:55.
fn ui(ctx: &Ctx) -> ConsoleUi {
    ConsoleUi {
        cards: vec![Card {
            at: UtcMs(ctx.now.0 - 20 * MINUTE),
            line: "/ls".into(),
            text: "2 sessions need you:\n\n- Fix login\n- Release notes".into(),
        }],
        errors: vec![(
            UtcMs(ctx.now.0 - 5 * MINUTE),
            "moa's agent couldn't start".into(),
        )],
        ..ConsoleUi::default()
    }
}

fn assert_fits(lines: &[Line], w: u16) {
    for line in lines {
        let cells: usize = line.spans.iter().map(|s| width(&s.text)).sum();
        assert!(cells <= usize::from(w), "wider than {w}: {:?}", line.text());
    }
}

fn row(id: i64, kind: &str, body: Value, state: Option<&str>) -> FeedRow {
    FeedRow {
        id,
        thread: THREAD_TODAY.into(),
        at: 1_000 + id,
        kind: kind.into(),
        message_id: None,
        automatic: false,
        body,
        state: state.map(str::to_string),
    }
}

fn only(rows: Vec<FeedRow>, ctx: &Ctx, w: u16) -> ConsoleView {
    let mut data = Data::default();
    for r in rows {
        data.feed.insert(r.id, r);
    }
    data.touch();
    build(&data, ctx, &ConsoleUi::default(), w)
}

/// The day at 40, 80 and 120: yesterday's exchange, the separator, moa's own turn (`· on its
/// own`) with its thought and answer, a plan, five tool calls folded to the last three, a
/// cancelled turn, the session lines folded where they fall (Old spike asleep draws none), the
/// `/ls` card, a failed turn, the error and the tool still running.
#[test]
fn the_day_at_three_widths() {
    let (data, ctx) = day();
    assert_eq!(data.unreadable(), 0);
    for w in [40, 80, 120] {
        let view = build(&data, &ctx, &ui(&ctx), w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("console@{w}"), debug_lines(&view.lines));
    }
    // The buttons: the two session lines', in the order they show.
    let view = build(&data, &ctx, &ui(&ctx), 80);
    let pressed: Vec<(&str, &str)> = view
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.command.as_str()))
        .collect();
    let focus = |n| format!("/focus {}", moa_view::fixture::session(n));
    let (fix_login, rust_scaffold) = (focus(1), focus(2));
    assert_eq!(
        pressed,
        [
            ("↩ answer", fix_login.as_str()),
            ("↩ open", rust_scaffold.as_str())
        ]
    );
}

/// The panel shows it too, in place of the old placeholder, and so does `views::build`.
#[test]
fn the_panel_draws_the_console() {
    let (data, ctx) = day();
    let built = views::build(View::Console, &data, &ctx, &ViewUi::default(), 80);
    assert_eq!(built, build(&data, &ctx, &ConsoleUi::default(), 80).lines);
    assert!(built.iter().any(|l| l.text().starts_with("◆ moa  08:00")));
}

/// A `tool` row whose body is a string, a `plan` with no entries, a kind nobody knows, a text
/// row without its text and a `tool` without a title each draw one dim line naming the id.
#[test]
fn bad_rows_draw_degraded() {
    let (_, ctx) = lantern();
    let rows = vec![
        row(1, "tool", json!("ls -la"), Some("completed")),
        row(2, "plan", json!({"entries": []}), None),
        row(3, "nope", json!({"text": "hi"}), None),
        row(4, "moa", json!({"words": "hi"}), None),
        row(5, "tool", json!({"input": "{}"}), None),
        row(6, "plan", json!({"entries": [{"status": "pending"}]}), None),
    ];
    let view = only(rows, &ctx, 80);
    let want: String = (1..=6)
        .map(|id| format!("[dim]· an entry moa couldn't show (#{id})\n"))
        .collect();
    assert_eq!(debug_lines(&view.lines), want);
    // Inside a turn, behind its gutter, still one line each.
    let mut rows = vec![row(0, "you", json!({"text": "go"}), None)];
    rows.push(row(1, "tool", json!("ls -la"), Some("completed")));
    let view = only(rows, &ctx, 80);
    let text: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert_eq!(
        text,
        [
            "› go",
            "◆ moa  19:00",
            "│ · an entry moa couldn't show (#1)"
        ]
    );
}

/// A tool call that is pending or in progress spins, and its line asks for ticks; a finished,
/// failed or cancelled one doesn't.
#[test]
fn the_thinking_line_animates() {
    let (_, ctx) = lantern();
    let input = json!({"command": "cargo test"}).to_string();
    let body = json!({"title": "Terminal", "kind": "execute", "input": input});
    for (state, animated) in [
        ("pending", true),
        ("in_progress", true),
        ("completed", false),
        ("failed", false),
        ("cancelled", false),
    ] {
        let view = only(vec![row(1, "tool", body.clone(), Some(state))], &ctx, 80);
        assert_eq!(view.lines.len(), 1, "{state}");
        let line = &view.lines[0];
        assert_eq!(line.animated, animated, "{state}: {:?}", line.text());
        assert!(
            line.text().starts_with("⚙ Terminal: cargo test "),
            "{state}"
        );
    }
    let spinning = only(vec![row(1, "tool", body, Some("in_progress"))], &ctx, 80);
    let frame = views::spinner(ctx.tick);
    assert!(spinning.lines[0].text().ends_with(frame));
}

/// Rows paged in and the topic's tail are merged by id: an overlap draws once.
#[test]
fn paged_rows_merge_with_the_tail_once() {
    let (_, ctx) = lantern();
    let text = |id: i64| row(id, "system", json!({"text": format!("line {id}")}), None);
    let mut data = Data::default();
    data.feed = (3..=6).map(|id| (id, text(id))).collect();
    data.touch();
    let ui = ConsoleUi {
        older: (1..=4).map(|id| (id, text(id))).collect(),
        ..ConsoleUi::default()
    };
    let lines: Vec<String> = build(&data, &ctx, &ui, 80)
        .lines
        .iter()
        .map(Line::text)
        .collect();
    let want: Vec<String> = (1..=6).map(|id| format!("· line {id}")).collect();
    assert_eq!(lines, want);
}

/// The `feed` topic's rows land in `Data`: lines by id, events by seq, moa's state and the
/// bar; deletes take each back out; an unknown key is counted unreadable.
#[test]
fn the_feed_topic_decodes() {
    let (mut data, _) = day();
    assert_eq!(data.feed.len(), 19);
    assert_eq!(data.events.len(), 8);
    assert_eq!(
        data.moa.as_ref().map(|m| m.phase.as_str()),
        Some("thinking")
    );
    assert_eq!(data.bar.as_ref().map(|b| b.by.as_str()), Some("default"));
    let keys: Vec<String> = ["#19", "ev:8", "state", "bar"].map(String::from).into();
    data.delete(Topic::Feed, &keys);
    assert!(!data.feed.contains_key(&19) && !data.events.contains_key(&8));
    assert!(data.moa.is_none() && data.bar.is_none());
    data.upsert(Topic::Feed, vec![json!({"key": "what", "x": 1})]);
    assert_eq!(data.unreadable(), 1);
    data.replace(Topic::Feed, Vec::new());
    assert!(data.feed.is_empty() && data.events.is_empty());
    assert_eq!(data.unreadable(), 0);
}
