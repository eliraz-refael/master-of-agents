//! `moactl` and `moa <verb>`, `moa status|shutdown` and the offline tools' guard: the real
//! binary, run only against a `/tmp` root.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use moa_testkit::TempBrain;

const MOA: &str = env!("CARGO_BIN_EXE_moa");

/// A root, a folder to run from, and `moactl` linked to `moa`.
struct Rig {
    tb: TempBrain,
    cwd: PathBuf,
}

fn rig(moa_toml: Option<&str>) -> Rig {
    let tb = TempBrain::new(moa_toml);
    let cwd = tb.root().join("private/lantern");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(tb.root().join("bin")).unwrap();
    symlink(MOA, tb.root().join("bin/moactl")).unwrap();
    Rig { tb, cwd }
}

fn typical() -> Rig {
    let r = rig(None);
    r.tb.adopt_typical("default");
    r
}

impl Rig {
    fn moactl_path(&self) -> PathBuf {
        self.tb.root().join("bin/moactl")
    }

    /// `exe args…` from `cwd`, with only the testkit's environment plus `extra`.
    fn run_as(&self, exe: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        Command::new(exe)
            .args(args)
            .env_clear()
            .envs(self.tb.env())
            .envs(extra.iter().copied())
            .current_dir(&self.cwd)
            .output()
            .unwrap()
    }

    fn moa(&self, args: &[&str]) -> Output {
        self.run_as(Path::new(MOA), args, &[])
    }

    fn moactl(&self, args: &[&str]) -> Output {
        self.run_as(&self.moactl_path(), args, &[])
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

fn pid_of(status: &Output) -> u32 {
    let text = out(status);
    let after = text.split("pid ").nth(1).expect("a pid in the status");
    after
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn moactl_autostarts_and_answers() {
    let r = typical();
    let o = r.moactl(&["brief"]);
    ok(&o);
    assert!(out(&o).starts_with("# Brief ("), "{}", out(&o));
    let first = r.moa(&["status"]);
    ok(&first);
    let pid = pid_of(&first);
    ok(&r.moactl(&["task", "ls"]));
    assert_eq!(pid_of(&r.moa(&["status"])), pid);
}

#[test]
fn moa_verb_equals_moactl() {
    let r = typical();
    let a = r.moa(&["task", "ls"]);
    let b = r.moactl(&["task", "ls"]);
    ok(&a);
    ok(&b);
    assert_eq!(out(&a), out(&b));
    // The global flag may come before the verb.
    let c = r.moa(&["--json", "task", "ls"]);
    ok(&c);
    assert!(out(&c).starts_with('['), "{}", out(&c));
}

#[test]
fn racing_clients_start_one_daemon() {
    let r = rig(None);
    let moactl = r.moactl_path();
    // Output goes to files, not pipes: on macOS std sets CLOEXEC on a pipe after creating it, so a
    // racing spawn can leak a pipe end into another child (the daemon) and `.output()` never ends.
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let (exe, cwd, env) = (moactl.clone(), r.cwd.clone(), r.tb.env());
            let (so, se) = (
                r.tb.root().join(format!("race{i}.out")),
                r.tb.root().join(format!("race{i}.err")),
            );
            std::thread::spawn(move || {
                let status = Command::new(exe)
                    .arg("brief")
                    .env_clear()
                    .envs(env)
                    .current_dir(cwd)
                    .stdout(fs::File::create(&so).unwrap())
                    .stderr(fs::File::create(&se).unwrap())
                    .status()
                    .unwrap();
                Output {
                    status,
                    stdout: fs::read(&so).unwrap(),
                    stderr: fs::read(&se).unwrap(),
                }
            })
        })
        .collect();
    for h in handles {
        ok(&h.join().unwrap());
    }
    ok(&r.moa(&["status"]));
    assert_eq!(r.sockets().len(), 1, "{:?}", r.sockets());
    // A daemon that gets past the lock logs one start line; a second one would show here.
    let log = fs::read_to_string(r.tb.paths().log("default")).unwrap();
    let starts = log.matches("moa daemon starting for brain default").count();
    assert_eq!(starts, 1, "one daemon took the lock:\n{log}");
}

#[test]
fn without_moa_root_nothing_starts() {
    let r = rig(None);
    let line = "moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)\n";
    for exe in [r.moactl_path(), PathBuf::from(MOA)] {
        let args: &[&str] = if exe.ends_with("moactl") {
            &["brief"]
        } else {
            &["daemon"]
        };
        let o = Command::new(&exe)
            .args(args)
            .env_clear()
            .env("HOME", r.tb.root())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&r.cwd)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
        assert_eq!(format!("{}{}", out(&o), err(&o)), line);
    }
    assert!(!r.tb.root().join(".local").exists());
}

#[test]
fn errors_are_one_line_and_exit_1() {
    let r = typical();
    let o = r.moactl(&["task", "set", "999", "--status", "done"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "no task #999\n");
    assert_eq!(out(&o), "");
}

#[test]
fn later_verbs_say_so_without_a_daemon() {
    let r = rig(None);
    let o = r.moactl(&["resume", "x"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "moactl resume isn't in this build yet\n");
    assert!(r.sockets().is_empty());
    assert!(!r.tb.paths().run_dir().exists());
}

/// The folder is made first, as the spike did, and sent in its canonical form; with no
/// terminal linked the daemon then says so.
#[test]
fn open_makes_a_missing_folder() {
    let r = typical();
    let o = r.moactl(&["open", "../driftwood/new"]);
    let made = r.tb.root().join("private/driftwood/new");
    assert!(made.is_dir());
    // Named from the folder it was typed in, as the system spells it.
    let shown = fs::canonicalize(r.tb.root())
        .unwrap()
        .join("private/driftwood/new");
    assert_eq!(out(&o), format!("created {}\n", shown.display()));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "moa isn't open in a terminal yet; run moa first\n");
    // An existing folder is just used.
    let o = r.moactl(&["open", "."]);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "moa isn't open in a terminal yet; run moa first\n");
}

#[test]
fn json_prints_data() {
    let r = typical();
    let o = r.moactl(&["task", "ls", "--json"]);
    ok(&o);
    assert_eq!(out(&o).lines().count(), 1, "one line");
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v.as_array().expect("an array").len(), 3, "{v}");
}

#[test]
fn a_rebuilt_client_restarts_the_daemon_on_its_build() {
    let r = typical();
    let other_dir = r.tb.root().join("other");
    fs::create_dir_all(&other_dir).unwrap();
    // Another path is another build id.
    fs::copy(MOA, other_dir.join("moa")).unwrap();
    symlink(other_dir.join("moa"), other_dir.join("moactl")).unwrap();

    ok(&r.moactl(&["brief"]));
    let before = r.moa(&["status"]);
    ok(&before);

    let o = r.run_as(&other_dir.join("moactl"), &["brief"], &[]);
    ok(&o);
    let after = r.run_as(&other_dir.join("moa"), &["status"], &[]);
    ok(&after);
    assert_ne!(pid_of(&after), pid_of(&before));
    let build = |o: &Output| {
        out(o)
            .split("build ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .to_string()
    };
    assert_ne!(build(&after), build(&before));
}

#[test]
fn the_caller_reaches_the_daemon() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    // S1's moa id is a uuid whose handle is `3f9a1c52`.
    let ls = r.moactl(&["ls", "--all", "--json"]);
    ok(&ls);
    let rows: serde_json::Value = serde_json::from_str(&out(&ls)).unwrap();
    let id = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["title"] == "Wire up the dimmer")
        .expect("S1 is there")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let o = r.run_as(Path::new(MOA), &["status"], &[("MOA_SESSION", &id)]);
    ok(&o);
    assert!(out(&o).contains("you are: 3f9a1c52\n"), "{}", out(&o));
    let o = r.moa(&["status"]);
    ok(&o);
    assert!(
        out(&o).contains("you are: unknown in lantern\n"),
        "{}",
        out(&o)
    );
}

#[test]
fn status_prints_four_lines_in_state_order() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    let o = r.moa(&["status"]);
    ok(&o);
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(lines[3], "terminal: not connected");
    assert!(
        lines[0].starts_with("moa default: running, build ") && lines[0].ends_with(" launches"),
        "{}",
        lines[0]
    );
    let sessions = lines[2]
        .strip_prefix("sessions: ")
        .expect("a sessions line");
    let words: Vec<&str> = sessions
        .split(", ")
        .map(|p| p.split(' ').next().unwrap())
        .collect();
    let order: Vec<String> = moa_core::live::DisplayState::ALL
        .iter()
        .map(|d| moa_core::publish::word(*d))
        .collect();
    let mut at = 0;
    for w in &words {
        let i = order
            .iter()
            .position(|o| o == w)
            .unwrap_or_else(|| panic!("{w}"));
        assert!(i >= at, "{sessions}");
        at = i;
    }
    assert!(!sessions.contains(" 0"), "zeros are left out: {sessions}");
}

#[test]
fn status_without_a_daemon_says_so_and_starts_nothing() {
    let r = typical();
    let o = r.moa(&["status"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "the default brain's moa isn't running\n");
    assert!(r.sockets().is_empty());
}

#[test]
fn a_brain_missing_from_the_config_is_named() {
    let r = rig(Some("[brains.work]\nfolders = []\n"));
    let o = r.run_as(Path::new(MOA), &["status", "--brain", "ghost"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        format!(
            "there's no brain named ghost in {}\n",
            r.tb.paths().config_file().display()
        )
    );
    let o = r.run_as(&r.moactl_path(), &["brief"], &[("MOA_BRAIN", "ghost")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with("there's no brain named ghost in "),
        "{}",
        err(&o)
    );
    assert!(r.sockets().is_empty());
}

#[test]
fn bad_brain_names_say_where_they_came_from() {
    let r = rig(None);
    let o = r.run_as(&r.moactl_path(), &["brief"], &[("MOA_BRAIN", "Bad Name")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with("MOA_BRAIN: \"Bad Name\" isn't a brain name (use "),
        "{}",
        err(&o)
    );
    let o = r.moa(&["status", "--brain", "Bad"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with("--brain: \"Bad\" isn't a brain name (use "),
        "{}",
        err(&o)
    );
}

#[test]
fn moa_daemon_reports_already_running_and_exits_0() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    let o = r.moa(&["daemon"]);
    ok(&o);
    assert_eq!(
        format!("{}{}", out(&o), err(&o)),
        "the default brain's moa is already running\n"
    );
}

#[test]
fn offline_tools_refuse_a_running_brain() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    let db = r.tb.db("default");
    let line =
        "the default brain's moa is running; stop it first with: moa shutdown --brain default\n";
    let into = r.tb.root().join("x.db");
    let o = r.moa(&[
        "export-spike",
        "--from",
        db.to_str().unwrap(),
        "--into",
        into.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), line);
    assert!(!into.exists());
    // `adopt-spike --into` the running brain's file is refused for the same reason.
    let spike = r.tb.root().join("spike.db");
    fs::write(&spike, b"").unwrap();
    let o = r.moa(&[
        "adopt-spike",
        "--from",
        spike.to_str().unwrap(),
        "--into",
        db.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), line);
}

#[test]
fn adopt_spike_makes_the_brain_folder() {
    let r = rig(None);
    let home = r.tb.root().join("spike-home");
    fs::create_dir_all(&home).unwrap();
    let spike = r.tb.root().join("spike.db");
    moa_testkit::SpikeFixture::write(&spike, moa_testkit::Scenario::Typical, &home).unwrap();
    let into = r.tb.db("default");
    assert!(!into.parent().unwrap().exists());
    let o = r.moa(&[
        "adopt-spike",
        "--from",
        spike.to_str().unwrap(),
        "--into",
        into.to_str().unwrap(),
        "--tz",
        "UTC",
    ]);
    ok(&o);
    assert!(into.is_file());
    let mode = fs::metadata(into.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    // Any other missing folder is still refused.
    let elsewhere = r.tb.root().join("nowhere/moa.db");
    let o = r.moa(&[
        "adopt-spike",
        "--from",
        spike.to_str().unwrap(),
        "--into",
        elsewhere.to_str().unwrap(),
        "--tz",
        "UTC",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("doesn't exist"), "{}", err(&o));
}

#[test]
fn shutdown_stops_it() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    assert_eq!(r.sockets().len(), 1);
    let o = r.moa(&["shutdown"]);
    ok(&o);
    assert!(
        r.sockets().is_empty(),
        "the socket is gone right after exit"
    );
    let o = r.moa(&["shutdown"]);
    ok(&o);
    assert!(out(&o).contains("isn't running"), "{}", out(&o));
}

#[test]
fn an_unlisted_account_may_not_write() {
    let r = rig(Some(
        "[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-acct-a\"\n",
    ));
    let o = r.run_as(
        &r.moactl_path(),
        &["note", "x"],
        &[(
            "CLAUDE_CONFIG_DIR",
            r.tb.root().join(".claude-other").to_str().unwrap(),
        )],
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        "~/.claude-other isn't an account in moa.toml, so moa won't save anything from it; \
         add it under [accounts] or set MOA_BRAIN\n"
    );
    assert_eq!(out(&o), "");
    assert!(r.sockets().is_empty());
    assert!(!r.tb.paths().run_dir().exists(), "nothing was started");
    // With no CLAUDE_CONFIG_DIR the store is ~/.claude.
    let o = r.moactl(&["note", "x"]);
    assert!(
        err(&o).starts_with("~/.claude isn't an account in moa.toml"),
        "{}",
        err(&o)
    );
}

/// M1: the daemon launches nothing for data verbs. (`launches` counts processes the daemon
/// launched, not daemon starts.)
#[test]
fn cli_flow_launches_nothing_after_start() {
    let r = typical();
    ok(&r.moactl(&["brief"]));
    let verbs: [&[&str]; 10] = [
        &["attention"],
        &["ls"],
        &["ls", "--all"],
        &["find", "lantern"],
        &["project", "Lantern"],
        &["task", "ls"],
        &["task", "add", "Lantern", "Solder", "--due", "fri"],
        &["note", "volts", "--project", "Lantern"],
        &["group", "add", "Work/Infra"],
        &["suggestion", "ls"],
    ];
    for v in verbs {
        let o = r.moactl(v);
        assert!(o.status.success(), "{v:?}: {}{}", out(&o), err(&o));
    }
    let o = r.moa(&["status"]);
    ok(&o);
    assert!(out(&o).contains(" 0 launches"), "{}", out(&o));
}

/// M7: `moactl open <path>` goes to the brain the folder it opens belongs to, not the one of
/// the folder it was typed in. `acct-b` writes to `work` except in `~/private`: typed in
/// `~/private/lantern`, opening `~/work/beacon` goes to `work`. With `MOA_BRAIN` set, that
/// says where it goes, as before.
#[test]
fn open_goes_to_the_brain_of_the_folder_it_opens() {
    let r = rig(Some(
        r#"
[brains.default]
folders = ["~/private"]

[brains.work]
folders = ["~/work"]

[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
brain = "work"
except = ["~/private"]
"#,
    ));
    let store = r.tb.root().join(".claude-b");
    fs::create_dir_all(&store).unwrap();
    let store = store.to_str().unwrap();
    let target = r.tb.root().join("work/beacon");
    fs::create_dir_all(&target).unwrap();
    let target = target.to_str().unwrap();
    let o = r.run_as(
        &r.moactl_path(),
        &["open", target],
        &[("CLAUDE_CONFIG_DIR", store)],
    );
    assert_eq!(err(&o), "moa isn't open in a terminal yet; run moa first\n");
    assert_eq!(r.sockets(), ["work.sock"]);

    let o = r.run_as(
        &r.moactl_path(),
        &["open", target],
        &[("CLAUDE_CONFIG_DIR", store), ("MOA_BRAIN", "default")],
    );
    assert_eq!(err(&o), "moa isn't open in a terminal yet; run moa first\n");
    let mut sockets = r.sockets();
    sockets.sort();
    assert_eq!(sockets, ["default.sock", "work.sock"]);
}
