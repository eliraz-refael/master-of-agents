//! Help on the Lantern fixture (S10): written from moa's own tables (the `Ctrl-a` key table in
//! the brain's picker form, the views' key hints, the layouts, the display states), its words
//! and its keys.

use moa_core::config::Form;
use moa_core::keymap::ctrl_a;
use moa_core::panel::View;
use moa_core::publish::{PanelRow, Topic};
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::{self, Key};
use moa_view::line::{Line, debug_lines};
use moa_view::panel::{Out, Panel, debug_screen};
use moa_view::text;
use moa_view::views::{help, status};
use serde_json::json;

const SIZE: Size = Size { w: 80, h: 16 };

/// Help's lines' text at `width`.
fn texts(data: &Data, ctx: &Ctx, width: u16) -> Vec<String> {
    help::build(data, ctx, width)
        .iter()
        .map(Line::text)
        .collect()
}

/// The line listing `key` in the `Ctrl-a` section, wide enough that no help wraps.
fn key_line(data: &Data, ctx: &Ctx, key: &str) -> String {
    let head = format!("{key} ");
    texts(data, ctx, 200)
        .into_iter()
        .find(|l| l.starts_with("  ") && l.trim_start().starts_with(&head))
        .unwrap_or_else(|| panic!("no line for {key}"))
}

/// The fixture with the `brain` row's `picker` set to `form`.
fn with_form(form: &str) -> (Data, Ctx) {
    let (mut data, ctx) = lantern();
    let mut row = serde_json::to_value(data.brain.as_ref().expect("a brain")).unwrap();
    row["key"] = json!("brain");
    row["picker"] = json!(form);
    data.upsert(Topic::Brain, vec![row]);
    (data, ctx)
}

#[test]
fn help_lists_every_table_key() {
    let (data, ctx) = lantern();
    for b in ctrl_a(Form::Popup).into_iter().flatten() {
        let line = key_line(&data, &ctx, b.key);
        assert!(line.ends_with(b.help), "{}: {line:?}", b.key);
    }
}

/// With `[ui] picker = "tab"`, `/` opens the full tab and `s` the popup: help lists the tab
/// form's table. An older daemon's row, without `picker`, is the popup's.
#[test]
fn help_follows_the_picker_form() {
    let (data, ctx) = with_form("tab");
    assert_eq!(data.brain.as_ref().unwrap().picker, Form::Tab);
    for b in ctrl_a(Form::Tab).into_iter().flatten() {
        let line = key_line(&data, &ctx, b.key);
        assert!(line.ends_with(b.help), "{}: {line:?}", b.key);
    }
    assert!(key_line(&data, &ctx, "s").contains("find a session in a popup"));
    assert!(
        key_line(&data, &ctx, "/").ends_with("find a session: type part of its name, Enter jumps")
    );

    let (data, ctx) = with_form("popup");
    assert!(key_line(&data, &ctx, "s").contains("find a session in a full tab"));
    let (mut data, ctx) = lantern();
    let mut row = serde_json::to_value(data.brain.as_ref().unwrap()).unwrap();
    row.as_object_mut().unwrap().remove("picker");
    row["key"] = json!("brain");
    data.upsert(Topic::Brain, vec![row]);
    assert_eq!(data.unreadable(), 0);
    assert!(key_line(&data, &ctx, "s").contains("find a session in a full tab"));
}

#[test]
fn help_says_hold_shift_to_select_text() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 80);
    assert!(
        lines
            .iter()
            .any(|l| l.trim() == "hold Shift to select text"),
        "{lines:#?}"
    );
}

#[test]
fn help_lists_the_layouts_with_the_current_one() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 80);
    let at = lines
        .iter()
        .position(|l| l == "Layouts")
        .expect("a Layouts heading");
    let listed: Vec<&str> = lines[at + 1..at + 7].iter().map(|l| l.trim_end()).collect();
    assert_eq!(
        listed,
        [
            "  ▸ 1 cockpit: tree · console · plan",
            "    2 focus: tree · plan · agent · console",
            "    3 classic: console · plan · tree",
            "    4 triage: status · console · plan",
            "  see one: moa layout show NAME",
            "  change one: moa layout set NAME JSON",
        ]
    );

    // Another current layout moves the mark; a broken one says why.
    let mut data = data;
    data.upsert(
        Topic::Layouts,
        vec![
            json!({"key": "cockpit", "name": "cockpit", "index": 0,
                   "leaves": ["tree", "console", "plan"], "current": false, "error": null}),
            json!({"key": "focus", "name": "focus", "index": 1, "leaves": [],
                   "current": true, "error": "no panel called nope"}),
        ],
    );
    let lines = texts(&data, &ctx, 80);
    assert!(lines.contains(&"    1 cockpit: tree · console · plan".to_string()));
    assert!(lines.contains(&"  ▸ 2 focus: no panel called nope".to_string()));
}

/// A preset's number is the `Ctrl-a` digit that builds it, wherever it is in the file's
/// order: `moa layout set cockpit …` moves cockpit to the file's end, and `Ctrl-a 1` still
/// builds it. A layout of your own has no key, so no number; it comes after the presets.
#[test]
fn layout_numbers_are_their_keys() {
    let (mut data, ctx) = lantern();
    let row = |name: &str, index: usize, leaves: &[&str]| {
        json!({"key": name, "name": name, "index": index, "leaves": leaves,
               "current": name == "cockpit", "error": null})
    };
    data.replace(
        Topic::Layouts,
        vec![
            row("focus", 0, &["tree", "plan", "agent", "console"]),
            row("classic", 1, &["console", "plan", "tree"]),
            row("triage", 2, &["status", "console", "plan"]),
            row("cockpit", 3, &["tree", "console"]),
            row("wide", 4, &["console"]),
        ],
    );
    let lines = texts(&data, &ctx, 80);
    let at = lines.iter().position(|l| l == "Layouts").unwrap();
    assert_eq!(
        lines[at + 1..at + 6],
        [
            "  ▸ 1 cockpit: tree · console",
            "    2 focus: tree · plan · agent · console",
            "    3 classic: console · plan · tree",
            "    4 triage: status · console · plan",
            "      wide: console",
        ]
    );
    let digits: Vec<&str> = ctrl_a(Form::Popup)
        .into_iter()
        .flatten()
        .filter(|b| b.help.starts_with("switch to the "))
        .map(|b| b.key)
        .collect();
    assert_eq!(digits, ["1", "2", "3", "4"]);
}

/// A word too long for its line is broken where the line ends, never split by a space, at a
/// width too narrow to hang the rest under the words.
#[test]
fn a_long_word_wraps_whole() {
    let (mut data, ctx) = lantern();
    let long = "/h/private/a-very-long-folder-name-that-breaks/layouts.json";
    data.upsert(
        Topic::Layouts,
        vec![json!({"key": "", "error": format!("can't read {long}")})],
    );
    for width in [20, 24, 30] {
        let lines = texts(&data, &ctx, width);
        let at = lines.iter().position(|l| l == "Layouts").unwrap();
        let shown: String = lines[at + 1..]
            .iter()
            .take_while(|l| !l.contains("cockpit"))
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join("");
        // The path may start beside `read`, or on the next line; no space is inside it.
        let shown = shown.replacen("can't read ", "can't read", 1);
        assert_eq!(shown, format!("can't read{long}"), "{width}");
    }
}

/// A lead too wide to leave room beside it (the picker's, at 20 columns) goes on a line of
/// its own: the words after it are all shown, none cut off at the edge.
#[test]
fn a_wide_lead_keeps_every_word() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 20);
    let at = lines
        .iter()
        .position(|l| l.contains("find a session"))
        .unwrap();
    let words: Vec<&str> = lines[at + 1..at + 4].iter().map(|l| l.trim()).collect();
    assert_eq!(
        words.join(" ").split(" · ").next(),
        Some("type part of a name")
    );
}

/// At the narrowest widths a panel draws (10 to 13 columns) help still keeps every word of
/// its plain lines, each row whole within the width, with no blank line before them.
#[test]
fn narrow_help_keeps_every_word_with_no_blank_line() {
    let (data, ctx) = lantern();
    for width in 10..=13 {
        let lines = texts(&data, &ctx, width);
        for l in &lines {
            assert!(text::width(l) <= usize::from(width), "{width}: {l:?}");
            assert!(
                l.is_empty() || !l.trim().is_empty(),
                "{width}: a blank line of spaces in {lines:#?}"
            );
        }
        let all = lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        for said in [
            "↑↓ move · PgUp/PgDn · ⏎ or a click acts · the wheel scrolls · Ctrl-c cancels a prompt",
            "hold Shift to select text",
            "see one: moa layout show NAME",
        ] {
            assert!(all.contains(said), "{width}: {said:?} in {all}");
        }
    }
}

/// `layouts.json` that doesn't parse: the file's row (key `""`) says why under the heading,
/// and isn't counted as a row that couldn't be read.
#[test]
fn a_broken_layouts_file_says_why() {
    let (mut data, ctx) = lantern();
    data.upsert(
        Topic::Layouts,
        vec![json!({"key": "", "error": "layouts.json: expected a value at line 1"})],
    );
    assert_eq!(data.unreadable(), 0);
    let lines = texts(&data, &ctx, 80);
    let at = lines.iter().position(|l| l == "Layouts").unwrap();
    assert_eq!(lines[at + 1], "  layouts.json: expected a value at line 1");
    data.delete(Topic::Layouts, &[String::new()]);
    assert_eq!(data.layouts_error, None);
    assert_eq!(data.layouts.len(), 4);
}

/// `text` says "pane" or "window" (or their plurals) as a whole word; "panel" is fine.
fn says_tmux_words(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.split(|c: char| !c.is_alphanumeric()).any(|word| {
        let one = word.strip_suffix('s').unwrap_or(word);
        one == "pane" || one == "window"
    })
}

/// Lesson 35: help speaks of tabs, panels and splits, never of tmux's panes and windows.
#[test]
fn help_has_no_tmux_words() {
    for form in ["popup", "tab"] {
        let (data, ctx) = with_form(form);
        for width in [40, 80, 200] {
            for line in texts(&data, &ctx, width) {
                assert!(!says_tmux_words(&line), "{form} at {width}: {line:?}");
            }
        }
    }
}

#[test]
fn help_names_the_prefix_plainly() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 80);
    let at = lines
        .iter()
        .position(|l| l == "tmux's prefix (Ctrl-b unless you changed it)")
        .expect("the prefix heading");
    assert_eq!(lines[at + 1], "  Space menu · m back to moa · C repair");
}

/// Each view's own keys are its key hints, after its frame's title.
#[test]
fn help_lists_each_views_hints() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 200);
    for view in [
        View::Tree,
        View::Plan,
        View::Status,
        View::Today,
        View::Projects,
        View::Activity,
        View::Picture,
    ] {
        let want = format!("  {}: {}", view.label(), keys::hint(view, false));
        assert!(lines.contains(&want), "{view:?}: {lines:#?}");
    }
}

/// Final review M4: in the picker, letters type (S7): help says so where it says j/k move,
/// and lists the picker's own keys, from its hint.
#[test]
fn help_says_letters_type_in_the_picker() {
    let (data, ctx) = lantern();
    let lines = texts(&data, &ctx, 200);
    let picker = format!(
        "  find a session: type part of a name · {}",
        keys::hint(View::Picker, false)
    );
    assert!(lines.contains(&picker), "{lines:#?}");
    // The panel-wide line that names j/k says where they type.
    let at = lines
        .iter()
        .position(|l| l == "In every panel")
        .expect("its heading");
    let every: Vec<_> = lines[at..]
        .iter()
        .take_while(|l| !l.is_empty())
        .filter(|l| l.contains("j/k"))
        .collect();
    assert_eq!(every.len(), 1, "{every:?}");
    assert!(
        every[0].ends_with("but in the picker letters type"),
        "{every:?}"
    );
}

/// The agent states, with the status view's glyph, word and look.
#[test]
fn help_shows_the_agent_states_as_the_status_view_does() {
    let (data, ctx) = lantern();
    let lines = help::build(&data, &ctx, 80);
    let at = lines
        .iter()
        .position(|l| l.text() == "Agent states")
        .unwrap();
    let states = &lines[at + 1..];
    let words: Vec<String> = states.iter().map(|l| l.text()).collect();
    // At the fixture's tick, 3: waiting's blink shows `!`, and the spinner its fourth frame.
    assert_eq!(
        words,
        [
            "  ! needs you",
            "  ? asks you",
            "  ✗ failed",
            "  ✓ done",
            "  ⠸ working",
            "  ◌ quiet",
            "  ● idle",
        ]
    );
    let looks: Vec<_> = states
        .iter()
        .map(|l| (l.spans[0].tone, l.spans[0].bold, l.animated))
        .collect();
    let want: Vec<_> = moa_core::live::DisplayState::ALL[..7]
        .iter()
        .map(|s| {
            let (tone, bold) = status::look(*s);
            (tone, bold, moa_view::views::animates(*s))
        })
        .collect();
    assert_eq!(looks, want);
}

/// Nothing wider than the width, at any width, wrapped lines included.
#[test]
fn help_fits_every_width() {
    let (data, ctx) = with_form("tab");
    for width in 1..=120u16 {
        for line in help::build(&data, &ctx, width) {
            let w = moa_view::text::width(&line.text());
            assert!(w <= usize::from(width), "{width}: {:?}", line.text());
        }
    }
    // At 80 the longest help wraps under its words, not under the key.
    let lines = texts(&data, &ctx, 80);
    let at = lines
        .iter()
        .position(|l| l.starts_with("  $ "))
        .expect("the terminal's line");
    assert!(lines[at + 1].starts_with("        ") && !lines[at + 1].trim().is_empty());
}

#[test]
fn help_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("help_model", debug_lines(&help::build(&data, &ctx, 80)));
    let mut panel = Panel::new(View::Help);
    insta::assert_snapshot!(
        "help_screen",
        debug_screen(&panel.screen(&data, &ctx, SIZE))
    );
}

// ---------- keys ----------

fn key(panel: &mut Panel, data: &Data, ctx: &Ctx, k: Key) -> Vec<Out> {
    panel.key(k, data, ctx, SIZE)
}

#[test]
fn q_esc_and_ctrl_c_close_help() {
    let (data, ctx) = lantern();
    for managed in [false, true] {
        for k in [Key::Char('q'), Key::Esc, Key::Ctrl('c')] {
            let mut panel = Panel::new(View::Help);
            panel.set_managed(managed);
            panel.screen(&data, &ctx, SIZE);
            assert_eq!(key(&mut panel, &data, &ctx, k), [Out::Close], "{k:?}");
        }
    }
    // One you started quits on them while moa is down, as on its close.
    let panel = Panel::new(View::Help);
    assert!(panel.quits(Key::Char('q')) && panel.quits(Key::Esc));
    assert!(!panel.quits(Key::Char('j')));
}

/// The first body line shown.
fn first_shown(panel: &mut Panel, data: &Data, ctx: &Ctx) -> String {
    let screen = panel.screen(data, ctx, SIZE);
    screen.body[0].0.text()
}

/// `j`/`k` and the rest scroll, as activity's do; nothing is selected and Enter does
/// nothing. The footer names the keys.
#[test]
fn help_scrolls_and_selects_nothing() {
    let (data, ctx) = lantern();
    let mut panel = Panel::new(View::Help);
    let screen = panel.screen(&data, &ctx, SIZE);
    assert_eq!(screen.title.as_deref(), Some("keys and help"));
    assert_eq!(screen.footer.text, "j/k scroll · q close");
    assert_eq!(first_shown(&mut panel, &data, &ctx), "Ctrl-a keys");
    assert_eq!(key(&mut panel, &data, &ctx, Key::Char('j')), []);
    assert_ne!(first_shown(&mut panel, &data, &ctx), "Ctrl-a keys");
    assert_eq!(key(&mut panel, &data, &ctx, Key::Enter), []);
    assert_eq!(panel.selected(), None);
    key(&mut panel, &data, &ctx, Key::Char('G'));
    let screen = panel.screen(&data, &ctx, SIZE);
    assert_eq!(screen.body.last().unwrap().0.text(), "  ● idle");
    key(&mut panel, &data, &ctx, Key::Char('g'));
    assert_eq!(first_shown(&mut panel, &data, &ctx), "Ctrl-a keys");
}

/// Help's own `panels` row, `c7`, followed: in tab `%3`, moa's, with its sheet's open.
fn row(panel: &mut Panel, data: &mut Data, sheet: Option<i64>) {
    data.panels.insert(
        "c7".into(),
        PanelRow {
            pane: Some("%3".into()),
            view: View::Help,
            managed: true,
            sheet,
        },
    );
    panel.follow(data, "c7");
}

/// Each open shows help from its top (S9); the same open said again keeps the scroll.
#[test]
fn a_new_open_scrolls_help_to_the_top() {
    let (mut data, ctx) = lantern();
    let mut panel = Panel::new(View::Help);
    row(&mut panel, &mut data, Some(1));
    panel.screen(&data, &ctx, SIZE);
    for _ in 0..5 {
        key(&mut panel, &data, &ctx, Key::Char('j'));
    }
    let scrolled = first_shown(&mut panel, &data, &ctx);
    assert_ne!(scrolled, "Ctrl-a keys");
    row(&mut panel, &mut data, Some(1));
    assert_eq!(first_shown(&mut panel, &data, &ctx), scrolled);
    row(&mut panel, &mut data, None);
    row(&mut panel, &mut data, Some(2));
    assert_eq!(first_shown(&mut panel, &data, &ctx), "Ctrl-a keys");
}
