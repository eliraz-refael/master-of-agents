//! Every product launch goes through [`Spawner`], which counts launches.

use std::io::{self, PipeReader};
use std::os::fd::AsFd as _;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static LAUNCHES: AtomicU64 = AtomicU64::new(0);

/// One launch at a time. A descriptor made inheritable for one child
/// (`spawn_with_held_stdout`) must not reach another started meanwhile. On macOS, where
/// close-on-exec is set only after a pipe is made, this also keeps the pipes made under it
/// (`std`'s for a child's stdio, and `spawn_with_held_stdout`'s) out of other children. A
/// pipe made elsewhere isn't covered.
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

    /// [`Spawner::spawn`], with the child's stdout a new pipe whose read end is returned.
    /// The child also holds a copy of that read end (at a number of its own, unused) while
    /// it runs, so its writes don't fail for want of a reader until the pipe is full. The
    /// pipe is made under the lock, so no other child gets either end, and this process
    /// keeps only the returned read end.
    pub fn spawn_with_held_stdout(&self, cmd: &mut Command) -> io::Result<(Child, PipeReader)> {
        let _one = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        LAUNCHES.fetch_add(1, Ordering::SeqCst);
        let (read, write) = std::io::pipe()?;
        let inherited = crate::sys::dup_inheritable(read.as_fd())?;
        cmd.stdout(Stdio::from(write));
        let child = cmd.spawn();
        drop(inherited);
        // `cmd` held the write end: the reader sees the end only once the child's copies go.
        cmd.stdout(Stdio::null());
        Ok((child?, read))
    }
}

/// How many launches this process has attempted.
pub fn launches() -> u64 {
    LAUNCHES.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    /// How many descriptors a child started this way has open (`ls /dev/fd`).
    fn open_in_child(held: bool) -> usize {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "ls /dev/fd"]).stdin(Stdio::null());
        let mut text = String::new();
        if held {
            let (mut child, mut out) = Spawner.spawn_with_held_stdout(&mut cmd).unwrap();
            // This ends only if no write end is left here: `cmd`'s copy was dropped.
            out.read_to_string(&mut text).unwrap();
            child.wait().unwrap();
        } else {
            cmd.stdout(Stdio::piped());
            let out = Spawner.spawn(&mut cmd).unwrap().wait_with_output().unwrap();
            text = String::from_utf8_lossy(&out.stdout).into_owned();
        }
        text.split_whitespace().count()
    }

    #[test]
    fn a_held_stdout_reaches_that_child_only() {
        let plain = open_in_child(false);
        assert_eq!(
            open_in_child(true),
            plain + 1,
            "the child holds the read end"
        );
        assert_eq!(open_in_child(false), plain, "this process's copy is closed");
    }
}
