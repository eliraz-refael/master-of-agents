//! Backups made with `VACUUM INTO`, never with `cp` (most of a brain's data can live in
//! its `-wal`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::civil::Date;
use rusqlite::Connection;

use crate::{Store, StoreError, create_private_dirs, file_name_of, sidecars};

/// Writes a consistent copy of `conn`'s database to `dest`.
///
/// Refuses, before running any SQL, a `dest` that exists or that has a leftover `-wal`,
/// `-shm` or `-journal` beside it. Missing parent directories are created with mode 0700.
pub fn vacuum_into(conn: &Connection, dest: &Path) -> Result<(), StoreError> {
    for path in occupied_names(dest) {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", file_name_of(&path)),
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
    names.extend(sidecars(dest));
    names
}

/// Writes `dir/daily-YYYY-MM-DD.db` unless today's backup already exists.
///
/// Returns the new file's path, or `None` when that day already has one.
pub fn daily_backup(store: &Store, dir: &Path, today: Date) -> Result<Option<PathBuf>, StoreError> {
    let dest = dir.join(daily_name(today));
    if fs::symlink_metadata(&dest).is_ok() {
        return Ok(None);
    }
    // A leftover -wal/-shm/-journal is not "already backed up": vacuum_into reports it.
    vacuum_into(store.conn(), &dest)?;
    Ok(Some(dest))
}

/// The dates to delete: everything but the 7 newest and, of the rest, the newest date
/// in each of the 4 most recent ISO weeks that have a date in the rest.
pub fn backups_to_delete(existing: &[Date]) -> Vec<Date> {
    let mut newest_first = existing.to_vec();
    newest_first.sort_unstable_by(|a, b| b.cmp(a));
    newest_first.dedup();

    let mut keep: Vec<Date> = newest_first.iter().copied().take(7).collect();
    let mut weeks: Vec<(i16, i8)> = Vec::new();
    for d in newest_first.iter().skip(7) {
        let week = d.iso_week_date();
        let key = (week.year(), week.week());
        if weeks.contains(&key) {
            continue;
        }
        if weeks.len() == 4 {
            break;
        }
        weeks.push(key);
        keep.push(*d);
    }
    newest_first.retain(|d| !keep.contains(d));
    newest_first
}

/// Applies [`backups_to_delete`] to the `daily-YYYY-MM-DD.db` files in `dir`, removing
/// them. Every other file is left alone. Returns the removed paths.
pub fn prune_backups(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dated = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(date) = entry.file_name().to_str().and_then(parse_daily_name) {
            dated.push(date);
        }
    }
    let mut removed = Vec::new();
    for date in backups_to_delete(&dated) {
        let path = dir.join(daily_name(date));
        fs::remove_file(&path)?;
        removed.push(path);
    }
    Ok(removed)
}

fn daily_name(date: Date) -> String {
    format!("daily-{date}.db")
}

/// The date in a name that is exactly `daily-YYYY-MM-DD.db`.
fn parse_daily_name(name: &str) -> Option<Date> {
    let text = name.strip_prefix("daily-")?.strip_suffix(".db")?;
    let date: Date = text.parse().ok()?;
    (daily_name(date) == name).then_some(date)
}
