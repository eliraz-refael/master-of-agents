//! The signals that stop a panel (L14).

use std::future::pending;
use std::io;

use futures_util::FutureExt;
use tokio::signal::unix::{Signal, SignalKind, signal};

/// The signals that stop a panel (L14): `kill`'s SIGTERM, the SIGHUP tmux sends a tab it
/// kills, and SIGINT. Each ends the loop as Ctrl-c does, so the terminal is given back.
pub(super) struct Signals {
    term: Signal,
    hup: Signal,
    int: Signal,
}

impl Signals {
    pub(super) fn new() -> io::Result<Signals> {
        Ok(Signals {
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// The next signal's name.
    pub(super) async fn recv(&mut self) -> &'static str {
        tokio::select! {
            Some(()) = self.term.recv() => "SIGTERM",
            Some(()) = self.hup.recv() => "SIGHUP",
            Some(()) = self.int.recv() => "SIGINT",
            else => pending().await,
        }
    }

    /// A signal that has already come, if any. One yield first lets the runtime take in
    /// a signal that arrived with the wake being handled.
    pub(super) async fn pending(&mut self) -> Option<&'static str> {
        tokio::task::yield_now().await;
        self.recv().now_or_never()
    }
}
