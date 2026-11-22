//! Sleep end to end: the real `moa` binaries, a TempBrain's own terminal server, and
//! fake-claude (named `claude`) started by `moactl open --bg`. An idle session off screen goes
//! to sleep on its own once `sleep_after_hours` has passed, and one in the cockpit's agent
//! spot leaves the empty slot in its place (5b's L11).

mod common;

use std::fs;

use moa_platform::pid_alive;
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

/// The cockpit's panes: id, `@moa_view` and `@moa_session`, of the window `moa` in session
/// `moa`.
fn cockpit(tb: &TempBrain) -> Vec<(String, String, String)> {
    tmux(
        tb,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_name}\t#{pane_id}\t#{@moa_view}\t#{@moa_session}",
        ],
    )
    .lines()
    .filter_map(|l| l.strip_prefix("moa\tmoa\t"))
    .filter_map(|l| {
        let mut f = l.split('\t');
        Some((f.next()?.into(), f.next()?.into(), f.next()?.into()))
    })
    .collect()
}

/// `moactl args…`, which must succeed: its output.
fn ok(tb: &TempBrain, term: &moa_testkit::Terminal, args: &[&str]) -> String {
    let o = run_exe(tb, &moactl(tb), args, &[("MOA_BRAIN", B)]);
    assert!(
        o.status.success(),
        "moactl {args:?}: {}{}\n{}",
        out(&o),
        err(&o),
        what_happened(tb, term)
    );
    out(&o)
}

/// L11 on real tmux: the agent in the `focus` layout's spot, put to sleep, leaves the empty
/// slot in its place; its process is gone and the session is asleep.
#[test]
fn the_spot_sleeps_and_shows_no_agent_here_yet() {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    ok(&tb, &term, &["layout", "apply", "focus"]);

    let dir = tb.root().join("work").join("a");
    fs::create_dir_all(&dir).unwrap();
    let start = FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::new(),
    };
    write_script(&dir, &[start, FakeStep::Hang]);
    let opened = ok(
        &tb,
        &term,
        &["--json", "open", "--bg", &dir.display().to_string()],
    );
    let data: Value = serde_json::from_str(&opened).unwrap();
    let id = data["id"].as_str().unwrap().to_string();
    ok(&tb, &term, &["focus", &id]);
    let spot = cockpit(&tb)
        .into_iter()
        .find(|(_, view, session)| view == "agent" && *session == id)
        .map(|(pane, _, _)| pane)
        .unwrap_or_else(|| panic!("not in the spot: {:?}", cockpit(&tb)));
    let pid: u32 = tmux(&tb, &["display-message", "-p", "-t", &spot, "#{pane_pid}"])
        .parse()
        .unwrap();

    ok(&tb, &term, &["sleep", &id]);
    let slot = wait_for(SHOWS, || {
        let (pane, _, _) = cockpit(&tb)
            .into_iter()
            .find(|(_, view, _)| view == "empty-slot")?;
        screen_of(&tb, &pane)
            .contains("no agent here yet")
            .then_some(())
    });
    assert!(
        slot.is_some(),
        "no empty slot in the spot: {:?}\n{}",
        cockpit(&tb),
        what_happened(&tb, &term)
    );
    assert!(
        !cockpit(&tb).iter().any(|(_, _, session)| *session == id),
        "{:?}",
        cockpit(&tb)
    );
    let gone = wait_for(SHOWS, || (!pid_alive(pid)).then_some(()));
    assert!(gone.is_some(), "the agent {pid} still runs");
    let asleep = wait_for(SHOWS, || {
        (ls_display(&tb, &id).as_deref() == Some("asleep")).then_some(())
    });
    assert!(asleep.is_some(), "{:?}", ls_display(&tb, &id));
}
