//! Claude Code's on-disk layout: where a conversation's transcript lives.

use std::fs;
use std::path::Path;

pub mod normalize;
pub mod plugin;

pub use normalize::{last_assistant_text, normalize};
pub use plugin::session_plugin;

pub use moa_core::model::TranscriptCheck;

/// True for ids that are safe to use as a file name: `^[A-Za-z0-9-]{1,64}$`.
fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Looks for `<store>/projects/*/<conversation_id>.jsonl`. It checks that the file exists and
/// never reads it.
pub fn find_transcript(store: &Path, conversation_id: &str) -> TranscriptCheck {
    if !safe_id(conversation_id) {
        return TranscriptCheck::UnsafeId;
    }
    let Ok(dirs) = fs::read_dir(store.join("projects")) else {
        return TranscriptCheck::Missing;
    };
    let mut dirs: Vec<_> = dirs.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let file = dir.join(format!("{conversation_id}.jsonl"));
        if file.is_file() {
            return TranscriptCheck::Present(file);
        }
    }
    TranscriptCheck::Missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_missing_and_unsafe() {
        let t = tempfile::tempdir().unwrap();
        let store = t.path();
        assert_eq!(find_transcript(store, "abc-1"), TranscriptCheck::Missing);
        fs::create_dir_all(store.join("projects/-x")).unwrap();
        assert_eq!(find_transcript(store, "abc-1"), TranscriptCheck::Missing);
        fs::write(store.join("projects/-x/abc-1.jsonl"), "{}\n").unwrap();
        assert_eq!(
            find_transcript(store, "abc-1"),
            TranscriptCheck::Present(store.join("projects/-x/abc-1.jsonl"))
        );
        assert_eq!(
            find_transcript(store, "../../etc"),
            TranscriptCheck::UnsafeId
        );
        assert_eq!(find_transcript(store, ""), TranscriptCheck::UnsafeId);
        assert_eq!(
            find_transcript(store, &"a".repeat(65)),
            TranscriptCheck::UnsafeId
        );
    }

    #[test]
    fn a_directory_named_like_a_transcript_is_not_one() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("projects/-x/abc.jsonl")).unwrap();
        assert_eq!(find_transcript(t.path(), "abc"), TranscriptCheck::Missing);
    }
}
