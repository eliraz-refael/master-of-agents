//! One client connection: lines in, replies and notices out.
//!
//! Backpressure: a connection has at most one op in flight. The session reads its next line
//! only after the previous op's reply has been written, so what one connection can queue in
//! the core's unbounded inbox and reply channel is one request. Notices can't pile up
//! either: the model is a `watch` (a slow reader sees the latest, as one patch), events are
//! a bounded `broadcast` (a slow reader gets a fresh snapshot instead of the ones it missed).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use moa_agents::claude;
use moa_core::config::BrainName;
use moa_core::hook::HookLine;
use moa_core::op::{ConnId, Done, Op, OpError, PAGE_MAX, ReqId};
use moa_core::publish::FeedRow;
use moa_core::state::CallerInfo;
use moa_core::upgrade::Why;
use moa_proto::{
    Call, CoreEvent, Hello, HelloOk, Id, Incoming, Model, Patch, RpcError, StatusOk, Subscribe,
    Topic, UpgradeOk, allowed_on_mismatch, code, decode_incoming, encode_error, encode_notice,
    encode_result, snapshot_of,
};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::{broadcast, watch};
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};
use tokio_util::sync::CancellationToken;

use crate::core_task::{ConnReply, CoreHandle, STOPPING};

/// How much input is thrown away after an over-long line, looking for its end.
const DISCARD_LIMIT: usize = 16 << 20;
/// How long that may take.
const DISCARD_TIME: Duration = Duration::from_secs(5);

/// How long a closing session spends writing what it still owes a client.
const FLUSH_TIME: Duration = Duration::from_secs(1);

const MISMATCH: &str = "this moa is a different build; run the command again to restart it";

/// The daemon's program, as `hello` and `upgrade` tell it.
#[derive(Clone, Debug)]
pub struct Program {
    /// The path it runs from and upgrades into (`MOA_EXE`).
    pub exe: PathBuf,
    /// The build this image runs: the daemon is stale once `exe`'s build differs.
    pub build: String,
}

impl Program {
    /// Whether `exe` is now another build than this image's. One that can't be read (being
    /// replaced, or gone) isn't: there is nothing to upgrade into yet.
    fn stale(&self) -> bool {
        moa_platform::build_id_of(&self.exe).is_ok_and(|b| b != self.build)
    }
}

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
        Call::Upgrade { .. } => "upgrade",
        Call::Subscribe(_) => "subscribe",
        Call::Op(op) => op.method(),
    }
}

/// Closes the connection's reply channel in the core, and tells the core it went, however
/// the session ends.
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
    // Biased, model first: the core publishes a drain's model before its events, so a
    // subscriber gets the patch before an event about the rows it changed.
    tokio::select! {
        biased;
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

/// `bare`: a hook's reply is its data alone (`{"context": …}`), not the op reply's text and data.
fn reply_line(id: &Id, result: Result<moa_core::op::Done, OpError>, bare: bool) -> String {
    match result {
        Ok(done) if bare => encode_result(id, &done.data),
        Ok(done) => encode_result(id, &done),
        // The core's word that it had stopped before running it.
        Err(OpError(message)) if message == STOPPING => {
            encode_error(Some(id), &rpc(code::STOPPING, &message))
        }
        Err(OpError(message)) => encode_error(Some(id), &rpc(code::FAILED, &message)),
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
    program: Arc<Program>,
    /// Cancelled by `shutdown`: the daemon stops accepting and winds down.
    shutdown: CancellationToken,
    /// Cancelled once the core has ended: only then may a session cut a write short or
    /// leave without its reply. Before that a session closes when the core drops its
    /// reply channel, after the replies already in it.
    close: CancellationToken,
    conn: ConnId,
    replies: tokio::sync::mpsc::UnboundedReceiver<ConnReply>,
    greeted: Option<Greeted>,
    sub: Option<Sub>,
    /// The op in flight: the core's id for it and the client's.
    pending: Option<(ReqId, Id)>,
    /// The request in flight when it is a hook's.
    hook: Option<ReqId>,
    next_req: u64,
}

/// Serves one connection until it closes, the core ends (its reply channel drops, after
/// what it holds is delivered) or `close` is cancelled. A `shutdown` request cancels
/// `shutdown` once its reply is written; an `upgrade` is the core's to start.
pub async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    core: CoreHandle,
    brain: BrainName,
    program: Arc<Program>,
    shutdown: CancellationToken,
    close: CancellationToken,
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
        program,
        shutdown,
        close,
        conn,
        replies,
        greeted: None,
        sub: None,
        pending: None,
        hook: None,
        next_req: 0,
    };
    session.run().await;
    tracing::debug!(conn = conn.0, "connection closed");
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Session<S> {
    async fn run(&mut self) {
        loop {
            // `close` first: once it is cancelled the session reads nothing new.
            let flow = tokio::select! {
                biased;
                _ = self.close.cancelled() => {
                    self.flush_replies().await;
                    Flow::Close
                }
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

    /// Writes one line; `false` when the connection is gone. Once the daemon has closed
    /// the sessions, a write already under way gets a short time to finish rather than being
    /// cut off: it may be the reply the client is waiting for.
    async fn send(&mut self, line: String) -> bool {
        let close = &self.close;
        let sending = self.framed.send(line);
        tokio::pin!(sending);
        tokio::select! {
            r = &mut sending => r.is_ok(),
            _ = close.cancelled() => matches!(
                tokio::time::timeout(FLUSH_TIME, &mut sending).await,
                Ok(Ok(()))
            ),
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
            Ok(Incoming::Hook { id, params }) => self.on_hook(id, params).await,
            Ok(Incoming::Request { id, call }) => self.on_call(id, call).await,
        }
    }

    /// A hook needs no `hello` and has no caller. With an id it is answered once, after the
    /// core's commit: a start with `{"context": …}`, an event the agent can't read with
    /// `{"context": null}`, a line that isn't a hook line with `INVALID_PARAMS`.
    async fn on_hook(&mut self, id: Option<Id>, params: serde_json::Value) -> Flow {
        let Ok(line) = serde_json::from_value::<HookLine>(params) else {
            tracing::debug!(conn = self.conn.0, "a hook line was not readable");
            return match id {
                Some(id) => {
                    self.refuse(&id, code::INVALID_PARAMS, "not a hook line")
                        .await
                }
                None => Flow::Go,
            };
        };
        let Some(envelope) = claude::normalize(&line, &claude::last_assistant_text) else {
            return match id {
                Some(id) => {
                    self.say(encode_result(&id, &json!({"context": null})))
                        .await
                }
                None => Flow::Go,
            };
        };
        match id {
            None => self.core.hook(None, envelope, Some(line)),
            Some(id) => {
                self.next_req += 1;
                let req = ReqId(self.next_req);
                self.pending = Some((req, id));
                self.hook = Some(req);
                self.core.hook(Some((self.conn, req)), envelope, Some(line));
            }
        }
        // Behind this event in the inbox: H5 orders what the spool held against it.
        self.core.drain_spool_if_any();
        Flow::Go
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
            Call::Upgrade { if_stale } => self.upgrade(&id, if_stale).await,
            Call::Shutdown => {
                let flow = self.say(encode_result(&id, &json!({}))).await;
                tracing::info!(conn = self.conn.0, "asked to stop");
                self.shutdown.cancel();
                if flow == Flow::Go { Flow::Close } else { flow }
            }
            Call::Subscribe(s) => self.subscribe(&id, s).await,
            Call::Op(Op::ConsolePage { before, n }) => self.page(&id, before, n).await,
            Call::Op(op) => {
                let req = self.send_to_core(op);
                self.pending = Some((req, id));
                Flow::Go
            }
        }
    }

    /// Sends `op` to the core as this connection's caller, under a new request id: the one
    /// its reply comes back with.
    fn send_to_core(&mut self, op: Op) -> ReqId {
        self.next_req += 1;
        let req = ReqId(self.next_req);
        let caller = self.greeted.as_ref().map(|g| g.caller.clone());
        self.core
            .send(self.conn, req, caller.unwrap_or_default(), op);
        req
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
            exe: Some(self.program.exe.display().to_string()),
        };
        self.say(encode_result(id, &ok)).await
    }

    /// `upgrade` (L1): unless `if_stale` and the program is still this image's build, the
    /// core upgrades the daemon once moa is between turns, and the reply says whether it
    /// waits for that. Otherwise the reply names the program, which runs this brain.
    async fn upgrade(&mut self, id: &Id, if_stale: bool) -> Flow {
        let ok = if !if_stale || self.program.stale() {
            let Some(waiting) = self.core.upgrade(Why::Asked).await else {
                return self.refuse(id, code::STOPPING, STOPPING).await;
            };
            tracing::info!(conn = self.conn.0, waiting, "asked to upgrade");
            UpgradeOk {
                upgrading: true,
                waiting_for_moa: waiting,
                exe: None,
            }
        } else {
            UpgradeOk {
                upgrading: false,
                waiting_for_moa: false,
                exe: Some(self.program.exe.display().to_string()),
            }
        };
        self.say(encode_result(id, &ok)).await
    }

    async fn status(&mut self, id: &Id) -> Flow {
        let caller = self
            .greeted
            .as_ref()
            .map(|g| g.caller.clone())
            .unwrap_or_default();
        let Some(data) = self.core.status(caller).await else {
            return self.refuse(id, code::STOPPING, STOPPING).await;
        };
        let ok = StatusOk {
            brain: data.brain,
            build: moa_platform::build_id().to_string(),
            pid: std::process::id(),
            launches: moa_platform::spawn::launches(),
            sessions: data.sessions,
            caller: data.caller,
            tmux: data.tmux,
            tmux_restarts: data.tmux_restarts,
            panel_starts: data.panel_starts,
            upgrade: data.upgrade,
        };
        self.say(encode_result(id, &ok)).await
    }

    /// `console.page`: older feed rows, read from the store here rather than through the core.
    /// The core hears of it too, as the user being here (D2); it answers nothing.
    async fn page(&mut self, id: &Id, before: i64, n: u16) -> Flow {
        if !(1..=PAGE_MAX).contains(&n) {
            return self
                .refuse(id, code::INVALID_PARAMS, &format!("n is 1 to {PAGE_MAX}"))
                .await;
        }
        self.send_to_core(Op::ConsolePage { before, n });
        match self.core.feed_page(before, usize::from(n)).await {
            Ok(rows) => {
                let done = Done {
                    text: format!("{} older rows", rows.len()),
                    data: json!({"rows": rows.iter().map(FeedRow::from).collect::<Vec<_>>()}),
                };
                self.say(encode_result(id, &done)).await
            }
            Err(why) => self.refuse(id, code::FAILED, &why).await,
        }
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
                self.refuse(&id, code::STOPPING, STOPPING).await;
            }
            return Flow::Close;
        };
        match self.pending.take() {
            Some((want, id)) if want == req => {
                let bare = self.hook.take() == Some(req);
                self.say(reply_line(&id, result, bare)).await
            }
            // Not ours to answer: put back what was there.
            other => {
                self.pending = other;
                Flow::Go
            }
        }
    }

    /// When the daemon closes the session: writes the replies already waiting (the core
    /// answers everything it held before it ends), with a short bound on a stuck client.
    async fn flush_replies(&mut self) {
        let ended = loop {
            match self.replies.try_recv() {
                Ok((req, result)) => match self.pending.take() {
                    Some((want, id)) if want == req => {
                        let line = reply_line(&id, result, self.hook.take() == Some(req));
                        let _ = tokio::time::timeout(FLUSH_TIME, self.framed.send(line)).await;
                    }
                    other => self.pending = other,
                },
                Err(e) => break e == tokio::sync::mpsc::error::TryRecvError::Disconnected,
            }
        };
        // Still owed a line: every request gets exactly one. A core that ended cleared its
        // connections after answering all it ran, so the request never ran (`STOPPING`, and
        // an upgraded daemon takes it again); a core that hung may be running it still.
        if let Some((_, id)) = self.pending.take() {
            let code = if ended { code::STOPPING } else { code::FAILED };
            let line = encode_error(Some(&id), &rpc(code, STOPPING));
            let _ = tokio::time::timeout(FLUSH_TIME, self.framed.send(line)).await;
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
            let discard = tokio::time::timeout(DISCARD_TIME, async {
                let mut left = DISCARD_LIMIT;
                let mut buf = vec![0u8; 64 << 10];
                while left > 0 {
                    match io.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) if buf[..n].contains(&b'\n') => break,
                        Ok(n) => left = left.saturating_sub(n),
                    }
                }
            });
            // A daemon that is closing its sessions doesn't wait for this.
            tokio::select! {
                _ = discard => {}
                _ = self.close.cancelled() => {}
            }
        }
        Flow::Close
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use moa_core::state::Deps;
    use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
    use tokio::io::{AsyncBufReadExt, BufReader, duplex};

    use super::*;
    use crate::core_task::CoreOptions;
    use crate::testing::FakeTime;

    #[test]
    fn only_our_own_uid_is_allowed() {
        assert!(peer_allowed(501, 501) && !peer_allowed(502, 501));
    }

    /// A request still owed a line when the session closes (it reached a core that had
    /// ended, so no reply is queued) gets the stopping line, exactly one.
    #[tokio::test]
    async fn a_pending_op_gets_a_line_when_the_session_closes() {
        let brain = TypicalBrain::new();
        let time = FakeTime::new(brain.now().wall);
        let core = CoreHandle::start(CoreOptions {
            brain: BrainName::default_brain(),
            config: brain.config(),
            deps: Deps::default(),
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: time.clone(),
            alarm: Box::new(time.alarm()),
            db: brain.db.clone(),
            backups: brain.dir.path().join("backups"),
            events_capacity: 4,
            tmux: None,
            spool: None,
            facts: None,
        })
        .await
        .unwrap();
        let (conn, replies) = core.open_conn();
        let (client, server) = duplex(4096);
        let mut session = Session {
            framed: Framed::new(server, LinesCodec::new()),
            core,
            brain: BrainName::default_brain(),
            program: Arc::new(Program {
                exe: PathBuf::from("/nonexistent/moa"),
                build: "b".into(),
            }),
            shutdown: CancellationToken::new(),
            close: CancellationToken::new(),
            conn,
            replies,
            greeted: None,
            sub: None,
            pending: Some((ReqId(1), Id::Num(7))),
            hook: None,
            next_req: 1,
        };
        session.close.cancel();
        session.run().await;
        drop(session);
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).await.unwrap();
        assert!(line.contains(r#""id":7"#), "{line}");
        assert!(line.contains(STOPPING), "{line}");
    }
}
