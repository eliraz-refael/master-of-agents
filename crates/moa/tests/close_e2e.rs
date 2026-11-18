//! `close` and `term` end to end: the real `moa` binaries, a TempBrain's own terminal
//! server, and fake-claude (named `claude`) started by `moactl open`.

mod common;

use std::fs;

use moa_testkit::{FakeStep, TempBrain, fake_claude_named_claude, wait_for, write_script};
use serde_json::{Map, Value};

use common::*;

/// `moactl open <root>/work/<name>`, fake-claude running `steps` there; the session's id.
fn open_session(tb: &TempBrain, name: &str, steps: &[FakeStep]) -> String {
    let dir = tb.root().join("work").join(name);
    fs::create_dir_all(&dir).unwrap();
    write_script(&dir, steps);
    let o = run_exe(
        tb,
        &moactl(tb),
        &["--json", "open", &dir.display().to_string()],
        &[("MOA_BRAIN", B)],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let data: Value = serde_json::from_str(&out(&o)).unwrap();
    data["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{data}"))
        .to_string()
}

/// Waits until session `id` shows `want`.
fn shows(tb: &TempBrain, id: &str, want: &str) {
    let seen = wait_for(SHOWS, || {
        (ls_display(tb, id).as_deref() == Some(want)).then_some(())
    });
    assert!(seen.is_some(), "never {want}: {:?}", ls_display(tb, id));
}

/// Every pane of the server: its id, `@moa_session`, `@moa_term` and window name.
fn panes(tb: &TempBrain) -> Vec<[String; 4]> {
    let list = tmux(
        tb,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{@moa_session}\t#{@moa_term}\t#{window_name}",
        ],
    );
    list.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            let [a, b, c, d] = f[..] else {
                return None;
            };
            Some([a, b, c, d].map(str::to_string))
        })
        .collect()
}

#[test]
fn close_and_term_end_to_end() {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);

    let start = FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::new(),
    };
    let id = open_session(&tb, "a", &[start, FakeStep::Hang]);
    shows(&tb, &id, "idle");
    let tagged = wait_for(SHOWS, || {
        panes(&tb)
            .into_iter()
            .find(|p| p[1] == id)
            .map(|p| p[0].clone())
    });
    let pane = tagged.unwrap_or_else(|| panic!("no tab tagged {id}: {:?}", panes(&tb)));

    let o = run_exe(&tb, &moactl(&tb), &["close", &id], &[("MOA_BRAIN", B)]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert_eq!(out(&o), "closed\n");
    shows(&tb, &id, "ended");
    let gone = wait_for(SHOWS, || {
        (!panes(&tb).iter().any(|p| p[0] == pane)).then_some(())
    });
    assert!(gone.is_some(), "{pane} is still there: {:?}", panes(&tb));
    // That the kill's own end (an exit, `SessionEnd(other)`) doesn't put it to sleep is the
    // core's `the_agents_end_after_close_is_history_only`: nothing here sees when moa has
    // heard of it.

    let work = tb.root().join("work");
    let o = run_exe(
        &tb,
        &moactl(&tb),
        &["term", &work.display().to_string()],
        &[("MOA_BRAIN", B)],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).ends_with("(tab ⌨ work)\n"), "{}", out(&o));
    let shell = wait_for(SHOWS, || {
        panes(&tb)
            .into_iter()
            .find(|p| p[3] == "⌨ work" && p[2] == "1")
    });
    let shell = shell.unwrap_or_else(|| panic!("no terminal tab: {:?}", panes(&tb)));
    assert_eq!(shell[1], "", "a terminal is no session's");

    // `ls` lists sessions only: the one it had, and not the terminal.
    let o = run(&tb, &["--json", "ls", "--all"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let rows: Value = serde_json::from_str(&out(&o)).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["id"], id.as_str());
}
