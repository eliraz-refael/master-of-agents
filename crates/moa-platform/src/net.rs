//! A Unix-socket connect that cannot hang a hook.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// Connects to the socket at `path`, giving up after `limit` with `TimedOut`. The connect
/// runs on a helper thread; on timeout it is left to finish on its own.
pub fn connect_within(path: &Path, limit: Duration) -> io::Result<UnixStream> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        // The receiver is gone after a timeout; the late stream is simply dropped.
        let _ = tx.send(UnixStream::connect(path));
    });
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")),
    }
}
