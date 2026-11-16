//! Local only: runs the data verbs through a real daemon on a copy of a real spike brain, and
//! checks that its writes survive a reload. It returns at once unless `MOA_LOCAL_BRAIN_COPY`
//! is set; `MOA_LOCAL_BRAIN_TZ` and `MOA_LOCAL_BRAIN_ACCOUNTS` (`NAME=STORE,...`) say how to
//! read it, as in slice 1's local test. It prints only each verb's exit code and line count,
//! never a record's contents.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_client::{Client, ClientOptions, OnMismatch};
use moa_core::config::BrainName;
use moa_core::effect::handle_checked;
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, Deps};
use moa_core::{Input, Now, State, UtcMs};
use moa_platform::build_id_of;
use moa_proto::snapshot_of;
use moa_store::Store;
use moa_testkit::TempBrain;

const MOA: &str = env!("CARGO_BIN_EXE_moa");

fn zone(s: &str) -> TimeZone {
    TimeZone::get(s)
        .or_else(|_| TimeZone::posix(s))
        .expect("MOA_LOCAL_BRAIN_TZ is not a time zone")
}

struct Rig {
    tb: TempBrain,
    tz_spec: String,
    log: Vec<(String, i32, usize)>,
}

impl Rig {
    fn run(&mut self, exe: &Path, args: &[&str]) -> Output {
        let o = Command::new(exe)
            .args(args)
            .env_clear()
            .envs(self.tb.env())
            .env("TZ", &self.tz_spec)
            .current_dir(self.tb.root())
            .output()
            .unwrap();
        // Only the exit code and the number of lines are kept, never the output itself.
        let label = format!("{} {}", exe.file_name().unwrap().to_string_lossy(), args[0]);
        self.log.push((
            label,
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).lines().count(),
        ));
        o
    }

    fn moactl(&mut self, args: &[&str]) -> Output {
        let exe = self.tb.root().join("bin/moactl");
        self.run(&exe, args)
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The `id` of a `--json` reply.
fn id_of(o: &Output) -> String {
    let v: serde_json::Value = serde_json::from_str(&stdout(o)).expect("a JSON reply");
    match &v["id"] {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => panic!("the reply has no id"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_verbs_work_and_survive_a_reload() {
    let Some(copy) = std::env::var_os("MOA_LOCAL_BRAIN_COPY").map(PathBuf::from) else {
        return;
    };
    let tz_spec = std::env::var("MOA_LOCAL_BRAIN_TZ").expect("set MOA_LOCAL_BRAIN_TZ");
    let tz = zone(&tz_spec);
    let accounts = std::env::var("MOA_LOCAL_BRAIN_ACCOUNTS").unwrap_or_default();

    let tb = TempBrain::new(None);
    fs::create_dir_all(tb.root().join("bin")).unwrap();
    symlink(MOA, tb.root().join("bin/moactl")).unwrap();
    let mut r = Rig {
        tb,
        tz_spec: tz_spec.clone(),
        log: vec![],
    };

    // 1. The copy becomes the brain, with no folder made beforehand.
    let db = r.tb.db("default");
    let mut adopt: Vec<String> = vec![
        "adopt-spike".into(),
        "--from".into(),
        copy.to_str().unwrap().into(),
        "--into".into(),
        db.to_str().unwrap().into(),
        "--tz".into(),
        tz_spec.clone(),
    ];
    for pair in accounts.split(',').filter(|p| !p.is_empty()) {
        adopt.push("--account".into());
        adopt.push(pair.into());
    }
    let adopt: Vec<&str> = adopt.iter().map(String::as_str).collect();
    let o = r.run(Path::new(MOA), &adopt);
    assert_eq!(o.status.code(), Some(0), "adopt-spike failed");

    // 2. Reads.
    let o = r.run(
        Path::new(MOA),
        &["sql", "select name from projects order by id limit 1"],
    );
    assert_eq!(o.status.code(), Some(0));
    let text = stdout(&o);
    let project = text
        .lines()
        .nth(1)
        .expect("the brain has a project")
        .to_string();
    for args in [
        vec!["brief"],
        vec!["attention"],
        vec!["ls"],
        vec!["ls", "--all"],
        vec!["task", "ls"],
        vec!["task", "ls", "--all"],
        vec!["suggestion", "ls"],
        vec!["find", "a"],
        vec!["project", &project],
    ] {
        r.moactl(&args);
    }

    // 3. Writes, on the copy.
    let o = r.moactl(&[
        "task",
        "add",
        &project,
        "moa-local-check",
        "--due",
        "+1",
        "--json",
    ]);
    let task = id_of(&o);
    r.moactl(&["task", "set", &task, "--status", "done"]);
    r.moactl(&["note", "moa-local-check", "--project", &project]);
    let o = r.moactl(&["suggest", "moa-local-check", "--json"]);
    let suggestion = id_of(&o);
    r.moactl(&["suggestion", "no", &suggestion]);
    r.moactl(&["group", "add", "moa-local-check"]);
    r.moactl(&["group", "rm", "moa-local-check"]);

    // 4. Every topic's snapshot, then stop the daemon.
    let paths = r.tb.paths();
    let mut client = Client::connect(ClientOptions {
        paths: paths.clone(),
        brain: BrainName::default_brain(),
        client: "local-verbs",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: OnMismatch::Restart,
        build: build_id_of(Path::new(MOA)).expect("a build id"),
    })
    .await
    .expect("the daemon is running");
    let kept = client.subscribe(Topic::ALL).await.expect("a snapshot");
    drop(client);
    let o = r.run(Path::new(MOA), &["shutdown"]);
    assert_eq!(o.status.code(), Some(0));

    // 5. A fresh state from the file equals what the daemon showed.
    let store = Store::open(&db, &paths.backups("default")).expect("the brain opens");
    let snapshot = store.load_snapshot().expect("the snapshot loads");
    let deps = Deps {
        parse_action: Some(moa_proto::cli::parse_action),
        canonical: moa_platform::canonical,
        home: paths.home().to_path_buf(),
    };
    let mut config = moa_daemon::load_config(&paths).expect("a config");
    config.canonicalize_with(deps.canonical);
    let mut state = State::new(BrainName::default_brain(), config).with_deps(deps);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let now = Now::at(UtcMs(now_ms), Duration::ZERO, tz);
    let _ = handle_checked(&mut state, Input::Startup(Box::new(snapshot)), now);
    let reloaded = snapshot_of(&state.model(), Topic::ALL);
    let differing: Vec<String> = Topic::ALL
        .iter()
        .filter(|t| kept.topics.get(t) != reloaded.topics.get(t))
        .map(|t| format!("{t:?}"))
        .collect();

    // 6. Counts and exit codes only.
    for (label, code, lines) in &r.log {
        println!("{label:<24} exit {code}, {lines} lines");
    }
    assert!(
        r.log.iter().all(|(_, code, _)| *code == 0),
        "a verb exited non-zero"
    );
    assert!(
        differing.is_empty(),
        "the reload differs in topics: {differing:?}"
    );
    println!("reload matches on {} topics", Topic::ALL.len());
}
