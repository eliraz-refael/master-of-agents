//! The accept loop: one task per connection, and only the daemon's own user gets in.

use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::Duration;

use moa_core::config::BrainName;
use moa_platform::peer_uid;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::core_task::CoreHandle;
use crate::session::{peer_allowed, serve};

/// Pause after a failed `accept` (out of descriptors, say), so a persistent error doesn't spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(200);

/// Accepts until `stop`; sessions get `stop` as their shutdown token and `close` to end on. A connection from another user is closed and logged.
pub(crate) async fn accept_loop(
    listener: Arc<UnixListener>,
    core: CoreHandle,
    brain: BrainName,
    stop: CancellationToken,
    close: CancellationToken,
    sessions: TaskTracker,
    my_uid: u32,
) {
    loop {
        let accepted = tokio::select! {
            _ = stop.cancelled() => return,
            r = listener.accept() => r,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!("accept failed: {}", e.kind());
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(ACCEPT_RETRY) => {}
                }
                continue;
            }
        };
        // Not `peer_cred`: on macOS it fails for a peer that has already closed, and a hook
        // often has, having written its line and exited.
        match peer_uid(stream.as_fd()) {
            Ok(uid) if peer_allowed(uid, my_uid) => {
                sessions.spawn(serve(
                    stream,
                    core.clone(),
                    brain.clone(),
                    stop.clone(),
                    close.clone(),
                ));
            }
            Ok(uid) => tracing::warn!("closed a connection from user {uid}"),
            Err(e) => tracing::warn!("closed a connection whose user is unknown: {}", e.kind()),
        }
    }
}
