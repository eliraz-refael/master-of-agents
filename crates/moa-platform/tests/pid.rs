use std::process::Command;

use moa_platform::{kill_term, pid_alive};

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
    // `kill(0, ..)` would address our own process group, and a pid past i32::MAX wraps
    // negative (a whole group again): neither may reach the system call.
    assert!(!pid_alive(0));
    assert!(!pid_alive(u32::MAX));
    kill_term(0);
    kill_term(u32::MAX);
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
