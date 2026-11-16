//! One client connection: lines in, replies and notices out.
//!
//! Backpressure: a connection has at most one op in flight. The session reads its next line
//! only after the previous op's reply has been written, so what one connection can queue in
//! the core's unbounded inbox and reply channel is one request. Notices can't pile up
//! either: the model is a `watch` (a slow reader sees the latest, as one patch), events are
//! a bounded `broadcast` (a slow reader gets a fresh snapshot instead of the ones it missed).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use moa_core::config::BrainName;
use moa_core::op::{ConnId, OpError, ReqId};
use moa_core::state::CallerInfo;
use moa_proto::{
    Call, CoreEvent, Hello, HelloOk, Id, Incoming, Model, Patch, RpcError, StatusOk, Subscribe,
    Topic, allowed_on_mismatch, code, decode_incoming, encode_error, encode_notice, encode_result,
    snapshot_of,
};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::{broadcast, watch};
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};
use tokio_util::sync::CancellationToken;

use crate::core_task::{ConnReply, CoreHandle};

/// How much input is thrown away after an over-long line, looking for its end.
const DISCARD_LIMIT: usize = 16 << 20;
/// How long that may take.
const DISCARD_TIME: Duration = Duration::from_secs(5);

const MISMATCH: &str = "this moa is a different build; run the command again to restart it";
const STOPPING: &str = "moa is stopping; try again";

/// Whether a peer with this uid may talk to a daemon run by `my_uid`.
pub fn peer_allowed(peer_uid: u32, my_uid: u32) -> bool {
    peer_uid == my_uid
}

fn rpc(code: i64, message: &str) -> RpcError {
    RpcError {
        code,
        message: message.to_string(),
    }
}

fn method_of(call: &Call) -> &'static str {
    match call {
        Call::Hello(_) => "hello",
        Call::Status => "status",
        Call::Shutdown => "shutdown",
        Call::Upgrade => "upgrade",
        Call::Subscribe(_) => "subscribe",
        Call::Op(op) => op.method(),
    }
}

/// Closes the connection's reply channel in the core however the session ends.
struct ConnGuard {
    core: CoreHandle,
    conn: ConnId,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.core.close_conn(self.conn);
    }
}

/// What `hello` told us.
struct Greeted {
    caller: CallerInfo,
    matched: bool,
}

/// A connection's subscription: what it follows and what it was last sent.
struct Sub {
    topics: Vec<Topic>,
    last: Arc<Model>,
    model: watch::Receiver<Arc<Model>>,
    generation: watch::Receiver<u64>,
    seen_generation: u64,
    events: broadcast::Receiver<CoreEvent>,
}

enum Push {
    /// The model or the generation changed.
    State,
    Event(CoreEvent),
    /// Events were missed.
    Missed,
}

/// The next thing to push to a subscriber; never resolves without a subscription.
async fn next_push(sub: &mut Option<Sub>) -> Push {
    let Some(sub) = sub else {
        return std::future::pending().await;
    };
    tokio::select! {
        r = sub.model.changed() => match r {
            Ok(()) => Push::State,
            Err(_) => std::future::pending().await,
        },
        r = sub.generation.changed() => match r {
            Ok(()) => Push::State,
            Err(_) => std::future::pending().await,
        },
        r = sub.events.recv() => match r {
            Ok(e) => Push::Event(e),
            Err(broadcast::error::RecvError::Lagged(_)) => Push::Missed,
            Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
        },
    }
}

/// Whether the session goes on.
#[derive(PartialEq)]
enum Flow {
    Go,
    Close,
}

struct Session<S> {
    framed: Framed<S, LinesCodec>,
    core: CoreHandle,
    brain: BrainName,
    stop: CancellationToken,
    conn: ConnId,
    replies: tokio::sync::mpsc::UnboundedReceiver<ConnReply>,
    greeted: Option<Greeted>,
    sub: Option<Sub>,
    /// The op in flight: the core's id for it and the client's.
    pending: Option<(ReqId, Id)>,
    next_req: u64,
}

/// Serves one connection until it closes or `stop` is cancelled. A `shutdown` or `upgrade`
/// request cancels `stop` once its reply is written.
pub async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    core: CoreHandle,
    brain: BrainName,
    stop: CancellationToken,
) {
    let (conn, replies) = core.open_conn();
    let _guard = ConnGuard {
        core: core.clone(),
        conn,
    };
    tracing::debug!(conn = conn.0, "connection opened");
    let mut session = Session {
        framed: Framed::new(stream, LinesCodec::new_with_max_length(moa_proto::MAX_LINE)),
        core,
        brain,
        stop,
        conn,
        replies,
        greeted: None,
        sub: None,
        pending: None,
        next_req: 0,
    };
    session.run().await;
    tracing::debug!(conn = conn.0, "connection closed");
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Session<S> {
    async fn run(&mut self) {
        loop {
            let flow = tokio::select! {
                _ = self.stop.cancelled() => Flow::Close,
                // One op at a time: nothing is read while one is in flight.
                line = self.framed.next(), if self.pending.is_none() => match line {
                    Some(Ok(line)) => self.on_line(&line).await,
                    Some(Err(LinesCodecError::MaxLineLengthExceeded)) => self.too_long().await,
                    Some(Err(LinesCodecError::Io(_))) | None => Flow::Close,
                },
                reply = self.replies.recv() => self.on_reply(reply).await,
                push = next_push(&mut self.sub) => self.on_push(push).await,
            };
            if flow == Flow::Close {
                return;
            }
        }
    }

    /// Writes one line; `false` when the connection is gone or the daemon is stopping.
    async fn send(&mut self, line: String) -> bool {
        tokio::select! {
            r = self.framed.send(line) => r.is_ok(),
            _ = self.stop.cancelled() => false,
        }
    }

    async fn say(&mut self, line: String) -> Flow {
        if self.send(line).await {
            Flow::Go
        } else {
            Flow::Close
        }
    }

    async fn refuse(&mut self, id: &Id, code: i64, message: &str) -> Flow {
        self.say(encode_error(Some(id), &rpc(code, message))).await
    }

    async fn on_line(&mut self, line: &str) -> Flow {
        if line.trim().is_empty() {
            return Flow::Go;
        }
        match decode_incoming(line) {
            Err((id, e)) => self.say(encode_error(id.as_ref(), &e)).await,
            Ok(Incoming::Ignored) => Flow::Go,
            // A hook is never answered, with an id or without (slice 2b).
            Ok(Incoming::Hook { id, .. }) => {
                tracing::debug!(conn = self.conn.0, with_id = id.is_some(), "hook dropped");
                Flow::Go
            }
            Ok(Incoming::Request { id, call }) => self.on_call(id, call).await,
        }
    }

    async fn on_call(&mut self, id: Id, call: Call) -> Flow {
        tracing::debug!(conn = self.conn.0, method = method_of(&call), "request");
        match &self.greeted {
            None if !matches!(call, Call::Hello(_)) => {
                return self.refuse(&id, code::HELLO_FIRST, "say hello first").await;
            }
            Some(g) if !g.matched && !allowed_on_mismatch(&call) => {
                return self.refuse(&id, code::BUILD_MISMATCH, MISMATCH).await;
            }
            _ => {}
        }
        match call {
            Call::Hello(hello) => self.hello(&id, hello).await,
            Call::Status => self.status(&id).await,
            Call::Shutdown | Call::Upgrade => {
                let flow = self.say(encode_result(&id, &json!({}))).await;
                tracing::info!(conn = self.conn.0, "asked to stop");
                self.stop.cancel();
                if flow == Flow::Go { Flow::Close } else { flow }
            }
            Call::Subscribe(s) => self.subscribe(&id, s).await,
            Call::Op(op) => {
                self.next_req += 1;
                let req = ReqId(self.next_req);
                self.pending = Some((req, id));
                let caller = self.greeted.as_ref().map(|g| g.caller.clone());
                self.core
                    .send(self.conn, req, caller.unwrap_or_default(), op);
                Flow::Go
            }
        }
    }

    async fn hello(&mut self, id: &Id, hello: Hello) -> Flow {
        let build = moa_platform::build_id();
        let matched = hello.build == build;
        tracing::debug!(conn = self.conn.0, client = %hello.client, matched, "hello");
        self.greeted = Some(Greeted {
            caller: hello.caller,
            matched,
        });
        let rev = self.core.model().borrow().rev;
        let ok = HelloOk {
            brain: self.brain.to_string(),
            build: build.to_string(),
            rev,
            matched,
            pid: std::process::id(),
        };
        self.say(encode_result(id, &ok)).await
    }

    async fn status(&mut self, id: &Id) -> Flow {
        let caller = self
            .greeted
            .as_ref()
            .map(|g| g.caller.clone())
            .unwrap_or_default();
        let data = self.core.status(caller).await;
        let ok = StatusOk {
            brain: data.brain,
            build: moa_platform::build_id().to_string(),
            pid: std::process::id(),
            launches: moa_platform::spawn::launches(),
            sessions: data.sessions,
            caller: data.caller,
        };
        self.say(encode_result(id, &ok)).await
    }

    /// Always a full snapshot (`since` is not used yet), then patches and events.
    async fn subscribe(&mut self, id: &Id, s: Subscribe) -> Flow {
        let events = self.core.events();
        let mut model = self.core.model();
        let mut generation = self.core.generation();
        let last = model.borrow_and_update().clone();
        let seen_generation = *generation.borrow_and_update();
        let snapshot = snapshot_of(&last, &s.topics);
        self.sub = Some(Sub {
            topics: s.topics,
            last,
            model,
            generation,
            seen_generation,
            events,
        });
        self.say(encode_result(id, &snapshot)).await
    }

    async fn on_reply(&mut self, reply: Option<ConnReply>) -> Flow {
        let Some((req, result)) = reply else {
            // The core cleared its connections: it has stopped.
            if let Some((_, id)) = self.pending.take() {
                self.refuse(&id, code::FAILED, STOPPING).await;
            }
            return Flow::Close;
        };
        match self.pending.take() {
            Some((want, id)) if want == req => match result {
                Ok(done) => self.say(encode_result(&id, &done)).await,
                Err(OpError(message)) => self.refuse(&id, code::FAILED, &message).await,
            },
            // Not ours to answer: put back what was there.
            other => {
                self.pending = other;
                Flow::Go
            }
        }
    }

    async fn on_push(&mut self, push: Push) -> Flow {
        let Some(sub) = &mut self.sub else {
            return Flow::Go;
        };
        let line = match push {
            Push::Event(e) => encode_notice("event", &e),
            Push::State | Push::Missed => {
                let cur = sub.model.borrow_and_update().clone();
                let generation = *sub.generation.borrow_and_update();
                let reloaded = generation != sub.seen_generation;
                let patch = if reloaded || matches!(push, Push::Missed) {
                    None
                } else {
                    Patch::between(&sub.last, &cur, &sub.topics)
                };
                let line = match patch {
                    Some(p) => Some(encode_notice("patch", &p)),
                    None if reloaded || matches!(push, Push::Missed) => {
                        Some(encode_notice("snapshot", &snapshot_of(&cur, &sub.topics)))
                    }
                    None => None,
                };
                sub.last = cur;
                sub.seen_generation = generation;
                match line {
                    Some(line) => line,
                    None => return Flow::Go,
                }
            }
        };
        self.say(line).await
    }

    /// Says so, then reads and drops the rest of the line (bounded) before closing, so that
    /// closing with unread input doesn't reset the connection before the client reads this.
    async fn too_long(&mut self) -> Flow {
        tracing::warn!(conn = self.conn.0, "a line was too long");
        let line = encode_error(None, &rpc(code::TOO_LONG, "that message is too long"));
        self.send(line).await;
        if !self.framed.read_buffer().contains(&b'\n') {
            let io = self.framed.get_mut();
            let _ = tokio::time::timeout(DISCARD_TIME, async {
                let mut left = DISCARD_LIMIT;
                let mut buf = vec![0u8; 64 << 10];
                while left > 0 {
                    match io.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) if buf[..n].contains(&b'\n') => break,
                        Ok(n) => left = left.saturating_sub(n),
                    }
                }
            })
            .await;
        }
        Flow::Close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_own_uid_is_allowed() {
        assert!(peer_allowed(501, 501) && !peer_allowed(502, 501));
    }
}
