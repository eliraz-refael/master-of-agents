//! T23: tests reach tmux only through the testkit (`Sandbox`, `TempBrain`, `Terminal`),
//! which clear the environment first. A bare `tmux` from a test inside the user's live
//! tmux could hit their server.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn tmux_only_through_the_testkit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Built at run time, so this file doesn't contain what it looks for.
    let needles = [
        concat!("Command::new(", "\"tmux\""),
        concat!("Command::new(", "tmux"),
    ];
    let mut files = Vec::new();
    for krate in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        // The testkit's own src/ holds the helpers, and isn't scanned.
        rust_files(&krate.path().join("tests"), &mut files);
    }
    assert!(files.len() > 5, "found only {} test files", files.len());
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for n in &needles {
            if text.contains(n) {
                bad.push(format!("{} has {n}", f.display()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "run tmux through moa_testkit's Sandbox, TempBrain or Terminal:\n{}",
        bad.join("\n")
    );
}
