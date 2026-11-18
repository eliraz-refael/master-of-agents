//! Process reads: argv, the kept environment, children and start time, in-process.

use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use moa_platform::kill_term;
use moa_platform::proc::{ENV_KEPT, Reader, Unread, children, owned_by_another, started};

/// Polls `f` every 20 ms for up to 10 s.
fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A child that is stopped when dropped, with its own children first, so a failed assertion
/// leaves no `sleep` behind.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        for kid in children(self.0.id()).unwrap_or_default() {
            kill_term(kid);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(cmd: &mut Command) -> Guard {
    Guard(cmd.spawn().unwrap())
}

#[test]
fn args_reads_argv_and_only_the_kept_env() {
    let path = std::env::var_os("PATH").unwrap();
    let child = spawn(
        Command::new("/bin/sh")
            .args(["-c", "exec sleep 30", "x"])
            .env_clear()
            .env("PATH", path)
            .env("MOA_TITLE", "t")
            .env("SECRET", "s"),
    );
    let pid = child.0.id();
    let mut reader = Reader::new();
    let args = wait_for(|| {
        reader
            .args(pid)
            .ok()
            .filter(|a| a.argv.first().is_some_and(|w| w == "sleep"))
    })
    .expect("sh never became sleep");
    assert_eq!(args.argv, ["sleep", "30"]);
    assert!(
        args.env
            .contains(&("MOA_TITLE".to_string(), "t".to_string())),
        "{:?}",
        args.env.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
    assert!(args.env.iter().all(|(k, _)| ENV_KEPT.contains(&k.as_str())));
    assert!(!args.env.iter().any(|(k, _)| k == "SECRET"));
    // The same reader serves the next call.
    assert_eq!(reader.args(pid).unwrap().argv, ["sleep", "30"]);
}

#[test]
fn children_lists_a_shells_child() {
    let sh = spawn(Command::new("/bin/sh").args(["-c", "sleep 30; :"]));
    let pid = sh.0.id();
    let mut reader = Reader::new();
    let kid = wait_for(|| {
        children(pid).ok()?.into_iter().find(|&k| {
            reader
                .args(k)
                .is_ok_and(|a| a.argv.first().is_some_and(|w| w == "sleep"))
        })
    });
    assert!(kid.is_some(), "the shell's sleep was never listed");
    assert_eq!(children(kid.unwrap()), Ok(Vec::new()));
}

#[test]
fn children_lists_every_child() {
    let sh = spawn(Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30 & sleep 30; :"]));
    let pid = sh.0.id();
    let kids = wait_for(|| children(pid).ok().filter(|k| k.len() == 3));
    assert!(kids.is_some(), "{:?}", children(pid));
}

#[test]
fn started_is_close_to_the_spawn() {
    let now = SystemTime::now();
    let child = spawn(Command::new("sleep").arg("30"));
    let at = started(child.0.id()).expect("a start time");
    let gap = match at.duration_since(now) {
        Ok(d) => d,
        Err(e) => e.duration(),
    };
    assert!(gap < Duration::from_secs(60), "{gap:?}");
}

#[test]
fn a_pid_that_doesnt_exist_reads_as_gone() {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert_eq!(Reader::new().args(pid), Err(Unread::Gone));
    assert_eq!(children(pid), Err(Unread::Gone));
    assert!(started(pid).is_none());
    // A pid past any system's pid limit, and pid 0, are never a process.
    for pid in [i32::MAX as u32, 0] {
        assert_eq!(Reader::new().args(pid), Err(Unread::Gone));
        assert_eq!(children(pid), Err(Unread::Gone));
    }
}

/// A long-lived process another user owns, if this machine shows one: pid 1, else one of
/// its children. A build sandbox's pid namespace may hold only the build user's.
fn another_users_process() -> Option<u32> {
    std::iter::once(1)
        .chain(children(1).unwrap_or_default())
        .find(|&p| owned_by_another(p) == Some(true))
}

/// Whether this runs in a Nix build sandbox, whose pid namespace holds only the build
/// user's processes. `nix develop` sets `NIX_BUILD_TOP` too, and `IN_NIX_SHELL` with it.
fn in_nix_build() -> bool {
    std::env::var_os("NIX_BUILD_TOP").is_some() && std::env::var_os("IN_NIX_SHELL").is_none()
}

/// Another user's process is never "couldn't read" nor gone: its arguments are closed to us
/// (foreign), or open (we are root). Every machine shows one (pid 1 is root's) except a Nix
/// build sandbox: only there is the rule left to `proc`'s `why_a_read_failed`, with made-up
/// owners. Anywhere else, finding none means owners can't be read, and fails.
#[test]
fn another_users_process_reads_as_foreign_not_failed() {
    let Some(pid) = another_users_process() else {
        assert!(
            in_nix_build(),
            "no other user's process is visible: can moa read who owns one?"
        );
        eprintln!(
            "in a Nix build, no other user's process is visible; `why_a_read_failed` covers the rule"
        );
        return;
    };
    let read = Reader::new().args(pid);
    assert!(
        read.is_ok() || read == Err(Unread::Foreign),
        "pid {pid}: {read:?}"
    );
    if moa_platform::current_uid() != 0 {
        assert_eq!(read, Err(Unread::Foreign), "pid {pid}");
    }
}

#[test]
fn who_owns_a_process_is_read_for_any_user() {
    assert_eq!(owned_by_another(std::process::id()), Some(false));
    let child = spawn(Command::new("sleep").arg("30"));
    assert_eq!(owned_by_another(child.0.id()), Some(false));
    // Pid 1 exists everywhere (a sandbox's is the build user's), and its owner reads.
    assert!(owned_by_another(1).is_some());
    for pid in [i32::MAX as u32, 0] {
        assert_eq!(owned_by_another(pid), None);
    }
}

#[test]
fn a_live_process_reads_as_itself() {
    let me = std::process::id();
    assert!(Reader::new().args(me).is_ok_and(|a| !a.argv.is_empty()));
    assert!(children(me).is_ok());
}
