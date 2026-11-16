//! Brain isolation through the real CLI (spec section 11.2): what is written under the work
//! brain never reaches the default one. Only `/tmp` roots and fixture names.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use moa_testkit::TempBrain;
use rusqlite::{Connection, OpenFlags};

const MOA: &str = env!("CARGO_BIN_EXE_moa");

const CONFIG: &str = r#"
[brains.default]
folders = ["~/private"]
[brains.work]
folders = ["~/work"]
[accounts.acct-a]
kind = "claude"
store = "~/.claude-acct-a"
brain = "work"
except = ["~/private"]
[accounts.main]
kind = "claude"
store = "~/.claude"
brain = "work"
except = ["~/private"]
"#;

struct Rig {
    tb: TempBrain,
}

fn rig(config: &str) -> Rig {
    let tb = TempBrain::new(Some(config));
    let root = tb.root();
    for d in [
        "private/lantern",
        "work/tidepool",
        ".claude-acct-a",
        ".claude",
        "bin",
    ] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    symlink(MOA, root.join("bin/moactl")).unwrap();
    Rig { tb }
}

impl Rig {
    fn dir(&self, rel: &str) -> PathBuf {
        self.tb.root().join(rel)
    }

    fn run(&self, exe: &Path, args: &[&str], cwd: &str, extra: &[(&str, &str)]) -> Output {
        Command::new(exe)
            .args(args)
            .env_clear()
            .envs(self.tb.env())
            .envs(extra.iter().copied())
            .current_dir(self.dir(cwd))
            .output()
            .unwrap()
    }

    fn moactl(&self, args: &[&str], cwd: &str, extra: &[(&str, &str)]) -> Output {
        self.run(&self.dir("bin/moactl"), args, cwd, extra)
    }

    fn moa(&self, args: &[&str]) -> Output {
        self.run(Path::new(MOA), args, "private", &[])
    }

    fn sockets(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.tb.paths().run_dir()) else {
            return vec![];
        };
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sock"))
            .collect()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) {
    assert!(o.status.success(), "stdout: {}\nstderr: {}", out(o), err(o));
}

/// Every text value of every table of a brain's database, read-only, as `table.column: value`.
fn all_text(db: &Path) -> Vec<String> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = conn
        .prepare("select name from sqlite_master where type = 'table' and name not like 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut found = vec![];
    for t in tables {
        let mut stmt = conn.prepare(&format!("select * from \"{t}\"")).unwrap();
        let cols: Vec<String> = stmt.column_names().map(str::to_string).collect();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for (i, c) in cols.iter().enumerate() {
                if let rusqlite::types::ValueRef::Text(b) = row.get_ref(i).unwrap() {
                    found.push(format!("{t}.{c}: {}", String::from_utf8_lossy(b)));
                }
            }
        }
    }
    found
}

fn count(db: &Path, table: &str) -> i64 {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
        .unwrap()
}

fn names(db: &Path, sql: &str) -> Vec<String> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.prepare(sql)
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn work_never_lands_in_default() {
    let r = rig(CONFIG);
    let acct_a = r.dir(".claude-acct-a");
    let a = [("CLAUDE_CONFIG_DIR", acct_a.to_str().unwrap())];

    // The work account, from a work folder: the work daemon autostarts.
    ok(&r.moactl(&["project", "Tidepool", "--add", "."], "work/tidepool", &a));
    ok(&r.moactl(
        &["note", "WORK-NOTE-1", "--project", "Tidepool"],
        "work/tidepool",
        &a,
    ));
    ok(&r.moactl(
        &["task", "add", "Tidepool", "WORK-TASK-1"],
        "work/tidepool",
        &a,
    ));
    // No CLAUDE_CONFIG_DIR: the default account, `~/.claude`, is listed too.
    ok(&r.moactl(
        &["note", "WORK-NOTE-2", "--project", "Tidepool"],
        "work/tidepool",
        &[],
    ));
    // An `except` folder goes to the default brain.
    ok(&r.moactl(&["project", "Lantern", "--add", "."], "private/lantern", &a));
    // Forced onto default, a work folder is refused.
    let o = r.moactl(
        &["project", "Other", "--add", "."],
        "work/tidepool",
        &[("MOA_BRAIN", "default")],
    );
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("isn't in this brain's folders; this brain doesn't track it"),
        "{}",
        err(&o)
    );

    ok(&r.moa(&["shutdown", "--brain", "work"]));
    ok(&r.moa(&["shutdown", "--brain", "default"]));

    let (default, work) = (r.tb.db("default"), r.tb.db("work"));
    assert_eq!(
        names(&default, "select name from projects order by name"),
        ["Lantern"]
    );
    assert_eq!(count(&default, "notes"), 0);
    assert_eq!(count(&default, "tasks"), 0);
    let leaked: Vec<String> = all_text(&default)
        .into_iter()
        .filter(|t| t.contains("WORK-"))
        .collect();
    assert!(leaked.is_empty(), "work text in default: {leaked:?}");

    assert_eq!(
        names(&work, "select name from projects order by name"),
        ["Tidepool"]
    );
    assert_eq!(count(&work, "notes"), 2);
    assert_eq!(count(&work, "tasks"), 1);
    let text = all_text(&work).join("\n");
    for token in ["WORK-NOTE-1", "WORK-NOTE-2", "WORK-TASK-1"] {
        assert!(text.contains(token), "{token} is missing from work");
    }
}

/// S17: an account moa.toml doesn't list can read but not write, unless it names a brain.
#[test]
fn an_unlisted_account_cannot_write() {
    let r = rig(CONFIG);
    let other = r.dir(".claude-other");
    let o = [("CLAUDE_CONFIG_DIR", other.to_str().unwrap())];

    let w = r.moactl(&["note", "x"], "private/lantern", &o);
    assert_eq!(w.status.code(), Some(1), "{}{}", out(&w), err(&w));
    assert_eq!(
        err(&w),
        "~/.claude-other isn't an account in moa.toml, so moa won't save anything from it; \
         add it under [accounts] or set MOA_BRAIN\n"
    );
    assert!(r.sockets().is_empty(), "{:?}", r.sockets());
    assert!(!r.tb.paths().run_dir().exists(), "no daemon was started");

    // A listed account (no CLAUDE_CONFIG_DIR) makes Lantern in default.
    ok(&r.moactl(
        &["project", "Lantern", "--add", "."],
        "private/lantern",
        &[],
    ));
    // Reads still route to default.
    ok(&r.moactl(&["brief"], "private/lantern", &o));
    // The override names a brain, so the write goes through.
    ok(&r.moactl(
        &["note", "x", "--project", "Lantern"],
        "private/lantern",
        &[
            ("CLAUDE_CONFIG_DIR", other.to_str().unwrap()),
            ("MOA_BRAIN", "default"),
        ],
    ));
    ok(&r.moa(&["shutdown", "--brain", "default"]));
    assert_eq!(count(&r.tb.db("default"), "notes"), 1);
    assert!(
        !r.tb.db("work").exists(),
        "the work brain was never started"
    );

    // With no `[accounts]` at all, routing is unchanged: nothing is refused.
    let r =
        rig("[brains.default]\nfolders = [\"~/private\"]\n[brains.work]\nfolders = [\"~/work\"]\n");
    let other = r.dir(".claude-other");
    ok(&r.moactl(
        &["note", "x"],
        "private/lantern",
        &[("CLAUDE_CONFIG_DIR", other.to_str().unwrap())],
    ));
}
