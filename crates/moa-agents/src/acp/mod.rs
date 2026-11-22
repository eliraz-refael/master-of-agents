//! The host of moa's agent: the one place moa speaks the agent protocol (ACP). It starts the
//! agent in its own process group, reads everything it says as it arrives and hands it to the
//! core as moa's own types (`central::wire`), and carries the core's commands back. The host
//! holds no policy: what to open, what to send and how to answer a permission request are
//! the core's.
//!
//! Nothing here waits for the core. The handlers only call `to_core`, which never blocks (the
//! daemon's unbounded channel), so the agent's output is always read, however far behind the
//! core is.

mod convert;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, ByteStreams, Client, JsonRpcRequest};
use moa_core::central::wire::{AcpIn, AdapterSpec, Opened, ThreadHow};
use moa_core::text::cut;
use moa_platform::child::{Reaper, each_stderr_line};
use moa_platform::{Spawner, ensure_private_dir, kill_group, lock};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

/// The agent this build was packaged with, if any: set only by the Nix package's build. A
/// plain `cargo` build (every test's) knows none.
pub const BUILT_IN_ADAPTER: Option<&str> = option_env!("MOA_ACP_ADAPTER");

/// How much of each of the agent's stderr lines goes to the log, in characters.
const STDERR_LINE_CHARS: usize = 500;

/// How long `stop` waits for the agent to leave on `SIGTERM` before it sends `SIGKILL`.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// How long `stop` then waits for `SIGKILL` to take.
const KILL_WAIT: Duration = Duration::from_secs(1);

/// Where the host's messages go: the core's input, which never blocks.
pub type ToCore = Arc<dyn Fn(AcpIn) + Send + Sync>;

/// Permission requests waiting for the core's answer, by their number.
type Waiting = Arc<Mutex<HashMap<u64, Responder<RequestPermissionResponse>>>>;

/// One running agent, of one generation.
pub struct Adapter {
    generation: u64,
    to_core: ToCore,
    cx: ConnectionTo<Agent>,
    waiting: Waiting,
    process: Process,
}

/// The agent's process and the task that reads it. Dropped without `stop`, it still sends the
/// agent's process group `SIGTERM` (unless the agent has gone already) and, if the agent is
/// still there after `STOP_WAIT`, `SIGKILL`; once the agent leaves, its reaper ends the rest
/// of the group.
struct Process {
    reaper: Reaper,
    driver: Option<JoinHandle<()>>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut signalled = false;
        self.reaper.kill_unless_exited(|group| {
            signalled = true;
            kill_group(group, libc::SIGTERM);
        });
        if signalled {
            // An agent that ignores `SIGTERM` would otherwise outlive its host.
            let reaper = self.reaper.clone();
            std::thread::spawn(move || {
                std::thread::sleep(STOP_WAIT);
                reaper.kill_unless_exited(|group| kill_group(group, libc::SIGKILL));
            });
        }
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}

/// Says `Exited` once per generation, whichever notices first: the reaper or the
/// connection's end. While the agent is starting a report is held back: if `start` fails,
/// the daemon says why itself, so nothing is said here.
struct Exit {
    generation: u64,
    to_core: ToCore,
    state: Mutex<ExitState>,
}

enum ExitState {
    /// `start` hasn't finished; the first report, if any, waits here.
    Starting(Option<String>),
    Live,
    Said,
}

impl Exit {
    fn say(&self, why: &str) {
        let mut state = lock(&self.state);
        match &mut *state {
            ExitState::Starting(held) => {
                held.get_or_insert_with(|| why.to_string());
            }
            ExitState::Live => {
                *state = ExitState::Said;
                drop(state);
                (self.to_core)(AcpIn::Exited {
                    generation: self.generation,
                    why: why.to_string(),
                });
            }
            ExitState::Said => {}
        }
    }

    /// `start` succeeded: from now on an exit is said, and one held back is said at once.
    fn live(&self) {
        let held = match std::mem::replace(&mut *lock(&self.state), ExitState::Live) {
            ExitState::Starting(held) => held,
            _ => None,
        };
        if let Some(why) = held {
            self.say(&why);
        }
    }

    /// `start` failed: nothing is ever said for this generation.
    fn failed(&self) {
        *lock(&self.state) = ExitState::Said;
    }
}

/// Writes the agent dir's files: `bin/moactl`, a link to moa's own `moactl`, and Claude's
/// settings (the allow-list).
fn write_agent_dir(spec: &AdapterSpec) -> io::Result<()> {
    ensure_private_dir(&spec.cwd).map_err(io::Error::other)?;
    let bin = spec.cwd.join("bin");
    std::fs::create_dir_all(&bin)?;
    let link = bin.join("moactl");
    match std::fs::remove_file(&link) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::os::unix::fs::symlink(&spec.moactl, &link)?;
    let claude = spec.cwd.join(".claude");
    std::fs::create_dir_all(&claude)?;
    std::fs::write(claude.join("settings.json"), &spec.settings)
}

/// Writes `CLAUDE.md` in `dir`, or removes it when there is no text for it.
fn write_claude_md(dir: &Path, text: Option<&str>) -> io::Result<()> {
    let path = dir.join("CLAUDE.md");
    match text {
        Some(text) => std::fs::write(path, text),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// The agent's command: in the agent dir and a process group of its own, with this process's
/// environment, less `unset`, plus `env`, and `<agent dir>/bin` first on `PATH`.
fn command(spec: &AdapterSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for name in &spec.unset {
        cmd.env_remove(name);
    }
    cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    let inherited = match spec.env.iter().find(|(k, _)| k == "PATH") {
        Some((_, path)) => Some(OsString::from(path)),
        None if spec.unset.iter().any(|k| k == "PATH") => None,
        None => std::env::var_os("PATH"),
    };
    let mut path = spec.cwd.join("bin").into_os_string();
    if let Some(rest) = inherited.filter(|p| !p.is_empty()) {
        path.push(":");
        path.push(rest);
    }
    cmd.env("PATH", path);
    cmd
}

impl Adapter {
    /// Writes the agent dir's files, starts the agent, asks what it offers (`initialize`)
    /// and tells the core (`Started`). An error is the caller's to report: nothing is said
    /// through `to_core` for a start that fails. Dropping the future part-way ends the agent.
    pub async fn start(
        generation: u64,
        spec: &AdapterSpec,
        to_core: ToCore,
    ) -> Result<Adapter, String> {
        write_agent_dir(spec)
            .map_err(|e| format!("couldn't set up {}: {e}", spec.cwd.display()))?;
        let mut child = Spawner
            .spawn(&mut command(spec))
            .map_err(|e| format!("couldn't start {}: {e}", spec.program.display()))?;
        let pid = child.id();
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());

        let exit = Arc::new(Exit {
            generation,
            to_core: Arc::clone(&to_core),
            state: Mutex::new(ExitState::Starting(None)),
        });
        // Seen exited while its pid is still its own (unreaped, so its group id can't be
        // reused): whatever it started in its group goes with it, even a process that
        // ignores `SIGTERM`. Only then is it reaped.
        let reaper = Reaper::watch(child, {
            let exit = Arc::clone(&exit);
            move || {
                kill_group(pid, libc::SIGKILL);
                exit.say("moa's agent exited");
            }
        });
        let mut process = Process {
            reaper,
            driver: None,
        };
        let failed = |why: String| {
            exit.failed();
            why
        };
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            return Err(failed("no pipes to moa's agent".into()));
        };
        let (stdin, stdout, stderr) = match (
            ChildStdin::from_std(stdin),
            ChildStdout::from_std(stdout),
            ChildStderr::from_std(stderr),
        ) {
            (Ok(stdin), Ok(stdout), Ok(stderr)) => (stdin, stdout, stderr),
            _ => return Err(failed("couldn't read moa's agent".into())),
        };
        each_stderr_line(stderr, |line| {
            tracing::info!("adapter: {}", cut(&line, STDERR_LINE_CHARS));
        });

        let waiting = Waiting::default();
        let (cx_tx, cx_rx) = tokio::sync::oneshot::channel();
        let connection = connection(generation, &to_core, &waiting);
        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
        let driver_exit = Arc::clone(&exit);
        process.driver = Some(tokio::spawn(async move {
            let closed = Arc::clone(&driver_exit);
            let ended = connection
                .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
                    let _ = cx_tx.send(cx.clone());
                    cx.incoming_closed().await;
                    closed.say("moa's agent closed its connection");
                    Ok(())
                })
                .await;
            if ended.is_err() {
                driver_exit.say("moa's agent's connection failed");
            }
        }));
        let Ok(cx) = cx_rx.await else {
            return Err(failed("moa's agent's connection failed".into()));
        };

        let init = cx
            .send_request(convert::initialize())
            .block_task()
            .await
            .map_err(|e| {
                failed(format!(
                    "moa's agent didn't start: {}",
                    convert::error_text(&e)
                ))
            })?;
        to_core(AcpIn::Started {
            generation,
            caps: convert::caps(&init),
        });
        exit.live();
        Ok(Adapter {
            generation,
            to_core,
            cx,
            waiting,
            process,
        })
    }

    /// The agent's pid, which is also its process group's id.
    pub fn pid(&self) -> u32 {
        self.process.reaper.pid()
    }

    /// Opens a thread in `cwd`; `Opened` says how it went. A new thread's `CLAUDE.md` in
    /// `cwd` is written first, or removed when there is no text for it.
    pub fn open(&self, how: ThreadHow, cwd: &Path) {
        let (generation, to_core) = (self.generation, Arc::clone(&self.to_core));
        let opened = move |result| to_core(AcpIn::Opened { generation, result });
        match how {
            ThreadHow::New {
                system_prompt,
                claude_md,
            } => {
                if let Err(e) = write_claude_md(cwd, claude_md.as_deref()) {
                    opened(Err(format!("couldn't write CLAUDE.md: {e}")));
                    return;
                }
                let new = convert::new_session(cwd, system_prompt.as_deref());
                self.request(new, move |r| {
                    opened(r.map(|new| (convert::new_thread(&new), Opened::New)));
                });
            }
            ThreadHow::Resume { thread } => {
                self.request(convert::resume(&thread, cwd), move |r| {
                    opened(r.map(|_| (thread, Opened::Resumed)));
                });
            }
            // The replay's updates reach the core before `Opened`.
            ThreadHow::Load { thread } => {
                self.request(convert::load(&thread, cwd), move |r| {
                    opened(r.map(|_| (thread, Opened::Loaded)));
                });
            }
        }
    }

    /// Sends `text` to `thread`; `TurnEnded` carries `prompt` back once every update of the
    /// turn has reached the core.
    pub fn prompt(&self, thread: &str, prompt: u64, text: &str) {
        let (generation, to_core) = (self.generation, Arc::clone(&self.to_core));
        self.request(convert::prompt(thread, text), move |r| {
            to_core(AcpIn::TurnEnded {
                generation,
                prompt,
                result: r.map(|done| convert::stop_reason(done.stop_reason)),
            });
        });
    }

    /// Asks the agent to end `thread`'s turn; the turn's `TurnEnded` says how it ended.
    pub fn cancel(&self, thread: &str) {
        if self.cx.send_notification(convert::cancel(thread)).is_err() {
            tracing::warn!("couldn't send a cancel to moa's agent: its connection is gone");
        }
    }

    /// Answers permission request `req` with the chosen option, or cancels it (`None`).
    pub fn answer(&self, req: u64, choice: Option<String>) {
        let Some(responder) = lock(&self.waiting).remove(&req) else {
            tracing::warn!("no permission request {req} is waiting");
            return;
        };
        if responder
            .respond(convert::permission_answer(choice))
            .is_err()
        {
            tracing::warn!("couldn't answer moa's agent: its connection is gone");
        }
    }

    /// Ends the agent and its whole process group: `SIGTERM` to the group, up to 2 s for the
    /// agent to leave, then `SIGKILL` to the group. Once the agent has left, and before it is
    /// reaped, the reaper sends the group `SIGKILL`, so nothing it started outlives it, a
    /// process that ignored the `SIGTERM` included. Signals go only while the agent isn't
    /// reaped, so its group id can't have been reused.
    ///
    /// When the agent has left within the 3 s (2 s, then 1 s after `SIGKILL`), its `Exited`
    /// has been said when this returns, and nothing more is. An agent that outlasts even
    /// `SIGKILL` (stuck in the kernel) is left to its reaper, which ends the group and says
    /// `Exited` whenever it does leave.
    pub async fn stop(mut self) {
        let reaper = &self.process.reaper;
        reaper.kill_unless_exited(|group| kill_group(group, libc::SIGTERM));
        if tokio::time::timeout(STOP_WAIT, reaper.exited())
            .await
            .is_err()
        {
            reaper.kill_unless_exited(|group| kill_group(group, libc::SIGKILL));
            let _ = tokio::time::timeout(KILL_WAIT, reaper.exited()).await;
        }
        if let Some(driver) = self.process.driver.take() {
            driver.abort();
            let _ = driver.await;
        }
    }

    /// Sends `req` so that its answer reaches `done` after every message the agent sent
    /// before it. A request that can't be sent reaches `done` as an error at once.
    fn request<Req>(
        &self,
        req: Req,
        done: impl FnOnce(Result<Req::Response, String>) + Send + 'static,
    ) where
        Req: JsonRpcRequest,
        Req::Response: Send + 'static,
    {
        let slot = Arc::new(Mutex::new(Some(done)));
        let answered = Arc::clone(&slot);
        let sent = self
            .cx
            .prepare_request(req)
            .on_receiving_result(move |result| async move {
                if let Some(done) = lock(&answered).take() {
                    done(result.map_err(|e| convert::error_text(&e)));
                }
                Ok(())
            });
        if let Err(e) = sent
            && let Some(done) = lock(&slot).take()
        {
            done(Err(convert::error_text(&e)));
        }
    }
}

/// The connection's handlers: each only hands the core a message, and never waits.
fn connection(
    generation: u64,
    to_core: &ToCore,
    waiting: &Waiting,
) -> agent_client_protocol::v1::Builder<
    Client,
    impl agent_client_protocol::HandleDispatchFrom<Agent> + 'static,
> {
    let updates = Arc::clone(to_core);
    let asks = Arc::clone(to_core);
    let waiting = Arc::clone(waiting);
    let numbers = AtomicU64::new(1);
    Client
        .v1()
        .name("moa")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                let (thread, update) = convert::notification(n);
                updates(AcpIn::Update {
                    generation,
                    thread,
                    update,
                });
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                let number = numbers.fetch_add(1, Ordering::Relaxed);
                let (thread, title, options) = convert::permission(&req);
                lock(&waiting).insert(number, responder);
                asks(AcpIn::Permission {
                    generation,
                    req: number,
                    thread,
                    title,
                    options,
                });
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
}
