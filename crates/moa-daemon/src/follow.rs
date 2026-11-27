//! Following the daemon's program (L3): a new build put at its path upgrades the daemon.
//!
//! The watch hears every entry of the program's symlink chain ([`moa_platform::exe_chain`]),
//! so a rebuild renamed over it and a profile switch that retargets a link are both heard,
//! with no polling. After a change the watch is armed again from the chain as it now is, and
//! once changes have settled for [`SETTLE`] the program's build is read. A build other than
//! this image's is asked for its build (`<exe> build-id`, [`ProbeWait`]): a program caught
//! half-written can't say it, and is skipped until the next change. One that says nothing in
//! time is asked once more, with longer (a first launch can be slow), then skipped too. One
//! that says its build becomes `Input::Upgrade(Rebuilt)`, and the core takes it from there.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::Duration;

use moa_core::upgrade::Why;
use moa_platform::child::Reaper;
use moa_platform::{PathWatch, Spawner, build_id_of, exe_chain, kill_term};
use tokio::io::AsyncReadExt as _;
use tokio::process::ChildStdout;

use crate::core_task::CoreHandle;

/// How long the program must stay unchanged before its build is read.
const SETTLE: Duration = Duration::from_millis(500);
/// The most of the new program's answer that is read.
const PROBE_MAX: u64 = 256;

/// How long a new program has to say its build: `first`, and, when it said nothing in that
/// time, `again`, asked once more at once (a first launch can be slow: a large program, a
/// busy machine, the system checking a new binary).
#[derive(Clone, Copy, Debug)]
pub struct ProbeWait {
    pub first: Duration,
    pub again: Duration,
}

impl Default for ProbeWait {
    fn default() -> ProbeWait {
        ProbeWait {
            first: Duration::from_secs(5),
            again: Duration::from_secs(15),
        }
    }
}

pub(crate) struct Follow {
    exe: PathBuf,
    wait: ProbeWait,
    /// This image's build.
    build: String,
    /// `None` once the watch couldn't be armed: the daemon goes on without one until its
    /// next start.
    watch: Option<PathWatch>,
    /// The last build that said its build and was wanted: each is wanted once. One that
    /// couldn't say it (not executable yet, or too slow twice) is asked again on the next
    /// change, which may leave its build as it was (a `chmod +x` keeps size and mtime).
    probed: Option<String>,
    /// The watch failed on the last change too: a second failure in a row turns it off,
    /// rather than arming it again and again.
    failed: bool,
}

impl Follow {
    /// Watches `exe`, the program of an image running `build`. Inside the runtime, and
    /// before the daemon says it is ready, so a change right after that is heard.
    pub(crate) fn new(exe: PathBuf, build: String, wait: ProbeWait) -> Follow {
        let watch = arm(&exe);
        Follow {
            exe,
            wait,
            build,
            watch,
            probed: None,
            failed: false,
        }
    }

    /// Follows the program until the task is cancelled.
    pub(crate) async fn run(mut self, core: CoreHandle) {
        loop {
            self.changed().await;
            loop {
                tokio::select! {
                    () = tokio::time::sleep(SETTLE) => break,
                    () = self.changed() => {}
                }
            }
            self.check(&core).await;
        }
    }

    /// Resolves after the next change, with the watch armed again from the chain as it now
    /// is. A failed watch counts as a change (a directory in the chain may have gone) and is
    /// armed again once; failing again straight after, it is turned off until the next start.
    async fn changed(&mut self) {
        let Some(watch) = &mut self.watch else {
            return std::future::pending().await;
        };
        match watch.changed().await {
            Ok(()) => self.failed = false,
            Err(e) if self.failed => {
                tracing::warn!(
                    "moa's watch on its program keeps failing ({}); it upgrades only when \
                     asked until it starts again",
                    e.kind()
                );
                self.watch = None;
                return;
            }
            Err(e) => {
                tracing::warn!("the program's watch failed ({}); watching again", e.kind());
                self.failed = true;
            }
        }
        self.watch = arm(&self.exe);
    }

    /// Reads the program's build; a new one that says so is an upgrade.
    async fn check(&mut self, core: &CoreHandle) {
        let build = match build_id_of(&self.exe) {
            Ok(build) => build,
            // Between an install's steps: the next change says more.
            Err(e) => {
                tracing::debug!("the program can't be read yet: {}", e.kind());
                return;
            }
        };
        if build == self.build || self.probed.as_ref() == Some(&build) {
            return;
        }
        let exe = self.exe.display();
        let mut said = probe(&self.exe, &build, self.wait.first).await;
        if said == Said::Nothing {
            // It said nothing about itself: asked once more, with longer to answer.
            tracing::info!("{exe} is slow to say its build; asking once more");
            said = probe(&self.exe, &build, self.wait.again).await;
        }
        if said == Said::Build {
            self.probed = Some(build.clone());
            tracing::info!("{exe} is a new build ({build}); it takes over between moa's turns");
            core.stats().builds_found.fetch_add(1, Ordering::SeqCst);
            core.want_upgrade(Why::Rebuilt);
        } else {
            match said {
                Said::Nothing => {
                    tracing::warn!("upgrade skipped: {exe} didn't say its build in time");
                }
                _ => tracing::warn!("upgrade skipped: {exe} isn't a whole moa yet"),
            }
            core.stats().builds_skipped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A watch on `exe`'s chain as it is now; `None`, said in the log, when it can't be armed.
fn arm(exe: &Path) -> Option<PathWatch> {
    match PathWatch::new(&exe_chain(exe)) {
        Ok(watch) => Some(watch),
        Err(e) => {
            tracing::warn!(
                "moa can't watch its program for new builds ({}); it upgrades when asked",
                e.kind()
            );
            None
        }
    }
}

/// What a new program said when asked for its build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Said {
    /// The build its file says: it is whole.
    Build,
    /// Another answer, or it couldn't start (half-written, not runnable yet).
    Wrong,
    /// Nothing within the limit: it says nothing about the program.
    Nothing,
}

/// What `exe build-id` says within `limit`, against the build `want` its file says.
async fn probe(exe: &Path, want: &str, limit: Duration) -> Said {
    match said(exe, limit).await {
        Ok(Some(out)) if out.trim() == want => Said::Build,
        Ok(Some(_)) => Said::Wrong,
        Ok(None) => Said::Nothing,
        Err(e) => {
            tracing::debug!("the new program didn't start: {}", e.kind());
            Said::Wrong
        }
    }
}

/// What `exe build-id` printed; `None` when it took longer than `limit` (it is stopped).
async fn said(exe: &Path, limit: Duration) -> io::Result<Option<String>> {
    let mut cmd = Command::new(exe);
    cmd.arg("build-id")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Spawner.spawn(&mut cmd)?;
    let stdout = child.stdout.take();
    let reaper = Reaper::watch(child, || {});
    let Some(stdout) = stdout else {
        return Ok(None);
    };
    let mut stdout = ChildStdout::from_std(stdout)?.take(PROBE_MAX);
    let answer = async {
        let mut out = String::new();
        let read = stdout.read_to_string(&mut out).await;
        reaper.exited().await;
        read.map(|_| out)
    };
    match tokio::time::timeout(limit, answer).await {
        Ok(out) => out.map(Some),
        Err(_) => {
            reaper.kill_unless_exited(kill_term);
            Ok(None)
        }
    }
}
