//! [`Live`]: a subscribed connection that never makes its user wait. A task owns the
//! connection; an op goes out at once and its reply comes back by tag, while pushes keep
//! arriving in order.

use std::collections::{BTreeMap, VecDeque};

use moa_core::op::{Done, Op};
use moa_proto::{CoreEvent, Id, Patch, ServerLine, SnapshotMsg, Topic};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{Client, ClientError, Wire, decode};

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
    /// The connection has ended. Said once, after every reply still owed.
    Closed,
}

/// A subscribed connection. Dropping it closes the connection.
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
    pub fn send(&mut self, op: Op) -> u64 {
        self.tag += 1;
        if self.ops.send((self.tag, op)).is_err() {
            // The connection has ended; the op never left.
            self.late.push_back(LiveMsg::Reply {
                tag: self.tag,
                result: Err(ClientError::Stopped { wrote: false }.to_string()),
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

/// An op written and not yet answered.
struct Owed {
    tag: u64,
    writes: bool,
}

/// Owns the connection until it ends or the [`Live`] is dropped: writes ops as they come,
/// and hands on replies and pushes as they are read. When the connection ends, every reply
/// still owed is answered with why there is none, and then the channel closes.
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
    // Whether writing still works.
    let mut open = true;
    loop {
        tokio::select! {
            op = ops.recv() => {
                // `None`: the `Live` was dropped, and nobody is listening.
                let Some((tag, op)) = op else { return };
                if !open {
                    reply(tag, Err(ClientError::Stopped { wrote: false }.to_string()));
                    continue;
                }
                match wire.send_op(&op).await {
                    Ok(id) => {
                        owed.insert(id, Owed { tag, writes: op.writes() });
                    }
                    // It never reached the daemon; replies to earlier ops may still come.
                    Err(e) => {
                        open = false;
                        reply(tag, Err(e.to_string()));
                    }
                }
            }
            line = wire.read() => match line {
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
    // Ended. Ops sent from now on are answered by `Live::send` itself.
    ops.close();
    for o in owed.into_values() {
        reply(
            o.tag,
            Err(ClientError::Stopped { wrote: o.writes }.to_string()),
        );
    }
    while let Ok((tag, _)) = ops.try_recv() {
        reply(tag, Err(ClientError::Stopped { wrote: false }.to_string()));
    }
}
