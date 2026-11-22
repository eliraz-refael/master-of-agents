//! A stand-in for moa's agent, the ACP adapter: it plays a script of `StubStep`s, one turn per
//! prompt, so no test runs the real adapter. Like `fake-claude` it reads its script from its
//! cwd and writes only inside it: a log of what it received, and what a restarted stub needs to
//! go on (turn and session counters, each session's transcript).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
    InitializeResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, PermissionOption, PromptRequest, PromptResponse, RequestPermissionRequest,
    ResumeSessionRequest, ResumeSessionResponse, SessionCapabilities, SessionId,
    SessionNotification, SessionResumeCapabilities, SessionSystemPromptCapabilities, SessionUpdate,
    StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, ByteStreams, Client, Error};
use moa_testkit::StubStep;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// The argument that makes the stub a child that sleeps, for `Spawn`.
const CHILD: &str = "--child";
/// Like `CHILD`, but it ignores `SIGTERM` and says its pid once it does, for `SpawnStubborn`.
const CHILD_STUBBORN: &str = "--child-stubborn";
/// The file in the stub's cwd that holds its child's pid.
const CHILD_PID: &str = ".stub-acp.child";

/// What the stub keeps between handlers; everything else is on disk, inside `dir`.
struct Stub {
    dir: PathBuf,
    /// A `session/cancel` arrived; `Hang` waits on it.
    cancelled: Notify,
}

fn fail(what: impl ToString) -> Error {
    Error::internal_error().data(Value::String(what.to_string()))
}

fn unknown_session() -> Error {
    Error::invalid_params().data(Value::String("unknown session".into()))
}

fn text(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(s))
}

/// A string from a script (`"completed"`, `"allow_once"`, `"max_tokens"`) as the library's type.
fn parse<T: DeserializeOwned>(s: &str) -> Result<T, Error> {
    serde_json::from_value(Value::String(s.into())).map_err(|e| fail(format!("{s:?}: {e}")))
}

impl Stub {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Starts this binary again in a child mode (`CHILD` or `CHILD_STUBBORN`), which only
    /// sleeps: no system `sleep` or shell is needed, so it runs wherever the stub does. A
    /// failure to start goes to the log, then to the caller.
    fn spawn_child(&self, mode: &str) -> Result<std::process::Child, Error> {
        let exe = std::env::current_exe().map_err(fail)?;
        std::process::Command::new(exe)
            .arg(mode)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .inspect_err(|e| self.log("spawn.failed", &mode, json!({"error": e.to_string()})))
            .map_err(fail)
    }

    /// Appends one `{"method", "params"}` line to the log, with `extra` merged in.
    fn log(&self, method: &str, params: &impl Serialize, extra: Value) {
        let mut line = json!({"method": method, "params": params});
        if let (Some(line), Value::Object(extra)) = (line.as_object_mut(), extra) {
            line.extend(extra);
        }
        append(&self.path(".stub-acp.log"), &line.to_string());
    }

    fn script(&self) -> Vec<StubStep> {
        let Ok(text) = fs::read_to_string(self.path(".stub-acp.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l).unwrap_or_else(|e| panic!("bad script line {l:?}: {e}"))
            })
            .collect()
    }

    /// The `n`-th turn of the script (counting from 0): the steps up to and with the next
    /// `End`, `Hang`, `HangHard` or `Die`. A script that has run out ends the turn at once.
    fn turn(&self, n: usize) -> Vec<StubStep> {
        let steps = self.script();
        let mut turns = steps
            .into_iter()
            .filter(|s| !matches!(s, StubStep::Caps { .. }))
            .fold(vec![Vec::new()], |mut turns, step| {
                let last = matches!(
                    step,
                    StubStep::End { .. } | StubStep::Hang | StubStep::HangHard | StubStep::Die
                );
                turns.last_mut().expect("a turn").push(step);
                if last {
                    turns.push(Vec::new());
                }
                turns
            });
        turns.swap_remove(n.min(turns.len() - 1))
    }

    /// Adds one to the number kept in `.stub-acp/<name>` and returns the new value.
    fn bump(&self, name: &str) -> usize {
        let path = self.path(".stub-acp").join(name);
        let n = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0)
            + 1;
        fs::create_dir_all(path.parent().expect("a folder")).expect("state folder");
        fs::write(&path, n.to_string()).expect("write counter");
        n
    }

    fn transcript(&self, id: &SessionId) -> PathBuf {
        self.path(".stub-acp").join(format!("{id}.jsonl"))
    }

    /// Sends `update` and keeps it in the session's transcript, for a later `session/load`.
    fn emit(
        &self,
        cx: &ConnectionTo<Client>,
        id: &SessionId,
        update: SessionUpdate,
    ) -> Result<(), Error> {
        append(
            &self.transcript(id),
            &serde_json::to_string(&update).map_err(fail)?,
        );
        cx.send_notification(SessionNotification::new(id.clone(), update))
    }

    /// Plays one turn's steps; returns how it ended.
    async fn play(
        &self,
        cx: &ConnectionTo<Client>,
        id: &SessionId,
        steps: Vec<StubStep>,
    ) -> Result<StopReason, Error> {
        let chunk = |s: &str| ContentChunk::new(text(s));
        for step in steps {
            match step {
                StubStep::Caps { .. } => {}
                StubStep::Say { text, id: message } => {
                    let mut c = chunk(&text);
                    if let Some(message) = message {
                        c = c.message_id(message.as_str());
                    }
                    self.emit(cx, id, SessionUpdate::AgentMessageChunk(c))?;
                }
                StubStep::Think { text } => {
                    self.emit(cx, id, SessionUpdate::AgentThoughtChunk(chunk(&text)))?;
                }
                StubStep::Tool {
                    id: tool,
                    title,
                    input,
                } => {
                    let call = ToolCall::new(tool, title).raw_input(input);
                    self.emit(cx, id, SessionUpdate::ToolCall(call))?;
                }
                StubStep::ToolDone { id: tool, status } => {
                    let fields =
                        ToolCallUpdateFields::new().status(parse::<ToolCallStatus>(&status)?);
                    let update = ToolCallUpdate::new(tool, fields);
                    self.emit(cx, id, SessionUpdate::ToolCallUpdate(update))?;
                }
                StubStep::Ask { title, options } => {
                    let options = options
                        .into_iter()
                        .map(|(option, kind)| {
                            Ok(PermissionOption::new(option.clone(), option, parse(&kind)?))
                        })
                        .collect::<Result<Vec<_>, Error>>()?;
                    let fields = ToolCallUpdateFields::new().title(title);
                    let outcome = cx
                        .send_request(RequestPermissionRequest::new(
                            id.clone(),
                            ToolCallUpdate::new("ask", fields),
                            options,
                        ))
                        .block_task()
                        .await?
                        .outcome;
                    self.log("permission.outcome", &outcome, Value::Null);
                }
                StubStep::Burst { n } => {
                    for i in 0..n {
                        let c = chunk(&format!("{i} ")).message_id("burst");
                        self.emit(cx, id, SessionUpdate::AgentMessageChunk(c))?;
                    }
                    self.log("burst.done", &json!({"n": n}), Value::Null);
                }
                StubStep::Stderr { line } => eprintln!("{line}"),
                StubStep::SpawnStubborn => {
                    self.spawn_child(CHILD_STUBBORN)?;
                }
                StubStep::Spawn => {
                    let child = self.spawn_child(CHILD)?;
                    fs::write(self.path(CHILD_PID), child.id().to_string()).map_err(fail)?;
                }
                StubStep::Wait { file } => {
                    while !self.path(&file).exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                StubStep::End { stop } => {
                    return parse(stop.as_deref().unwrap_or("end_turn"));
                }
                StubStep::Hang => {
                    self.cancelled.notified().await;
                    return Ok(StopReason::Cancelled);
                }
                StubStep::HangHard => std::future::pending().await,
                StubStep::Die => std::process::exit(1),
            }
        }
        Ok(StopReason::EndTurn)
    }
}

fn append(path: &Path, line: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{path:?}: {e}"));
    writeln!(file, "{line}").expect("append");
}

/// The initialize line's extra: the environment moa gave its agent (a few values, and the
/// names of every variable), and the cwd.
fn launch_facts() -> Value {
    let var = |name: &str| std::env::var(name).ok();
    let names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    json!({
        "env_names": names,
        "env": {
            "MOA_SESSION": var("MOA_SESSION"),
            "MOA_BRAIN": var("MOA_BRAIN"),
            "CLAUDE_CONFIG_DIR": var("CLAUDE_CONFIG_DIR"),
            "CLAUDE_CODE_EXECUTABLE": var("CLAUDE_CODE_EXECUTABLE"),
            "CLAUDE_AGENT_ACP_PROTOCOL": var("CLAUDE_AGENT_ACP_PROTOCOL"),
            "PATH": var("PATH"),
        },
        "cwd": std::env::current_dir().expect("cwd"),
    })
}

/// What a child does: nothing, until the test's group kill ends it.
async fn sleep_for_good() -> Result<(), Error> {
    tokio::time::sleep(Duration::from_secs(1000)).await;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    match std::env::args().nth(1).as_deref() {
        Some(CHILD) => return sleep_for_good().await,
        Some(CHILD_STUBBORN) => {
            // Handling SIGTERM (and never acting on it) is ignoring it. The pid is written
            // by an atomic rename, so the file is never read half-written, and only once
            // the signal is handled.
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("handle SIGTERM");
            let new = format!("{CHILD_PID}.new");
            fs::write(&new, std::process::id().to_string()).expect("write the pid");
            fs::rename(&new, CHILD_PID).expect("publish the pid");
            tokio::spawn(async move { while term.recv().await.is_some() {} });
            return sleep_for_good().await;
        }
        _ => {}
    }
    let stub = Arc::new(Stub {
        dir: std::env::current_dir().expect("cwd"),
        cancelled: Notify::new(),
    });
    let (s_init, s_new, s_resume, s_load, s_prompt, s_cancel) = (
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub.clone(),
        stub,
    );
    Agent
        .v1()
        .name("stub-acp")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx| {
                s_init.log("initialize", &req, launch_facts());
                let caps = match s_init.script().first() {
                    Some(&StubStep::Caps {
                        load,
                        resume,
                        system_prompt,
                    }) => (load, resume, system_prompt),
                    _ => (true, true, true),
                };
                let mut session = SessionCapabilities::new();
                if caps.1 {
                    session = session.resume(SessionResumeCapabilities::default());
                }
                if caps.2 {
                    session = session.system_prompt(SessionSystemPromptCapabilities::default());
                }
                responder.respond(
                    InitializeResponse::new(req.protocol_version).agent_capabilities(
                        AgentCapabilities::new()
                            .load_session(caps.0)
                            .session_capabilities(session),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, _cx| {
                s_new.log("session/new", &req, Value::Null);
                let id = SessionId::new(format!("stub-{:04}", s_new.bump("sessions")));
                fs::write(s_new.transcript(&id), "").map_err(fail)?;
                responder.respond(NewSessionResponse::new(id))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ResumeSessionRequest, responder, _cx| {
                s_resume.log("session/resume", &req, Value::Null);
                if s_resume.transcript(&req.session_id).exists() {
                    responder.respond(ResumeSessionResponse::new())
                } else {
                    responder.respond_with_error(unknown_session())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest, responder, cx| {
                s_load.log("session/load", &req, Value::Null);
                let Ok(history) = fs::read_to_string(s_load.transcript(&req.session_id)) else {
                    return responder.respond_with_error(unknown_session());
                };
                for line in history.lines() {
                    let update = serde_json::from_str(line).map_err(fail)?;
                    cx.send_notification(SessionNotification::new(req.session_id.clone(), update))?;
                }
                responder.respond(LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            // A prompt is answered from a spawned task, so the loop stays free for `session/cancel`.
            async move |req: PromptRequest, responder, cx| {
                s_prompt.log("session/prompt", &req, Value::Null);
                let stub = s_prompt.clone();
                let task_cx = cx.clone();
                cx.spawn(async move {
                    // A cancel from before this turn isn't this turn's.
                    let _ = tokio::time::timeout(Duration::ZERO, stub.cancelled.notified()).await;
                    for block in req.prompt {
                        let update = SessionUpdate::UserMessageChunk(ContentChunk::new(block));
                        append(
                            &stub.transcript(&req.session_id),
                            &serde_json::to_string(&update).map_err(fail)?,
                        );
                    }
                    let steps = stub.turn(stub.bump("turns") - 1);
                    let stop = stub.play(&task_cx, &req.session_id, steps).await?;
                    responder.respond(PromptResponse::new(stop))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |n: CancelNotification, _cx| {
                s_cancel.log("session/cancel", &n, Value::Null);
                s_cancel.cancelled.notify_one();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}
