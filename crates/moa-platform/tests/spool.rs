use moa_platform::spool::{append, draining, take};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

#[test]
fn append_writes_whole_lines_with_mode_600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"{\"a\":1}").unwrap();
    append(&path, b"{\"b\":2}").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"a\":1}\n{\"b\":2}\n"
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn concurrent_appenders_never_interleave() {
    let dir = tempfile::tempdir().unwrap();
    let path = Arc::new(dir.path().join("spool/hooks.jsonl"));
    let handles: Vec<_> = (0..8u8)
        .map(|t| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                let line = vec![b'a' + t; 64 * 1024];
                for _ in 0..200 {
                    append(&path, &line).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let text = std::fs::read_to_string(&*path).unwrap();
    let mut count = [0usize; 8];
    for line in text.lines() {
        assert_eq!(line.len(), 64 * 1024);
        let first = line.as_bytes()[0];
        assert!(line.bytes().all(|b| b == first), "interleaved line");
        count[(first - b'a') as usize] += 1;
    }
    assert_eq!(count, [200; 8]);
}

#[test]
fn take_moves_lines_to_draining_and_empties_the_spool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"one").unwrap();
    append(&path, b"two").unwrap();
    let taken = take(&path).unwrap();
    assert_eq!(taken.lines, ["one", "two"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    assert_eq!(
        std::fs::read_to_string(draining(&path)).unwrap(),
        "one\ntwo\n"
    );
}

#[test]
fn a_hook_during_the_drain_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"old").unwrap();
    let taken = take(&path).unwrap();
    append(&path, b"new").unwrap();
    assert_eq!(taken.lines, ["old"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    taken.done().unwrap();
    assert_eq!(take(&path).unwrap().lines, ["new"]);
}

#[test]
fn a_leftover_draining_file_comes_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    moa_platform::ensure_private_dir(&dir.path().join("spool")).unwrap();
    std::fs::write(draining(&path), "A\n").unwrap();
    append(&path, b"B").unwrap();
    assert_eq!(take(&path).unwrap().lines, ["A", "B"]);
}

#[test]
fn a_missing_spool_gives_the_draining_lines_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    moa_platform::ensure_private_dir(&dir.path().join("spool")).unwrap();
    assert!(take(&path).unwrap().lines.is_empty());
    std::fs::write(draining(&path), "A\n").unwrap();
    assert_eq!(take(&path).unwrap().lines, ["A"]);
}

#[test]
fn done_deletes_draining() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"x").unwrap();
    let taken = take(&path).unwrap();
    assert!(draining(&path).exists());
    taken.done().unwrap();
    assert!(!draining(&path).exists());
}

#[test]
fn done_is_fine_when_nothing_was_drained() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    moa_platform::ensure_private_dir(&dir.path().join("spool")).unwrap();
    take(&path).unwrap().done().unwrap();
    std::fs::write(&path, "").unwrap();
    take(&path).unwrap().done().unwrap();
}

#[test]
fn a_non_utf8_line_is_kept_lossy_and_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"bad \xff\xfe line").unwrap();
    append(&path, b"good").unwrap();
    let taken = take(&path).unwrap();
    assert_eq!(taken.lines.len(), 2);
    assert!(taken.lines[0].starts_with("bad "));
    assert_eq!(taken.lines[1], "good");
    // Still drainable again, as after a crash before `done`.
    assert_eq!(take(&path).unwrap().lines.len(), 2);
}

#[test]
fn a_torn_line_never_swallows_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool/hooks.jsonl");
    append(&path, b"one").unwrap();
    // A writer that died mid-line.
    std::fs::write(&path, "one\ntor").unwrap();
    append(&path, b"two").unwrap();
    assert_eq!(take(&path).unwrap().lines, ["one", "tor", "two"]);
    // The same in `.draining`: a crashed drain's torn tail stays apart from the next lines.
    std::fs::write(draining(&path), "torn").unwrap();
    append(&path, b"three").unwrap();
    assert_eq!(take(&path).unwrap().lines, ["torn", "three"]);
}
