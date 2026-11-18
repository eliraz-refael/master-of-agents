//! Scrolling and selection: headings come with their items, wrapped lines with theirs, and
//! the selection follows its item, not its index.

use moa_core::model::{SessionId, TaskId};
use moa_view::line::{Item, Line, Span, Tone, debug_lines};
use moa_view::list::ListState;

fn item(n: u128) -> Item {
    Item::Session(SessionId(uuid::Uuid::from_u128(n)))
}

/// A heading.
fn h() -> Line {
    Line::plain("heading", Tone::Head)
}

/// Item `n`'s line.
fn i(n: u128) -> Line {
    Line::plain(format!("item {n}"), Tone::Text).with_item(item(n))
}

/// A wrapped line of the item above.
fn m() -> Line {
    Line::wrapped(vec![Span::new("  more", Tone::Text)])
}

/// The index of item `n`'s line.
fn at(lines: &[Line], n: u128) -> usize {
    lines
        .iter()
        .position(|l| l.item == Some(item(n)))
        .expect("the item is listed")
}

fn sel(state: &ListState, lines: &[Line]) -> Option<Item> {
    state.selected(lines).cloned()
}

#[test]
fn follow_shows_the_headings_above_the_first_item() {
    let lines = vec![h(), h(), i(1), i(2), i(3), i(4), i(5), i(6)];
    let mut s = ListState::default();
    s.reselect(None, &lines);
    s.ends(&lines, true);
    s.follow(&lines, 3);
    assert!(s.top > 0);

    s.ends(&lines, false);
    s.follow(&lines, 3);
    assert_eq!(sel(&s, &lines), Some(item(1)));
    assert_eq!(s.top, 0, "the headings above the first item come into view");
}

#[test]
fn follow_brings_the_key_hints_with_the_last_item() {
    let lines = vec![
        i(1),
        i(2),
        i(3),
        i(4),
        i(5),
        i(6),
        i(7),
        i(8),
        i(9),
        h(),
        h(),
    ];
    let mut s = ListState::default();
    s.reselect(None, &lines);
    s.ends(&lines, true);
    s.follow(&lines, 4);
    assert_eq!(sel(&s, &lines), Some(item(9)));
    assert!(
        s.top + 4 >= lines.len(),
        "the last line is visible (top {})",
        s.top
    );

    // Any other item doesn't bring them.
    s.ends(&lines, false);
    s.follow(&lines, 4);
    for _ in 0..7 {
        s.step(&lines, true);
        s.follow(&lines, 4);
    }
    assert_eq!(sel(&s, &lines), Some(item(8)));
    assert_eq!(s.top, 4);
}

#[test]
fn wrapped_lines_scroll_and_click_with_their_item() {
    let lines = vec![h(), i(1), m(), m(), i(2), m(), i(3)];
    let s = ListState::default();
    assert_eq!(s.row_at(&lines, 2, 10), Some(at(&lines, 1)));
    assert_eq!(s.row_at(&lines, 3, 10), Some(at(&lines, 1)));
    assert_eq!(s.row_at(&lines, 5, 10), Some(at(&lines, 2)));

    // Selecting item 2 scrolls its wrapped line in with it.
    let mut s = ListState {
        sel: Some(at(&lines, 2)),
        ..ListState::default()
    };
    s.follow(&lines, 2);
    assert_eq!(s.top, 4);
    assert_eq!(s.row_at(&lines, 1, 2), Some(at(&lines, 2)));
}

#[test]
fn an_item_taller_than_the_body_shows_from_its_first_line() {
    let lines = vec![h(), i(1), i(2), m(), m(), m(), m(), h()];
    let mut s = ListState {
        sel: Some(at(&lines, 2)),
        ..ListState::default()
    };
    s.follow(&lines, 3);
    assert_eq!(s.top, at(&lines, 2));
}

#[test]
fn page_moves_by_selectable_lines_and_stops_at_the_ends() {
    let lines = vec![
        h(),
        i(0),
        i(1),
        h(),
        i(2),
        i(3),
        i(4),
        h(),
        i(5),
        i(6),
        i(7),
        i(8),
        i(9),
    ];
    let mut s = ListState::default();
    s.reselect(None, &lines);
    assert_eq!(sel(&s, &lines), Some(item(0)));

    let mut seen = Vec::new();
    for _ in 0..4 {
        s.page(&lines, 4, true);
        seen.push(sel(&s, &lines));
    }
    assert_eq!(
        seen,
        vec![Some(item(3)), Some(item(6)), Some(item(9)), Some(item(9))]
    );

    let mut seen = Vec::new();
    for _ in 0..4 {
        s.page(&lines, 4, false);
        seen.push(sel(&s, &lines));
    }
    assert_eq!(
        seen,
        vec![Some(item(5)), Some(item(2)), Some(item(0)), Some(item(0))]
    );
}

#[test]
fn the_wheel_moves_the_view_and_drags_the_selection_into_it() {
    let mut lines = vec![h()];
    lines.extend((0..20).map(i));
    let mut s = ListState::default();
    s.reselect(None, &lines);
    s.follow(&lines, 5);
    assert_eq!((s.top, s.sel), (0, Some(1)));

    s.wheel(&lines, 5, true);
    assert_eq!(s.top, 3);
    assert_eq!(s.sel, Some(3), "the nearest visible item");
    assert!(!s.tracking);
    s.follow(&lines, 5);
    assert_eq!(s.top, 3, "the view stays where it was scrolled");

    s.wheel(&lines, 5, true);
    assert_eq!((s.top, s.sel), (6, Some(6)));
    s.wheel(&lines, 5, false);
    assert_eq!((s.top, s.sel), (3, Some(6)), "a visible selection stays");

    for _ in 0..10 {
        s.wheel(&lines, 5, true);
    }
    assert_eq!(s.top, lines.len() - 5, "the wheel stops at the end");
    assert_eq!(s.sel, Some(16));

    s.step(&lines, true);
    assert!(s.tracking, "a key follows the selection again");
    assert_eq!(s.sel, Some(17));
}

#[test]
fn a_click_on_a_heading_is_ignored() {
    let lines = vec![h(), i(1), h(), i(2)];
    let s = ListState::default();
    assert_eq!(s.row_at(&lines, 0, 10), None);
    assert_eq!(s.row_at(&lines, 1, 10), Some(1));
    assert_eq!(s.row_at(&lines, 2, 10), None);
    assert_eq!(s.row_at(&lines, 3, 10), Some(3));
    assert_eq!(s.row_at(&lines, 4, 10), None, "below the last line");
    assert_eq!(s.row_at(&lines, 3, 3), None, "outside the body");
}

#[test]
fn the_selection_follows_its_item_when_rows_change() {
    let (a, b, c, d, e) = (1, 2, 3, 4, 5);
    let first = vec![h(), i(c), i(d), i(b), i(e)];
    let mut s = ListState::default();
    s.reselect(Some(&item(b)), &first);
    assert_eq!(s.sel, Some(3));

    let was = sel(&s, &first);
    let second = vec![h(), i(c), i(d), i(a), i(b), i(e)];
    s.reselect(was.as_ref(), &second);
    assert_eq!(s.sel, Some(4));
    assert_eq!(sel(&s, &second), Some(item(b)));

    let was = sel(&s, &second);
    let third = vec![h(), i(c), i(d), i(a), h(), h(), i(e)];
    s.reselect(was.as_ref(), &third);
    assert_eq!(s.sel, Some(3), "the nearest selectable line");
    assert_eq!(sel(&s, &third), Some(item(a)));

    let was = sel(&s, &third);
    s.reselect(was.as_ref(), &[h(), h()]);
    assert_eq!(s.sel, None);
    assert_eq!(sel(&s, &[h(), h()]), None);
}

#[test]
fn a_list_with_nothing_to_select_scrolls_with_keys() {
    let lines: Vec<Line> = (0..20).map(|_| h()).collect();
    let body = 5;
    let mut s = ListState::default();
    s.reselect(None, &lines);
    assert_eq!(s.sel, None);

    let key = |s: &mut ListState, f: &dyn Fn(&mut ListState)| {
        f(s);
        s.follow(&lines, body);
        s.top
    };
    assert_eq!(key(&mut s, &|s| s.step(&lines, true)), 1);
    assert_eq!(key(&mut s, &|s| s.step(&lines, false)), 0);
    assert_eq!(key(&mut s, &|s| s.step(&lines, false)), 0);
    assert_eq!(key(&mut s, &|s| s.page(&lines, body, true)), 5);
    assert_eq!(key(&mut s, &|s| s.ends(&lines, true)), 15);
    assert_eq!(key(&mut s, &|s| s.page(&lines, body, true)), 15);
    assert_eq!(key(&mut s, &|s| s.step(&lines, true)), 15);
    assert_eq!(key(&mut s, &|s| s.page(&lines, body, false)), 10);
    assert_eq!(key(&mut s, &|s| s.ends(&lines, false)), 0);
    assert_eq!(s.sel, None);
}

#[test]
fn debug_lines_shows_tone_bold_text_chip_and_kind() {
    let lines = vec![
        Line::new(vec![
            Span::bold("TODAY", Tone::Head),
            Span::new(" · Mon", Tone::Dim),
        ]),
        Line::plain("fix it", Tone::Text)
            .with_item(Item::Task(TaskId(3)))
            .with_chip(Span::bold(" 2d late ", Tone::Late)),
        Line::wrapped(vec![Span::new("  more", Tone::Text)]),
    ];
    assert_eq!(
        debug_lines(&lines),
        "[head*]TODAY[dim] · Mon\n[text]fix it ⟨[late*] 2d late ⟩  ‹task›\n[text]  more  ‹more›\n"
    );
}
