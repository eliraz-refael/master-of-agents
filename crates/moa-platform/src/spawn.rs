//! Every product launch goes through [`Spawner`], which counts launches.

use std::io;
use std::os::fd::BorrowedFd;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static LAUNCHES: AtomicU64 = AtomicU64::new(0);

/// One launch at a time. A descriptor made inheritable for one child (`spawn_keeping`) must
/// not reach another started meanwhile; on macOS this also keeps the pipes `std` makes for
/// one child (close-on-exec is set after they're made) out of another.
static SPAWNING: Mutex<()> = Mutex::new(());

/// Spawns child processes and counts every attempt.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spawner;

impl Spawner {
    /// Adds 1 to the process-wide counter *before* spawning, so a failed launch counts too.
    pub fn spawn(&self, cmd: &mut Command) -> io::Result<Child> {
        let _one = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        LAUNCHES.fetch_add(1, Ordering::SeqCst);
        cmd.spawn()
    }

    /// [`Spawner::spawn`], with the child also holding a copy of `keep` (at a number of its
    /// own, unused) for as long as it runs. This process's copy is closed once the child has
    /// started.
    pub fn spawn_keeping(&self, cmd: &mut Command, keep: BorrowedFd<'_>) -> io::Result<Child> {
        let _one = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        LAUNCHES.fetch_add(1, Ordering::SeqCst);
        let inherited = crate::sys::dup_inheritable(keep)?;
        let child = cmd.spawn();
        drop(inherited);
        child
    }
}

/// How many launches this process has attempted.
pub fn launches() -> u64 {
    LAUNCHES.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd as _;
    use std::process::Stdio;

    use super::*;

    /// How many descriptors a child started this way has open.
    fn open_in_child(keep: Option<BorrowedFd<'_>>) -> usize {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "ls /dev/fd"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let child = match keep {
            Some(fd) => Spawner.spawn_keeping(&mut cmd, fd),
            None => Spawner.spawn(&mut cmd),
        };
        let out = child.unwrap().wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .count()
    }

    #[test]
    fn a_kept_descriptor_reaches_that_child_only() {
        let (r, _w) = std::io::pipe().unwrap();
        let plain = open_in_child(None);
        assert_eq!(open_in_child(Some(r.as_fd())), plain + 1);
        assert_eq!(open_in_child(None), plain, "this process's copy is closed");
    }
}
