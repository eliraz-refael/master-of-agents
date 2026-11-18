//! The keymap per view, the hints, and the footer's editor.

use moa_core::panel::View;
use moa_view::editor::{Edited, Editor};
use moa_view::keys::{Action, Key, actions, hint};

/// The picture's tabs (the spike's six).
const TABS: [View; 6] = [
    View::Plan,
    View::Today,
    View::Projects,
    View::Tree,
    View::Activity,
    View::Status,
];

/// Every view a panel lists, with the tab the picture shows.
fn every_view() -> Vec<(View, Option<View>)> {
    let mut out: Vec<(View, Option<View>)> = TABS.iter().map(|v| (*v, None)).collect();
    out.extend(TABS.iter().map(|t| (View::Picture, Some(*t))));
    out
}

fn key_of(word: &str) -> Key {
    match word {
        "Tab" => Key::Tab,
        "BackTab" => Key::BackTab,
        "Right" => Key::Right,
        "Left" => Key::Left,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Enter" => Key::Enter,
        "Space" => Key::Char(' '),
        c => {
            let mut chars = c.chars();
            let k = chars.next().expect("a key");
            assert!(chars.next().is_none(), "one character: {c}");
            Key::Char(k)
        }
    }
}

fn has_all(view: View, tab: Option<View>, keys: &str) {
    for word in keys.split_whitespace() {
        assert!(
            !actions(view, tab, key_of(word)).is_empty(),
            "{view} (tab {tab:?}): {word} has no action"
        );
    }
}

#[test]
fn every_spec_key_has_an_action() {
    has_all(View::Tree, None, "n N + m r D a x R");
    has_all(View::Plan, None, "d x n N a");
    for (view, tab) in [
        (View::Status, None),
        (View::Today, None),
        (View::Projects, None),
        (View::Picture, Some(View::Status)),
    ] {
        has_all(view, tab, "a x n N");
    }
    has_all(View::Projects, None, "n N");
    has_all(View::Today, None, "y n");
    for tab in TABS {
        has_all(
            View::Picture,
            Some(tab),
            "Tab BackTab l h Right Left 1 2 3 4 5 6 q",
        );
    }
    for (view, tab) in every_view() {
        has_all(
            view,
            tab,
            "j k Up Down g G Home End PageUp PageDown Enter Space",
        );
    }
}

#[test]
fn moving_keys_mean_the_same_everywhere() {
    for (view, tab) in every_view() {
        for (keys, action) in [
            ("j Down", Action::Down),
            ("k Up", Action::Up),
            ("g Home", Action::Top),
            ("G End", Action::Bottom),
            ("PageUp", Action::PageUp),
            ("PageDown", Action::PageDown),
            ("Enter Space", Action::Activate),
        ] {
            for word in keys.split_whitespace() {
                assert_eq!(actions(view, tab, key_of(word)), [action], "{view} {word}");
            }
        }
        assert_eq!(actions(view, tab, Key::Ctrl('c')), [Action::Quit]);
    }
}

#[test]
fn the_picture_has_its_own_tab_keys_and_q() {
    let p = |k| actions(View::Picture, Some(View::Status), k);
    assert_eq!(p(Key::Tab), [Action::NextTab]);
    assert_eq!(p(Key::Char('l')), [Action::NextTab]);
    assert_eq!(p(Key::BackTab), [Action::PrevTab]);
    assert_eq!(p(Key::Left), [Action::PrevTab]);
    assert_eq!(p(Key::Char('1')), [Action::Tab(1)]);
    assert_eq!(p(Key::Char('6')), [Action::Tab(6)]);
    assert_eq!(p(Key::Char('7')), []);
    assert_eq!(p(Key::Char('q')), [Action::Quit]);
    for view in TABS {
        assert_eq!(actions(view, None, Key::Char('q')), [], "{view}: q");
        assert_eq!(actions(view, None, Key::Tab), [], "{view}: Tab");
    }
}

#[test]
fn n_is_no_on_a_suggestion_and_new_elsewhere() {
    let n = Key::Char('n');
    assert_eq!(
        actions(View::Today, None, n),
        [Action::No, Action::NewSession]
    );
    assert_eq!(
        actions(View::Picture, Some(View::Today), n),
        [Action::No, Action::NewSession]
    );
    assert_eq!(actions(View::Today, None, Key::Char('y')), [Action::Yes]);
    assert_eq!(actions(View::Tree, None, n), [Action::NewSession]);
    assert_eq!(actions(View::Status, None, n), [Action::NewSession]);
    assert_eq!(
        actions(View::Projects, None, Key::Char('N')),
        [Action::ChooseAccount]
    );
}

#[test]
fn x_in_the_plan_only_drops() {
    let x = Key::Char('x');
    assert_eq!(actions(View::Plan, None, x), [Action::Drop]);
    assert_eq!(actions(View::Picture, Some(View::Plan), x), [Action::Drop]);
    assert!(actions(View::Picture, Some(View::Status), x).contains(&Action::Close));
    assert!(actions(View::Tree, None, x).contains(&Action::Close));
}

#[test]
fn the_picture_rows_follow_its_tab() {
    let r = Key::Char('R');
    assert_eq!(actions(View::Tree, None, r), [Action::Restart]);
    assert_eq!(
        actions(View::Picture, Some(View::Tree), r),
        [Action::Restart]
    );
    assert_eq!(actions(View::Picture, Some(View::Status), r), []);
    assert_eq!(
        actions(View::Picture, Some(View::Plan), Key::Char('d')),
        [Action::Done]
    );
    assert_eq!(actions(View::Status, None, Key::Char('d')), []);
}

#[test]
fn hints_are_the_spikes() {
    let tree =
        "⏎/click open · n new · + group · m move · r rename · D del group · a archive · x close";
    assert_eq!(hint(View::Tree, false), tree);
    assert_eq!(
        hint(View::Tree, true),
        format!("{tree} · R restart under moa")
    );
    assert_eq!(hint(View::Plan, false), "⏎/click jump · d done · x drop");
    assert_eq!(hint(View::Activity, true), "j/k scroll");
    assert_eq!(
        hint(View::Picture, false),
        "1-6/tab · j/k · ⏎ open · n/N new · a archive · x close · y/n suggestion · q"
    );
}

fn typed(e: &mut Editor, s: &str) {
    for c in s.chars() {
        assert_eq!(e.key(Key::Char(c)), Edited::Typing);
    }
}

#[test]
fn editor_keys() {
    let mut e = Editor::new("title:", "");
    typed(&mut e, "fix the  bug");
    assert_eq!(e.text, "fix the  bug");
    assert_eq!(e.key(Key::Backspace), Edited::Typing);
    assert_eq!(e.text, "fix the  bu");
    assert_eq!(e.key(Key::Ctrl('w')), Edited::Typing);
    assert_eq!(e.text, "fix the  ");
    e.key(Key::Ctrl('w'));
    assert_eq!(e.text, "fix ");
    e.key(Key::Ctrl('w'));
    assert_eq!(e.text, "");
    typed(&mut e, "word");
    e.key(Key::Ctrl('w'));
    assert_eq!(e.text, "", "a single word goes whole");
    typed(&mut e, "gone");
    assert_eq!(e.key(Key::Ctrl('u')), Edited::Typing);
    assert_eq!(e.text, "");
    assert_eq!(e.key(Key::Backspace), Edited::Typing, "nothing to erase");

    typed(&mut e, "  hello  ");
    assert_eq!(e.key(Key::Up), Edited::Typing, "other keys do nothing");
    assert_eq!(e.text, "  hello  ");
    assert_eq!(e.key(Key::Enter), Edited::Done("hello".into()));

    let mut e = Editor::new("folder:", "~/private");
    assert_eq!(e.key(Key::Esc), Edited::Cancelled);

    let mut e = Editor::new("title:", "");
    e.paste("a\nb");
    assert_eq!(e.text, "a b");

    let mut e = Editor::new("t:", "");
    e.paste("🇮🇱 launch 👍🏽\r\nnow\u{2028}x\u{202e}y");
    assert_eq!(
        e.text, "🇮🇱 launch 👍🏽 now xy",
        "a paste keeps what typing keeps"
    );
    assert_eq!(
        e.shown(40),
        "t: IL launch 👍 now xy",
        "and is cleaned only to draw"
    );

    let e = Editor::new("folder:", "~/private/lantern/src");
    assert_eq!(e.shown(40), "folder: ~/private/lantern/src");
    assert_eq!(e.shown(10), "antern/src", "the end is kept");
}

#[test]
fn the_editor_erases_and_shows_by_display_width() {
    let mut e = Editor::new("title:", "日本");
    e.key(Key::Backspace);
    assert_eq!(e.text, "日");
    let e = Editor::new("t:", "語語語");
    assert_eq!(e.shown(5), "語語", "a wide character never splits");
}
