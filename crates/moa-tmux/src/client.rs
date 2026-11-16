//! The control client: one `tmux -C` child, replies matched to commands in order, and one
//! ordered stream of events (notifications and tagged replies).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::notify::Notification;
use moa_platform::{Spawner, kill_term};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};

use crate::parse::{Frame, Parser};

/// What the first command asks the server: version, pid, start time, our own name.
pub const INFO_FORMAT: &str = "V #{version} #{pid} #{start_time} #{client_name}";

/// How long `close` waits for the child to exit before it signals it.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// How long `connect` waits for the server's first reply.
const CONNECT_WAIT: Duration = Duration::from_secs(5);

pub struct ConnectOptions {
    /// The tmux program (an absolute path).
    pub tmux: PathBuf,
    /// Which server: `-L name` or `-S path`.
    pub socket: Vec<OsString>,
    /// The whole environment of the child, which is cleared first.
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub version: String,
    pub generation: Generation,
    /// The control client's own name, as tmux's hooks and `-c` know it.
    pub client: ClientName,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("can't run tmux: {0}")]
    Spawn(io::Error),
    /// tmux's first line on stderr (there is no server, or it refused us).
    #[error("{0}")]
    Exited(String),
    #[error("tmux {0} is too old; moa needs tmux 3.4 or newer")]
    TooOld(String),
    #[error("tmux said something unexpected: {0}")]
    Protocol(&'static str),
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CmdError {
    /// The text of tmux's `%error` block.
    #[error("{0}")]
    Tmux(String),
    /// Never written: `Cmd::check` said no.
    #[error("{0}")]
    Refused(&'static str),
    #[error("the terminal server connection closed")]
    Gone,
}

type Reply = Result<Vec<String>, CmdError>;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Notification(Notification),
    /// The results of a tagged `send`, in command order, at the moment the last one came.
    Replies {
        tag: u64,
        results: Vec<Reply>,
    },
}

/// Ends (`None`) once the client has exited.
pub type Events = mpsc::UnboundedReceiver<Event>;

/// The arguments after the tmux program. `-u` because a daemon started at login may have no
/// `LANG`, and `-N` so that it never starts a server.
pub fn ctl_argv(socket: &[OsString]) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["-u".into(), "-N".into()];
    v.extend(socket.iter().cloned());
    v.extend(
        [
            "-C",
            "new-session",
            "-A",
            "-s",
            "_moa",
            "-f",
            "no-output,ignore-size,no-detach-on-destroy",
            "-x",
            "10",
            "-y",
            "3",
            "/bin/sh",
        ]
        .map(OsString::from),
    );
    v
}

/// `major.minor` from "3.4", "3.4a", "3.8", "next-3.9".
fn major_minor(v: &str) -> Option<(u32, u32)> {
    let v = v.strip_prefix("next-").unwrap_or(v);
    let digits: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let (major, minor) = digits.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether moa supports this tmux version: 3.4 or newer.
pub fn version_ok(v: &str) -> bool {
    v == "master" || major_minor(v).is_some_and(|mm| mm >= (3, 4))
}

/// From 3.8 tmux holds notifications back until after a reply block.
fn notifications_after_blocks(v: &str) -> bool {
    // Unsure (`master`, or a version that doesn't parse) keeps in-block notifications on:
    // the parser's `%`-line rule then applies, which is the safer way to be wrong.
    match v.strip_prefix("next-") {
        Some(rest) => major_minor(rest).is_some_and(|mm| mm >= (3, 9)),
        None => major_minor(v).is_some_and(|mm| mm >= (3, 8)),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What happens when the last result of a batch is in.
enum Done {
    Wait(oneshot::Sender<Reply>),
    Tag(u64),
    /// Untagged: only failures are logged, by command name.
    Log(Vec<String>),
}

struct BatchState {
    results: Vec<Option<Reply>>,
    remaining: usize,
    done: Option<Done>,
}

struct Batch(Mutex<BatchState>);

impl Batch {
    /// Records one result; delivers the batch if it was the last.
    fn set(&self, index: usize, r: Reply, events: Option<&mpsc::UnboundedSender<Event>>) {
        let finished = {
            let mut st = lock(&self.0);
            st.results[index] = Some(r);
            st.remaining -= 1;
            if st.remaining == 0 {
                let results = st.results.iter_mut().filter_map(Option::take).collect();
                st.done.take().map(|d| (results, d))
            } else {
                None
            }
        };
        if let Some((results, done)) = finished {
            deliver(results, done, events);
        }
    }
}

fn deliver(mut results: Vec<Reply>, done: Done, events: Option<&mpsc::UnboundedSender<Event>>) {
    match done {
        Done::Wait(tx) => {
            let _ = tx.send(results.pop().unwrap_or(Err(CmdError::Gone)));
        }
        Done::Tag(tag) => {
            if let Some(ev) = events {
                let _ = ev.send(Event::Replies { tag, results });
            }
        }
        Done::Log(names) => {
            for (name, r) in names.iter().zip(&results) {
                if let Err(e @ (CmdError::Tmux(_) | CmdError::Refused(_))) = r {
                    let text = e.to_string();
                    let first = text.lines().next().unwrap_or("");
                    tracing::warn!("tmux refused {name}: {first}");
                }
            }
        }
    }
}

struct Slot {
    batch: Arc<Batch>,
    index: usize,
    /// For a command that nests: its blocks are absorbed up to the block that holds this.
    sentinel: Option<Sentinel>,
}

struct Sentinel {
    text: String,
    /// The first error among the absorbed blocks.
    error: Option<String>,
}

/// What the writer and the reader share. The writer pushes a command's slot before it
/// writes the line, so the reader's pop always matches.
struct Shared {
    pending: VecDeque<Slot>,
    /// Set by the reader at the end of the output: nothing more will be answered.
    gone: bool,
    events: Option<mpsc::UnboundedSender<Event>>,
}

enum Msg {
    Batch { cmds: Vec<Cmd>, done: Done },
    Close,
}

pub struct ControlClient {
    tx: mpsc::UnboundedSender<Msg>,
    pid: u32,
    exited: watch::Receiver<bool>,
}

impl ControlClient {
    /// Starts a control client on the server named by `opts` (never starting a server) and
    /// reads the server's version, pid and start time.
    pub async fn connect(
        opts: &ConnectOptions,
    ) -> Result<(ControlClient, Events, ServerInfo), ConnectError> {
        let mut cmd = Command::new(&opts.tmux);
        cmd.args(ctl_argv(&opts.socket))
            .env_clear()
            .envs(opts.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Spawner.spawn(&mut cmd).map_err(ConnectError::Spawn)?;
        let pid = child.id();
        let stdin = child.stdin.take().map(ChildStdin::from_std).transpose();
        let stdout = child.stdout.take().map(ChildStdout::from_std).transpose();
        let stderr = child.stderr.take().map(ChildStderr::from_std).transpose();
        // The reaper reports the exit, so `close` can wait for it and nothing signals a pid
        // that is gone.
        let (exit_tx, exited) = watch::channel(false);
        std::thread::spawn(move || {
            drop(child.wait());
            let _ = exit_tx.send(true);
        });
        let (Ok(Some(mut stdin)), Ok(Some(mut stdout)), Ok(Some(stderr))) = (stdin, stdout, stderr)
        else {
            kill_unless_exited(pid, &exited);
            return Err(ConnectError::Protocol("no pipes to the child"));
        };
        let first_line = first_stderr_line(stderr);

        let first = Cmd::new("display-message").args(["-p", INFO_FORMAT]).line() + "\n";
        if stdin.write_all(first.as_bytes()).await.is_err() {
            return Err(exited_error(first_line).await);
        }

        let (ev_tx, events) = mpsc::unbounded_channel();
        let mut parser = Parser::new();
        let mut buf = vec![0u8; 64 << 10];
        let mut leftover = Vec::new();
        let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
        let reply = 'read: loop {
            let n = match tokio::time::timeout_at(deadline, stdout.read(&mut buf)).await {
                Err(_) => {
                    kill_unless_exited(pid, &exited);
                    return Err(ConnectError::Protocol("tmux didn't answer"));
                }
                Ok(Ok(0) | Err(_)) => return Err(exited_error(first_line).await),
                Ok(Ok(n)) => n,
            };
            let frames = match parser.push(&buf[..n]) {
                Ok(f) => f,
                Err(_) => {
                    kill_unless_exited(pid, &exited);
                    return Err(ConnectError::Protocol("a line was too long"));
                }
            };
            let mut frames = frames.into_iter();
            while let Some(f) = frames.next() {
                match f {
                    Frame::Reply {
                        flags: 1,
                        error,
                        lines,
                    } => {
                        leftover.extend(frames);
                        break 'read (error, lines);
                    }
                    // Notifications before our answer still go out, in order.
                    Frame::Notification(n) => {
                        let _ = ev_tx.send(Event::Notification(n));
                    }
                    _ => {}
                }
            }
        };
        let (error, lines) = reply;
        if error {
            kill_unless_exited(pid, &exited);
            return Err(ConnectError::Protocol("the first command failed"));
        }
        let info = parse_info(&lines).ok_or_else(|| {
            kill_unless_exited(pid, &exited);
            ConnectError::Protocol("the first reply didn't parse")
        })?;
        parser.set_notifications_in_blocks(!notifications_after_blocks(&info.version));

        let shared = Arc::new(Mutex::new(Shared {
            pending: VecDeque::new(),
            gone: false,
            events: Some(ev_tx.clone()),
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(stdin, rx, Arc::clone(&shared)));
        tokio::spawn(read_loop(
            stdout,
            parser,
            leftover,
            shared,
            ev_tx,
            pid,
            exited.clone(),
        ));
        let client = ControlClient { tx, pid, exited };
        if !version_ok(&info.version) {
            let version = info.version.clone();
            client.close().await;
            return Err(ConnectError::TooOld(version));
        }
        Ok((client, events, info))
    }

    /// Runs one command and waits for its reply.
    pub async fn run(&self, cmd: &Cmd) -> Result<Vec<String>, CmdError> {
        let (tx, rx) = oneshot::channel();
        let msg = Msg::Batch {
            cmds: vec![cmd.clone()],
            done: Done::Wait(tx),
        };
        if self.tx.send(msg).is_err() {
            return Err(CmdError::Gone);
        }
        rx.await.unwrap_or(Err(CmdError::Gone))
    }

    /// Writes the commands in one go. With a tag, the results arrive as `Event::Replies`;
    /// without one, failures are logged. A batch in which every command is refused is
    /// answered at once, possibly before earlier batches that are still in flight.
    pub fn send(&self, cmds: Vec<Cmd>, tag: Option<u64>) {
        let done = match tag {
            Some(t) => Done::Tag(t),
            None => Done::Log(cmds.iter().map(|c| c.name().to_string()).collect()),
        };
        let _ = self.tx.send(Msg::Batch { cmds, done });
    }

    /// Closes the child's stdin, waits up to 1 s for it to exit, then signals it.
    pub async fn close(self) {
        let _ = self.tx.send(Msg::Close);
        let mut exited = self.exited.clone();
        let waited = tokio::time::timeout(CLOSE_WAIT, exited.wait_for(|gone| *gone)).await;
        if waited.is_err() {
            kill_unless_exited(self.pid, &self.exited);
        }
    }

    #[doc(hidden)]
    pub fn child_pid(&self) -> u32 {
        self.pid
    }
}

/// Signals `pid` only while its reaper has not reported the exit, so a reused pid is
/// never signalled.
fn kill_unless_exited(pid: u32, exited: &watch::Receiver<bool>) {
    if !*exited.borrow() {
        kill_term(pid);
    }
}

/// Reads stderr to its end, handing back the first line and discarding the rest.
fn first_stderr_line(stderr: ChildStderr) -> oneshot::Receiver<String> {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut tx = Some(tx);
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(tx) = tx.take() {
                let _ = tx.send(line);
            }
        }
    });
    rx
}

async fn exited_error(first_line: oneshot::Receiver<String>) -> ConnectError {
    let line = tokio::time::timeout(CLOSE_WAIT, first_line)
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "tmux exited".to_string());
    ConnectError::Exited(line)
}

fn parse_info(lines: &[String]) -> Option<ServerInfo> {
    let [line] = lines else { return None };
    let mut f = line.strip_prefix("V ")?.splitn(4, ' ');
    let version = f.next()?.to_string();
    let pid = f.next()?.parse().ok()?;
    let start = f.next()?.parse().ok()?;
    let client = f.next().filter(|c| !c.is_empty())?;
    Some(ServerInfo {
        version,
        generation: Generation { pid, start },
        client: ClientName(client.to_string()),
    })
}

/// The one task that writes: it owns stdin and queues each command's slot first.
async fn write_loop(
    mut stdin: ChildStdin,
    mut rx: mpsc::UnboundedReceiver<Msg>,
    shared: Arc<Mutex<Shared>>,
) {
    let mut nonce = 0u64;
    while let Some(msg) = rx.recv().await {
        let Msg::Batch { cmds, done } = msg else {
            break;
        };
        let mut results: Vec<Option<Reply>> = Vec::with_capacity(cmds.len());
        let mut lines = String::new();
        let mut written = Vec::new();
        let mut sentinels: Vec<Option<String>> = Vec::new();
        for (i, cmd) in cmds.iter().enumerate() {
            match cmd.check() {
                Ok(()) => {
                    results.push(None);
                    written.push(i);
                    lines.push_str(&cmd.line());
                    lines.push('\n');
                    // A command that runs others gets one block per command it ran; a
                    // sentinel right after marks where they end.
                    sentinels.push(cmd.nests().then(|| {
                        nonce += 1;
                        let text = format!("Z {nonce}");
                        let probe = Cmd::new("display-message").args(["-p", text.as_str()]);
                        lines.push_str(&probe.line());
                        lines.push('\n');
                        text
                    }));
                }
                Err(r) => results.push(Some(Err(CmdError::Refused(r.0)))),
            }
        }
        let remaining = written.len();
        let batch = Arc::new(Batch(Mutex::new(BatchState {
            results,
            remaining,
            done: Some(done),
        })));
        let events = {
            let mut sh = lock(&shared);
            if sh.gone {
                // The output has ended: nobody will answer.
                let ev = sh.events.clone();
                drop(sh);
                batch.finish_refused_only(ev.as_ref());
                for i in written {
                    batch.set(i, Err(CmdError::Gone), ev.as_ref());
                }
                continue;
            }
            for (&index, sentinel) in written.iter().zip(sentinels) {
                sh.pending.push_back(Slot {
                    batch: Arc::clone(&batch),
                    index,
                    sentinel: sentinel.map(|text| Sentinel { text, error: None }),
                });
            }
            sh.events.clone()
        };
        if remaining == 0 {
            batch.finish_refused_only(events.as_ref());
            continue;
        }
        if stdin.write_all(lines.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            // The reader sees the end of the output and answers `Gone` for every slot.
            break;
        }
    }
    // Dropping stdin closes the child's input, and it exits.
    drop(stdin);
}

impl Batch {
    /// Delivers a batch in which nothing was written (all refused) straight away.
    fn finish_refused_only(&self, events: Option<&mpsc::UnboundedSender<Event>>) {
        let finished = {
            let mut st = lock(&self.0);
            if st.remaining != 0 {
                return;
            }
            let results = st.results.iter_mut().filter_map(Option::take).collect();
            st.done.take().map(|d| (results, d))
        };
        if let Some((results, done)) = finished {
            deliver(results, done, events);
        }
    }
}

/// The one task that reads: it answers slots in order and forwards notifications.
async fn read_loop(
    mut stdout: ChildStdout,
    mut parser: Parser,
    leftover: Vec<Frame>,
    shared: Arc<Mutex<Shared>>,
    events: mpsc::UnboundedSender<Event>,
    pid: u32,
    exited: watch::Receiver<bool>,
) {
    for frame in leftover {
        handle(frame, &shared, &events);
    }
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        match parser.push(&buf[..n]) {
            Ok(frames) => {
                for frame in frames {
                    handle(frame, &shared, &events);
                }
            }
            Err(e) => {
                tracing::warn!("dropping the tmux connection: {e}");
                kill_unless_exited(pid, &exited);
                break;
            }
        }
    }
    let drained = {
        let mut sh = lock(&shared);
        sh.gone = true;
        sh.events = None;
        std::mem::take(&mut sh.pending)
    };
    for slot in drained {
        slot.batch
            .set(slot.index, Err(CmdError::Gone), Some(&events));
    }
    // `events` drops here, and the stream ends.
}

fn handle(frame: Frame, shared: &Mutex<Shared>, events: &mpsc::UnboundedSender<Event>) {
    match frame {
        // Flag 0 is a block tmux ran for itself (a hook, a config file), not our command.
        Frame::Reply { flags: 0, .. } => {}
        // tmux reports a file it couldn't parse outside any block: a bare line before 3.8,
        // `%config-error` from 3.8, and the file's own block still ends with `%end`.
        Frame::Stray => file_error(shared),
        Frame::Reply { error, lines, .. } => {
            let mut sh = lock(shared);
            let Some(front) = sh.pending.front_mut() else {
                tracing::warn!("a reply from tmux matched no command");
                return;
            };
            if let Some(s) = &mut front.sentinel {
                if error || lines != [s.text.as_str()] {
                    // One of the blocks of the command that nests.
                    if error && s.error.is_none() {
                        s.error = Some(lines.join("\n"));
                    }
                    return;
                }
                // The sentinel itself: the command is done, and callers never see it.
                let failure = s.error.take();
                let Some(slot) = sh.pending.pop_front() else {
                    return;
                };
                drop(sh);
                let r = failure.map_or(Ok(Vec::new()), |e| Err(CmdError::Tmux(e)));
                slot.batch.set(slot.index, r, Some(events));
                return;
            }
            let Some(slot) = sh.pending.pop_front() else {
                return;
            };
            drop(sh);
            let r = if error {
                Err(CmdError::Tmux(lines.join("\n")))
            } else {
                Ok(lines)
            };
            slot.batch.set(slot.index, r, Some(events));
        }
        Frame::Notification(n) => {
            if n == Notification::Other("config-error") {
                file_error(shared);
            }
            let _ = events.send(Event::Notification(n));
        }
    }
}

/// A file that didn't parse fails the command that nests, which is at the front.
fn file_error(shared: &Mutex<Shared>) {
    let mut sh = lock(shared);
    if let Some(s) = sh.pending.front_mut().and_then(|f| f.sentinel.as_mut()) {
        s.error
            .get_or_insert_with(|| "the file has an error".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ok_table() {
        for v in ["3.4", "3.4a", "3.8", "3.10", "4.0", "next-3.9", "master"] {
            assert!(version_ok(v), "{v}");
        }
        for v in ["3.3a", "3.2", "2.9a", "3", "", "openbsd-7", "next-"] {
            assert!(!version_ok(v), "{v}");
        }
    }

    #[test]
    fn the_notification_knob_flips_at_3_8() {
        for v in ["3.8", "3.8a", "4.0", "next-3.9", "next-3.10"] {
            assert!(notifications_after_blocks(v), "{v}");
        }
        for v in ["3.4", "3.5a", "3.7", "next-3.8", "master", "", "openbsd-7"] {
            assert!(!notifications_after_blocks(v), "{v}");
        }
    }

    #[test]
    fn the_argv_starts_utf8_and_never_starts_a_server() {
        let argv = ctl_argv(&["-L".into(), "x".into()]);
        assert_eq!(&argv[..4], ["-u", "-N", "-L", "x"].map(OsString::from));
        assert!(argv.iter().any(|a| a == "-C"));
    }

    #[test]
    fn info_parses() {
        let l = |s: &str| vec![s.to_string()];
        let i = parse_info(&l("V 3.8 123 1700000000 client-9")).unwrap();
        assert_eq!(
            i.generation,
            Generation {
                pid: 123,
                start: 1700000000
            }
        );
        assert_eq!(i.client, ClientName("client-9".into()));
        assert!(parse_info(&l("V 3.8 x 1 c")).is_none());
        assert!(parse_info(&l("V 3.8 1 1")).is_none());
        assert!(parse_info(&[]).is_none());
    }
}
