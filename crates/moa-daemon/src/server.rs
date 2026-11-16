//! The accept loop: one task per connection, and only the daemon's own user gets in.

use std::sync::Arc;
use std::time::Duration;

use moa_core::config::BrainName;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use crate::core_task::CoreHandle;
use crate::session::{peer_allowed, serve};

/// Pause after a failed `accept` (out of descriptors, say), so a persistent error doesn't spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(200);

/// Accepts until `stop`. A connection from another user is closed and logged.
pub(crate) async fn accept_loop(
    listener: Arc<UnixListener>,
    core: CoreHandle,
    brain: BrainName,
    stop: CancellationToken,
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
        match stream.peer_cred() {
            Ok(cred) if peer_allowed(cred.uid(), my_uid) => {
                tokio::spawn(serve(stream, core.clone(), brain.clone(), stop.clone()));
            }
            Ok(cred) => tracing::warn!("closed a connection from user {}", cred.uid()),
            Err(e) => tracing::warn!("closed a connection whose user is unknown: {}", e.kind()),
        }
    }
}
