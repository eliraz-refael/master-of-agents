//! Running `moa` against a TempBrain: on a terminal, as a user would, or to the end. Every
//! tmux query goes through `TempBrain::tmux` (T23).
#![allow(dead_code)]

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use moa_testkit::{TempBrain, Terminal, fake_claude, wait_for};
use serde_json::Value;

pub const MOA: &str = env!("CARGO_BIN_EXE_moa");
pub const B: &str = "default";
/// What the status line shows once moa's config is in.
pub const STATUS_LINE: &str = "^a keys";
/// How long a wait for something to show may take: every wait returns as soon as it does,
/// so this only lengthens a failure.
pub const SHOWS: Duration = Duration::from_secs(10);

/// `moa args…` on a terminal, with the root's environment plus `extra`.
pub fn open(tb: &TempBrain, args: &[&str], extra: &[(&str, &str)]) -> Terminal {
    let mut env = tb.env();
    env.extend(
        extra
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    Terminal::spawn(Path::new(MOA), &args, &env, 120, 30).unwrap()
}

/// `exe args…` to the end, with the root's environment plus `extra`.
pub fn run_exe(tb: &TempBrain, exe: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    Command::new(exe)
        .args(args)
        .env_clear()
        .envs(tb.env())
        .envs(extra.iter().copied())
        .current_dir(tb.root())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap()
}

/// `moa args…` to the end, with the root's environment plus `extra`.
pub fn run(tb: &TempBrain, args: &[&str], extra: &[(&str, &str)]) -> Output {
    run_exe(tb, Path::new(MOA), args, extra)
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn tmux(tb: &TempBrain, args: &[&str]) -> String {
    tb.tmux(B, args)
        .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
}

/// The session each user's terminal (not a control client) is on.
pub fn user_clients(tb: &TempBrain) -> Vec<String> {
    let list = tmux(
        tb,
        &[
            "list-clients",
            "-F",
            "#{client_control_mode} #{session_name}",
        ],
    );
    list.lines()
        .filter_map(|l| l.strip_prefix("0 "))
        .map(str::to_string)
        .collect()
}

pub fn server_pid(tb: &TempBrain) -> String {
    tmux(tb, &["display-message", "-p", "#{pid}"])
}

pub fn status_json(tb: &TempBrain) -> Value {
    let o = run(tb, &["status", "--json"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    serde_json::from_str(&out(&o)).unwrap()
}

pub fn shutdown(tb: &TempBrain) {
    let o = run(tb, &["shutdown"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

/// `moactl`, linked to `moa` as an install would.
pub fn moactl(tb: &TempBrain) -> PathBuf {
    let bin = tb.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("moactl");
    if !path.exists() {
        symlink(MOA, &path).unwrap();
    }
    path
}

/// Waits until `term` shows the status line and the daemon reports its terminal link. A
/// failure names the caller's line, and shows the terminal and the daemon's log.
#[track_caller]
pub fn came_up(tb: &TempBrain, term: &Terminal) {
    assert!(
        term.wait_for_text(STATUS_LINE, SHOWS),
        "no status line\n{}",
        what_happened(tb, term)
    );
    let linked = wait_for(SHOWS, || status_json(tb)["tmux"].is_object().then_some(()));
    assert!(
        linked.is_some(),
        "not linked: {}\n{}",
        status_json(tb),
        what_happened(tb, term)
    );
}

/// For a failure message: what `term` showed last, what `moa status` says, and the tail of
/// the daemon's log.
pub fn what_happened(tb: &TempBrain, term: &Terminal) -> String {
    let status = Command::new(MOA)
        .arg("status")
        .env_clear()
        .envs(tb.env())
        .current_dir(tb.root())
        .timeout(Duration::from_secs(5))
        .output()
        .map_or_else(|e| e.to_string(), |o| out(&o) + &err(&o));
    format!(
        "the terminal showed: {}\nmoa status: {status}\n{}",
        term.tail(1500),
        tb.log_tail(B, 60)
    )
}

/// The root's `moa.toml`: brain `default` on `<root>/work`, opening sessions with `acct-a`,
/// a Claude account whose store is `<root>/.claude-a` and whose launcher is fake-claude, so
/// the real `claude` is never looked for. Both folders are made.
pub fn moa_toml_with_fake_account(tb: &TempBrain) {
    moa_toml_with_launcher(tb, &fake_claude());
}

/// [`moa_toml_with_fake_account`], with `launcher` as `acct-a`'s launcher.
pub fn moa_toml_with_launcher(tb: &TempBrain, launcher: &Path) {
    let work = tb.root().join("work");
    let store = tb.root().join(".claude-a");
    for dir in [&work, &store] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let toml = format!(
        "[brains.default]\nfolders = [\"{}\"]\nopen_account = \"acct-a\"\n\n\
         [accounts.acct-a]\nkind = \"claude\"\nstore = \"{}\"\nlauncher = \"{}\"\n",
        work.display(),
        store.display(),
        launcher.display()
    );
    let paths = tb.paths();
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(paths.config_file(), toml).unwrap();
}

/// Session `id`'s row in `moa ls --json`, or `None` while it isn't listed.
pub fn ls_row(tb: &TempBrain, id: &str) -> Option<Value> {
    let o = run(tb, &["--json", "ls"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let rows: Value = serde_json::from_str(&out(&o)).unwrap();
    rows.as_array()?.iter().find(|r| r["id"] == id).cloned()
}

/// Session `id`'s `display` in `moa ls --json`, or `None` while it isn't listed.
pub fn ls_display(tb: &TempBrain, id: &str) -> Option<String> {
    ls_row(tb, id)?["display"].as_str().map(str::to_string)
}

/// Waits until the user clients are `want`.
pub fn clients_become(tb: &TempBrain, want: &[&str]) {
    let seen = wait_for(SHOWS, || {
        let c = user_clients(tb);
        (c == want).then_some(())
    });
    assert!(seen.is_some(), "clients: {:?}", user_clients(tb));
}
