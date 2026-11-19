//! The panel's state machine on the sessions tree: keys, prompts, confirms, replies and the
//! hover mark, and the geometry it lays out.

use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::op::{Done, Op};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, session};
use moa_view::geometry::{Rect, Size, geometry};
use moa_view::keys::Key;
use moa_view::line::{Item, Tone};
use moa_view::panel::{EVENT_SHOWN_MS, Mark, Mouse, Out, Panel, debug_screen};

const SIZE: Size = Size { w: 80, h: 16 };

/// Fix login, Release notes, Hand started and Old spike, by their fixture number.
const FIX_LOGIN: u8 = 1;
const RELEASE_NOTES: u8 = 3;
const HAND_STARTED: u8 = 8;
const OLD_SPIKE: u8 = 9;

/// The screen row of Release notes at 80 × 16: the body starts at row 1, and it is line 7.
const RELEASE_ROW: u16 = 8;

fn id(n: u8) -> String {
    session(n).to_string()
}

fn group(path: &str) -> Item {
    Item::Group(path.to_string())
}

fn on(n: u8) -> Item {
    Item::Session(session(n))
}

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    fn new() -> T {
        let (data, ctx) = lantern();
        let mut panel = Panel::new(View::Tree);
        panel.screen(&data, &ctx, SIZE);
        T { panel, data, ctx }
    }

    /// A tree panel with `item` selected.
    fn on(item: Item) -> T {
        let mut t = T::new();
        assert!(t.panel.select(&item), "{item:?} isn't listed");
        t
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, SIZE)
    }

    fn keys(&mut self, keys: &[Key]) -> Vec<Out> {
        keys.iter().flat_map(|k| self.key(*k)).collect()
    }

    fn char(&mut self, c: char) -> Vec<Out> {
        self.key(Key::Char(c))
    }

    fn typed(&mut self, text: &str) -> Vec<Out> {
        text.chars().flat_map(|c| self.char(c)).collect()
    }

    /// Clears the prompt's prefill and answers it with `text`.
    fn answer(&mut self, text: &str) -> Vec<Out> {
        let mut out = self.key(Key::Ctrl('u'));
        out.extend(self.typed(text));
        out.extend(self.key(Key::Enter));
        out
    }

    fn mouse(&mut self, m: Mouse) -> moa_view::panel::Handled {
        self.panel.mouse(m, &self.data, &self.ctx, SIZE)
    }

    fn footer(&mut self) -> String {
        self.panel.screen(&self.data, &self.ctx, SIZE).footer.text
    }

    fn marks(&mut self) -> Vec<Mark> {
        let screen = self.panel.screen(&self.data, &self.ctx, SIZE);
        screen.body.into_iter().map(|(_, m)| m).collect()
    }

    fn hint(&mut self) -> &'static str {
        moa_view::keys::hint(View::Tree, false)
    }
}

fn send(op: Op) -> Vec<Out> {
    vec![Out::Send(op)]
}

// ---------- Enter ----------

#[test]
fn enter_on_a_session_focuses_it() {
    let mut t = T::on(on(FIX_LOGIN));
    assert_eq!(
        t.key(Key::Enter),
        send(Op::SessionFocus { q: id(FIX_LOGIN) })
    );
}

#[test]
fn enter_on_a_node_folds_it() {
    let mut t = T::on(group("Private/Lantern"));
    assert_eq!(
        t.key(Key::Enter),
        send(Op::GroupFold {
            path: "Private/Lantern".to_string(),
            collapsed: true
        })
    );
    t.data.brain.as_mut().expect("a brain").folded = vec!["Private/Lantern".to_string()];
    assert_eq!(
        t.key(Key::Enter),
        send(Op::GroupFold {
            path: "Private/Lantern".to_string(),
            collapsed: false
        })
    );
    let mut t = T::on(group("Loose"));
    assert_eq!(
        t.key(Key::Enter),
        send(Op::GroupFold {
            path: "Loose".to_string(),
            collapsed: true
        })
    );
}

// ---------- n and N ----------

#[test]
fn n_asks_folder_title_account_then_opens() {
    let mut t = T::on(group("Private/Lantern"));
    assert!(t.char('n').is_empty());
    assert_eq!(
        t.footer(),
        "new session in Private/Lantern · folder: ~/private/lantern"
    );
    assert!(t.key(Key::Enter).is_empty());
    assert_eq!(t.footer(), "title: ");
    assert!(t.typed("Probe").is_empty());
    assert!(t.key(Key::Enter).is_empty());
    assert_eq!(t.footer(), "account (acct-a / acct-b): acct-a");
    assert_eq!(
        t.key(Key::Enter),
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/lantern"),
            account: Some("acct-a".to_string()),
            title: Some("Probe".to_string()),
            group: Some("Private/Lantern".to_string()),
            prompt: None,
            bg: false,
        })
    );
    let hint = t.hint();
    assert_eq!(t.footer(), hint, "the prompt is done");
}

#[test]
fn n_stops_at_a_folder_it_cant_use() {
    let mut t = T::on(group("Private/Lantern"));
    t.ctx.is_dir = |p: &Path| p != Path::new("/h/private/nope");

    t.char('n');
    assert!(t.answer("~/private/nope").is_empty());
    assert_eq!(t.footer(), "no folder ~/private/nope");

    t.char('n');
    assert!(t.answer("work/x").is_empty());
    assert_eq!(t.footer(), "a folder starts with ~ or /");

    // The core judges which brain a folder belongs to.
    t.char('n');
    assert!(t.answer("/h/work/x").is_empty());
    assert_eq!(t.footer(), "title: ");
}

#[test]
fn n_on_a_session_prefills_its_own_folder_and_group() {
    let mut t = T::on(on(RELEASE_NOTES));
    t.char('n');
    assert_eq!(
        t.footer(),
        "new session in Private/Kite · folder: ~/private/kite"
    );
    let mut t = T::on(on(OLD_SPIKE));
    t.char('n');
    assert_eq!(t.footer(), "new session in top level · folder: ~/private");
}

#[test]
#[allow(non_snake_case)]
fn N_in_the_tree_asks_the_account_for_a_sessions_project() {
    let mut t = T::on(on(FIX_LOGIN));
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "account (acct-a / acct-b): acct-a");
    assert_eq!(
        t.answer("acct-b"),
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/lantern"),
            account: Some("acct-b".to_string()),
            title: None,
            group: None,
            prompt: None,
            bg: false,
        })
    );

    let mut t = T::on(group("Private/Lantern"));
    assert!(t.char('N').is_empty());
    let hint = t.hint();
    assert_eq!(t.footer(), hint, "no prompt on a group");

    // A session with no project says so.
    let mut t = T::on(on(OLD_SPIKE));
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "this row has no project");
}

#[test]
fn n_on_a_group_prefills_from_its_subgroups() {
    let mut t = T::on(group("Private"));
    t.char('n');
    assert_eq!(
        t.footer(),
        "new session in Private · folder: ~/private/lantern"
    );
}

#[test]
fn an_empty_account_answer_cancels() {
    let mut t = T::on(group("Private/Lantern"));
    let mut out = t.char('n');
    out.extend(t.keys(&[Key::Enter, Key::Enter]));
    out.extend(t.answer(""));
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(t.footer(), "cancelled");
}

// ---------- the other tree keys ----------

#[test]
#[allow(non_snake_case)]
fn plus_m_r_D_a_send_their_ops() {
    let mut t = T::on(group("Private/Lantern"));
    t.char('+');
    assert_eq!(t.footer(), "new group: Private/Lantern/");
    t.typed("Sub");
    assert_eq!(
        t.key(Key::Enter),
        send(Op::GroupAdd {
            path: "Private/Lantern/Sub".to_string()
        })
    );

    let mut t = T::on(on(OLD_SPIKE));
    t.char('+');
    assert_eq!(t.footer(), "new group: ", "empty at the top level");
    assert_eq!(
        t.answer("Kite/"),
        send(Op::GroupAdd {
            path: "Kite".to_string()
        })
    );

    let mut t = T::on(on(FIX_LOGIN));
    t.char('m');
    assert_eq!(t.footer(), "move Fix login to group: Private/Lantern");
    assert_eq!(
        t.answer("Work"),
        send(Op::SessionSet {
            q: id(FIX_LOGIN),
            title: None,
            group: Some("Work".to_string()),
        })
    );

    t.char('r');
    assert_eq!(t.footer(), "rename session: Fix login");
    assert_eq!(
        t.answer("Login fix"),
        send(Op::SessionSet {
            q: id(FIX_LOGIN),
            title: Some("Login fix".to_string()),
            group: None,
        })
    );

    let mut t = T::on(group("Private/Lantern"));
    t.char('r');
    assert_eq!(t.footer(), "rename group: Lantern");
    assert_eq!(
        t.answer("Beacon"),
        send(Op::GroupRename {
            old: "Private/Lantern".to_string(),
            new: "Private/Beacon".to_string(),
        })
    );
    // The same name is no rename.
    t.char('r');
    assert!(t.key(Key::Enter).is_empty());
    assert_eq!(t.footer(), "cancelled");

    let mut t = T::on(group("Private/Ideas"));
    assert_eq!(
        t.char('D'),
        send(Op::GroupRm {
            path: "Private/Ideas".to_string()
        })
    );

    let mut t = T::on(on(FIX_LOGIN));
    assert_eq!(
        t.char('a'),
        send(Op::SessionArchive {
            qs: vec![id(FIX_LOGIN)]
        })
    );
    // D on a session and a/m/x on a group do nothing.
    assert!(t.char('D').is_empty());
    let mut t = T::on(group("Private/Lantern"));
    assert!(t.char('a').is_empty());
    assert!(t.char('m').is_empty());
    assert!(t.char('x').is_empty());
    let hint = t.hint();
    assert_eq!(t.footer(), hint);
}

#[test]
fn x_closes_the_session_it_named_even_after_the_list_moved() {
    let mut t = T::on(on(FIX_LOGIN));
    assert!(t.char('x').is_empty());
    assert_eq!(t.footer(), "close Fix login? y/N");

    // A newer session lands above it: every line below moves down one.
    let mut row = serde_json::to_value(&t.data.sessions[&session(FIX_LOGIN)]).expect("a row");
    let newer = "5e5500ff-0000-4000-8000-0000000000ff";
    row["key"] = serde_json::json!(newer);
    row["id"] = serde_json::json!(newer);
    row["title"] = serde_json::json!("Newer");
    row["last_seen_at"] = serde_json::json!(t.ctx.now.0);
    t.data.upsert(Topic::Sessions, vec![row]);
    assert_eq!(t.footer(), "close Fix login? y/N");

    assert_eq!(t.char('y'), send(Op::SessionClose { q: id(FIX_LOGIN) }));

    // Any other key answers no.
    for k in [Key::Char('n'), Key::Enter, Key::Esc, Key::Char('Y')] {
        let mut t = T::on(on(FIX_LOGIN));
        t.char('x');
        assert!(t.key(k).is_empty(), "{k:?}");
        let hint = t.hint();
        assert_eq!(t.footer(), hint);
    }
}

#[test]
#[allow(non_snake_case)]
fn R_restarts_only_an_adopted_session() {
    let mut t = T::on(on(HAND_STARTED));
    assert!(t.footer().ends_with(" · R restart under moa"));
    assert!(t.char('R').is_empty());
    assert_eq!(t.footer(), "restart Hand started under moa? y/N");
    assert_eq!(
        t.char('y'),
        send(Op::SessionRestart {
            q: id(HAND_STARTED)
        })
    );

    let mut t = T::on(on(FIX_LOGIN));
    assert!(t.char('R').is_empty());
    let footer = t.footer();
    assert!(!footer.contains('R'), "{footer:?}");
}

#[test]
fn esc_in_a_prompt_says_cancelled() {
    let mut t = T::on(group("Private/Lantern"));
    t.char('n');
    assert!(t.key(Key::Esc).is_empty());
    assert_eq!(t.footer(), "cancelled");
    // Keys move again.
    t.char('j');
    assert_eq!(t.panel.selected(), Some(&on(FIX_LOGIN)));
}

#[test]
fn a_reply_shows_its_last_line_and_the_next_key_clears_it() {
    let mut t = T::new();
    t.panel.replied(Ok(Done {
        text: "opening\nopened Probe in a new tab\n\n".to_string(),
        data: serde_json::Value::Null,
    }));
    assert_eq!(t.footer(), "opened Probe in a new tab");
    t.char('j');
    let hint = t.hint();
    assert_eq!(t.footer(), hint);

    t.panel
        .replied(Err("no session matches\u{1b}[2J".to_string()));
    assert_eq!(t.footer(), "no session matches [2J");

    t.panel.replied(Ok(Done {
        text: String::new(),
        data: serde_json::Value::Null,
    }));
    assert_eq!(t.footer(), hint, "an empty text shows nothing");

    t.panel.note("moa went away; reconnecting");
    assert_eq!(t.footer(), "moa went away; reconnecting");
}

#[test]
fn an_event_shows_dim_and_goes_at_its_deadline() {
    // No data: no line of the tree has a deadline of its own.
    let mut t = T::new();
    t.data = Data::default();
    let hint = t.hint();
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    assert_eq!(screen.wake_at, None);

    let at = t.ctx.now;
    t.panel.event("archived Fix login", at);
    let ends = UtcMs(at.0 + EVENT_SHOWN_MS);
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    assert_eq!(screen.footer.text, "archived Fix login");
    assert_eq!(screen.footer.tone, Tone::Dim);
    assert_eq!(screen.wake_at, Some(ends));

    t.ctx.now = UtcMs(ends.0 - 1);
    assert_eq!(t.footer(), "archived Fix login");
    t.ctx.now = ends;
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    assert_eq!(screen.footer.text, hint);
    assert_eq!(screen.wake_at, None);

    // A newer event takes the footer, with its own deadline; a key clears it.
    t.panel.event("renamed Kite", t.ctx.now);
    assert_eq!(t.footer(), "renamed Kite");
    t.char('j');
    assert_eq!(t.footer(), hint);
}

#[test]
fn an_error_stays_until_a_key() {
    let mut t = T::new();
    t.data = Data::default();
    let hint = t.hint();
    t.panel.replied(Err("no group Kite".to_string()));
    // Another panel's event doesn't hide it.
    t.panel.event("archived Fix login", t.ctx.now);
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    assert_eq!(screen.footer.text, "no group Kite");
    assert_eq!(screen.footer.tone, Tone::Warn);
    assert_eq!(screen.wake_at, None, "an error has no deadline");
    t.ctx.now = UtcMs(t.ctx.now.0 + 3_600_000);
    assert_eq!(t.footer(), "no group Kite");
    t.char('j');
    assert_eq!(t.footer(), hint);

    // The panel's own lines (an error event, the connection's) stay too.
    t.panel.note("sessions stopped and was restarted");
    t.ctx.now = UtcMs(t.ctx.now.0 + 3_600_000);
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    assert_eq!(screen.footer.text, "sessions stopped and was restarted");
    assert_eq!(screen.footer.tone, Tone::Warn);
}

#[test]
#[allow(non_snake_case)]
fn r_and_D_on_a_node_that_isnt_a_group_say_so() {
    let mut t = T::new();
    // Release notes (project Kite) loses its group: it sits under the project's node. Old
    // spike loses its project too: it sits under `Loose`.
    reshape(&mut t, RELEASE_NOTES, |row| {
        row["group_path"] = serde_json::Value::Null;
    });
    reshape(&mut t, OLD_SPIKE, |row| {
        row["group_path"] = serde_json::Value::Null;
        row["project"] = serde_json::Value::Null;
        row["project_id"] = serde_json::Value::Null;
    });
    t.panel.screen(&t.data, &t.ctx, SIZE);
    for node in ["Kite", "Loose"] {
        assert!(t.panel.select(&group(node)), "{node} isn't listed");
        assert!(t.char('r').is_empty());
        assert_eq!(t.footer(), format!("{node} isn't a group"));
        assert!(!t.panel.typing(), "no prompt opens");
        assert!(t.char('D').is_empty());
        assert_eq!(t.footer(), format!("{node} isn't a group"));
    }
}

#[test]
fn the_footer_says_how_many_rows_it_couldnt_read() {
    let mut t = T::new();
    t.data
        .upsert(Topic::Sessions, vec![serde_json::json!({"key": "nope"})]);
    assert_eq!(t.footer(), "1 rows from moa couldn't be shown");
}

#[test]
fn a_long_prompt_keeps_its_end_and_a_cell_for_the_cursor() {
    let mut t = T::on(group("Private/Lantern"));
    t.char('n');
    t.typed(&"x".repeat(100));
    let footer = t.footer();
    assert_eq!(
        moa_view::text::width(&footer),
        76,
        "77 cells, one for the cursor"
    );
    assert!(footer.ends_with("xxx"));
}

#[test]
fn here_is_the_selected_rows_folder() {
    let t = T::on(on(FIX_LOGIN));
    assert_eq!(
        t.panel.here(&t.data),
        Some("/h/private/lantern".to_string())
    );
    let t = T::on(group("Private/Lantern"));
    assert_eq!(t.panel.here(&t.data), None);
}

#[test]
fn ctrl_c_quits_and_q_doesnt_in_the_tree() {
    let mut t = T::new();
    assert!(t.char('q').is_empty());
    assert_eq!(t.key(Key::Ctrl('c')), vec![Out::Quit]);
    t.char('n');
    assert_eq!(t.key(Key::Ctrl('c')), vec![Out::Quit], "even in a prompt");
}

#[test]
fn a_paste_lands_in_the_prompt() {
    let mut t = T::on(group("Private/Lantern"));
    t.panel.paste("ignored: no prompt");
    t.char('+');
    t.key(Key::Ctrl('u'));
    t.panel.paste("Work/\nKite");
    assert_eq!(t.footer(), "new group: Work/ Kite");
}

// ---------- hover (V11) ----------

#[test]
fn tree_hovered() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    insta::assert_snapshot!("tree_hovered", debug_screen(&screen));
}

#[test]
fn motion_along_one_row_doesnt_redraw() {
    let mut t = T::new();
    let moved = |t: &mut T, x, y| t.mouse(Mouse::Move { x, y }).redraw;
    assert!(moved(&mut t, 10, RELEASE_ROW));
    assert!(!moved(&mut t, 40, RELEASE_ROW), "the same row");
    assert!(moved(&mut t, 40, RELEASE_ROW + 1), "the next row");
    // The heading on the body's first row hovers nothing.
    assert!(moved(&mut t, 40, 1), "off a row");
    assert!(!moved(&mut t, 50, 1), "still no row");
    // Outside the body: nothing either.
    assert!(!moved(&mut t, 0, 0));
    assert!(!moved(&mut t, 40, 14), "the footer");
}

#[test]
fn a_click_acts_on_the_row_under_the_pointer() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    let handled = t.mouse(Mouse::Click {
        x: 10,
        y: RELEASE_ROW,
    });
    assert_eq!(
        handled.out,
        send(Op::SessionFocus {
            q: id(RELEASE_NOTES)
        })
    );
    assert!(handled.redraw);
    assert_eq!(t.panel.selected(), Some(&on(RELEASE_NOTES)));

    // A click on the heading or the footer does nothing.
    let handled = t.mouse(Mouse::Click { x: 10, y: 1 });
    assert!(handled.out.is_empty());
    let handled = t.mouse(Mouse::Click { x: 10, y: 14 });
    assert!(handled.out.is_empty());
    assert_eq!(t.panel.selected(), Some(&on(RELEASE_NOTES)));
}

#[test]
fn a_key_clears_the_hover_mark() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    assert!(t.marks().contains(&Mark::Hovered));
    t.char('j');
    assert!(!t.marks().contains(&Mark::Hovered));
}

#[test]
fn focus_out_clears_the_hover_mark() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    assert!(t.panel.blur());
    assert!(!t.marks().contains(&Mark::Hovered));
    assert!(!t.panel.blur());
}

#[test]
fn a_hovered_item_that_leaves_the_list_clears_the_mark() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    t.data.delete(Topic::Sessions, &[id(RELEASE_NOTES)]);
    assert!(!t.marks().contains(&Mark::Hovered));
}

#[test]
fn the_selection_wins_over_hover() {
    let mut t = T::new();
    // Private, the first item, is selected; it is on row 2.
    assert_eq!(t.panel.selected(), Some(&group("Private")));
    t.mouse(Mouse::Move { x: 10, y: 2 });
    let marks = t.marks();
    assert_eq!(marks[1], Mark::Selected);
    assert!(!marks.contains(&Mark::Hovered));
}

#[test]
fn the_wheel_hovers_what_comes_under_the_pointer() {
    let mut t = T::new();
    t.mouse(Mouse::Move {
        x: 10,
        y: RELEASE_ROW,
    });
    assert!(t.mouse(Mouse::WheelDown).redraw);
    let screen = t.panel.screen(&t.data, &t.ctx, SIZE);
    let hovered: Vec<String> = screen
        .body
        .iter()
        .filter(|(_, m)| *m == Mark::Hovered)
        .map(|(l, _)| l.text())
        .collect();
    assert_eq!(hovered.len(), 1);
    assert!(hovered[0].contains("Harbor"), "{hovered:?}");
}

// ---------- geometry ----------

#[test]
fn a_framed_view_keeps_the_spikes_padding() {
    let g = geometry(View::Tree, SIZE);
    assert!(g.framed);
    assert_eq!(g.tabs, None);
    assert_eq!(g.strip, None);
    assert_eq!(
        g.body,
        Rect {
            x: 2,
            y: 1,
            w: 77,
            h: 12
        }
    );
    assert_eq!(
        g.footer,
        Rect {
            x: 2,
            y: 14,
            w: 77,
            h: 1
        }
    );
}

#[test]
fn the_picture_has_tabs_a_strip_and_a_footer() {
    let g = geometry(View::Picture, Size { w: 80, h: 24 });
    assert!(!g.framed);
    assert_eq!(
        g.tabs,
        Some(Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 1
        })
    );
    assert_eq!(
        g.body,
        Rect {
            x: 0,
            y: 1,
            w: 80,
            h: 14
        }
    );
    assert_eq!(
        g.strip,
        Some(Rect {
            x: 0,
            y: 16,
            w: 80,
            h: 7
        })
    );
    assert_eq!(
        g.footer,
        Rect {
            x: 0,
            y: 23,
            w: 80,
            h: 1
        }
    );
}

#[test]
fn any_size_lays_out_inside_itself() {
    let inside = |r: Rect, size: Size| {
        r == Rect::default()
            || (r.w > 0
                && r.h > 0
                && u32::from(r.x) + u32::from(r.w) <= u32::from(size.w)
                && u32::from(r.y) + u32::from(r.h) <= u32::from(size.h))
    };
    for view in [View::Tree, View::Picture] {
        for w in 0..12 {
            for h in 0..14 {
                let size = Size { w, h };
                let g = geometry(view, size);
                for r in [Some(g.body), Some(g.footer), g.tabs, g.strip]
                    .into_iter()
                    .flatten()
                {
                    assert!(inside(r, size), "{view:?} {size:?}: {r:?}");
                }
            }
        }
    }
    let (data, ctx) = lantern();
    let mut panel = Panel::new(View::Tree);
    for w in 0..12 {
        for h in 0..8 {
            let size = Size { w, h };
            panel.screen(&data, &ctx, size);
            panel.key(Key::Char('j'), &data, &ctx, size);
            panel.mouse(Mouse::Click { x: 3, y: 2 }, &data, &ctx, size);
            panel.mouse(Mouse::WheelDown, &data, &ctx, size);
        }
    }
}

// ---------- fix round 1 ----------

/// Session `n`'s row, changed by `f`, upserted.
fn reshape(t: &mut T, n: u8, f: impl FnOnce(&mut serde_json::Value)) {
    let mut row = serde_json::to_value(&t.data.sessions[&session(n)]).expect("a row");
    row["key"] = serde_json::json!(session(n).to_string());
    f(&mut row);
    t.data.upsert(Topic::Sessions, vec![row]);
}

#[test]
fn n_on_a_project_node_opens_at_the_top_level() {
    let mut t = T::new();
    // Release notes (project Kite) loses its group: it sits under the project's node.
    reshape(&mut t, RELEASE_NOTES, |row| {
        row["group_path"] = serde_json::Value::Null;
    });
    t.panel.screen(&t.data, &t.ctx, SIZE);
    assert!(t.panel.select(&group("Kite")));
    t.char('n');
    assert_eq!(
        t.footer(),
        "new session in top level · folder: ~/private/kite"
    );
    let out = t.keys(&[Key::Enter, Key::Enter, Key::Enter]);
    assert_eq!(
        out,
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/kite"),
            account: Some("acct-a".to_string()),
            title: None,
            group: None,
            prompt: None,
            bg: false,
        })
    );
    // `+` there starts from the top level too.
    t.char('+');
    assert_eq!(t.footer(), "new group: ");
}

#[test]
fn n_falls_back_to_the_brains_folder_when_the_session_has_none() {
    let mut t = T::new();
    reshape(&mut t, FIX_LOGIN, |row| {
        row["cwd"] = serde_json::Value::Null
    });
    t.panel.screen(&t.data, &t.ctx, SIZE);
    assert!(t.panel.select(&on(FIX_LOGIN)));
    t.char('n');
    assert_eq!(
        t.footer(),
        "new session in Private/Lantern · folder: ~/private"
    );
    // The same for the newest session under a group.
    t.key(Key::Esc);
    assert!(t.panel.select(&group("Private/Lantern")));
    t.char('n');
    assert_eq!(
        t.footer(),
        "new session in Private/Lantern · folder: ~/private"
    );
}

// ---------- review fixes ----------

#[test]
fn an_archived_sessions_old_group_doesnt_make_a_project_node_a_group() {
    let mut t = T::new();
    // Release notes (project Kite) sits under the project's node; an archived session still
    // names a group `Kite` whose row was removed.
    reshape(&mut t, RELEASE_NOTES, |row| {
        row["group_path"] = serde_json::Value::Null;
    });
    reshape(&mut t, OLD_SPIKE, |row| {
        row["group_path"] = serde_json::json!("Kite");
        row["display_state"] = serde_json::json!("archived");
    });
    assert_eq!(
        t.data.sessions[&session(OLD_SPIKE)].display_state,
        moa_core::live::DisplayState::Archived
    );
    t.panel.screen(&t.data, &t.ctx, SIZE);
    assert!(t.panel.select(&group("Kite")));
    t.char('n');
    let footer = t.footer();
    assert!(footer.starts_with("new session in top level"), "{footer}");
}

#[test]
fn renaming_a_group_to_its_own_name_with_a_slash_is_no_rename() {
    let mut t = T::on(group("Private/Lantern"));
    t.char('r');
    assert!(t.answer("Lantern/").is_empty());
    assert_eq!(t.footer(), "cancelled");
    t.char('r');
    assert_eq!(
        t.answer("Beacon/"),
        send(Op::GroupRename {
            old: "Private/Lantern".to_string(),
            new: "Private/Beacon".to_string(),
        })
    );
}
