//! Sleep end to end: the real `moa` binaries, a TempBrain's own terminal server, and
//! fake-claude (named `claude`) started by `moactl open --bg`. An idle session off screen goes
//! to sleep on its own once `sleep_after_hours` has passed.

mod common;

use std::fs;

use moa_testkit::{FakeStep, TempBrain, fake_claude_named_claude, wait_for, write_script};
use serde_json::{Map, Value};

use common::*;

/// Every pane of the server: its id and `@moa_session`.
fn panes(tb: &TempBrain) -> Vec<(String, String)> {
    tmux(
        tb,
        &["list-panes", "-a", "-F", "#{pane_id}\t#{@moa_session}"],
    )
    .lines()
    .filter_map(|l| l.split_once('\t'))
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

/// The newest activity rows' text.
fn activity(tb: &TempBrain) -> String {
    let o = run(
        tb,
        &["sql", "select text from activity order by id desc limit 20"],
        &[],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    out(&o)
}

#[test]
fn an_idle_session_goes_to_sleep_on_its_own() {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    // About a second.
    let file = tb.paths().config_file();
    let toml = fs::read_to_string(&file).unwrap().replace(
        "[brains.default]\n",
        "[brains.default]\nsleep_after_hours = 0.0003\n",
    );
    fs::write(&file, toml).unwrap();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);

    // In the background, so no user is looking at its tab.
    let dir = tb.root().join("work").join("a");
    fs::create_dir_all(&dir).unwrap();
    let start = FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::new(),
    };
    write_script(&dir, &[start, FakeStep::Hang]);
    let o = run_exe(
        &tb,
        &moactl(&tb),
        &["--json", "open", "--bg", &dir.display().to_string()],
        &[("MOA_BRAIN", B)],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let data: Value = serde_json::from_str(&out(&o)).unwrap();
    let id = data["id"].as_str().unwrap().to_string();

    let asleep = wait_for(SHOWS, || {
        (ls_display(&tb, &id).as_deref() == Some("asleep")).then_some(())
    });
    assert!(
        asleep.is_some(),
        "never asleep: {:?}\n{}",
        ls_display(&tb, &id),
        what_happened(&tb, &term)
    );
    let gone = wait_for(SHOWS, || {
        (!panes(&tb).iter().any(|(_, s)| *s == id)).then_some(())
    });
    assert!(gone.is_some(), "its tab is still there: {:?}", panes(&tb));
    let rows = activity(&tb);
    assert!(
        rows.lines()
            .any(|l| l.starts_with("💤 put 1 idle session to sleep: ")
                && l.ends_with(" · resume from the tree")),
        "{rows}"
    );
}
