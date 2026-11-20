//! Folders as people write them.

use std::path::{Path, PathBuf};

/// `path` with the folder `home` written `~`. Whole folders only: `/h` and `/h/x` are under
/// `/h`, `/hx` isn't. The rest of the path is kept exactly, a trailing `/` included. An
/// empty `home` changes nothing, and so does `/` (no home).
pub fn tilde(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    }
}

/// The folder a typed answer names, checked as `n` checks it (V7): it starts with `~` or `/`,
/// and `is_dir` says it is a folder. Nothing is made. Which brain it belongs to is `open`'s
/// to judge.
pub fn typed_folder(
    typed: &str,
    home: &Path,
    is_dir: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    let missing = || format!("no folder {typed}");
    let path = if typed.starts_with('/') {
        PathBuf::from(typed)
    } else if typed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home.join(rest)
    } else if typed.starts_with('~') {
        // Another user's home: not one moa can name.
        return Err(missing());
    } else {
        return Err("a folder starts with ~ or /".to_string());
    };
    if is_dir(&path) {
        Ok(path)
    } else {
        Err(missing())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_matches_whole_folders() {
        let h = Path::new("/h");
        for (path, want) in [
            ("/h", "~"),
            ("/h/", "~/"),
            ("/h/x/", "~/x/"),
            ("/h/x", "~/x"),
            ("/hx", "/hx"),
            ("/other/h", "/other/h"),
        ] {
            assert_eq!(tilde(path, h), want, "{path}");
        }
        assert_eq!(tilde("/h/x", Path::new("")), "/h/x");
        assert_eq!(tilde("/h/x", Path::new("/h/")), "~/x");
        assert_eq!(tilde("/x", Path::new("/")), "/x");
    }

    #[test]
    fn typed_folder_needs_tilde_or_slash_and_a_folder() {
        let h = Path::new("/h");
        let all = |_: &Path| true;
        assert_eq!(typed_folder("~", h, all), Ok(PathBuf::from("/h")));
        assert_eq!(typed_folder("~/x/", h, all), Ok(PathBuf::from("/h/x/")));
        assert_eq!(typed_folder("/y", h, all), Ok(PathBuf::from("/y")));
        let refused = "a folder starts with ~ or /".to_string();
        assert_eq!(typed_folder("x", h, all), Err(refused.clone()));
        assert_eq!(typed_folder("", h, all), Err(refused));
        assert_eq!(
            typed_folder("~bob/x", h, all),
            Err("no folder ~bob/x".into())
        );
        let none = |_: &Path| false;
        assert_eq!(typed_folder("~/x", h, none), Err("no folder ~/x".into()));
    }
}
