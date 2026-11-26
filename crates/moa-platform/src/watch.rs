//! A watch on a few directory entries: an executable's symlink chain, or config files.
//!
//! Each entry is a `(dir, name)` pair. The watch hears it created, removed, renamed over,
//! retargeted (a symlink) or written in place, and nothing else in its directory. Kernel
//! events only, never polling: kqueue on macOS, inotify on Linux.

use std::ffi::OsString;
use std::io;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::{collections::BTreeSet, os::fd::OwnedFd};

use tokio::io::unix::AsyncFd;

use crate::sys;

/// Resolves when one of a few directory entries may have changed.
#[derive(Debug)]
pub struct PathWatch {
    #[cfg(target_os = "macos")]
    kq: AsyncFd<OwnedFd>,
    #[cfg(target_os = "macos")]
    entries: Vec<(PathBuf, OsString)>,
    /// The registered directories and entries; dropping one ends its registration.
    #[cfg(target_os = "macos")]
    armed: Vec<OwnedFd>,
    /// Each entry as it was when last looked at, to tell its own change from a neighbour's.
    #[cfg(target_os = "macos")]
    seen: Vec<Option<Stamp>>,
    #[cfg(target_os = "linux")]
    fd: AsyncFd<std::os::fd::OwnedFd>,
    /// Each entry's directory as its watch descriptor, and its name.
    #[cfg(target_os = "linux")]
    names: Vec<(i32, OsString)>,
}

impl PathWatch {
    /// Watch each `(dir, name)` entry. A missing entry is watched for its creation, but
    /// each directory must exist. Must be called inside a tokio runtime.
    pub fn new(entries: &[(PathBuf, OsString)]) -> io::Result<PathWatch> {
        #[cfg(target_os = "macos")]
        {
            let kq = sys::register_readable(sys::vnode_queue()?)?;
            let armed = arm(kq.get_ref(), entries)?;
            Ok(PathWatch {
                kq,
                entries: entries.to_vec(),
                armed,
                seen: stamps(entries),
            })
        }
        #[cfg(target_os = "linux")]
        {
            let fd = sys::inotify_new()?;
            let names = entries
                .iter()
                .map(|(dir, name)| Ok((sys::inotify_watch(&fd, dir)?, name.clone())))
                .collect::<io::Result<_>>()?;
            Ok(PathWatch {
                fd: sys::register_readable(fd)?,
                names,
            })
        }
    }

    /// Resolves after one or more changes since the last call. Cancel-safe.
    pub async fn changed(&mut self) -> io::Result<()> {
        loop {
            #[cfg(target_os = "macos")]
            {
                let mut guard = self.kq.readable().await?;
                let woke = sys::vnode_drain(self.kq.get_ref());
                // Drained to empty, so the readiness is spent either way.
                guard.clear_ready();
                if !woke? {
                    continue;
                }
                // A renamed-over entry is a new file: watch that one from here. The new
                // registrations come before the look, so a change between them is seen
                // either by the look or by the next event.
                self.armed = arm(self.kq.get_ref(), &self.entries)?;
                let now = stamps(&self.entries);
                if now != self.seen {
                    self.seen = now;
                    return Ok(());
                }
            }
            #[cfg(target_os = "linux")]
            {
                let mut guard = self.fd.readable().await?;
                let names = &self.names;
                let hit = sys::inotify_drain(self.fd.get_ref(), |wd, name| {
                    names.iter().any(|(w, n)| *w == wd && n == name)
                });
                // Drained to WouldBlock, so the readiness is spent either way.
                guard.clear_ready();
                if hit? {
                    return Ok(());
                }
            }
        }
    }
}

/// Registers each distinct directory, and each entry that exists, on `kq`.
#[cfg(target_os = "macos")]
fn arm(kq: &OwnedFd, entries: &[(PathBuf, OsString)]) -> io::Result<Vec<OwnedFd>> {
    let dirs: BTreeSet<&PathBuf> = entries.iter().map(|(dir, _)| dir).collect();
    let mut armed = dirs
        .into_iter()
        .map(|dir| sys::vnode_watch_dir(kq, dir))
        .collect::<io::Result<Vec<_>>>()?;
    // A missing entry is heard through its directory when it appears.
    armed.extend(
        entries
            .iter()
            .filter_map(|(dir, name)| sys::vnode_watch_entry(kq, &dir.join(name)).ok()),
    );
    Ok(armed)
}

/// What tells one state of an entry from another: which file it is, and its size, mode
/// and change times. A rename over it, a retarget or a write in place each changes one.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

#[cfg(target_os = "macos")]
fn stamps(entries: &[(PathBuf, OsString)]) -> Vec<Option<Stamp>> {
    entries
        .iter()
        .map(|(dir, name)| {
            let m = std::fs::symlink_metadata(dir.join(name)).ok()?;
            Some(Stamp {
                dev: m.dev(),
                ino: m.ino(),
                mode: m.mode(),
                size: m.size(),
                mtime: (m.mtime(), m.mtime_nsec()),
                ctime: (m.ctime(), m.ctime_nsec()),
            })
        })
        .collect()
}
