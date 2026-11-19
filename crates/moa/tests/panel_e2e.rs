//! Slice 5a end to end: real `moa panel`s in tabs of a TempBrain's own terminal server, `moa`
//! on a `Terminal` attached to it, and fake-claude sessions `moactl open --bg` started. The
//! panels are driven as a user drives them: keys sent to their tab, clicks, the pointer and
//! the wheel typed on the terminal as mouse reports, `Ctrl-a $`, and moa stopped and started
//! again under them. Every wait is for what tmux shows or moa says, never for a fixed time.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use moa_client::{Client, ClientOptions, OnMismatch};
use moa_core::config::BrainName;
use moa_core::publish::Topic;
use moa_core::state::CallerInfo;
use moa_platform::{build_id_of, canonical};
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
                Some((rows, bar, want))
            });
            let Some((rows, bar, want)) = found else {
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
            let moved = wait_for(SHOWS, || (self.colours(pane) != rows).then_some(()));
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
    /// terminal tab named `name` comes up, in `dir`.
    #[track_caller]
    fn terminal_key_opens(&self, pane: &str, name: &str, dir: &Path) {
        tmux(&self.tb, &["select-window", "-t", pane]);
        tmux(&self.tb, &["select-pane", "-t", pane]);
        self.client_shows(pane);
        let before = self.tabs_named(name).len();
        ctrl_a(&self.tb, &self.term);
        self.term.press(b"$");
        let opened = wait_for(SHOWS, || {
            let tabs = self.tabs_named(name);
            (tabs.len() > before).then(|| tabs.last().cloned())?
        });
        let opened = opened.unwrap_or_else(|| {
            panic!(
                "Ctrl-a $ opened no {name:?} tab\n{}",
                what_happened(&self.tb, &self.term)
            )
        });
        assert_eq!(opened, *dir);
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

/// The rows of the `panels` topic, as a client subscribed to it sees them now.
fn panels(tb: &TempBrain) -> Vec<Value> {
    let opts = ClientOptions {
        paths: tb.paths(),
        brain: BrainName::default_brain(),
        client: "panel-e2e",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: OnMismatch::Accept,
        build: build_id_of(Path::new(MOA)).unwrap(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let Ok(mut client) = Client::connect(opts).await else {
            return Vec::new();
        };
        let Ok(snapshot) = client.subscribe(&[Topic::Panels]).await else {
            return Vec::new();
        };
        snapshot
            .topics
            .get(&Topic::Panels)
            .cloned()
            .unwrap_or_default()
    })
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
    // The panel's tab, split: the panel is active first, then the other half is.
    let other = tmux(
        &r.tb,
        &[
            "split-window",
            "-h",
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
    let active = |p: &str| tmux(&r.tb, &["display-message", "-p", "-t", p, "#{pane_active}"]);
    assert_eq!(active(&pane), "0");
    // The panel's rows move with its new width: it shows the titles again.
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
    assert_eq!(active(&pane), "0", "the pointer made the panel active");

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

    tmux(&r.tb, &["select-window", "-t", &pane]);
    r.client_shows(&pane);
    let at = cell_of(&r.tb, &pane, "Harbor");
    r.mouse(65, at);
    r.mouse(64, at);
    for i in 0..20 {
        let (x, y) = at;
        r.mouse(35, (x + i % 4, y - i % 2));
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
    let registered = wait_for(SHOWS, || {
        let rows = panels(&r.tb);
        let one = rows.len() == 1 && rows[0]["pane"] == pane.as_str() && rows[0]["view"] == "tree";
        one.then_some(())
    });
    assert!(registered.is_some(), "panels: {:?}", panels(&r.tb));

    let linked = wait_for(SHOWS, || {
        status_json(&r.tb)["tmux"].is_object().then_some(())
    });
    assert!(linked.is_some(), "{}", what_happened(&r.tb, &r.term));
    r.terminal_key_opens(&pane, "⌨ b", &harbor.dir);
}
