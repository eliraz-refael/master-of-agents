//! Slice 5a end to end: real `moa panel`s in tabs of a TempBrain's own terminal server, `moa`
//! on a `Terminal` attached to it, and fake-claude sessions `moactl open --bg` started. The
//! panels are driven as a user drives them: keys sent to their tab, clicks, the pointer and
//! the wheel typed on the terminal as mouse reports, `Ctrl-a $`, and moa stopped and started
//! again under them. Every wait is for what tmux shows or moa says, never for a fixed time.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use moa_platform::{canonical, pid_alive};
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for, write_script};
use serde_json::{Map, Value};

use common::*;

/// The selection bar's background, `sel` (`#5e81ac`), as `capture-pane -e` writes it.
const SEL_BG: &str = "48;2;94;129;172";
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

/// A session `moactl open` started, and where.
struct Opened {
    id: String,
    dir: PathBuf,
}

/// `text` without its escape sequences.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // `ESC [ params final`: up to the final byte.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Rig {
    /// `moactl args…` to the end.
    fn moactl(&self, args: &[&str]) -> std::process::Output {
        run_exe(&self.tb, &moactl(&self.tb), args, &[("MOA_BRAIN", B)])
    }

    /// `moactl args…`, which must succeed; its stdout.
    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let o = self.moactl(args);
        assert!(
            o.status.success(),
            "moactl {args:?}: {}{}\n{}",
            out(&o),
            err(&o),
            what_happened(&self.tb, &self.term)
        );
        out(&o)
    }

    /// `moactl open --bg <root>/work/<name> --title <title>` (and `--group` when given),
    /// with fake-claude starting and hanging there.
    #[track_caller]
    fn session(&self, name: &str, title: &str, group: Option<&str>) -> Opened {
        let dir = self.tb.root().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        let start = FakeStep::Hook {
            event: "SessionStart".into(),
            fields: Map::from_iter([("source".to_string(), Value::String("startup".into()))]),
        };
        write_script(&dir, &[start, FakeStep::Hang]);
        let path = dir.display().to_string();
        let mut args = vec!["--json", "open", "--bg", &path, "--title", title];
        if let Some(group) = group {
            args.extend(["--group", group]);
        }
        let data: Value = serde_json::from_str(&self.ok(&args)).unwrap();
        let id = data["id"].as_str().unwrap_or_else(|| panic!("{data}"));
        Opened {
            id: id.to_string(),
            dir: canonical(&dir),
        }
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
        let seen = wait_for(SHOWS, || {
            let screen = screen_of(&self.tb, pane);
            texts.iter().all(|t| screen.contains(t)).then_some(())
        });
        assert!(
            seen.is_some(),
            "{pane} never showed {texts:?}\n{}",
            tab_shows(&self.tb, pane)
        );
    }

    /// `pane`'s rows with their colours, as `capture-pane -e` writes them.
    fn colours(&self, pane: &str) -> Vec<String> {
        let shown = tmux(&self.tb, &["capture-pane", "-p", "-e", "-t", pane]);
        shown.lines().map(str::to_string).collect()
    }

    /// The text of `pane`'s row drawn on background `bg`, if one is.
    fn row_on(&self, pane: &str, bg: &str) -> Option<String> {
        self.colours(pane)
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

    /// Presses `j` or `k` in `pane` until the selection bar is on the row showing `text`.
    #[track_caller]
    fn select(&self, pane: &str, text: &str) {
        for _ in 0..20 {
            // Where the bar is, and the row showing `text`.
            let found = wait_for(SHOWS, || {
                let rows = self.colours(pane);
                let bar = rows.iter().position(|l| l.contains(SEL_BG))?;
                let want = rows.iter().position(|l| plain(l).contains(text))?;
                Some((bar, want))
            });
            let Some((bar, want)) = found else {
                panic!(
                    "{pane} draws no bar, or no {text:?}\n{}",
                    tab_shows(&self.tb, pane)
                );
            };
            if bar == want {
                return;
            }
            let key = if want > bar { "j" } else { "k" };
            tmux(&self.tb, &["send-keys", "-t", pane, key]);
            // The key has landed once the bar is on another row.
            let moved = wait_for(SHOWS, || {
                let rows = self.colours(pane);
                let now = rows.iter().position(|l| l.contains(SEL_BG));
                now.filter(|now| *now != bar)
            });
            assert!(
                moved.is_some(),
                "{key} left the bar on row {bar}, not {text:?}'s {want}\n{}",
                tab_shows(&self.tb, pane)
            );
        }
        panic!("the bar never reached {text:?}");
    }

    /// The pane of the tab running session `id`.
    #[track_caller]
    fn session_pane(&self, id: &str) -> String {
        let found = wait_for(SHOWS, || {
            let list = tmux(
                &self.tb,
                &["list-panes", "-a", "-F", "#{pane_id} #{@moa_session}"],
            );
            list.lines().find_map(|l| {
                let (pane, session) = l.split_once(' ')?;
                (session == id).then(|| pane.to_string())
            })
        });
        found.unwrap_or_else(|| panic!("no tab runs {id}\n{}", what_happened(&self.tb, &self.term)))
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

    r.select(&pane, "Harbor");
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

    r.select(&pane, "Harbor");
    r.terminal_key_opens(&pane, "⌨ b", &harbor.dir);
}

#[test]
fn a_fold_is_kept_across_panels() {
    let r = rig();
    r.session("a", "Kite", Some("Private"));
    let first = r.tree(&["▾ Private", "Kite"]);

    r.select(&first, "Private");
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

    r.select(&pane, "Harbor");
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
    r.select(&pane, "Harbor");

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

    // One started by hand starts it, and the placed one links then.
    panel_tab(&r.tb, "tree");
    let held = wait_for(SHOWS, || free().is_none().then_some(()));
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

        assert_eq!(
            field("#{alternate_on} #{mouse_any_flag} #{pane_dead_status}"),
            "0 0 0",
            "SIG{signal}\n{}",
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
