use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_platform::{PathWatch, exe_chain};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap()
}

fn entry(dir: &Path, name: &str) -> (PathBuf, OsString) {
    (dir.to_path_buf(), name.into())
}

/// Replaces `dir/name` the way an install does: a new file beside it, renamed over it.
fn rename_over(dir: &Path, name: &str, body: &str) {
    let new = dir.join(format!("{name}.new"));
    std::fs::write(&new, body).unwrap();
    std::fs::rename(new, dir.join(name)).unwrap();
}

/// Points the symlink `dir/name` at `target`, as `ln -sfn` does.
fn retarget(dir: &Path, name: &str, target: &Path) {
    let new = dir.join(format!("{name}.new"));
    symlink(target, &new).unwrap();
    std::fs::rename(new, dir.join(name)).unwrap();
}

async fn fires(w: &mut PathWatch) {
    timeout(LIMIT, w.changed())
        .await
        .expect("the watch fired")
        .unwrap();
}

async fn stays_quiet(w: &mut PathWatch) {
    assert!(
        timeout(QUIET, w.changed()).await.is_err(),
        "the watch fired with nothing changed"
    );
}

#[tokio::test]
async fn a_rename_over_fires() {
    let t = tmp();
    std::fs::write(t.path().join("moa"), "old").unwrap();
    let mut w = PathWatch::new(&[entry(t.path(), "moa")]).unwrap();
    rename_over(t.path(), "moa", "new");
    fires(&mut w).await;
}

#[tokio::test]
async fn a_symlink_retarget_fires() {
    let t = tmp();
    // Resolved, so the chain is only these links (`/tmp` is itself one on macOS).
    let root = moa_platform::canonical(t.path());
    let (d1, d2, d3) = (root.join("d1"), root.join("d2"), root.join("d3"));
    for d in [&d1, &d2, &d3] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::write(d3.join("moa-1"), "one").unwrap();
    std::fs::write(d3.join("moa-2"), "two").unwrap();
    symlink(d3.join("moa-1"), d2.join("moa")).unwrap();
    symlink(d2.join("moa"), d1.join("moa")).unwrap();
    let chain = exe_chain(&d1.join("moa"));
    assert_eq!(chain.len(), 3, "{chain:?}");
    let mut w = PathWatch::new(&chain).unwrap();

    // The link under the watched one, as a profile switch moves it.
    retarget(&d2, "moa", &d3.join("moa-2"));
    fires(&mut w).await;
    // And the watched link itself, still watched after the first change.
    retarget(&d1, "moa", &d3.join("moa-1"));
    fires(&mut w).await;
}

/// An install that swaps the program's whole folder: the old one renamed away, a new one
/// renamed into its place.
#[tokio::test]
async fn a_swapped_directory_fires() {
    let t = tmp();
    for d in ["bin", "bin.new"] {
        std::fs::create_dir(t.path().join(d)).unwrap();
        std::fs::write(t.path().join(d).join("moa"), d).unwrap();
    }
    let bin = t.path().join("bin");
    let mut w = PathWatch::new(&exe_chain(&bin.join("moa"))).unwrap();
    std::fs::rename(&bin, t.path().join("bin.old")).unwrap();
    std::fs::rename(t.path().join("bin.new"), &bin).unwrap();
    fires(&mut w).await;
}

#[tokio::test]
async fn a_directory_link_retarget_fires() {
    let t = tmp();
    for g in ["gen1", "gen2"] {
        std::fs::create_dir_all(t.path().join(g).join("bin")).unwrap();
        std::fs::write(t.path().join(g).join("bin/moa"), g).unwrap();
    }
    let d1 = t.path().join("d1");
    std::fs::create_dir(&d1).unwrap();
    symlink(t.path().join("gen1"), d1.join("profile")).unwrap();
    let mut w = PathWatch::new(&exe_chain(&d1.join("profile/bin/moa"))).unwrap();

    // A profile switch: the directory link moves to the next generation.
    retarget(&d1, "profile", &t.path().join("gen2"));
    fires(&mut w).await;
}

#[tokio::test]
async fn an_in_place_write_fires() {
    let t = tmp();
    std::fs::write(t.path().join("moa"), "old").unwrap();
    let mut w = PathWatch::new(&[entry(t.path(), "moa")]).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(t.path().join("moa"))
        .unwrap();
    f.write_all(b" and more").unwrap();
    drop(f);
    fires(&mut w).await;
}

#[tokio::test]
async fn an_unrelated_entry_doesnt_fire() {
    let t = tmp();
    std::fs::write(t.path().join("moa"), "old").unwrap();
    let mut w = PathWatch::new(&[entry(t.path(), "moa")]).unwrap();
    std::fs::write(t.path().join("other"), "x").unwrap();
    rename_over(t.path(), "other", "y");
    std::fs::remove_file(t.path().join("other")).unwrap();
    stays_quiet(&mut w).await;
}

#[tokio::test]
async fn changes_coalesce() {
    let t = tmp();
    std::fs::write(t.path().join("moa"), "old").unwrap();
    let mut w = PathWatch::new(&[entry(t.path(), "moa")]).unwrap();
    rename_over(t.path(), "moa", "one");
    rename_over(t.path(), "moa", "two");
    std::fs::write(t.path().join("moa"), "three").unwrap();
    fires(&mut w).await;
    stays_quiet(&mut w).await;
    // Coalescing never swallows a later change.
    rename_over(t.path(), "moa", "four");
    fires(&mut w).await;
}
