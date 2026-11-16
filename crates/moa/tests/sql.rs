//! `moa sql`: the real binary, run only against a `/tmp` root.

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use moa_testkit::TempBrain;

const MOA: &str = env!("CARGO_BIN_EXE_moa");

struct Rig {
    tb: TempBrain,
    cwd: PathBuf,
}

fn rig(adopt: bool) -> Rig {
    let tb = TempBrain::new(None);
    let cwd = tb.root().join("private/lantern");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(tb.root().join("bin")).unwrap();
    symlink(MOA, tb.root().join("bin/moactl")).unwrap();
    if adopt {
        tb.adopt_typical("default");
    }
    Rig { tb, cwd }
}

impl Rig {
    fn command(&self, exe: &str) -> Command {
        let mut c = Command::new(exe);
        c.env_clear().envs(self.tb.env()).current_dir(&self.cwd);
        c
    }

    fn moa(&self, args: &[&str]) -> Output {
        self.command(MOA).args(args).output().unwrap()
    }

    fn moactl(&self, args: &[&str]) -> Output {
        let exe = self.tb.root().join("bin/moactl");
        self.command(exe.to_str().unwrap())
            .args(args)
            .output()
            .unwrap()
    }

    /// `moa` with `input` on stdin (a pipe, so no prompt).
    fn moa_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(MOA)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn count(r: &Rig, table: &str) -> String {
    let o = r.moa(&["sql", &format!("select count(*) from {table}")]);
    assert!(o.status.success(), "{}", err(&o));
    out(&o).lines().nth(1).unwrap().to_string()
}

#[test]
fn select_prints_a_header_and_rows() {
    let r = rig(true);
    let o = r.moa(&["sql", "select count(*) from projects"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "count(*)\n3\n");
    assert_eq!(err(&o), "");
}

#[test]
fn null_is_empty_and_columns_are_tabbed() {
    let r = rig(true);
    let o = r.moa(&["sql", "select 1 as a, null as b, 'x' as c, 1.5 as d"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "a\tb\tc\td\n1\t\tx\t1.5\n");
}

#[test]
fn writes_are_refused_and_change_nothing() {
    let r = rig(true);
    assert_eq!(count(&r, "tasks"), "5");
    let o = r.moa(&["sql", "delete from tasks"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("readonly"), "{}", err(&o));
    assert_eq!(err(&o).lines().count(), 1, "{}", err(&o));
    assert_eq!(count(&r, "tasks"), "5");
}

#[test]
fn stdin_runs_each_line() {
    let r = rig(true);
    let o = r.moa_stdin(
        &["sql"],
        "select count(*) from projects\nselect nope from nowhere\n\nselect count(*) from tasks\n",
    );
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "count(*)\n3\ncount(*)\n5\n");
    assert_eq!(err(&o).lines().count(), 1, "{}", err(&o));
    assert!(err(&o).contains("nowhere"), "{}", err(&o));
}

#[test]
fn a_missing_brain_says_so() {
    let r = rig(false);
    let o = r.moa(&["sql", "select 1"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "the default brain has no database yet\n");
    assert_eq!(out(&o), "");
}

#[test]
fn a_bad_brain_name_is_worded_like_status() {
    let r = rig(true);
    let a = r.moa(&["sql", "--brain", "No Such!", "select 1"]);
    let b = r.moa(&["status", "--brain", "No Such!"]);
    assert_eq!(a.status.code(), Some(1));
    assert!(err(&a).starts_with("--brain: "), "{}", err(&a));
    assert_eq!(err(&a), err(&b));
}

#[test]
fn works_while_the_daemon_runs() {
    let r = rig(true);
    let o = r.moactl(&["brief"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&r.moa(&["status"])).contains("running"));
    let before: i64 = count(&r, "tasks").parse().unwrap();
    let name = r.moa(&["sql", "select name from projects order by id limit 1"]);
    let name = out(&name).lines().nth(1).unwrap().to_string();
    let added = r.moactl(&["task", "add", &name, "a task from the test"]);
    assert!(added.status.success(), "{}", err(&added));
    assert_eq!(count(&r, "tasks"), (before + 1).to_string());
    r.moa(&["shutdown"]);
}
