use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use moa_platform::which;

fn file(dir: &Path, name: &str, mode: u32) {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn finds_the_first_executable_file() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    file(a.path(), "tool", 0o644);
    file(b.path(), "tool", 0o755);
    let path = path_of(&[a.path(), b.path()]);
    assert_eq!(which("tool", Some(&path)), Some(b.path().join("tool")));
    file(a.path(), "tool", 0o700);
    assert_eq!(which("tool", Some(&path)), Some(a.path().join("tool")));
}

#[test]
fn skips_directories_missing_names_and_relative_entries() {
    let a = tempfile::tempdir().unwrap();
    std::fs::create_dir(a.path().join("tool")).unwrap();
    let path = path_of(&[a.path()]);
    assert_eq!(which("tool", Some(&path)), None);
    assert_eq!(which("other", Some(&path)), None);
    assert_eq!(which("tool", None), None);
    // A relative or empty entry is never searched, even if it holds the file.
    let b = tempfile::tempdir().unwrap();
    file(b.path(), "tool", 0o755);
    let cwd = std::env::current_dir().unwrap();
    let up: PathBuf = cwd.components().map(|_| "..").collect();
    let rel = up.join(b.path().strip_prefix("/").unwrap());
    assert_eq!(which("tool", Some(rel.as_os_str())), None);
    assert_eq!(which("tool", Some("".as_ref())), None);
}
