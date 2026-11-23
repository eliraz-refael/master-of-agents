//! The console panel's keys, clicks and frame (B7–B10): typing to moa, the history, Esc,
//! Tab over the buttons, scrolling and paging back, the errors it shows, and `/quit` (F10).

use moa_core::UtcMs;
use moa_core::op::{Done, NoParams, Op};
use moa_core::panel::View;
use moa_core::publish::{BarRow, FeedRow, MoaRow};
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::Line;
use moa_view::panel::{CONSOLE_MANAGED, Mouse, Out, PAGE, Panel, Screen};
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
