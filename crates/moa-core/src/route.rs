//! Which brain a folder and account belong to. Pure: callers canonicalise the cwd and, via
//! `Config::canonicalize_with`, the configured folders; this does no IO.

use std::path::{Component, Path, PathBuf};

use crate::config::{AccountName, BrainName, Config};

#[derive(Clone, PartialEq, Debug)]
pub enum Route {
    Brain(BrainName),
    NotAvailable,
}

/// Whether `path` is `folder` or inside it, by whole path components. An empty `folder`
/// and a relative `path` match nothing, and neither does a `path` containing `..` (it is
/// not resolved, so `starts_with` would compare it literally and could be fooled). An unset
/// or unresolved entry never grants access: routing fails closed. `route` goes further: for
/// an account with limits, such a cwd routes to `NotAvailable`.
pub fn is_under(path: &Path, folder: &Path) -> bool {
    !folder.as_os_str().is_empty() && is_resolved(path) && path.starts_with(folder)
}

/// Absolute and free of `..`: a path whose folder we can name without touching the disk.
fn is_resolved(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir))
}

pub fn is_under_any(path: &Path, folders: &[PathBuf]) -> bool {
    folders.iter().any(|f| is_under(path, f))
}

/// Routes `cwd` for `account`. `home` is the opener's brain, else `default`.
///
/// - No account, or one moa.toml doesn't list: `home`. (`moactl` writes from an unlisted
///   account are refused before routing; see `Config::refuses_unlisted`.)
/// - `brain = B`: a cwd outside its `except` folders goes to B. Inside one, an except
///   folder means "not B", so the session goes to the opener when the opener is another
///   brain. When there's no opener or the opener is B, it goes to the brain that owns the
///   folder (`Config::brain_for_folder`) if that isn't B, else to `default`; if B is
///   `default` itself, there is nowhere left and it's `NotAvailable`. Never B.
/// - `only`: inside its folders, `home`; outside, `NotAvailable`.
/// - Otherwise `home`.
///
/// For an account with limits, a cwd that is relative or has `..` is `NotAvailable`.
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
    let limited = !acct.except.is_empty() || !acct.only.is_empty();
    if limited && !is_resolved(cwd) {
        // Can't tell which folder this is, so it goes nowhere rather than risk a wrong brain.
        return Route::NotAvailable;
    }
    if let Some(b) = &acct.brain {
        if !is_under_any(cwd, &acct.except) {
            return Route::Brain(b.clone());
        }
        if let Some(o) = opener.filter(|o| *o != b) {
            return Route::Brain(o.clone());
        }
        return match config.brain_for_folder(cwd) {
            Some(owner) if owner != *b => Route::Brain(owner),
            _ if *b != BrainName::default_brain() => Route::Brain(BrainName::default_brain()),
            _ => Route::NotAvailable,
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
        // The work opener is ignored in an except folder: that folder is "not work".
        assert_eq!(
            route(&c, Some(&a), p("/h/private/lantern"), Some(&brain("work"))),
            Route::Brain(brain("default"))
        );
        // Another brain's opener keeps it.
        assert_eq!(
            route(&c, Some(&a), p("/h/private/lantern"), Some(&brain("home"))),
            Route::Brain(brain("home"))
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
    fn dotdot_never_counts_as_under() {
        assert!(!is_under(p("/h/private/../work/x"), p("/h/private")));
        let c = cfg();
        let a = acct("acct-a");
        // except: an unresolvable cwd could be private or work, so it goes nowhere.
        assert_eq!(
            route(&c, Some(&a), p("/h/private/../work/x"), None),
            Route::NotAvailable
        );
        // only: fails closed.
        let c = cfg_text(
            "[accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.claude-b\"\nonly = [\"~/private\"]\n",
        );
        assert_eq!(
            route(&c, Some(&acct("acct-b")), p("/h/private/../work/x"), None),
            Route::NotAvailable
        );
    }

    #[test]
    fn a_relative_cwd_never_hides_in_an_except_folder() {
        let c = cfg();
        let a = acct("acct-a");
        assert_eq!(
            route(&c, Some(&a), p("private/x"), None),
            Route::NotAvailable
        );
    }

    #[test]
    fn accounts_without_limits_are_unchanged_by_unresolved_cwds() {
        let c = cfg_text(
            "[brains.work]\n[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.a\"\nbrain = \"work\"\n\
             [accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.b\"\n",
        );
        for cwd in ["rel/x", "/h/a/../b"] {
            assert_eq!(
                route(&c, Some(&acct("acct-a")), p(cwd), None),
                Route::Brain(brain("work"))
            );
            assert_eq!(
                route(&c, Some(&acct("acct-b")), p(cwd), None),
                Route::Brain(brain("default"))
            );
        }
    }

    #[test]
    fn an_except_folder_without_another_opener_goes_to_its_owning_brain() {
        let c = cfg_text(
            "[brains.work]\nfolders = [\"~/work\"]\n[brains.home]\nfolders = [\"~/private\"]\n\
             [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.a\"\nbrain = \"work\"\n\
             except = [\"~/private\", \"~/scratch\"]\n",
        );
        let a = acct("acct-a");
        for opener in [None, Some(brain("work"))] {
            assert_eq!(
                route(&c, Some(&a), p("/h/private/x"), opener.as_ref()),
                Route::Brain(brain("home"))
            );
            // No brain owns ~/scratch: default.
            assert_eq!(
                route(&c, Some(&a), p("/h/scratch/x"), opener.as_ref()),
                Route::Brain(brain("default"))
            );
        }
        // An except folder inside the account's own brain's folder still isn't that brain.
        let c = cfg_text(
            "[brains.work]\nfolders = [\"~\"]\n\
             [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.a\"\nbrain = \"work\"\n\
             except = [\"~/private\"]\n",
        );
        assert_eq!(
            route(&c, Some(&a), p("/h/private/x"), Some(&brain("work"))),
            Route::Brain(brain("default"))
        );
    }

    #[test]
    fn a_default_brain_account_has_nowhere_for_its_except_folders() {
        let c = cfg_text(
            "[brains.home]\nfolders = [\"~/home\"]\n\
             [accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.b\"\nbrain = \"default\"\n\
             except = [\"~/private\", \"~/home\"]\n",
        );
        let b = acct("acct-b");
        for opener in [None, Some(brain("default"))] {
            assert_eq!(
                route(&c, Some(&b), p("/h/private/x"), opener.as_ref()),
                Route::NotAvailable
            );
            assert_eq!(
                route(&c, Some(&b), p("/h/home/x"), opener.as_ref()),
                Route::Brain(brain("home"))
            );
        }
        assert_eq!(
            route(&c, Some(&b), p("/h/elsewhere"), None),
            Route::Brain(brain("default"))
        );
    }

    proptest! {
        /// Whatever the opener and whoever owns the folder, an except folder never routes to
        /// the account's own brain.
        #[test]
        fn an_except_folder_never_routes_to_the_account_brain(
            own in prop::sample::select(vec!["work", "default"]),
            owner in 0..3u8,
            seg in "[a-z]{1,8}",
            tail in "(/[a-z]{1,8}){0,3}",
            opener in prop::option::of(prop_oneof![
                Just("work".to_string()),
                Just("default".to_string()),
                "[a-z]{1,8}",
            ]),
        ) {
            // 0: no brain owns the folder; 1: the account's brain owns all of ~; 2: another
            // brain owns exactly the except folder.
            let mut text = String::new();
            match owner {
                1 => text.push_str(&format!("[brains.{own}]\nfolders = [\"~\"]\n")),
                2 => text.push_str(&format!("[brains.zz-other]\nfolders = [\"~/{seg}\"]\n")),
                _ => {}
            }
            if own == "work" && owner != 1 {
                text.push_str("[brains.work]\n");
            }
            text.push_str(&format!(
                "[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.c\"\nbrain = \"{own}\"\nexcept = [\"~/{seg}\"]\n"
            ));
            let c = cfg_text(&text);
            let opener = opener.map(|o| brain(&o));
            let cwd = PathBuf::from(format!("/h/{seg}{tail}"));
            let got = route(&c, Some(&acct("acct-a")), &cwd, opener.as_ref());
            prop_assert_ne!(&got, &Route::Brain(brain(own)));
            if let Some(o) = opener.filter(|o| o.as_str() != own) {
                prop_assert_eq!(got, Route::Brain(o));
            }
        }

        #[test]
        fn an_except_folder_routes_to_another_brains_opener(
            seg in "[a-z]{1,8}",
            tail in "(/[a-z]{1,8}){0,3}",
            opener in prop::option::of("[a-z]{1,8}"),
        ) {
            let c = cfg_text(&format!(
                "[brains.work]\n[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.c\"\nbrain = \"work\"\nexcept = [\"~/{seg}\"]\n"
            ));
            let opener = opener.map(|o| brain(&o));
            let got = route(&c, Some(&acct("acct-a")), &PathBuf::from(format!("/h/{seg}{tail}")), opener.as_ref());
            let want = opener
                .filter(|o| o.as_str() != "work")
                .unwrap_or_else(BrainName::default_brain);
            prop_assert_eq!(got, Route::Brain(want));
        }

        #[test]
        fn outside_every_except_folder_routes_to_the_account_brain(
            seg in "[a-z]{1,8}",
            other in "[a-z]{1,8}",
            tail in "(/[a-z]{1,8}){0,3}",
            opener in prop::option::of("[a-z]{1,8}"),
        ) {
            prop_assume!(other != seg);
            let c = cfg_text(&format!(
                "[brains.work]\n[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.c\"\nbrain = \"work\"\nexcept = [\"~/{seg}\"]\n"
            ));
            let opener = opener.map(|o| brain(&o));
            let got = route(&c, Some(&acct("acct-a")), &PathBuf::from(format!("/h/{other}{tail}")), opener.as_ref());
            prop_assert_eq!(got, Route::Brain(brain("work")));
        }

        #[test]
        fn only_outside_its_folders_is_not_available(seg in "[a-z]{1,8}", other in "[a-z]{1,8}") {
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
