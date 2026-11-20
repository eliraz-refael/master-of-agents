//! Test support for moa: a throwaway tmux server in an isolated home and environment.

pub mod brain;
mod sandbox;
pub mod spike_fixture;
pub mod spike_gen;
pub mod temp_brain;
mod terminal;

pub use sandbox::{Sandbox, TMUX_NOT_FOUND, tmux_path, wait_for};
pub use spike_fixture::{OLD_SCHEMA_SQL, Scenario, SpikeBrain, SpikeFixture, ids};
pub use temp_brain::TempBrain;
pub use terminal::{Terminal, pty, without_terminal};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One step of a `fake-claude` script, a JSON line in `.fake-claude.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "do")]
pub enum FakeStep {
    /// Fire a hook event with these fields (plus the ones fake-claude fills in).
    Hook {
        event: String,
        #[serde(flatten)]
        fields: Map<String, Value>,
    },
    Sleep {
        ms: u64,
    },
    /// Wait until `<cwd>/<file>` exists: a test opens the gate once it has seen what the
    /// steps before it did, so no step races the clock.
    Wait {
        file: String,
    },
    /// `/clear`: end the session, take a new id, start again.
    Clear,
    /// Quit cleanly.
    Exit,
    /// Crash at once.
    Die,
    /// Sit there forever.
    Hang,
}

/// The path of the `fake-claude` binary, built next to the test binaries by `--workspace`.
pub fn fake_claude() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    // Test binaries sit in `deps/` (or deeper, in a per-package build dir) under the profile
    // dir that holds the workspace's binaries, so look upward for it.
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join("fake-claude"))
        .find(|path| path.is_file())
        .expect("fake-claude isn't built: run the tests with --workspace")
}

/// `fake-claude` under the name `claude`: the symlink `<dir>/bin/claude`, so process reads
/// recognise it as Claude by its argv (R9). Returns the symlink's path.
pub fn fake_claude_named_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("make the bin folder");
    let link = bin.join("claude");
    // A second call replaces the link; a link that can't be replaced fails just below.
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(fake_claude(), &link).expect("link fake-claude as claude");
    link
}

/// Write `steps` as `<dir>/.fake-claude.jsonl`, the script fake-claude reads from its cwd.
pub fn write_script(dir: &Path, steps: &[FakeStep]) {
    let mut text = String::new();
    for step in steps {
        text.push_str(&serde_json::to_string(step).expect("step json"));
        text.push('\n');
    }
    std::fs::write(dir.join(".fake-claude.jsonl"), text).expect("write fake-claude script");
}
