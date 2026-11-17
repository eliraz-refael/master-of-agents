//! Running `moa` against a TempBrain: on a terminal, as a user would, or to the end. Every
//! tmux query goes through `TempBrain::tmux` (T23).
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use moa_testkit::{TempBrain, Terminal, wait_for};
use serde_json::Value;

pub const MOA: &str = env!("CARGO_BIN_EXE_moa");
pub const B: &str = "default";
/// What the status line shows once moa's config is in.
pub const STATUS_LINE: &str = "^a keys";
pub const SHOWS: Duration = Duration::from_secs(5);

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

/// Waits until the user clients are `want`.
pub fn clients_become(tb: &TempBrain, want: &[&str]) {
    let seen = wait_for(SHOWS, || {
        let c = user_clients(tb);
        (c == want).then_some(())
    });
    assert!(seen.is_some(), "clients: {:?}", user_clients(tb));
}
