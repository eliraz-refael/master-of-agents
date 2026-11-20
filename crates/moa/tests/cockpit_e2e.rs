//! Slice 5b end to end: the cockpit on a TempBrain's own terminal server, built by the daemon
//! when `moa` first attaches a `Terminal`, with real `moa panel` processes in its panes and
//! fake-claude sessions `moactl open --bg` started. Layouts are switched, sessions focused
//! and panels repaired with the keys a user presses; what each did is read back from tmux
//! (which pane is where, and its process) and from what the terminal shows. Every wait is
//! for what tmux shows or moa says, never for a fixed time.

mod common;

use std::fs;
use std::process::Command;

use moa_core::layout::{Axis, Leaf, Node, parse, presets};
use moa_platform::pid_alive;
use moa_testkit::{FakeStep, TempBrain, Terminal, wait_for, write_script};
use serde_json::{Map, Value};

use common::screen::{Cell, Fg, Screen};
use common::*;

/// The terminal's size, as `cockpit_up` opens it.
const COLS: usize = 120;
const ROWS: usize = 30;
/// A framed panel's frame colour, focused and idle (the spike's `FRAME_ACTIVE` and
/// `FRAME_IDLE`).
const LIT: &str = "#88c0d0";
const DIM: &str = "#4c566a";
/// What a stopped panel's pane says (L7).
const STOPPED: &str = "panel stopped · Ctrl-a r brings it back";
/// What Ctrl-c says in a panel moa placed (L14).
const MANAGED: &str = "this panel is part of moa's layout; Ctrl-a r repairs the layout";

/// A root with the fake account, and `moa` on a terminal with the cockpit built.
struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
}

fn rig() -> Rig {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    let term = cockpit_up(&tb);
    Rig { term, tb }
}

/// A pane's place: left, top, width, height.
type Rect = (u32, u32, u32, u32);

fn rect(p: &PaneRow) -> Rect {
    (p.left, p.top, p.width, p.height)
}

/// Whether pane `p` plays `leaf`: a panel by its view, the spot by an agent or the empty
/// slot.
fn plays(p: &PaneRow, leaf: Leaf) -> bool {
    match leaf {
        Leaf::Agent => p.tag == "agent" || p.tag == "empty-slot",
        Leaf::Panel(view) => p.tag == view.word(),
    }
}

/// Where `node`'s cells are among `panes`: each leaf's pane, each split's children side by
/// side (`h`) or stacked (`v`) in the layout's order, each child's cell against the next with
/// one border between them. The whole cell, or why it isn't one.
fn place(node: &Node, panes: &[PaneRow]) -> Result<Rect, String> {
    match node {
        Node::Leaf { leaf, .. } => {
            let mine: Vec<&PaneRow> = panes.iter().filter(|p| plays(p, *leaf)).collect();
            match mine[..] {
                [p] => Ok(rect(p)),
                _ => Err(format!("{} is in {} panes", leaf.word(), mine.len())),
            }
        }
        Node::Split { axis, children, .. } => {
            let cells = children
                .iter()
                .map(|c| place(c, panes))
                .collect::<Result<Vec<Rect>, String>>()?;
            for pair in cells.windows(2) {
                let [(l1, t1, w1, h1), (l2, t2, w2, h2)] = pair else {
                    unreachable!()
                };
                let next = match axis {
                    Axis::H => *t2 == *t1 && *h2 == *h1 && *l2 == l1 + w1 + 1,
                    Axis::V => *l2 == *l1 && *w2 == *w1 && *t2 == t1 + h1 + 1,
                };
                if !next {
                    return Err(format!(
                        "{axis:?} split: {:?} isn't followed by {:?}",
                        pair[0], pair[1]
                    ));
                }
            }
            let (left, top, ..) = cells[0];
            let (l, t, w, h) = cells[cells.len() - 1];
            Ok((left, top, l + w - left, t + h - top))
        }
    }
}

/// The preset `name`, parsed.
fn preset(name: &str) -> Node {
    parse(&presets().entries[name]).unwrap()
}

/// Whether the cockpit holds `layout` and nothing else, filling its tab's width.
fn holds(tb: &TempBrain, layout: &str) -> Result<(), String> {
    let node = preset(layout);
    let panes = cockpit_panes(tb);
    let count = moa_core::layout::leaves(&node).len();
    if panes.len() != count {
        return Err(format!("{} panes for {count} leaves", panes.len()));
    }
    let (left, _, width, _) = place(&node, &panes)?;
    let window: u32 = tmux(
        tb,
        &["display-message", "-p", "-t", "=moa:moa", "#{window_width}"],
    )
    .parse()
    .unwrap();
    if (left, width) != (0, window) {
        return Err(format!(
            "the layout spans {left}+{width} of {window} columns"
        ));
    }
    Ok(())
}

impl Rig {
    /// `moactl args…`, which must succeed; its stdout.
    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let o = run_exe(&self.tb, &moactl(&self.tb), args, &[("MOA_BRAIN", B)]);
        assert!(
            o.status.success(),
            "moactl {args:?}: {}{}\n{}",
            out(&o),
            err(&o),
            what_happened(&self.tb, &self.term)
        );
        out(&o)
    }

    /// `moactl open --bg <root>/work/<name> --title <title>`, with fake-claude starting and
    /// hanging there; the session's id.
    #[track_caller]
    fn session(&self, name: &str, title: &str) -> String {
        let dir = self.tb.root().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        let start = FakeStep::Hook {
            event: "SessionStart".into(),
            fields: Map::from_iter([("source".to_string(), Value::String("startup".into()))]),
        };
        write_script(&dir, &[start, FakeStep::Hang]);
        let path = dir.display().to_string();
        let opened = self.ok(&["--json", "open", "--bg", &path, "--title", title]);
        let data: Value = serde_json::from_str(&opened).unwrap();
        data["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{data}"))
            .to_string()
    }

    /// The pane running session `id`.
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

    /// The cockpit's pane tagged `tag`.
    #[track_caller]
    fn cockpit_pane(&self, tag: &str) -> PaneRow {
        cockpit_panes(&self.tb)
            .into_iter()
            .find(|p| p.tag == tag)
            .unwrap_or_else(|| panic!("no {tag} in the cockpit: {:#?}", cockpit_panes(&self.tb)))
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

    /// Waits until the cockpit holds `layout`.
    #[track_caller]
    fn layout_shows(&self, layout: &str) {
        let seen = wait_for(SHOWS, || holds(&self.tb, layout).ok());
        assert!(
            seen.is_some(),
            "the cockpit never held {layout}: {:?}\n{:#?}\n{}",
            holds(&self.tb, layout),
            cockpit_panes(&self.tb),
            what_happened(&self.tb, &self.term)
        );
    }

    /// Ctrl-a `key`, which applies `layout`: once it says so, the cockpit holds it.
    #[track_caller]
    fn apply(&self, key: &str, layout: &str) {
        self.key(key);
        self.says(&format!("layout {layout} applied"));
        self.layout_shows(layout);
    }

    /// The daemon's launches and its panel starts.
    fn counts(&self) -> (u64, u64) {
        let s = status_json(&self.tb);
        let n = |k: &str| s[k].as_u64().unwrap_or_else(|| panic!("no {k}: {s}"));
        (n("launches"), n("panel_starts"))
    }

    /// Every panel's pane and its process, in the cockpit and parked.
    fn panel_pids(&self) -> Vec<(String, u32)> {
        let mut all = panes_of(&self.tb, "=moa");
        all.extend(panes_of(&self.tb, "=_moa"));
        let mut pids: Vec<(String, u32)> = all
            .into_iter()
            .filter(|p| !p.tag.is_empty() && p.tag != "agent")
            .map(|p| (p.id, p.pid))
            .collect();
        pids.sort();
        pids
    }

    /// What the terminal shows now.
    fn screen(&self) -> Screen {
        Screen::of(&self.term, COLS, ROWS)
    }

    /// Waits until `pane` is the cockpit's active pane.
    #[track_caller]
    fn active_becomes(&self, pane: &str) {
        let seen = wait_for(SHOWS, || {
            cockpit_panes(&self.tb)
                .iter()
                .any(|p| p.id == pane && p.active)
                .then_some(())
        });
        assert!(
            seen.is_some(),
            "{pane} never became active: {:#?}\n{}",
            cockpit_panes(&self.tb),
            what_happened(&self.tb, &self.term)
        );
    }
}

/// The terminal cell of `pane`'s frame's top left corner, 1-based as a mouse report gives
/// it, placed as [`cell_of`] places a row.
fn corner_of(p: &PaneRow) -> (u32, u32) {
    (p.left + 1, p.top + 1)
}

#[test]
fn the_first_attach_builds_the_cockpit() {
    let r = rig();
    r.layout_shows("cockpit");
    let [tree, console, plan] = &cockpit_panes(&r.tb)[..] else {
        panic!("{:#?}", cockpit_panes(&r.tb));
    };
    // Side by side, filling the width: the tree about 26%, the plan about 30%, the console
    // the rest, with a border after each of the first two.
    let cols = COLS as u32;
    let near = |got: u32, share: u32| got.abs_diff(cols * share / 100) <= 2;
    assert!(near(tree.width, 26), "the tree is {} wide", tree.width);
    assert!(near(plan.width, 30), "the plan is {} wide", plan.width);
    assert_eq!(console.width, cols - tree.width - plan.width - 2);

    r.shows(&tree.id, &["▤ sessions"]);
    r.shows(&console.id, &["◆ moa"]);
    r.shows(&plan.id, &["◷ big picture"]);

    // The login shell has a tab of its own, and still runs.
    let shell: Vec<PaneRow> = panes_of(&r.tb, "=moa")
        .into_iter()
        .filter(|p| p.window == "⌨ shell")
        .collect();
    let [shell] = &shell[..] else {
        panic!("{:#?}", panes_of(&r.tb, "=moa"));
    };
    assert!(!shell.dead && pid_alive(shell.pid), "{shell:?}");

    assert_eq!(r.counts().1, 3, "{}", status_json(&r.tb));

    // The terminal is on the cockpit, and the status line says so.
    assert_eq!(
        client_field(&r.tb, &r.term, "#{window_name}").as_deref(),
        Some("moa")
    );
    let pill = wait_for(SHOWS, || {
        let s = r.screen();
        (s.row(ROWS - 1) + &s.row(ROWS))
            .contains("▦ cockpit")
            .then_some(())
    });
    assert!(pill.is_some(), "no ▦ cockpit pill:\n{}", r.screen().text());
}

#[test]
fn a_layout_switch_moves_panes_and_starts_nothing() {
    let r = rig();
    let presets = [
        ("1", "cockpit"),
        ("2", "focus"),
        ("3", "classic"),
        ("4", "triage"),
    ];
    // Every panel any preset needs is started once here.
    for (key, layout) in presets {
        r.apply(key, layout);
    }
    let pids = r.panel_pids();
    let counts = r.counts();

    // Back to back: each may replace the one still waiting.
    for key in ["2", "4", "1", "3"] {
        r.key(key);
    }
    r.layout_shows("classic");
    assert_eq!(r.counts(), counts, "a layout switch started something");
    assert_eq!(r.panel_pids(), pids, "a panel was restarted");

    // Each preset in turn puts every pane in its place.
    for (key, layout) in presets {
        r.apply(key, layout);
    }
    assert_eq!(r.counts(), counts, "a layout switch started something");
    assert_eq!(r.panel_pids(), pids, "a panel was restarted");
}

#[test]
fn focus_swaps_and_the_old_agent_goes_home() {
    let r = rig();
    r.apply("2", "focus");
    // The spot starts as the empty slot.
    let slot = r.cockpit_pane("empty-slot");
    r.shows(&slot.id, &["no agent here yet"]);
    let lantern = r.session("lantern", "Lantern");
    let kite = r.session("kite", "Kite");
    let lantern_pane = r.session_pane(&lantern);
    let kite_pane = r.session_pane(&kite);
    let pid_of = |pane: &str| tmux(&r.tb, &["display-message", "-p", "-t", pane, "#{pane_pid}"]);
    let pids = (pid_of(&lantern_pane), pid_of(&kite_pane));
    let tree = r.cockpit_pane("tree");
    r.shows(&tree.id, &["Lantern", "Kite"]);

    let in_spot = |pane: &str| {
        let seen = wait_for(SHOWS, || {
            let spot = cockpit_panes(&r.tb)
                .into_iter()
                .find(|p| p.tag == "agent")?;
            (spot.id == pane).then_some(())
        });
        assert!(
            seen.is_some(),
            "{pane} never came into the spot: {:#?}\n{}",
            cockpit_panes(&r.tb),
            what_happened(&r.tb, &r.term)
        );
    };
    select_row(&r.tb, &tree.id, "Lantern");
    tmux(&r.tb, &["send-keys", "-t", &tree.id, "Enter"]);
    in_spot(&lantern_pane);
    r.layout_shows("focus");

    select_row(&r.tb, &tree.id, "Kite");
    tmux(&r.tb, &["send-keys", "-t", &tree.id, "Enter"]);
    in_spot(&kite_pane);
    r.layout_shows("focus");
    // Lantern is back in a tab of its own, named by its title.
    let home: Vec<PaneRow> = panes_of(&r.tb, "=moa")
        .into_iter()
        .filter(|p| p.window == "Lantern")
        .collect();
    assert_eq!(
        home.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        [lantern_pane.as_str()],
        "{:#?}",
        panes_of(&r.tb, "=moa")
    );
    assert_eq!((pid_of(&lantern_pane), pid_of(&kite_pane)), pids);
}

#[test]
fn repair_brings_a_killed_panel_back() {
    let r = rig();
    let plan = r.cockpit_pane("plan");
    r.shows(&plan.id, &["◷ big picture"]);

    let killed = Command::new("kill")
        .args(["-9", &plan.pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    // tmux writes the stopped line under what the panel last drew, cut to the pane's width.
    let shown: String = STOPPED.chars().take(plan.width as usize).collect();
    r.shows(&plan.id, &[&shown]);
    let format = tmux(
        &r.tb,
        &[
            "show-options",
            "-p",
            "-v",
            "-t",
            &plan.id,
            "remain-on-exit-format",
        ],
    );
    assert_eq!(format, STOPPED);
    let line = format!("panel plan in {} stopped", plan.id);
    let logged = wait_for(SHOWS, || {
        fs::read_to_string(r.tb.paths().log(B))
            .is_ok_and(|log| log.contains(&line))
            .then_some(())
    });
    assert!(
        logged.is_some(),
        "{line:?} not logged\n{}",
        r.tb.log_tail(B, 60)
    );

    // A layout switch keeps it, stopped, in the plan's place.
    r.apply("2", "focus");
    let moved = r.cockpit_pane("plan");
    assert_eq!(moved.id, plan.id);
    assert!(moved.dead, "{moved:?}");
    let starts = r.counts().1;

    r.key("r");
    r.says("repaired: 1 panel restarted");
    let back = wait_for(SHOWS, || {
        let p = r.cockpit_pane("plan");
        (!p.dead && screen_of(&r.tb, &p.id).contains("◷ big picture")).then_some(p)
    });
    let back = back.unwrap_or_else(|| panic!("{}", tab_shows(&r.tb, &plan.id)));
    assert_eq!(
        (back.id.as_str(), rect(&back)),
        (plan.id.as_str(), rect(&moved))
    );
    r.layout_shows("focus");
    assert_eq!(r.counts().1, starts + 1, "{}", status_json(&r.tb));
}

#[test]
fn frame_colours_follow_focus() {
    let r = rig();
    let tree = r.cockpit_pane("tree");
    let plan = r.cockpit_pane("plan");
    for p in [&tree, &plan] {
        let option = |name: &str| tmux(&r.tb, &["show-options", "-p", "-v", "-t", &p.id, name]);
        assert_eq!(option("window-style"), format!("fg={DIM}"), "{p:?}");
        assert_eq!(option("window-active-style"), format!("fg={LIT}"), "{p:?}");
    }
    r.shows(&tree.id, &["▤ sessions"]);
    r.shows(&plan.id, &["◷ big picture"]);

    // The focused panel's frame is lit on the terminal, and the other one's dim.
    for (on, off) in [(&tree, &plan), (&plan, &tree)] {
        tmux(&r.tb, &["select-pane", "-t", &on.id]);
        let want = [
            (
                corner_of(on),
                Cell {
                    ch: '╭',
                    fg: Fg::hex(LIT),
                },
            ),
            (
                corner_of(off),
                Cell {
                    ch: '╭',
                    fg: Fg::hex(DIM),
                },
            ),
        ];
        let drawn = wait_for(SHOWS, || {
            let s = r.screen();
            want.iter()
                .all(|(at, cell)| s.cell(*at) == *cell)
                .then_some(())
        });
        assert!(
            drawn.is_some(),
            "with {} focused, want {want:?}, got {:?}\n{}",
            on.tag,
            want.map(|(at, _)| r.screen().cell(at)),
            r.screen().text()
        );
    }
}

#[test]
fn moas_view_wins_after_moa_restarts() {
    let r = rig();
    let tree = r.cockpit_pane("tree");
    r.shows(&tree.id, &["▤ sessions"]);

    shutdown(&r.tb);
    // The tree is too narrow for the whole line.
    r.shows(&tree.id, &["moa isn't running"]);
    // moa's view for the pane changes while moa is down: the panel still remembers `tree`.
    tmux(
        &r.tb,
        &["set-option", "-p", "-t", &tree.id, "@moa_view", "status"],
    );

    r.ok(&["ls"]);
    r.shows(&tree.id, &["◉ status"]);
    assert_eq!(
        r.cockpit_pane("status").pid,
        tree.pid,
        "the panel was restarted"
    );

    // Zoomed, so the whole line fits.
    tmux(&r.tb, &["resize-pane", "-Z", "-t", &tree.id]);
    let wide = wait_for(SHOWS, || {
        let top = screen_of(&r.tb, &tree.id).lines().next()?.chars().count();
        (top == COLS).then_some(())
    });
    assert!(wide.is_some(), "{}", tab_shows(&r.tb, &tree.id));
    tmux(&r.tb, &["send-keys", "-t", &tree.id, "C-c"]);
    r.shows(&tree.id, &[MANAGED]);
    let now = r.cockpit_pane("status");
    assert!(
        !now.dead && now.pid == tree.pid && pid_alive(tree.pid),
        "{now:?}"
    );
}

#[test]
fn goto_keys_select_their_panels() {
    let r = rig();
    let plan = r.cockpit_pane("plan");
    tmux(&r.tb, &["select-pane", "-t", &r.cockpit_pane("tree").id]);
    r.active_becomes(&r.cockpit_pane("tree").id);
    r.key("p");
    r.active_becomes(&plan.id);

    r.apply("2", "focus");
    let spot = r.cockpit_pane("empty-slot");
    assert!(!spot.active, "the spot was active before Ctrl-a g");
    r.key("g");
    r.active_becomes(&spot.id);
}
