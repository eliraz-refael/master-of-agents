//! The console's lines (B6): a made-up day of moa's conversation at widths 40, 80 and 120,
//! rows it can't read, the spinner of a running tool, and paged-in rows merged with the
//! topic's.

use std::collections::BTreeSet;

use moa_core::UtcMs;
use moa_core::model::{Suggestion, SuggestionAction, SuggestionId, SuggestionStatus};
use moa_core::panel::View;
use moa_core::publish::{EventRow, FeedRow, Topic};
use moa_core::state::EventWhat;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{THREAD_TODAY, console_rows, lantern, permission_rows, session};
use moa_view::line::{Line, debug_lines};
use moa_view::text::width;
use moa_view::views::console::{Card, ConsoleUi, ConsoleView, build, button_at};
use moa_view::views::{self, ViewUi};
use serde_json::{Value, json};

const MINUTE: i64 = 60_000;

fn s(text: &str) -> String {
    text.to_string()
}

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
    build(&data, ctx, &ConsoleUi::default(), true, w)
}

/// The day at 40, 80 and 120: yesterday's exchange, the separator, moa's wake-up (`· on its
/// own`, reading 3 session reports) with its thought and answer, a plan, five tool calls
/// folded to the last three, a cancelled turn, the session lines folded where they fall (Old
/// spike asleep draws none), the `/ls` card, a failed turn, the error, and the prompt that
/// carried 2 session reports with its tool still running.
#[test]
fn the_day_at_three_widths() {
    let (data, ctx) = day();
    assert_eq!(data.unreadable(), 0);
    for w in [40, 80, 120] {
        let view = build(&data, &ctx, &ui(&ctx), true, w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("console@{w}"), debug_lines(&view.lines));
    }
    // The buttons: the Markdown's two commands, the two session lines', then the bar's.
    let view = build(&data, &ctx, &ui(&ctx), true, 80);
    let pressed: Vec<(&str, &str)> = view
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.command.as_str()))
        .collect();
    let focus = |n| format!("/focus {}", session(n));
    let (fix_login, rust_scaffold) = (focus(1), focus(2));
    assert_eq!(
        pressed,
        [
            ("/brief", "/brief"),
            ("/attention", "/attention"),
            ("↩ answer", fix_login.as_str()),
            ("↩ open", rust_scaffold.as_str()),
            // The bar's, after the text's.
            ("/brief", "/brief"),
            ("/attention", "/attention"),
            ("/ls", "/ls"),
            ("/help", "/help"),
        ]
    );
}

/// The panel shows it too, in place of the old placeholder, and so does `views::build`.
#[test]
fn the_panel_draws_the_console() {
    let (data, ctx) = day();
    let built = views::build(View::Console, &data, &ctx, &ViewUi::default(), 80);
    assert_eq!(
        built,
        build(&data, &ctx, &ConsoleUi::default(), true, 80).lines
    );
    assert!(built.iter().any(|l| l.text().starts_with("◆ moa  08:00")));
}

/// A `tool` row whose body is a string, a `plan` with no entries, a kind nobody knows, a text
/// row without its text and a `tool` without a title each draw one dim line naming the id,
/// behind the gutter of the turn they open.
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
        .map(|id| format!("[dim]│ [dim]· an entry moa couldn't show (#{id})\n"))
        .collect();
    assert_eq!(
        debug_lines(&view.lines),
        format!("[brain*]◆ moa  19:00\n{want}")
    );
    // After a prompt, still one line each.
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
        assert_eq!(view.lines.len(), 2, "{state}: the header, then the call");
        assert!(!view.lines[0].animated);
        let line = &view.lines[1];
        assert_eq!(line.animated, animated, "{state}: {:?}", line.text());
        assert!(
            line.text().starts_with("│ ⚙ Terminal: cargo test "),
            "{state}"
        );
    }
    let spinning = only(vec![row(1, "tool", body, Some("in_progress"))], &ctx, 80);
    let frame = views::spinner(ctx.tick);
    assert!(spinning.lines[1].text().ends_with(frame));
}

/// moa's rows with no prompt before them (moa speaking first, or the edge of what is loaded)
/// still read as a turn: a header, then the gutter. A thought that is only blank lines draws
/// nothing.
#[test]
fn rows_without_a_prompt_open_a_turn() {
    let (_, ctx) = lantern();
    let rows = vec![
        row(1, "thought", json!({"text": "\n  \n\t\n"}), None),
        row(
            2,
            "moa",
            json!({"text": "Morning. **Fix login** needs you."}),
            None,
        ),
        row(3, "you", json!({"text": "on it"}), None),
    ];
    let text: Vec<String> = only(rows, &ctx, 80).lines.iter().map(Line::text).collect();
    assert_eq!(
        text,
        [
            "◆ moa  19:00",
            "│ Morning. Fix login needs you.",
            "",
            "› on it"
        ]
    );
}

/// A session line whose time falls inside a turn waits for the turn to end, and then shows
/// right after it; its last line shows only under needs-you and finished.
#[test]
fn session_lines_never_split_a_turn() {
    let (mut data, ctx) = lantern();
    let rows = [
        row(1, "you", json!({"text": "go"}), None),
        row(2, "moa", json!({"text": "first"}), None),
        row(4, "moa", json!({"text": "second"}), None),
        row(6, "you", json!({"text": "next"}), None),
    ];
    data.feed = rows.into_iter().map(|r| (r.id, r)).collect();
    let event = |seq: u64, n: u8, what: EventWhat| EventRow {
        seq,
        at: 1_003,
        session: session(n),
        what,
    };
    data.events.insert(1, event(1, 1, EventWhat::NeedsYou));
    data.events.insert(2, event(2, 6, EventWhat::Started));
    data.touch();
    let text: Vec<String> = build(&data, &ctx, &ConsoleUi::default(), true, 80)
        .lines
        .iter()
        .map(Line::text)
        .collect();
    assert_eq!(
        text,
        [
            "› go",
            "◆ moa  19:00",
            "│ first",
            "│ second",
            "",
            "⚠ Fix login needs you [↩ answer]",
            "  Bash: cargo test ⚠",
            "▶ Migrate DB started",
            "",
            "› next",
        ]
    );
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
    let lines: Vec<String> = build(&data, &ctx, &ui, true, 80)
        .lines
        .iter()
        .map(Line::text)
        .collect();
    let mut want = vec![s("◆ moa  19:00")];
    want.extend((1..=6).map(|id| format!("│ · line {id}")));
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

/// moa's one answer, `text`, over the fixture's sessions, with the bar's commands.
fn says(text: &str, bar: &[&str], ui: &ConsoleUi, w: u16) -> (Data, ConsoleView) {
    let (mut data, ctx) = lantern();
    let rows = [row(1, "moa", json!({ "text": text }), None)];
    data.feed = rows.into_iter().map(|r| (r.id, r)).collect();
    data.bar = Some(moa_core::publish::BarRow {
        commands: bar.iter().map(|c| s(c)).collect(),
        by: s("you"),
    });
    data.touch();
    let view = build(&data, &ctx, ui, true, w);
    (data, view)
}

fn pressed(view: &ConsoleView) -> Vec<(&str, &str)> {
    view.buttons
        .iter()
        .map(|b| (b.label.as_str(), b.command.as_str()))
        .collect()
}

/// A command moa names in code is a button; a session's `moactl` line is the session's name;
/// other code stays code.
#[test]
fn commands_in_text_become_buttons() {
    let rust = session(2).to_string();
    let text = format!(
        "Try `/brief`, then `moactl focus {rust}`; `moactl ls --all` and `/nope` and \
         `moactl focus --all` stay code."
    );
    let (_, view) = says(&text, &[], &ConsoleUi::default(), 100);
    let focus = format!("/focus {rust}");
    assert_eq!(
        pressed(&view),
        [
            ("/brief", "/brief"),
            ("↩ Lantern · Rust scaffold", focus.as_str())
        ]
    );
    let line = &view.lines[1];
    assert!(line.text().contains("[/brief]"), "{}", line.text());
    assert!(line.text().contains("[↩ Lantern · Rust scaffold]"));
    for code in ["moactl ls --all", "/nope", "moactl focus --all"] {
        assert!(line.text().contains(code), "{code} stays: {}", line.text());
        assert!(!line.text().contains(&format!("[{code}]")), "{code}");
    }
    assert_eq!(view.buttons.len(), 2);
}

/// A session button is built from the sessions every time: a rename shows at once.
#[test]
fn session_buttons_follow_renames() {
    let rust = session(2);
    let text = format!("`moactl resume {rust}`");
    let (mut data, view) = says(&text, &[], &ConsoleUi::default(), 80);
    assert_eq!(view.buttons[0].label, "↩ Lantern · Rust scaffold");
    data.sessions.get_mut(&rust).expect("there").session.title = Some(s("Renamed"));
    data.touch();
    let (_, ctx) = lantern();
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    assert_eq!(view.buttons[0].label, "↩ Lantern · Renamed");
    assert_eq!(view.buttons[0].command, format!("/resume {rust}"));
}

/// A session nobody knows shows its id, and a press still sends the command, so the reply's
/// refusal shows in the footer.
#[test]
fn an_unknown_session_button_says_why() {
    let id = "01930000-0000-7000-8000-00000000dead";
    let (_, view) = says(
        &format!("`moactl focus {id}`"),
        &[],
        &ConsoleUi::default(),
        80,
    );
    assert_eq!(
        pressed(&view),
        [(format!("↩ {id}").as_str(), format!("/focus {id}").as_str())]
    );
}

/// A title with a ZWJ sequence, right-to-left text and a wide glyph, at width 40: the button
/// is cut to the width, whole, and not split.
#[test]
fn odd_text_in_buttons() {
    let rust = session(2);
    let (mut data, ctx) = lantern();
    let title = "👩‍👩‍👧 שלום עולם 日本語のとても長いタイトル and more words to fill it";
    data.sessions.get_mut(&rust).expect("there").session.title = Some(s(title));
    let rows = [row(
        1,
        "moa",
        json!({"text": format!("go `moactl focus {rust}` now")}),
        None,
    )];
    data.feed = rows.into_iter().map(|r| (r.id, r)).collect();
    data.touch();
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 40);
    assert_fits(&view.lines, 40);
    let spans: Vec<_> = view
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .filter(|s| s.button.is_some())
        .collect();
    assert_eq!(spans.len(), 1, "one span, not split");
    assert!(spans[0].text.starts_with("[↩ Lantern"));
    assert!(width(&spans[0].text) <= 40);
}

/// A click on a button's cells finds it; a cell beside it finds nothing.
#[test]
fn a_click_finds_the_button_under_it() {
    let (_, view) = says("go `/brief` now", &[], &ConsoleUi::default(), 80);
    let line = &view.lines[1];
    // `│ go [/brief] now`: the button covers columns 5..=12.
    let text = line.text();
    let at = text.find('[').expect("a button");
    let col = width(&text[..at]);
    for c in col..col + width("[/brief]") {
        assert_eq!(button_at(line, c), Some(0), "column {c}");
    }
    assert_eq!(button_at(line, col - 1), None);
    assert_eq!(button_at(line, col + width("[/brief]")), None);
    assert_eq!(button_at(line, 200), None);
}

/// The bar draws as buttons, wraps, and comes after the text's buttons in the numbering.
#[test]
fn the_bar_draws_as_buttons() {
    let bar = ["/brief", "/attention", "/ls", "/help"];
    let (_, view) = says("go `/new`", &bar, &ConsoleUi::default(), 80);
    assert_eq!(view.bar.len(), 1);
    assert_eq!(view.bar[0].text(), "[/brief] [/attention] [/ls] [/help]");
    let all: Vec<&str> = view.buttons.iter().map(|b| b.command.as_str()).collect();
    assert_eq!(all, ["/new", "/brief", "/attention", "/ls", "/help"]);
    let (_, narrow) = says("hi", &bar, &ConsoleUi::default(), 20);
    assert_eq!(narrow.bar.len(), 3);
    assert_fits(&narrow.bar, 20);
    assert!(says("hi", &[], &ConsoleUi::default(), 20).1.bar.is_empty());
}

/// Tab's order (F26): the visible text buttons in reading order, then the bar's.
#[test]
fn the_tab_order_is_reading_order() {
    let rust = session(2);
    let text = format!("First `/ls` and `/brief`.\n\nThen `moactl focus {rust}`.");
    let (_, view) = says(&text, &["/help", "/attention"], &ConsoleUi::default(), 80);
    let command = |i: &u16| view.buttons[usize::from(*i)].command.clone();
    let order: Vec<String> = view
        .order(0..view.lines.len())
        .iter()
        .map(command)
        .collect();
    assert_eq!(
        order,
        [
            "/ls",
            "/brief",
            &format!("/focus {rust}"),
            "/help",
            "/attention"
        ]
    );
    // Only the visible lines count: the last line alone has the session's button.
    let last = view.lines.len() - 1;
    let order: Vec<String> = view.order(last..last + 5).iter().map(command).collect();
    assert_eq!(
        order,
        [format!("/focus {rust}"), s("/help"), s("/attention")]
    );
    // Nothing visible: the bar's first.
    assert_eq!(view.order(0..0).first().map(command), Some(s("/help")));
}

/// The focused button is drawn reversed, text's or bar's, and exactly one: a text `/ls` and a
/// bar `/ls` are two buttons.
#[test]
fn the_focused_button_is_reversed() {
    let reversed = |view: &ConsoleView| -> Vec<(usize, String)> {
        view.lines
            .iter()
            .chain(&view.bar)
            .flat_map(|l| &l.spans)
            .filter(|s| s.reversed)
            .map(|s| (usize::from(s.button.expect("a button")), s.text.clone()))
            .collect()
    };
    for focus in [0u16, 1, 2] {
        let ui = ConsoleUi {
            focus: Some(focus),
            ..ConsoleUi::default()
        };
        let (_, view) = says("go `/ls`", &["/ls", "/help"], &ui, 80);
        // Button 0 is the text's `/ls`, 1 the bar's `/ls`, 2 the bar's `/help`.
        assert_eq!(reversed(&view).len(), 1, "focus {focus}");
        assert_eq!(reversed(&view)[0].0, usize::from(focus));
    }
    let (_, view) = says("go `/ls`", &["/ls"], &ConsoleUi::default(), 80);
    assert!(reversed(&view).is_empty());
}

/// Cards and the console's own errors never split a turn either: one timed inside a turn is
/// drawn right after it, in time order.
#[test]
fn cards_and_errors_never_split_a_turn() {
    let (_, ctx) = lantern();
    let rows = vec![
        row(1, "you", json!({"text": "go"}), None),
        row(2, "moa", json!({"text": "first"}), None),
        row(4, "moa", json!({"text": "second"}), None),
        row(6, "you", json!({"text": "next"}), None),
    ];
    let mut data = Data::default();
    data.feed = rows.into_iter().map(|r| (r.id, r)).collect();
    data.touch();
    let ui = ConsoleUi {
        cards: vec![Card {
            at: UtcMs(1_002),
            line: s("/ls"),
            text: s("nothing"),
        }],
        errors: vec![(UtcMs(1_003), s("it broke"))],
        ..ConsoleUi::default()
    };
    let text: Vec<String> = build(&data, &ctx, &ui, true, 80)
        .lines
        .iter()
        .map(Line::text)
        .collect();
    assert_eq!(
        text,
        [
            "› go",
            "◆ moa  19:00",
            "│ first",
            "│ second",
            "",
            "▸ /ls",
            "┊ nothing",
            "",
            "✗ it broke",
            "",
            "› next",
        ]
    );
}

/// B9: the head line at each of moa's phases, at 40 and 80: connecting (the link down), off,
/// starting, thinking (animated), ready, failed with and without a reason, an unknown phase,
/// and `↓ N new` below a view scrolled up.
#[test]
fn the_head_line_at_each_phase() {
    use moa_core::publish::MoaRow;
    use moa_view::views::console::head;
    let (mut data, ctx) = lantern();
    let thread = Some("0193b4f0-aaaa-7000-8000-000000000001".to_string());
    let phases: [(&str, Option<&str>, bool, usize); 9] = [
        ("ready", None, false, 0),
        ("off", None, true, 0),
        ("starting", None, true, 0),
        ("thinking", None, true, 0),
        ("ready", None, true, 0),
        (
            "failed",
            Some("moa's agent exited (code 1)\nsee moa.log"),
            true,
            0,
        ),
        ("failed", None, true, 0),
        ("waking", None, true, 0),
        ("thinking", None, true, 12),
    ];
    for w in [40, 80] {
        let mut lines = Vec::new();
        for (phase, why, up, new) in phases {
            data.moa = Some(MoaRow {
                phase: phase.into(),
                thread: thread.clone(),
                why: why.map(str::to_string),
                queued: 0,
                asking: Vec::new(),
                auto: true,
            });
            let line = head(&data, &ctx, up, &BTreeSet::new(), new, w);
            assert_eq!(line.animated, up && phase == "thinking", "{phase}");
            lines.push(line);
        }
        assert_fits(&lines, w);
        insta::assert_snapshot!(format!("head@{w}"), debug_lines(&lines));
    }
}

/// The permission fixture: a turn of answered requests (and one a restart left `pending`),
/// then two still waiting, moa thinking.
fn asked() -> (Data, Ctx) {
    let (mut data, ctx) = lantern();
    permission_rows(&mut data);
    (data, ctx)
}

/// K12: each request is a card. Answered: `✓ allowed once`, `✓ allowed always`, `✗ refused`
/// (once or always), and `⊘ not answered` for a cancelled one and for one still `pending` that nothing waits
/// on. Waiting: `⚠ moa asks: <title>`, its buttons behind the card's gutter for the answers
/// offered, and the name moa's agent gives `always`.
#[test]
fn permission_cards_at_40_and_80() {
    let (data, ctx) = asked();
    assert_eq!(data.unreadable(), 0);
    for w in [40, 80] {
        let view = build(&data, &ctx, &ConsoleUi::default(), true, w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("permissions@{w}"), debug_lines(&view.lines));
    }
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    assert_eq!(
        pressed(&view),
        [
            ("y once", "/permission 9 once"),
            ("a always", "/permission 9 always"),
            ("n no", "/permission 9 no"),
            ("y once", "/permission 10 once"),
            ("n no", "/permission 10 no"),
        ]
    );
}

/// A row still `pending` whose request isn't in `asking` (the daemon restarted, or moa's
/// state hasn't come) can never be answered: `⊘ not answered`, with no buttons.
#[test]
fn a_stale_pending_row_is_not_answered() {
    let (mut data, ctx) = asked();
    let text = |data: &Data| -> Vec<String> {
        let view = build(data, &ctx, &ConsoleUi::default(), true, 80);
        assert!(view.buttons.is_empty(), "no buttons: {:?}", view.buttons);
        view.lines.iter().map(Line::text).collect()
    };
    data.moa.as_mut().unwrap().asking.clear();
    data.touch();
    let lines = text(&data);
    assert!(lines.contains(&s("│ ⊘ not answered: Delete ~/private/harbor/old.log")));
    assert!(lines.contains(&s("│ ⊘ not answered: Write ~/private/kite/RELEASE.md")));
    assert!(lines.contains(&s("│ ⊘ not answered: cargo publish -p kite")));
    assert!(!lines.iter().any(|l| l.contains("moa asks")));
    data.moa = None;
    data.touch();
    assert!(text(&data).contains(&s("│ ⊘ not answered: cargo publish -p kite")));
}

/// K12: while a request waits, the head line's state says which keys answer the oldest:
/// `⚠ answer y/a/n`, or only the letters it offers, and it doesn't spin. None once nothing
/// waits, or while the link is down.
#[test]
fn the_head_says_answer() {
    use moa_view::views::console::head;
    let (mut data, ctx) = asked();
    let none = BTreeSet::new();
    let line = |data: &Data, up: bool, w: u16| head(data, &ctx, up, &none, 0, w);
    for w in [40, 80] {
        let all = line(&data, true, w);
        assert_fits(std::slice::from_ref(&all), w);
        assert!(
            all.text().contains("⚠ answer y/a/n Esc cancels"),
            "{w}: {:?}",
            all.text()
        );
        assert!(
            !all.text().contains("thinking") && !all.animated,
            "{w}: {:?}",
            all.text()
        );
        let warn = all.spans.iter().find(|s| s.text.contains("⚠ answer"));
        assert_eq!(warn.map(|s| s.tone), Some(moa_view::line::Tone::Warn));
    }
    data.moa.as_mut().unwrap().asking = vec![10];
    data.touch();
    let only = line(&data, true, 80).text();
    assert!(
        only.contains("⚠ answer y/n") && !only.contains("y/a/n"),
        "{only:?}"
    );
    assert!(!line(&data, false, 80).text().contains("answer"));
    data.moa.as_mut().unwrap().asking.clear();
    data.touch();
    let thinking = line(&data, true, 80);
    assert!(!thinking.text().contains("answer") && thinking.animated);
}

/// While the panel's connection is down, moa's state may be stale: no card waits, so none
/// has buttons, and each one `pending` says it wasn't answered.
#[test]
fn a_card_waits_only_while_the_link_is_up() {
    let (data, ctx) = asked();
    let view = build(&data, &ctx, &ConsoleUi::default(), false, 80);
    assert!(view.buttons.is_empty(), "no buttons: {:?}", view.buttons);
    let lines: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert!(!lines.iter().any(|l| l.contains("moa asks")), "{lines:#?}");
    assert!(lines.contains(&s("│ ⊘ not answered: cargo publish -p kite")));
}

/// K7: a card moa's core writes is drawn as a command's card is: its head line (Warn), then
/// its text as Markdown behind the card's gutter, whose commands are buttons. In a turn it
/// sits behind the turn's gutter too. An unknown `what` says only `▸ moa`.
#[test]
fn cards_draw_as_cards() {
    let (_, ctx) = lantern();
    let card =
        |id, what: &str, text: &str| row(id, "card", json!({"what": what, "text": text}), None);
    let watchdog = "moa's turn had no news for 5 min, so moa cancelled it. It still hadn't \
                    stopped 30 s later, so moa's agent was restarted and today's thread reopened.";
    let account = "moa's account acct-a failed: Not logged in · Please run /login.\n\n\
                   Nothing was switched. To go on with another account:\n\n`/account acct-b`";
    let rows = vec![
        row(1, "you", json!({"text": "Tidy the Harbor folder"}), None),
        row(2, "moa", json!({"text": "Looking at the folder"}), None),
        card(3, "watchdog", watchdog),
        row(4, "you", json!({"text": "Try again"}), None),
        card(5, "account", account),
        card(
            6,
            "a later kind",
            "From a newer moa: `/new` starts a new thread.",
        ),
    ];
    for w in [40, 80] {
        let view = only(rows.clone(), &ctx, w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("cards@{w}"), debug_lines(&view.lines));
    }
    let view = only(rows, &ctx, 80);
    let lines: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert!(lines.contains(&s("│ ▸ moa's turn was stuck")), "{lines:#?}");
    assert!(lines.contains(&s("│ ▸ moa's account failed")), "{lines:#?}");
    assert!(lines.contains(&s("│ ▸ moa")), "{lines:#?}");
    // The account card's `/account B` is a button, as the card offers it.
    assert_eq!(
        pressed(&view),
        [("/account acct-b", "/account acct-b"), ("/new", "/new")]
    );
}

/// A9: an ask to moa is a card, `▸ an ask for moa` with who asked and what, then moa's turn
/// on its own, saying whose ask it answers (Dim): `· moa answers Kite's ask #12`; the hidden
/// prompt itself is never drawn. At 40 and 100.
#[test]
fn an_ask_for_moa_draws() {
    let (_, ctx) = lantern();
    let card = row(
        1,
        "card",
        json!({"what": "ask", "text": "Kite · #12: Is the domain renewed?"}),
        None,
    );
    let mut asked = row(
        2,
        "you",
        json!({
            "text": "(moa · Kite asks you, ask #12, not typed by the user)\nIs the domain \
                     renewed?\n\nYour reply this turn goes back to Kite as the answer.",
            "ask": {"id": 12, "from": "Kite"},
        }),
        None,
    );
    asked.automatic = true;
    let answer = row(3, "moa", json!({"text": "Renewed in May."}), None);
    let rows = vec![card, asked.clone(), answer];
    for w in [40, 100] {
        let view = only(rows.clone(), &ctx, w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("ask_for_moa@{w}"), debug_lines(&view.lines));
    }
    let lines: Vec<String> = only(rows, &ctx, 100).lines.iter().map(Line::text).collect();
    assert!(lines.contains(&s("│ ▸ an ask for moa")), "{lines:#?}");
    assert!(
        lines.contains(&s("│ · moa answers Kite's ask #12")),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("not typed")), "{lines:#?}");
    // A body this build can't read says nothing of an ask.
    asked.body["ask"] = json!({"id": "twelve"});
    let lines: Vec<String> = only(vec![asked], &ctx, 100)
        .lines
        .iter()
        .map(Line::text)
        .collect();
    assert!(!lines.iter().any(|l| l.contains("answers")), "{lines:#?}");
    for w in 1..=40 {
        let view = only(
            vec![row(
                1,
                "card",
                json!({"what": "ask", "text": "Kite · #12: Is the domain renewed?"}),
                None,
            )],
            &ctx,
            w,
        );
        assert_fits(&view.lines, w);
    }
}

/// moa's state row: `phase`, today's thread, `queued` reports waiting, `auto`.
fn moa(phase: &str, queued: u32, auto: bool) -> moa_core::publish::MoaRow {
    moa_core::publish::MoaRow {
        phase: phase.into(),
        thread: Some(THREAD_TODAY.into()),
        why: None,
        queued,
        asking: Vec::new(),
        auto,
    }
}

/// K12: while session reports wait for moa, the head line says how many after the state
/// (Dim): `◷ 3 for moa`; nothing while none do, or while the link is down. At 40 and 80:
/// ready, thinking, off and waiting for an answer with 3 waiting, ready with 1 under `↓ N
/// new`, and ready with none.
#[test]
fn the_head_counts_reports() {
    use moa_view::views::console::head;
    let (mut asked, ctx) = asked();
    let (mut data, _) = lantern();
    let none = BTreeSet::new();
    for w in [40, 80] {
        let mut lines = Vec::new();
        for (phase, queued, new) in [
            ("ready", 3, 0),
            ("thinking", 3, 0),
            ("off", 3, 0),
            ("ready", 1, 4),
            ("ready", 0, 0),
        ] {
            data.moa = Some(moa(phase, queued, false));
            data.touch();
            lines.push(head(&data, &ctx, true, &none, new, w));
        }
        asked.moa.as_mut().unwrap().queued = 3;
        asked.touch();
        lines.push(head(&asked, &ctx, true, &none, 0, w));
        assert_fits(&lines, w);
        insta::assert_snapshot!(format!("head-reports@{w}"), debug_lines(&lines));
    }
    data.moa = Some(moa("ready", 3, true));
    data.touch();
    let line = head(&data, &ctx, true, &none, 0, 80);
    let count = line.spans.iter().find(|s| s.text.contains("◷ 3 for moa"));
    assert_eq!(count.map(|s| s.tone), Some(moa_view::line::Tone::Dim));
    assert!(!head(&data, &ctx, false, &none, 0, 80).text().contains('◷'));
    data.moa = Some(moa("ready", 0, true));
    data.touch();
    assert!(!head(&data, &ctx, true, &none, 0, 80).text().contains('◷'));
}

/// K12: a wake-up that carried reports says so under its turn's header, behind the gutter,
/// as soon as it is sent: `· moa is reading 3 session reports…` (Dim); its text, the reports
/// themselves, is never drawn. moa's answer follows under the same header.
#[test]
fn a_wake_says_it_reads_reports() {
    let (_, ctx) = lantern();
    let mut wake = row(
        1,
        "you",
        json!({"text": "(moa · a wake-up) 3 session reports:", "reports": 3}),
        None,
    );
    wake.automatic = true;
    let text = |rows: Vec<FeedRow>| -> Vec<String> {
        let view = only(rows, &ctx, 80);
        assert_fits(&view.lines, 80);
        view.lines.iter().map(Line::text).collect()
    };
    assert_eq!(
        text(vec![wake.clone()]),
        [
            "◆ moa  19:00 · on its own",
            "│ · moa is reading 3 session reports…"
        ]
    );
    let answer = row(2, "moa", json!({"text": "Kite finished."}), None);
    assert_eq!(
        text(vec![wake.clone(), answer]),
        [
            "◆ moa  19:00 · on its own",
            "│ · moa is reading 3 session reports…",
            "│ Kite finished.",
        ]
    );
    let view = only(vec![wake.clone()], &ctx, 80);
    assert!(
        view.lines[1]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.tone == moa_view::line::Tone::Dim)
    );
    // One report reads as one; a wake-up with none says nothing more.
    wake.body["reports"] = json!(1);
    assert_eq!(
        text(vec![wake.clone()])[1],
        "│ · moa is reading 1 session report…"
    );
    wake.body["reports"] = json!(0);
    assert!(text(vec![wake]).is_empty());
}

/// D10: an automatic prompt that carried your yeses says so under its turn's header (Dim):
/// `· moa acts on your yes to #12 and #14`, before the reports it reads. Your own prompt says
/// `· with your yes to #12`.
#[test]
fn moa_says_it_acts_on_your_yes() {
    let (_, ctx) = lantern();
    let text = |rows: Vec<FeedRow>, w: u16| -> Vec<String> {
        let view = only(rows, &ctx, w);
        assert_fits(&view.lines, w);
        view.lines.iter().map(Line::text).collect()
    };
    let mut yes = row(
        1,
        "you",
        json!({"text": "(moa · the user said yes)", "yeses": [12, 14]}),
        None,
    );
    yes.automatic = true;
    assert_eq!(
        text(vec![yes.clone()], 80),
        [
            "◆ moa  19:00 · on its own",
            "│ · moa acts on your yes to #12 and #14"
        ]
    );
    let view = only(vec![yes.clone()], &ctx, 80);
    assert!(
        view.lines[1]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.tone == moa_view::line::Tone::Dim)
    );
    yes.body["yeses"] = json!([12]);
    assert_eq!(
        text(vec![yes.clone()], 80)[1],
        "│ · moa acts on your yes to #12"
    );
    yes.body["yeses"] = json!([12, 14, 15]);
    yes.body["reports"] = json!(2);
    assert_eq!(
        text(vec![yes.clone()], 80)[1..],
        [
            "│ · moa acts on your yes to #12, #14 and #15",
            "│ · moa is reading 2 session reports…"
        ]
    );
    for w in 1..=40 {
        assert_fits(&only(vec![yes.clone()], &ctx, w).lines, w);
    }

    let hi = row(
        2,
        "you",
        json!({"text": "hi", "yeses": [12], "reports": 1, "carried": "(moa · …)"}),
        None,
    );
    assert_eq!(
        text(vec![hi.clone()], 80),
        [
            "› hi",
            "  · with your yes to #12",
            "  · with 1 session report"
        ]
    );
    for w in 1..=40 {
        assert_fits(&only(vec![hi.clone()], &ctx, w).lines, w);
    }
}

/// D10: the prompt that opens the day says so first: `· moa opens the day` under an
/// automatic one's header, `· opens the day` under your own (Dim).
#[test]
fn the_day_opens() {
    let (_, ctx) = lantern();
    let text = |rows: Vec<FeedRow>, w: u16| -> Vec<String> {
        let view = only(rows, &ctx, w);
        assert_fits(&view.lines, w);
        view.lines.iter().map(Line::text).collect()
    };
    let mut brief = row(
        1,
        "you",
        json!({"text": "(moa · the user is here)", "opening": true}),
        None,
    );
    brief.automatic = true;
    assert_eq!(
        text(vec![brief.clone()], 80),
        ["◆ moa  19:00 · on its own", "│ · moa opens the day"]
    );
    let view = only(vec![brief.clone()], &ctx, 80);
    assert!(
        view.lines[1]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.tone == moa_view::line::Tone::Dim)
    );
    brief.body["yeses"] = json!([12]);
    brief.body["reports"] = json!(2);
    assert_eq!(
        text(vec![brief.clone()], 80)[1..],
        [
            "│ · moa opens the day",
            "│ · moa acts on your yes to #12",
            "│ · moa is reading 2 session reports…"
        ]
    );
    for w in 1..=40 {
        assert_fits(&only(vec![brief.clone()], &ctx, w).lines, w);
    }

    let hi = row(
        2,
        "you",
        json!({"text": "hi", "opening": true, "yeses": [12], "reports": 1, "carried": "(moa · …)"}),
        None,
    );
    assert_eq!(
        text(vec![hi.clone()], 80),
        [
            "› hi",
            "  · opens the day",
            "  · with your yes to #12",
            "  · with 1 session report"
        ]
    );
    let view = only(vec![hi.clone()], &ctx, 80);
    assert!(
        view.lines[1]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.tone == moa_view::line::Tone::Dim)
    );
    for w in 1..=40 {
        assert_fits(&only(vec![hi.clone()], &ctx, w).lines, w);
    }
}

/// K12: your prompt that carried session reports says how many under it (Dim): `· with 2
/// session reports`. One without says nothing more.
#[test]
fn your_prompt_says_what_it_carried() {
    let (_, ctx) = lantern();
    let hi = row(
        1,
        "you",
        json!({"text": "hi", "reports": 2, "carried": "(moa · 2 session reports)"}),
        None,
    );
    let view = only(vec![hi.clone()], &ctx, 80);
    let text: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert_eq!(text, ["› hi", "  · with 2 session reports"]);
    assert!(
        view.lines[1]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.tone == moa_view::line::Tone::Dim)
    );
    let mut one = hi.clone();
    one.body["reports"] = json!(1);
    assert_eq!(
        only(vec![one], &ctx, 80).lines[1].text(),
        "  · with 1 session report"
    );
    let plain = row(1, "you", json!({"text": "hi"}), None);
    assert_eq!(only(vec![plain], &ctx, 80).lines.len(), 1);
    for w in 1..=40 {
        assert_fits(&only(vec![hi.clone()], &ctx, w).lines, w);
    }
}

/// K12 (B3): a session's last line shows under its line (finished or needing you) only while
/// auto is off (moa doesn't read the reports on its own); with auto on, moa says what matters.
/// No state row counts as off, and so does a link that is down (moa's state may be stale).
#[test]
fn turn_text_only_when_auto_is_off() {
    let (mut data, ctx) = lantern();
    let event = |seq: u64, n: u8, what: EventWhat| EventRow {
        seq,
        at: 1_000,
        session: session(n),
        what,
    };
    data.events.insert(1, event(1, 3, EventWhat::Finished));
    data.events.insert(2, event(2, 1, EventWhat::NeedsYou));
    let text = |data: &Data, up: bool| -> Vec<String> {
        build(data, &ctx, &ConsoleUi::default(), up, 80)
            .lines
            .iter()
            .map(Line::text)
            .collect()
    };
    data.touch();
    let off = [
        "⚠ Fix login needs you [↩ answer]",
        "  Bash: cargo test ⚠",
        "● Release notes · 1 turn since 19:00 [↩ open]",
        "  Ship it today?",
    ];
    assert_eq!(text(&data, true), off);
    data.moa = Some(moa("ready", 0, false));
    data.touch();
    assert_eq!(text(&data, true), off);
    data.moa = Some(moa("ready", 0, true));
    data.touch();
    assert_eq!(
        text(&data, true),
        [
            "⚠ Fix login needs you [↩ answer]",
            "● Release notes · 1 turn since 19:00 [↩ open]",
        ]
    );
    assert_eq!(text(&data, false), off);
}

/// Suggestion `id`, `status`, with `actions` (verb first) and `result`.
fn suggestion(
    id: i64,
    text: &str,
    status: SuggestionStatus,
    actions: &[&[&str]],
    result: Option<&str>,
) -> Suggestion {
    let actions = actions
        .iter()
        .map(|a| SuggestionAction {
            verb: a[0].to_string(),
            args: a[1..].iter().map(|s| s.to_string()).collect(),
        })
        .collect();
    Suggestion {
        id: SuggestionId(id),
        text: text.into(),
        status,
        actions,
        actions_raw: None,
        actions_invalid: None,
        result: result.map(str::to_string),
        created_at: UtcMs(1_000),
        resolved_at: None,
    }
}

/// A `suggestion` row, id `row_id`, for suggestion `id` with the text it was made with.
fn card_row(row_id: i64, id: i64, text: &str) -> FeedRow {
    row(row_id, "suggestion", json!({"id": id, "text": text}), None)
}

/// A turn of moa's with six suggestion cards, one of each kind, and the suggestions.
fn suggested() -> (Data, Ctx) {
    use SuggestionStatus::{Accepted, Later, Open, Rejected};
    let (_, ctx) = lantern();
    let close: &[&str] = &["term", "close", "4"];
    let task: &[&str] = &["task", "add", "Kite", "Write the release notes"];
    let made = [
        suggestion(
            12,
            "Close Kite's shell, it's been idle **2 h**",
            Open,
            &[close, task],
            None,
        ),
        suggestion(13, "Set Kite's due date", Later, &[], None),
        suggestion(14, "Read the Harbor notes before the call", Open, &[], None),
        suggestion(
            15,
            "Close Lantern's shell",
            Accepted,
            &[close],
            Some("done"),
        ),
        suggestion(
            16,
            "Close Beacon's shell",
            Accepted,
            &[close],
            Some("term close 4: that terminal is gone"),
        ),
        suggestion(17, "Archive the Beacon spike", Rejected, &[], None),
    ];
    let mut data = Data::default();
    let mut rows = vec![
        row(1, "you", json!({"text": "What's left today?"}), None),
        row(2, "moa", json!({"text": "A few things."}), None),
    ];
    for (i, s) in made.iter().enumerate() {
        rows.push(card_row(3 + i as i64, s.id.0, &s.text));
    }
    for r in rows {
        data.feed.insert(r.id, r);
    }
    for s in made {
        data.suggestions.insert(s.id, s);
    }
    data.touch();
    (data, ctx)
}

/// D7, D10: moa's suggestion is a card: open or later, `✦ moa suggests` (` · later`), its
/// text as Markdown behind the card's gutter, what yes runs, and `[yes] [later] [no]`;
/// answered, `✓ yes: <text>` (with `but:` and the failure unless it was done) or `✗ no:
/// <text>`.
#[test]
fn suggestion_cards_at_40_and_80() {
    let (data, ctx) = suggested();
    for w in [40, 80] {
        let view = build(&data, &ctx, &ConsoleUi::default(), true, w);
        assert_fits(&view.lines, w);
        insta::assert_snapshot!(format!("suggestions@{w}"), debug_lines(&view.lines));
    }
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    let buttons =
        |n: i64| ["yes", "later", "no"].map(|a| (a.to_string(), format!("/suggestion {a} {n}")));
    let want: Vec<(String, String)> = [12, 13, 14].into_iter().flat_map(buttons).collect();
    let got: Vec<(String, String)> = pressed(&view)
        .into_iter()
        .map(|(l, c)| (l.to_string(), c.to_string()))
        .collect();
    assert_eq!(got, want);
}

/// §6.12: a card answered anywhere (the today view, `moactl`) says so as soon as the
/// suggestions topic does: the same feed draws `✓ yes: …`, with no buttons.
#[test]
fn answering_anywhere_redraws_the_card() {
    let (_, ctx) = lantern();
    let text = "Set Kite's due date";
    let mut data = Data::default();
    let r = card_row(1, 12, text);
    data.feed.insert(r.id, r);
    data.suggestions.insert(
        SuggestionId(12),
        suggestion(12, text, SuggestionStatus::Open, &[], None),
    );
    data.touch();
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    assert_eq!(view.buttons.len(), 3);
    let s = data.suggestions.get_mut(&SuggestionId(12)).unwrap();
    s.status = SuggestionStatus::Accepted;
    s.result = Some("done".into());
    data.touch();
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    let lines: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert_eq!(lines[1..], [format!("│ ✓ yes: {text}")], "{lines:#?}");
    assert!(view.buttons.is_empty(), "{:?}", view.buttons);
}

/// Review focus 4: a card whose suggestion isn't there (`✦ suggestion #N: <text>`, from the
/// row's text, dim), one whose actions can't run (`yes can't run its actions`, no `[yes]`),
/// one with a status this build doesn't know (as one that isn't there), and 5 000 characters
/// at 40: each drawn, and no line wider than its width at 40 to 200.
#[test]
fn odd_suggestion_cards_draw() {
    let (_, ctx) = lantern();
    let long = "Kite ".repeat(1_000);
    let mut data = Data::default();
    let rows = vec![
        card_row(1, 99, "Tidy the **Harbor** folder"),
        card_row(2, 12, "Close Kite's shell"),
        card_row(3, 13, "From a newer moa"),
        card_row(4, 14, &long),
        row(5, "suggestion", json!({"text": "no id"}), None),
    ];
    for r in rows {
        data.feed.insert(r.id, r);
    }
    let mut invalid = suggestion(
        12,
        "Close Kite's shell",
        SuggestionStatus::Open,
        &[&["term", "open", "4"]],
        None,
    );
    invalid.actions_invalid = Some("an action doesn't parse as moactl".into());
    data.suggestions.insert(SuggestionId(12), invalid);
    data.suggestions.insert(
        SuggestionId(14),
        suggestion(14, &long, SuggestionStatus::Open, &[], None),
    );
    data.upsert(
        Topic::Suggestions,
        vec![json!({
            "key": "13", "id": 13, "text": "From a newer moa", "status": "snoozed",
            "actions": [], "actions_raw": null, "actions_invalid": null, "result": null,
            "created_at": 1_000, "resolved_at": null,
        })],
    );
    for w in 40..=200 {
        let view = build(&data, &ctx, &ConsoleUi::default(), true, w);
        assert_fits(&view.lines, w);
    }
    let view = build(&data, &ctx, &ConsoleUi::default(), true, 80);
    let lines: Vec<String> = view.lines.iter().map(Line::text).collect();
    assert_eq!(lines[1], "│ ✦ suggestion #99: Tidy the **Harbor** folder");
    let tones: Vec<_> = view.lines[1].spans.iter().map(|s| s.tone).collect();
    assert!(
        tones.iter().all(|t| *t == moa_view::line::Tone::Dim),
        "{tones:?}"
    );
    assert!(
        lines.contains(&s("│ ┊ yes can't run its actions")),
        "{lines:#?}"
    );
    assert!(
        lines.contains(&s("│ ✦ suggestion #13: From a newer moa")),
        "{lines:#?}"
    );
    assert!(
        lines.contains(&s("│ · an entry moa couldn't show (#5)")),
        "{lines:#?}"
    );
    assert_eq!(
        pressed(&view),
        [
            ("later", "/suggestion later 12"),
            ("no", "/suggestion no 12"),
            ("yes", "/suggestion yes 14"),
            ("later", "/suggestion later 14"),
            ("no", "/suggestion no 14"),
        ]
    );
}
