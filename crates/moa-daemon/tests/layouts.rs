//! `layout.set`'s write of `layouts.json`: the daemon runs the core's `WriteFile` before the
//! reply, through a temporary file and a rename, and a write that fails refuses the request
//! with nothing published (L2).

use std::future::Future;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_core::op::{Done, Op, OpError, ReqId};
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, Deps};
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use serde_json::json;
use tokio::time::timeout;

async fn within<T>(f: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out after 10 s")
}

fn read_text(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A core whose `layouts.json` is `file`.
async fn core_with(brain: &TypicalBrain, file: &Path) -> CoreHandle {
    let time = FakeTime::new(brain.now().wall);
    within(CoreHandle::start(CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps {
            home: brain.home.clone(),
            read_text,
            layouts_file: file.to_path_buf(),
            ..Deps::default()
        },
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 16,
        tmux: None,
        spool: None,
    }))
    .await
    .expect("the core starts")
}

/// `layout set mine {"panel": "tree"}`, answered.
async fn set_mine(core: &CoreHandle) -> Result<Done, OpError> {
    let (conn, mut rx) = core.open_conn();
    core.send(
        conn,
        ReqId(1),
        CallerInfo::default(),
        Op::LayoutSet {
            name: "mine".into(),
            layout: json!({"panel": "tree"}),
        },
    );
    within(rx.recv()).await.expect("a reply").1
}

fn config_dir(brain: &TypicalBrain) -> PathBuf {
    brain.home.join(".config/moa")
}

#[tokio::test]
async fn a_failed_write_refuses_the_request() {
    let brain = TypicalBrain::new();
    let dir = config_dir(&brain);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let file = dir.join("layouts.json");
    let core = core_with(&brain, &file).await;
    let model = core.model();
    let rev = model.borrow().rev;

    let refused = set_mine(&core).await;
    // Writable again before anything can fail, so the folder can be removed.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let refused = refused.expect_err("the write fails");
    assert!(
        refused.0.starts_with("couldn't save layouts.json: "),
        "{}",
        refused.0
    );
    within(core.barrier()).await;
    // Nothing was said about the entry that wasn't saved.
    assert_eq!(model.borrow().rev, rev);
    assert!(!model.borrow().topics[&Topic::Layouts].contains_key("mine"));
    assert!(!file.exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn a_write_replaces_the_file_atomically() {
    let brain = TypicalBrain::new();
    let dir = config_dir(&brain);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("layouts.json");
    std::fs::write(&file, r#"{"layouts": {"zeta": {"panel": "plan"}}}"#).unwrap();
    let before = std::fs::metadata(&file).unwrap().ino();
    let core = core_with(&brain, &file).await;

    let done = set_mine(&core).await.expect("saved");
    assert_eq!(
        done.text,
        "saved layout mine; apply it with: moa layout apply mine"
    );
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({"layouts": {"zeta": {"panel": "plan"}, "mine": {"panel": "tree"}}})
    );
    // A new file renamed over the old one, and no temporary file left beside it.
    assert_ne!(std::fs::metadata(&file).unwrap().ino(), before);
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["layouts.json"]);
    within(core.barrier()).await;
    assert!(core.model().borrow().topics[&Topic::Layouts].contains_key("mine"));
}
