//! Which conversation a hand-started Claude is on, when its argv names none (R21): the
//! newest transcript in its folder's project slot, not held by another tab, and written since
//! the agent started.

use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use moa_agents::claude::{newest_conversation, project_slot};
use moa_core::UtcMs;

/// Writes `<dir>/<name>` and sets its modification time to `at_ms` after the epoch.
fn write_at(dir: &Path, name: &str, at_ms: u64) {
    let file = dir.join(name);
    fs::write(&file, "{}\n").unwrap();
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(at_ms))
        .unwrap();
}

#[test]
fn newest_conversation_skips_held_and_older_ones() {
    let t = tempfile::tempdir().unwrap();
    let store = t.path().join("store");
    let cwd = Path::new("/h/private/lantern");
    let slot = store.join("projects").join(project_slot(cwd));
    fs::create_dir_all(&slot).unwrap();
    // Before the agent started, the free one it should take, and a newer one another tab holds.
    write_at(&slot, "old-1.jsonl", 1_000);
    write_at(&slot, "free-2.jsonl", 2_000);
    write_at(&slot, "held-3.jsonl", 3_000);
    // Not transcripts, or not ids moa can use: never chosen, however new.
    write_at(&slot, "notes.txt", 9_000);
    write_at(&slot, "bad id.jsonl", 9_000);
    fs::create_dir_all(slot.join("dir-9.jsonl")).unwrap();
    // Another folder's slot.
    let other = store
        .join("projects")
        .join(project_slot(Path::new("/h/private/web")));
    fs::create_dir_all(&other).unwrap();
    write_at(&other, "web-9.jsonl", 9_000);

    let held = vec!["held-3".to_string()];
    let since = Some(UtcMs(1_500));
    assert_eq!(
        newest_conversation(&store, cwd, &held, since),
        Some("free-2".into())
    );
    // At the very moment it started counts.
    assert_eq!(
        newest_conversation(&store, cwd, &held, Some(UtcMs(2_000))),
        Some("free-2".into())
    );
    // Nothing written since the agent started.
    assert_eq!(
        newest_conversation(&store, cwd, &held, Some(UtcMs(3_500))),
        None
    );
    // With nothing held, the newest; with no start time, any age.
    assert_eq!(
        newest_conversation(&store, cwd, &[], since),
        Some("held-3".into())
    );
    let all: Vec<String> = ["held-3", "free-2"].map(String::from).to_vec();
    assert_eq!(
        newest_conversation(&store, cwd, &all, None),
        Some("old-1".into())
    );
    // No slot for the folder, or no store at all.
    assert_eq!(
        newest_conversation(&store, Path::new("/h/elsewhere"), &[], None),
        None
    );
    assert_eq!(
        newest_conversation(&t.path().join("none"), cwd, &[], None),
        None
    );
}
