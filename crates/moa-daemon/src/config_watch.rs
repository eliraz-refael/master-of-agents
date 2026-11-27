//! Following the config files (L8, §6.15.4): `moa.toml`, `layouts.json`, the brain's scope
//! file and moa's constitution, all in the config folder, are read again when saved.
//!
//! One watch hears the four ([`Settled`]), so an editor's several steps are one save. Once a
//! save has settled, each file whose bytes changed since it was last looked at is told to
//! the core: `moa.toml` read whole as at start ([`load_config`]), or why it can't be; the
//! others only as changed, since the core reads them itself. A save that leaves a file as it
//! was (a touch) is told nothing.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use moa_core::effect::Input;
use moa_platform::Paths;

use crate::core_task::CoreHandle;
use crate::run::{load_config, warn_skipped};
use crate::settled::Settled;

/// Which of the config files a path is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfigFile {
    MoaToml,
    Layouts,
    /// The brain's scope file or moa's constitution: both are moa's instructions.
    Instructions,
}

/// One watched file, and its bytes when it was last looked at (`None`: none, or unreadable).
struct Seen {
    file: ConfigFile,
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

pub(crate) struct ConfigWatch {
    paths: Paths,
    files: Vec<Seen>,
    watch: Settled,
}

impl ConfigWatch {
    /// Watches `files`, each as it is now. Their folders are made when missing, so a file
    /// saved there later is heard. Inside the runtime, and before the daemon says it is
    /// ready.
    pub(crate) fn new(paths: Paths, files: Vec<(ConfigFile, PathBuf)>) -> ConfigWatch {
        let entries: Vec<(PathBuf, OsString)> = files
            .iter()
            .filter_map(|(_, path)| Some((path.parent()?.to_path_buf(), path.file_name()?.into())))
            .collect();
        for (dir, _) in &entries {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!("the config folder can't be made: {}", e.kind());
            }
        }
        let files = files
            .into_iter()
            .map(|(file, path)| Seen {
                file,
                bytes: std::fs::read(&path).ok(),
                path,
            })
            .collect();
        let watch = Settled::new(
            "its config files",
            "they're read again when it starts",
            move || entries.clone(),
        );
        ConfigWatch {
            paths,
            files,
            watch,
        }
    }

    /// Follows the files until the task is cancelled.
    pub(crate) async fn run(mut self, core: CoreHandle) {
        loop {
            self.watch.next().await;
            self.check(&core);
        }
    }

    /// Tells the core of each file whose bytes changed: the scope file and the constitution
    /// together, once.
    fn check(&mut self, core: &CoreHandle) {
        let mut instructions = false;
        for seen in &mut self.files {
            let bytes = std::fs::read(&seen.path).ok();
            if bytes == seen.bytes {
                continue;
            }
            seen.bytes = bytes;
            match seen.file {
                ConfigFile::MoaToml => {
                    let read = load_config(&self.paths);
                    if let Ok(config) = &read {
                        warn_skipped(config);
                    }
                    tracing::info!("moa.toml was saved; reading it again");
                    core.stats().configs_read.fetch_add(1, Ordering::SeqCst);
                    core.input(Input::Config(read.map_err(|e| e.to_string())));
                }
                ConfigFile::Layouts => core.input(Input::LayoutsChanged),
                ConfigFile::Instructions => instructions = true,
            }
        }
        if instructions {
            core.input(Input::InstructionsChanged);
        }
    }
}
