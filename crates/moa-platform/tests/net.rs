use moa_platform::net::connect_within;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

#[test]
fn connect_within_reaches_a_listener() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s.sock");
    let _listener = UnixListener::bind(&sock).unwrap();
    connect_within(&sock, Duration::from_secs(2)).unwrap();
}

#[test]
fn connect_within_fails_fast_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = connect_within(&dir.path().join("none.sock"), Duration::from_secs(2)).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(50));
    assert!(matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    ));
}
