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
    /// The lines were handled: forget them.
    pub fn done(self) -> io::Result<()> {
        std::fs::remove_file(&self.draining)
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
    let text = match std::fs::read_to_string(&draining) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    Ok(Taken {
        lines: text.lines().map(str::to_owned).collect(),
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
