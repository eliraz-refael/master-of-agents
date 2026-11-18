//! Claude Code's on-disk layout: where a conversation's transcript lives, and how a tab
//! running Claude is recognised.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use moa_core::UtcMs;

pub mod agent;
pub mod normalize;
pub mod plugin;

pub use agent::{merge, pane_agent, project_slot};
pub use normalize::{last_assistant_text, normalize};
pub use plugin::session_plugin;

pub use moa_core::model::TranscriptCheck;
use moa_core::model::safe_id;

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

/// The conversation an agent in `cwd` most likely runs when its argv names none (R21): the
/// newest `*.jsonl` (by modification time) in `<store>/projects/<project_slot(cwd)>/` whose
/// name is a safe id, isn't in `skip`, and was modified at or after `since`. Directory reads
/// only; no transcript is opened. Two equally new ones go to the greater id, so the answer
/// doesn't depend on the order the directory lists them in.
pub fn newest_conversation(
    store: &Path,
    cwd: &Path,
    skip: &[String],
    since: Option<UtcMs>,
) -> Option<String> {
    let dir = store.join("projects").join(project_slot(cwd));
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let id = path.file_stem()?.to_str()?;
            if !safe_id(id) || skip.iter().any(|s| s == id) {
                return None;
            }
            let meta = fs::metadata(&path).ok().filter(fs::Metadata::is_file)?;
            let at = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            let at = UtcMs(i64::try_from(at.as_millis()).ok()?);
            since.is_none_or(|s| at >= s).then(|| (at, id.to_string()))
        })
        .max()
        .map(|(_, id)| id)
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
