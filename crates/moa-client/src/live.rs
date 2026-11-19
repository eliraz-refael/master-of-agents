//! [`Live`]: a subscribed connection that never makes its user wait. A task owns the
//! connection; an op goes out at once and its reply comes back by tag, while pushes keep
//! arriving in order.

use std::collections::{BTreeMap, VecDeque};
use std::future::poll_fn;
use std::task::{Context, Poll, ready};

use futures_util::{SinkExt, StreamExt};
use moa_core::op::{Done, Op};
use moa_proto::{CoreEvent, Id, Patch, ServerLine, SnapshotMsg, Topic};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

use crate::{Client, ClientError, Wire, decode, server_line};

/// What a [`Live`] connection hands its user.
#[derive(Clone, PartialEq, Debug)]
pub enum LiveMsg {
    Patch(Patch),
    Snapshot(SnapshotMsg),
    Event(CoreEvent),
    /// The answer to the op [`Live::send`] tagged `tag`: its reply, or the line that says why
    /// there is none.
    Reply {
        tag: u64,
        result: Result<Done, String>,
    },
    /// The connection has ended. Said once, after every reply still owed. An op sent after
    /// the end is answered at once, so its reply can come after this.
    Closed,
}

/// A subscribed connection. Dropping it closes the connection.
///
/// Pushes wait in an unbounded queue until [`Live::recv`] takes them, so the panel loop must
/// drain `recv` on every wake.
pub struct Live {
    ops: mpsc::UnboundedSender<(u64, Op)>,
    msgs: mpsc::UnboundedReceiver<LiveMsg>,
    /// The last tag handed out.
    tag: u64,
    /// Answers to ops sent once the connection's task had ended.
    late: VecDeque<LiveMsg>,
    /// The task has ended: everything it had to say has been read.
    ended: bool,
    closed: bool,
}

impl Client {
    /// Sends `first` in order and waits for their replies (a refusal is its message), then
    /// follows `topics`: their snapshot now, and from then on a task owns the connection.
    pub async fn live(
        mut self,
        topics: &[Topic],
        first: Vec<Op>,
    ) -> Result<(Live, Vec<Result<Done, String>>, SnapshotMsg), ClientError> {
        let mut replies = Vec::with_capacity(first.len());
        for op in first {
            replies.push(match self.call(op).await {
                Ok(done) => Ok(done),
                Err(ClientError::Rpc(e)) => Err(e.message),
                Err(e) => return Err(e),
            });
        }
        let snapshot = self.subscribe(topics).await?;
        let (ops_tx, ops_rx) = mpsc::unbounded_channel();
        let (msgs_tx, msgs_rx) = mpsc::unbounded_channel();
        let mut wire = self.wire;
        // Pushes read before the snapshot are older than it: its rows replace any patch or
        // snapshot among them, but an event still happened.
        for (method, params) in wire.notices.drain(..) {
            if method == "event"
                && let Ok(Some(msg)) = push(&method, params)
            {
                let _ = msgs_tx.send(msg);
            }
        }
        tokio::spawn(run(wire, ops_rx, msgs_tx));
        let live = Live {
            ops: ops_tx,
            msgs: msgs_rx,
            tag: 0,
            late: VecDeque::new(),
            ended: false,
            closed: false,
        };
        Ok((live, replies, snapshot))
    }
}

impl Live {
    /// Sends `op` without waiting and returns its tag: 1 for the first, then one more for each.
    /// Once the connection has ended, the op is answered at once (`try again`), which can be
    /// after [`LiveMsg::Closed`].
    pub fn send(&mut self, op: Op) -> u64 {
        self.tag += 1;
        if self.ops.send((self.tag, op)).is_err() {
            // The connection has ended; the op never left.
            self.late.push_back(LiveMsg::Reply {
                tag: self.tag,
                result: stopped(false),
            });
        }
        self.tag
    }

    /// The next message. Cancel-safe: dropping the future loses nothing. After
    /// [`LiveMsg::Closed`] it never completes, except to answer an op sent after it.
    pub async fn recv(&mut self) -> LiveMsg {
        if !self.ended {
            match self.msgs.recv().await {
                Some(msg) => return msg,
                None => self.ended = true,
            }
        }
        if let Some(msg) = self.late.pop_front() {
            return msg;
        }
        if !self.closed {
            self.closed = true;
            return LiveMsg::Closed;
        }
        std::future::pending().await
    }
}

/// A push as a message: `None` for a method this client doesn't know (methods are additive)
/// or an event it can't read; an error for a patch or snapshot it can't read, since its
/// user's copy of the rows would go wrong.
fn push(method: &str, params: Value) -> Result<Option<LiveMsg>, ClientError> {
    Ok(match method {
        "patch" => Some(LiveMsg::Patch(decode(params)?)),
        "snapshot" => Some(LiveMsg::Snapshot(decode(params)?)),
        "event" => decode(params).ok().map(LiveMsg::Event),
        _ => None,
    })
}

/// The answer to an op that got no reply: `wrote` when it may have reached the daemon.
fn stopped(wrote: bool) -> Result<Done, String> {
    Err(ClientError::Stopped { wrote }.to_string())
}

/// An op sent and not yet answered.
struct Owed {
    tag: u64,
    writes: bool,
}

/// Request lines waiting to be written.
struct Outbox {
    lines: VecDeque<(Id, String)>,
    /// Some lines were handed to the connection but not flushed yet.
    unflushed: bool,
}

impl Outbox {
    /// Writes as much as the connection takes now, without waiting for the rest. `Ready(Ok)`
    /// once everything queued is written.
    fn poll_write(
        &mut self,
        framed: &mut Framed<UnixStream, LinesCodec>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), LinesCodecError>> {
        while !self.lines.is_empty() {
            ready!(SinkExt::<String>::poll_ready_unpin(framed, cx))?;
            if let Some((_, line)) = self.lines.pop_front() {
                framed.start_send_unpin(line)?;
                self.unflushed = true;
            }
        }
        if self.unflushed {
            ready!(SinkExt::<String>::poll_flush_unpin(framed, cx))?;
            self.unflushed = false;
        }
        Poll::Ready(Ok(()))
    }
}

/// What woke the connection's task.
// Made and matched at once, one per wake; boxing the op would only add an allocation.
#[allow(clippy::large_enum_variant)]
enum Wake {
    /// An op from the `Live`, or `None` once it was dropped.
    Op(Option<(u64, Op)>),
    /// A line read, or the end of the connection.
    Line(Option<Result<String, LinesCodecError>>),
    /// A write failed, which ends the connection.
    WriteFailed,
}

/// Owns the connection until it ends or the [`Live`] is dropped: writes ops as they come,
/// and hands on replies and pushes as they are read. Reading never waits for a write: the
/// daemon reads nothing while an op is in flight, so ops queued behind a slow one can fill
/// the socket, and the daemon's pushes must still be read. The connection ends when the
/// daemon hangs up, sends what isn't a reply, or can't be written to; then every reply still
/// owed is answered with why there is none, and the channel closes.
async fn run(
    mut wire: Wire,
    mut ops: mpsc::UnboundedReceiver<(u64, Op)>,
    msgs: mpsc::UnboundedSender<LiveMsg>,
) {
    let reply = |tag: u64, result: Result<Done, String>| {
        let _ = msgs.send(LiveMsg::Reply { tag, result });
    };
    // By request id, which counts up, so the first is the oldest.
    let mut owed: BTreeMap<Id, Owed> = BTreeMap::new();
    let mut out = Outbox {
        lines: VecDeque::new(),
        unflushed: false,
    };
    loop {
        let wake = poll_fn(|cx| {
            if let Poll::Ready(Err(_)) = out.poll_write(&mut wire.framed, cx) {
                return Poll::Ready(Wake::WriteFailed);
            }
            if let Poll::Ready(op) = ops.poll_recv(cx) {
                return Poll::Ready(Wake::Op(op));
            }
            wire.framed.poll_next_unpin(cx).map(Wake::Line)
        })
        .await;
        match wake {
            // The `Live` was dropped, and nobody is listening.
            Wake::Op(None) => return,
            Wake::Op(Some((tag, op))) => match wire.op_line(&op) {
                Ok((id, line)) => {
                    let writes = op.writes();
                    owed.insert(id.clone(), Owed { tag, writes });
                    out.lines.push_back((id, line));
                }
                Err(e) => reply(tag, Err(e.to_string())),
            },
            // A connection that can't be written to has ended, even if the daemon still
            // writes: an op handed to it would wait on a reply forever.
            Wake::WriteFailed => break,
            Wake::Line(next) => match server_line(next) {
                Ok(Some(ServerLine::Result { id, value })) => {
                    if let Some(o) = owed.remove(&id) {
                        reply(o.tag, decode(value).map_err(|e| e.to_string()));
                    }
                }
                Ok(Some(ServerLine::Error { id, error })) => {
                    // Without an id it answers the oldest, as `Wire::reply` takes it for
                    // the one awaited.
                    let o = match id {
                        Some(id) => owed.remove(&id),
                        None => owed.pop_first().map(|(_, o)| o),
                    };
                    if let Some(o) = o {
                        reply(o.tag, Err(error.message));
                    }
                }
                Ok(Some(ServerLine::Notice { method, params })) => match push(&method, params) {
                    Ok(Some(msg)) => {
                        let _ = msgs.send(msg);
                    }
                    Ok(None) => {}
                    Err(_) => break,
                },
                Ok(None) | Err(_) => break,
            },
        }
    }
    // Ended. Ops sent from now on are answered by `Live::send` itself; those already sent are
    // drained here, so each is answered before `Closed`. Lines still queued never left; those
    // handed to the connection may have reached the daemon.
    ops.close();
    for (id, _) in out.lines.drain(..) {
        if let Some(o) = owed.remove(&id) {
            reply(o.tag, stopped(false));
        }
    }
    for o in owed.into_values() {
        reply(o.tag, stopped(o.writes));
    }
    while let Some((tag, _)) = ops.recv().await {
        reply(tag, stopped(false));
    }
}
