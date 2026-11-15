//! Backups made with `VACUUM INTO`, never with `cp` (most of a brain's data can live in
//! its `-wal`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::{StoreError, create_private_dirs};

/// Writes a consistent copy of `conn`'s database to `dest`.
///
/// Refuses, before running any SQL, a `dest` that exists or that has a leftover `-wal`,
/// `-shm` or `-journal` beside it. Missing parent directories are created with mode 0700.
pub fn vacuum_into(conn: &Connection, dest: &Path) -> Result<(), StoreError> {
    for path in occupied_names(dest) {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            )));
        }
    }
    if let Some(parent) = dest.parent() {
        create_private_dirs(parent)?;
    }
    let dest = dest.to_str().ok_or_else(|| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the backup path is not valid text",
        ))
    })?;
    conn.execute("VACUUM INTO ?1", [dest])?;
    Ok(())
}

fn occupied_names(dest: &Path) -> Vec<PathBuf> {
    let mut names = vec![dest.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut s = dest.as_os_str().to_owned();
        s.push(suffix);
        names.push(PathBuf::from(s));
    }
    names
}
