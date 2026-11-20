//! Slice 5c end to end: the picker as a popup and as a full tab, help in both forms, and
//! `Ctrl-a n`, on a TempBrain's own terminal server, with real `moa` daemons and panels,
//! fake-claude sessions `moactl open --bg` started, and `moa` on a `Terminal`. Keys are typed
//! on the terminal as a user types them. A popup is no pane, so what it draws is read from
//! the terminal's screen; what moa did is read back from tmux, `moa status` and
//! `panel.log`. Every wait is for what tmux shows or moa says, never for a fixed time.

mod common;

use std::fs;

use moa_core::layout::{leaves, parse, presets};
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for, write_script};
use serde_json::{Map, Value};

use common::screen::Screen;
use common::*;

/// The title tmux draws on the picker's popup's border.
const PICKER_TITLE: &str = "⌕ find a session";
/// Esc, Enter and Ctrl-u (which empties tmux's prompt), typed on a terminal.
const ESC: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";
const CTRL_U: &[u8] = b"\x15";
/// A terminal tall enough for help to show its "In every panel" section unscrolled, as a
/// popup (90% of it) and as a tab.
const TALL: (u16, u16) = (120, 60);

/// A root with the fake account, and `moa` on a terminal with the cockpit built.
struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
}

fn rig() -> Rig {
    rig_with(TERM_SIZE, "")
}

/// [`rig`] on a terminal of `size`, with `more` added to the root's `moa.toml`.
fn rig_with(size: (u16, u16), more: &str) -> Rig {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    let file = tb.paths().config_file();
    let toml = fs::read_to_string(&file).unwrap() + more;
    fs::write(&file, toml).unwrap();
    let term = cockpit_up_sized(&tb, size);
    Rig { term, tb }
}

/// The query a picker shows on its query line (`› query   3/3 · 3 live`), the first line of
/// `drawn` (a panel's screen, or the inside of its popup) that has a `›`: the words after
/// it, up to the match count.
fn query_on(drawn: &str) -> Option<String> {
    let line = drawn.lines().find(|l| l.contains('›'))?;
    let (_, after) = line.split_once('›')?;
    let count = |w: &str| {
        w.split_once('/').is_some_and(|(a, b)| {
            [a, b]
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        })
    };
    let words: Vec<&str> = after.split_whitespace().take_while(|w| !count(w)).collect();
    Some(words.join(" "))
}

impl Rig {
    /// Ctrl-a `key` on the terminal.
    fn key(&self, key: &str) {
        ctrl_a(&self.tb, &self.term);
        self.term.press(key.as_bytes());
    }

    /// Waits until the terminal says `text`, since the last key.
    #[track_caller]
    fn says(&self, text: &str) {
        assert!(
            self.term.wait_for_text(text, SHOWS),
            "the terminal never said {text:?}\n{}",
            what_happened(&self.tb, &self.term)
        );
    }

    /// Waits until the terminal's screen shows each of `texts`, or with `shown` false none.
    #[track_caller]
    fn on_screen(&self, texts: &[&str], shown: bool) {
        screen_shows(&self.term, texts, shown);
    }

    /// The daemon's launches and its panel starts.
    fn counts(&self) -> (u64, u64) {
        let s = status_json(&self.tb);
        let n = |k: &str| s[k].as_u64().unwrap_or_else(|| panic!("no {k}: {s}"));
        (n("launches"), n("panel_starts"))
    }

    /// A session `moactl open --bg` started in `<root>/work/<name>`, titled `title`; its id.
    #[track_caller]
    fn session(&self, name: &str, title: &str) -> String {
        open_session(&self.tb, &self.term, name, title, None).id
    }

    /// The pane the terminal shows.
    fn client_pane(&self) -> Option<String> {
        client_field(&self.tb, &self.term, "#{pane_id}")
    }

    /// The picker panels parked in `_moa` (a linked one is listed there too).
    fn pickers(&self) -> Vec<PaneRow> {
        panes_of(&self.tb, "=_moa")
            .into_iter()
            .filter(|p| p.tag == "picker")
            .collect()
    }

    /// Whether moa's session has the picker's tab.
    fn picker_in_moa(&self) -> bool {
        panes_of(&self.tb, "=moa").iter().any(|p| p.tag == "picker")
    }

    /// `Ctrl-a s`: the terminal shows the picker's tab, linked into moa's session, once its
    /// panel draws its hint and each of `shows`. The picker panel parked in `_moa`, which
    /// must be the only one.
    #[track_caller]
    fn open_picker_tab(&self, shows: &[&str]) -> PaneRow {
        self.key("s");
        let shown = wait_for(SHOWS, || {
            let pane = self.client_pane()?;
            let picker = self.pickers().into_iter().find(|p| p.id == pane)?;
            let screen = screen_of(&self.tb, &picker.id);
            let drawn = screen.contains(PICKER_SHOWS) && shows.iter().all(|t| screen.contains(t));
            (drawn && self.picker_in_moa()).then_some(picker)
        });
        let picker = shown.unwrap_or_else(|| {
            panic!(
                "the picker's tab never showed {shows:?}: {:#?}\n{}",
                self.pickers(),
                what_happened(&self.tb, &self.term)
            )
        });
        let all = self.pickers();
        assert_eq!(all.len(), 1, "more than one picker panel: {all:#?}");
        picker
    }

    /// Waits until moa's session has no picker tab.
    #[track_caller]
    fn picker_closes(&self) {
        let gone = wait_for(SHOWS, || (!self.picker_in_moa()).then_some(()));
        assert!(
            gone.is_some(),
            "the picker's tab is still in moa's session: {:#?}\n{}",
            panes_of(&self.tb, "=moa"),
            what_happened(&self.tb, &self.term)
        );
    }

    /// Waits until the query the picker in `pane` shows is `want`.
    #[track_caller]
    fn tab_query_becomes(&self, pane: &str, want: &str) {
        let seen = wait_for(SHOWS, || {
            (query_on(&screen_of(&self.tb, pane)).as_deref() == Some(want)).then_some(())
        });
        assert!(
            seen.is_some(),
            "the picker's query never became {want:?}\n{}",
            tab_shows(&self.tb, pane)
        );
    }

    /// `Ctrl-a /`: the picker's popup draws each of `shows` inside its border, and its panel
    /// is connected to moa.
    #[track_caller]
    fn open_picker_popup(&self, shows: &[&str]) {
        self.key("/");
        let up = wait_for(SHOWS, || (popups(&self.tb, "picker") == 1).then_some(()));
        assert!(
            up.is_some(),
            "the picker's popup never connected: {:?}\n{}",
            panels(&self.tb),
            what_happened(&self.tb, &self.term)
        );
        popup_shows(&self.term, PICKER_TITLE, shows);
    }

    /// The picker's popup is gone: its panel left moa, and tmux closed it, off the
    /// terminal's screen.
    #[track_caller]
    fn picker_popup_goes(&self) {
        let gone = wait_for(SHOWS, || (popups(&self.tb, "picker") == 0).then_some(()));
        assert!(
            gone.is_some(),
            "the picker's popup never closed\n{}",
            what_happened(&self.tb, &self.term)
        );
        popup_closes(&self.tb, &self.term, PICKER_TITLE);
    }

    /// Waits until the query inside the picker's popup is `want`.
    #[track_caller]
    fn popup_query_becomes(&self, want: &str) {
        let seen = wait_for(SHOWS, || {
            let inside = popup_text(&self.term, PICKER_TITLE)?;
            (query_on(&inside).as_deref() == Some(want)).then_some(())
        });
        assert!(
            seen.is_some(),
            "the popup's query never became {want:?}:\n{}",
            Screen::of(&self.term).text()
        );
    }

    /// The session in the cockpit's agent spot, if an agent is there.
    fn spot_session(&self) -> Option<String> {
        let spot = cockpit_panes(&self.tb)
            .into_iter()
            .find(|p| p.tag == "agent")?;
        Some(tmux(
            &self.tb,
            &["display-message", "-p", "-t", &spot.id, "#{@moa_session}"],
        ))
    }
}

/// fake-claude's `SessionStart` hook, as a resumed session's agent sends it.
fn resume_hook() -> FakeStep {
    FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::from_iter([("source".to_string(), Value::String("resume".into()))]),
    }
}

/// What `panel.log` ends a picker popup with: after a pick moa took (the panel closes on its
/// reply), and after Esc (a key that quits it).
const PICKED: &str = "stopped: closed";
const QUIT: &str = "stopped: quit";

/// Waits until `panel.log` has, after its first `from` lines for the picker's popup, its
/// start line and then `end`, and nothing else: a popup that panicked, or ended another
/// way, fails.
#[track_caller]
fn one_popup_logged(tb: &TempBrain, from: usize, end: &str) {
    let logged = wait_for(SHOWS, || {
        let lines = popup_log(tb, "picker");
        (lines.len() >= from + 2).then(|| lines[from..].to_vec())
    });
    let lines = logged.unwrap_or_else(|| {
        panic!(
            "panel.log never logged the popup: {:?}",
            popup_log(tb, "picker")
        )
    });
    assert_eq!(lines, ["started", end]);
}

/// S8 and Review Focus 5: `Ctrl-a /` opens the picker as a popup tmux runs; typing finds
/// a session and Enter brings it up. The one picked is asleep on purpose: its resume opens a
/// tab just as the popup's moa exits, what Ubuntu's tmux 3.4 can lose. Each open is one
/// `moa panel picker --once`, logged once, and none is moa's launch.
#[test]
fn the_popup_picker_finds_and_focuses() {
    let r = rig();
    // A layout with the agent spot, for the pick to fill.
    r.key("2");
    r.says("layout focus applied");
    r.session("lantern", "Lantern");
    let Opened { id: kite, dir } = open_session(&r.tb, &r.term, "kite", "Kite", None);
    r.session("harbor", "Harbor");
    // Idle once its first hook came, so its conversation has a transcript to resume.
    let idle = wait_for(SHOWS, || {
        (ls_display(&r.tb, &kite).as_deref() == Some("idle")).then_some(())
    });
    assert!(idle.is_some(), "{:?}", ls_display(&r.tb, &kite));
    moactl_ok(&r.tb, &r.term, &["sleep", &kite]);
    write_script(&dir, &[resume_hook(), FakeStep::Hang]);
    let asleep = wait_for(SHOWS, || {
        (ls_display(&r.tb, &kite).as_deref() == Some("asleep")).then_some(())
    });
    assert!(asleep.is_some(), "{:?}", ls_display(&r.tb, &kite));
    // Neither moa's launches nor its panel starts: tmux starts the popup, and the resume's
    // tab is tmux's too.
    let counts = r.counts();

    let logged = popup_log(&r.tb, "picker").len();
    r.open_picker_popup(&["Lantern", "Kite", "Harbor"]);
    r.term.press(b"kit");
    r.popup_query_becomes("kit");
    r.term.press(ENTER);
    r.picker_popup_goes();
    let resumed = wait_for(SHOWS, || {
        (r.spot_session().as_deref() == Some(kite.as_str())).then_some(())
    });
    assert!(
        resumed.is_some(),
        "Kite never came into the spot: {:#?}\n{}",
        cockpit_panes(&r.tb),
        what_happened(&r.tb, &r.term)
    );
    one_popup_logged(&r.tb, logged, PICKED);
    assert_eq!(
        r.counts(),
        counts,
        "the popup or its pick started something"
    );

    // Again: one more start line and one more end line, and nothing started.
    let logged = popup_log(&r.tb, "picker").len();
    r.open_picker_popup(&["Lantern", "Kite", "Harbor"]);
    r.term.press(ESC);
    r.picker_popup_goes();
    one_popup_logged(&r.tb, logged, QUIT);
    assert_eq!(r.counts(), counts, "the popup started something");
}

/// S9: the first `Ctrl-a s` starts the picker's panel, parked in `_moa`, and links its tab
/// in; every open after that starts nothing (the same panel, the same process), with an
/// empty query. Esc and a pick both close its tab.
#[test]
fn the_full_tab_picker_starts_once_then_never() {
    let r = rig();
    r.session("lantern", "Lantern");
    r.session("kite", "Kite");
    let harbor = r.session("harbor", "Harbor");
    let (launches, starts) = r.counts();

    let picker = r.open_picker_tab(&["Lantern", "Kite", "Harbor"]);
    assert_eq!(r.counts(), (launches, starts + 1), "{}", status_json(&r.tb));
    r.term.press(b"kit");
    r.tab_query_becomes(&picker.id, "kit");
    r.term.press(ESC);
    r.picker_closes();
    let back = wait_for(SHOWS, || {
        (client_field(&r.tb, &r.term, "#{window_name}").as_deref() == Some("moa")).then_some(())
    });
    assert!(
        back.is_some(),
        "Esc left the terminal on {:?}",
        client_field(&r.tb, &r.term, "#{window_name}")
    );
    let counts = (launches, starts + 1);
    assert_eq!(r.counts(), counts, "closing the picker started something");

    let again = r.open_picker_tab(&[]);
    assert_eq!((&again.id, again.pid), (&picker.id, picker.pid));
    assert_eq!(
        r.counts(),
        counts,
        "opening the picker again started something"
    );
    r.tab_query_becomes(&picker.id, "");
    shows(&r.tb, &picker.id, &["Lantern", "Kite", "Harbor"]);

    r.term.press(b"harb");
    r.tab_query_becomes(&picker.id, "harb");
    r.term.press(ENTER);
    let pane = session_pane(&r.tb, &r.term, &harbor);
    let shown = wait_for(SHOWS, || {
        (r.client_pane().as_deref() == Some(pane.as_str())).then_some(())
    });
    assert!(
        shown.is_some(),
        "Harbor's tab {pane} was never shown\n{}",
        what_happened(&r.tb, &r.term)
    );
    r.picker_closes();
    assert_eq!(r.counts(), counts, "a pick started something");
}

/// Review Focus 2: leaving the picker's tab another way than Esc (`Ctrl-a ]`, the next
/// tab) closes it.
#[test]
fn leaving_the_picker_tab_closes_it() {
    let r = rig();
    let picker = r.open_picker_tab(&[]);
    r.key("]");
    r.picker_closes();
    assert_ne!(
        r.client_pane(),
        Some(picker.id),
        "the terminal stayed on the picker"
    );
}

/// Review Focus 1: a layout key with the picker's tab open leaves its tab, so the picker
/// closes; the build moves and kills nothing of it. It waits parked, the same process, and
/// the next `Ctrl-a s` opens it with nothing started.
#[test]
fn a_layout_switch_with_the_picker_open_keeps_it() {
    let r = rig();
    let picker = r.open_picker_tab(&[]);
    let mut before = panes_of(&r.tb, "=moa");
    before.extend(panes_of(&r.tb, "=_moa"));

    r.key("4");
    let node = parse(&presets().entries["triage"]).unwrap();
    let mut want: Vec<&str> = leaves(&node).iter().map(|l| l.word()).collect();
    want.sort_unstable();
    let built = wait_for(SHOWS, || {
        let mut tags: Vec<String> = cockpit_panes(&r.tb).into_iter().map(|p| p.tag).collect();
        tags.sort_unstable();
        (tags == want).then_some(())
    });
    assert!(
        built.is_some(),
        "the cockpit never held triage's {want:?}: {:#?}\n{}",
        cockpit_panes(&r.tb),
        what_happened(&r.tb, &r.term)
    );
    r.picker_closes();
    let parked: Vec<(String, u32)> = r.pickers().into_iter().map(|p| (p.id, p.pid)).collect();
    assert_eq!(parked, [(picker.id.clone(), picker.pid)]);
    let mut after = panes_of(&r.tb, "=moa");
    after.extend(panes_of(&r.tb, "=_moa"));
    assert!(!after.iter().any(|p| p.dead), "a pane died: {after:#?}");
    for p in &before {
        assert!(
            after.iter().any(|a| a.id == p.id && a.pid == p.pid),
            "{} ({}) is gone or restarted: {after:#?}",
            p.id,
            p.pid
        );
    }

    let counts = r.counts();
    let again = r.open_picker_tab(&[]);
    assert_eq!((&again.id, again.pid), (&picker.id, picker.pid));
    assert_eq!(
        r.counts(),
        counts,
        "opening the picker again started something"
    );
}

/// S10: help lists moa's keys in both forms: as a popup by default, which `q` closes, and as
/// a tab with `[ui] picker = "tab"`, which Esc closes.
#[test]
fn help_shows_the_keys_in_both_forms() {
    let shows = ["find a session", "hold Shift to select text"];
    let r = rig_with(TALL, "");
    the_help_popup_opens_and_closes(&r.tb, &r.term, || r.key("?"), &shows);
    drop(r);

    let r = rig_with(TALL, "\n[ui]\npicker = \"tab\"\n");
    let mut all = vec![HELP_SHOWS];
    all.extend(shows);
    a_sheet_opens_and_closes(&r.tb, &r.term, || r.key("?"), &all, ESC);
}

/// S10 and Review Focus 5: help's popup opens on an 80×24 terminal, narrower than its 84
/// columns, and `q` closes it.
#[test]
fn help_opens_on_a_small_terminal() {
    let r = rig_with((80, 24), "");
    the_help_popup_opens_and_closes(&r.tb, &r.term, || r.key("?"), &[HELP_SHOWS]);
}

/// S5, S6 and Review Focus 3: `Ctrl-a n` asks for a folder, prefilled with the brain's
/// first, then a title, one prompt at a time, and opens the session with exactly what was
/// typed, quotes, `$` and `#` among it. A folder that isn't there is refused in plain words,
/// and nothing is made.
#[test]
fn ctrl_a_n_opens_a_session() {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    // `~/private` is the brain's first folder.
    let private = tb.root().join("private");
    let file = tb.paths().config_file();
    let toml = fs::read_to_string(&file).unwrap().replace(
        "folders = [",
        &format!("folders = [\"{}\", ", private.display()),
    );
    fs::write(&file, toml).unwrap();
    let kite = private.join("kite");
    fs::create_dir_all(&kite).unwrap();
    write_script(&kite, &[start_hook(), FakeStep::Hang]);
    let term = cockpit_up(&tb);
    let r = Rig { term, tb };

    let title = r#"it's "a" $HOME #1"#;
    r.key("n");
    r.on_screen(&["new session · folder: ~/private/"], true);
    r.term.press(b"kite");
    r.term.press(ENTER);
    r.on_screen(&["title:"], true);
    r.term.press(title.as_bytes());
    r.term.press(ENTER);
    let id = wait_for(SHOWS, || {
        let o = run(&r.tb, &["--json", "ls"], &[]);
        let rows: Value = serde_json::from_str(&out(&o)).ok()?;
        rows.as_array()?
            .iter()
            .find(|row| row["title"] == title)
            .map(|row| (row["id"].clone(), row["cwd"].clone()))
    });
    let Some((id, cwd)) = id else {
        panic!(
            "no session titled {title:?}: {}\n{}",
            out(&run(&r.tb, &["ls"], &[])),
            what_happened(&r.tb, &r.term)
        )
    };
    let want = moa_platform::canonical(&kite);
    assert_eq!(cwd.as_str(), Some(want.to_str().unwrap()), "{id}");
    let pane = session_pane(&r.tb, &r.term, id.as_str().unwrap());
    let shown = wait_for(SHOWS, || {
        (r.client_pane().as_deref() == Some(pane.as_str())).then_some(())
    });
    assert!(
        shown.is_some(),
        "the new session's tab {pane} was never shown\n{}",
        what_happened(&r.tb, &r.term)
    );

    r.key("n");
    r.on_screen(&["new session · folder: ~/private/"], true);
    r.term.press(CTRL_U);
    r.term.press(b"~/nope");
    r.on_screen(&["new session · folder: ~/nope"], true);
    r.term.press(ENTER);
    // Read from what the terminal was sent since `Ctrl-a`, not the screen: tmux takes the
    // notice down after 4 s.
    r.says("moa: no folder ~/nope");
    assert!(!r.tb.root().join("nope").exists(), "~/nope was made");
}
