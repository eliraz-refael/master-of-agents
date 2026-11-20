//! The picture, on the Lantern fixture: its model, its tabs and their keys, `q`, the activity
//! strip's flash, and the views this build doesn't draw.

use moa_core::UtcMs;
use moa_core::model::{ActivityId, TaskId, TaskStatus};
use moa_core::op::{Op, TaskSet};
use moa_core::panel::View;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Span, Tone, debug_lines};
use moa_view::panel::{Out, Panel, Screen, debug_screen};
use moa_view::views::{self, ViewUi};

const SIZE: Size = Size { w: 80, h: 24 };

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    fn new(view: View) -> T {
        let (data, ctx) = lantern();
        let mut panel = Panel::new(view);
        panel.screen(&data, &ctx, SIZE);
        T { panel, data, ctx }
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, SIZE)
    }

    fn screen(&mut self) -> Screen {
        self.panel.screen(&self.data, &self.ctx, SIZE)
    }

    /// The tab the picture shows.
    fn tab(&mut self) -> usize {
        self.screen().tabs.expect("the picture has tabs").1
    }
}

/// The screen's body lines, without their marks.
fn body(screen: &Screen) -> Vec<Line> {
    screen.body.iter().map(|(l, _)| l.clone()).collect()
}

/// The ctx `ms` milliseconds later, at `tick`.
fn later(ctx: &Ctx, ms: i64, tick: u64) -> Ctx {
    Ctx::at(
        UtcMs(ctx.now.0 + ms),
        ctx.tz.clone(),
        tick,
        ctx.home.clone(),
        ctx.is_dir,
    )
}

#[test]
fn picture_model() {
    let mut t = T::new(View::Picture);
    insta::assert_snapshot!("picture_model", debug_screen(&t.screen()));
}

#[test]
fn tabs_switch_with_tab_numbers_and_arrows() {
    let mut t = T::new(View::Picture);
    let screen = t.screen();
    let (names, at) = screen.tabs.clone().expect("tabs");
    assert_eq!(
        names,
        [
            "Plan", "Today", "Projects", "Sessions", "Activity", "Status"
        ]
    );
    assert_eq!(at, 0);
    assert_eq!(screen.title, None, "the picture has no frame");

    let steps = [
        (Key::Tab, 1),
        (Key::Right, 2),
        (Key::Char('l'), 3),
        (Key::BackTab, 2),
        (Key::Left, 1),
        (Key::Char('h'), 0),
        (Key::Char('h'), 5),
        (Key::Tab, 0),
        (Key::Char('4'), 3),
        (Key::Char('6'), 5),
        (Key::Char('1'), 0),
    ];
    for (k, tab) in steps {
        assert_eq!(t.key(k), [], "{k:?}");
        assert_eq!(t.tab(), tab, "{k:?}");
    }

    // Each tab shows its view's lines, as wide as the screen.
    let tabs = [
        View::Plan,
        View::Today,
        View::Projects,
        View::Tree,
        View::Activity,
        View::Status,
    ];
    for (i, view) in tabs.into_iter().enumerate() {
        t.key(Key::Char(char::from(b'1' + i as u8)));
        let screen = t.screen();
        let want = views::build(view, &t.data, &t.ctx, &ViewUi::default(), SIZE.w);
        let shown = body(&screen);
        assert_eq!(shown[..], want[..shown.len()], "{view:?}");
    }

    // A tab opens with its first item selected, scrolled to the top.
    t.key(Key::Char('4'));
    let first = t.panel.selected().cloned();
    assert_eq!(first, Some(Item::Group("Private".to_string())));
    for _ in 0..12 {
        t.key(Key::Char('j'));
    }
    let moved = t.panel.selected().cloned();
    assert_ne!(moved, first);
    // The shown tab's own digit keeps its selection.
    t.key(Key::Char('4'));
    t.screen();
    assert_eq!(t.panel.selected().cloned(), moved, "the same tab stays put");
    t.key(Key::Char('5'));
    t.key(Key::Char('4'));
    t.screen();
    assert_eq!(t.panel.selected().cloned(), first);
    let screen = t.screen();
    let tree = views::build(View::Tree, &t.data, &t.ctx, &ViewUi::default(), SIZE.w);
    assert_eq!(screen.body[0].0, tree[0], "back at the top");
}

#[test]
fn the_tabs_rows_keep_their_views_keys() {
    let mut t = T::new(View::Picture);

    // The plan: `x` drops a task and never closes a session (V21).
    t.screen();
    assert_eq!(
        t.panel.selected(),
        Some(&Item::Session(moa_view::fixture::session(1)))
    );
    assert_eq!(t.key(Key::Char('x')), []);
    assert!(!t.screen().footer.text.starts_with("close"));
    t.key(Key::Char('j'));
    assert_eq!(t.panel.selected(), Some(&Item::Task(TaskId(1))));
    assert_eq!(
        t.key(Key::Char('x')),
        [Out::Send(Op::TaskSet(TaskSet {
            id: 1,
            status: Some(TaskStatus::Dropped.as_str().to_string()),
            ..TaskSet::default()
        }))]
    );

    // The sessions tab: Enter on a group folds it, `n` asks the tree's questions.
    t.key(Key::Char('4'));
    t.screen();
    let out = t.key(Key::Enter);
    assert!(
        matches!(&out[..], [Out::Send(Op::GroupFold { path, .. })] if path == "Private"),
        "{out:?}"
    );
    assert_eq!(t.key(Key::Char('n')), []);
    let footer = t.screen().footer.text;
    assert!(footer.starts_with("new session in "), "{footer}");
    assert_eq!(t.key(Key::Esc), []);

    // Its footer hints are the tab's own on the sessions and status tabs, else the picture's
    // for that tab, naming only the keys that work there.
    let hint = |t: &mut T| t.screen().footer.text;
    t.key(Key::Char('1'));
    let plan = hint(&mut t);
    assert!(plan.contains("x drop"), "{plan}");
    assert!(plan.contains("d done"), "{plan}");
    assert!(!plan.contains("x close"), "{plan}");
    t.key(Key::Char('2'));
    let today = hint(&mut t);
    assert!(today.contains("y/n suggestion"), "{today}");
    // Its waiting and working sessions take `a` and `x`.
    assert!(
        today.contains("a archive") && today.contains("x close"),
        "{today}"
    );
    t.key(Key::Char('3'));
    let projects = hint(&mut t);
    // An opened project's sessions take `a` and `x`.
    assert!(
        projects.contains("a archive") && projects.contains("x close"),
        "{projects}"
    );
    t.key(Key::Char('5'));
    assert_eq!(hint(&mut t), "1-6/tab · j/k scroll · q");
    t.key(Key::Char('6'));
    let status = hint(&mut t);
    assert_eq!(status, moa_view::keys::hint(View::Status, false));
    // Its sessions take `a`, `x`, `n` and `N`.
    assert!(
        status.contains("a archive") && status.contains("x close") && status.contains("n/N new"),
        "{status}"
    );
    t.key(Key::Char('4'));
    assert!(
        hint(&mut t).starts_with("⏎/click open · n new"),
        "the tree's"
    );
    t.key(Key::Char('2'));
    assert!(hint(&mut t).starts_with("1-6/tab"), "the picture's");
}

#[test]
fn q_quits_only_the_picture() {
    let mut t = T::new(View::Picture);
    assert_eq!(t.key(Key::Char('q')), [Out::Quit]);
    for view in [View::Tree, View::Status, View::Plan, View::Activity] {
        let mut t = T::new(view);
        assert_eq!(t.key(Key::Char('q')), [], "{view:?}");
        assert_eq!(t.key(Key::Ctrl('c')), [Out::Quit], "{view:?}");
    }

    // In a prompt, `q` is typed.
    let mut t = T::new(View::Picture);
    t.key(Key::Char('4'));
    t.screen();
    assert_eq!(t.key(Key::Char('+')), []);
    assert_eq!(t.key(Key::Char('q')), []);
    assert!(t.screen().footer.text.ends_with('q'));
}

/// L14: `q` leaves a picture moa manages where it is and says why, as Ctrl-c does; a
/// managed panel that quit would leave a hole in the layout. A picture you started quits.
#[test]
fn q_in_a_managed_picture_stays_and_says_so() {
    let mut t = T::new(View::Picture);
    t.panel.set_managed(true);
    assert!(!t.panel.quits(Key::Char('q')));
    assert_eq!(t.key(Key::Char('q')), []);
    assert_eq!(
        t.screen().footer.text,
        "this panel is part of moa's layout; Ctrl-a r repairs the layout"
    );
    // On every tab.
    t.key(Key::Char('3'));
    assert_eq!(t.key(Key::Char('q')), []);
    assert!(t.screen().footer.text.starts_with("this panel is part"));

    let mut t = T::new(View::Picture);
    assert!(t.panel.quits(Key::Char('q')));
    assert_eq!(t.key(Key::Char('q')), [Out::Quit]);
}

#[test]
fn the_strip_flashes_rows_under_4_seconds() {
    let (data, ctx) = lantern();
    let mut panel = Panel::new(View::Picture);

    // The newest seven, oldest first; the 2 s old row flashes.
    let screen = panel.screen(&data, &ctx, SIZE);
    let texts: Vec<String> = screen.strip.iter().map(Line::text).collect();
    assert_eq!(
        texts,
        [
            "09:09  added to today: \"Fix the login loop\"",
            "09:16  linked session 5e550002 → Lantern",
            "09:27  task \"Proofread chapter 1\": active → done",
            "09:40  suggests #1: Archive 3 ended sessions?",
            "09:48  Release notes asks: Ship it today?",
            "09:56  project Beacon: active → idea",
            "09:59 ✦Fix login needs you",
        ]
    );
    // The rule is the frame's colour with only ` activity ` dim, as the spike drew it.
    let rule = screen.rule.clone().expect("a rule above the strip");
    assert_eq!(moa_view::text::width(&rule.text()), 80);
    assert_eq!(rule.spans.len(), 3, "{rule:?}");
    assert_eq!(rule.spans[0], Span::new("─", Tone::Frame));
    assert_eq!(rule.spans[1], Span::new(" activity ", Tone::Dim));
    assert_eq!(rule.spans[2], Span::new("─".repeat(69), Tone::Frame));
    let narrow = views::picture::rule(5);
    assert_eq!(narrow.text(), "─ act");
    assert_eq!(narrow.spans.len(), 2);
    assert!(views::picture::rule(0).spans.is_empty());

    let fresh = &screen.strip[6];
    let at = data.activity[&ActivityId(8)].at;
    assert!(fresh.animated);
    assert_eq!(fresh.changes_at, Some(UtcMs(at.0 + 4_000)));
    assert!(fresh.spans.iter().all(|s| s.bold));
    assert_eq!(fresh.spans[0].tone, Tone::Dim, "a session's row");
    let old = &screen.strip[5];
    assert!(!old.animated);
    assert_eq!(old.changes_at, None);
    assert!(old.spans.iter().all(|s| !s.bold && !s.reversed));
    assert_eq!(old.spans[0].tone, Tone::You);

    // The brand wakes for the flash only while the strip draws it.
    let brand = views::picture::brand(&data, &ctx, 7);
    assert_eq!(brand.changes_at, Some(UtcMs(at.0 + 4_000)));
    assert_eq!(views::picture::brand(&data, &ctx, 0).changes_at, None);

    // Reversed on every other 250 ms: `(tick * 2 / 5) % 2 == 1`.
    for (tick, reversed) in [(0, false), (2, false), (3, true), (4, true), (5, false)] {
        let ctx = later(&ctx, 0, tick);
        let screen = panel.screen(&data, &ctx, SIZE);
        assert_eq!(
            screen.strip[6].spans.iter().all(|s| s.reversed),
            reversed,
            "tick {tick}"
        );
    }

    // 4 s after it, it is an ordinary row.
    let ctx = later(&ctx, 2_000, 3);
    let screen = panel.screen(&data, &ctx, SIZE);
    let last = &screen.strip[6];
    assert_eq!(last.text(), "09:59  Fix login needs you");
    assert!(!last.animated);
    assert!(last.spans.iter().all(|s| !s.bold && !s.reversed));

    // A screen too short for the strip has none, and no rule.
    let screen = panel.screen(&data, &ctx, Size { w: 80, h: 8 });
    assert!(screen.strip.is_empty());
    assert_eq!(screen.rule, None);
}

#[test]
fn unknown_views_say_not_yet() {
    let (data, ctx) = lantern();
    let cases = [
        (View::Console, "the console isn't in this build yet"),
        (View::Picker, "finding a session isn't in this build yet"),
        (View::Help, "help isn't in this build yet"),
        (View::Setup, "setup isn't in this build yet"),
    ];
    for (view, words) in cases {
        let lines = views::build(view, &data, &ctx, &ViewUi::default(), 80);
        assert_eq!(debug_lines(&lines), format!("[dim]{words}\n"), "{view:?}");
        let narrow = views::build(view, &data, &ctx, &ViewUi::default(), 10);
        assert_eq!(narrow[0].text(), words[..10], "{view:?}");
    }
    assert_eq!(views::not_yet(View::EmptySlot), None);
}

/// The agent spot with no agent in it says how to fill it (the spike's `EMPTY_SLOT`): dim,
/// with nothing to select, cut to the width.
#[test]
fn empty_slot_model() {
    let lines = views::empty_slot::build(80);
    insta::assert_snapshot!("empty_slot_model", debug_lines(&lines));
    let (data, ctx) = lantern();
    let built = views::build(View::EmptySlot, &data, &ctx, &ViewUi::default(), 80);
    assert_eq!(built, lines);
    assert!(lines.iter().all(|l| !l.selectable()));
    let narrow = views::empty_slot::build(10);
    assert_eq!(narrow[2].text(), "press Ente");
    assert!(narrow.iter().all(|l| l.text().chars().count() <= 10));
}
