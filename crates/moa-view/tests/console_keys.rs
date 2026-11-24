//! The console panel's keys, clicks and frame (B7–B10): typing to moa, the history, Esc,
//! Tab over the buttons, scrolling and paging back, the errors it shows, and `/quit` (F10).

use moa_core::UtcMs;
use moa_core::op::{Done, NoParams, Op};
use moa_core::panel::View;
use moa_core::publish::{BarRow, FeedRow, LayoutRow, MoaRow};
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::Line;
use moa_view::panel::{CONSOLE_MANAGED, Mouse, Out, PAGE, Panel, STILL_WAITING, Screen};
use serde_json::json;

/// 9 lines of feed between the head line and the input line.
const SIZE: Size = Size { w: 80, h: 16 };
/// Tall enough for every line the tests make.
const TALL: Size = Size { w: 80, h: 400 };
const THREAD: &str = "0193b4f0-aaaa-7000-8000-000000000001";

fn row(id: i64, kind: &str, text: &str) -> FeedRow {
    FeedRow {
        id,
        thread: THREAD.into(),
        at: 1_000 * id,
        kind: kind.into(),
        message_id: None,
        automatic: false,
        body: json!({ "text": text }),
        state: None,
    }
}

/// `· row N` lines, ids `ids`.
fn notes(ids: impl IntoIterator<Item = i64>) -> Vec<FeedRow> {
    ids.into_iter()
        .map(|id| row(id, "system", &format!("row {id}")))
        .collect()
}

fn done(text: &str) -> Done {
    Done {
        text: text.into(),
        data: json!(null),
    }
}

fn text(line: &Line) -> String {
    line.text()
}

struct C {
    panel: Panel,
    data: Data,
    ctx: Ctx,
    size: Size,
    tag: u64,
}

impl C {
    /// The console on the fixture, `rows` in its feed, moa ready on [`THREAD`], no bar.
    fn new(rows: Vec<FeedRow>, size: Size) -> C {
        let (mut data, ctx) = lantern();
        data.feed = rows.into_iter().map(|r| (r.id, r)).collect();
        data.moa = Some(MoaRow {
            phase: "ready".into(),
            thread: Some(THREAD.into()),
            why: None,
            queued: 0,
            asking: Vec::new(),
        });
        data.touch();
        let mut c = C {
            panel: Panel::new(View::Console),
            data,
            ctx,
            size,
            tag: 0,
        };
        c.screen();
        c
    }

    fn phase(&mut self, phase: &str) {
        self.data.moa.as_mut().unwrap().phase = phase.into();
        self.data.touch();
    }

    fn add(&mut self, rows: Vec<FeedRow>) {
        self.data.feed.extend(rows.into_iter().map(|r| (r.id, r)));
        self.data.touch();
    }

    fn bar(&mut self, commands: &[&str]) {
        self.data.bar = Some(BarRow {
            commands: commands.iter().map(|c| c.to_string()).collect(),
            by: "you".into(),
        });
        self.data.touch();
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, self.size)
    }

    fn typed(&mut self, s: &str) -> Vec<Out> {
        s.chars().flat_map(|c| self.key(Key::Char(c))).collect()
    }

    fn screen(&mut self) -> Screen {
        self.panel.screen(&self.data, &self.ctx, self.size)
    }

    fn input(&mut self) -> String {
        self.screen().input.as_ref().map(text).unwrap_or_default()
    }

    fn body(&mut self) -> Vec<String> {
        self.screen().body.iter().map(|(l, _)| text(l)).collect()
    }

    fn head(&mut self) -> String {
        self.screen().head.iter().map(text).collect()
    }

    fn footer(&mut self) -> String {
        self.screen().footer.text
    }

    /// The one op `outs` sends, sent: its tag and the op.
    fn send(&mut self, outs: Vec<Out>) -> (u64, Op) {
        let [Out::Send(op)] = &outs[..] else {
            panic!("not one op sent: {outs:?}");
        };
        self.tag += 1;
        self.panel.sent(self.tag);
        (self.tag, op.clone())
    }

    fn reply(&mut self, tag: u64, result: Result<Done, String>) {
        assert_eq!(self.panel.replied(tag, result), None);
    }

    /// The focused button's text, and whether it is in the bar.
    fn focused(&mut self) -> Option<(String, bool)> {
        let screen = self.screen();
        let body = screen.body.iter().map(|(l, _)| (l, false));
        let bar = screen.bar.iter().map(|l| (l, true));
        let mut found = body.chain(bar).flat_map(|(l, in_bar)| {
            l.spans
                .iter()
                .filter(|s| s.reversed)
                .map(move |s| (s.text.clone(), in_bar))
        });
        let first = found.next();
        assert_eq!(found.next(), None, "one button is focused");
        first
    }
}

/// B7: Enter sends a prompt, or a command for a line starting with `/`; nothing for a blank
/// line. The draft stays until the send is accepted; a command's reply is a card.
#[test]
fn enter_sends_a_prompt_or_a_command() {
    let mut c = C::new(Vec::new(), SIZE);
    assert_eq!(c.input(), "› ask moa, or / for commands");
    assert!(c.key(Key::Enter).is_empty(), "nothing to send");
    assert!(c.typed("   ").is_empty());
    assert!(c.key(Key::Enter).is_empty(), "a blank line sends nothing");
    c.key(Key::Ctrl('u'));

    assert!(c.typed("hello moa").is_empty());
    let outs = c.key(Key::Enter);
    let (tag, op) = c.send(outs);
    assert_eq!(
        op,
        Op::ConsolePrompt {
            text: "hello moa".into(),
            wait: false
        }
    );
    assert_eq!(c.input(), "› hello moa", "the draft stays until moa has it");
    c.reply(tag, Ok(done("moa has it")));
    assert_eq!(c.input(), "› ask moa, or / for commands");
    assert_eq!(c.footer(), "moa has it");

    c.typed("/ls");
    let outs = c.key(Key::Enter);
    let (tag, op) = c.send(outs);
    assert_eq!(op, Op::ConsoleCommand { line: "/ls".into() });
    c.reply(tag, Ok(done("2 sessions need you")));
    assert_eq!(c.input(), "› ask moa, or / for commands");
    let body = c.body();
    assert!(body.contains(&"▸ /ls".to_string()), "{body:#?}");
    assert!(
        body.contains(&"┊ 2 sessions need you".to_string()),
        "{body:#?}"
    );

    // A paste's line breaks become spaces.
    c.panel.paste("one\ntwo");
    assert_eq!(c.input(), "› one two");
}

/// B8: in the console every letter is typed, `j`, `k`, `g`, `G`, space and `q` among them;
/// none moves the feed, and Home and End move the cursor, not the feed.
#[test]
fn letters_never_move_the_list() {
    let mut c = C::new(notes(1..=40), SIZE);
    let before = c.body();
    assert!(c.typed("jkgG q").is_empty());
    c.key(Key::Home);
    c.key(Key::End);
    assert_eq!(c.input(), "› jkgG q");
    assert_eq!(c.body(), before);
}

/// B7: Up and Down walk your prompts from the rows (moa's own left out, F20), then the lines
/// sent here whose rows haven't come; Down past the newest brings the draft back.
#[test]
fn up_and_down_walk_the_history() {
    let mut morning = row(3, "you", "Morning brief");
    morning.automatic = true;
    let mut c = C::new(
        vec![
            row(1, "you", "first"),
            row(2, "moa", "ok"),
            morning,
            row(4, "you", "second"),
        ],
        SIZE,
    );
    c.typed("draft");
    for want in ["second", "first", "first"] {
        assert!(c.key(Key::Up).is_empty());
        assert_eq!(c.input(), format!("› {want}"));
    }
    for want in ["second", "draft"] {
        c.key(Key::Down);
        assert_eq!(c.input(), format!("› {want}"));
    }

    // A line sent here is in the history before its row comes, and once after.
    c.key(Key::Esc);
    c.typed("third");
    let outs = c.key(Key::Enter);
    let (tag, _) = c.send(outs);
    c.reply(tag, Ok(done("moa has it")));
    c.key(Key::Up);
    assert_eq!(c.input(), "› third");
    c.key(Key::Esc);
    c.add(vec![row(5, "you", "third")]);
    for want in ["third", "second"] {
        c.key(Key::Up);
        assert_eq!(c.input(), format!("› {want}"));
    }
}

/// B7: the history is in the order things were sent: a command typed before a prompt is older
/// than the prompt once its row comes. A pressed prompt button is none of yours: it never
/// joins the history, and Enter still sends while it waits.
#[test]
fn the_history_keeps_the_order_sent() {
    let mut c = C::new(Vec::new(), SIZE);
    c.typed("/brief");
    let outs = c.key(Key::Enter);
    let (tag, _) = c.send(outs);
    c.reply(tag, Ok(done("the brief")));
    let mut hello = row(1, "you", "hello");
    hello.at = c.ctx.now.0 + 1_000;
    c.add(vec![hello]);
    for want in ["hello", "/brief"] {
        c.key(Key::Up);
        assert_eq!(c.input(), format!("› {want}"));
    }
    c.key(Key::Esc);

    c.bar(&["say hi"]);
    c.screen();
    c.key(Key::Tab);
    let outs = c.key(Key::Enter);
    let (tag, op) = c.send(outs);
    assert!(matches!(op, Op::ConsolePrompt { .. }), "{op:?}");
    c.typed("next");
    let outs = c.key(Key::Enter);
    assert!(matches!(outs[..], [Out::Send(_)]), "{outs:?}");
    c.reply(tag, Ok(done("ok")));
    c.key(Key::Up);
    assert_ne!(c.input(), "› say hi");
}

/// B8: Esc while moa thinks cancels its turn, the draft kept; the head line says so.
#[test]
fn esc_cancels_while_thinking() {
    let mut c = C::new(vec![row(1, "you", "go")], SIZE);
    c.phase("thinking");
    assert!(c.head().ends_with("thinking… Esc cancels"), "{}", c.head());
    c.typed("next");
    assert_eq!(
        c.key(Key::Esc),
        vec![Out::Send(Op::ConsoleCancel(NoParams))]
    );
    assert_eq!(c.input(), "› next");
}

/// B8: Esc with moa idle clears the input.
#[test]
fn esc_clears_when_idle() {
    let mut c = C::new(Vec::new(), SIZE);
    c.typed("abc");
    assert!(c.key(Key::Esc).is_empty());
    assert_eq!(c.input(), "› ask moa, or / for commands");
}

/// B7: a refused prompt keeps the draft, the refusal in the footer, and Enter can send it
/// again.
#[test]
fn a_refused_prompt_keeps_the_draft() {
    let mut c = C::new(Vec::new(), SIZE);
    c.typed("hello");
    let outs = c.key(Key::Enter);
    let (tag, _) = c.send(outs);
    assert!(c.key(Key::Enter).is_empty(), "one line at a time");
    assert_eq!(c.footer(), STILL_WAITING);
    c.reply(tag, Err("moa is still answering".into()));
    assert_eq!(c.input(), "› hello");
    assert_eq!(c.footer(), "moa is still answering");
    let outs = c.key(Key::Enter);
    c.send(outs);
}

/// B8, F15: scrolled up, the feed stays where it is as rows come below (the head line counts
/// them) and as a page comes above.
#[test]
fn scrolled_up_stays_put() {
    let mut c = C::new(notes(101..=140), SIZE);
    c.key(Key::PageUp);
    let up = c.body();
    assert!(up[0].starts_with("↑ "), "{up:#?}");
    assert!(!c.head().contains("new"));
    c.add(notes(141..=143));
    assert_eq!(c.body(), up);
    assert!(c.head().ends_with(" · ↓ 3 new"), "{}", c.head());

    // Up to the top, where PgUp asks for a page; it comes, and the view stays.
    let page = loop {
        let outs = c.key(Key::PageUp);
        if !outs.is_empty() {
            break outs;
        }
    };
    let (tag, op) = c.send(page);
    assert_eq!(
        op,
        Op::ConsolePage {
            before: 101,
            n: PAGE
        }
    );
    let top = c.body();
    let rows: Vec<FeedRow> = notes(1..=100);
    c.reply(
        tag,
        Ok(Done {
            text: "100 older rows".into(),
            data: json!({ "rows": rows }),
        }),
    );
    let after = c.body();
    assert!(after[0].starts_with("↑ "), "{after:#?}");
    assert_eq!(after[1..], top[1..], "the view stays where it was");
}

/// B8: at the bottom the feed follows what comes; PgDn back to the bottom follows again.
#[test]
fn at_the_bottom_it_follows() {
    let mut c = C::new(notes(1..=40), SIZE);
    assert!(c.body().last().unwrap().ends_with("· row 40"));
    c.add(notes(41..=43));
    assert!(c.body().last().unwrap().ends_with("· row 43"));
    c.key(Key::PageUp);
    c.key(Key::PageDown);
    c.add(notes(44..=44));
    assert!(c.body().last().unwrap().ends_with("· row 44"));
    assert!(!c.head().contains("new"));
}

/// B2: a page that overlaps the topic's rows shows each row once.
#[test]
fn paging_never_duplicates() {
    let mut c = C::new(notes(101..=200), TALL);
    let outs = c.key(Key::PageUp);
    let (tag, op) = c.send(outs);
    assert_eq!(
        op,
        Op::ConsolePage {
            before: 101,
            n: PAGE
        }
    );
    let rows = notes(50..=149);
    c.reply(
        tag,
        Ok(Done {
            text: "100 older rows".into(),
            data: json!({ "rows": rows }),
        }),
    );
    let body = c.body();
    for id in 50..=200 {
        let end = format!(" row {id}");
        let n = body.iter().filter(|l| l.ends_with(&end)).count();
        assert_eq!(n, 1, "row {id} shows {n} times");
    }
    assert_eq!(body.iter().filter(|l| l.contains("row ")).count(), 151);

    // The topic's tail moves on: the rows that leave it stay, with no gap.
    let tail: Vec<FeedRow> = notes(111..=210);
    c.data.feed = tail.into_iter().map(|r| (r.id, r)).collect();
    c.data.touch();
    let body = c.body();
    assert_eq!(body.iter().filter(|l| l.contains("row ")).count(), 161);
}

/// B2: a page that comes back short has reached the first row: PgUp asks no more.
#[test]
fn the_first_row_stops_paging() {
    let mut c = C::new(notes(21..=40), TALL);
    let outs = c.key(Key::PageUp);
    let (tag, op) = c.send(outs);
    assert_eq!(
        op,
        Op::ConsolePage {
            before: 21,
            n: PAGE
        }
    );
    let rows = notes(1..=20);
    c.reply(
        tag,
        Ok(Done {
            text: "20 older rows".into(),
            data: json!({ "rows": rows }),
        }),
    );
    assert!(c.body().iter().any(|l| l.ends_with(" row 1")));
    assert!(c.key(Key::PageUp).is_empty());
    assert!(c.key(Key::PageUp).is_empty());
}

/// B8: no second page is asked for while one is out; one that couldn't be sent, or whose
/// connection ended, is asked for again.
#[test]
fn one_page_request_at_a_time() {
    let mut c = C::new(notes(201..=240), TALL);
    let outs = c.key(Key::PageUp);
    let (tag, _) = c.send(outs);
    assert!(c.key(Key::PageUp).is_empty(), "one is out");
    let rows = notes(101..=200);
    c.reply(
        tag,
        Ok(Done {
            text: "100 older rows".into(),
            data: json!({ "rows": rows }),
        }),
    );
    let outs = c.key(Key::PageUp);
    assert_eq!(
        outs,
        vec![Out::Send(Op::ConsolePage {
            before: 101,
            n: PAGE
        })]
    );
    // That send never went: the panel forgets it, and asks again.
    c.panel.forget_replies();
    let outs = c.key(Key::PageUp);
    assert_eq!(
        outs,
        vec![Out::Send(Op::ConsolePage {
            before: 101,
            n: PAGE
        })]
    );
}

/// B10: an error is a `✗` line in the feed, the same text at most once in 30 s; the footer
/// shows it every time.
#[test]
fn an_error_shows_once_per_30_s() {
    let mut c = C::new(Vec::new(), SIZE);
    let now = c.ctx.now.0;
    let errors = |c: &mut C, what: &str| c.body().iter().filter(|l| *l == what).count();
    c.panel.error("moa's agent couldn't start", UtcMs(now));
    assert_eq!(errors(&mut c, "✗ moa's agent couldn't start"), 1);
    c.panel
        .error("moa's agent couldn't start", UtcMs(now + 29_000));
    assert_eq!(c.footer(), "moa's agent couldn't start");
    assert_eq!(errors(&mut c, "✗ moa's agent couldn't start"), 1);
    c.panel.error("another", UtcMs(now + 29_000));
    assert_eq!(errors(&mut c, "✗ another"), 1);
    c.panel
        .error("moa's agent couldn't start", UtcMs(now + 30_000));
    assert_eq!(errors(&mut c, "✗ moa's agent couldn't start"), 2);

    // Only the newest 50 error lines are kept.
    let mut c = C::new(Vec::new(), TALL);
    for n in 0..60 {
        c.panel.error(&format!("error {n}"), UtcMs(now + n));
    }
    let body = c.body();
    assert_eq!(
        body.iter().filter(|l| l.starts_with("✗ error ")).count(),
        50
    );
    assert!(body.contains(&"✗ error 59".to_string()));
    assert!(!body.contains(&"✗ error 9".to_string()));

    // Any other view keeps its footer only.
    let mut tree = Panel::new(View::Tree);
    tree.error("boom", UtcMs(now));
    let screen = tree.screen(&c.data, &c.ctx, SIZE);
    assert_eq!(screen.footer.text, "boom");
}

/// The two lines with buttons the Tab tests use: `/ls` and `/brief` on the first, `/attention`
/// on the second.
fn buttons() -> Vec<FeedRow> {
    vec![
        row(10, "moa", "try `/ls` or `/brief`"),
        row(11, "moa", "or `/attention`"),
    ]
}

/// F12, F26: with the input empty, Tab and BTab move the focus in reading order (the text's
/// buttons, then the bar's), wrapping; the first Tab takes the newest text button; Enter
/// presses the focused one. Once something is typed, Enter sends the draft.
#[test]
fn tab_hops_and_enter_presses() {
    let mut c = C::new(buttons(), SIZE);
    c.bar(&["/help", "/ls"]);
    assert_eq!(c.focused(), None);
    let walk = [
        ("[/attention]", false),
        ("[/help]", true),
        ("[/ls]", true),
        ("[/ls]", false),
        ("[/brief]", false),
        ("[/attention]", false),
    ];
    for (label, in_bar) in walk {
        assert!(c.key(Key::Tab).is_empty());
        assert_eq!(c.focused(), Some((label.to_string(), in_bar)));
    }
    c.key(Key::BackTab);
    assert_eq!(c.focused(), Some(("[/brief]".to_string(), false)));
    for _ in 0..2 {
        c.key(Key::BackTab);
    }
    assert_eq!(c.focused(), Some(("[/ls]".to_string(), true)), "wraps back");
    for _ in 0..2 {
        c.key(Key::BackTab);
    }
    assert_eq!(c.focused(), Some(("[/attention]".to_string(), false)));
    let outs = c.key(Key::Enter);
    let (tag, op) = c.send(outs);
    assert_eq!(
        op,
        Op::ConsoleCommand {
            line: "/attention".into()
        }
    );
    c.reply(tag, Ok(done("nothing needs you")));
    assert!(c.body().contains(&"▸ /attention".to_string()));

    // Typing drops the focus: Enter sends what is typed.
    c.typed("hi");
    assert_eq!(c.focused(), None);
    let outs = c.key(Key::Enter);
    let (_, op) = c.send(outs);
    assert_eq!(
        op,
        Op::ConsolePrompt {
            text: "hi".into(),
            wait: false
        }
    );

    // With no text button in sight, the first Tab takes the bar's first.
    let mut c = C::new(Vec::new(), SIZE);
    c.bar(&["/help", "/ls"]);
    c.key(Key::Tab);
    assert_eq!(c.focused(), Some(("[/help]".to_string(), true)));
}

/// The focused button stays focused when the feed is rebuilt: a row comes below it, and rows
/// come above it (which shifts every button's number).
#[test]
fn focus_stays_on_its_button() {
    let mut c = C::new(buttons(), SIZE);
    c.key(Key::Tab);
    c.key(Key::BackTab);
    assert_eq!(c.focused(), Some(("[/brief]".to_string(), false)));
    c.add(vec![row(12, "moa", "and `/brief` again, `/help`")]);
    assert_eq!(c.focused(), Some(("[/brief]".to_string(), false)));
    c.add(vec![row(5, "moa", "earlier: `/ls` and `/brief`")]);
    assert_eq!(c.focused(), Some(("[/brief]".to_string(), false)));
    // Still the one on its own line: Tab goes on to `/attention`, just below it.
    c.key(Key::Tab);
    assert_eq!(c.focused(), Some(("[/attention]".to_string(), false)));
}

/// B5: the bar is drawn under the input line, and a click on its button presses it.
#[test]
fn a_click_on_the_bar_presses_it() {
    let mut c = C::new(Vec::new(), SIZE);
    c.bar(&["/brief", "/help"]);
    let screen = c.screen();
    let bar = screen.geometry.bar.unwrap();
    let input = screen.geometry.input.unwrap();
    assert_eq!(bar.y, input.y + 1, "under the input line");
    let line = text(&screen.bar[0]);
    assert_eq!(line, "[/brief] [/help]");
    let x = bar.x + u16::try_from(line.find("[/help]").unwrap()).unwrap();
    let handled = c
        .panel
        .mouse(Mouse::Click { x, y: bar.y }, &c.data, &c.ctx, SIZE);
    assert_eq!(
        handled.out,
        vec![Out::Send(Op::ConsoleCommand {
            line: "/help".into()
        })]
    );
    let handled = c
        .panel
        .mouse(Mouse::Click { x: x - 1, y: bar.y }, &c.data, &c.ctx, SIZE);
    assert!(handled.out.is_empty(), "between the buttons");
}

/// F10: `/quit`, typed or pressed, is the console's own and never sent: one you started
/// quits; one in moa's layout says why it stays.
#[test]
fn quit_is_the_consoles_own() {
    for managed in [false, true] {
        let mut c = C::new(vec![row(1, "moa", "or `/quit`")], SIZE);
        c.panel.set_managed(managed);
        c.typed("/quit");
        let typed = c.key(Key::Enter);
        if managed {
            assert_eq!(c.footer(), CONSOLE_MANAGED, "the typed line says why");
        }
        let screen = c.screen();
        let (y, line) = screen
            .body
            .iter()
            .enumerate()
            .find(|(_, (l, _))| text(l).contains("[/quit]"))
            .map(|(y, (l, _))| (y, text(l)))
            .expect("the button is drawn");
        let col = line[..line.find('[').unwrap()].chars().count();
        let g = screen.geometry.body;
        let (x, y) = (
            g.x + u16::try_from(col).unwrap(),
            g.y + u16::try_from(y).unwrap(),
        );
        let pressed = c
            .panel
            .mouse(Mouse::Click { x, y }, &c.data, &c.ctx, SIZE)
            .out;
        if managed {
            assert!(typed.is_empty() && pressed.is_empty());
            assert_eq!(c.footer(), CONSOLE_MANAGED);
        } else {
            assert_eq!((typed, pressed), (vec![Out::Quit], vec![Out::Quit]));
        }
    }
}

/// B9: the frame is `◆ moa · <brain>`; the head line names the thread and moa's state, and
/// it alone makes the screen tick while moa thinks.
#[test]
fn the_frame_names_the_brain_and_the_thread() {
    let mut c = C::new(vec![row(1, "you", "go")], SIZE);
    let screen = c.screen();
    assert_eq!(screen.title.as_deref(), Some("◆ moa · default"));
    assert_eq!(c.head(), "thread 0193b4f0 · ● ready");
    assert!(!c.screen().animate);
    c.phase("thinking");
    assert!(c.screen().animate, "the spinner in the head line ticks");
    c.panel.set_link_up(false);
    assert_eq!(c.head(), "connecting…");
    assert!(!c.screen().animate);
}

/// The focus goes when its button scrolls out of sight: Enter then presses nothing.
#[test]
fn a_focus_out_of_sight_goes() {
    let mut rows = notes(1..=20);
    rows.push(row(30, "moa", "try `/ls`"));
    let mut c = C::new(rows, SIZE);
    c.key(Key::Tab);
    assert_eq!(c.focused(), Some(("[/ls]".to_string(), false)));
    c.add(notes(31..=50));
    assert_eq!(c.focused(), None);
    assert!(c.key(Key::Enter).is_empty(), "nothing in sight to press");

    // Scrolled away by a key, too.
    let mut rows = vec![row(1, "moa", "try `/ls`")];
    rows.extend(notes(2..=40));
    let mut c = C::new(rows, SIZE);
    while !c.body().iter().any(|l| l.contains("[/ls]")) {
        c.key(Key::PageUp);
    }
    c.key(Key::Tab);
    assert_eq!(c.focused(), Some(("[/ls]".to_string(), false)));
    c.key(Key::PageDown);
    assert_eq!(c.focused(), None);
    assert!(c.key(Key::Enter).is_empty());
}

/// F15: a page that changes what is drawn above a row (a thread separator added, or the
/// row's turn header gone into an earlier turn) doesn't move a view anchored inside that
/// row, even one taller than the view.
#[test]
fn a_page_above_a_tall_row_keeps_the_view() {
    let items: Vec<String> = (1..=30).map(|i| format!("- item {i}")).collect();
    let tall = row(101, "moa", &items.join("\n"));
    let other = |mut r: FeedRow| {
        r.thread = "0193aaaa-bbbb-7000-8000-000000000002".into();
        r
    };
    let pages: [(&str, Vec<FeedRow>); 2] = [
        (
            "another thread: a separator",
            vec![
                other(row(50, "you", "earlier")),
                other(row(51, "moa", "yes")),
            ],
        ),
        (
            "the same turn: no header",
            vec![row(50, "you", "go on"), row(51, "moa", "first part")],
        ),
    ];
    for (what, page) in pages {
        let mut c = C::new(vec![tall.clone(), row(102, "system", "end")], SIZE);
        let outs = loop {
            let outs = c.key(Key::PageUp);
            if !outs.is_empty() {
                break outs;
            }
        };
        let (tag, _) = c.send(outs);
        let before = c.body();
        assert!(before.iter().any(|l| l.ends_with("item 5")), "{before:#?}");
        c.reply(
            tag,
            Ok(Done {
                text: "2 older rows".into(),
                data: json!({ "rows": page }),
            }),
        );
        let after = c.body();
        assert!(after[0].starts_with("↑ "), "{what}: {after:#?}");
        assert_eq!(after[1..], before[1..], "{what}: the view stays");
    }
}

/// B5: a bar too long for one row wraps onto up to 3; the third ends in `…` when buttons are
/// left out. Only the drawn buttons are in the Tab order and can be clicked.
#[test]
fn a_long_bar_wraps_onto_three_rows() {
    const NARROW: Size = Size { w: 40, h: 16 };
    let mut c = C::new(Vec::new(), NARROW);
    c.bar(&[
        "/brief",
        "/attention",
        "/ls",
        "/help",
        "/resume lantern-rust-scaffold",
        "/focus kite-release-notes",
        "/layout apply cockpit-wide",
        "/suggestions",
    ]);
    let screen = c.screen();
    let g = screen.geometry;
    let bar = g.bar.unwrap();
    assert_eq!(bar.h, 3);
    assert_eq!(bar.y, g.input.unwrap().y + 1, "under the input line");
    assert_eq!(
        g.body.y + g.body.h,
        g.input.unwrap().y,
        "the feed gave the rows"
    );
    let rows: Vec<String> = screen.bar.iter().map(text).collect();
    assert_eq!(
        rows,
        [
            "[/brief] [/attention] [/ls] [/help]",
            "[/resume lantern-rust-scaffold]",
            "[/focus kite-release-notes] …",
        ]
    );
    // Tab walks the six drawn buttons and wraps.
    let mut seen = Vec::new();
    for _ in 0..7 {
        c.key(Key::Tab);
        seen.push(c.focused().unwrap().0);
    }
    assert_eq!(seen[6], seen[0], "{seen:?}");
    assert!(!seen.iter().any(|b| b.contains("layout")), "{seen:?}");
    // The third row's button is pressed by a click.
    let x = bar.x + 3;
    let handled = c
        .panel
        .mouse(Mouse::Click { x, y: bar.y + 2 }, &c.data, &c.ctx, NARROW);
    assert_eq!(
        handled.out,
        vec![Out::Send(Op::ConsoleCommand {
            line: "/focus kite-release-notes".into()
        })]
    );
}

/// The palette's lines, as text, the chosen one marked with `*`.
fn palette(c: &mut C) -> Vec<String> {
    let screen = c.screen();
    screen
        .palette
        .iter()
        .map(|l| {
            let star = l.spans.iter().any(|s| s.reversed);
            format!("{}{}", if star { "*" } else { "" }, text(l).trim())
        })
        .collect()
}

fn layouts(c: &mut C, names: &[&str]) {
    c.data.layouts.clear();
    for (index, name) in names.iter().enumerate() {
        c.data.layouts.insert(
            name.to_string(),
            LayoutRow {
                name: name.to_string(),
                index,
                leaves: Vec::new(),
                current: index == 0,
                error: None,
            },
        );
    }
    c.data.touch();
}

/// The id of the live session titled `title`.
fn id_of(c: &C, title: &str) -> String {
    let row = c
        .data
        .sessions
        .values()
        .find(|r| r.session.title.as_deref() == Some(title))
        .unwrap();
    row.session.id.to_string()
}

/// B11: `/` shows the commands (not the hidden ones), at most 8 lines, directly above the
/// input line; `/re` narrows them to `/resume`.
#[test]
fn slash_opens_the_palette() {
    let mut c = C::new(notes(1..=20), SIZE);
    assert!(palette(&mut c).is_empty());
    c.typed("/");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 8, "{lines:#?}");
    assert!(lines[0].starts_with("*/attention"), "{lines:#?}");
    let g = c.screen().geometry;
    let (p, input) = (g.palette.unwrap(), g.input.unwrap());
    assert_eq!((p.h, p.y + p.h), (8, input.y), "right above the input");
    // The hidden command is in no list, even typed in full.
    c.typed("sugg");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(lines[0].contains("/suggestions"), "{lines:#?}");
    c.key(Key::Ctrl('u'));
    c.typed("/re");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(lines[0].starts_with("*/resume"), "{lines:#?}");
    // A line that isn't a command opens nothing.
    c.key(Key::Ctrl('u'));
    c.typed("re");
    assert!(palette(&mut c).is_empty());
}

/// The palette completes the last word, so it shows only with the cursor at the end.
#[test]
fn the_palette_waits_for_the_cursor_at_the_end() {
    let mut c = C::new(Vec::new(), SIZE);
    c.typed("/re");
    assert_eq!(palette(&mut c).len(), 1);
    c.key(Key::Left);
    assert!(palette(&mut c).is_empty());
    c.key(Key::Tab);
    assert_eq!(
        c.input(),
        "› /re",
        "Tab completes nothing away from the end"
    );
    c.key(Key::End);
    assert_eq!(palette(&mut c).len(), 1);
}

/// B11, F9: `/resume ` lists the live sessions' titles, narrowed by what is typed; layout
/// names come after `/layout apply ` and `/layout show `, not after a bare `/layout `.
#[test]
fn after_resume_it_lists_sessions() {
    let mut c = C::new(Vec::new(), SIZE);
    layouts(&mut c, &["Work", "Review"]);
    c.typed("/resume ");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 8, "{lines:#?}");
    assert!(
        lines.iter().all(|l| !l.contains("Gone")),
        "archived: {lines:#?}"
    );
    c.typed("rel");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(lines[0].contains("Release notes"), "{lines:#?}");
    // Only the first argument of /resume is a session.
    c.key(Key::Ctrl('u'));
    c.typed("/resume x rel");
    assert!(palette(&mut c).is_empty());
    // /archive takes several.
    c.key(Key::Ctrl('u'));
    c.typed("/archive x rel");
    assert_eq!(palette(&mut c).len(), 1);

    for open in ["/layout apply ", "/layout show "] {
        c.key(Key::Ctrl('u'));
        c.typed(open);
        let lines = palette(&mut c);
        assert_eq!(lines.len(), 2, "{open}: {lines:#?}");
        assert!(lines[0].contains("Work") && lines[1].contains("Review"));
    }
    c.typed("rev");
    let lines = palette(&mut c);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    for bare in ["/layout ", "/layout list ", "/layout set "] {
        c.key(Key::Ctrl('u'));
        c.typed(bare);
        assert!(palette(&mut c).is_empty(), "{bare}");
    }
}

/// B11: Tab puts the chosen entry in the word's place, Up and Down choose; for a session the
/// text is its id, quoted only if needed.
#[test]
fn tab_completes_and_up_down_choose() {
    let mut c = C::new(Vec::new(), SIZE);
    layouts(&mut c, &["Work", "my layout"]);
    c.typed("/re");
    assert!(c.key(Key::Tab).is_empty());
    assert_eq!(c.input(), "› /resume ", "a name leads on to its argument");
    assert_eq!(palette(&mut c).len(), 8, "now the sessions");

    c.typed("release");
    assert!(c.key(Key::Tab).is_empty());
    let id = id_of(&c, "Release notes");
    assert_eq!(c.input(), format!("› /resume {id}"));
    assert!(palette(&mut c).is_empty(), "a finished word closes it");

    // Down and Up choose, and wrap.
    c.key(Key::Ctrl('u'));
    c.typed("/layout apply ");
    assert_eq!(palette(&mut c), ["*Work  current", "my layout"]);
    c.key(Key::Down);
    assert_eq!(palette(&mut c), ["Work  current", "*my layout"]);
    c.key(Key::Down);
    assert!(palette(&mut c)[0].starts_with('*'), "wraps");
    c.key(Key::Up);
    assert!(palette(&mut c)[1].starts_with('*'), "wraps back");
    assert!(c.key(Key::Tab).is_empty());
    assert_eq!(
        c.input(),
        "› /layout apply 'my layout'",
        "quoted: it has a space"
    );

    // Up and Down walk the history again once it is closed.
    assert!(palette(&mut c).is_empty());
    c.key(Key::Ctrl('u'));
    c.typed("/ls");
    let outs = c.key(Key::Enter);
    let (tag, _) = c.send(outs);
    c.reply(tag, Ok(done("ok")));
    c.key(Key::Up);
    assert_eq!(c.input(), "› /ls");
    assert!(palette(&mut c).is_empty(), "a recalled line opens none");
}

/// B11: Esc closes the palette first, then does what it did before (clears the draft); typing
/// opens it again.
#[test]
fn esc_closes_the_palette_first() {
    let mut c = C::new(Vec::new(), SIZE);
    c.typed("/re");
    assert_eq!(palette(&mut c).len(), 1);
    assert!(c.key(Key::Esc).is_empty());
    assert!(palette(&mut c).is_empty());
    assert_eq!(c.input(), "› /re", "the draft stays");
    // While closed, Tab and Up leave the palette alone.
    c.key(Key::Esc);
    assert_eq!(
        c.input(),
        "› ask moa, or / for commands",
        "the second Esc clears"
    );
    c.typed("/re");
    assert_eq!(palette(&mut c).len(), 1, "reopens");
    c.key(Key::Esc);
    c.typed("s");
    assert_eq!(palette(&mut c).len(), 1, "typing opens it again");

    // While moa thinks, the open palette still takes Esc before the cancel.
    c.phase("thinking");
    assert!(c.key(Key::Esc).is_empty());
    assert_eq!(
        c.key(Key::Esc),
        vec![Out::Send(Op::ConsoleCancel(NoParams))]
    );
}

/// F16: the palette takes rows from the feed, and the feed keeps one.
#[test]
fn the_palette_leaves_the_feed_a_row() {
    let mut c = C::new(notes(1..=20), Size { w: 80, h: 11 });
    c.typed("/");
    let g = c.screen().geometry;
    assert!(g.body.h >= 1, "{g:?}");
    let p = g.palette.unwrap();
    assert_eq!(p.y + p.h, g.input.unwrap().y);
    assert_eq!(palette(&mut c).len(), usize::from(p.h));
    // Down to the last of the commands: the chosen line is always drawn.
    for _ in 0..4 {
        c.key(Key::Down);
    }
    assert!(palette(&mut c).iter().any(|l| l.starts_with('*')));
}

/// Once Esc closed the palette on a `/` draft, Tab and BackTab are Task 8's: they type or do
/// nothing to the draft, and never complete or choose.
#[test]
fn a_closed_palette_leaves_tab_alone() {
    let mut c = C::new(Vec::new(), SIZE);
    c.bar(&["/brief", "/ls"]);
    c.screen();
    c.typed("/re");
    c.key(Key::Esc);
    assert!(palette(&mut c).is_empty());
    c.key(Key::Tab);
    c.key(Key::BackTab);
    assert_eq!(c.input(), "› /re", "no completion");
    // Deleting back and retyping reopens it.
    c.key(Key::Backspace);
    c.typed("e");
    assert_eq!(palette(&mut c).len(), 1);
    // With the draft cleared, Tab moves button focus as before.
    c.key(Key::Ctrl('u'));
    c.key(Key::Tab);
    assert!(c.focused().is_some());
}
