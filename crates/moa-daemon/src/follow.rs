//! Following the daemon's program (L3): a new build put at its path upgrades the daemon.
//!
//! The watch hears every entry of the program's symlink chain ([`moa_platform::exe_chain`]),
//! so a rebuild renamed over it and a profile switch that retargets a link are both heard,
//! with no polling. After a change the watch is armed again from the chain as it now is, and
//! once changes have settled for [`SETTLE`] the program's build is read. A build other than
//! this image's is asked for its build (`<exe> build-id`, at most [`PROBE_WAIT`]): a program
//! caught half-written can't say it, and is skipped until the next change. One that does
//! becomes `Input::Upgrade(Rebuilt)`, and the core takes it from there.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use moa_core::upgrade::Why;
use moa_platform::child::Reaper;
use moa_platform::{PathWatch, Spawner, build_id_of, exe_chain, kill_term};
use tokio::io::AsyncReadExt as _;
use tokio::process::ChildStdout;

use crate::core_task::CoreHandle;

/// How long the program must stay unchanged before its build is read.
const SETTLE: Duration = Duration::from_millis(500);
/// How long the new program has to say its build.
const PROBE_WAIT: Duration = Duration::from_secs(5);
/// The most of the new program's answer that is read.
const PROBE_MAX: u64 = 256;

pub(crate) struct Follow {
    exe: PathBuf,
    /// This image's build.
    build: String,
    /// `None` once the watch couldn't be armed: the daemon goes on without one until its
    /// next start.
    watch: Option<PathWatch>,
    /// The last build asked for its build, whatever it said: each is asked once.
    probed: Option<String>,
    /// The watch failed on the last change too: a second failure in a row turns it off,
    /// rather than arming it again and again.
    failed: bool,
}

impl Follow {
    /// Watches `exe`, the program of an image running `build`. Inside the runtime, and
    /// before the daemon says it is ready, so a change right after that is heard.
    pub(crate) fn new(exe: PathBuf, build: String) -> Follow {
        let watch = arm(&exe);
        Follow {
            exe,
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
        self.probed = Some(build.clone());
        let exe = self.exe.display();
        if probe(&self.exe, &build).await {
            tracing::info!("{exe} is a new build ({build}); it takes over between moa's turns");
            core.want_upgrade(Why::Rebuilt);
        } else {
            tracing::warn!("upgrade skipped: {exe} isn't a whole moa yet");
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

/// Whether `exe build-id` prints `want` within [`PROBE_WAIT`]: the program is whole, and is
/// the build its file says.
async fn probe(exe: &Path, want: &str) -> bool {
    match said(exe).await {
        Ok(Some(out)) => out.trim() == want,
        Ok(None) => false,
        Err(e) => {
            tracing::debug!("the new program didn't start: {}", e.kind());
            false
        }
    }
}

/// What `exe build-id` printed; `None` when it took longer than [`PROBE_WAIT`] (it is
/// stopped).
async fn said(exe: &Path) -> io::Result<Option<String>> {
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
    match tokio::time::timeout(PROBE_WAIT, answer).await {
        Ok(out) => out.map(Some),
        Err(_) => {
            reaper.kill_unless_exited(kill_term);
            Ok(None)
        }
    }
}
