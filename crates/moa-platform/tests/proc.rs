//! Process reads: argv, the kept environment, children and start time, in-process.

use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use moa_platform::kill_term;
use moa_platform::proc::{ENV_KEPT, Reader, children, started};

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
        for kid in children(self.0.id()) {
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
        children(pid).into_iter().find(|&k| {
            reader
                .args(k)
                .is_some_and(|a| a.argv.first().is_some_and(|w| w == "sleep"))
        })
    });
    assert!(kid.is_some(), "the shell's sleep was never listed");
    assert!(children(kid.unwrap()).is_empty());
}

#[test]
fn children_lists_every_child() {
    let sh = spawn(Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30 & sleep 30; :"]));
    let pid = sh.0.id();
    let kids = wait_for(|| Some(children(pid)).filter(|k| k.len() == 3));
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
fn args_of_a_gone_pid_is_none() {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(Reader::new().args(pid).is_none());
    assert!(children(pid).is_empty());
    assert!(started(pid).is_none());
    // Pid 0 is never read as a process.
    assert!(Reader::new().args(0).is_none());
    assert!(children(0).is_empty());
}
