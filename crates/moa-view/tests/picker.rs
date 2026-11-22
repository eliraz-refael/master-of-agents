//! The session picker on the Lantern fixture with two terminals (S7): its rows and their
//! order, the fuzzy match and its ranks, the head, the highlights, and its keys.

use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::{Location, SessionId};
use moa_core::op::{Done, Op};
use moa_core::panel::View;
use moa_core::publish::{PanelRow, Topic};
use moa_view::ctx::Ctx;
use moa_view::data::{Data, SessionRow};
use moa_view::fixture::{lantern, session};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Tone, debug_lines};
use moa_view::panel::{Out, Panel, debug_screen};
use moa_view::text;
use moa_view::views::picker::{self, Kind, rank, rows};
use moa_view::views::{self, ViewUi};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::json;

const SIZE: Size = Size { w: 80, h: 16 };

/// The fixture with its two terminals: `⌨ kite` in `%5` and `⌨ harbor` in `%7`.
fn fixture() -> (Data, Ctx) {
    let (mut data, ctx) = lantern();
    data.upsert(
        Topic::Terms,
        vec![
            json!({"key": "%5", "name": "kite"}),
            json!({"key": "%7", "name": "harbor"}),
        ],
    );
    (data, ctx)
}

fn term(pane: &str) -> Item {
    Item::Term {
        pane: pane.to_string(),
    }
}

fn on(n: u8) -> Item {
    Item::Session(session(n))
}

/// Pushes session `n` again, as the daemon would, after `f` changes its row.
fn push_session(data: &mut Data, n: u8, f: impl FnOnce(&mut serde_json::Value)) {
    let mut row = serde_json::to_value(&data.sessions[&session(n)]).expect("a row");
    row["key"] = json!(session(n).to_string());
    f(&mut row);
    data.upsert(Topic::Sessions, vec![row]);
}

fn done(text: &str) -> Done {
    Done {
        text: text.to_string(),
        data: Default::default(),
    }
}

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    fn new() -> T {
        let (data, ctx) = fixture();
        let mut panel = Panel::new(View::Picker);
        panel.screen(&data, &ctx, SIZE);
        T { panel, data, ctx }
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, SIZE)
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            assert_eq!(self.key(Key::Char(c)), Vec::new(), "typing {c:?}");
        }
    }

    fn screen(&mut self) -> moa_view::panel::Screen {
        self.panel.screen(&self.data, &self.ctx, SIZE)
    }

    /// The selected row, as the next frame shows it.
    fn selected(&mut self) -> Option<Item> {
        self.screen();
        self.panel.selected().cloned()
    }

    /// The query line's text, before the count.
    fn query_line(&mut self) -> String {
        self.screen().head[0].text()
    }

    fn footer(&mut self) -> String {
        self.screen().footer.text
    }
}

// ---------- the rows ----------

#[test]
fn rows_follow_urgency_then_terminals_then_recency() {
    let (data, _) = fixture();
    let rows = rows(&data);
    let items: Vec<Item> = rows.iter().map(|r| r.item.clone()).collect();
    // Waiting, question, failed, done, working, quiet, idle (Dev server's activity is the
    // newer), the terminals, then asleep and ended, the most recently seen first.
    let want = vec![
        on(1),
        on(3),
        on(6),
        on(4),
        on(2),
        on(7),
        on(5),
        on(8),
        term("%5"),
        term("%7"),
        on(9),
        on(10),
    ];
    assert_eq!(items, want);
    let words: Vec<&str> = rows.iter().map(|r| r.kind.word()).collect();
    assert_eq!(
        words,
        [
            "needs you",
            "asks you",
            "failed",
            "done",
            "working",
            "quiet",
            "idle",
            "idle",
            "terminal",
            "terminal",
            "asleep",
            "ended",
        ]
    );
    // No archived row: Gone is session 11.
    assert!(!items.contains(&on(11)));
}

/// A session at `n`, ended, seen `ago` minutes before the fixture's now.
fn ended(base: &SessionRow, n: u128, seen: UtcMs) -> SessionRow {
    let mut row = base.clone();
    row.session.id = SessionId(uuid::Uuid::from_u128(n));
    row.session.last_seen_at = Some(seen);
    row
}

#[test]
fn at_most_300_old_rows() {
    let (mut data, ctx) = fixture();
    data.sessions.clear();
    let base = lantern().0.sessions[&session(10)].clone();
    for n in 0..400 {
        let row = ended(&base, 1_000 + n, UtcMs(ctx.now.0 - 60_000 * n as i64));
        data.sessions.insert(row.session.id, row);
    }
    let rows = rows(&data);
    let old: Vec<_> = rows.iter().filter(|r| r.kind == Kind::Ended).collect();
    assert_eq!(old.len(), 300);
    // The newest 300, newest first.
    let first = Item::Session(SessionId(uuid::Uuid::from_u128(1_000)));
    let last = Item::Session(SessionId(uuid::Uuid::from_u128(1_299)));
    assert_eq!(old[0].item, first);
    assert_eq!(old[299].item, last);
    // The terminals still come first.
    assert_eq!(rows.len(), 302);
    assert_eq!(rows[0].kind, Kind::Term);
}

#[test]
fn elsewhere_and_asleep_rows_have_their_words() {
    let (mut data, _) = fixture();
    // Fix login runs on another machine: the core shows it ended, with why `elsewhere`.
    let row = data.sessions.get_mut(&session(1)).unwrap();
    row.session.location = Location::Elsewhere;
    row.display_state = DisplayState::Ended;
    row.why = "elsewhere".to_string();
    let rows = rows(&data);
    let fix = rows.iter().find(|r| r.item == on(1)).unwrap();
    assert_eq!((fix.kind, fix.kind.word()), (Kind::Elsewhere, "elsewhere"));
    assert_eq!(fix.why, "", "the word says it already");
    let spike = rows.iter().find(|r| r.item == on(9)).unwrap();
    assert_eq!(spike.kind, Kind::Asleep);
    // Fix login was seen most recently of the old rows.
    let old: Vec<&Item> = rows
        .iter()
        .filter(|r| !matches!(r.kind, Kind::Live(_) | Kind::Term))
        .map(|r| &r.item)
        .collect();
    assert_eq!(old, [&on(1), &on(9), &on(10)]);
}

// ---------- the match ----------

#[test]
fn terms_match_as_subsequences_in_any_order() {
    let hay = "kite work private/kite 5e550003";
    assert_eq!(rank("kt wk", hay), Some(2), "both spread out");
    assert_eq!(rank("wk kt", hay), Some(2), "in any order");
    assert_eq!(rank("kite", hay), Some(0));
    assert_eq!(
        rank("KITE  work", hay),
        Some(0),
        "lowercased, split on spaces"
    );
    assert_eq!(rank("kite kt", hay), Some(1));
    assert_eq!(rank("zz", hay), None);
    assert_eq!(rank("kite zz", hay), None, "every term must match");
    assert_eq!(rank("", hay), Some(0));
    assert_eq!(rank("ék", "kite"), None);
}

#[test]
fn contiguous_matches_rank_first() {
    let mut data = Data::default();
    data.upsert(
        Topic::Terms,
        vec![
            json!({"key": "%1", "name": "kayak in a tent"}),
            json!({"key": "%2", "name": "kite"}),
            json!({"key": "%3", "name": "harbor"}),
        ],
    );
    let m = picker::matched(&data, "kit");
    let shown: Vec<&str> = m.shown.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(shown, ["kite", "kayak in a tent"]);
    // An empty query keeps every row, in order.
    let all = picker::matched(&data, " ");
    let shown: Vec<&str> = all.shown.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(shown, ["kayak in a tent", "kite", "harbor"]);
}

#[test]
fn the_hay_holds_the_folder_handle_and_id() {
    let (data, _) = fixture();
    let rows = rows(&data);
    let fix = &rows[0];
    assert_eq!(
        fix.hay,
        "fix login private/lantern needs you lantern 5e550001 5e550001"
    );
    // Closed thing has no project: its folder is `private`, its group Private/Kite.
    let closed = rows.iter().find(|r| r.item == on(10)).unwrap();
    assert_eq!(closed.group, "Private/Kite");
    assert!(
        closed.hay.contains(" ended private 5e55000a"),
        "{}",
        closed.hay
    );
    let kite = rows.iter().find(|r| r.item == term("%5")).unwrap();
    assert_eq!(kite.hay, "kite terminal %5");
}

// ---------- the screen ----------

#[test]
fn picker_model() {
    let (data, ctx) = fixture();
    let mut panel = Panel::new(View::Picker);
    let size = Size { w: 80, h: 20 };
    insta::assert_snapshot!(
        "picker_model",
        debug_screen(&panel.screen(&data, &ctx, size))
    );
    panel.paste("kite");
    insta::assert_snapshot!(
        "picker_model_kite",
        debug_screen(&panel.screen(&data, &ctx, size))
    );
}

#[test]
fn the_head_counts_shown_rows_and_live_ones() {
    let mut t = T::new();
    let screen = t.screen();
    let count = screen.head[0].chip.clone().expect("a count");
    assert_eq!(
        (count.text.as_str(), count.tone),
        ("12/12 · 8 live", Tone::Dim)
    );
    t.typed("kite");
    let screen = t.screen();
    assert_eq!(screen.head[0].text(), "› kite");
    assert_eq!(screen.head[0].chip.as_ref().unwrap().text, "6/12 · 8 live");
    assert_eq!(screen.head[1].spans[0].tone, Tone::Dim);
    assert_eq!(screen.head[1].text(), "─".repeat(77));
    // The cursor stands after the query: the body's x 2, then `› kite`.
    assert_eq!(screen.cursor, Some((2 + 6, 1)));
    // The list starts under the rule.
    assert_eq!(screen.geometry.body.y, 3);
}

#[test]
fn a_long_query_shows_its_tail() {
    let mut t = T::new();
    let long = "a".repeat(70) + "xyz";
    t.panel.paste(&long);
    let screen = t.screen();
    let line = &screen.head[0];
    let count = line.chip.as_ref().unwrap();
    assert_eq!(count.text, "0/12 · 8 live");
    let text = line.text();
    assert!(text.ends_with("xyz"), "{text}");
    // `› `, the query, a cell for the cursor and one before the count.
    assert_eq!(text::width(&text), 77 - text::width(&count.text) - 2);
    let (x, _) = screen.cursor.unwrap();
    assert_eq!(usize::from(x - 2), text::width(&text));
    assert_eq!(screen.body[0].0.text(), "  no session matches");
    assert_eq!(screen.body[0].0.spans[0].tone, Tone::Dim);
}

#[test]
fn the_footer_names_the_picker_keys() {
    let mut t = T::new();
    assert_eq!(t.footer(), "↵ jump · ↑↓ move · esc close");
}

#[test]
fn rows_have_the_spikes_columns() {
    let (data, ctx) = fixture();
    let ui = ViewUi::default();
    let lines = views::build(View::Picker, &data, &ctx, &ui, 80);
    // 80 columns: the title in 20, the group in 13, the word in 10, the age in 4.
    let fix = lines[0].text();
    assert_eq!(
        fix,
        format!(
            " {}{}{}{}{}  Bash: cargo test ⚠",
            "! ",
            text::fit("Fix login", 20),
            text::fit("Private/Lant", 13),
            text::fit("needs you", 10),
            "  1m"
        )
    );
    // A terminal has no age and no why; a two-cell glyph takes the space after it.
    let kite = lines.iter().find(|l| l.item == Some(term("%5"))).unwrap();
    assert_eq!(
        kite.text().trim_end(),
        format!(" ⌨ {}terminal", text::fit("kite", 33))
    );
    assert_eq!(kite.spans[0].tone, Tone::Text);
    let spike = lines.iter().find(|l| l.item == Some(on(9))).unwrap();
    assert!(spike.text().starts_with(" 💤Old spike"), "{}", spike.text());
    assert!(spike.text().trim_end().ends_with("asleep      8h"));
    assert_eq!(spike.spans[0].tone, Tone::Dim);
    // Under 70 columns there is no group; the title is at least 10.
    let narrow = views::build(View::Picker, &data, &ctx, &ui, 40);
    assert!(narrow[0].text().starts_with(" ! Fix login needs you"));
    for line in &narrow {
        assert!(text::width(&line.text()) <= 40, "{:?}", line.text());
    }
    // Working spins and waiting blinks: those lines animate.
    assert!(lines[0].animated);
    assert!(!kite.animated);
}

#[test]
fn hits_are_highlighted_in_title_and_group_only() {
    let (data, ctx) = fixture();
    let ui = ViewUi {
        query: "kite".to_string(),
        ..ViewUi::default()
    };
    let lines = views::build(View::Picker, &data, &ctx, &ui, 80);
    // The title starts at cell 3; the title and the group take 20 + 13 cells.
    let (from, to) = (3, 3 + 20 + 13);
    let mut hits = 0;
    for line in &lines {
        let mut at = 0;
        for span in &line.spans {
            let w = text::width(&span.text);
            if span.tone == Tone::Hit {
                assert!(span.bold);
                assert!(
                    from <= at && at + w <= to,
                    "{}",
                    debug_lines(std::slice::from_ref(line))
                );
                hits += 1;
            }
            at += w;
        }
    }
    assert!(hits >= 4, "{}", debug_lines(&lines));
    // Release notes: its group's `Kite`, not its title or its folder.
    let notes = lines.iter().find(|l| l.item == Some(on(3))).unwrap();
    let hit: Vec<&str> = notes
        .spans
        .iter()
        .filter(|s| s.tone == Tone::Hit)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(hit, ["Kite"]);
    // Rust scaffold matches through `working` and `lantern`, outside the title and the
    // group: nothing there is highlighted.
    let rust = lines.iter().find(|l| l.item == Some(on(2))).unwrap();
    assert!(rust.spans.iter().all(|s| s.tone != Tone::Hit));
    // A term with no substring hit marks its first subsequence positions.
    let ui = ViewUi {
        query: "fxl".to_string(),
        ..ViewUi::default()
    };
    let lines = views::build(View::Picker, &data, &ctx, &ui, 80);
    let hit: Vec<&str> = lines[0]
        .spans
        .iter()
        .filter(|s| s.tone == Tone::Hit)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(hit, ["F", "x", "l"]);
}

// ---------- the keys ----------

#[test]
fn typing_j_k_and_q_goes_into_the_query() {
    let mut t = T::new();
    t.typed("jkq gG");
    assert_eq!(t.query_line(), "› jkq gG");
    assert_eq!(t.panel.view(), View::Picker);
    // Pasted text joins it; its line breaks become spaces.
    t.panel.paste("a\nb");
    assert_eq!(t.query_line(), "› jkq gGa b");
}

#[test]
fn backspace_ctrl_u_and_ctrl_w_edit_the_query() {
    let mut t = T::new();
    t.typed("kite 設計");
    t.key(Key::Backspace);
    assert_eq!(t.query_line(), "› kite 設");
    t.key(Key::Ctrl('u'));
    assert_eq!(t.query_line(), "› ");
}

#[test]
fn ctrl_w_deletes_the_last_word() {
    let mut t = T::new();
    t.typed("kite work");
    t.key(Key::Ctrl('w'));
    assert_eq!(t.query_line(), "› kite ");
    t.typed("notes  ");
    t.key(Key::Ctrl('w'));
    assert_eq!(t.query_line(), "› kite ");
}

#[test]
fn an_edit_selects_the_first_row() {
    let mut t = T::new();
    assert_eq!(t.selected(), Some(on(1)));
    t.key(Key::Down);
    t.key(Key::Down);
    assert_eq!(t.selected(), Some(on(6)));
    // A space changes no match, and still puts the selection back.
    t.typed(" ");
    assert_eq!(t.selected(), Some(on(1)));
    t.key(Key::Down);
    t.key(Key::Backspace);
    assert_eq!(t.selected(), Some(on(1)));
    t.key(Key::Down);
    t.panel.paste("x");
    t.key(Key::Ctrl('u'));
    assert_eq!(t.selected(), Some(on(1)));
}

#[test]
fn tab_and_ctrl_n_move_down() {
    let mut t = T::new();
    t.key(Key::Tab);
    assert_eq!(t.selected(), Some(on(3)));
    t.key(Key::Ctrl('n'));
    assert_eq!(t.selected(), Some(on(6)));
    t.key(Key::Down);
    assert_eq!(t.selected(), Some(on(4)));
    t.key(Key::BackTab);
    t.key(Key::Ctrl('p'));
    t.key(Key::Up);
    assert_eq!(t.selected(), Some(on(1)));
    // The selection stops at the ends.
    t.key(Key::Up);
    assert_eq!(t.selected(), Some(on(1)));
    // PgDn goes by the list's height (16 rows: 10 under the head), and stops at the last.
    t.key(Key::PageDown);
    assert_eq!(t.selected(), Some(on(9)));
    t.key(Key::PageDown);
    assert_eq!(t.selected(), Some(on(10)));
    t.key(Key::PageDown);
    assert_eq!(t.selected(), Some(on(10)));
    t.key(Key::Down);
    assert_eq!(t.selected(), Some(on(10)));
    // The list scrolls under the head to keep it in view.
    let screen = t.screen();
    let last = screen.body.last().unwrap();
    assert_eq!(last.0.item, Some(on(10)));
    assert_eq!(screen.head[0].text(), "› ");
    t.key(Key::PageUp);
    assert_eq!(t.selected(), Some(on(3)));
    t.key(Key::PageUp);
    assert_eq!(t.selected(), Some(on(1)));
}

#[test]
fn enter_on_a_session_focuses_it() {
    let mut t = T::new();
    let out = t.key(Key::Enter);
    assert_eq!(
        out,
        [Out::Send(Op::SessionFocus {
            q: session(1).to_string()
        })]
    );
    assert_eq!(t.footer(), "opening Fix login…");
    assert_eq!(
        t.panel.replied(Ok(done("focused Fix login"))),
        Some(Out::Close)
    );
    // Only the pick's reply closes it.
    assert_eq!(t.panel.replied(Ok(done(""))), None);
}

#[test]
fn enter_on_a_terminal_shows_it() {
    let mut t = T::new();
    assert!(t.panel.select(&term("%5")));
    let out = t.key(Key::Enter);
    assert_eq!(
        out,
        [Out::Send(Op::TermShow {
            pane: "%5".to_string()
        })]
    );
    assert_eq!(t.footer(), "opening kite…");
    assert_eq!(t.panel.replied(Ok(done("went to kite"))), Some(Out::Close));
}

#[test]
fn a_click_picks() {
    let mut t = T::new();
    // Release notes is the list's second row: screen row 4.
    let handled = t.panel.mouse(
        moa_view::panel::Mouse::Click { x: 10, y: 4 },
        &t.data,
        &t.ctx,
        SIZE,
    );
    assert_eq!(
        handled.out,
        [Out::Send(Op::SessionFocus {
            q: session(3).to_string()
        })]
    );
    // A click on the head picks nothing.
    let handled = t.panel.mouse(
        moa_view::panel::Mouse::Click { x: 10, y: 1 },
        &t.data,
        &t.ctx,
        SIZE,
    );
    assert_eq!(handled.out, []);
}

#[test]
fn a_pick_that_fails_keeps_the_picker_open() {
    let mut t = T::new();
    t.typed("fix");
    assert!(matches!(t.key(Key::Enter)[..], [Out::Send(_)]));
    let reply = t.panel.replied(Err("its folder ~/x is gone".to_string()));
    assert_eq!(reply, None);
    assert_eq!(t.footer(), "its folder ~/x is gone");
    assert_eq!(t.query_line(), "› fix");
    assert_eq!(t.panel.view(), View::Picker);
    // A pick after it still closes on its reply.
    t.key(Key::Enter);
    assert_eq!(t.panel.replied(Ok(done(""))), Some(Out::Close));
}

#[test]
fn enter_with_no_rows_closes() {
    let mut t = T::new();
    t.typed("zzzz");
    assert_eq!(t.key(Key::Enter), [Out::Close]);
}

#[test]
fn esc_ctrl_c_and_ctrl_g_close() {
    for managed in [false, true] {
        for k in [Key::Esc, Key::Ctrl('c'), Key::Ctrl('g')] {
            let mut t = T::new();
            t.panel.set_managed(managed);
            t.typed("kite");
            assert_eq!(t.key(k), [Out::Close], "{k:?}, managed {managed}");
            assert_eq!(t.footer(), "↵ jump · ↑↓ move · esc close", "{k:?}");
            assert_eq!(t.panel.quits(k), !managed, "{k:?}");
        }
    }
    let mut t = T::new();
    assert_eq!(t.panel.interrupt(), Some(Out::Close));
    assert!(!t.panel.quits(Key::Char('q')));
}

/// Review M1: while a pick waits, another Enter sends nothing and says what is opening, so
/// two picks are never in flight. A refused pick lets the next one go.
#[test]
fn a_second_pick_waits_for_the_first() {
    let mut t = T::new();
    assert_eq!(t.key(Key::Enter).len(), 1);
    t.key(Key::Down);
    assert_eq!(t.key(Key::Enter), []);
    assert_eq!(t.footer(), "opening Fix login…");
    assert_eq!(t.panel.replied(Err("its folder ~/x is gone".into())), None);
    // Release notes is still selected: it is picked now, and its `Ok` closes.
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::SessionFocus {
            q: session(3).to_string()
        })]
    );
    assert_eq!(t.footer(), "opening Release notes…");
    assert_eq!(
        t.panel.replied(Ok(done("went to Release notes"))),
        Some(Out::Close)
    );
    // A connection that ended takes the wait with it.
    t.key(Key::Enter);
    assert_eq!(t.key(Key::Enter), []);
    t.panel.forget_pick();
    assert_eq!(t.key(Key::Enter).len(), 1);
}

/// Review M2: the query keeps its first 200 characters, whatever is typed or pasted.
#[test]
fn a_huge_paste_is_capped() {
    let mut t = T::new();
    t.panel.paste(&"k".repeat(10_000));
    let started = std::time::Instant::now();
    let screen = t.screen();
    let shown = screen.head[0].text();
    assert!(shown.ends_with('k'), "{shown}");
    t.key(Key::Char('x'));
    t.panel.paste("yz");
    t.key(Key::Backspace);
    // 200 `k`s less one: what was typed past the cap went, then Backspace took a `k`.
    t.key(Key::Char('i'));
    let screen = t.screen();
    let query = screen.head[0].spans[1].text.clone();
    assert!(query.ends_with("kki"), "{query}");
    assert_eq!(screen.head[0].chip.as_ref().unwrap().text, "0/12 · 8 live");
    t.key(Key::Ctrl('u'));
    t.panel.paste(&"x".repeat(picker::MAX_QUERY + 1));
    t.typed("kite");
    assert!(!t.query_line().contains("kite"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// Review M3: terminals list by their tab number, as tmux lists them: `%9` before `%10`.
#[test]
fn terminals_list_by_tab_number() {
    let mut data = Data::default();
    data.upsert(
        Topic::Terms,
        vec![
            json!({"key": "%10", "name": "ten"}),
            json!({"key": "%9", "name": "nine"}),
            json!({"key": "%100", "name": "hundred"}),
        ],
    );
    let titles: Vec<String> = rows(&data).into_iter().map(|r| r.title).collect();
    assert_eq!(titles, ["nine", "ten", "hundred"]);
}

/// V8: a push that reorders the rows keeps the selection on its item, and Enter picks that
/// item; a selected row that goes leaves the selection on the row now nearest.
#[test]
fn the_selection_follows_its_item_across_a_push() {
    let mut t = T::new();
    t.key(Key::Down);
    t.key(Key::Down);
    assert_eq!(t.selected(), Some(on(6)));
    // Fix login ends: it moves to the old rows, and Migrate DB moves up a row.
    push_session(&mut t.data, 1, |row| {
        row["lifecycle"] = json!("ended");
        row["display_state"] = json!("ended");
    });
    let screen = t.screen();
    assert_eq!(screen.body[1].0.item, Some(on(6)));
    assert_eq!(screen.body[1].1, moa_view::panel::Mark::Selected);
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::SessionFocus {
            q: session(6).to_string()
        })]
    );
    t.panel.replied(Err("no".into()));
    // Migrate DB goes: the selection falls to the row now in its place, Done's 設計レビュー.
    t.data.delete(Topic::Sessions, &[session(6).to_string()]);
    assert_eq!(t.selected(), Some(on(4)));
    assert_eq!(
        t.key(Key::Enter),
        [Out::Send(Op::SessionFocus {
            q: session(4).to_string()
        })]
    );
    // Rows arriving while the pick waits don't stop its `Ok` from closing the picker.
    push_session(&mut t.data, 2, |row| {
        row["activity"] = json!("needs_permission");
        row["display_state"] = json!("waiting");
    });
    t.screen();
    assert_eq!(t.panel.replied(Ok(done(""))), Some(Out::Close));
}

#[test]
fn reset_pick_clears_the_query_and_selects_the_first_row() {
    let mut t = T::new();
    t.typed("kite");
    t.key(Key::Down);
    t.key(Key::Enter);
    t.panel.reset_pick();
    assert_eq!(t.query_line(), "› ");
    assert_eq!(t.selected(), Some(on(1)));
    assert_eq!(t.footer(), "↵ jump · ↑↓ move · esc close");
    // The pick before it no longer closes the picker.
    assert_eq!(t.panel.replied(Ok(done(""))), None);
}

// ---------- never panics (V9) ----------

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z /-]{0,8}",
        1 => select(vec![
            "設計レビュー".to_string(),
            "👨‍👩‍👧".to_string(),
            "👍🏽".to_string(),
            "🇮🇱".to_string(),
            "\u{202e}abc".to_string(),
            "שלום עולם".to_string(),
            "e\u{301}".to_string(),
            "\u{301}".to_string(),
            "İΣ".to_string(),
            "💤".to_string(),
            "\n\u{1b}[2J".to_string(),
            String::new(),
        ]),
    ]
}

fn words() -> impl Strategy<Value = String> {
    vec(piece(), 0..4).prop_map(|p| p.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn picker_never_panics_or_overflows(
        rows in vec((words(), words(), 0usize..10, any::<bool>()), 0..1000),
        terms in vec(words(), 0..4),
        query in words(),
        width in 10u16..=200,
        h in 0u16..40,
    ) {
        let (mut data, ctx) = lantern();
        data.sessions.clear();
        let base = lantern().0.sessions[&session(1)].clone();
        for (n, (title, why, state, here)) in rows.into_iter().enumerate() {
            let mut row = base.clone();
            row.session.id = SessionId(uuid::Uuid::from_u128(n as u128 + 1));
            row.session.title = Some(title.clone());
            row.session.group_path = Some(why.clone());
            row.session.location = if here { Location::Here } else { Location::Elsewhere };
            row.display_state = DisplayState::ALL[state];
            row.why = why;
            data.sessions.insert(row.session.id, row);
        }
        for (n, name) in terms.into_iter().enumerate() {
            data.upsert(Topic::Terms, vec![json!({"key": format!("%{n}"), "name": name})]);
        }
        let ui = ViewUi { query: query.clone(), ..ViewUi::default() };
        let max = usize::from(width);
        for line in views::build(View::Picker, &data, &ctx, &ui, width) {
            prop_assert!(text::width(&line.text()) <= max, "{:?}", line.text());
        }
        let m = picker::matched(&data, &query);
        for line in picker::head(&m, &query, width).lines {
            let w = text::width(&line.text())
                + line.chip.as_ref().map_or(0, |c| text::width(&c.text) + 1);
            prop_assert!(w <= max, "{:?}", line);
        }
        let mut panel = Panel::new(View::Picker);
        panel.paste(&query);
        let size = Size { w: width, h };
        for k in [Key::Down, Key::PageDown, Key::Char('a'), Key::Ctrl('w'), Key::Up] {
            panel.key(k, &data, &ctx, size);
            let screen = panel.screen(&data, &ctx, size);
            let body = usize::from(screen.geometry.body.w.max(10));
            for (line, _) in &screen.body {
                prop_assert!(text::width(&line.text()) <= body);
            }
            if let Some((x, y)) = screen.cursor {
                prop_assert!(screen.geometry.head.unwrap().contains(x, y));
            }
        }
    }
}

#[test]
fn lines_are_selectable_rows() {
    let (data, ctx) = fixture();
    let lines: Vec<Line> = views::build(View::Picker, &data, &ctx, &ViewUi::default(), 80);
    assert_eq!(lines.len(), 12);
    assert!(lines.iter().all(Line::selectable));
}

// ---------- the sheet (S9) ----------

/// The picker's own `panels` row, `c7`, followed: in tab `%3`, `managed` or not, with its
/// sheet's open `sheet`.
fn row(t: &mut T, managed: bool, sheet: Option<i64>) {
    t.data.panels.insert(
        "c7".into(),
        PanelRow {
            pane: Some("%3".into()),
            view: View::Picker,
            managed,
            sheet,
        },
    );
    t.panel.follow(&t.data, "c7");
}

/// A sheet moa placed waits, parked, while it is closed: it draws nothing, and nothing in
/// it moves or wakes the panel, though a working session's glyph would.
#[test]
fn a_closed_sheet_draws_nothing_and_never_animates() {
    let mut t = T::new();
    row(&mut t, true, Some(1));
    let open = t.screen();
    assert!(open.animate, "the working session's glyph moves");
    assert!(!open.body.is_empty());

    row(&mut t, true, None);
    let closed = t.screen();
    assert!(closed.body.is_empty() && closed.head.is_empty() && closed.strip.is_empty());
    assert_eq!((closed.title.as_deref(), closed.cursor), (None, None));
    assert_eq!(closed.footer.text, "");
    assert!(!closed.geometry.framed);
    assert!(!closed.animate);
    assert_eq!(closed.wake_at, None);
}

/// A picker you started (`moa panel picker`), or a popup, has no open to wait for: it always
/// draws, and its close quits it.
#[test]
fn a_picker_you_started_always_draws() {
    let mut t = T::new();
    row(&mut t, false, None);
    let screen = t.screen();
    assert!(!screen.body.is_empty() && screen.animate);
    assert_eq!(t.key(Key::Esc), [Out::Close]);
    assert!(t.panel.quits(Key::Esc));

    let mut popup = T::new();
    popup.panel.set_popup(true);
    row(&mut popup, true, None);
    assert!(!popup.screen().body.is_empty());
}

/// Each open starts the picker afresh: the query empty and the first row selected. The same
/// open said again keeps what was typed.
#[test]
fn a_new_sheet_number_clears_the_query() {
    let mut t = T::new();
    row(&mut t, true, Some(1));
    let first = t.selected();
    t.typed("harb");
    row(&mut t, true, Some(1));
    assert_eq!(t.query_line(), "› harb");

    // Closed, then open again.
    row(&mut t, true, None);
    row(&mut t, true, Some(2));
    assert_eq!(t.query_line(), "› ");
    t.key(Key::Down);
    assert_ne!(t.selected(), first);
    row(&mut t, true, Some(3));
    assert_eq!(t.selected(), first);
    assert_eq!(t.footer(), "↵ jump · ↑↓ move · esc close");
}

/// In a sheet, Ctrl-c closes, as Esc does: it never says the managed line. Help too.
#[test]
fn ctrl_c_closes_a_sheet() {
    let mut t = T::new();
    row(&mut t, true, Some(1));
    assert_eq!(t.key(Key::Ctrl('c')), [Out::Close]);
    let mut help = Panel::new(View::Help);
    help.set_managed(true);
    assert_eq!(help.interrupt(), Some(Out::Close));
}

#[test]
fn a_terminal_row_shows_its_folder_and_program() {
    let (mut data, ctx) = fixture();
    data.upsert(
        Topic::Terms,
        vec![json!({"key": "%5", "name": "kite", "folder": "/h/private/kite", "command": "node"})],
    );
    let ui = ViewUi::default();
    let lines = views::build(View::Picker, &data, &ctx, &ui, 80);
    let row = lines
        .iter()
        .find(|l| l.item == Some(term("%5")))
        .expect("kite's row");
    assert!(
        row.text().trim_end().ends_with("  ~/private/kite · node"),
        "{}",
        row.text()
    );
    // The row without a folder or program keeps the name-only look.
    let harbor = lines.iter().find(|l| l.item == Some(term("%7"))).unwrap();
    assert!(!harbor.text().contains('·'), "{}", harbor.text());
}
