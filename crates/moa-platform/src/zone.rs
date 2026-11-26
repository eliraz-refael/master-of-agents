//! A watch on the system time zone.

use std::io;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "macos")]
use tokio::io::unix::AsyncFd;

#[cfg(target_os = "macos")]
use crate::sys;
#[cfg(target_os = "linux")]
use crate::watch::PathWatch;

/// Resolves when the system time zone may have changed.
#[derive(Debug)]
pub struct ZoneWatch {
    #[cfg(target_os = "macos")]
    fd: AsyncFd<sys::NotifyFd>,
    #[cfg(target_os = "linux")]
    watch: PathWatch,
}

impl ZoneWatch {
    /// Watch the real system zone. Must be called inside a tokio runtime.
    pub fn system() -> io::Result<ZoneWatch> {
        #[cfg(target_os = "macos")]
        return Self::notify_name("com.apple.system.timezone");
        // `/etc/localtime` is a symlink that gets replaced, so watch the directory.
        #[cfg(target_os = "linux")]
        return Self::file(Path::new("/etc"), "localtime");
    }

    /// Test only: watch an arbitrary notification name.
    #[cfg(target_os = "macos")]
    #[doc(hidden)]
    pub fn notify_name(name: &str) -> io::Result<ZoneWatch> {
        Ok(ZoneWatch {
            fd: sys::register_readable(sys::notify_open(name)?)?,
        })
    }

    /// Test only: watch the entry `name` in `dir`.
    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    pub fn file(dir: &Path, name: &str) -> io::Result<ZoneWatch> {
        Ok(ZoneWatch {
            watch: PathWatch::new(&[(dir.to_path_buf(), name.into())])?,
        })
    }

    /// Resolves after one or more changes since the last call. Cancel-safe.
    pub async fn changed(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        return self.watch.changed().await;
        #[cfg(target_os = "macos")]
        loop {
            let mut guard = self.fd.readable().await?;
            let changed = sys::notify_drain(self.fd.get_ref());
            // Drained to WouldBlock, so the readiness is spent either way.
            guard.clear_ready();
            if changed? {
                return Ok(());
            }
        }
    }
}

/// Test only: post a notification, as the system does when the zone changes.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn post_for_test(name: &str) -> io::Result<()> {
    sys::notify_post_name(name)
}
