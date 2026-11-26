//! The path moa was run as, and the chain of symlinks under it.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

/// The most symlinks [`exe_chain`] follows, as the kernel's `SYMLOOP_MAX`.
const MAX_LINKS: usize = 40;

/// The path this process was run as, with its symlinks kept, so a daemon started through a
/// profile link keeps following that link: `argv[0]` when it names a path (made absolute
/// against the cwd), else the first executable of that name on `PATH` if that is this
/// binary, else `current_exe()`.
pub fn invoked_exe() -> PathBuf {
    let cwd = std::env::current_dir().ok();
    let path = std::env::var_os("PATH");
    let current = std::env::current_exe().ok();
    std::env::args_os()
        .next()
        .and_then(|argv0| invoked(&argv0, cwd.as_deref(), path.as_deref(), current.as_deref()))
        .or(current)
        .unwrap_or_default()
}

/// `invoked_exe` with its inputs passed in (`current` is `current_exe()`). `None` when
/// `argv0` is empty, is relative with no `cwd`, or names nothing on `path`.
fn invoked(
    argv0: &OsStr,
    cwd: Option<&Path>,
    path: Option<&OsStr>,
    current: Option<&Path>,
) -> Option<PathBuf> {
    if argv0.is_empty() {
        return None;
    }
    if !argv0.as_bytes().contains(&b'/') {
        let found = crate::which(argv0.to_str()?, path)?;
        // A bare name is only what the starter passed: the first one on PATH may be
        // another install than the one running.
        return match current {
            Some(c) if crate::canonical(&found) != crate::canonical(c) => Some(c.to_path_buf()),
            _ => Some(found),
        };
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

/// Every entry whose change would change what `exe` (an absolute path) names, as
/// `(directory, name)`: each symlink met while resolving it, a directory link mid-path
/// included (a profile link), then the final file. The walk goes one component at a time,
/// as `realpath` does, so each directory listed is free of links. A missing entry ends
/// the chain but is still listed, so its creation can be watched. A loop ends at its first
/// repeat, and the walk at `MAX_LINKS` links.
pub fn exe_chain(exe: &Path) -> Vec<(PathBuf, OsString)> {
    let mut chain: Vec<(PathBuf, OsString)> = Vec::new();
    // The resolved part, and what is still to walk.
    let mut done = PathBuf::new();
    let mut rest: VecDeque<OsString> = steps(exe);
    while let Some(step) = rest.pop_front() {
        match step.to_str() {
            Some("/") => done = PathBuf::from("/"),
            Some(".") => {}
            Some("..") => {
                // `done` has no links in it, so stepping up is plain.
                done.pop();
            }
            _ => {
                let at = done.join(&step);
                let entry = (done.clone(), step);
                let Ok(meta) = std::fs::symlink_metadata(&at) else {
                    chain.push(entry);
                    break;
                };
                if !meta.file_type().is_symlink() {
                    if rest.is_empty() {
                        chain.push(entry);
                    }
                    done = at;
                    continue;
                }
                if chain.contains(&entry) || chain.len() >= MAX_LINKS {
                    break;
                }
                chain.push(entry);
                let Ok(target) = std::fs::read_link(&at) else {
                    break;
                };
                // The target goes in the link's place; a relative one starts at `done`.
                for s in steps(&target).into_iter().rev() {
                    rest.push_front(s);
                }
            }
        }
    }
    chain
}

/// `p`'s components as strings to walk: `/` for the root, `.` and `..` as themselves.
fn steps(p: &Path) -> VecDeque<OsString> {
    p.components()
        .map(|c| c.as_os_str().to_os_string())
        .collect()
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
        let got = invoked(
            OsStr::new("./bin/moa"),
            Some(Path::new("/tmp/x")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(got.as_os_str(), "/tmp/x/bin/moa");
        let got = invoked(OsStr::new("/opt/moa/bin/moa"), None, None, None).unwrap();
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
        let found = |current: &Path| invoked(OsStr::new("moa"), None, Some(&path), Some(current));
        // The link on PATH is this binary: it is kept, not its target.
        assert_eq!(found(&real.join("moa")), Some(bin.join("moa")));
        assert_eq!(
            invoked(OsStr::new("moa"), None, Some(&path), None),
            Some(bin.join("moa"))
        );
        assert_eq!(invoked(OsStr::new("other"), None, Some(&path), None), None);
    }

    #[test]
    fn a_bare_name_on_path_from_another_install_gives_the_running_binary() {
        let t = tmp();
        let (bin, elsewhere) = (t.path().join("bin"), t.path().join("elsewhere"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        for f in [bin.join("moa"), elsewhere.join("moa")] {
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([&bin]).unwrap();
        let current = elsewhere.join("moa");
        assert_eq!(
            invoked(OsStr::new("moa"), None, Some(&path), Some(&current)),
            Some(current)
        );
    }

    #[test]
    fn the_chain_lists_each_link_and_the_file() {
        let t = tmp();
        // Resolved, as the chain's directories are (`/tmp` is itself a link on macOS).
        let root = crate::canonical(t.path());
        let (d1, d2, d3) = (root.join("d1"), root.join("d2"), root.join("d3"));
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
        assert_eq!(chain[1], entry(&d2, "b"));
        assert_eq!(chain[2], entry(&d3, "file"));
        assert_eq!(exe_chain(&d3.join("file")), vec![entry(&d3, "file")]);
    }

    #[test]
    fn a_directory_link_in_the_path_is_in_the_chain() {
        let t = tmp();
        let root = crate::canonical(t.path());
        std::fs::create_dir_all(root.join("gen1/bin")).unwrap();
        std::fs::create_dir(root.join("d1")).unwrap();
        std::fs::write(root.join("gen1/bin/moa"), "x").unwrap();
        // A profile: a link to a generation, with the binary under it.
        symlink("../gen1", root.join("d1/profile")).unwrap();
        assert_eq!(
            exe_chain(&root.join("d1/profile/bin/moa")),
            vec![
                entry(&root.join("d1"), "profile"),
                entry(&root.join("gen1/bin"), "moa"),
            ]
        );
    }

    #[test]
    fn a_missing_file_or_a_loop_ends_the_chain() {
        let t = tmp();
        let root = crate::canonical(t.path());
        let d = root.as_path();
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
