//! tmux's version string (`#{version}`, `tmux -V`'s word), asked one way everywhere.

/// Whether `version` is one of `releases` (`"3.4"`): that release, a patch of it (`3.4a`),
/// or the work leading to it (`next-3.4`). `3.40` isn't `3.4`.
pub fn is(version: &str, releases: &[&str]) -> bool {
    let version = version.strip_prefix("next-").unwrap_or(version);
    releases.iter().any(|r| {
        version
            .strip_prefix(r)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.'))
    })
}

#[cfg(test)]
mod tests {
    use super::is;

    #[test]
    fn a_release_its_patches_and_its_next_are_it() {
        for v in ["3.4", "3.4a", "next-3.4", "3.5", "3.5a", "next-3.5"] {
            assert!(is(v, &["3.4", "3.5"]), "{v}");
        }
        for v in [
            "3.3",
            "3.6",
            "3.40",
            "3.4.1",
            "next-3.6",
            "master",
            "",
            "openbsd-7.4",
        ] {
            assert!(!is(v, &["3.4", "3.5"]), "{v}");
        }
        assert!(!is("3.4", &[]));
    }
}
