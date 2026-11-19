//! The drain loop: `Core`, running a drain through `handle`, and committing it.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::effect::{
    Down, Effect, Input, LogLevel, LogLine, Publish, TmuxIn, TmuxToken, check_order,
};
use moa_core::hook::{HookEnvelope, HookLine};
use moa_core::op::{Done, OpError};
use moa_core::publish::{CoreEvent, Model};
use moa_core::state::State;
use moa_core::{Now, handle};
use moa_store::actor::StoreHandle;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;

use super::recover::fail_held;
use super::*;
use crate::clock::Clock;
use crate::scheduler::{SchedCmd, Wake};

pub(super) struct Core {
    /// The core's own inbox, for answers it gives itself. Weak, so the loop still ends
    /// when every handle has gone.
    pub(super) inbox: mpsc::WeakUnboundedSender<Item>,
    /// The terminal link, when the daemon has one.
    pub(super) link: Option<mpsc::UnboundedSender<TmuxCmd>>,
    pub(super) state: State,
    pub(super) store: SharedStore,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) tz: TimeZone,
    pub(super) conns: Conns,
    pub(super) stats: Arc<Stats>,
    pub(super) model_tx: watch::Sender<Arc<Model>>,
    pub(super) events: broadcast::Sender<CoreEvent>,
    pub(super) generation_tx: watch::Sender<u64>,
    pub(super) sched: mpsc::UnboundedSender<SchedCmd>,
    pub(super) backups: TaskTracker,
    /// Backups the drain asked for: started once its commit has been sent, so the commit is
    /// ahead of the copy on the store thread.
    pub(super) due_backups: Vec<Date>,
    /// Where the store reopens from when its thread dies.
    pub(super) db: PathBuf,
    pub(super) backups_dir: PathBuf,
    /// Failed commits since the last good one; from the third, writes are refused for a while.
    pub(super) failures_in_row: u32,
    /// Items a drain left for the next one: what followed a panic.
    pub(super) carry: VecDeque<Item>,
    /// `StartDrained` panicked once and was sent again: a second panic stops the core, as
    /// one loading the brain does.
    pub(super) start_drained_again: bool,
    /// Where unapplied hooks are kept when the core stops.
    pub(super) spool: Option<PathBuf>,
    pub(super) panic_next: Arc<AtomicU32>,
    pub(super) finished_tx: watch::Sender<Option<Result<(), String>>>,
    /// Dropped with the core, which is what [`CoreHandle::ended`] waits for.
    pub(super) _ended: watch::Sender<()>,
}

/// What a drain leaves the loop to do.
pub(super) enum Flow {
    Go,
    /// The drain held a `stop`; the sender answers once the loop has ended.
    Stop(oneshot::Sender<()>),
    /// The core can't go on: the loop ends and `finished()` says why.
    Fatal,
}

/// How one input went through `handle`.
pub(super) enum Stepped {
    Done,
    /// `handle` panicked. `loading` is true for `Startup` and `Reload`, which aren't retried,
    /// and for a `StartDrained` already sent again.
    Panicked {
        loading: bool,
    },
}

const BUG_REPLY: &str = "moa hit a bug answering that; it reloaded";
/// How long the end of the loop waits for the terminal link to take its hooks down.
const LINK_STOP_WAIT: Duration = Duration::from_secs(2);
pub(super) const UNEXPECTED: &str = "the core stopped unexpectedly; see the log";
pub(super) const BUG_LOADING: &str = "moa hit a bug loading the brain; see the log";

impl Core {
    pub(super) fn store(&self) -> StoreHandle {
        lock(&self.store).clone()
    }

    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Item>) {
        let end = loop {
            if self.carry.is_empty() {
                match inbox.recv().await {
                    Some(item) => self.carry.push_back(item),
                    None => break Flow::Go,
                }
            }
            let work = take_drain(&mut self.carry, &mut inbox);
            self.stats.drains.fetch_add(1, Ordering::SeqCst);
            match self.drain(work).await {
                Flow::Go => {}
                end => break end,
            }
        };
        self.wait_for_backups().await;
        self.stop_link().await;
        // Nobody waits for the process to exit: what is left is told to try again, and the
        // connections' senders drop.
        inbox.close();
        let rest: Vec<Item> = std::mem::take(&mut self.carry)
            .into_iter()
            .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
            .collect();
        let mut unapplied = Vec::new();
        for item in rest {
            match item {
                Item::Input(Input::Client { conn, req, .. }) => {
                    reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
                }
                // A hook the core never applied is kept for the next start.
                Item::Hook { reply, line, .. } => {
                    unapplied.extend(line);
                    if let Some((conn, req)) = reply {
                        reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
                    }
                }
                _ => {}
            }
        }
        spool_hooks(self.spool.as_deref(), unapplied.iter().map(|l| &**l));
        lock(&self.conns).clear();
        // The alarm would otherwise stay armed into a closed inbox.
        let _ = self.sched.send(SchedCmd::Stop);
        match end {
            // Already said by `fatal`.
            Flow::Fatal => {}
            Flow::Stop(stop) => {
                self.finished_tx.send_replace(Some(Ok(())));
                let _ = stop.send(());
            }
            Flow::Go => {
                self.finished_tx.send_replace(Some(Ok(())));
            }
        }
    }

    /// Runs one drain: every item, then one commit, then what it held. A failed commit, a
    /// dead store or a panic is put right (see `recover`) before anything is released.
    pub(super) async fn drain(&mut self, mut work: VecDeque<Item>) -> Flow {
        let mut batch: Vec<Change> = Vec::new();
        let mut held: Vec<Held> = Vec::new();
        let mut panicked = false;
        // The hooks stepped here, with their lines: if the commit fails, they are applied
        // again after the reload rather than lost with it.
        let mut replay: Vec<(HookEnvelope, Box<HookLine>)> = Vec::new();
        while let Some(item) = work.pop_front() {
            // A `StartDrained` whose step panics is sent again after the reload.
            let mut start_drained = false;
            let stepped = match item {
                Item::Input(input) => {
                    start_drained = matches!(input, Input::StartDrained);
                    self.step(input, &mut batch, &mut held)
                }
                // While writes are refused the core would drop the event: keep it for the end
                // of the refusal instead, and tell a waiting hook there is nothing to say. A
                // live hook's line goes to the spool; a drained one is still in `.draining`,
                // which its drain keeps when it sees the count move.
                Item::Hook { reply, line, .. }
                    if self.spool.is_some() && self.state.writes_refused(self.clock.wall()) =>
                {
                    if let Some(line) = line {
                        spool_hooks(self.spool.as_deref(), std::iter::once(&*line));
                    }
                    self.stats.refused_hooks.fetch_add(1, Ordering::SeqCst);
                    if let Some(refused) = self.state.refuse_writes() {
                        let _ = self.sched.send(SchedCmd::Set(Wake::Spool, refused.until));
                    }
                    if let Some((conn, req)) = reply {
                        let done = Done {
                            text: String::new(),
                            data: serde_json::json!({ "context": null }),
                        };
                        reply_to(&self.conns, conn, req, Ok(done));
                    }
                    Stepped::Done
                }
                Item::Hook {
                    reply,
                    envelope,
                    line,
                } => {
                    let kept = line.map(|line| (envelope.clone(), line));
                    let stepped = self.step(Input::Hook { reply, envelope }, &mut batch, &mut held);
                    if matches!(stepped, Stepped::Done) {
                        replay.extend(kept);
                    }
                    stepped
                }
                Item::Zone(tz) => {
                    self.tz = tz;
                    self.step(Input::ZoneChanged, &mut batch, &mut held)
                }
                Item::Status { caller, reply } => {
                    let data = self.state.status(&caller, self.clock.wall());
                    held.push(Held::Status(reply, data, caller));
                    Stepped::Done
                }
                Item::Barrier(reply) => {
                    held.push(Held::Barrier(reply));
                    Stepped::Done
                }
                Item::Committed(reply) => {
                    held.push(Held::Committed(reply));
                    Stepped::Done
                }
                Item::Crash => panic!("a test crashed the core task"),
                Item::Stop(reply) => {
                    held.push(Held::Stop(reply));
                    Stepped::Done
                }
            };
            match stepped {
                Stepped::Done => {}
                Stepped::Panicked { loading: true } => {
                    // Nothing was committed: what the drain held is not true. What follows is
                    // left for the end of the loop, which tells its callers to try again.
                    fail_held(&mut held, BUG_LOADING.to_string());
                    while let Some(rest) = work.pop_back() {
                        self.carry.push_front(rest);
                    }
                    return self.fatal(BUG_LOADING.to_string(), held);
                }
                Stepped::Panicked { loading: false } => {
                    // The drain stops here: what is before it is committed below, what is
                    // after it goes first into the next drain, in order.
                    panicked = true;
                    while let Some(rest) = work.pop_back() {
                        self.carry.push_front(rest);
                    }
                    // Sent once per start, and reconcile waits for it (R2): it goes again,
                    // first, after the reload.
                    if start_drained {
                        self.start_drained_again = true;
                        self.carry.push_front(Item::Input(Input::StartDrained));
                    }
                    break;
                }
            }
        }
        let mut failed = None;
        if !batch.is_empty() {
            self.stats.commits.fetch_add(1, Ordering::SeqCst);
            match self.store().commit(batch).await {
                Ok(()) => self.failures_in_row = 0,
                Err(why) => failed = Some(why),
            }
        }
        if failed.is_some() {
            // Nothing retries a hook as a client retries an op: its event goes first into the
            // next drain, after the reload has forgotten the keys this one remembered (or to
            // the spool, with the rest of the carry, if the core stops). A waiting hook was
            // answered with the drain's failure, so it isn't answered again.
            for (envelope, line) in replay.into_iter().rev() {
                self.carry.push_front(Item::Hook {
                    reply: None,
                    envelope,
                    line: Some(line),
                });
            }
        }
        if (failed.is_some() || panicked)
            && let Err(line) = self.recover(failed, panicked, &mut held).await
        {
            return self.fatal(line, held);
        }
        for day in std::mem::take(&mut self.due_backups) {
            self.start_backup(day);
        }
        match self.release(held).await {
            Some(stop) => Flow::Stop(stop),
            None => Flow::Go,
        }
    }

    /// One input through `handle`, inside `catch_unwind`; its effects go to the batch, the
    /// held list, or run now. A panicking input's partial changes die with its unwound call:
    /// `handle` returns effects only when it finishes.
    pub(super) fn step(
        &mut self,
        input: Input,
        batch: &mut Vec<Change>,
        held: &mut Vec<Held>,
    ) -> Stepped {
        let loading = match input {
            Input::Startup(_) | Input::Reload { .. } => true,
            Input::StartDrained => self.start_drained_again,
            _ => false,
        };
        let target = match &input {
            Input::Client { conn, req, .. } => Some((*conn, *req)),
            // A hook that waits for its answer gets one even when it panics.
            Input::Hook { reply, .. } => *reply,
            _ => None,
        };
        let now = Now::at(self.clock.wall(), self.clock.mono(), self.tz.clone());
        self.stats.inputs.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        lock(&self.stats.stepped).push(kind(&input));
        let panic_next = &self.panic_next;
        let state = &mut self.state;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if panic_next
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                panic!("a test asked for a panic");
            }
            handle(state, input, now)
        }));
        let effects = match outcome {
            Ok(effects) => effects,
            Err(_) => {
                // The payload stays out of the log: it may carry contents.
                tracing::error!("handle panicked");
                self.stats.panics.fetch_add(1, Ordering::SeqCst);
                if let Some((conn, req)) = target {
                    held.push(Held::Reply {
                        conn,
                        req,
                        result: Err(OpError(BUG_REPLY.into())),
                    });
                }
                return Stepped::Panicked { loading };
            }
        };
        debug_assert!(check_order(&effects).is_ok(), "effects out of order");
        // The request whose file couldn't be written: what the op says after the write isn't
        // true, so its publish and reply are skipped up to that reply.
        let mut unsaved: Option<(ConnId, ReqId)> = None;
        for effect in effects {
            if effect.runs_at_once() {
                self.run_now(effect);
                continue;
            }
            match effect {
                // Run here, before the effects after it.
                Effect::WriteFile { path, text, req } => {
                    if let Err(e) = write_file(&path, &text) {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let line = format!("couldn't save {name}: {e}");
                        tracing::warn!("{line}");
                        // The core already holds the unsaved layout: read the file again, or
                        // the next publish of any topic would show it.
                        if let Some(p) = moa_core::layout::reread(&mut self.state) {
                            held.push(Held::Publish(p));
                        }
                        if let Some((conn, req)) = req {
                            held.push(Held::Reply {
                                conn,
                                req,
                                result: Err(OpError(line)),
                            });
                            unsaved = Some((conn, req));
                        }
                    }
                }
                Effect::Publish(_) if unsaved.is_some() => {}
                Effect::Reply { conn, req, .. } if unsaved == Some((conn, req)) => unsaved = None,
                Effect::Persist(changes) => batch.extend(changes),
                Effect::Backup { day } => self.due_backups.push(day),
                Effect::Publish(p) => held.push(Held::Publish(p)),
                Effect::Reply { conn, req, result } => held.push(Held::Reply { conn, req, result }),
                // A view of saved state waits for the commit, as replies do; so does an
                // `open`'s new window, which starts an agent for a session the commit saves.
                // The link's plumbing ran above.
                Effect::Tmux { conn, cmds, token } => held.push(Held::Tmux { conn, cmds, token }),
                // Handled above.
                Effect::SetTimer { .. }
                | Effect::CancelTimer(_)
                | Effect::Log(_)
                | Effect::TmuxConnect { .. } => {}
            }
        }
        Stepped::Done
    }

    pub(super) fn run_now(&self, effect: Effect) {
        match effect {
            Effect::SetTimer { key, at } => {
                let _ = self.sched.send(SchedCmd::Set(Wake::Core(key), at));
            }
            Effect::CancelTimer(key) => {
                let _ = self.sched.send(SchedCmd::Cancel(Wake::Core(key)));
            }
            Effect::Log(LogLine { level, message }) => match level {
                LogLevel::Debug => tracing::debug!("{message}"),
                LogLevel::Info => tracing::info!("{message}"),
                LogLevel::Warn => tracing::warn!("{message}"),
                LogLevel::Error => tracing::error!("{message}"),
            },
            // The answers the core gives itself go through its inbox, never inside the
            // `handle` that asked.
            Effect::TmuxConnect { tmux } => match &self.link {
                Some(link) => {
                    if link.send(TmuxCmd::Connect { tmux }).is_err() {
                        // The link has ended: nobody else will answer the waiters.
                        tracing::warn!("the terminal link has ended");
                        self.tell_self(TmuxIn::Down(Down::GaveUp));
                    }
                }
                None => self.tell_self(TmuxIn::Down(Down::Disabled)),
            },
            Effect::Tmux {
                conn,
                cmds,
                token: Some(token),
            } => self.to_link(conn, cmds, Some(token)),
            _ => {}
        }
    }

    /// What the link would say, said by the core to itself.
    fn tell_self(&self, input: TmuxIn) {
        if let Some(inbox) = self.inbox.upgrade() {
            let _ = inbox.send(Item::Input(Input::Tmux(input)));
        }
    }

    /// Commands for the server, to the link if there is one.
    fn to_link(&self, conn: u64, cmds: Vec<Cmd>, token: Option<TmuxToken>) {
        match &self.link {
            Some(link) => {
                if link.send(TmuxCmd::Send { conn, cmds, token }).is_err() {
                    tracing::debug!("the terminal link has ended: dropped");
                }
            }
            // The commands stay out of the log.
            None => tracing::debug!(conn, cmds = cmds.len(), ?token, "no terminal link: dropped"),
        }
    }

    /// Asks the link to take its hooks down and close, and waits for it a short while.
    async fn stop_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        let (done, stopped) = oneshot::channel();
        if link.send(TmuxCmd::Stop(done)).is_ok()
            && tokio::time::timeout(LINK_STOP_WAIT, stopped).await.is_err()
        {
            tracing::warn!(
                "the terminal link didn't stop within {} s",
                LINK_STOP_WAIT.as_secs()
            );
        }
    }

    /// The backup runs on its own task so the core never waits for the copy. The log line
    /// has counts only.
    pub(super) fn start_backup(&self, day: Date) {
        let store = self.store();
        self.backups.spawn(async move {
            match store.backup(day).await {
                Ok(done) => tracing::info!(
                    written = done.written.is_some(),
                    pruned = done.pruned.removed.len(),
                    prune_failed = done.pruned.failed.len(),
                    "daily backup done"
                ),
                Err(e) => tracing::warn!("daily backup failed: {e}"),
            }
        });
    }

    /// Waits for the backups in flight now, not for ones started later.
    pub(super) async fn wait_for_backups(&self) {
        self.backups.close();
        self.backups.wait().await;
        self.backups.reopen();
    }

    pub(super) async fn release(&mut self, held: Vec<Held>) -> Option<oneshot::Sender<()>> {
        let mut stop = None;
        for h in held {
            match h {
                Held::Publish(Publish::Model(m)) => {
                    self.model_tx.send_replace(m);
                }
                Held::Publish(Publish::Event(e)) => {
                    // No subscriber is fine.
                    let _ = self.events.send(e);
                }
                Held::Reply { conn, req, result } => reply_to(&self.conns, conn, req, result),
                Held::Status(reply, data, _) => {
                    let _ = reply.send(data);
                }
                Held::Tmux { conn, cmds, token } => {
                    if token.is_none() {
                        self.stats.views_released.fetch_add(1, Ordering::SeqCst);
                    }
                    self.to_link(conn, cmds, token);
                }
                Held::Barrier(reply) => {
                    self.store().sync().await;
                    sync_scheduler(&self.sched).await;
                    self.wait_for_backups().await;
                    let _ = reply.send(());
                }
                Held::Committed(reply) => {
                    let _ = reply.send(());
                }
                Held::Stop(reply) => stop = Some(reply),
            }
        }
        stop
    }
}

/// However the core task ends, `finished()` says so and the connections close, so nobody
/// waits on a core that is gone. After a normal end both are already done.
impl Drop for Core {
    fn drop(&mut self) {
        if self.finished_tx.borrow().is_none() {
            tracing::error!("the core task ended unexpectedly");
            self.finished_tx
                .send_replace(Some(Err(UNEXPECTED.to_string())));
        }
        lock(&self.conns).clear();
    }
}

/// Writes `text` to `path` through a temporary file in the same folder and a rename, making
/// the folder when it's missing. A `path` that is a link is written through it, at its
/// target, which the write makes when it's missing (its folder isn't made): the rename would
/// otherwise put a plain file in the link's place.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let target = through_links(path)?;
    if target == path
        && let Some(dir) = path.parent()
    {
        std::fs::create_dir_all(dir)?;
    }
    moa_platform::fs::write_if_changed(&target, text).map(|_| ())
}

/// The most links followed from one path.
const MAX_LINKS: usize = 8;

/// The file at the end of `path`'s links (`path` itself when it isn't one), which may not
/// exist yet. A relative link is read from its own folder.
fn through_links(path: &Path) -> io::Result<PathBuf> {
    let mut at = path.to_path_buf();
    for _ in 0..=MAX_LINKS {
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                let to = std::fs::read_link(&at)?;
                // An absolute `to` replaces the folder in `join`.
                at = at.parent().map_or_else(|| to.clone(), |dir| dir.join(&to));
            }
            Ok(_) => return Ok(at),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(at),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("its links go round in a circle"))
}

/// The carried-over items, then what is already waiting, up to [`DRAIN_CAP`]. A `stop` ends
/// the drain: whatever follows it stays where it is.
fn take_drain(
    carry: &mut VecDeque<Item>,
    inbox: &mut mpsc::UnboundedReceiver<Item>,
) -> VecDeque<Item> {
    let mut work = VecDeque::new();
    while work.len() < DRAIN_CAP {
        let Some(item) = carry.pop_front().or_else(|| inbox.try_recv().ok()) else {
            break;
        };
        let last = matches!(item, Item::Stop(_));
        work.push_back(item);
        if last {
            break;
        }
    }
    work
}

/// An input's kind, for the tests' record of what was stepped.
#[cfg(test)]
fn kind(input: &Input) -> &'static str {
    match input {
        Input::Startup(_) => "Startup",
        Input::Reload { .. } => "Reload",
        Input::Client { .. } => "Client",
        Input::ClientGone(_) => "ClientGone",
        Input::Timer(_) => "Timer",
        Input::ZoneChanged => "ZoneChanged",
        Input::ActorFailed { .. } => "ActorFailed",
        Input::Tmux(_) => "Tmux",
        Input::Hook { .. } => "Hook",
        Input::StartDrained => "StartDrained",
    }
}
