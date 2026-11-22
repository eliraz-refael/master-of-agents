//! The terminals view (5d, T9) on the fixture's four terminals: its rows and their order, the
//! idle mark, the keys that close, name, note and set a timeout, and when its text changes.

use moa_core::UtcMs;
use moa_core::op::{Done, Op};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_core::time::until;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, terminals};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Tone};
use moa_view::panel::{Out, Panel};
use moa_view::text::next_until_change;
use moa_view::views::terms;
use serde_json::json;

const SIZE: Size = Size { w: 100, h: 16 };
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

fn fixture() -> (Data, Ctx) {
    let (mut data, ctx) = lantern();
    terminals(&mut data, &ctx);
    (data, ctx)
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

fn term(pane: &str) -> Item {
    Item::Term {
        pane: pane.to_string(),
    }
}

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
    /// The tag the last op sent went out under.
    tag: u64,
}

impl T {
    fn new() -> T {
        let (data, ctx) = fixture();
        let mut panel = Panel::new(View::Terms);
        panel.screen(&data, &ctx, SIZE);
        T {
            panel,
            data,
            ctx,
            tag: 0,
        }
    }

    /// Selects `pane`'s row.
    fn on(mut self, pane: &str) -> T {
        assert!(self.panel.select(&term(pane)));
        self
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        let outs = self.panel.key(k, &self.data, &self.ctx, SIZE);
        for _ in outs.iter().filter(|o| matches!(o, Out::Send(_))) {
            self.tag += 1;
            self.panel.sent(self.tag);
        }
        outs
    }

    /// The reply to the last op sent.
    fn replied(&mut self, result: Result<Done, String>) -> Option<Out> {
        self.panel.replied(self.tag, result)
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            assert_eq!(self.key(Key::Char(c)), Vec::new(), "typing {c:?}");
        }
    }

    fn footer(&mut self) -> String {
        self.panel.screen(&self.data, &self.ctx, SIZE).footer.text
    }
}

#[test]
fn terms_rows_order_and_idle_mark() {
    let (data, ctx) = fixture();
    let lines = terms::lines(&data, &ctx, 120);
    let items: Vec<Option<Item>> = lines.iter().map(|l| l.item.clone()).collect();
    // The one you're looking at, then the most recently touched (or opened): kite 10 minutes
    // ago, scratch opened 3 hours ago, harbor logs touched 5 hours ago.
    assert_eq!(
        items,
        [
            None,
            Some(term("%5")),
            Some(term("%7")),
            Some(term("%11")),
            Some(term("%9"))
        ]
    );
    let text = texts(&lines);
    assert!(text[1].contains("dev server"), "{}", text[1]);
    assert!(text[1].contains("~/private/lantern"), "{}", text[1]);
    assert!(text[1].contains("node 2h"), "{}", text[1]);
    assert!(text[1].contains("now"), "{}", text[1]);
    assert!(text[1].contains("⏱1h"), "{}", text[1]);
    assert!(text[1].ends_with("  port 3000"), "{}", text[1]);
    assert!(text[2].contains("shell"), "{}", text[2]);
    assert!(text[2].contains("10m"), "{}", text[2]);
    // `harbor logs` belongs to a session; its note is dim.
    assert!(text[4].contains("tail 6h · Fix login"), "{}", text[4]);
    assert!(text[4].ends_with("watching the deploy"), "{}", text[4]);
    assert_eq!(lines[4].spans.last().map(|s| s.tone), Some(Tone::Dim));
    // Idle: not being looked at, and untouched (or unopened) for 2 hours. Dim, and said.
    let idle: Vec<bool> = text[1..].iter().map(|t| t.contains(" idle")).collect();
    assert_eq!(idle, [false, false, true, true]);
    let tones: Vec<Tone> = lines[1..].iter().map(|l| l.spans[0].tone).collect();
    assert_eq!(tones, [Tone::Text, Tone::Text, Tone::Dim, Tone::Dim]);
    // Never touched shows a dash, not a made-up age.
    assert!(text[3].contains("  - "), "{}", text[3]);
}

#[test]
fn terms_header_counts_idle() {
    let (mut data, ctx) = fixture();
    let head = |data: &Data| terms::lines(data, &ctx, 80)[0].clone();
    assert_eq!(head(&data).text(), "4 terminals · 2 idle");
    assert_eq!(head(&data).spans[0].tone, Tone::Dim);
    assert_eq!(head(&data).item, None);
    // `terminal_idle_hours = 6`, as the brain row says it: nothing is idle yet.
    data.upsert(
        Topic::Brain,
        vec![json!({
            "key": "brain", "name": "default", "folders": [], "accounts": [],
            "open_account": null, "folded": [], "terminal_idle_ms": 6 * HOUR,
        })],
    );
    assert_eq!(head(&data).text(), "4 terminals · 0 idle");
    // 0 turns the mark off.
    data.upsert(
        Topic::Brain,
        vec![json!({
            "key": "brain", "name": "default", "folders": [], "accounts": [],
            "open_account": null, "folded": [], "terminal_idle_ms": 0,
        })],
    );
    assert_eq!(head(&data).text(), "4 terminals · 0 idle");
    // One is singular.
    for pane in ["%7", "%9", "%11"] {
        data.terms.remove(pane);
    }
    data.touch();
    assert_eq!(head(&data).text(), "1 terminal · 0 idle");
}

/// `BrainRow.terminal_idle_ms` is what the view counts idle by: the brain's setting reaches
/// it, and a brain row from an older daemon (none of it) means the default, 2 hours.
#[test]
fn the_idle_setting_reaches_the_view() {
    let (mut data, ctx) = fixture();
    let idle = |data: &Data| {
        terms::lines(data, &ctx, 120)[1..]
            .iter()
            .filter(|l| l.text().contains(" idle"))
            .count()
    };
    // The fixture's brain row has no setting: 2 hours.
    assert_eq!(idle(&data), 2);
    let row = |ms: i64| {
        json!({
            "key": "brain", "name": "default", "folders": [], "accounts": [],
            "open_account": null, "folded": [], "terminal_idle_ms": ms,
        })
    };
    // 4 hours: only `harbor logs` (5 hours) is.
    data.upsert(Topic::Brain, vec![row(4 * HOUR)]);
    assert_eq!(idle(&data), 1);
    data.upsert(Topic::Brain, vec![row(5 * MINUTE)]);
    assert_eq!(idle(&data), 3);
    data.upsert(Topic::Brain, vec![row(0)]);
    assert_eq!(idle(&data), 0);
}

#[test]
fn none_open_says_so() {
    let (data, ctx) = lantern();
    let lines = terms::lines(&data, &ctx, 80);
    assert_eq!(texts(&lines), ["no terminals open"]);
    assert_eq!(lines[0].spans[0].tone, Tone::Dim);
    assert!(!lines[0].selectable());
    // Enter and the keys on nothing do nothing.
    let mut panel = Panel::new(View::Terms);
    panel.screen(&data, &ctx, SIZE);
    for k in ['x', 'r', 'n', 't'] {
        assert_eq!(panel.key(Key::Char(k), &data, &ctx, SIZE), Vec::new());
        assert!(!panel.typing());
    }
    assert_eq!(panel.key(Key::Enter, &data, &ctx, SIZE), Vec::new());
}

fn done(text: &str) -> Done {
    Done {
        text: text.to_string(),
        data: Default::default(),
    }
}

#[test]
fn enter_shows_x_always_confirms_q_and_esc_close() {
    let mut t = T::new().on("%5");
    // Enter shows it, and the popup closes on the `Ok`.
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermShow {
            pane: "%5".to_string()
        })]
    );
    assert_eq!(t.replied(Ok(done("went to dev server"))), Some(Out::Close));
    // Only the jump's reply closes it.
    assert_eq!(t.replied(Ok(done(""))), None);

    // `x` asks first, whatever is in the terminal; only `y` sends. A running one says what.
    assert_eq!(t.key(Key::Char('x')), Vec::new());
    assert_eq!(t.footer(), "close dev server, running node? y/N");
    assert_eq!(t.key(Key::Char('n')), Vec::new());
    assert_eq!(t.key(Key::Char('x')), Vec::new());
    assert_eq!(
        t.key(Key::Char('y')),
        [Out::Send(Op::TermClose {
            target: "%5".to_string()
        })]
    );
    // A shell at its prompt asks too, without the program.
    let mut t = T::new().on("%7");
    assert_eq!(t.key(Key::Char('x')), Vec::new());
    assert_eq!(t.footer(), "close kite? y/N");
    assert_eq!(
        t.key(Key::Char('y')),
        [Out::Send(Op::TermClose {
            target: "%7".to_string()
        })]
    );
    // A footer of 30 columns keeps `y/N`.
    for pane in ["%5", "%7", "%9"] {
        let mut t = T::new().on(pane);
        let size = Size { w: 30, h: 12 };
        t.panel.key(Key::Char('x'), &t.data, &t.ctx, size);
        let footer = t.panel.screen(&t.data, &t.ctx, size).footer.text;
        assert!(footer.ends_with("? y/N"), "{footer:?}");
        assert!(footer.chars().count() <= 27, "{footer:?}");
    }
    // `q` and Esc close the view, and so does Ctrl-c; Esc answers a question first.
    for k in [Key::Char('q'), Key::Esc] {
        assert_eq!(T::new().key(k), [Out::Close], "{k:?}");
    }
    assert_eq!(T::new().panel.interrupt(), Some(Out::Close));
    let mut t = T::new().on("%5");
    t.key(Key::Char('x'));
    assert_eq!(t.key(Key::Esc), Vec::new());
    assert_eq!(t.key(Key::Esc), [Out::Close]);
}

/// A countdown redraws when its text changes: from a day and more it steps by days, then
/// hours, then minutes, and the last minute reads `<1m` until it is `now`.
#[test]
fn a_timeout_countdown_redraws_on_time() {
    let (mut data, ctx) = lantern();
    // Open 5 days, so no age text changes before tomorrow; long untouched, so idle already.
    data.upsert(
        Topic::Terms,
        vec![json!({
            "key": "%5", "name": "dev", "id": 1, "opened_at": ctx.now.0 - 120 * HOUR,
            "timeout_at": ctx.now.0 + 90 * MINUTE,
        })],
    );
    let lines = terms::lines(&data, &ctx, 100);
    assert!(lines[1].text().contains("⏱1h"), "{}", lines[1].text());
    // `1h` becomes `59m` one millisecond after the timeout is under an hour away.
    let at = UtcMs(ctx.now.0 + 30 * MINUTE + 1);
    assert_eq!(lines[1].changes_at, Some(at));
    let timeout = UtcMs(ctx.now.0 + 90 * MINUTE);
    assert_eq!(until(timeout, UtcMs(at.0 - 1)), "1h");
    assert_eq!(until(timeout, at), "59m");

    // Each step lands exactly where the text changes, whatever the unit.
    for left in [
        1,
        MINUTE - 1,
        MINUTE,
        MINUTE + 1,
        59 * MINUTE + 59_999,
        HOUR,
        HOUR + 1,
        23 * HOUR + 59 * MINUTE,
        24 * HOUR,
        3 * 24 * HOUR + 5,
        7 * 24 * HOUR,
    ] {
        let timeout = UtcMs(ctx.now.0 + left);
        let change = next_until_change(timeout, ctx.now).expect("still counting");
        assert!(change > ctx.now, "{left}");
        assert_eq!(
            until(timeout, UtcMs(change.0 - 1)),
            until(timeout, ctx.now),
            "{left}"
        );
        assert_ne!(until(timeout, change), until(timeout, ctx.now), "{left}");
    }
    assert_eq!(next_until_change(UtcMs(5), UtcMs(5)), None);
    assert_eq!(next_until_change(UtcMs(5), UtcMs(9)), None);
}

#[test]
fn only_the_picks_own_reply_closes_the_popup() {
    let mut t = T::new().on("%5");
    // A note goes out, then a pick; the note's `Ok` comes first and closes nothing.
    t.key(Key::Char('n'));
    t.key(Key::Ctrl('u'));
    t.typed("later");
    assert!(matches!(
        t.key(Key::Enter)[..],
        [Out::Send(Op::TermNote { .. })]
    ));
    let note = t.tag;
    assert!(matches!(
        t.key(Key::Enter)[..],
        [Out::Send(Op::TermShow { .. })]
    ));
    assert_eq!(t.panel.replied(note, Ok(done("noted"))), None);
    assert_eq!(t.footer(), "noted");
    // The pick still waits: another Enter only says so.
    assert_eq!(t.key(Key::Enter), Vec::new());
    assert_eq!(t.replied(Ok(done("went to dev server"))), Some(Out::Close));
}

#[test]
fn a_refused_show_stays_in_the_footer() {
    let mut t = T::new().on("%5");
    t.key(Key::Enter);
    assert_eq!(t.replied(Err("that terminal is gone".to_string())), None);
    assert_eq!(t.footer(), "that terminal is gone");
    // The pick is free again.
    assert!(matches!(
        t.key(Key::Enter)[..],
        [Out::Send(Op::TermShow { .. })]
    ));
}

#[test]
fn r_n_t_prompt_and_send_their_ops() {
    let mut t = T::new().on("%5");
    // `r`: the name, prefilled.
    assert_eq!(t.key(Key::Char('r')), Vec::new());
    assert!(t.panel.prompting());
    assert_eq!(t.footer(), "name: dev server");
    t.key(Key::Ctrl('u'));
    t.typed("api");
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermName {
            target: "%5".to_string(),
            name: "api".to_string()
        })]
    );
    // An empty name is no name.
    t.key(Key::Char('r'));
    t.key(Key::Ctrl('u'));
    assert_eq!(t.key(Key::Enter), Vec::new());
    assert_eq!(t.footer(), "cancelled");
    // `n`: the note, prefilled; emptied, it clears the note.
    assert_eq!(t.key(Key::Char('n')), Vec::new());
    assert_eq!(t.footer(), "note: port 3000");
    t.key(Key::Ctrl('u'));
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermNote {
            target: "%5".to_string(),
            note: String::new()
        })]
    );
    // On a terminal with no note, `n` starts empty.
    let mut t = T::new().on("%7");
    t.key(Key::Char('n'));
    assert_eq!(t.footer(), "note: ");
    t.typed("later");
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermNote {
            target: "%7".to_string(),
            note: "later".to_string()
        })]
    );
    // `t`: 2h to start with; `3h` and `off` are what the CLI reads them as.
    assert_eq!(t.key(Key::Char('t')), Vec::new());
    assert_eq!(t.footer(), "close in (3h, 90m, off): 2h");
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermTimeout {
            target: "%7".to_string(),
            after_ms: Some(2 * HOUR)
        })]
    );
    t.key(Key::Char('t'));
    t.key(Key::Ctrl('u'));
    t.typed("off");
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::TermTimeout {
            target: "%7".to_string(),
            after_ms: None
        })]
    );
    // Esc leaves a prompt without sending.
    t.key(Key::Char('r'));
    assert_eq!(t.key(Key::Esc), Vec::new());
    assert!(!t.panel.typing());
}

#[test]
fn a_bad_timeout_answer_stays_in_the_footer() {
    let mut t = T::new().on("%5");
    t.key(Key::Char('t'));
    t.key(Key::Ctrl('u'));
    t.typed("soon");
    assert_eq!(t.key(Key::Enter), Vec::new());
    let want = "a timeout is like 3h or 90m, up to 7 days";
    assert_eq!(t.footer(), want);
    // It stays through redraws, until a key.
    assert_eq!(t.footer(), want);
    assert!(!t.panel.typing());
    // 8 days is too long, the same way.
    t.key(Key::Char('t'));
    t.key(Key::Ctrl('u'));
    t.typed("8d");
    assert_eq!(t.key(Key::Enter), Vec::new());
    assert_eq!(t.footer(), want);
}
