//! Slice 5a end to end: real `moa panel`s in tabs of a TempBrain's own terminal server, `moa`
//! on a `Terminal` attached to it, and fake-claude sessions `moactl open --bg` started. The
//! panels are driven as a user drives them: keys sent to their tab, clicks, the pointer and
//! the wheel typed on the terminal as mouse reports, `Ctrl-a $`, and moa stopped and started
//! again under them. Every wait is for what tmux shows or moa says, never for a fixed time.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_platform::{canonical, kill_term, pid_alive};
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for};
use serde_json::Map;

use common::*;

/// The hover mark's background, `surface` (`#3b4252`), as `capture-pane -e` writes it.
const HOVER_BG: &str = "48;2;59;66;82";
/// What the tree's footer says while nothing else is said: its key hints begin so.
const TREE_HINT: &str = "⏎/click open";

/// A root with the fake account, and `moa` on a terminal, linked to its daemon.
struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
}

fn rig() -> Rig {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    Rig { term, tb }
}

impl Rig {
    /// `moactl args…`, which must succeed; its stdout.
    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        moactl_ok(&self.tb, &self.term, args)
    }

    /// A session `moactl open --bg` started in `<root>/work/<name>`, titled `title`.
    #[track_caller]
    fn session(&self, name: &str, title: &str, group: Option<&str>) -> Opened {
        open_session(&self.tb, &self.term, name, title, group)
    }

    /// A tree panel's tab, once it shows its frame and `texts`; its pane.
    #[track_caller]
    fn tree(&self, texts: &[&str]) -> String {
        let pane = panel_tab(&self.tb, "tree");
        let mut all = vec!["▤ sessions"];
        all.extend(texts);
        self.shows(&pane, &all);
        pane
    }

    /// Waits until `pane` shows every one of `texts`.
    #[track_caller]
    fn shows(&self, pane: &str, texts: &[&str]) {
        shows(&self.tb, pane, texts);
    }

    /// The text of `pane`'s row drawn on background `bg`, if one is.
    fn row_on(&self, pane: &str, bg: &str) -> Option<String> {
        colours(&self.tb, pane)
            .into_iter()
            .find(|l| l.contains(bg))
            .map(|l| plain(&l))
    }

    /// Waits until `pane`'s row showing `text` is drawn on `bg`, or with `None` until no row
    /// is.
    #[track_caller]
    fn row_becomes(&self, pane: &str, bg: &str, text: Option<&str>) {
        let seen = wait_for(SHOWS, || {
            let row = self.row_on(pane, bg);
            let ok = match text {
                Some(text) => row.is_some_and(|r| r.contains(text)),
                None => row.is_none(),
            };
            ok.then_some(())
        });
        assert!(
            seen.is_some(),
            "{pane}: the row on {bg} never became {text:?}: {:?}\n{}",
            self.row_on(pane, bg),
            tab_shows(&self.tb, pane)
        );
    }

    /// The pane of the tab running session `id`.
    #[track_caller]
    fn session_pane(&self, id: &str) -> String {
        session_pane(&self.tb, &self.term, id)
    }

    /// Waits until the attached client shows the tab of `pane`.
    #[track_caller]
    fn client_shows(&self, pane: &str) {
        let want = tmux(
            &self.tb,
            &["display-message", "-p", "-t", pane, "#{window_id}"],
        );
        let seen = wait_for(SHOWS, || {
            (client_field(&self.tb, &self.term, "#{window_id}").as_deref() == Some(want.as_str()))
                .then_some(())
        });
        assert!(
            seen.is_some(),
            "the terminal shows {:?}, not {pane}'s tab {want}\n{}",
            client_field(&self.tb, &self.term, "#{window_id} #{window_name}"),
            what_happened(&self.tb, &self.term)
        );
    }

    /// The `moa` tabs named `name`, with the folder each one's shell is in.
    fn tabs_named(&self, name: &str) -> Vec<PathBuf> {
        let list = tmux(
            &self.tb,
            &[
                "list-windows",
                "-t",
                "=moa",
                "-F",
                "#{window_name}\t#{pane_current_path}",
            ],
        );
        list.lines()
            .filter_map(|l| l.split_once('\t'))
            .filter(|(n, _)| *n == name)
            .map(|(_, path)| canonical(Path::new(path)))
            .collect()
    }

    /// `Ctrl-a $` on the terminal with `pane` the active pane of the tab it shows: a new
    /// terminal tab named `name` comes up, in `dir`. A tab has its name from the moment tmux
    /// makes it, but its shell moves into the folder only once it runs, so the wait is for a
    /// new tab of that name in `dir`.
    #[track_caller]
    fn terminal_key_opens(&self, pane: &str, name: &str, dir: &Path) {
        tmux(&self.tb, &["select-window", "-t", pane]);
        tmux(&self.tb, &["select-pane", "-t", pane]);
        self.client_shows(pane);
        let in_dir = |tabs: &[PathBuf]| tabs.iter().filter(|p| *p == dir).count();
        let before = in_dir(&self.tabs_named(name));
        ctrl_a(&self.tb, &self.term);
        self.term.press(b"$");
        let opened = wait_for(SHOWS, || {
            (in_dir(&self.tabs_named(name)) > before).then_some(())
        });
        assert!(
            opened.is_some(),
            "Ctrl-a $ opened no {name:?} tab in {}: the {name:?} tabs are in {:?}\n{}",
            dir.display(),
            self.tabs_named(name),
            what_happened(&self.tb, &self.term)
        );
    }

    /// The daemon's launches so far.
    fn launches(&self) -> u64 {
        let s = status_json(&self.tb);
        s["launches"].as_u64().unwrap_or_else(|| panic!("{s}"))
    }

    /// Types an SGR mouse report for button code `code` at `at` on the terminal.
    fn mouse(&self, code: u32, at: (u32, u32)) {
        let (x, y) = at;
        self.term.press(format!("\x1b[<{code};{x};{y}M").as_bytes());
    }
}

#[test]
fn a_tree_panel_shows_the_brain_and_follows_it() {
    let r = rig();
    let kite = r.session("a", "Kite", None);
    r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);

    r.ok(&["set", &kite.id, "--title", "Beacon"]);
    r.shows(&pane, &["Beacon"]);
    r.ok(&["group", "add", "Private/Ideas"]);
    r.shows(&pane, &["Ideas"]);
}

#[test]
fn keys_and_a_click_act() {
    let r = rig();
    let kite = r.session("a", "Kite", None);
    let harbor = r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);

    select_row(&r.tb, &pane, "Harbor");
    tmux(&r.tb, &["send-keys", "-t", &pane, "Enter"]);
    r.client_shows(&r.session_pane(&harbor.id));

    click(&r.tb, &r.term, &pane, "Kite");
    r.client_shows(&r.session_pane(&kite.id));
}

#[test]
fn the_terminal_key_opens_where_the_panel_points() {
    let r = rig();
    r.session("a", "Kite", None);
    let harbor = r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);

    select_row(&r.tb, &pane, "Harbor");
    r.terminal_key_opens(&pane, "⌨ b", &harbor.dir);
}

/// What `select_row` takes for the item under the bar, on rows as the tree draws them: a
/// spinner frame, an age or a group's counts changing leave the item the same, another
/// session is another item, and a name matches only whole.
#[test]
fn a_row_is_known_by_its_item_not_its_spinner() {
    let lantern = "│ ├─ ⠋ Lantern          now│";
    for same in [
        "│ ├─ ⠙ Lantern          now│",
        "│ ├─ ● Lantern           1m│",
        "│ ├─ ! Lantern          12h│",
    ] {
        assert_eq!(item_of(lantern), item_of(same), "{same}");
    }
    assert_eq!(
        item_of("│ ▾ Loose  2 · 2 live ⠋   │"),
        item_of("│ ▾ Loose  3 · 1 live ⠸   │")
    );
    assert_ne!(item_of(lantern), item_of("│ └─ ⠋ Kite             now│"));
    assert_ne!(item_of(lantern), item_of("│ ▾ Loose  2 · 2 live ⠋   │"));

    assert!(names(lantern, "Lantern"));
    assert!(names("│ ▾ Private  1           │", "Private"));
    assert!(!names("│ └─ ● Kitesurf         now│", "Kite"));
    assert!(!names("│ ▾ Private  1           │", "Priv"));
    assert!(!names("╭─ ▤ sessions ───────────╮", "Kite"));
}

/// `select_row` knows a session by its name: with every session working, each one's glyph
/// spins and so does its group's label, and the bar still lands on the session asked for,
/// from above and from below, and stays there.
#[test]
fn select_row_lands_among_spinning_rows() {
    let r = rig();
    let prompt = FakeStep::Hook {
        event: "UserPromptSubmit".into(),
        fields: Map::new(),
    };
    for (name, title) in [("a", "Kite"), ("b", "Harbor"), ("c", "Beacon")] {
        let steps = [start_hook(), prompt.clone(), FakeStep::Hang];
        open_session_with(&r.tb, &r.term, name, title, None, &steps);
    }
    let pane = r.tree(&["Kite", "Harbor", "Beacon", "3 working"]);
    for target in ["Harbor", "Kite", "Harbor", "Beacon", "Harbor", "Kite"] {
        select_row(&r.tb, &pane, target);
        // No key it sent is still on its way: a moment on, the bar is where it was left.
        let stays = wait_for(Duration::from_millis(300), || {
            let rows = colours(&r.tb, &pane);
            let bar = rows.iter().find(|l| l.contains(SEL_BG)).map(|l| plain(l));
            bar.filter(|b| !names(b, target))
        });
        assert!(
            stays.is_none(),
            "the bar left {target:?}: {stays:?}\n{}",
            tab_shows(&r.tb, &pane)
        );
    }
}

#[test]
fn a_fold_is_kept_across_panels() {
    let r = rig();
    r.session("a", "Kite", Some("Private"));
    let first = r.tree(&["▾ Private", "Kite"]);

    select_row(&r.tb, &first, "Private");
    tmux(&r.tb, &["send-keys", "-t", &first, "Enter"]);
    r.shows(&first, &["▸ Private"]);

    let second = r.tree(&["▸ Private"]);
    assert!(
        !screen_of(&r.tb, &second).contains("Kite"),
        "{}",
        screen_of(&r.tb, &second)
    );
}

/// V11 on the real tmux: tmux turns motion reporting on for the terminal, hands the pointer's
/// motion to a panel that isn't the active pane, which marks the row under it; a click there
/// acts; and focus leaving the panel clears the mark.
#[test]
fn hover_marks_the_row_under_the_pointer_and_focus_out_clears_it() {
    let r = rig();
    let kite = r.session("a", "Kite", None);
    let harbor = r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);
    // The panel's tab, split with the new half on the left, so the panel stands away from
    // the terminal's first column. The panel is active first, then the other half is.
    let other = tmux(
        &r.tb,
        &[
            "split-window",
            "-h",
            "-b",
            "-d",
            "-t",
            &pane,
            "-P",
            "-F",
            "#{pane_id}",
        ],
    );
    tmux(&r.tb, &["select-pane", "-t", &pane]);
    tmux(&r.tb, &["select-pane", "-t", &other]);
    tmux(&r.tb, &["select-window", "-t", &pane]);
    r.client_shows(&pane);
    let field = |p: &str, f: &str| tmux(&r.tb, &["display-message", "-p", "-t", p, f]);
    assert_eq!(field(&pane, "#{pane_active}"), "0");
    let left: u32 = field(&pane, "#{pane_left}").parse().unwrap();
    assert!(left > 0, "the panel is at the terminal's first column");
    // The panel has drawn itself at its new width: its frame's top right corner is in its
    // last column.
    let width: usize = field(&pane, "#{pane_width}").parse().unwrap();
    let redrawn = wait_for(SHOWS, || {
        let screen = screen_of(&r.tb, &pane);
        let top = screen.lines().next()?;
        (top.chars().count() == width && top.ends_with('╮')).then_some(())
    });
    assert!(
        redrawn.is_some(),
        "the panel never drew itself {width} wide\n{}",
        tab_shows(&r.tb, &pane)
    );
    r.shows(&pane, &["Kite", "Harbor"]);

    // 1. tmux asked the terminal for every mouse event, motion included.
    let on = wait_for(SHOWS, || {
        r.term
            .tail(usize::MAX)
            .contains("\\u{1b}[?1003h")
            .then_some(())
    });
    assert!(on.is_some(), "no ?1003h: {}", r.term.tail(600));

    // 2. The pointer over a session's row marks it, while the panel stays inactive. The bar
    // wins over the mark on the selected row, so the pointer goes to the session the bar
    // isn't on.
    let bar = r.row_on(&pane, SEL_BG).unwrap_or_default();
    let target = if bar.contains("Kite") {
        "Harbor"
    } else {
        "Kite"
    };
    r.mouse(35, cell_of(&r.tb, &pane, target));
    r.row_becomes(&pane, HOVER_BG, Some(target));
    assert_eq!(
        field(&pane, "#{pane_active}"),
        "0",
        "the pointer made the panel active"
    );

    // 3. A click there opens that session's tab.
    click(&r.tb, &r.term, &pane, target);
    let opened = if target == "Kite" { &kite } else { &harbor };
    r.client_shows(&r.session_pane(&opened.id));

    // 4. Back on the split tab with the panel active, a hovered row loses its mark once
    // the other half is selected.
    tmux(&r.tb, &["select-window", "-t", &pane]);
    tmux(&r.tb, &["select-pane", "-t", &pane]);
    r.client_shows(&pane);
    let bar = r.row_on(&pane, SEL_BG).unwrap_or_default();
    let target = if bar.contains("Kite") {
        "Harbor"
    } else {
        "Kite"
    };
    r.mouse(35, cell_of(&r.tb, &pane, target));
    r.row_becomes(&pane, HOVER_BG, Some(target));
    tmux(&r.tb, &["select-pane", "-t", &other]);
    r.row_becomes(&pane, HOVER_BG, None);
}

#[test]
fn panels_launch_nothing_in_the_daemon() {
    let r = rig();
    let kite = r.session("a", "Kite", None);
    let harbor = r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);
    let before = r.launches();

    for key in ["j", "k", "j", "k", "j", "k", "j", "k", "j", "k"] {
        tmux(&r.tb, &["send-keys", "-t", &pane, key]);
    }
    click(&r.tb, &r.term, &pane, "Kite");
    r.client_shows(&r.session_pane(&kite.id));

    // Twenty moves back and forth over the two sessions' rows, ending on the one the bar
    // isn't on (the bar wins over the mark): that row is marked, so the moves reached the
    // panel.
    tmux(&r.tb, &["select-window", "-t", &pane]);
    r.client_shows(&pane);
    let bar = r.row_on(&pane, SEL_BG).unwrap_or_default();
    let target = if bar.contains("Kite") {
        "Harbor"
    } else {
        "Kite"
    };
    let rows = [
        cell_of(&r.tb, &pane, "Kite"),
        cell_of(&r.tb, &pane, "Harbor"),
    ];
    for i in 0..19u32 {
        let (x, y) = rows[(i % 2) as usize];
        r.mouse(35, (x + i % 4, y));
    }
    r.mouse(35, cell_of(&r.tb, &pane, target));
    r.row_becomes(&pane, HOVER_BG, Some(target));
    // A key no view uses clears the mark; each turn of the wheel marks the row under the
    // pointer again, so the wheel reached the panel too.
    for wheel in [65, 64] {
        tmux(&r.tb, &["send-keys", "-t", &pane, "z"]);
        r.row_becomes(&pane, HOVER_BG, None);
        r.mouse(wheel, cell_of(&r.tb, &pane, target));
        r.row_becomes(&pane, HOVER_BG, Some(target));
    }
    tmux(&r.tb, &["send-keys", "-t", &pane, "r"]);
    r.shows(&pane, &["rename session:"]);
    tmux(&r.tb, &["send-keys", "-t", &pane, "Escape"]);
    r.shows(&pane, &["cancelled"]);

    select_row(&r.tb, &pane, "Harbor");
    r.terminal_key_opens(&pane, "⌨ b", &harbor.dir);

    assert_eq!(r.launches(), before, "a panel launched something");
}

/// Review Focus 4: a panel outlives its daemon. While moa is down it says so and keeps what
/// it showed; once moa runs again it registers again, follows the brain, and `Ctrl-a $` opens
/// where it points.
#[test]
fn a_panel_comes_back_after_moa_restarts() {
    let r = rig();
    let kite = r.session("a", "Kite", None);
    let harbor = r.session("b", "Harbor", None);
    let pane = r.tree(&["Kite", "Harbor"]);
    select_row(&r.tb, &pane, "Harbor");

    shutdown(&r.tb);
    r.shows(
        &pane,
        &["moa isn't running; trying again…", "Kite", "Harbor"],
    );

    r.ok(&["ls"]);
    r.shows(&pane, &[TREE_HINT]);
    r.ok(&["set", &kite.id, "--title", "Kite2"]);
    r.shows(&pane, &["Kite2"]);
    // The cockpit's own panels (built when the terminal attached) register too.
    let registered = wait_for(SHOWS, || {
        let rows = panels(&r.tb);
        let mine: Vec<_> = rows
            .iter()
            .filter(|row| row["pane"] == pane.as_str())
            .collect();
        (mine.len() == 1 && mine[0]["view"] == "tree").then_some(())
    });
    assert!(registered.is_some(), "panels: {:?}", panels(&r.tb));

    let linked = wait_for(SHOWS, || {
        status_json(&r.tb)["tmux"].is_object().then_some(())
    });
    assert!(linked.is_some(), "{}", what_happened(&r.tb, &r.term));
    r.terminal_key_opens(&pane, "⌨ b", &harbor.dir);
}

/// L15: a test that ends right after `panel_tab` takes its root down while the panel's first
/// connect may still be under way. That connect finds moa stopped and starts a daemon of its
/// own, which nobody stops, under a root about to be removed. Once the TempBrain is gone, no
/// daemon holds the brain's lock and the terminal server has exited.
///
/// It is timing-bound: it catches the leak only when the panel's first connect lands while
/// the teardown is under way, as it did in 6 runs of 6 before the fix. A run where the
/// connect lands earlier or later passes either way.
#[test]
fn a_panel_started_at_the_end_leaves_nothing_behind() {
    let r = rig();
    // Held open past the root: whoever holds its lock then outlived the TempBrain.
    let lock = fs::File::open(r.tb.paths().lock(B)).unwrap();
    let server: u32 = server_pid(&r.tb).parse().unwrap();
    let root = r.tb.root().to_path_buf();
    panel_tab(&r.tb, "tree");
    drop(r);
    assert!(
        lock.try_lock().is_ok(),
        "a daemon outlived {}",
        root.display()
    );
    let gone = wait_for(SHOWS, || (!pid_alive(server)).then_some(()));
    assert!(gone.is_some(), "the terminal server outlived its root");
    assert!(!root.exists());
}

/// A panel moa placed itself (`MOA_NO_AUTOSTART`, as every cockpit panel has) never starts
/// moa: with moa stopped, its first connect waits as a reconnect does, and it links once
/// moa runs again. One started by hand still starts moa.
#[test]
fn a_panel_moa_placed_never_starts_moa() {
    let r = rig();
    shutdown(&r.tb);
    // No moa holds the brain's lock: it catches one still starting, which its socket
    // wouldn't yet. Only where no moa should be starting: one starting in the instant the
    // lock is tried finds it held, takes it for another moa, and gives up.
    let free = || {
        fs::File::open(r.tb.paths().lock(B))
            .is_ok_and(|f| f.try_lock().is_ok())
            .then_some(())
    };
    assert!(wait_for(SHOWS, free).is_some(), "moa didn't stop");

    let cmd = format!("'{MOA}' panel tree");
    let placed = tmux(
        &r.tb,
        &[
            "new-window",
            "-d",
            "-t",
            "=moa:",
            "-e",
            "MOA_NO_AUTOSTART=1",
            "-P",
            "-F",
            "#{pane_id}",
            &cmd,
        ],
    );
    r.shows(&placed, &["moa isn't running; trying again…"]);
    assert!(free().is_some(), "the placed panel started moa");

    // One started by hand starts it, and the placed one links then. That moa is starting
    // now, so it is asked on its socket, never by its lock.
    panel_tab(&r.tb, "tree");
    let socket = r.tb.paths().socket(B).unwrap();
    let answers = || {
        std::os::unix::net::UnixStream::connect(&socket)
            .is_ok()
            .then_some(())
    };
    let held = wait_for(SHOWS, answers);
    assert!(held.is_some(), "a panel started by hand didn't start moa");
    r.shows(&placed, &[TREE_HINT]);
}

/// L14: a panel told to stop by a signal (`kill`, or tmux's hang-up when it kills the tab)
/// gives its terminal back first: the tab it leaves is off the alternate screen with the
/// mouse off, and `panel.log` names the signal. The tab stays after the exit
/// (`remain-on-exit`), so tmux can still be asked.
#[test]
fn sigterm_and_sighup_give_the_terminal_back() {
    let r = rig();
    let log = r.tb.paths().log_dir().join("panel.log");
    for signal in ["TERM", "HUP"] {
        let cmd = format!("exec '{MOA}' panel tree");
        let pane = tmux(
            &r.tb,
            &[
                "new-window",
                "-d",
                "-t",
                "=moa:",
                "-P",
                "-F",
                "#{pane_id}",
                &cmd,
            ],
        );
        tmux(
            &r.tb,
            &["set-option", "-p", "-t", &pane, "remain-on-exit", "on"],
        );
        r.shows(&pane, &["▤ sessions", TREE_HINT]);
        let field = |f: &str| tmux(&r.tb, &["display-message", "-p", "-t", &pane, f]);
        assert_eq!(field("#{alternate_on} #{mouse_any_flag}"), "1 1");

        let pid = field("#{pane_pid}");
        let killed = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &pid])
            .status()
            .unwrap();
        assert!(killed.success(), "kill -{signal} {pid}");
        let dead = wait_for(SHOWS, || (field("#{pane_dead}") == "1").then_some(()));
        assert!(dead.is_some(), "{}", tab_shows(&r.tb, &pane));
        // tmux 3.4 and 3.5 built with utempter can lose the panel's exit (moa-tmux's
        // `every_hook_reaches_the_mailbox` says how): the pane is dead with no exit status
        // until tmux reaps a child. moa nudges only the panes it tagged; this one is the
        // test's, so the test nudges.
        tmux(&r.tb, &["run-shell", "true"]);

        let ended = wait_for(SHOWS, || {
            let f = field("#{alternate_on} #{mouse_any_flag} #{pane_dead_status}");
            (f == "0 0 0").then_some(())
        });
        assert!(
            ended.is_some(),
            "SIG{signal}: {:?}\n{}",
            field("#{alternate_on} #{mouse_any_flag} #{pane_dead_status}"),
            tab_shows(&r.tb, &pane)
        );
        let written = fs::read_to_string(&log).unwrap_or_default();
        let line = format!("panel tree in {pane}: stopped by SIG{signal}");
        assert!(written.contains(&line), "{line:?} not in:\n{written}");
        assert!(
            written.contains(&format!("panel tree in {pane}: started")),
            "{written}"
        );
    }
}

/// L14's third way out, the one moa's own layout changes take: tmux kills the panel's pane
/// (`kill-pane`) while its tab lives on. The terminal closes and tmux's SIGHUP comes at the
/// same moment; whichever the panel hears first, `panel.log` gets exactly one end line for
/// it, and it isn't a panic.
#[test]
fn a_panel_tmux_kills_logs_one_plain_end_line() {
    let r = rig();
    let log = r.tb.paths().log_dir().join("panel.log");
    let cmd = format!("exec '{MOA}' panel tree");
    let pane = tmux(
        &r.tb,
        &[
            "new-window",
            "-d",
            "-t",
            "=moa:",
            "-P",
            "-F",
            "#{pane_id}",
            &cmd,
        ],
    );
    // A second pane keeps the tab when the panel's goes.
    tmux(&r.tb, &["split-window", "-d", "-t", &pane]);
    r.shows(&pane, &["▤ sessions", TREE_HINT]);
    let pid: u32 = tmux(
        &r.tb,
        &["display-message", "-p", "-t", &pane, "#{pane_pid}"],
    )
    .parse()
    .unwrap();

    tmux(&r.tb, &["kill-pane", "-t", &pane]);
    let gone = wait_for(SHOWS, || (!pid_alive(pid)).then_some(()));
    assert!(gone.is_some(), "the panel outlived its pane");

    let written = fs::read_to_string(&log).unwrap_or_default();
    let mine = format!("panel tree in {pane}: ");
    let ends: Vec<&str> = written
        .lines()
        .filter_map(|l| l.split_once(&mine).map(|(_, what)| what))
        .filter(|what| is_end(what))
        .collect();
    assert_eq!(ends.len(), 1, "{ends:?}\n{written}");
    assert!(ends[0].starts_with("stopped"), "{ends:?}\n{written}");
}

/// A child that is killed (SIGKILL) and reaped when dropped, so a test that fails leaves no
/// panel behind.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// L14: a panel whose terminal goes with no signal at all (its pty's master closed; the pty
/// isn't its controlling terminal, so nothing sends SIGHUP) ends by itself, with one end
/// line, rather than spinning in the terminal's read and ignoring SIGTERM.
///
/// The panel is linked to moa and idle (an empty tree: no timer, no push), so the only
/// thing that can end it is noticing its terminal went.
#[test]
fn a_panel_whose_terminal_goes_ends_by_itself() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    let r = rig();
    let log = r.tb.paths().log_dir().join("panel.log");
    let (master, slave, _) = moa_testkit::pty(80, 24).unwrap();
    let child = Command::new(MOA)
        .args(["panel", "tree"])
        .env_clear()
        .envs(r.tb.env())
        .current_dir(r.tb.root())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let mut child = Reaped(child);
    let pid = child.0.id();

    // What the panel draws is read until it is linked and in its loop; then the reader lets
    // go of its copy of the master, so dropping `master` closes the pty's last master end.
    let seen = Arc::new(Mutex::new(String::new()));
    let reader = {
        let mut master = master.try_clone().unwrap();
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = master.read(&mut buf) {
                let mut seen = seen.lock().unwrap();
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                if n == 0 || plain(&seen).contains(TREE_HINT) {
                    return;
                }
            }
        })
    };
    let drawn = wait_for(SHOWS, || reader.is_finished().then_some(()));
    assert!(
        drawn.is_some(),
        "the panel never drew: {:?}",
        seen.lock().unwrap()
    );
    drop(master);

    let ended = wait_for(SHOWS, || child.0.try_wait().ok().flatten());
    if ended.is_none() {
        kill_term(pid);
        let termed = wait_for(SHOWS, || child.0.try_wait().ok().flatten());
        panic!(
            "the panel outlived its terminal; SIGTERM {} it",
            if termed.is_some() {
                "ended"
            } else {
                "didn't end"
            }
        );
    }
    let written = fs::read_to_string(&log).unwrap_or_default();
    let ends: Vec<&str> = written
        .lines()
        .filter_map(|l| l.split_once("panel tree in -: ").map(|(_, what)| what))
        .filter(|what| is_end(what))
        .collect();
    assert_eq!(ends, ["stopped: the terminal closed"], "{written}");
}

/// A panel with no terminal at all (stdin `/dev/null`, no controlling terminal, as from a
/// script or a service) refuses with one line and fails: it never takes "no terminal" for
/// a terminal that went.
#[test]
fn a_panel_with_no_terminal_refuses() {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let tb = TempBrain::new(None);
    let log = tb.paths().log_dir().join("panel.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let mut cmd = Command::new(MOA);
    cmd.args(["panel", "tree"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_NO_AUTOSTART", "1")
        .current_dir(tb.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    moa_testkit::without_terminal(&mut cmd);
    let mut child = Reaped(cmd.spawn().unwrap());
    let status = wait_for(SHOWS, || child.0.try_wait().ok().flatten());
    let status = status.expect("the panel with no terminal never ended");
    let mut err = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(!status.success(), "it succeeded; stderr: {err}");
    assert!(err.starts_with("can't use this terminal: "), "{err:?}");
    assert_eq!(err.lines().count(), 1, "{err:?}");
    let written = fs::read_to_string(&log).unwrap_or_default();
    let ends: Vec<&str> = written
        .lines()
        .filter_map(|l| l.split_once("panel tree in -: ").map(|(_, what)| what))
        .filter(|what| is_end(what))
        .collect();
    assert_eq!(ends.len(), 1, "{written}");
    assert!(
        ends[0].starts_with("stopped: can't use this terminal: "),
        "{written}"
    );
}

/// L14: a terminal that goes while the panel is still taking it or drawing its first
/// frame ends the panel the same way. Nobody reads the pty: on macOS, whose pty buffer is
/// small, the panel's first writes fill it and wait, and the master end closes under them;
/// on Linux they fit, and the hang-up watch ends it. Whichever part notices first (a
/// write, the first draw, the watch), the one end line is `stopped: the terminal closed`.
#[test]
fn a_panel_whose_terminal_goes_before_its_first_frame_says_so() {
    use std::process::{Command, Stdio};

    let tb = TempBrain::new(None);
    let log = tb.paths().log_dir().join("panel.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let (master, slave, _) = moa_testkit::pty(200, 60).unwrap();
    // A placed panel, so it starts no moa.
    let child = Command::new(MOA)
        .args(["panel", "tree"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_NO_AUTOSTART", "1")
        .current_dir(tb.root())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let mut child = Reaped(child);

    let started = wait_for(SHOWS, || {
        let written = fs::read_to_string(&log).unwrap_or_default();
        written.contains("panel tree in -: started").then_some(())
    });
    assert!(started.is_some(), "the panel never started");
    drop(master);

    let ended = wait_for(SHOWS, || child.0.try_wait().ok().flatten());
    assert!(ended.is_some(), "the panel outlived its terminal");
    let written = fs::read_to_string(&log).unwrap_or_default();
    let ends: Vec<&str> = written
        .lines()
        .filter_map(|l| l.split_once("panel tree in -: ").map(|(_, what)| what))
        .filter(|what| is_end(what))
        .collect();
    assert_eq!(ends, ["stopped: the terminal closed"], "{written}");
}

/// What a panel's input thread relies on: `terminal_gone` says no while the terminal is
/// there (input waiting unread included) and yes once its master end closes, and
/// `wait_hangup` returns then.
#[test]
fn a_closed_master_is_heard_as_the_terminal_going() {
    use std::io::Write;
    use std::os::fd::AsFd;

    let (mut master, slave, _) = moa_testkit::pty(80, 24).unwrap();
    master.write_all(b"typed\n").unwrap();
    assert!(!moa_platform::terminal_gone(slave.as_fd()), "unread input");

    let watched = slave.try_clone().unwrap();
    let waiting = std::thread::spawn(move || moa_platform::wait_hangup(watched.as_fd()));
    drop(master);
    let woke = wait_for(SHOWS, || waiting.is_finished().then_some(()));
    assert!(woke.is_some(), "a closed master never woke it");
    waiting.join().unwrap().unwrap();
    assert!(moa_platform::terminal_gone(slave.as_fd()));
}

/// A key that comes with a resize is read at once, not when the next key comes. crossterm's
/// default reader (0.29, kqueue or epoll, edge-triggered) could take a SIGWINCH and the
/// terminal's input in one wake, answer the resize and drop the input's wake: the key then
/// waited, unread, for more input (the cockpit's focus test lost a `j` so, when tmux resized
/// its tree as the key came). Here each key is typed amid a burst of SIGWINCH, and must show
/// before anything else is typed.
#[test]
fn a_key_amid_resizes_is_read_at_once() {
    let tb = TempBrain::new(None);
    let term = open(&tb, &["panel", "tree"], &[]);
    assert!(
        term.wait_for_text(TREE_HINT, SHOWS),
        "the tree never drew: {}",
        term.tail(600)
    );
    // `+` opens the footer's editor; each letter then shows in it.
    let mut typed = String::new();
    for key in "+abcdefghij".chars() {
        let want = if key == '+' {
            "new group:".to_string()
        } else {
            typed.push(key);
            format!("new group: {typed}")
        };
        term.winch(50).unwrap();
        term.press(key.to_string().as_bytes());
        term.winch(50).unwrap();
        let shown = wait_for(SHOWS, || {
            let screen = common::screen::Screen::of(&term).text();
            screen.contains(&want).then_some(())
        });
        assert!(
            shown.is_some(),
            "{key:?}, typed amid resizes, was never read: no {want:?} in\n{}",
            common::screen::Screen::of(&term).text()
        );
    }
    term.press(b"\x1b");
}

// --- Popups (S8): `moa panel picker --once`, run here in a tab of its own ---

/// The picker's footer hint: it shows once the picker draws.
const PICKER_HINT: &str = "↵ jump";

/// A new tab in `moa` running `cmd`, with `env` (`K=V`) as well; its pane, which stays
/// when its program ends.
fn tab_running(tb: &TempBrain, cmd: &str, env: &[&str]) -> String {
    let mut args = vec!["new-window", "-d", "-t", "=moa:", "-P", "-F", "#{pane_id}"];
    for e in env {
        args.extend(["-e", e]);
    }
    args.push(cmd);
    let pane = tmux(tb, &args);
    tmux(
        tb,
        &["set-option", "-p", "-t", &pane, "remain-on-exit", "on"],
    );
    pane
}

/// Waits until `pane`'s program has ended, with status 0.
#[track_caller]
fn ended_well(tb: &TempBrain, pane: &str) {
    let field = |f: &str| tmux(tb, &["display-message", "-p", "-t", pane, f]);
    let dead = wait_for(SHOWS, || (field("#{pane_dead}") == "1").then_some(()));
    assert!(
        dead.is_some(),
        "{pane} never ended\n{}",
        tab_shows(tb, pane)
    );
    // tmux 3.4 and 3.5 built with utempter can lose the exit until tmux reaps a child (see
    // `sigterm_and_sighup_give_the_terminal_back`); this tab is the test's, so the test
    // nudges.
    tmux(tb, &["run-shell", "true"]);
    let status = wait_for(SHOWS, || {
        let s = field("#{pane_dead_status}");
        (!s.is_empty()).then_some(s)
    });
    assert_eq!(status.as_deref(), Some("0"), "{}", tab_shows(tb, pane));
}

/// S8: a popup's panel claims no tab, whatever `TMUX_PANE` says (tmux gives a popup none;
/// here it runs in a tab, which has one): no `panels` row names its tab, and its own row
/// has none. Esc closes it with status 0, and `panel.log` has its start line and its end
/// line, naming it `picker (popup)`.
#[test]
fn a_once_panel_claims_no_pane_and_exits_on_esc() {
    let r = rig();
    r.session("a", "Kite", None);
    let pane = tab_running(&r.tb, &format!("exec '{MOA}' panel picker --once"), &[]);
    r.shows(&pane, &["Kite", PICKER_HINT]);
    let rows = || {
        let rows = panels(&r.tb);
        let popups = rows
            .iter()
            .filter(|row| row["view"] == "picker" && row["pane"].is_null())
            .count();
        let named = rows.iter().any(|row| row["pane"] == pane.as_str());
        (popups == 1 && !named).then_some(())
    };
    assert!(
        wait_for(SHOWS, rows).is_some(),
        "panels: {:?}",
        panels(&r.tb)
    );
    // Drawn without a frame of its own: tmux frames the popup.
    assert!(
        !screen_of(&r.tb, &pane).contains('╭'),
        "{}",
        tab_shows(&r.tb, &pane)
    );

    tmux(&r.tb, &["send-keys", "-t", &pane, "Escape"]);
    ended_well(&r.tb, &pane);
    let log = fs::read_to_string(r.tb.paths().log_dir().join("panel.log")).unwrap_or_default();
    let mine: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split_once("panel picker (popup) in -: ").map(|(_, w)| w))
        .collect();
    assert_eq!(mine, ["started", "stopped: quit"], "{log}");
}

/// S8: a popup moa's own key started (`MOA_NO_AUTOSTART`) with moa down never starts moa:
/// it says moa isn't running, and any key, Ctrl-c among them, closes it with status 0. It
/// isn't moa's: it draws its view, and Ctrl-c never shows the managed line. `moa pick` is
/// the same panel.
#[test]
fn a_once_panel_with_moa_down_says_so_and_exits() {
    let r = rig();
    shutdown(&r.tb);
    let free = || {
        fs::File::open(r.tb.paths().lock(B))
            .is_ok_and(|f| f.try_lock().is_ok())
            .then_some(())
    };
    assert!(wait_for(SHOWS, free).is_some(), "moa didn't stop");

    let picks = [
        (format!("exec '{MOA}' panel picker --once"), "x"),
        (format!("exec '{MOA}' pick"), "C-c"),
    ];
    for (cmd, key) in picks {
        let pane = tab_running(&r.tb, &cmd, &["MOA_NO_AUTOSTART=1"]);
        r.shows(&pane, &["moa isn't running", "›"]);
        // That it ends is the check that it isn't moa's: a managed panel's Ctrl-c shows the
        // managed line and stays.
        tmux(&r.tb, &["send-keys", "-t", &pane, key]);
        ended_well(&r.tb, &pane);
        assert!(free().is_some(), "{cmd} started moa");
    }
}

/// S8: only the picker and help draw as a popup.
#[test]
fn once_refuses_other_views() {
    let tb = TempBrain::new(None);
    let o = run(&tb, &["panel", "tree", "--once"], &[]);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("--once works only for the picker and help"),
        "{}",
        err(&o)
    );
}

/// `plain` drops every escape sequence `capture-pane -e` or a terminal may write, and only
/// those: a CSI up to its final byte, an OSC, DCS, APC or PM up to BEL or ST, a charset
/// designation with its one character, and any other two-character escape.
#[test]
fn plain_skips_whole_escapes_and_nothing_more() {
    assert_eq!(plain("\x1b[48;2;94;129;172mKite\x1b[0m"), "Kite");
    assert_eq!(plain("a\x1b]0;title\x07b"), "ab");
    assert_eq!(plain("a\x1b]2;title\x1b\\b"), "ab");
    assert_eq!(plain("a\x1bPq#0\x1b\\b"), "ab");
    assert_eq!(plain("a\x1b(Bb"), "ab");
    assert_eq!(plain("a\x1b=b"), "ab");
    assert_eq!(plain("Harbor\x1b"), "Harbor");
}
