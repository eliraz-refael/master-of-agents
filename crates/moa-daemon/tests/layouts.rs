//! `layout.set`'s write of `layouts.json`: the daemon runs the core's `WriteFile` before the
//! reply, through a temporary file and a rename, and a write that fails refuses the request
//! and reads the file again, so the entry that wasn't saved is never published (L2).

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
use tokio::sync::mpsc::UnboundedReceiver;
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

type ReadText = fn(&Path) -> io::Result<Option<String>>;

/// A core whose `layouts.json` is `file`.
async fn core_with(brain: &TypicalBrain, file: &Path) -> CoreHandle {
    core_reading(brain, file, read_text).await
}

/// A core whose `layouts.json` is `file`, read with `read_text`.
async fn core_reading(brain: &TypicalBrain, file: &Path, read_text: ReadText) -> CoreHandle {
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

type Replies = UnboundedReceiver<(ReqId, Result<Done, OpError>)>;

/// `layout set mine {"panel": "tree"}`, answered, and the connection for anything after.
async fn set_mine(core: &CoreHandle) -> (Result<Done, OpError>, Replies) {
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
    let reply = within(rx.recv()).await.expect("a reply").1;
    (reply, rx)
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

    let (refused, mut rx) = set_mine(&core).await;
    // Writable again before anything can fail, so the folder can be removed.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let refused = refused.expect_err("the write fails");
    assert!(
        refused.0.starts_with("couldn't save layouts.json: "),
        "{}",
        refused.0
    );
    within(core.barrier()).await;
    // One answer: the op's own reply went with its publish.
    assert!(rx.try_recv().is_err());
    // Nothing was said about the entry that wasn't saved: the file is read again, so the
    // core's own model (published now, or with the next change) doesn't hold it either.
    assert!(model.borrow().rev > rev, "the file read again is published");
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

    let done = set_mine(&core).await.0.expect("saved");
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

/// A `layouts.json` that links elsewhere (a dotfiles folder, say) stays a link.
#[tokio::test]
async fn a_linked_file_is_written_through_its_link() {
    let brain = TypicalBrain::new();
    let dir = config_dir(&brain);
    std::fs::create_dir_all(&dir).unwrap();
    let elsewhere = brain.dir.path().join("dotfiles");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("layouts.json");
    std::fs::write(&target, r#"{"layouts": {}}"#).unwrap();
    let file = dir.join("layouts.json");
    std::os::unix::fs::symlink(&target, &file).unwrap();
    let core = core_with(&brain, &file).await;

    set_mine(&core).await.0.expect("saved");
    let link = std::fs::symlink_metadata(&file).unwrap();
    assert!(link.file_type().is_symlink());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(saved, json!({"layouts": {"mine": {"panel": "tree"}}}));
}

/// A link whose target isn't there yet (a relative one, too) makes the target and stays a link.
#[tokio::test]
async fn a_link_to_a_missing_file_makes_its_target() {
    let brain = TypicalBrain::new();
    let dir = config_dir(&brain);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
    let file = dir.join("layouts.json");
    std::os::unix::fs::symlink("dotfiles/layouts.json", &file).unwrap();
    let core = core_with(&brain, &file).await;

    set_mine(&core).await.0.expect("saved");
    let link = std::fs::symlink_metadata(&file).unwrap();
    assert!(link.file_type().is_symlink());
    let target = dir.join("dotfiles/layouts.json");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(saved, json!({"layouts": {"mine": {"panel": "tree"}}}));
}

/// Links that go round in a circle are refused, and nothing is written. (A real read of the
/// file fails first and the core refuses; this core reads no file, so the write is tried.)
#[tokio::test]
async fn a_link_loop_is_refused() {
    let brain = TypicalBrain::new();
    let dir = config_dir(&brain);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("layouts.json");
    std::os::unix::fs::symlink("other.json", &file).unwrap();
    std::os::unix::fs::symlink("layouts.json", dir.join("other.json")).unwrap();
    let core = core_reading(&brain, &file, |_| Ok(None)).await;

    let refused = set_mine(&core).await.0.expect_err("refused");
    assert!(
        refused.0.starts_with("couldn't save layouts.json: "),
        "{}",
        refused.0
    );
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    assert_eq!(names, [file.clone(), dir.join("other.json")]);
    assert!(names.iter().all(|p| {
        std::fs::symlink_metadata(p)
            .unwrap()
            .file_type()
            .is_symlink()
    }));
}
