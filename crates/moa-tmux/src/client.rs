//! The control client: one `tmux -C` child, replies matched to commands in order, and one
//! ordered stream of events (notifications and tagged replies).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::notify::Notification;
use moa_platform::child::{Reaper, first_stderr_line};
use moa_platform::{Spawner, kill_term};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::process::{ChildStderr, ChildStdin};
use tokio::sync::{mpsc, oneshot};

use crate::parse::{Frame, Parser};

/// What the first command asks the server: version, pid, start time, our own name.
pub const INFO_FORMAT: &str = "V #{version} #{pid} #{start_time} #{client_name}";

/// How long `close` waits for the child to exit before it signals it.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// How long `connect` usually waits for the server's first reply. A bound on a hang, not a
/// latency target: CI runners are slow and cold, and a loaded machine can take seconds to
/// start tmux. With the daemon's 2 s for its setup after it, each connect attempt is up or
/// given up on within 8 s, under the front door's 10 s wait for an answer. A timed-out
/// attempt is backed off from and tried again without telling the core, so for a server
/// that stays hung the front door gives its own "didn't answer" at 10 s.
pub const CONNECT_WAIT: Duration = Duration::from_secs(6);

pub struct ConnectOptions {
    /// The tmux program (an absolute path).
    pub tmux: PathBuf,
    /// Which server: `-L name` or `-S path`.
    pub socket: Vec<OsString>,
    /// The whole environment of the child, which is cleared first.
    pub env: Vec<(OsString, OsString)>,
    /// How long to wait for the server's first reply: [`CONNECT_WAIT`], or less in a test.
    pub wait: Duration,
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
    /// The server didn't answer within `wait`, and tmux said nothing on stderr.
    #[error("tmux didn't answer in time")]
    TimedOut,
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
/// `LANG`, and `-N` so that it never starts a server. `_moa`'s shell is two words, so tmux
/// execs it itself instead of handing it to the user's login shell.
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
            "-s",
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
    /// Whether the command sources a file, so a file's parse error is its own (a chain of
    /// moves never parses one).
    sources: bool,
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
    reaper: Reaper,
}

impl ControlClient {
    /// Starts a control client on the server named by `opts` (never starting a server) and
    /// reads the server's version, pid and start time.
    pub async fn connect(
        opts: &ConnectOptions,
    ) -> Result<(ControlClient, Events, ServerInfo), ConnectError> {
        // The child holds a reader of its own output. tmux never lets go of a control client
        // whose output it couldn't write: if this process dies with output on its way, a
        // write to a pipe with no reader fails, and tmux 3.4 then keeps the client attached
        // for good, and `@moa_ctl` with it. With the child's reader left, late output lands
        // in the pipe and the client leaves on its input's end.
        // Limit: only while the late output fits in the pipe (16–64 KiB). With more unread
        // output when this process dies (a lagging reader, a large reply, a batch in
        // flight), tmux keeps the client attached anyway: 3.4 for good, 3.8 too with many
        // unread replies. The daemon's link stops such a client on its next connect.
        let mut cmd = Command::new(&opts.tmux);
        cmd.args(ctl_argv(&opts.socket))
            .env_clear()
            .envs(opts.env.iter().cloned())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, out_r) = Spawner
            .spawn_with_held_stdout(&mut cmd)
            .map_err(ConnectError::Spawn)?;
        let stdin = child.stdin.take().map(ChildStdin::from_std).transpose();
        let stdout = pipe::Receiver::from_owned_fd(OwnedFd::from(out_r)).map(Some);
        let stderr = child.stderr.take().map(ChildStderr::from_std).transpose();
        // The reaper reports the exit, so `close` can wait for it and nothing signals a pid
        // that is gone.
        let reaper = Reaper::watch(child, || {});
        let (Ok(Some(mut stdin)), Ok(Some(mut stdout)), Ok(Some(stderr))) = (stdin, stdout, stderr)
        else {
            reaper.kill_unless_exited(kill_term);
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
        let deadline = tokio::time::Instant::now() + opts.wait;
        let reply = 'read: loop {
            let n = match tokio::time::timeout_at(deadline, stdout.read(&mut buf)).await {
                Err(_) => {
                    reaper.kill_unless_exited(kill_term);
                    // If tmux said why on stderr, that is the better error.
                    return Err(match stderr_line(first_line).await {
                        Some(line) => ConnectError::Exited(line),
                        None => ConnectError::TimedOut,
                    });
                }
                Ok(Ok(0)) => return Err(exited_error(first_line).await),
                Ok(Err(_)) => {
                    // A read error doesn't mean the child is gone.
                    reaper.kill_unless_exited(kill_term);
                    return Err(exited_error(first_line).await);
                }
                Ok(Ok(n)) => n,
            };
            let frames = match parser.push(&buf[..n]) {
                Ok(f) => f,
                Err(_) => {
                    reaper.kill_unless_exited(kill_term);
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
            reaper.kill_unless_exited(kill_term);
            return Err(ConnectError::Protocol("the first command failed"));
        }
        let info = parse_info(&lines).ok_or_else(|| {
            reaper.kill_unless_exited(kill_term);
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
            reaper.clone(),
        ));
        let client = ControlClient { tx, reaper };
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
        let waited = tokio::time::timeout(CLOSE_WAIT, self.reaper.exited()).await;
        if waited.is_err() {
            self.reaper.kill_unless_exited(kill_term);
        }
    }

    #[doc(hidden)]
    pub fn child_pid(&self) -> u32 {
        self.reaper.pid()
    }
}

/// The first stderr line, if tmux wrote one within a short wait.
async fn stderr_line(first_line: oneshot::Receiver<String>) -> Option<String> {
    let line = tokio::time::timeout(CLOSE_WAIT, first_line).await;
    line.ok().and_then(Result::ok).filter(|l| !l.is_empty())
}

async fn exited_error(first_line: oneshot::Receiver<String>) -> ConnectError {
    let line = stderr_line(first_line).await;
    ConnectError::Exited(line.unwrap_or_else(|| "tmux exited".to_string()))
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
        let mut sentinels: Vec<Option<Sentinel>> = Vec::new();
        for (i, cmd) in cmds.iter().enumerate() {
            match cmd.check() {
                Ok(()) => {
                    results.push(None);
                    written.push(i);
                    lines.push_str(&cmd.line());
                    lines.push('\n');
                    // A command that runs others (`source-file`, a chain) gets one block per
                    // command it ran; a sentinel right after marks where they end. A chain
                    // stops at its first failure, and the sentinel, on its own line, still
                    // runs.
                    sentinels.push(cmd.nests().then(|| {
                        nonce += 1;
                        let text = format!("moa:sentinel {nonce}");
                        let probe = Cmd::new("display-message").args(["-p", text.as_str()]);
                        lines.push_str(&probe.line());
                        lines.push('\n');
                        Sentinel {
                            text,
                            error: None,
                            sources: cmd.parts().iter().any(|p| p.name() == "source-file"),
                        }
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
                    sentinel,
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
    mut stdout: pipe::Receiver,
    mut parser: Parser,
    leftover: Vec<Frame>,
    shared: Arc<Mutex<Shared>>,
    events: mpsc::UnboundedSender<Event>,
    reaper: Reaper,
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
                reaper.kill_unless_exited(kill_term);
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
                    // One of the blocks of the command that nests, whose first error is its
                    // answer (a chain's has no blocks after it). tmux keeps its config
                    // parse errors server-wide (`cfg_causes`), so another client's parse
                    // error can be blamed on our `source-file`, or ours on theirs. That
                    // stays inside this one slot and never shifts the FIFO.
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

/// A file that didn't parse fails the command that sources it, when that is at the front.
/// tmux keeps config parse errors server-wide (`cfg_causes`), so this may also be another
/// client's error; it is still only ever applied to the one source-file slot.
fn file_error(shared: &Mutex<Shared>) {
    let mut sh = lock(shared);
    let front = sh.pending.front_mut().and_then(|f| f.sentinel.as_mut());
    if let Some(s) = front.filter(|s| s.sources) {
        s.error
            .get_or_insert_with(|| "the file has an error".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file's parse error, said outside any block (a bare line before 3.8, `%config-error`
    /// from 3.8), fails only a command that sources a file. A chain at the front stays `Ok`.
    #[test]
    fn a_file_error_fails_only_a_command_that_sources_one() {
        for (sources, want) in [
            (false, Ok(vec![])),
            (true, Err(CmdError::Tmux("the file has an error".into()))),
        ] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let batch = Arc::new(Batch(Mutex::new(BatchState {
                results: vec![None],
                remaining: 1,
                done: Some(Done::Tag(1)),
            })));
            let text = "moa:sentinel 1".to_string();
            let slot = Slot {
                batch,
                index: 0,
                sentinel: Some(Sentinel {
                    text: text.clone(),
                    error: None,
                    sources,
                }),
            };
            let shared = Mutex::new(Shared {
                pending: VecDeque::from([slot]),
                gone: false,
                events: Some(tx.clone()),
            });
            let block = |lines: Vec<String>| Frame::Reply {
                flags: 1,
                error: false,
                lines,
            };
            for frame in [
                block(vec![]),
                Frame::Stray,
                Frame::Notification(Notification::Other("config-error")),
                block(vec![]),
                block(vec![text]),
            ] {
                handle(frame, &shared, &tx);
            }
            let mut replies = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                if let Event::Replies { tag, results } = ev {
                    replies.push((tag, results));
                }
            }
            assert_eq!(replies, [(1, vec![want])], "sources {sources}");
            assert!(lock(&shared).pending.is_empty());
        }
    }

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
        // Two words, so tmux execs the shell itself and not through the user's.
        assert_eq!(
            argv[argv.len() - 2..],
            ["/bin/sh", "-s"].map(OsString::from)
        );
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
