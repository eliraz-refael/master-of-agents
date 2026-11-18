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
    assert_eq!(agents(&tb), 0, "an agent started on its own");
}

/// How many tabs run fake-claude.
fn agents(tb: &TempBrain) -> usize {
    let running = tmux(tb, &["list-panes", "-a", "-F", "#{pane_current_command}"]);
    running
        .lines()
        .filter(|c| *c == "claude" || *c == "fake-claude")
        .count()
}

/// C11 end to end: a session put to sleep by a `kill-server` resumes its conversation, under
/// the account it was recorded in, in one new tab. Its transcript is the one fake-claude
/// wrote on the first run (R34).
#[test]
fn a_session_asleep_after_kill_server_resumes() {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    let first = open(&tb, &[], &[]);
    came_up(&tb, &first);

    let start = |source: &str| FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::from_iter([("source".to_string(), Value::String(source.into()))]),
    };
    let id = open_session(&tb, "a", &[start("startup"), FakeStep::Hang]);
    shows(&tb, &id, "idle");
    let conversation = ls_row(&tb, &id).unwrap()["conversation"].clone();
    let conv = conversation.as_str().unwrap().to_string();
    // The agent has started once it has its transcript: the `kill-server` must not come
    // before.
    let store = tb.root().join(".claude-a/projects");
    let written = wait_for(SHOWS, || {
        let slots = fs::read_dir(&store).ok()?;
        slots
            .flatten()
            .any(|slot| slot.path().join(format!("{conv}.jsonl")).is_file())
            .then_some(())
    });
    assert!(written.is_some(), "fake-claude never wrote its transcript");

    tmux(&tb, &["kill-server"]);
    let second = open(&tb, &[], &[]);
    came_up(&tb, &second);
    shows(&tb, &id, "asleep");

    // fake-claude reads its script from its folder at each launch.
    write_script(
        &tb.root().join("work/a"),
        &[start("resume"), FakeStep::Hang],
    );
    let o = run_exe(&tb, &moactl(&tb), &["resume", &id], &[("MOA_BRAIN", B)]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).starts_with("resumed "), "{}", out(&o));

    shows(&tb, &id, "idle");
    assert_eq!(ls_row(&tb, &id).unwrap()["conversation"], conversation);
    assert_eq!(agents(&tb), 1);
}
