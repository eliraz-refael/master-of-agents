//! A watch on the system time zone.

use std::io;
#[cfg(target_os = "linux")]
use std::{ffi::OsString, os::fd::OwnedFd, path::Path};

use tokio::io::unix::AsyncFd;

use crate::sys;

/// Resolves when the system time zone may have changed.
#[derive(Debug)]
pub struct ZoneWatch {
    #[cfg(target_os = "macos")]
    fd: AsyncFd<sys::NotifyFd>,
    #[cfg(target_os = "linux")]
    fd: AsyncFd<OwnedFd>,
    #[cfg(target_os = "linux")]
    name: OsString,
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
            fd: sys::register_readable(sys::notify_open(dir)?)?,
            name: name.into(),
        })
    }

    /// Resolves after one or more changes since the last call. Cancel-safe.
    pub async fn changed(&mut self) -> io::Result<()> {
        loop {
            let mut guard = self.fd.readable().await?;
            #[cfg(target_os = "macos")]
            let changed = sys::notify_drain(self.fd.get_ref());
            #[cfg(target_os = "linux")]
            let changed = sys::notify_drain(self.fd.get_ref(), &self.name);
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
