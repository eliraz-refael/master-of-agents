//! A watch on a few directory entries whose changes are heard once they settle (L3, U5): the
//! daemon's program ([`crate::follow`]) and its config files ([`crate::config_watch`]).
//!
//! After a change the watch is armed again from the entries as they now are (a symlink
//! retargeted names other ones), and a change is heard once [`SETTLE`] has gone by without
//! another: an install's or an editor's several steps are one change. Kernel events only;
//! the settle's timer is armed only after one.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use moa_platform::PathWatch;

/// How long the entries must stay unchanged before a change is heard.
pub const SETTLE: Duration = Duration::from_millis(500);

type Entries = Box<dyn Fn() -> Vec<(PathBuf, OsString)> + Send>;

pub(crate) struct Settled {
    /// What is watched, as the log names it (`its program`).
    what: &'static str,
    /// What moa does without the watch, as the log says it (`it upgrades when asked`).
    without: &'static str,
    entries: Entries,
    /// `None` once the watch couldn't be armed: the daemon goes on without one until its
    /// next start.
    watch: Option<PathWatch>,
    /// The watch failed on the last change too: a second failure in a row turns it off,
    /// rather than arming it again and again.
    failed: bool,
}

impl Settled {
    /// Watches `entries()`, armed now. Inside the runtime, and before the daemon says it is
    /// ready, so a change right after that is heard.
    pub(crate) fn new(
        what: &'static str,
        without: &'static str,
        entries: impl Fn() -> Vec<(PathBuf, OsString)> + Send + 'static,
    ) -> Settled {
        let entries: Entries = Box::new(entries);
        let watch = arm(what, without, &entries);
        Settled {
            what,
            without,
            entries,
            watch,
            failed: false,
        }
    }

    /// Resolves once a change has been followed by [`SETTLE`] without another. Never, once
    /// the watch is off.
    pub(crate) async fn next(&mut self) {
        self.changed().await;
        loop {
            tokio::select! {
                () = tokio::time::sleep(SETTLE) => return,
                () = self.changed() => {}
            }
        }
    }

    /// Resolves after the next change, with the watch armed again from the entries as they
    /// now are. A failed watch counts as a change (a directory may have gone) and is armed
    /// again once; failing again straight after, it is turned off until the next start.
    async fn changed(&mut self) {
        let Some(watch) = &mut self.watch else {
            return std::future::pending().await;
        };
        match watch.changed().await {
            Ok(()) => self.failed = false,
            Err(e) if self.failed => {
                tracing::warn!(
                    "moa's watch on {} keeps failing ({}), so it is off until moa starts \
                     again; {}",
                    self.what,
                    e.kind(),
                    self.without
                );
                self.watch = None;
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "moa's watch on {} failed ({}); watching again",
                    self.what,
                    e.kind()
                );
                self.failed = true;
            }
        }
        self.watch = arm(self.what, self.without, &self.entries);
    }
}

/// A watch on the entries as they are now; `None`, said in the log, when it can't be armed.
fn arm(what: &str, without: &str, entries: &Entries) -> Option<PathWatch> {
    match PathWatch::new(&entries()) {
        Ok(watch) => Some(watch),
        Err(e) => {
            tracing::warn!("moa can't watch {what} ({}); {without}", e.kind());
            None
        }
    }
}
