//! The panel loop's pure parts: what it asks of the terminal, which pane it claims, what it
//! says on every connect, how long it keeps reconnecting, and how terminal events map to a
//! panel's keys and mouse.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use moa_core::op::Op;
use moa_core::panel::View;
use moa_core::publish::{PanelRow, Topic};
use moa_core::tmux::ids::PaneId;
use moa_platform::TmuxSocket;
use moa_tui::run::{
    DownKey, INIT, RESTORE, claim_pane, cursor, down_key, down_line, first_ops, key_of, log_line,
    mouse_of, once_ops, reconnect_delays, reconnect_ops, topics,
};
use moa_view::data::{Data, TOPICS};
use moa_view::fixture::lantern;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::panel::{Mouse, Panel};

fn pane(s: &str) -> PaneId {
    s.parse().unwrap()
}

#[test]
fn the_panel_asks_for_the_mouse_focus_and_paste() {
    assert_eq!(
        INIT,
        "\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h"
    );
    for mode in ["1003", "1006", "1004", "2004"] {
        assert!(INIT.contains(&format!("\x1b[?{mode}h")), "{mode}");
        assert!(RESTORE.contains(&format!("\x1b[?{mode}l")), "{mode}");
    }
    // The cursor shows again and the screen the panel took is given back.
    assert!(RESTORE.contains("\x1b[?25h"));
    assert!(RESTORE.ends_with("\x1b[?1049l"));
}

/// A folder under `/tmp` for socket paths (on macOS `/tmp` is itself a symlink).
fn temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("moa-run")
        .tempdir_in("/tmp")
        .unwrap()
}

/// V14: `TMUX_PANE` names a pane of this brain's server only when `TMUX` is its socket.
#[test]
fn a_pane_from_another_server_is_not_claimed() {
    let dir = temp();
    let socket = dir.path().join("tmux-default");
    std::fs::write(&socket, "").unwrap();
    let tmux = format!("{},1,0", socket.display());
    let ours = TmuxSocket::Path(socket.clone());
    let claim = |t: Option<&str>, p: Option<&str>, s: &TmuxSocket, tmpdir: Option<&Path>| {
        claim_pane(t, p, s, tmpdir, 501)
    };
    assert_eq!(
        claim(Some(&tmux), Some("%3"), &ours, None),
        Some(pane("%3"))
    );

    // The same socket named through a symlinked folder.
    let other = temp();
    let link = other.path().join("link");
    symlink(dir.path(), &link).unwrap();
    let linked = TmuxSocket::Path(link.join("tmux-default"));
    assert_eq!(
        claim(Some(&tmux), Some("%3"), &linked, None),
        Some(pane("%3"))
    );

    // A named server under another TMUX_TMPDIR is another server.
    let named = TmuxSocket::Name("moa".into());
    assert_eq!(
        claim(Some(&tmux), Some("%3"), &named, Some(other.path())),
        None
    );
    // And its own socket is claimed.
    let own: PathBuf = other.path().join("tmux-501/moa");
    let own_tmux = format!("{},77,2", own.display());
    assert_eq!(
        claim(Some(&own_tmux), Some("%9"), &named, Some(other.path())),
        Some(pane("%9"))
    );

    // No TMUX, or one that isn't tmux's, or a pane that isn't one.
    assert_eq!(claim(None, Some("%3"), &ours, None), None);
    for garbage in ["", "nonsense", ",1,0", "x,y"] {
        assert_eq!(
            claim(Some(garbage), Some("%3"), &ours, None),
            None,
            "{garbage}"
        );
    }
    assert_eq!(claim(Some(&tmux), None, &ours, None), None);
    assert_eq!(claim(Some(&tmux), Some("3"), &ours, None), None);
}

/// V13: every connect says which panel this is, then where its selection is.
#[test]
fn first_ops_register_and_restore_here() {
    assert_eq!(
        first_ops(Some(pane("%3")), Some(View::Plan), Some("/x".into()), true),
        vec![
            Op::UiPanel {
                pane: Some("%3".into()),
                view: Some(View::Plan),
                placed: true,
                once: false,
            },
            Op::UiHere {
                folder: Some("/x".into()),
            },
        ]
    );
    assert_eq!(
        first_ops(None, None, None, false),
        vec![
            Op::UiPanel {
                pane: None,
                view: None,
                placed: false,
                once: false,
            },
            Op::UiHere { folder: None },
        ]
    );
}

/// L8: a reconnect asks for the view moa last gave the panel, not the one it started with.
#[test]
fn a_reconnect_sends_the_view_it_last_followed() {
    let mut panel = Panel::new(View::Tree);
    let mut data = Data::default();
    data.panels.insert(
        "c7".into(),
        PanelRow {
            pane: Some("%3".into()),
            view: View::Status,
            managed: true,
            sheet: None,
        },
    );
    assert!(panel.follow(&data, "c7"));
    assert_eq!(
        reconnect_ops(Some(pane("%3")), &panel, Some("/x".into()), true),
        first_ops(
            Some(pane("%3")),
            Some(View::Status),
            Some("/x".into()),
            true
        )
    );
}

/// V13: 250 ms, doubling to 8 s, then 8 s, for about a minute.
#[test]
fn reconnect_gives_up_after_about_a_minute() {
    let delays = reconnect_delays();
    let ms = |n: u64| Duration::from_millis(n);
    assert_eq!(
        delays[..6],
        [ms(250), ms(500), ms(1000), ms(2000), ms(4000), ms(8000)]
    );
    assert!(delays.iter().all(|d| *d <= Duration::from_secs(8)));
    let sum: Duration = delays.iter().sum();
    assert!(sum <= Duration::from_secs(60), "{sum:?}");
    assert!(sum >= Duration::from_secs(45), "{sum:?}");
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn keys_and_mouse_map() {
    let none = KeyModifiers::NONE;
    assert_eq!(
        key_of(key(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Some(Key::Ctrl('u'))
    );
    assert_eq!(
        key_of(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(Key::Ctrl('c'))
    );
    assert_eq!(
        key_of(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(Key::BackTab)
    );
    assert_eq!(
        key_of(key(KeyCode::Tab, KeyModifiers::SHIFT)),
        Some(Key::BackTab)
    );
    assert_eq!(
        key_of(key(KeyCode::Char('N'), KeyModifiers::SHIFT)),
        Some(Key::Char('N'))
    );
    assert_eq!(key_of(key(KeyCode::Enter, none)), Some(Key::Enter));
    assert_eq!(key_of(key(KeyCode::PageDown, none)), Some(Key::PageDown));
    assert_eq!(key_of(key(KeyCode::F(5), none)), None);
    // Only presses: a release is no key.
    let release = KeyEvent {
        code: KeyCode::Char('j'),
        modifiers: none,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(key_of(release), None);

    let left = MouseButton::Left;
    assert_eq!(
        mouse_of(mouse(MouseEventKind::Down(left), 5, 4)),
        Some(Mouse::Click { x: 5, y: 4 })
    );
    assert_eq!(
        mouse_of(mouse(MouseEventKind::Moved, 5, 4)),
        Some(Mouse::Move { x: 5, y: 4 })
    );
    assert_eq!(
        mouse_of(mouse(MouseEventKind::Drag(left), 6, 2)),
        Some(Mouse::Move { x: 6, y: 2 })
    );
    assert_eq!(
        mouse_of(mouse(MouseEventKind::ScrollUp, 0, 0)),
        Some(Mouse::WheelUp)
    );
    assert_eq!(
        mouse_of(mouse(MouseEventKind::ScrollDown, 0, 0)),
        Some(Mouse::WheelDown)
    );
    assert_eq!(mouse_of(mouse(MouseEventKind::Up(left), 5, 4)), None);
    assert_eq!(
        mouse_of(mouse(MouseEventKind::Down(MouseButton::Right), 5, 4)),
        None
    );
}

/// The terminal's cursor stands in the cell the editor leaves after its text, and only
/// while a prompt is answered.
#[test]
fn the_cursor_follows_the_prompt() {
    let (data, ctx) = lantern();
    let size = Size { w: 80, h: 16 };
    let mut panel = Panel::new(View::Tree);
    let screen = panel.screen(&data, &ctx, size);
    assert_eq!(cursor(&screen, panel.prompting()), None);

    panel.key(Key::Char('+'), &data, &ctx, size);
    panel.key(Key::Ctrl('u'), &data, &ctx, size);
    panel.key(Key::Char('K'), &data, &ctx, size);
    let screen = panel.screen(&data, &ctx, size);
    assert!(panel.prompting());
    let f = screen.geometry.footer;
    // `new group: K` is 12 cells.
    assert_eq!(cursor(&screen, true), Some((f.x + 12, f.y)));

    // A long answer keeps its end and the last cell for the cursor.
    for _ in 0..100 {
        panel.key(Key::Char('x'), &data, &ctx, size);
    }
    let screen = panel.screen(&data, &ctx, size);
    assert_eq!(cursor(&screen, true), Some((f.x + f.w - 1, f.y)));

    panel.key(Key::Esc, &data, &ctx, size);
    assert!(!panel.prompting());
}

/// In the picker the cursor stands after the query, with no prompt open; a long query keeps
/// its tail and a cell for the cursor before the count.
#[test]
fn the_cursor_stands_after_the_picker_query() {
    let (data, ctx) = lantern();
    let size = Size { w: 80, h: 16 };
    let mut panel = Panel::new(View::Picker);
    let screen = panel.screen(&data, &ctx, size);
    let head = screen.geometry.head.unwrap();
    // `› ` and nothing typed.
    assert_eq!(
        cursor(&screen, panel.prompting()),
        Some((head.x + 2, head.y))
    );
    panel.paste("kite");
    let screen = panel.screen(&data, &ctx, size);
    assert_eq!(cursor(&screen, false), Some((head.x + 6, head.y)));
    panel.paste(&"x".repeat(200));
    let screen = panel.screen(&data, &ctx, size);
    let (x, y) = cursor(&screen, false).unwrap();
    assert_eq!(y, head.y);
    let count = screen.head[0].chip.as_ref().unwrap();
    // A cell between the cursor and the count.
    let count_x = head.x + head.w - u16::try_from(count.text.chars().count()).unwrap();
    assert_eq!(x + 2, count_x);
}

/// While moa is down and the panel waits for a key: what types into the footer still types,
/// `q` in the picture quits, Ctrl-c is the panel's own (L14), and any other key tries again.
#[test]
fn a_key_while_down_types_quits_or_tries_again() {
    let (data, ctx) = lantern();
    let size = Size { w: 80, h: 16 };
    let mut picture = Panel::new(View::Picture);
    assert_eq!(down_key(&picture, Key::Char('q')), DownKey::Quit);
    assert_eq!(down_key(&picture, Key::Ctrl('c')), DownKey::Panel);
    assert_eq!(down_key(&picture, Key::Char('j')), DownKey::Retry);
    picture.key(Key::Char('2'), &data, &ctx, size);
    assert_eq!(down_key(&picture, Key::Char('q')), DownKey::Quit, "any tab");

    let mut tree = Panel::new(View::Tree);
    assert_eq!(
        down_key(&tree, Key::Char('q')),
        DownKey::Retry,
        "q isn't the tree's"
    );
    assert_eq!(down_key(&tree, Key::Ctrl('c')), DownKey::Panel);
    tree.key(Key::Char('+'), &data, &ctx, size);
    assert!(tree.prompting());
    for k in [Key::Char('q'), Key::Char('x'), Key::Esc, Key::Enter] {
        assert_eq!(down_key(&tree, k), DownKey::Panel, "{k:?}");
    }
    assert_eq!(down_key(&tree, Key::Ctrl('c')), DownKey::Panel);

    // The picker's closing keys quit a picker you started; the rest try again.
    let picker = Panel::new(View::Picker);
    for k in [Key::Esc, Key::Ctrl('g')] {
        assert_eq!(down_key(&picker, k), DownKey::Quit, "{k:?}");
    }
    assert_eq!(down_key(&picker, Key::Char('q')), DownKey::Retry);
    assert_eq!(down_key(&picker, Key::Ctrl('c')), DownKey::Panel);
}

/// After a first connect that failed, the footer says why and that a key tries again; once
/// reconnecting gave up, it says that. A panel moa placed can't start moa with a key, so it
/// says to run moa first.
#[test]
fn the_down_line_is_honest() {
    assert_eq!(
        down_line(Some("moa couldn't start: no such file"), false),
        "moa couldn't start: no such file; press any key to try again"
    );
    assert_eq!(
        down_line(None, false),
        "moa isn't running: press any key to try again"
    );
    assert_eq!(
        down_line(None, true),
        "moa isn't running: run moa, then press any key"
    );
}

/// L14: each `panel.log` line names the brain, the view (`-` before one is known) and the
/// pane (`-` outside moa's terminal), then what happened, on one line.
#[test]
fn a_log_line_names_the_view_and_the_pane() {
    assert_eq!(
        log_line(
            "default",
            Some(View::Tree),
            false,
            Some(pane("%3")),
            "started"
        ),
        "[default] panel tree in %3: started"
    );
    assert_eq!(
        log_line("work", None, false, None, "stopped by SIGTERM"),
        "[work] panel - in -: stopped by SIGTERM"
    );
    assert_eq!(
        log_line(
            "default",
            Some(View::Plan),
            false,
            None,
            "panicked: one\ntwo"
        ),
        "[default] panel plan in -: panicked: one two"
    );
    // A popup (S8) says so.
    assert_eq!(
        log_line("default", Some(View::Picker), true, None, "stopped: quit"),
        "[default] panel picker (popup) in -: stopped: quit"
    );
}

/// S8: a popup registers as one, with no tab and no folder to tell, and reads only what its
/// view needs; every other panel reads every topic.
#[test]
fn a_popup_registers_as_one_and_reads_its_views_topics() {
    assert_eq!(
        once_ops(View::Picker),
        vec![Op::UiPanel {
            pane: None,
            view: Some(View::Picker),
            placed: false,
            once: true,
        }]
    );
    assert_eq!(
        topics(View::Picker, true),
        [
            Topic::Sessions,
            Topic::Groups,
            Topic::Projects,
            Topic::Terms,
            Topic::Brain
        ]
    );
    assert_eq!(topics(View::Terms, true), [Topic::Terms, Topic::Brain]);
    assert_eq!(topics(View::Help, true), [Topic::Brain, Topic::Layouts]);
    assert_eq!(topics(View::Picker, false), TOPICS);
    assert_eq!(topics(View::Tree, false), TOPICS);
}
