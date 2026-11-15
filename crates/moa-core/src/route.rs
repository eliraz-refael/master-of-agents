//! Which brain a folder and account belong to. Pure: callers canonicalise the cwd and, via
//! `Config::canonicalize_with`, the configured folders; this does no IO.

use std::path::{Path, PathBuf};

use crate::config::{AccountName, BrainName, Config};

#[derive(Clone, PartialEq, Debug)]
pub enum Route {
    Brain(BrainName),
    NotAvailable,
}

/// Whether `path` is `folder` or inside it, by whole path components. An empty `folder`
/// and a relative `path` match nothing, so an unset or unresolved entry never grants access.
pub fn is_under(path: &Path, folder: &Path) -> bool {
    !folder.as_os_str().is_empty() && path.is_absolute() && path.starts_with(folder)
}

pub fn is_under_any(path: &Path, folders: &[PathBuf]) -> bool {
    folders.iter().any(|f| is_under(path, f))
}

/// Routes `cwd` for `account`. `home` is the opener's brain, else `default`.
pub fn route(
    config: &Config,
    account: Option<&AccountName>,
    cwd: &Path,
    opener: Option<&BrainName>,
) -> Route {
    let home = || Route::Brain(opener.cloned().unwrap_or_else(BrainName::default_brain));
    let Some(acct) = account.and_then(|a| config.accounts.get(a)) else {
        return home();
    };
    if let Some(b) = &acct.brain {
        return if is_under_any(cwd, &acct.except) {
            home()
        } else {
            Route::Brain(b.clone())
        };
    }
    if acct.only.is_empty() {
        return home();
    }
    if is_under_any(cwd, &acct.only) {
        home()
    } else {
        Route::NotAvailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }
    fn brain(s: &str) -> BrainName {
        BrainName::parse(s).unwrap()
    }
    fn acct(s: &str) -> AccountName {
        AccountName::parse(s).unwrap()
    }
    fn cfg_text(body: &str) -> Config {
        Config::parse(body, Path::new("/h")).unwrap()
    }
    fn cfg() -> Config {
        cfg_text(
            r#"
[brains.work]
[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
brain = "work"
except = ["~/private"]
[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
only = []
"#,
        )
    }

    #[test]
    fn the_spec_table() {
        let c = cfg();
        let a = acct("acct-a");
        let b = acct("acct-b");
        assert_eq!(
            route(&c, Some(&a), p("/h/work/x"), None),
            Route::Brain(brain("work"))
        );
        assert_eq!(
            route(&c, Some(&a), p("/h/private/lantern"), None),
            Route::Brain(brain("default"))
        );
        assert_eq!(
            route(&c, Some(&a), p("/h/private/lantern"), Some(&brain("work"))),
            Route::Brain(brain("work"))
        );
        assert_eq!(
            route(&c, Some(&b), p("/anywhere"), None),
            Route::Brain(brain("default"))
        );
        assert_eq!(
            route(&c, None, p("/h/work/x"), None),
            Route::Brain(brain("default"))
        );
    }

    #[test]
    fn an_unknown_account_routes_home() {
        let c = cfg();
        let ghost = acct("ghost");
        assert_eq!(
            route(&c, Some(&ghost), p("/h/work"), None),
            Route::Brain(brain("default"))
        );
    }

    #[test]
    fn a_lookalike_folder_is_not_the_except_folder() {
        let c = cfg();
        let a = acct("acct-a");
        assert_eq!(
            route(&c, Some(&a), p("/h/private2/x"), None),
            Route::Brain(brain("work"))
        );
    }

    #[test]
    fn only_routes_home_inside_and_nowhere_outside() {
        let c = cfg_text(
            "[accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.claude-b\"\nonly = [\"~/private\"]\n",
        );
        let b = acct("acct-b");
        assert_eq!(
            route(&c, Some(&b), p("/h/private/x"), None),
            Route::Brain(brain("default"))
        );
        assert_eq!(
            route(&c, Some(&b), p("/h/private2"), None),
            Route::NotAvailable
        );
        assert_eq!(
            route(&c, Some(&b), p("private/x"), None),
            Route::NotAvailable
        );
    }

    #[test]
    fn only_with_just_an_empty_entry_is_not_available() {
        let mut c = cfg_text("[accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.claude-b\"\n");
        c.accounts.get_mut(&acct("acct-b")).unwrap().only = vec![PathBuf::new()];
        let b = acct("acct-b");
        for cwd in ["/", "/anywhere", "/h/private"] {
            assert_eq!(route(&c, Some(&b), p(cwd), None), Route::NotAvailable);
        }
    }

    #[test]
    fn a_relative_cwd_is_under_nothing() {
        assert!(!is_under(p("private/x"), p("/h/private")));
        assert!(!is_under(p(""), p("/h/private")));
    }

    #[test]
    fn a_relative_cwd_never_hides_in_an_except_folder() {
        let c = cfg();
        let a = acct("acct-a");
        assert_eq!(
            route(&c, Some(&a), p("private/x"), None),
            Route::Brain(brain("work"))
        );
    }

    proptest! {
        #[test]
        fn an_except_folder_routes_to_the_opener(
            seg in "[a-z]{1,8}",
            tail in "(/[a-z]{1,8}){0,3}",
            opener in prop::option::of("[a-z]{1,8}"),
        ) {
            let c = cfg_text(&format!(
                "[brains.work]\n[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.c\"\nbrain = \"work\"\nexcept = [\"~/{seg}\"]\n"
            ));
            let opener = opener.map(|o| brain(&o));
            let got = route(&c, Some(&acct("acct-a")), &PathBuf::from(format!("/h/{seg}{tail}")), opener.as_ref());
            let want = opener.unwrap_or_else(BrainName::default_brain);
            prop_assert_eq!(got, Route::Brain(want));
        }

        #[test]
        fn only_outside_its_folders_is_not_available(seg in "[a-z]{1,8}", other in "[a-z]{1,8}") {
            prop_assume!(other != seg);
            prop_assume!(format!("{other}x") != seg);
            let c = cfg_text(&format!(
                "[accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.c\"\nonly = [\"~/{seg}\"]\n"
            ));
            let got = route(&c, Some(&acct("acct-b")), &PathBuf::from(format!("/h/{other}x")), None);
            prop_assert_eq!(got, Route::NotAvailable);
        }

        #[test]
        fn empty_entries_never_match(path in "(/[a-z]{1,6}){0,4}") {
            prop_assert!(!is_under(Path::new(&path), Path::new("")));
            prop_assert!(!is_under_any(Path::new(&path), &[PathBuf::new()]));
        }

        #[test]
        fn a_lookalike_prefix_is_not_under(seg in "[a-z]{1,8}", extra in "[a-z0-9]{1,4}") {
            let path = PathBuf::from(format!("/h/{seg}{extra}"));
            let folder = PathBuf::from(format!("/h/{seg}"));
            prop_assert!(!is_under(&path, &folder));
        }

        #[test]
        fn trailing_slashes_and_dots_dont_matter(seg in "[a-z]{1,8}") {
            let path = PathBuf::from(format!("/h/{seg}/./x/"));
            let folder = PathBuf::from(format!("/h/{seg}/"));
            prop_assert!(is_under(&path, &folder));
        }
    }
}
