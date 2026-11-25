//! stub-acp plays a script as moa's agent: one turn per prompt, a log of what it received, and
//! sessions that outlive the stub process. Driven as a child process, with the library's Client
//! role.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, SelectedPermissionOutcome, SessionId,
    SessionNotification, SessionUpdate, StopReason, SystemPrompt, SystemPromptMode, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, Error};
use moa_testkit::{StubStep, stub_acp, stub_log, wait_for, write_stub_script};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const LIMIT: Duration = Duration::from_secs(10);

type Events = mpsc::UnboundedReceiver<String>;

fn say(text: &str, id: Option<&str>) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: id.map(Into::into),
    }
}

fn end(stop: Option<&str>) -> StubStep {
    StubStep::End {
        stop: stop.map(Into::into),
    }
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.text.clone(),
        _ => "[other]".into(),
    }
}

/// A stub in `dir`, as a child process; it is killed when the child is dropped.
fn spawn_stub(dir: &Path) -> Child {
    let mut stub = Command::new(stub_acp());
    stub.current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    spawn(stub)
}

fn spawn(mut command: Command) -> Child {
    command.spawn().expect("start stub-acp")
}

/// Runs `main` as the Client of `child`'s stub, within the time limit. Every update becomes a
/// line on the returned channel; a permission request is answered with `reject_once` (else the
/// first option) and also becomes a line.
async fn run_client<R>(
    child: &mut Child,
    main: impl AsyncFnOnce(ConnectionTo<Agent>, Events) -> Result<R, Error>,
) -> Result<R, Error> {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let permissions = tx.clone();
    let client = Client
        .v1()
        .name("stub-check")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                let line = match n.update {
                    SessionUpdate::AgentMessageChunk(c) => format!(
                        "say {} {}",
                        c.message_id.map(|m| m.to_string()).unwrap_or_default(),
                        text_of(&c.content)
                    ),
                    SessionUpdate::AgentThoughtChunk(c) => format!("think {}", text_of(&c.content)),
                    SessionUpdate::UserMessageChunk(c) => format!("user {}", text_of(&c.content)),
                    SessionUpdate::ToolCall(t) => format!(
                        "tool {} {} {}",
                        t.tool_call_id,
                        t.title,
                        t.raw_input.unwrap_or(Value::Null)
                    ),
                    SessionUpdate::ToolCallUpdate(u) => {
                        let status = serde_json::to_value(u.fields.status).unwrap();
                        format!("done {} {status}", u.tool_call_id)
                    }
                    _ => "other update".into(),
                };
                tx.send(line).unwrap();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                permissions
                    .send(format!(
                        "ask {}",
                        req.tool_call.fields.title.unwrap_or_default()
                    ))
                    .unwrap();
                let pick = req
                    .options
                    .iter()
                    .find(|o| o.kind == PermissionOptionKind::RejectOnce)
                    .unwrap_or(&req.options[0]);
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        pick.option_id.clone(),
                    )),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        );
    let io = ByteStreams::new(
        child.stdin.take().expect("stdin").compat_write(),
        child.stdout.take().expect("stdout").compat(),
    );
    tokio::time::timeout(
        LIMIT,
        client.connect_with(io, async move |cx| main(cx, rx).await),
    )
    .await
    .expect("within the limit")
}

async fn initialize(cx: &ConnectionTo<Agent>) -> Result<(), Error> {
    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    Ok(())
}

async fn new_session(cx: &ConnectionTo<Agent>, dir: &Path) -> Result<SessionId, Error> {
    let new = cx
        .send_request(NewSessionRequest::new(dir))
        .block_task()
        .await?;
    Ok(new.session_id)
}

/// Sends a prompt at once and returns the receiver of its stop reason.
fn prompt(
    cx: &ConnectionTo<Agent>,
    session: &SessionId,
    said: &str,
) -> Result<oneshot::Receiver<Result<StopReason, Error>>, Error> {
    let (tx, rx) = oneshot::channel();
    cx.prepare_request(PromptRequest::new(
        session.clone(),
        vec![ContentBlock::Text(TextContent::new(said))],
    ))
    .on_receiving_result(move |result| async move {
        let _ = tx.send(result.map(|r| r.stop_reason));
        Ok(())
    })?;
    Ok(rx)
}

async fn next(events: &mut Events) -> String {
    tokio::time::timeout(LIMIT, events.recv())
        .await
        .expect("an event in time")
        .expect("the stub is there")
}

fn drain(events: &mut Events) -> Vec<String> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// Waits for `n` `session/cancel` lines in the stub's log, off the runtime so the connection
/// goes on meanwhile.
async fn wait_cancels(dir: &Path, n: usize) {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        wait_for(LIMIT, || {
            (logged(&dir, "session/cancel").len() >= n).then_some(())
        })
        .expect("the cancels show up")
    })
    .await
    .unwrap();
}

fn logged(dir: &Path, method: &str) -> Vec<Value> {
    stub_log(dir)
        .into_iter()
        .filter(|l| l["method"] == method)
        .collect()
}

#[tokio::test]
async fn a_scripted_turn_plays_in_order() {
    let dir = tempfile::tempdir().unwrap();
    write_stub_script(
        dir.path(),
        &[
            say("hello", Some("m1")),
            StubStep::Think { text: "hmm".into() },
            StubStep::Tool {
                id: "t1".into(),
                title: "Read a file".into(),
                input: Some(json!({"path": "a.txt"})),
            },
            StubStep::ToolDone {
                id: "t1".into(),
                status: "completed".into(),
            },
            end(None),
        ],
    );
    let mut child = spawn_stub(dir.path());
    let (stop, events) = run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        let stop = prompt(&cx, &session, "go")?.await.expect("answered")?;
        Ok((stop, drain(&mut events)))
    })
    .await
    .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(
        events,
        [
            "say m1 hello",
            "think hmm",
            r#"tool t1 Read a file {"path":"a.txt"}"#,
            r#"done t1 "completed""#,
        ]
    );
}

#[tokio::test]
async fn a_permission_outcome_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    let ask = |title: &str, options: &[(&str, &str)]| StubStep::Ask {
        title: title.into(),
        options: options
            .iter()
            .map(|(id, kind)| (id.to_string(), kind.to_string()))
            .collect(),
    };
    write_stub_script(
        dir.path(),
        &[
            ask(
                "Run it",
                &[("o-always", "allow_always"), ("o-no", "reject_once")],
            ),
            end(None),
        ],
    );
    let mut child = spawn_stub(dir.path());
    let events = run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        prompt(&cx, &session, "go")?.await.expect("answered")?;
        Ok(drain(&mut events))
    })
    .await
    .unwrap();
    assert_eq!(events, ["ask Run it"]);
    let outcomes = logged(dir.path(), "permission.outcome");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["params"]["optionId"], "o-no");
}

#[tokio::test]
async fn a_run_waits_for_its_command_and_logs_it() {
    let dir = tempfile::tempdir().unwrap();
    let run = StubStep::Run {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "pwd -P; echo oops >&2; exit 3".into(),
        ],
    };
    write_stub_script(dir.path(), &[run, say("after", None), end(None)]);
    let mut child = spawn_stub(dir.path());
    let events = run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        prompt(&cx, &session, "go")?.await.expect("answered")?;
        Ok(drain(&mut events))
    })
    .await
    .unwrap();
    assert_eq!(events, ["say  after"]);
    let runs = logged(dir.path(), "run.done");
    assert_eq!(runs.len(), 1, "{runs:?}");
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(runs[0]["stdout"], format!("{}\n", cwd.display()));
    assert_eq!(runs[0]["stderr"], "oops\n");
    assert_eq!(runs[0]["code"], 3);
}

#[tokio::test]
async fn fail_answers_the_prompt_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let fail = StubStep::Fail {
        message: "Authentication required".into(),
    };
    write_stub_script(dir.path(), &[fail, say("again", None), end(None)]);
    let mut child = spawn_stub(dir.path());
    let (failed, stop, events) = run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        let failed = prompt(&cx, &session, "one")?.await.expect("answered");
        // `Fail` ends its turn: the next prompt plays the next one.
        let stop = prompt(&cx, &session, "two")?.await.expect("answered")?;
        Ok((failed, stop, drain(&mut events)))
    })
    .await
    .unwrap();
    let error = failed.expect_err("the prompt fails");
    assert_eq!(i32::from(error.code), -32000);
    assert_eq!(error.message, "Authentication required");
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(events, ["say  again"]);
}

#[tokio::test]
async fn hang_ends_on_cancel_and_hang_hard_does_not() {
    let dir = tempfile::tempdir().unwrap();
    write_stub_script(
        dir.path(),
        &[
            say("one", None),
            StubStep::Hang,
            say("two", None),
            StubStep::HangHard,
        ],
    );
    let mut child = spawn_stub(dir.path());
    run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        let first = prompt(&cx, &session, "one")?;
        assert_eq!(next(&mut events).await, "say  one");
        cx.send_notification(CancelNotification::new(session.clone()))?;
        assert_eq!(first.await.expect("answered")?, StopReason::Cancelled);

        let mut second = prompt(&cx, &session, "two")?;
        assert_eq!(next(&mut events).await, "say  two");
        cx.send_notification(CancelNotification::new(session.clone()))?;
        // The second cancel reached the stub, and its turn still goes on.
        wait_cancels(dir.path(), 2).await;
        assert!(second.try_recv().is_err(), "a hard hang answers nothing");
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn die_exits_mid_turn() {
    let dir = tempfile::tempdir().unwrap();
    // The gate keeps the stub from exiting before its words are out.
    let gate = StubStep::Wait {
        file: "gate".into(),
    };
    write_stub_script(dir.path(), &[say("last words", None), gate, StubStep::Die]);
    let mut child = spawn_stub(dir.path());
    // The connection may close under the client as the stub exits; that is the point.
    let _ = run_client(&mut child, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        let turn = prompt(&cx, &session, "go")?;
        assert_eq!(next(&mut events).await, "say  last words");
        std::fs::write(dir.path().join("gate"), "").unwrap();
        // The prompt is never answered: the connection closes under the client.
        assert!(!matches!(turn.await, Ok(Ok(_))));
        Ok(())
    })
    .await;
    let status = tokio::time::timeout(LIMIT, child.wait())
        .await
        .expect("the stub exits")
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[tokio::test]
async fn a_second_stub_resumes_and_loads_the_first_ones_session() {
    let dir = tempfile::tempdir().unwrap();
    write_stub_script(dir.path(), &[say("hi there", Some("m1")), end(None)]);
    let mut first = spawn_stub(dir.path());
    let session = run_client(&mut first, async |cx, _events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        prompt(&cx, &session, "hello")?.await.expect("answered")?;
        Ok(session)
    })
    .await
    .unwrap();
    assert_eq!(session.to_string(), "stub-0001");
    drop(first);

    let mut second = spawn_stub(dir.path());
    let (resumed_events, unknown, loaded_events, next_id) =
        run_client(&mut second, async |cx, mut events| {
            initialize(&cx).await?;
            cx.send_request(ResumeSessionRequest::new(session.clone(), dir.path()))
                .block_task()
                .await?;
            let resumed_events = drain(&mut events);
            let unknown = cx
                .send_request(ResumeSessionRequest::new("stub-0099", dir.path()))
                .block_task()
                .await
                .is_err();
            cx.send_request(LoadSessionRequest::new(session.clone(), dir.path()))
                .block_task()
                .await?;
            let loaded_events = drain(&mut events);
            let next_id = new_session(&cx, dir.path()).await?;
            Ok((resumed_events, unknown, loaded_events, next_id))
        })
        .await
        .unwrap();
    assert!(resumed_events.is_empty(), "resume sends no update");
    assert!(unknown, "an unknown session id is an error");
    assert_eq!(loaded_events, ["user hello", "say m1 hi there"]);
    assert_eq!(next_id.to_string(), "stub-0002", "the counter goes on");
}

#[tokio::test]
async fn the_log_records_env_cwd_and_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let mut stub = Command::new(stub_acp());
    stub.current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env("MOA_SESSION", "moa-session")
        .env("MOA_BRAIN", "/tmp/brain")
        .env("CLAUDE_CODE_EXECUTABLE", "/tmp/fake-claude")
        .env("CLAUDE_AGENT_ACP_PROTOCOL", "1")
        .env_remove("CLAUDE_CONFIG_DIR");
    let mut child = spawn(stub);
    let pid = child.id().expect("the stub runs");
    run_client(&mut child, async |cx, _events| {
        initialize(&cx).await?;
        let system = SystemPrompt::new(vec![ContentBlock::Text(TextContent::new("Be moa."))])
            .mode(SystemPromptMode::Append);
        cx.send_request(NewSessionRequest::new(dir.path()).system_prompt(system))
            .block_task()
            .await?;
        Ok(())
    })
    .await
    .unwrap();
    let init = &logged(dir.path(), "initialize")[0];
    let env = &init["env"];
    assert_eq!(env["MOA_SESSION"], "moa-session");
    assert_eq!(env["MOA_BRAIN"], "/tmp/brain");
    assert_eq!(env["CLAUDE_CONFIG_DIR"], Value::Null);
    assert_eq!(env["CLAUDE_CODE_EXECUTABLE"], "/tmp/fake-claude");
    assert_eq!(env["CLAUDE_AGENT_ACP_PROTOCOL"], "1");
    assert_eq!(env["PATH"], std::env::var("PATH").unwrap());
    assert_eq!(init["pid"], pid, "the stub's own pid");
    let cwd = std::path::PathBuf::from(init["cwd"].as_str().unwrap());
    assert_eq!(
        cwd.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
    let new = &logged(dir.path(), "session/new")[0];
    assert_eq!(
        new["params"]["systemPrompt"],
        json!({"mode": "append", "content": [{"type": "text", "text": "Be moa."}]})
    );
}

#[tokio::test]
async fn the_second_prompt_plays_the_second_turn() {
    let dir = tempfile::tempdir().unwrap();
    write_stub_script(
        dir.path(),
        &[
            say("a", None),
            end(None),
            say("b", None),
            end(Some("max_tokens")),
            StubStep::Burst { n: 3 },
            end(None),
        ],
    );
    let mut first = spawn_stub(dir.path());
    let session = run_client(&mut first, async |cx, mut events| {
        initialize(&cx).await?;
        let session = new_session(&cx, dir.path()).await?;
        let stop = prompt(&cx, &session, "one")?.await.expect("answered")?;
        assert_eq!(
            (stop, drain(&mut events)),
            (StopReason::EndTurn, ["say  a"].map(String::from).to_vec())
        );
        Ok(session)
    })
    .await
    .unwrap();
    drop(first);

    // A restarted stub goes on with the next turn.
    let mut second = spawn_stub(dir.path());
    run_client(&mut second, async |cx, mut events| {
        initialize(&cx).await?;
        cx.send_request(ResumeSessionRequest::new(session.clone(), dir.path()))
            .block_task()
            .await?;
        let stop = prompt(&cx, &session, "two")?.await.expect("answered")?;
        assert_eq!(stop, StopReason::MaxTokens);
        assert_eq!(drain(&mut events), ["say  b"]);
        let stop = prompt(&cx, &session, "three")?.await.expect("answered")?;
        assert_eq!(stop, StopReason::EndTurn);
        assert_eq!(
            drain(&mut events),
            ["say burst 0 ", "say burst 1 ", "say burst 2 "]
        );
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(logged(dir.path(), "burst.done")[0]["params"]["n"], 3);
}
