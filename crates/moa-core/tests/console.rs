//! A console command parsed with the real grammar and the real folder checks: `/open` and
//! `/term` name a folder that is there, spelled as the system spells it. `/auto`, `/sleep`
//! and `/account` are moa's own ops.

use std::path::{Path, PathBuf};

use moa_core::console::{ACCOUNT_USAGE, AUTO_USAGE, Parsed, SLEEP_AFTER_USAGE, parse};
use moa_core::op::Op;
use moa_core::state::Deps;

fn deps(home: &Path) -> Deps {
    Deps {
        parse_command: Some(moa_proto::cli::parse_command),
        canonical: moa_platform::canonical,
        is_dir: Path::is_dir,
        home: home.to_path_buf(),
        ..Deps::default()
    }
}

/// The folder `line` opens in, run from `cwd`.
fn folder(line: &str, cwd: &Path, home: &Path) -> Result<PathBuf, String> {
    match parse(line, Some(cwd), &deps(home))? {
        Parsed::Run(Op::TermOpen { path, .. } | Op::SessionOpen { path, .. }) => Ok(path),
        other => panic!("{line}: {other:?}"),
    }
}

#[test]
fn open_and_term_name_a_folder_that_is_there_spelled_canonically() {
    let root = tempfile::tempdir_in("/tmp").expect("a temp dir");
    let real = root.path().join("lantern");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, root.path().join("lamp")).unwrap();
    let canonical = std::fs::canonicalize(&real).unwrap();
    // `/tmp` is itself a link on some systems: the canonical spelling differs from the typed.
    let cwd = root.path();

    for line in [
        "/term lamp",
        "/open lamp",
        "/term ./lantern",
        "/open lamp/../lantern",
    ] {
        assert_eq!(folder(line, cwd, cwd), Ok(canonical.clone()), "{line}");
    }
    // No folder: the caller's folder (else home) is the terminal's.
    assert_eq!(
        folder("/term", &real, cwd),
        Ok(canonical.clone()),
        "where it was typed"
    );

    let missing = cwd.join("harbor");
    for line in ["/term harbor", "/open harbor"] {
        assert_eq!(
            folder(line, cwd, cwd),
            Err(format!("no folder {}", missing.display())),
            "{line}"
        );
    }
    assert!(!missing.exists(), "the core never makes a folder");
}

/// `/auto`, `/sleep` and `/account` are ops of their own; only `/setup` is still to come.
#[test]
fn auto_sleep_and_account_are_ops() {
    let root = tempfile::tempdir_in("/tmp").expect("a temp dir");
    let deps = deps(root.path());
    let run = |line: &str| match parse(line, None, &deps) {
        Ok(Parsed::Run(op)) => Ok(op),
        Ok(other) => panic!("{line}: {other:?}"),
        Err(why) => Err(why),
    };
    let auto = |on| Ok(Op::ConsoleAuto { on });
    assert_eq!(run("/auto"), auto(None));
    assert_eq!(run("/auto on"), auto(Some(true)));
    assert_eq!(run("/auto off"), auto(Some(false)));
    for line in ["/auto maybe", "/auto on off"] {
        assert_eq!(run(line), Err(AUTO_USAGE.to_string()), "{line}");
    }

    let sleep = |value: Option<&str>| {
        Ok(Op::ConsoleSleepAfter {
            value: value.map(str::to_string),
        })
    };
    assert_eq!(run("/sleep"), sleep(None));
    for value in ["on", "off", "2", "1.5", "0"] {
        assert_eq!(
            run(&format!("/sleep {value}")),
            sleep(Some(value)),
            "{value}"
        );
    }
    for line in ["/sleep x", "/sleep -1", "/sleep inf", "/sleep 2 3"] {
        assert_eq!(run(line), Err(SLEEP_AFTER_USAGE.to_string()), "{line}");
    }

    let account = |name: Option<&str>| {
        Ok(Op::ConsoleAccount {
            name: name.map(str::to_string),
        })
    };
    assert_eq!(run("/account"), account(None));
    assert_eq!(run("/account acct-b"), account(Some("acct-b")));
    assert_eq!(
        run("/account acct-a acct-b"),
        Err(ACCOUNT_USAGE.to_string())
    );

    assert_eq!(
        run("/setup"),
        Err("/setup comes in a later version of moa".to_string())
    );
}
