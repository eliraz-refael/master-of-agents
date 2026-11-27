//! The spike's forwarders: the `# v1-forward begin` … `# v1-forward end` blocks in its hook
//! (`track.sh`), its `moactl` and its launcher (`moa`). Without the switch flag they change
//! nothing; with it they hand the call to the v1 `moa` (or the `moactl` beside it) the flag
//! names.
//!
//! Each test runs a copy of the spike in its own /tmp root, never the repo's: the spike's
//! default brain DB lives in its own folder. No test starts a terminal server: `TMUX_TMPDIR`
//! is the root's own folder, and each test that could start one checks it stays empty.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use moa_testkit::{Scenario, SpikeFixture, ids};
use serde_json::json;
use tempfile::TempDir;

/// What the spike's `moactl` and launcher say when the flag names no v1.
const NO_V1: &str = "moa v1 isn't where the switch left it; run moa switch back";

/// How long one script may run: a forwarder that loops fails its test instead of hanging it.
const LIMIT: Duration = Duration::from_secs(10);

/// A conversation no fixture brain has seen.
const NEW_SID: &str = "3f6b8a21-9c4d-4e57-b0a2-6d1e7f9c8b35";

/// The tools the spike runs. A missing one fails the test, as a missing tmux does.
fn need(tool: &str) -> PathBuf {
    moa_platform::which(tool, std::env::var_os("PATH").as_deref()).unwrap_or_else(|| {
        panic!("{tool} not found on PATH (the spike needs it; or run inside `nix develop`)")
    })
}

/// The repo's spike, which is only ever read.
fn repo_spike() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spike")
}

/// Copy the spike to `to`, without a brain DB, error logs or Python caches. Where there is
/// no `/bin/bash` (the Linux Nix build sandbox), the copy's `#!/bin/bash` names the bash on
/// `PATH` instead.
fn copy_spike(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("brain.db") || name.ends_with(".err") || name == "__pycache__" {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(&name));
        if entry.file_type().unwrap().is_dir() {
            copy_spike(&src, &dst);
            continue;
        }
        let mut bytes = fs::read(&src).unwrap();
        if bytes.starts_with(b"#!/bin/bash\n") && !Path::new("/bin/bash").exists() {
            let bash = format!("#!{}\n", need("bash").display());
            bytes.splice(..b"#!/bin/bash\n".len(), bash.into_bytes());
        }
        fs::write(&dst, bytes).unwrap();
        let mode = fs::metadata(&src).unwrap().permissions().mode() | 0o600;
        fs::set_permissions(&dst, fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// A stand-in for a v1 binary: it notes its name, argv, stdin, `MOA_DB` and `MOA_BRAIN` in
/// `called.<n>/` beside it, then exits `code`.
fn stand_in(path: &Path, code: i32) {
    let script = format!(
        "#!/bin/sh\n\
         d=$(dirname \"$0\"); n=0\n\
         while [ -e \"$d/called.$n\" ]; do n=$((n+1)); done\n\
         c=\"$d/called.$n\"; mkdir \"$c\"\n\
         basename \"$0\" >\"$c/name\"\n\
         i=0; for a in \"$@\"; do printf '%s' \"$a\" >\"$c/arg.$i\"; i=$((i+1)); done\n\
         cat >\"$c/stdin\"\n\
         printf '%s' \"${{MOA_DB-}}\" >\"$c/MOA_DB\"\n\
         printf '%s' \"${{MOA_BRAIN-}}\" >\"$c/MOA_BRAIN\"\n\
         exit {code}\n"
    );
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[derive(Debug, PartialEq)]
struct Call {
    name: String,
    argv: Vec<String>,
    stdin: String,
    moa_db: String,
    moa_brain: String,
}

struct Rig {
    dir: TempDir,
    spike: PathBuf,
    home: PathBuf,
    /// The default brain's DB, as `MOA_DB` names it: the Typical fixture.
    db: PathBuf,
    /// The stand-in v1: `moa` and `moactl`.
    v1: PathBuf,
    /// `TMUX_TMPDIR`, which must stay empty.
    tmux: PathBuf,
    /// Leads `PATH`: a `tmux` that never runs one, and notes that it was called.
    guard: PathBuf,
}

fn rig() -> Rig {
    for tool in ["bash", "python3", "sqlite3", "jq"] {
        need(tool);
    }
    let dir = tempfile::Builder::new()
        .prefix("moa-fwd")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path();
    let (spike, home, v1, tmux, guard) = (
        root.join("spike"),
        root.join("home"),
        root.join("v1"),
        root.join("tmux"),
        root.join("guard"),
    );
    copy_spike(&repo_spike(), &spike);
    for d in [&home, &v1, &tmux, &guard] {
        fs::create_dir_all(d).unwrap();
    }
    let fake_tmux = guard.join("tmux");
    fs::write(
        &fake_tmux,
        "#!/bin/sh\necho \"$*\" >>\"$(dirname \"$0\")/tmux.called\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake_tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let db = root.join("brain.db");
    SpikeFixture::write(&db, Scenario::Typical, &home).unwrap();
    stand_in(&v1.join("moa"), 0);
    stand_in(&v1.join("moactl"), 3);
    Rig {
        dir,
        spike,
        home,
        db,
        v1,
        tmux,
        guard,
    }
}

impl Rig {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Write the switch flag, naming `moa` as the v1 binary.
    fn flag(&self, moa: &Path) {
        let state = self.home.join(".local/state/moa");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("v1-live"), format!("{}\n", moa.display())).unwrap();
    }

    /// The `work` brain's DB, where the spike keeps it: another Typical fixture.
    fn work_db(&self) -> PathBuf {
        let db = self.home.join(".local/share/moa/work/brain.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        SpikeFixture::write(&db, Scenario::Typical, &self.home).unwrap();
        db
    }

    /// A copy of the spike's script `rel`, beside it, with its v1-forward block removed.
    fn without_block(&self, rel: &str) -> PathBuf {
        let src = self.spike.join(rel);
        let text = fs::read_to_string(&src).unwrap();
        let mut kept = String::new();
        let mut inside = false;
        for line in text.split_inclusive('\n') {
            match line.trim_end() {
                "# v1-forward begin" => inside = true,
                "# v1-forward end" => inside = false,
                _ if !inside => kept.push_str(line),
                _ => {}
            }
        }
        assert_ne!(kept, text, "{rel} has no v1-forward block");
        let dst = src.with_file_name(format!(
            "{}-without-block",
            src.file_name().unwrap().to_string_lossy()
        ));
        fs::write(&dst, kept).unwrap();
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
        dst
    }

    /// Run `exe` with `args` and `stdin`, in a clean environment: `PATH` behind the guard,
    /// the rig's `HOME` and `TMUX_TMPDIR`, and `env`.
    fn run(&self, exe: &Path, args: &[&str], stdin: &str, env: &[(&str, &OsStr)]) -> Output {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.guard.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("TMUX_TMPDIR", &self.tmux)
            .current_dir(self.root());
        for (k, v) in env {
            cmd.env(k, v);
        }
        // assert_cmd kills the child at the time limit and returns its status.
        let started = Instant::now();
        let o = assert_cmd::Command::from_std(cmd)
            .write_stdin(stdin)
            .timeout(LIMIT)
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", exe.display()));
        assert!(
            !(o.status.signal() == Some(9) && started.elapsed() >= LIMIT),
            "{} {args:?} never ended within {LIMIT:?} (killed): {o:?}",
            exe.display()
        );
        o
    }

    /// Run the hook `script` as its installed command does (`bash <track.sh>`, from
    /// `moactl setup`), with `payload` on stdin.
    fn hook(&self, script: &Path, payload: &str, env: &[(&str, &OsStr)]) -> Output {
        self.run(&need("bash"), &[script.to_str().unwrap()], payload, env)
    }

    /// How the stand-ins were called, in order.
    fn calls(&self) -> Vec<Call> {
        let mut calls = Vec::new();
        for n in 0.. {
            let c = self.v1.join(format!("called.{n}"));
            if !c.exists() {
                break;
            }
            let read = |f: &str| fs::read_to_string(c.join(f)).unwrap();
            let argv = (0..)
                .map(|i| c.join(format!("arg.{i}")))
                .take_while(|p| p.exists())
                .map(|p| fs::read_to_string(p).unwrap())
                .collect();
            calls.push(Call {
                name: read("name").trim_end().to_string(),
                argv,
                stdin: read("stdin"),
                moa_db: read("MOA_DB"),
                moa_brain: read("MOA_BRAIN"),
            });
        }
        calls
    }

    /// No terminal server was started (or asked for), and no spike brain or brain settings
    /// were made.
    fn assert_nothing_created(&self) {
        let servers: Vec<_> = fs::read_dir(&self.tmux).unwrap().collect();
        assert!(servers.is_empty(), "a terminal server started: {servers:?}");
        let asked = fs::read_to_string(self.guard.join("tmux.called")).unwrap_or_default();
        assert_eq!(asked, "", "the script ran tmux");
        for made in [
            self.spike.join("master-chat/brain.db"),
            self.home.join(".local/share/moa/work/brain.db"),
            self.home.join(".config/moa/work.env"),
            self.home.join(".config/moa/default.env"),
        ] {
            assert!(!made.exists(), "{} was created", made.display());
        }
    }
}

/// Nothing on stdout or stderr.
fn assert_silent(o: &Output) {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert_eq!(
        (text(&o.stdout), text(&o.stderr)),
        (String::new(), String::new())
    );
}

fn os(p: &Path) -> &OsStr {
    p.as_os_str()
}

/// `sqlite3 db .dump`.
fn dump(db: &Path) -> String {
    let o = Command::new(need("sqlite3"))
        .arg(db)
        .arg(".dump")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// `text` with every `YYYY-MM-DD` and `HH:MM:SS` masked: the hook stamps rows with the
/// current time, so two runs a second apart differ only there.
fn mask_times(text: &str) -> String {
    let shape = |s: &[u8], pat: &[u8]| {
        s.len() >= pat.len()
            && pat.iter().zip(s).all(|(p, c)| match p {
                b'9' => c.is_ascii_digit(),
                _ => p == c,
            })
    };
    let b = text.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if shape(&b[i..], b"9999-99-99") {
            out.extend_from_slice(b"<date>");
            i += 10;
        } else if shape(&b[i..], b"99:99:99") {
            out.extend_from_slice(b"<time>");
            i += 8;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// Claude Code's payloads for one turn of `sid` in `cwd`: it starts, gets moa's ask #3 as a
/// prompt, and stops with its answer.
fn turn(sid: &str, cwd: &Path) -> [String; 3] {
    let cwd = cwd.to_string_lossy();
    [
        json!({"hook_event_name": "SessionStart", "session_id": sid, "cwd": cwd, "source": "startup"}),
        json!({"hook_event_name": "UserPromptSubmit", "session_id": sid, "cwd": cwd,
               "prompt": "[moa · message from moa · ask #3]\nPlease allow the command"}),
        json!({"hook_event_name": "Stop", "session_id": sid, "cwd": cwd,
               "last_assistant_message": "Allowed."}),
    ]
    .map(|v| v.to_string())
}

/// A tracked session's environment: its brain's DB and its account's store.
fn tracked<'a>(db: &'a Path, store: &'a Path) -> [(&'static str, &'a OsStr); 3] {
    [
        ("MOA_TRACK", OsStr::new("1")),
        ("MOA_DB", os(db)),
        ("CLAUDE_CONFIG_DIR", os(store)),
    ]
}

const TRACK: &str = "master-chat/hooks/track.sh";
const MOACTL: &str = "moa-proto/moactl";
const LAUNCHER: &str = "moa-proto/moa";
const PLUGIN_MOACTL: &str = "moa-plugin/moa/bin/moactl";

#[test]
fn track_without_the_flag_is_the_script_without_the_block() {
    let r = rig();
    let plain = r.without_block(TRACK);
    let other = r.root().join("other.db");
    SpikeFixture::write(&other, Scenario::Typical, &r.home).unwrap();
    let store = r.home.join(".claude-acct-a");
    let lantern = r.home.join("private/lantern");
    for payload in turn(ids::S3_WAITING, &lantern) {
        let with = r.hook(&r.spike.join(TRACK), &payload, &tracked(&r.db, &store));
        let without = r.hook(&plain, &payload, &tracked(&other, &store));
        assert!(with.status.success(), "{with:?}");
        assert_eq!(
            (with.status.code(), &with.stdout, &with.stderr),
            (without.status.code(), &without.stdout, &without.stderr)
        );
    }
    // The turn did what it does: ask #3 is answered.
    let ask: (String, String) = rusqlite::Connection::open(&r.db)
        .unwrap()
        .query_row("SELECT status, reply FROM asks WHERE id = 3", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(ask, ("answered".to_string(), "Allowed.".to_string()));
    assert_eq!(mask_times(&dump(&r.db)), mask_times(&dump(&other)));
    assert_eq!(r.calls(), []);
}

#[test]
fn track_with_the_flag_forwards_the_payload() {
    let r = rig();
    r.flag(&r.v1.join("moa"));
    let work = r.work_db();
    let before = (dump(&r.db), dump(&work));
    let store = r.home.join(".claude-acct-a");
    let [start, ..] = turn(ids::S3_WAITING, &r.home.join("private/lantern"));
    let o = r.hook(
        &r.spike.join(TRACK),
        &start,
        &[
            ("MOA_TRACK", OsStr::new("1")),
            ("MOA_DB", os(&r.db)),
            ("CLAUDE_CONFIG_DIR", os(&store)),
        ],
    );
    assert!(o.status.success(), "{o:?}");
    assert_silent(&o);

    // An account whose sessions go to the work brain, in a folder that isn't excepted.
    let elsewhere = r.root().join("elsewhere");
    let [routed, ..] = turn(NEW_SID, &elsewhere);
    let o = r.hook(
        &r.spike.join(TRACK),
        &routed,
        &[
            ("MOA_TRACK", OsStr::new("1")),
            ("MOA_DB", os(&r.db)),
            ("CLAUDE_CONFIG_DIR", os(&store)),
            ("MOA_ACCOUNT_BRAIN", OsStr::new("work")),
            ("MOA_EXCEPT_UNDER", os(&r.home.join("private"))),
        ],
    );
    assert!(o.status.success(), "{o:?}");

    let hook = |payload: &str, db: &Path| Call {
        name: "moa".into(),
        argv: vec!["hook".into(), "claude".into(), "--from-spike".into()],
        stdin: format!("{payload}\n"),
        moa_db: db.to_string_lossy().into_owned(),
        moa_brain: String::new(),
    };
    assert_eq!(r.calls(), [hook(&start, &r.db), hook(&routed, &work)]);
    assert_eq!((dump(&r.db), dump(&work)), before);
}

#[test]
fn track_with_the_flag_and_no_v1_writes_nothing() {
    let r = rig();
    r.flag(&r.root().join("gone/moa"));
    let before = dump(&r.db);
    for payload in turn(ids::S3_WAITING, &r.home.join("private/lantern")) {
        let o = r.hook(
            &r.spike.join(TRACK),
            &payload,
            &[
                ("MOA_TRACK", OsStr::new("1")),
                ("MOA_DB", os(&r.db)),
                ("CLAUDE_CONFIG_DIR", os(&r.home.join(".claude-acct-a"))),
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{o:?}");
        assert_silent(&o);
    }
    assert_eq!(dump(&r.db), before);
    assert_eq!(r.calls(), []);
}

#[test]
fn an_untracked_session_never_forwards() {
    let r = rig();
    r.flag(&r.v1.join("moa"));
    let before = dump(&r.db);
    let [start, ..] = turn(ids::S3_WAITING, &r.home.join("private/lantern"));
    let o = r.hook(&r.spike.join(TRACK), &start, &[("MOA_DB", os(&r.db))]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_silent(&o);
    assert_eq!(dump(&r.db), before);
    assert_eq!(r.calls(), []);
}

#[test]
fn moactl_without_the_flag_runs_the_spike() {
    let r = rig();
    let other = r.root().join("other.db");
    SpikeFixture::write(&other, Scenario::Typical, &r.home).unwrap();
    let py = r.spike.join("moa-proto/moactl.py");
    // `task ls` lists the fixture's tasks; `task ls --json` is a usage error (exit 2).
    for args in [&["task", "ls"][..], &["task", "ls", "--json"]] {
        let ours = r.run(&r.spike.join(MOACTL), args, "", &[("MOA_DB", os(&r.db))]);
        let mut direct = vec![py.to_str().unwrap()];
        direct.extend(args);
        let spike = r.run(&need("python3"), &direct, "", &[("MOA_DB", os(&other))]);
        assert_eq!(
            (ours.status.code(), &ours.stdout, &ours.stderr),
            (spike.status.code(), &spike.stdout, &spike.stderr),
            "moactl {args:?}"
        );
    }
    let listed = r.run(
        &r.spike.join(MOACTL),
        &["task", "ls"],
        "",
        &[("MOA_DB", os(&r.db))],
    );
    assert!(
        listed.status.success() && !listed.stdout.is_empty(),
        "{listed:?}"
    );
    assert_eq!(r.calls(), []);
    r.assert_nothing_created();
}

#[test]
fn moactl_with_the_flag_forwards_every_argument() {
    let r = rig();
    r.flag(&r.v1.join("moa"));
    let o = r.run(&r.spike.join(MOACTL), &["ask", "a b", "c\"d"], "", &[]);
    assert_eq!(o.status.code(), Some(3), "{o:?}");
    let calls = r.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, "moactl");
    assert_eq!(calls[0].argv, ["ask", "a b", "c\"d"]);
}

#[test]
fn the_plugins_moactl_forwards_through_the_spikes() {
    let r = rig();
    r.flag(&r.v1.join("moa"));
    let work = r.work_db();
    let elsewhere = r.root().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let o = r.run(
        &r.spike.join(PLUGIN_MOACTL),
        &["peers", "--json"],
        "",
        &[
            ("MOA_ACCOUNT_BRAIN", OsStr::new("work")),
            ("MOA_EXCEPT_UNDER", os(&r.home.join("private"))),
            ("CLAUDE_PROJECT_DIR", os(&elsewhere)),
        ],
    );
    assert_eq!(o.status.code(), Some(3), "{o:?}");
    let calls = r.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        (calls[0].name.as_str(), calls[0].argv.as_slice()),
        ("moactl", &["peers".to_string(), "--json".to_string()][..])
    );
    assert_eq!(calls[0].moa_brain, "work");
    assert_eq!(calls[0].moa_db, work.to_string_lossy());
}

#[test]
fn moactl_with_the_flag_and_no_v1_says_so() {
    let r = rig();
    r.flag(&r.root().join("gone/moa"));
    let o = r.run(
        &r.spike.join(MOACTL),
        &["task", "ls"],
        "",
        &[("MOA_DB", os(&r.db))],
    );
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), format!("{NO_V1}\n"));
    assert_eq!(o.stdout, b"");
    assert_eq!(r.calls(), []);
}

#[test]
fn the_launcher_without_the_flag_is_the_script_without_the_block() {
    let r = rig();
    let plain = r.without_block(LAUNCHER);
    for args in [&["--help"][..], &["-x"]] {
        let with = r.run(&r.spike.join(LAUNCHER), args, "", &[]);
        let without = r.run(&plain, args, "", &[]);
        assert_eq!(
            (with.status.code(), &with.stdout, &with.stderr),
            (without.status.code(), &without.stdout, &without.stderr),
            "moa {args:?}"
        );
    }
    assert_eq!(r.calls(), []);
    r.assert_nothing_created();
}

#[test]
fn the_launcher_with_the_flag_forwards_every_argument() {
    let r = rig();
    r.flag(&r.v1.join("moa"));
    let o = r.run(
        &r.spike.join(LAUNCHER),
        &["work", "--account", "acct-a"],
        "",
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let calls = r.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, "moa");
    assert_eq!(calls[0].argv, ["work", "--account", "acct-a"]);
    r.assert_nothing_created();
}

#[test]
fn the_launcher_with_the_flag_and_no_v1_says_so() {
    let r = rig();
    r.flag(&r.root().join("gone/moa"));
    let o = r.run(
        &r.spike.join(LAUNCHER),
        &["work", "--account", "acct-a"],
        "",
        &[],
    );
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), format!("{NO_V1}\n"));
    assert_eq!(o.stdout, b"");
    assert_eq!(r.calls(), []);
    r.assert_nothing_created();
}

#[test]
fn a_flag_naming_the_spikes_own_launcher_is_no_v1() {
    let r = rig();
    r.flag(&r.spike.join(LAUNCHER));
    for (script, args) in [(LAUNCHER, &["work"][..]), (MOACTL, &["task", "ls"])] {
        let o = r.run(&r.spike.join(script), args, "", &[("MOA_DB", os(&r.db))]);
        assert_eq!(o.status.code(), Some(1), "{script}: {o:?}");
        assert_eq!(
            String::from_utf8_lossy(&o.stderr),
            format!("{NO_V1}\n"),
            "{script}"
        );
        assert_eq!(o.stdout, b"", "{script}");
    }
    let before = dump(&r.db);
    let store = r.home.join(".claude-acct-a");
    for payload in turn(ids::S3_WAITING, &r.home.join("private/lantern")) {
        let o = r.hook(&r.spike.join(TRACK), &payload, &tracked(&r.db, &store));
        assert_eq!(o.status.code(), Some(0), "{o:?}");
        assert_silent(&o);
    }
    assert_eq!(dump(&r.db), before);
    assert_eq!(r.calls(), []);
    r.assert_nothing_created();
}
