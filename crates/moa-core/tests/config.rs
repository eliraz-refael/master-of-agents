use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use moa_core::config::{AccountName, BrainName, Config, caller_store};
use moa_core::model::AgentKind;

const HOME: &str = "/h";

fn home() -> &'static Path {
    Path::new(HOME)
}

fn brain(s: &str) -> BrainName {
    BrainName::parse(s).unwrap()
}

fn account(s: &str) -> AccountName {
    AccountName::parse(s).unwrap()
}

fn paths(v: &[&str]) -> Vec<PathBuf> {
    v.iter().map(PathBuf::from).collect()
}

fn err(text: &str) -> String {
    Config::parse(text, home()).unwrap_err().0
}

const SPEC_EXAMPLE: &str = r#"
[brains.default]
folders = ["~/private", "~/.config"]
moa_account = "acct-b"
open_account = "acct-b"
[brains.work]
folders = ["~/work"]
moa_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-acct-a"
brain = "work"
except = ["~/private", "~/.config"]
[accounts.acct-b]
kind = "claude"
store = "~/.claude-acct-b"
only = []
"#;

#[test]
fn the_spec_example_parses() {
    let c = Config::parse(SPEC_EXAMPLE, home()).unwrap();
    assert!(c.skipped_tables.is_empty());
    assert!(c.setup.skipped.is_empty());

    let d = &c.brains[&brain("default")];
    assert_eq!(d.folders, paths(&["/h/private", "/h/.config"]));
    assert_eq!(d.moa_account, Some(account("acct-b")));
    assert_eq!(d.open_account, Some(account("acct-b")));
    assert_eq!(d.spike_db, None);

    let w = &c.brains[&brain("work")];
    assert_eq!(w.folders, paths(&["/h/work"]));
    assert_eq!(w.moa_account, Some(account("acct-a")));
    assert_eq!(w.open_account, None);

    let a = &c.accounts[&account("acct-a")];
    assert_eq!(a.kind, AgentKind::Claude);
    assert_eq!(a.store, PathBuf::from("/h/.claude-acct-a"));
    assert_eq!(a.brain, Some(brain("work")));
    assert_eq!(a.except, paths(&["/h/private", "/h/.config"]));
    assert!(a.only.is_empty());

    let b = &c.accounts[&account("acct-b")];
    assert_eq!(b.store, PathBuf::from("/h/.claude-acct-b"));
    assert_eq!(b.brain, None);
    assert!(b.only.is_empty());
    assert!(b.except.is_empty());
}

#[test]
fn no_file_means_one_default_brain() {
    let c = Config::from_file_text(None, home(), |p| p.to_path_buf()).unwrap();
    assert_eq!(c, Config::default());
    let d = c.brain(&BrainName::default_brain()).unwrap();
    assert!(d.folders.is_empty());
    assert_eq!(d.moa_account, None);
    assert!(c.brain(&brain("work")).is_none());
}

#[test]
fn from_file_text_parses_then_canonicalizes() {
    let text = "[brains.default]\nfolders = [\"~/link\"]\n";
    let c = Config::from_file_text(Some(text), home(), |p| {
        if p == Path::new("/h/link") {
            PathBuf::from("/real")
        } else {
            p.to_path_buf()
        }
    })
    .unwrap();
    assert_eq!(c.brains[&brain("default")].folders, paths(&["/real"]));
    assert!(Config::from_file_text(Some("[brains.Bad]"), home(), |p| p.to_path_buf()).is_err());
}

#[test]
fn a_configured_default_brain_is_returned_as_is() {
    let c = Config::parse("[brains.default]\nfolders = [\"/x\"]\n", home()).unwrap();
    assert_eq!(c.brain(&brain("default")).unwrap().folders, paths(&["/x"]));
}

#[test]
fn an_unknown_top_level_table_is_skipped() {
    let c = Config::parse("[layouts]\nx = 1\n[brains.default]\nfolders = []\n", home()).unwrap();
    assert_eq!(c.skipped_tables, vec!["layouts".to_string()]);
    assert!(c.brains.contains_key(&brain("default")));
}

#[test]
fn setup_skipped_is_read() {
    let c = Config::parse("[setup]\nskipped = [\"acct-a\"]\n", home()).unwrap();
    assert_eq!(c.setup.skipped, vec![account("acct-a")]);
    assert!(err("[setup]\nbogus = 1\n").starts_with("moa.toml line 2:"));
}

#[test]
fn empty_folder_entries_are_kept() {
    let c = Config::parse("[brains.default]\nfolders = [\"\", \"/x\"]\n", home()).unwrap();
    assert_eq!(c.brains[&brain("default")].folders, paths(&["", "/x"]));
}

#[test]
fn errors_are_one_plain_line() {
    let acct = |body: &str| format!("[accounts.acct-a]\nstore = \"/s\"\nkind = \"claude\"\n{body}");
    let cases: Vec<(String, &str)> = vec![
        (
            "[brains.Work]\n".into(),
            "moa.toml: \"Work\" isn't a brain name (use lowercase letters, digits, - and _)",
        ),
        (
            acct("except = [\"/x\"]\n"),
            "moa.toml: acct-a has except folders but no brain to send the rest to",
        ),
        (
            acct("brain = \"work\"\nonly = [\"/x\"]\n[brains.work]\n"),
            "moa.toml: acct-a can't both go to a brain and be limited to folders",
        ),
        (
            acct("brain = \"work\"\n"),
            "moa.toml: acct-a sends its sessions to the work brain, which isn't in moa.toml",
        ),
        (
            "[brains.default]\nmoa_account = \"acct-z\"\n".into(),
            "moa.toml: the default brain's moa_account acct-z isn't in [accounts]",
        ),
        (
            "[brains.default]\nfolders = [\"src\"]\n".into(),
            "moa.toml: the folder src in brains.default must start with / or ~",
        ),
        (
            "[brains.default]\nfolders = [\"~bob/x\"]\n".into(),
            "moa.toml: ~bob/x: only ~ or ~/… is understood",
        ),
        (
            "[accounts.acct-a]\nstore = \"/s\"\nkind = \"gemini\"\n".into(),
            "moa.toml: acct-a's kind must be claude or codex",
        ),
    ];
    for (input, want) in cases {
        let got = err(&input);
        assert_eq!(got, want, "input: {input}");
        assert!(!got.contains('\n'));
    }

    let got = err("[brains.default]\nfolder = []\n");
    assert!(got.starts_with("moa.toml line 2:"), "{got}");
    assert!(!got.contains('\n'));

    let got = err("[brains.default\n");
    assert!(got.starts_with("moa.toml line 1:"), "{got}");
    assert!(!got.contains('\n'));
}

#[test]
fn canonicalize_with_rewrites_every_path() {
    let mut c = Config::parse(
        r#"
[brains.default]
folders = ["~/link/a", ""]
[brains.work]
[accounts.acct-a]
kind = "claude"
store = "~/link"
brain = "work"
except = ["~/link/e", ""]
[accounts.acct-b]
kind = "claude"
store = "/s"
only = ["~/link/o"]
"#,
        home(),
    )
    .unwrap();
    c.canonicalize_with(|p| match p.strip_prefix("/h/link") {
        Ok(rest) => Path::new("/real").join(rest),
        Err(_) => p.to_path_buf(),
    });
    assert_eq!(c.brains[&brain("default")].folders, paths(&["/real/a", ""]));
    let a = &c.accounts[&account("acct-a")];
    assert_eq!(a.store, PathBuf::from("/real"));
    assert_eq!(a.except, paths(&["/real/e", ""]));
    let b = &c.accounts[&account("acct-b")];
    assert_eq!(b.store, PathBuf::from("/s"));
    assert_eq!(b.only, paths(&["/real/o"]));
}

#[test]
fn account_for_store_matches_the_store() {
    let c = Config::parse(SPEC_EXAMPLE, home()).unwrap();
    assert_eq!(
        c.account_for_store(Path::new("/h/.claude-acct-a")),
        Some(&account("acct-a"))
    );
    assert_eq!(
        c.account_for_store(Path::new("/h/.claude-acct-b")),
        Some(&account("acct-b"))
    );
    assert_eq!(c.account_for_store(Path::new("/h/.claude")), None);
}

#[test]
fn caller_store_defaults_to_dot_claude() {
    assert_eq!(caller_store(None, home()), PathBuf::from("/h/.claude"));
    assert_eq!(
        caller_store(Some(OsStr::new("")), home()),
        PathBuf::from("/h/.claude")
    );
    assert_eq!(
        caller_store(Some(OsStr::new("/x")), home()),
        PathBuf::from("/x")
    );
}

#[test]
fn names_are_validated() {
    assert_eq!(BrainName::default_brain().as_str(), "default");
    assert_eq!(brain("work-2_x").to_string(), "work-2_x");
    for bad in ["", "Work", "-a", "_a", "a b", "a.b", &"a".repeat(33)] {
        assert!(BrainName::parse(bad).is_err(), "{bad}");
    }
    assert!(BrainName::parse(&"a".repeat(32)).is_ok());
    assert!(AccountName::parse("Acct.A-1_b").is_ok());
    for bad in ["", ".a", "-a", "a b", &"a".repeat(65)] {
        assert!(AccountName::parse(bad).is_err(), "{bad}");
    }
    assert!(AccountName::parse(&"a".repeat(64)).is_ok());
}
