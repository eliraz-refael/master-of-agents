use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

mod common;

use assert_cmd::Command;
use moa_testkit::{TempBrain, Terminal, wait_for};

use common::{build_of, panels_on};

const MOA: &str = env!("CARGO_BIN_EXE_moa");

#[test]
fn version_prints_name_and_version() {
    Command::cargo_bin("moa")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout("moa 0.1.0\n");
}

/// T21: until the switch-over, the front door opens only a sandbox, even with
/// `MOA_REAL_HOME=1`, and starts nothing.
#[test]
fn moa_without_the_sandbox_refuses() {
    let tb = TempBrain::new(None);
    for real in [None, Some("1")] {
        let mut cmd = Command::cargo_bin("moa").unwrap();
        cmd.env_clear()
            .env("HOME", tb.root())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("SHELL", "/bin/sh");
        if let Some(v) = real {
            cmd.env("MOA_REAL_HOME", v);
        }
        cmd.assert()
            .code(1)
            .stdout("")
            .stderr("moa v1 isn't switched over yet; try moa --sandbox\n");
        // No daemon (no socket, no lock) and no terminal server under the root.
        let state = tb.root().join(".local/state");
        assert!(!state.exists(), "{} was created", state.display());
    }
}

/// V15: `moa panel` takes the views it draws, by name.
#[test]
fn moa_panel_names_the_views_it_knows() {
    let tb = TempBrain::new(None);
    Command::cargo_bin("moa")
        .unwrap()
        .args(["panel", "nope"])
        .env_clear()
        .envs(tb.env())
        .assert()
        .code(1)
        .stdout("")
        .stderr(
            "moa panel: no view called nope; views: tree, plan, status, today, projects, \
             activity, picture\n",
        );
    assert!(!tb.root().join(".local").exists(), "nothing started");
}

/// V15: outside the sandbox `moa panel` refuses as every verb does until the switch-over,
/// before it needs anything on `PATH` or touches the terminal.
#[test]
fn moa_panel_refuses_outside_the_sandbox() {
    let tb = TempBrain::new(None);
    let empty = tb.root().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    for args in [&["panel"][..], &["panel", "tree"]] {
        Command::cargo_bin("moa")
            .unwrap()
            .args(args)
            .env_clear()
            .env("HOME", tb.root())
            .env("PATH", &empty)
            .assert()
            .code(1)
            .stdout("")
            .stderr("moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)\n");
    }
    assert!(!tb.root().join(".local").exists(), "nothing started");
}

/// `moa status --json`'s pid, run as `exe`.
fn daemon_pid(tb: &TempBrain, exe: &Path) -> u64 {
    let out = std::process::Command::new(exe)
        .args(["status", "--json"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_BRAIN", "default")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["pid"].as_u64().expect("a pid")
}

/// V13, §6.15, Review Focus 4: a panel that finds another build's daemon follows it into
/// the daemon's own program, in its own terminal, and leaves the daemon running: restarting
/// a daemon other panels use isn't a panel's call.
#[test]
fn a_different_build_is_followed_not_restarted() {
    let tb = TempBrain::new(None);
    // The same binary at another path is another build.
    let other = tb.root().join("moa-other");
    std::fs::copy(MOA, &other).unwrap();
    let started = std::process::Command::new(&other)
        .args(["ls"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_BRAIN", "default")
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let pid = daemon_pid(&tb, &other);

    let args: Vec<OsString> = ["panel", "tree", "--brain", "default"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let term = Terminal::spawn(Path::new(MOA), &args, &tb.env(), 100, 20).unwrap();
    let log = tb.paths().log_dir().join("panel.log");
    let theirs = build_of(&tb, &other);
    let line = format!(
        "panel tree in -: followed {} → {theirs}",
        build_of(&tb, Path::new(MOA))
    );
    let followed = wait_for(Duration::from_secs(10), || {
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        written.contains(&line).then_some(())
    });
    assert!(
        followed.is_some(),
        "{}\n{}",
        std::fs::read_to_string(&log).unwrap_or_default(),
        term.tail(400)
    );
    // Linked to the daemon on its build: only a panel whose hello matched has a row.
    let linked = wait_for(Duration::from_secs(10), || {
        let rows = panels_on(&tb, &theirs);
        rows.iter()
            .any(|r| r["view"] == "tree" && r["pane"].is_null())
            .then_some(())
    });
    assert!(linked.is_some(), "{}", term.tail(400));
    assert!(
        term.wait_for_text("▤ sessions", Duration::from_secs(10)),
        "{}",
        term.tail(400)
    );
    assert!(
        !term.tail(4000).contains("from another build"),
        "{}",
        term.tail(400)
    );
    assert_eq!(
        daemon_pid(&tb, Path::new(MOA)),
        pid,
        "the daemon kept running"
    );
}

/// `exe args…` against `tb`'s default brain; it must succeed.
fn ok(tb: &TempBrain, exe: &Path, args: &[&str]) {
    let out = std::process::Command::new(exe)
        .args(args)
        .env_clear()
        .envs(tb.env())
        .env("MOA_BRAIN", "default")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The loop end to end: a panel draws its brain, says so when moa stops, comes back when it
/// runs again (V13), and Ctrl-c gives the terminal back.
#[test]
fn a_panel_follows_its_brain_and_gives_the_terminal_back() {
    let tb = TempBrain::new(None);
    tb.adopt_typical("default");
    let moa = Path::new(MOA);
    let args: Vec<OsString> = ["panel", "tree", "--brain", "default"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let term = Terminal::spawn(moa, &args, &tb.env(), 120, 30).unwrap();
    let shows = |text: &str| {
        assert!(
            term.wait_for_text(text, Duration::from_secs(10)),
            "{text:?} isn't shown: {}",
            term.tail(600)
        );
    };
    // The first connect started the daemon, and the tree shows its sessions.
    shows("Wire up the dimmer");

    term.mark();
    ok(&tb, moa, &["shutdown"]);
    shows("moa isn't running; trying again…");
    term.mark();
    ok(&tb, moa, &["ls"]);
    // Back: the footer shows the tree's hints again.
    shows("⏎/click open");

    term.mark();
    term.press(b"\x03");
    shows(moa_tui::run::RESTORE);
}
