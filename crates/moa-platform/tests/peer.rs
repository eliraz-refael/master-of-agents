use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};

use moa_platform::{current_uid, peer_uid};

/// A hook writes its line and exits at once, often before the daemon accepts: its user is
/// still known then. (tokio's `peer_cred` asks macOS for the peer's pid too, which fails
/// once the peer has closed.)
#[test]
fn a_peer_that_already_left_still_has_a_user() {
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let listener = UnixListener::bind(dir.path().join("s")).unwrap();
    let mut gone = UnixStream::connect(dir.path().join("s")).unwrap();
    gone.write_all(b"one line\n").unwrap();
    drop(gone);
    let (accepted, _) = listener.accept().unwrap();
    assert_eq!(peer_uid(accepted.as_fd()).unwrap(), current_uid());

    let _here = UnixStream::connect(dir.path().join("s")).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    assert_eq!(peer_uid(accepted.as_fd()).unwrap(), current_uid());
}

/// Anything but a Unix socket fails closed: there is no peer user, so the daemon turns the
/// connection away.
#[test]
fn a_pipe_has_no_peer_user() {
    let (read, write) = std::io::pipe().unwrap();
    assert!(peer_uid(read.as_fd()).is_err());
    assert!(peer_uid(write.as_fd()).is_err());
}
