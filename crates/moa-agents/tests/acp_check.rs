//! Proves the pinned ACP library plays both the Client and the Agent role, in one process, with
//! everything slice 6a needs: capabilities, `systemPrompt` and `_meta` on `session/new`, a turn's
//! updates arriving before its end, permission requests, resume, load, cancel, and a line with a
//! bad byte.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
    InitializeResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, ResumeSessionResponse, SelectedPermissionOutcome, SessionCapabilities,
    SessionNotification, SessionResumeCapabilities, SessionSystemPromptCapabilities, SessionUpdate,
    StopReason, SystemPrompt, SystemPromptMode, TextContent, ToolCall, ToolCallUpdate,
    ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, ByteStreams, Client, Error};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Io = ByteStreams<Compat<WriteHalf<DuplexStream>>, Compat<ReadHalf<DuplexStream>>>;
type Log = mpsc::UnboundedSender<String>;

const LIMIT: Duration = Duration::from_secs(10);

fn io(stream: DuplexStream) -> Io {
    let (read, write) = tokio::io::split(stream);
    ByteStreams::new(write.compat_write(), read.compat())
}

fn text(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(s))
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.text.clone(),
        _ => "[other]".into(),
    }
}

/// What the stub agent saw and decided, for the test to read.
#[derive(Default)]
struct Seen {
    new_session: Mutex<Option<Value>>,
    chosen_option: Mutex<Option<String>>,
    cancelled: Notify,
}

/// The agent side: advertises everything 6a needs, and acts on the prompt's text: `turn` streams
/// three chunks, a tool call and a permission request; `hang` waits for `session/cancel`.
fn spawn_agent(stream: DuplexStream, seen: Arc<Seen>) -> JoinHandle<Result<(), Error>> {
    let (s_new, s_prompt, s_cancel) = (seen.clone(), seen.clone(), seen);
    let agent = Agent
        .v1()
        .name("stub")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx| {
                let caps = SessionCapabilities::new()
                    .resume(SessionResumeCapabilities::default())
                    .system_prompt(SessionSystemPromptCapabilities::default());
                responder.respond(
                    InitializeResponse::new(req.protocol_version).agent_capabilities(
                        AgentCapabilities::new()
                            .load_session(true)
                            .session_capabilities(caps),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, _cx| {
                *s_new.new_session.lock().unwrap() = Some(serde_json::to_value(&req).unwrap());
                responder.respond(NewSessionResponse::new("s1"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: ResumeSessionRequest, responder, _cx| {
                responder.respond(ResumeSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest, responder, cx| {
                let replay = |update| SessionNotification::new(req.session_id.clone(), update);
                cx.send_notification(replay(SessionUpdate::UserMessageChunk(ContentChunk::new(
                    text("q"),
                ))))?;
                cx.send_notification(replay(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    text("a"),
                ))))?;
                responder.respond(LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            // A prompt is answered from a spawned task, so the loop stays free for `session/cancel`.
            async move |req: PromptRequest, responder, cx| {
                let seen = s_prompt.clone();
                let task_cx = cx.clone();
                cx.spawn(async move {
                    let sid = req.session_id;
                    if text_of(&req.prompt[0]) == "hang" {
                        seen.cancelled.notified().await;
                        return responder.respond(PromptResponse::new(StopReason::Cancelled));
                    }
                    for part in ["one ", "two ", "three"] {
                        let chunk = ContentChunk::new(text(part)).message_id("m1");
                        task_cx.send_notification(SessionNotification::new(
                            sid.clone(),
                            SessionUpdate::AgentMessageChunk(chunk),
                        ))?;
                    }
                    task_cx.send_notification(SessionNotification::new(
                        sid.clone(),
                        SessionUpdate::ToolCall(ToolCall::new("t1", "Read a file")),
                    ))?;
                    let options = vec![
                        PermissionOption::new(
                            "o-always",
                            "Always",
                            PermissionOptionKind::AllowAlways,
                        ),
                        PermissionOption::new("o-reject", "No", PermissionOptionKind::RejectOnce),
                        PermissionOption::new("o-once", "Once", PermissionOptionKind::AllowOnce),
                    ];
                    let answer = task_cx
                        .send_request(RequestPermissionRequest::new(
                            sid,
                            ToolCallUpdate::new("t1", ToolCallUpdateFields::new()),
                            options,
                        ))
                        .block_task()
                        .await?;
                    if let RequestPermissionOutcome::Selected(picked) = answer.outcome {
                        *seen.chosen_option.lock().unwrap() = Some(picked.option_id.to_string());
                    }
                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_n: CancelNotification, _cx| {
                s_cancel.cancelled.notify_one();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        );
    tokio::spawn(agent.connect_to(io(stream)))
}

/// The client side: logs every update it receives, and answers a permission request by kind,
/// with `reject_once`. Runs `main` on the connection, within the time limit.
async fn run_client<R>(
    stream: DuplexStream,
    log: Log,
    main: impl AsyncFnOnce(ConnectionTo<Agent>) -> Result<R, Error>,
) -> R {
    let (updates, permissions) = (log.clone(), log);
    let client = Client
        .v1()
        .name("check")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                let line = match n.update {
                    SessionUpdate::AgentMessageChunk(c) => format!(
                        "chunk {} {}",
                        c.message_id.map(|m| m.to_string()).unwrap_or_default(),
                        text_of(&c.content)
                    ),
                    SessionUpdate::UserMessageChunk(c) => format!("user {}", text_of(&c.content)),
                    SessionUpdate::ToolCall(t) => format!("tool_call {}", t.tool_call_id),
                    _ => "other update".into(),
                };
                updates.send(line).unwrap();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                permissions.send("permission".into()).unwrap();
                let rejecting = req
                    .options
                    .iter()
                    .find(|o| o.kind == PermissionOptionKind::RejectOnce)
                    .expect("a reject_once option");
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        rejecting.option_id.clone(),
                    )),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        );
    tokio::time::timeout(LIMIT, client.connect_with(io(stream), main))
        .await
        .expect("within the limit")
        .expect("the client ran")
}

/// Sends a prompt the ordered way and reports its stop reason, logging `end` when it arrives.
async fn prompt(cx: &ConnectionTo<Agent>, log: &Log, said: &str) -> Result<StopReason, Error> {
    let (tx, rx) = oneshot::channel();
    let log = log.clone();
    cx.prepare_request(PromptRequest::new("s1", vec![text(said)]))
        .on_receiving_result(move |result| async move {
            log.send("end".into()).unwrap();
            let _ = tx.send(result.map(|r| r.stop_reason));
            Ok(())
        })?;
    rx.await.expect("the prompt was answered")
}

async fn initialize(cx: &ConnectionTo<Agent>) -> Result<InitializeResponse, Error> {
    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await
}

fn drain(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[tokio::test]
async fn client_and_agent_agree_on_capabilities() {
    let (client_end, agent_end) = tokio::io::duplex(1 << 20);
    let seen = Arc::new(Seen::default());
    let agent = spawn_agent(agent_end, seen.clone());
    let (log, _rx) = mpsc::unbounded_channel();
    let meta = json!({"moa": 1}).as_object().unwrap().clone();
    run_client(client_end, log, async |cx| {
        let caps = initialize(&cx).await?.agent_capabilities;
        assert!(caps.load_session);
        assert!(caps.session_capabilities.resume.is_some());
        assert!(caps.session_capabilities.system_prompt.is_some());
        let system = SystemPrompt::new(vec![text("C")]).mode(SystemPromptMode::Append);
        cx.send_request(
            NewSessionRequest::new("/tmp/check")
                .system_prompt(system)
                .meta(meta),
        )
        .block_task()
        .await?;
        Ok(())
    })
    .await;
    let new = seen
        .new_session
        .lock()
        .unwrap()
        .clone()
        .expect("session/new arrived");
    assert_eq!(
        new["systemPrompt"],
        json!({"mode": "append", "content": [{"type": "text", "text": "C"}]})
    );
    assert_eq!(new["_meta"], json!({"moa": 1}));
    agent.abort();
}

#[tokio::test]
async fn a_turns_updates_arrive_before_its_end() {
    let (client_end, agent_end) = tokio::io::duplex(1 << 20);
    let seen = Arc::new(Seen::default());
    let agent = spawn_agent(agent_end, seen.clone());
    let (log, mut rx) = mpsc::unbounded_channel();
    let stop = run_client(client_end, log.clone(), async |cx| {
        initialize(&cx).await?;
        prompt(&cx, &log, "turn").await
    })
    .await;
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(
        *seen.chosen_option.lock().unwrap(),
        Some("o-reject".to_string())
    );
    assert_eq!(
        drain(&mut rx),
        [
            "chunk m1 one ",
            "chunk m1 two ",
            "chunk m1 three",
            "tool_call t1",
            "permission",
            "end"
        ]
    );
    agent.abort();
}

#[tokio::test]
async fn resume_and_load() {
    let (client_end, agent_end) = tokio::io::duplex(1 << 20);
    let agent = spawn_agent(agent_end, Arc::new(Seen::default()));
    let (log, mut rx) = mpsc::unbounded_channel();
    let (after_resume, after_load) = run_client(client_end, log.clone(), async |cx| {
        initialize(&cx).await?;
        cx.send_request(ResumeSessionRequest::new("s1", "/tmp/check"))
            .block_task()
            .await?;
        let after_resume = drain(&mut rx);
        let (tx, answered) = oneshot::channel();
        let log = log.clone();
        cx.prepare_request(LoadSessionRequest::new("s1", "/tmp/check"))
            .on_receiving_result(move |_| async move {
                log.send("answer".into()).unwrap();
                let _ = tx.send(());
                Ok(())
            })?;
        answered.await.expect("load was answered");
        Ok((after_resume, drain(&mut rx)))
    })
    .await;
    assert!(
        after_resume.is_empty(),
        "resume sends no update: {after_resume:?}"
    );
    assert_eq!(after_load, ["user q", "chunk  a", "answer"]);
    agent.abort();
}

#[tokio::test]
async fn cancel_ends_the_turn_cancelled() {
    let (client_end, agent_end) = tokio::io::duplex(1 << 20);
    let agent = spawn_agent(agent_end, Arc::new(Seen::default()));
    let (log, _rx) = mpsc::unbounded_channel();
    let stop = run_client(client_end, log.clone(), async |cx| {
        initialize(&cx).await?;
        let turn = prompt(&cx, &log, "hang");
        cx.send_notification(CancelNotification::new("s1"))?;
        turn.await
    })
    .await;
    assert_eq!(stop, StopReason::Cancelled);
    agent.abort();
}

#[tokio::test]
async fn invalid_utf8_is_replaced_not_fatal() {
    let (client_end, raw_end) = tokio::io::duplex(1 << 20);
    let (raw_read, mut raw_write) = tokio::io::split(raw_end);
    let update = |bytes: &[u8]| {
        let mut line = br#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":""#.to_vec();
        line.extend_from_slice(bytes);
        line.extend_from_slice(b"\"}}}}\n");
        line
    };
    let (log, mut rx) = mpsc::unbounded_channel();
    run_client(client_end, log, async |_cx| {
        raw_write.write_all(&update(b"a\xFFb")).await.unwrap();
        raw_write.write_all(&update(b"still here")).await.unwrap();
        let mut got = Vec::new();
        while got.len() < 2 {
            got.push(rx.recv().await.expect("the connection lives"));
        }
        assert_eq!(got, ["chunk m1 a\u{FFFD}b", "chunk m1 still here"]);
        drop(raw_read);
        Ok(())
    })
    .await;
}
