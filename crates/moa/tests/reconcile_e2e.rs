//! Reconcile end to end: the real `moa` binaries, a TempBrain's own terminal server, and
//! fake-claude (named `claude`, so process reads take it for Claude) started by `moactl
//! open`. A `kill-server` stands in for a reboot: the sessions come back asleep, and nothing
//! starts on its own (spec §6.13, U7).

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

#[test]
fn kill_server_brings_sessions_back_asleep() {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    // The daemon logs each request (method and ids only), so a failure's log tail shows
    // when the second `moa` asked for its terminal.
    let first = open(&tb, &[], &[("MOA_LOG", "debug")]);
    came_up(&tb, &first);

    let steps = [
        FakeStep::Hook {
            event: "SessionStart".into(),
            fields: Map::new(),
        },
        FakeStep::Hang,
    ];
    let ids = [
        open_session(&tb, "a", &steps),
        open_session(&tb, "b", &steps),
    ];
    for id in &ids {
        shows(&tb, id, "idle");
    }

    // A reboot, as far as moa can tell.
    tmux(&tb, &["kill-server"]);
    eprintln!("kill-server returned at {}", jiff::Timestamp::now());
    let second = open(&tb, &[], &[]);
    came_up(&tb, &second);

    for id in &ids {
        shows(&tb, id, "asleep");
    }
    let running = tmux(&tb, &["list-panes", "-a", "-F", "#{pane_current_command}"]);
    assert!(
        !running.lines().any(|c| c == "claude" || c == "fake-claude"),
        "an agent started on its own: {running}"
    );
}
