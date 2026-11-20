//! Folders as people write them.

use std::path::Path;

/// `path` with the folder `home` written `~`. Whole folders only: `/h` and `/h/x` are under
/// `/h`, `/hx` isn't. The rest of the path is kept exactly, a trailing `/` included. An
/// empty `home` changes nothing.
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
    }
}
