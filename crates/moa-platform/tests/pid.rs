use std::process::Command;

use moa_platform::{kill_term, pid_alive, wait_exited};

#[test]
fn pid_alive_sees_itself_and_not_a_reaped_child() {
    assert!(pid_alive(std::process::id()));
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!pid_alive(pid));
}

#[test]
fn pid_zero_and_out_of_range_pids_are_not_alive() {
    // Only signal 0 here. `kill_term` is never called with 0 or a wrapping pid, even in a
    // test: a regression would signal a whole process group. The guard shared by both is
    // unit-tested in `sys`.
    assert!(!pid_alive(0));
    assert!(!pid_alive(u32::MAX));
}

#[test]
fn kill_term_stops_a_child() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    assert!(pid_alive(child.id()));
    kill_term(child.id());
    let status = child.wait().unwrap();
    assert!(!status.success());
    assert!(!pid_alive(child.id()));
}

#[test]
fn wait_exited_leaves_the_child_to_reap() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 3"])
        .spawn()
        .unwrap();
    let pid = child.id();
    wait_exited(pid).unwrap();
    // Exited but not reaped: a zombie, so the pid can't be reused yet.
    assert!(pid_alive(pid));
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3), "{:?}", status.signal());
    assert!(!pid_alive(pid));
}
