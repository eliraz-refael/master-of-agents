//! Small file helpers.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Writes `text` to `path` through a temp file and a rename, mode 0600. `false` when the file
/// already holds exactly `text`.
pub fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(false);
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_if_changed_writes_once_and_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moa.tmux");
        assert!(write_if_changed(&path, "a\n").unwrap());
        assert!(!write_if_changed(&path, "a\n").unwrap());
        assert!(write_if_changed(&path, "b\n").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "a temp file was left: {names:?}");
    }
}
