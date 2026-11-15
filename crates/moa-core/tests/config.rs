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

/// Spec §6.9's example, copied verbatim (comments included).
const SPEC_EXAMPLE: &str = r#"
[brains.default]
folders = ["~/private", "~/.config"]   # folders this brain owns: adopt, project add,
moa_account = "acct-b"                 #   untracked panes, seeding, new-session prefill
open_account = "acct-b"
[brains.work]
folders = ["~/work"]
moa_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-acct-a"
brain = "work"                         # setup install acct-a --brain work --except …
except = ["~/private", "~/.config"]
[accounts.acct-b]
kind = "claude"
store = "~/.claude-acct-b"
only = ["~/private"]                   # setup only: the account works with moa only here
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
    assert_eq!(b.kind, AgentKind::Claude);
    assert_eq!(b.store, PathBuf::from("/h/.claude-acct-b"));
    assert_eq!(b.launcher, None);
    assert_eq!(b.brain, None);
    assert_eq!(b.only, paths(&["/h/private"]));
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
fn an_explicit_empty_only_is_an_error() {
    let text = "[accounts.acct-b]\nkind = \"claude\"\nstore = \"~/.c\"\nonly = []\n";
    assert_eq!(
        Config::parse(text, home()).unwrap_err().to_string(),
        "moa.toml: only in accounts.acct-b is empty; remove it to let the account work everywhere"
    );
}

#[test]
fn two_spellings_of_one_store_are_caught_after_canonicalising() {
    let t = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let r = t.path();
    std::fs::create_dir(r.join("real")).unwrap();
    std::os::unix::fs::symlink(r.join("real"), r.join("alias")).unwrap();
    let text = format!(
        "[accounts.acct-a]\nkind = \"claude\"\nstore = \"{r}/real\"\n\
         [accounts.acct-b]\nkind = \"claude\"\nstore = \"{r}/alias\"\n",
        r = r.display()
    );
    // Different spellings: parse alone can't tell.
    assert!(Config::parse(&text, home()).is_ok());
    let err = Config::from_file_text(Some(&text), home(), moa_platform::canonical).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("moa.toml: acct-a and acct-b have the same store "),
        "{err}"
    );
}

#[test]
fn dotdot_in_a_path_is_an_error() {
    let cases = [
        (
            "[brains.default]\nfolders = [\"~/a/../b\"]\n",
            "moa.toml: the folder in brains.default can't contain ..",
        ),
        (
            "[accounts.x]\nkind = \"claude\"\nstore = \"~/.c\"\nbrain = \"default\"\nexcept = [\"/a/..\"]\n",
            "moa.toml: the folder in accounts.x.except can't contain ..",
        ),
        (
            "[accounts.x]\nkind = \"claude\"\nstore = \"~/.c\"\nonly = [\"~/../b\"]\n",
            "moa.toml: the folder in accounts.x.only can't contain ..",
        ),
        (
            "[accounts.x]\nkind = \"claude\"\nstore = \"/a/../.c\"\n",
            "moa.toml: the store in accounts.x can't contain ..",
        ),
        (
            "[accounts.x]\nkind = \"claude\"\nstore = \"~/.c\"\nlauncher = \"/a/../l\"\n",
            "moa.toml: the launcher in accounts.x can't contain ..",
        ),
        (
            "[brains.default]\nspike_db = \"/a/../d\"\n",
            "moa.toml: the spike_db in brains.default can't contain ..",
        ),
    ];
    for (text, want) in cases {
        assert_eq!(
            Config::parse(text, home()).unwrap_err().to_string(),
            want,
            "{text}"
        );
    }
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

#[test]
fn a_misspelt_accounts_table_is_an_error() {
    assert_eq!(
        err("[acounts.x]\nkind = \"claude\"\n"),
        "moa.toml: [acounts] isn't a table moa knows; did you mean [accounts]?"
    );
}

#[test]
fn a_singular_brain_table_is_an_error() {
    assert_eq!(
        err("[brain.work]\nfolders = []\n"),
        "moa.toml: [brain] isn't a table moa knows; did you mean [brains]?"
    );
    // A distant name is still just skipped.
    assert!(Config::parse("[layouts]\nx = 1\n", home()).is_ok());
}

#[test]
fn empty_store_launcher_and_spike_db_are_errors() {
    assert_eq!(
        err("[accounts.acct-a]\nkind = \"claude\"\nstore = \"\"\n"),
        "moa.toml: the store in accounts.acct-a is empty; it must start with / or ~"
    );
    assert_eq!(
        err("[accounts.acct-a]\nkind = \"claude\"\nstore = \"/s\"\nlauncher = \"\"\n"),
        "moa.toml: the launcher in accounts.acct-a is empty; it must start with / or ~"
    );
    assert_eq!(
        err("[brains.default]\nspike_db = \"\"\n"),
        "moa.toml: the spike_db in brains.default is empty; it must start with / or ~"
    );
}

#[test]
fn two_accounts_cannot_share_a_store() {
    assert_eq!(
        err(
            "[accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.c\"\n[accounts.acct-b]\nkind = \"claude\"\nstore = \"/h/.c\"\n"
        ),
        "moa.toml: acct-a and acct-b have the same store /h/.c"
    );
}

#[test]
fn one_folder_in_two_brains_is_an_error() {
    let text =
        "[brains.default]\nfolders = [\"~/shared\"]\n[brains.work]\nfolders = [\"~/shared/\"]\n";
    assert_eq!(
        err(text),
        "moa.toml: /h/shared is a folder of both brains.default and brains.work; a folder belongs to one brain"
    );
    // Listed twice in one brain is harmless; empty entries never clash.
    let fine =
        "[brains.default]\nfolders = [\"~/a\", \"~/a\", \"\"]\n[brains.work]\nfolders = [\"\"]\n";
    assert!(Config::parse(fine, home()).is_ok());
}

#[test]
fn one_folder_in_two_brains_is_caught_after_canonicalising() {
    let t = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let r = t.path();
    std::fs::create_dir(r.join("real")).unwrap();
    std::os::unix::fs::symlink(r.join("real"), r.join("alias")).unwrap();
    let text = format!(
        "[brains.default]\nfolders = [\"{r}/real\"]\n[brains.work]\nfolders = [\"{r}/alias\"]\n",
        r = r.display()
    );
    // Different spellings: parse alone can't tell.
    assert!(Config::parse(&text, home()).is_ok());
    let err = Config::from_file_text(Some(&text), home(), moa_platform::canonical)
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("moa.toml: "), "{err}");
    assert!(
        err.ends_with(
            " is a folder of both brains.default and brains.work; a folder belongs to one brain"
        ),
        "{err}"
    );
}

#[test]
fn the_most_specific_folder_owns_a_path() {
    let c = Config::parse(
        "[brains.default]\nfolders = [\"~\"]\n[brains.work]\nfolders = [\"~/work\"]\n\
         [brains.deep]\nfolders = [\"~/work/deep\"]\n",
        home(),
    )
    .unwrap();
    let owner = |p: &str| c.brain_for_folder(Path::new(p));
    assert_eq!(owner("/h/work/foo"), Some(brain("work")));
    assert_eq!(owner("/h/work"), Some(brain("work")));
    assert_eq!(owner("/h/work/deep/x"), Some(brain("deep")));
    assert_eq!(owner("/h/private/x"), Some(brain("default")));
    assert_eq!(owner("/h"), Some(brain("default")));
    // `~/work2` is not inside `~/work`.
    assert_eq!(owner("/h/work2/foo"), Some(brain("default")));
    // Outside every folder, relative, or climbing with `..`: nobody owns it.
    assert_eq!(owner("/elsewhere"), None);
    assert_eq!(owner("work/foo"), None);
    assert_eq!(owner("/h/work/../private"), None);
    // The order brains are listed in doesn't matter.
    let c = Config::parse(
        "[brains.work]\nfolders = [\"~/work\"]\n[brains.default]\nfolders = [\"~\"]\n",
        home(),
    )
    .unwrap();
    assert_eq!(
        c.brain_for_folder(Path::new("/h/work/x")),
        Some(brain("work"))
    );
    assert_eq!(Config::default().brain_for_folder(Path::new("/h")), None);
}
