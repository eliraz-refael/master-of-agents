//! Slice 5d end to end: every terminal is recorded, `moa term ls`, and the terminals view
//! (`Ctrl-a T`) that jumps, closes, names and marks them, on a TempBrain's own terminal
//! server, with a real `moa` daemon and panels, fake-claude sessions, and `moa` on a
//! `Terminal`. Keys are typed on the terminal as a user types them. The view is a popup, so
//! what it draws is read from the terminal's screen; what moa did is read back from tmux,
//! `moa term ls` and the activity table. Every wait is for what tmux shows or moa says,
//! never for a fixed time.

mod common;

use std::path::PathBuf;

use moa_testkit::wait_for;
use serde_json::Value;

use common::*;

/// The title tmux draws on the terminals view's popup's border.
const TERMS_TITLE: &str = "⌨ terminals";
/// Backspace, typed on a terminal.
const BACKSPACE: &[u8] = b"\x7f";

/// A folder `name` under the root's `work`, made, as moa lists it.
fn folder(r: &Rig, name: &str) -> PathBuf {
    let dir = r.tb.root().join("work").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    moa_platform::canonical(&dir)
}

/// The open terminals, as `moa --json term ls` tells them.
fn terms(r: &Rig) -> Vec<Value> {
    let o = run(&r.tb, &["--json", "term", "ls"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    serde_json::from_str::<Value>(&out(&o))
        .unwrap()
        .as_array()
        .unwrap_or_else(|| panic!("{}", out(&o)))
        .clone()
}

/// The open terminal in `dir` that isn't one of `known` (panes), once it is listed.
#[track_caller]
fn term_in(r: &Rig, dir: &std::path::Path, known: &[String]) -> Value {
    let found = wait_for(SHOWS, || {
        terms(r).into_iter().find(|t| {
            t["folder"] == dir.to_str().unwrap() && !known.contains(&t["pane"].to_string())
        })
    });
    found.unwrap_or_else(|| {
        panic!(
            "no terminal in {}: {:#?}\n{}",
            dir.display(),
            terms(r),
            what_happened(&r.tb, &r.term)
        )
    })
}

/// `moa term <dir>`: a terminal in a tab of its own, which the terminal comes to show; its
/// pane.
#[track_caller]
fn open_terminal(r: &Rig, dir: &std::path::Path) -> String {
    let o = run(&r.tb, &["term", &dir.display().to_string()], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let t = term_in(r, dir, &[]);
    t["pane"].as_str().unwrap().to_string()
}

/// Waits until the terminal shows `pane`.
#[track_caller]
fn shown(r: &Rig, pane: &str) {
    let seen = wait_for(SHOWS, || {
        (r.client_pane().as_deref() == Some(pane)).then_some(())
    });
    assert!(
        seen.is_some(),
        "the terminal never showed {pane}, but {:?}\n{}",
        r.client_pane(),
        what_happened(&r.tb, &r.term)
    );
}

/// Brings `pane`'s tab to the terminal.
#[track_caller]
fn go_to(r: &Rig, pane: &str) {
    tmux(&r.tb, &["select-window", "-t", pane]);
    tmux(&r.tb, &["select-pane", "-t", pane]);
    shown(r, pane);
}

/// Every pane in moa's session.
fn pane_ids(r: &Rig) -> Vec<String> {
    panes_of(&r.tb, "=moa").into_iter().map(|p| p.id).collect()
}

/// Waits until `pane` is gone from moa's session.
#[track_caller]
fn pane_goes(r: &Rig, pane: &str) {
    let gone = wait_for(SHOWS, || {
        (!pane_ids(r).contains(&pane.to_string())).then_some(())
    });
    assert!(
        gone.is_some(),
        "{pane} is still there: {:#?}\n{}",
        panes_of(&r.tb, "=moa"),
        what_happened(&r.tb, &r.term)
    );
}

/// `Ctrl-a T`: the terminals view's popup, drawn with a row for each of `words`, and
/// connected to moa. The row words, in the order the view lists them.
#[track_caller]
fn open_view(r: &Rig, words: &[&str]) -> Vec<String> {
    r.key("T");
    let up = wait_for(SHOWS, || (popups(&r.tb, "terms") == 1).then_some(()));
    assert!(
        up.is_some(),
        "the terminals view never connected: {:?}\n{}",
        panels(&r.tb),
        what_happened(&r.tb, &r.term)
    );
    let drawn = wait_for(SHOWS, || {
        let rows = view_rows(r)?;
        words
            .iter()
            .all(|w| rows.iter().any(|row| row.contains(w)))
            .then_some(rows)
    });
    drawn.unwrap_or_else(|| {
        panic!(
            "the view never listed {words:?}; the terminal shows:\n{}",
            screen::Screen::of(&r.term).text()
        )
    })
}

/// The rows the open view lists: every line inside it that starts with `⌨`.
fn view_rows(r: &Rig) -> Option<Vec<String>> {
    let inside = popup_text(&r.term, TERMS_TITLE)?;
    Some(
        inside
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| l.starts_with('⌨'))
            .collect(),
    )
}

/// Moves the view's selection, which starts on its first row, to the row naming `word`.
#[track_caller]
fn select(r: &Rig, word: &str) {
    let rows = view_rows(r).unwrap_or_default();
    let at = rows
        .iter()
        .position(|row| row.contains(word))
        .unwrap_or_else(|| panic!("no row names {word:?}: {rows:?}"));
    for _ in 0..at {
        r.term.press(b"j");
    }
}

/// Waits until the view shows `text` inside its border.
#[track_caller]
fn view_says(r: &Rig, text: &str) {
    popup_shows(&r.term, TERMS_TITLE, &[text]);
}

/// The view is gone: its panel left moa, and tmux closed it.
#[track_caller]
fn view_goes(r: &Rig) {
    let gone = wait_for(SHOWS, || (popups(&r.tb, "terms") == 0).then_some(()));
    assert!(
        gone.is_some(),
        "the terminals view never closed\n{}",
        what_happened(&r.tb, &r.term)
    );
    popup_closes(&r.tb, &r.term, TERMS_TITLE);
}

/// `moa term`, `Ctrl-a $`, `Ctrl-a |` and a plain `tmux new-window` each make a terminal
/// moa records, with the right owner: all four are yours, and the split names the agent
/// it's beside. The one more is the cockpit's own shell, which moa found on start.
#[test]
fn every_kind_of_terminal_is_recorded() {
    let r = rig();
    let lantern = r.session("lantern", "Lantern");
    let agent = session_pane(&r.tb, &r.term, &lantern);
    let home = folder(&r, "lantern");

    // `Ctrl-a $` beside an agent: a tab of its own, in its folder.
    go_to(&r, &agent);
    r.key("$");
    let here = term_in(&r, &home, &[]);
    // `Ctrl-a |`: a split of the agent's tab.
    go_to(&r, &agent);
    r.key("|");
    let split = term_in(&r, &home, &[here["pane"].to_string()]);
    // `moa term`, and a tab tmux made by itself.
    let alpha = folder(&r, "alpha");
    open_terminal(&r, &alpha);
    let beta = folder(&r, "beta");
    tmux(
        &r.tb,
        &[
            "new-window",
            "-d",
            "-t",
            "=moa:",
            "-c",
            &beta.display().to_string(),
        ],
    );
    term_in(&r, &beta, &[]);

    let all = terms(&r);
    let by = |who: &str| all.iter().filter(|t| t["opened_by"] == who).count();
    assert_eq!((by("found"), by("you")), (1, 4), "{all:#?}");
    assert_eq!(split["name"], "shell", "{split}");
    assert_eq!(split["session"], "Lantern", "{split}");
    let owned: Vec<&Value> = all.iter().filter(|t| !t["session"].is_null()).collect();
    assert_eq!(
        owned.len(),
        1,
        "only the split belongs to a session: {all:#?}"
    );
    assert_eq!(owned[0]["pane"], split["pane"], "{all:#?}");
}

/// `Ctrl-a T` lists every terminal by name, and Enter on a row brings that terminal's tab,
/// and closes the view.
#[test]
fn the_view_lists_names_and_jumps() {
    let r = rig();
    let alpha = open_terminal(&r, &folder(&r, "alpha"));
    open_terminal(&r, &folder(&r, "beta"));
    let gamma = open_terminal(&r, &folder(&r, "gamma"));
    // Away from the one jumped to, and not on it.
    r.key("m");
    let away = wait_for(SHOWS, || {
        (client_field(&r.tb, &r.term, "#{window_name}").as_deref() == Some("moa")).then_some(())
    });
    assert!(away.is_some(), "{}", what_happened(&r.tb, &r.term));
    assert_ne!(r.client_pane().as_deref(), Some(alpha.as_str()));
    assert_ne!(r.client_pane().as_deref(), Some(gamma.as_str()));

    open_view(&r, &["alpha", "beta", "gamma"]);
    select(&r, "alpha");
    r.term.press(ENTER);
    view_goes(&r);
    shown(&r, &alpha);
}

/// `x`, then `y`, closes a terminal that ignores Ctrl-c: moa interrupts it, then closes it
/// 3 seconds later. Nothing else closes.
#[test]
fn the_view_closes_a_running_terminal() {
    let r = rig();
    let alpha = open_terminal(&r, &folder(&r, "alpha"));
    let beta = open_terminal(&r, &folder(&r, "beta"));
    let shell = tmux(
        &r.tb,
        &[
            "display-message",
            "-p",
            "-t",
            &alpha,
            "#{pane_current_command}",
        ],
    );
    tmux(
        &r.tb,
        &[
            "send-keys",
            "-t",
            &alpha,
            "trap '' INT; sleep 1000",
            "Enter",
        ],
    );
    let running = wait_for(SHOWS, || {
        let now = tmux(
            &r.tb,
            &[
                "display-message",
                "-p",
                "-t",
                &alpha,
                "#{pane_current_command}",
            ],
        );
        (now != shell).then_some(())
    });
    assert!(running.is_some(), "{}", tab_shows(&r.tb, &alpha));
    r.key("m");

    open_view(&r, &["alpha", "beta"]);
    select(&r, "alpha");
    r.term.press(b"x");
    view_says(&r, "y/N");
    r.term.press(b"y");
    pane_goes(&r, &alpha);
    assert!(
        pane_ids(&r).contains(&beta),
        "beta went too: {:?}",
        pane_ids(&r)
    );
}

/// `r` in the view asks for a new name, and the terminal's tab takes it.
#[test]
fn naming_from_the_view_renames_the_tab() {
    let r = rig();
    let alpha = open_terminal(&r, &folder(&r, "alpha"));
    r.key("m");

    open_view(&r, &["alpha"]);
    select(&r, "alpha");
    r.term.press(b"r");
    view_says(&r, "name:");
    // Its own name is the prefill.
    for _ in 0.."alpha".len() {
        r.term.press(BACKSPACE);
    }
    r.term.press(b"Beacon");
    r.term.press(ENTER);
    let renamed = wait_for(SHOWS, || {
        let tab = tmux(
            &r.tb,
            &["display-message", "-p", "-t", &alpha, "#{window_name}"],
        );
        (tab == "⌨ Beacon").then_some(())
    });
    assert!(
        renamed.is_some(),
        "the tab was never renamed: {}\n{}",
        tab_shows(&r.tb, &alpha),
        screen::Screen::of(&r.term).text()
    );
    let named = terms(&r)
        .into_iter()
        .any(|t| t["pane"] == alpha.as_str() && t["name"] == "Beacon");
    assert!(named, "{:#?}", terms(&r));
}

/// A timeout closes its terminal when it runs out, and says so in the activity.
#[test]
fn a_timeout_closes_a_terminal() {
    let r = rig();
    let alpha = open_terminal(&r, &folder(&r, "alpha"));
    let id = terms(&r)
        .into_iter()
        .find(|t| t["pane"] == alpha.as_str())
        .unwrap()["id"]
        .to_string();
    r.key("m");

    let o = run(&r.tb, &["term", "timeout", &id, "2s"], &[("MOA_BRAIN", B)]);
    let text = out(&o);
    assert!(text.starts_with("closes in "), "{text}{}", err(&o));
    pane_goes(&r, &alpha);
    let said = wait_for(SHOWS, || {
        activity(&r.tb)
            .lines()
            .any(|l| l == "moa closed alpha: its timeout ran out")
            .then_some(())
    });
    assert!(said.is_some(), "{}", activity(&r.tb));
}

/// With `terminal_idle_hours` a couple of seconds, a terminal nobody is looking at is marked
/// `idle` in the open view, which no poll draws: the view works out when the mark is due.
#[test]
fn idle_terminals_are_marked() {
    let r = rig_with(TERM_SIZE, "\n[ui]\nterminal_idle_hours = 0.0005\n");
    open_terminal(&r, &folder(&r, "alpha"));
    r.key("m");

    open_view(&r, &["alpha"]);
    let marked = wait_for(SHOWS, || {
        let rows = view_rows(&r)?;
        rows.iter()
            .any(|row| row.contains("alpha") && row.contains("idle"))
            .then_some(())
    });
    assert!(
        marked.is_some(),
        "alpha was never marked idle; the terminal shows:\n{}",
        screen::Screen::of(&r.term).text()
    );
}

/// A split of an agent's tab is a terminal the picker finds, beside the agent.
#[test]
fn the_split_shows_in_the_picker() {
    let r = rig();
    let lantern = r.session("lantern", "Lantern");
    let agent = session_pane(&r.tb, &r.term, &lantern);
    go_to(&r, &agent);
    r.key("|");
    term_in(&r, &folder(&r, "lantern"), &[]);

    r.open_picker_popup(&["Lantern", "⌨ shell", "lantern"]);
    let inside = popup_text(&r.term, PICKER_TITLE).unwrap_or_default();
    let row = inside
        .lines()
        .find(|l| l.contains("⌨ shell") && l.contains("lantern"))
        .unwrap_or_else(|| panic!("no row for the split:\n{inside}"));
    assert!(row.contains("terminal"), "{row}");
}
