//! The hook spool: where a hook leaves its event when the daemon is down, and how the daemon
//! drains it at start. Appends and the drain exclude each other with the same file lock.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::paths::ensure_private_dir;

/// Lines taken from the spool, plus the `.draining` file that still holds them until
/// `done`.
pub struct Taken {
    pub lines: Vec<String>,
    draining: PathBuf,
}

impl Taken {
    /// The lines were handled: forget them. Nothing to forget (an empty drain never made
    /// the file) is fine.
    pub fn done(self) -> io::Result<()> {
        match std::fs::remove_file(&self.draining) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// `<path>.draining`: where a drain parks lines until they are handled.
pub fn draining(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".draining");
    PathBuf::from(name)
}

/// Appends `line` and a newline to the spool as one write, under the file lock.
pub fn append(path: &Path, line: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent).map_err(|e| io::Error::other(e.to_string()))?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line);
    buf.push(b'\n');
    file.lock()?;
    let written = file.write_all(&buf);
    file.unlock()?;
    written
}

/// Moves the spool's lines into `.draining` and returns every line there, a leftover from a
/// crashed drain first. A hook appending meanwhile lands in the fresh spool, not lost.
pub fn take(path: &Path) -> io::Result<Taken> {
    let draining = draining(path);
    let spool = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => Some(f),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(mut spool) = spool {
        spool.lock()?;
        let moved = move_into(&mut spool, &draining);
        spool.unlock()?;
        moved?;
    }
    let bytes = match std::fs::read(&draining) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    // Lossy on purpose: a garbled line must not wedge every later drain. It fails to parse
    // downstream and is counted there.
    Ok(Taken {
        lines: bytes
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect(),
        draining,
    })
}

/// With the spool locked: appends its bytes to `draining`, syncs, then truncates the spool.
fn move_into(spool: &mut File, draining: &Path) -> io::Result<()> {
    let mut bytes = Vec::new();
    spool.read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(());
    }
    let mut out = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(draining)?;
    out.write_all(&bytes)?;
    out.sync_all()?;
    spool.set_len(0)
}
