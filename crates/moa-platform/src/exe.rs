//! The path moa was run as, and the chain of symlinks under it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

/// The path this process was run as, with its symlinks kept, so a daemon started through a
/// profile link keeps following that link: `argv[0]` when it names a path (made absolute
/// against the cwd), else the first executable of that name on `PATH`, else
/// `current_exe()`.
pub fn invoked_exe() -> PathBuf {
    let cwd = std::env::current_dir().ok();
    let path = std::env::var_os("PATH");
    std::env::args_os()
        .next()
        .and_then(|argv0| invoked(&argv0, cwd.as_deref(), path.as_deref()))
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_default()
}

/// `invoked_exe` with its inputs passed in. `None` when `argv0` is empty, is relative with
/// no `cwd`, or names nothing on `path`.
fn invoked(argv0: &OsStr, cwd: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    if argv0.is_empty() {
        return None;
    }
    if !argv0.as_bytes().contains(&b'/') {
        return crate::which(argv0.to_str()?, path);
    }
    let argv0 = Path::new(argv0);
    let joined = if argv0.is_absolute() {
        argv0.to_path_buf()
    } else {
        cwd?.join(argv0)
    };
    // `.` steps go; `..` stays, as it may cross a symlink.
    Some(
        joined
            .components()
            .filter(|c| *c != Component::CurDir)
            .collect(),
    )
}

/// Every entry on the way from `exe` (an absolute path) to the file it names, as
/// `(directory, name)`: `exe` itself, each symlink it goes through, and the final file. A
/// relative link target is read against its link's directory. A missing entry ends the
/// chain but is still listed, so its creation can be watched; a loop ends at its first
/// repeat.
pub fn exe_chain(exe: &Path) -> Vec<(PathBuf, OsString)> {
    let mut chain: Vec<(PathBuf, OsString)> = Vec::new();
    let mut at = exe.to_path_buf();
    while let (Some(dir), Some(name)) = (at.parent(), at.file_name()) {
        let entry = (dir.to_path_buf(), name.to_os_string());
        if chain.contains(&entry) {
            break;
        }
        chain.push(entry);
        match std::fs::read_link(&at) {
            Ok(target) => at = dir.join(target),
            Err(_) => break,
        }
    }
    chain
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn entry(dir: &Path, name: &str) -> (PathBuf, OsString) {
        (dir.to_path_buf(), name.into())
    }

    #[test]
    fn a_relative_argv0_is_made_absolute_against_the_cwd() {
        let got = invoked(OsStr::new("./bin/moa"), Some(Path::new("/tmp/x")), None).unwrap();
        assert_eq!(got.as_os_str(), "/tmp/x/bin/moa");
        let got = invoked(OsStr::new("/opt/moa/bin/moa"), None, None).unwrap();
        assert_eq!(got.as_os_str(), "/opt/moa/bin/moa");
    }

    #[test]
    fn a_bare_name_is_found_on_path_and_its_symlink_kept() {
        let t = tmp();
        let (bin, real) = (t.path().join("bin"), t.path().join("real"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("moa"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(real.join("moa"), std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../real/moa", bin.join("moa")).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        let got = invoked(OsStr::new("moa"), Some(t.path()), Some(&path));
        assert_eq!(got, Some(bin.join("moa")));
        assert_eq!(
            invoked(OsStr::new("other"), Some(t.path()), Some(&path)),
            None
        );
    }

    #[test]
    fn the_chain_lists_each_link_and_the_file() {
        let t = tmp();
        let (d1, d2, d3) = (
            t.path().join("d1"),
            t.path().join("d2"),
            t.path().join("d3"),
        );
        for d in [&d1, &d2, &d3] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(d3.join("file"), "x").unwrap();
        symlink(d3.join("file"), d2.join("b")).unwrap();
        // A relative target is read against its link's directory.
        symlink("../d2/b", d1.join("a")).unwrap();
        let chain = exe_chain(&d1.join("a"));
        assert_eq!(chain.len(), 3, "{chain:?}");
        assert_eq!(chain[0], entry(&d1, "a"));
        assert_eq!(chain[1], entry(&d1.join("../d2"), "b"));
        assert_eq!(chain[2], entry(&d3, "file"));
        assert_eq!(exe_chain(&d3.join("file")), vec![entry(&d3, "file")]);
    }

    #[test]
    fn a_missing_file_or_a_loop_ends_the_chain() {
        let t = tmp();
        let d = t.path();
        symlink(d.join("gone"), d.join("dangling")).unwrap();
        assert_eq!(
            exe_chain(&d.join("dangling")),
            vec![entry(d, "dangling"), entry(d, "gone")]
        );
        symlink(d.join("y"), d.join("x")).unwrap();
        symlink(d.join("x"), d.join("y")).unwrap();
        assert_eq!(exe_chain(&d.join("x")), vec![entry(d, "x"), entry(d, "y")]);
    }
}
