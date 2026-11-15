use std::fs;
use std::path::{Path, PathBuf};

use moa_core::model::AgentKind;
use moa_store::accounts::AccountMap;
use proptest::prelude::*;

fn home() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    for d in [".acct-a", ".acct-b", ".other"] {
        fs::create_dir(t.path().join(d)).unwrap();
    }
    t
}

fn map(home: &Path) -> AccountMap {
    AccountMap::new(home.to_path_buf()).with("work", home.join(".other"))
}

#[test]
fn resolves_by_name_dir_and_path() {
    let t = home();
    let h = t.path();
    let m = map(h);

    let r = m.resolve("acct-a");
    assert_eq!(
        (r.name.as_str(), r.store, r.kind),
        ("acct-a", Some(h.join(".acct-a")), AgentKind::Claude)
    );

    // An explicit entry by name wins.
    assert_eq!(m.resolve("work").store, Some(h.join(".other")));
    // A name whose home dir belongs to an explicit entry resolves to that entry.
    let r = m.resolve("other");
    assert_eq!((r.name.as_str(), r.store), ("work", Some(h.join(".other"))));
    // An absolute path gives the basename minus the leading dot.
    let r = m.resolve(h.join(".acct-b").to_str().unwrap());
    assert_eq!(
        (r.name.as_str(), r.store),
        ("acct-b", Some(h.join(".acct-b")))
    );
    // An absolute path that an explicit entry owns resolves to it.
    let r = m.resolve(h.join(".other").to_str().unwrap());
    assert_eq!(r.name, "work");

    let r = m.resolve("codex");
    assert_eq!((r.kind, r.store), (AgentKind::Codex, None));

    let r = m.resolve("acct-z");
    assert_eq!((r.name.as_str(), r.store), ("acct-z", None));
    // An absolute path with no directory keeps its basename and has no store.
    let r = m.resolve(h.join(".acct-z").to_str().unwrap());
    assert_eq!((r.name.as_str(), r.store), ("acct-z", None));
}

#[test]
fn store_for_and_index_of() {
    let t = home();
    let m = map(t.path());
    assert_eq!(m.store_for("work"), Some(t.path().join(".other")));
    assert_eq!(m.store_for("acct-a"), Some(t.path().join(".acct-a")));
    assert_eq!(m.store_for("nope"), None);
    assert_eq!(m.index_of("acct-b"), 1);
    assert_eq!(m.index_of("acct-a"), 2);
    assert_eq!(m.index_of("acct-b"), 1);
    assert_eq!(m.seen(), ["acct-b", "acct-a"]);
}

#[test]
fn parse_flag_splits_and_expands_the_tilde() {
    let home = PathBuf::from("/h/me");
    assert_eq!(
        AccountMap::parse_flag("acct-a=~/.acct-other", &home),
        Ok(("acct-a".to_string(), PathBuf::from("/h/me/.acct-other")))
    );
    assert_eq!(
        AccountMap::parse_flag("a=/x/y", &home),
        Ok(("a".to_string(), PathBuf::from("/x/y")))
    );
    assert_eq!(
        AccountMap::parse_flag("a=~", &home),
        Ok(("a".to_string(), home.clone()))
    );
    for bad in ["", "=x", "x=", "nodelim"] {
        let e = AccountMap::parse_flag(bad, &home).unwrap_err();
        assert!(!e.contains(bad) || bad.is_empty());
    }
}

proptest! {
    #[test]
    fn resolve_is_idempotent(name in "[a-z][a-z0-9-]{0,12}") {
        let t = home();
        let m = map(t.path());
        let once = m.resolve(&name).name;
        prop_assert_eq!(m.resolve(&once).name, once);
    }
}

#[test]
fn from_config_names_the_configured_stores() {
    let config = moa_core::config::Config::parse(
        "[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-acct-a\"\n",
        Path::new("/h"),
    )
    .unwrap();
    let m = AccountMap::from_config(&config, Path::new("/h"));
    assert_eq!(
        m.resolve("acct-a").store,
        Some(PathBuf::from("/h/.claude-acct-a"))
    );
    assert_eq!(m.resolve("/h/.claude-acct-a").name, "acct-a");
}
