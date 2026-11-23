//! A console command parsed with the real grammar and the real folder checks: `/open` and
//! `/term` name a folder that is there, spelled as the system spells it.

use std::path::{Path, PathBuf};

use moa_core::console::{Parsed, parse};
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
