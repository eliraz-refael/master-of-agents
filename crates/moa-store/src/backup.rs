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
    refuse_taken(occupied_names(dest))?;
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

fn already_exists(path: &Path) -> StoreError {
    StoreError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", file_name_of(path)),
    ))
}

fn refuse_taken(paths: impl IntoIterator<Item = PathBuf>) -> Result<(), StoreError> {
    match paths.into_iter().find(|p| fs::symlink_metadata(p).is_ok()) {
        Some(taken) => Err(already_exists(&taken)),
        None => Ok(()),
    }
}

fn remove_if_there(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Writes `dir/daily-YYYY-MM-DD.db` unless today's backup already exists.
///
/// Returns the new file's path, or `None` when that day already has one. Only a regular
/// file counts as a backup: a symlink there is replaced (the link, never its target), and
/// anything else (a directory) is `AlreadyExists`. The copy is made as `<name>.tmp` and
/// renamed, so a process killed mid-copy leaves no partial file that reads as a backup.
pub fn daily_backup(store: &Store, dir: &Path, today: Date) -> Result<Option<PathBuf>, StoreError> {
    let dest = dir.join(daily_name(today));
    match fs::symlink_metadata(&dest) {
        Ok(m) if m.file_type().is_file() => return Ok(None),
        Ok(m) if m.file_type().is_symlink() => {}
        Ok(_) => return Err(already_exists(&dest)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // A leftover -wal/-shm/-journal beside the day's file is not "already backed up": a
    // fresh file renamed next to a stale journal would have it rolled into it on first open.
    refuse_taken(sidecars(&dest))?;
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    for stale in std::iter::once(tmp.clone()).chain(sidecars(&tmp)) {
        remove_if_there(&stale)?;
    }
    let made = vacuum_into(store.conn(), &tmp).and_then(|()| Ok(fs::rename(&tmp, &dest)?));
    if let Err(e) = made {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
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

/// What [`prune_backups`] did.
#[derive(Debug, Default)]
pub struct Pruned {
    pub removed: Vec<PathBuf>,
    /// Files that couldn't be removed, and why. Pruning went on past each one.
    pub failed: Vec<(PathBuf, io::ErrorKind)>,
}

/// Applies [`backups_to_delete`] to the `daily-YYYY-MM-DD.db` files in `dir`, removing
/// them. Every other file is left alone. Only listing `dir` can fail the call: a file that
/// can't be removed is reported in [`Pruned::failed`] and the rest are still removed.
pub fn prune_backups(dir: &Path) -> io::Result<Pruned> {
    let mut dated = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(date) = entry.file_name().to_str().and_then(parse_daily_name) {
            dated.push(date);
        }
    }
    Ok(remove_dated(dir, &backups_to_delete(&dated)))
}

/// Removes the `daily` file of each date. One that is already gone is skipped quietly.
fn remove_dated(dir: &Path, dates: &[Date]) -> Pruned {
    let mut pruned = Pruned::default();
    for date in dates {
        let path = dir.join(daily_name(*date));
        match fs::remove_file(&path) {
            Ok(()) => pruned.removed.push(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => pruned.failed.push((path, e.kind())),
        }
    }
    pruned
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

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    #[test]
    fn a_backup_someone_else_removed_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let gone = date(2026, 1, 1);
        let there = date(2026, 1, 2);
        fs::write(dir.path().join(daily_name(there)), b"x").unwrap();

        let pruned = remove_dated(dir.path(), &[gone, there]);
        assert_eq!(pruned.removed, [dir.path().join(daily_name(there))]);
        assert!(pruned.failed.is_empty(), "{:?}", pruned.failed);
    }
}
