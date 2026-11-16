//! The core task: it runs inputs through `handle` in drains and commits each drain as one
//! batch, with at most one batch in flight (group commit).
//!
//! A drain is the carried-over items, then whatever else is already waiting, up to
//! [`DRAIN_CAP`]. Every change the drain's inputs make goes into one `commit`; nothing the
//! drain would say (a model, an event, a reply) is said until that commit is done. Inputs
//! that arrive meanwhile wait in the inbox and make up the next drain.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, LogLevel, LogLine, Publish, RefuseWrites, check_order};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Model};
use moa_core::state::{CallerInfo, Deps, State, StatusData};
use moa_core::{Now, UtcMs, handle};
use moa_store::actor::{CommitError, Fault, StoreHandle};
use moa_store::{Store, StoreError};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::scheduler::{self, SchedCmd};

/// The most items one drain takes.
pub const DRAIN_CAP: usize = 64;

pub(crate) const STOPPING: &str = "moa is stopping; try again";

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The core can't go on and has stopped; the daemon exits 1 with this line.
    #[error("{0}")]
    Fatal(String),
    #[error("the {0} brain's moa is already running")]
    AlreadyRunning(BrainName),
    #[error("there's no brain named {} in {}", .0, .1.display())]
    NoSuchBrain(BrainName, PathBuf),
    #[error(transparent)]
    Paths(#[from] moa_platform::PathsError),
    /// A problem with `moa.toml`; the line starts with `moa.toml`.
    #[error("{0}")]
    Config(#[from] moa_core::config::ConfigError),
}

pub struct CoreOptions {
    pub brain: BrainName,
    pub config: Config,
    pub deps: Deps,
    pub tz: TimeZone,
    pub clock: Arc<dyn Clock>,
    pub alarm: Box<dyn moa_platform::WallTimer>,
    pub db: PathBuf,
    pub backups: PathBuf,
    /// How many events a slow subscriber may fall behind by (256).
    pub events_capacity: usize,
}

/// Counters for tests and the idle check. Startup counts as an input, a drain and (when it
/// writes) a commit; barriers, stops and status calls are not inputs.
#[derive(Default, Debug)]
pub struct Stats {
    pub inputs: AtomicU64,
    pub drains: AtomicU64,
    pub commits: AtomicU64,
    pub timer_fires: AtomicU64,
    pub commit_failures: AtomicU64,
    pub reloads: AtomicU64,
}

/// One reply on a connection.
pub type ConnReply = (ReqId, Result<Done, OpError>);
type ConnTx = mpsc::UnboundedSender<ConnReply>;
type Conns = Arc<Mutex<HashMap<ConnId, ConnTx>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What waits in the core's inbox. One `Input` per request, so boxing buys nothing.
#[allow(clippy::large_enum_variant)]
enum Item {
    Input(Input),
    Zone(TimeZone),
    Status {
        caller: CallerInfo,
        reply: oneshot::Sender<StatusData>,
    },
    Barrier(oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
    /// Tests only: panics outside `catch_unwind`.
    Crash,
}

/// What a drain says after its commit, in order.
enum Held {
    Publish(Publish),
    Reply {
        conn: ConnId,
        req: ReqId,
        result: Result<Done, OpError>,
    },
    /// The answer so far, and the caller, so a reload can compute it again.
    Status(oneshot::Sender<StatusData>, StatusData, CallerInfo),
    Barrier(oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct CoreHandle {
    inbox: mpsc::UnboundedSender<Item>,
    sched: mpsc::UnboundedSender<SchedCmd>,
    conns: Conns,
    next_conn: Arc<AtomicU64>,
    /// The store's current handle: the core swaps it when the thread dies.
    store: SharedStore,
    model: watch::Receiver<Arc<Model>>,
    events: broadcast::Sender<CoreEvent>,
    generation: watch::Receiver<u64>,
    stats: Arc<Stats>,
    finished: watch::Receiver<Option<Result<(), String>>>,
    /// Closes when the core task is gone, cleanup included.
    ended: watch::Receiver<()>,
    panic_next: Arc<AtomicU32>,
}

type SharedStore = Arc<Mutex<StoreHandle>>;

impl CoreHandle {
    /// Opens the store, loads it and runs `Startup`. Returns once Startup's batch has been
    /// committed, so a fault a test queues next always meets its own commit.
    pub async fn start(opts: CoreOptions) -> Result<CoreHandle, DaemonError> {
        // Opening can migrate and VACUUM: keep it off the runtime's threads.
        let (db, backups) = (opts.db.clone(), opts.backups.clone());
        let store = tokio::task::spawn_blocking(move || Store::open(&db, &backups))
            .await
            .map_err(io::Error::other)??;
        Self::start_with_store(opts, store).await
    }

    #[doc(hidden)]
    pub async fn start_with_store(
        opts: CoreOptions,
        store: Store,
    ) -> Result<CoreHandle, DaemonError> {
        let store = StoreHandle::spawn(store, opts.backups.clone());
        let snapshot = store
            .snapshot()
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        let store: SharedStore = Arc::new(Mutex::new(store));
        let stats = Arc::new(Stats::default());
        let (finished_tx, finished) = watch::channel(None);
        let (ended_tx, ended) = watch::channel(());
        let panic_next = Arc::new(AtomicU32::new(0));
        let (inbox, inbox_rx) = mpsc::unbounded_channel();
        let state = State::new(opts.brain, opts.config).with_deps(opts.deps);
        let (model_tx, model) = watch::channel(state.model());
        let (events, _) = broadcast::channel(opts.events_capacity.max(1));
        let (generation_tx, generation) = watch::channel(0);

        // Weak: the core owns the scheduler, so a strong sender here would be a cycle that
        // keeps the inbox open after every handle has gone.
        let fire_inbox = inbox.downgrade();
        let failed_inbox = inbox.downgrade();
        let sched = scheduler::start(
            opts.clock.clone(),
            opts.alarm,
            stats.clone(),
            move |key| {
                if let Some(inbox) = fire_inbox.upgrade() {
                    let _ = inbox.send(Item::Input(Input::Timer(key)));
                }
            },
            // The raw message stays here: it may carry contents (2a's no-contents rule).
            move |actor, _message| {
                if let Some(inbox) = failed_inbox.upgrade() {
                    let _ = inbox.send(Item::Input(Input::ActorFailed {
                        actor,
                        message: String::new(),
                    }));
                }
            },
        );

        let conns = Conns::default();
        let core = Core {
            state,
            store: store.clone(),
            clock: opts.clock,
            tz: opts.tz,
            conns: conns.clone(),
            stats: stats.clone(),
            model_tx,
            events: events.clone(),
            generation_tx,
            sched: sched.clone(),
            backups: TaskTracker::new(),
            db: opts.db,
            backups_dir: opts.backups,
            failures_in_row: 0,
            carry: VecDeque::new(),
            panic_next: panic_next.clone(),
            finished_tx,
            _ended: ended_tx,
        };
        let _ = inbox.send(Item::Input(Input::Startup(Box::new(snapshot))));
        tokio::spawn(core.run(inbox_rx));

        let handle = CoreHandle {
            inbox,
            sched,
            conns,
            next_conn: Arc::new(AtomicU64::new(0)),
            store,
            model,
            events,
            generation,
            stats,
            finished,
            ended,
            panic_next,
        };
        // Startup is the first item, so this barrier answers after its commit. If Startup
        // brought the core down, the barrier is dropped and returns, and the line is here.
        handle.barrier().await;
        let ended = handle.finished.borrow().clone();
        if let Some(Err(line)) = ended {
            return Err(DaemonError::Fatal(line));
        }
        Ok(handle)
    }

    /// Resolves when the core task has ended: `Ok` after `stop`, `Err(Fatal)` when it can't
    /// go on (the store can't be reopened, or a bug while loading). The daemon then stops
    /// accepting and exits.
    pub async fn finished(&self) -> Result<(), DaemonError> {
        let mut rx = self.finished.clone();
        let ended = match rx.wait_for(Option::is_some).await {
            Ok(v) => v.clone(),
            Err(_) => Some(Err(UNEXPECTED.to_string())),
        };
        match ended {
            Some(Err(line)) => Err(DaemonError::Fatal(line)),
            _ => Ok(()),
        }
    }

    /// Resolves once the core task is gone: after its cleanup (backups waited for, queued
    /// callers told to try again). `finished()` can resolve with a fatal line before that.
    pub async fn ended(&self) {
        let mut rx = self.ended.clone();
        while rx.changed().await.is_ok() {}
    }

    /// A new connection and the channel its replies arrive on.
    pub fn open_conn(&self) -> (ConnId, mpsc::UnboundedReceiver<ConnReply>) {
        let id = ConnId(self.next_conn.fetch_add(1, Ordering::SeqCst) + 1);
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.conns).insert(id, tx);
        (id, rx)
    }

    pub fn close_conn(&self, conn: ConnId) {
        lock(&self.conns).remove(&conn);
    }

    pub fn send(&self, conn: ConnId, req: ReqId, caller: CallerInfo, op: Op) {
        let input = Input::Client {
            conn,
            req,
            caller,
            op,
        };
        if self.inbox.send(Item::Input(input)).is_err() {
            reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
        }
    }

    /// Answered in drain order, after the drain's commit. `None` when the core has stopped.
    pub async fn status(&self, caller: CallerInfo) -> Option<StatusData> {
        let (reply, answer) = oneshot::channel();
        self.inbox.send(Item::Status { caller, reply }).ok()?;
        answer.await.ok()
    }

    /// Tests only: the core task panics where nothing catches it.
    #[doc(hidden)]
    pub fn crash_core(&self) {
        let _ = self.inbox.send(Item::Crash);
    }

    pub fn zone_changed(&self, tz: TimeZone) {
        let _ = self.inbox.send(Item::Zone(tz));
    }

    pub fn model(&self) -> watch::Receiver<Arc<Model>> {
        self.model.clone()
    }

    pub fn events(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    /// Bumps on every reload.
    pub fn generation(&self) -> watch::Receiver<u64> {
        self.generation.clone()
    }

    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }

    /// Answers after everything before it has settled: its drain's commit, the store's queue,
    /// the scheduler's, and the backups in flight. Meant for tests.
    pub async fn barrier(&self) {
        // Commands already sent to the scheduler are applied first. It fires nothing here.
        sync_scheduler(&self.sched).await;
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Item::Barrier(reply)).is_ok() {
            let _ = answer.await;
        }
    }

    /// Ends the loop after the drain it joins has committed and released, and after the
    /// backups in flight have finished. Whatever is still waiting is told to try again.
    pub async fn stop(&self) {
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Item::Stop(reply)).is_ok() {
            let _ = answer.await;
        }
    }

    /// Tests only: the next input panics inside the core's `catch_unwind`, before `handle`.
    #[doc(hidden)]
    pub fn panic_next_handle(&self) {
        self.panic_next_handles(1);
    }

    /// Tests only: the next `n` calls panic (2 hits an input, then the reload after it).
    #[doc(hidden)]
    pub fn panic_next_handles(&self, n: u32) {
        self.panic_next.store(n, Ordering::SeqCst);
    }

    /// Tests only.
    #[doc(hidden)]
    pub fn store_fault(&self, f: Fault) {
        let store = lock(&self.store).clone();
        store.fault(f);
    }
}

async fn sync_scheduler(sched: &mpsc::UnboundedSender<SchedCmd>) {
    let (reply, answer) = oneshot::channel();
    if sched.send(SchedCmd::Sync(reply)).is_ok() {
        let _ = answer.await;
    }
}

fn reply_to(conns: &Conns, conn: ConnId, req: ReqId, result: Result<Done, OpError>) {
    // A connection that has gone is not an error.
    if let Some(tx) = lock(conns).get(&conn) {
        let _ = tx.send((req, result));
    }
}

struct Core {
    state: State,
    store: SharedStore,
    clock: Arc<dyn Clock>,
    tz: TimeZone,
    conns: Conns,
    stats: Arc<Stats>,
    model_tx: watch::Sender<Arc<Model>>,
    events: broadcast::Sender<CoreEvent>,
    generation_tx: watch::Sender<u64>,
    sched: mpsc::UnboundedSender<SchedCmd>,
    backups: TaskTracker,
    /// Where the store reopens from when its thread dies.
    db: PathBuf,
    backups_dir: PathBuf,
    /// Failed commits since the last good one; from the third, writes are refused for a while.
    failures_in_row: u32,
    /// Items a drain left for the next one: what followed a panic.
    carry: VecDeque<Item>,
    panic_next: Arc<AtomicU32>,
    finished_tx: watch::Sender<Option<Result<(), String>>>,
    /// Dropped with the core, which is what [`CoreHandle::ended`] waits for.
    _ended: watch::Sender<()>,
}

/// What a drain leaves the loop to do.
enum Flow {
    Go,
    /// The drain held a `stop`; the sender answers once the loop has ended.
    Stop(oneshot::Sender<()>),
    /// The core can't go on: the loop ends and `finished()` says why.
    Fatal,
}

/// How one input went through `handle`.
enum Stepped {
    Done,
    /// `handle` panicked. `loading` is true for `Startup` and `Reload`, which aren't retried.
    Panicked {
        loading: bool,
    },
}

const BUG_REPLY: &str = "moa hit a bug answering that; it reloaded";
const UNEXPECTED: &str = "the core stopped unexpectedly; see the log";
const BUG_LOADING: &str = "moa hit a bug loading the brain; see the log";

impl Core {
    fn store(&self) -> StoreHandle {
        lock(&self.store).clone()
    }

    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Item>) {
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
        // Nobody waits for the process to exit: what is left is told to try again, and the
        // connections' senders drop.
        inbox.close();
        let rest: Vec<Item> = std::mem::take(&mut self.carry)
            .into_iter()
            .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
            .collect();
        for item in rest {
            if let Item::Input(Input::Client { conn, req, .. }) = item {
                reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
            }
        }
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
    async fn drain(&mut self, mut work: VecDeque<Item>) -> Flow {
        let mut batch: Vec<Change> = Vec::new();
        let mut held: Vec<Held> = Vec::new();
        let mut panicked = false;
        while let Some(item) = work.pop_front() {
            let stepped = match item {
                Item::Input(input) => self.step(input, &mut batch, &mut held),
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
        if (failed.is_some() || panicked)
            && let Err(line) = self.recover(failed, panicked, &mut held).await
        {
            return self.fatal(line, held);
        }
        match self.release(held).await {
            Some(stop) => Flow::Stop(stop),
            None => Flow::Go,
        }
    }

    /// Puts the core right after a failed commit, a dead store or a panic: tells the drain's
    /// callers, then reloads the in-memory state from disk. `Err` is the line the daemon
    /// stops with.
    async fn recover(
        &mut self,
        failed: Option<CommitError>,
        panicked: bool,
        held: &mut Vec<Held>,
    ) -> Result<(), String> {
        // Reloading lifts a refusal unless it is handed back.
        let mut refuse = self.state.refuse_writes().cloned();
        let mut restarted = false;
        match failed {
            Some(CommitError::Failed(why)) => {
                self.stats.commit_failures.fetch_add(1, Ordering::SeqCst);
                self.failures_in_row += 1;
                tracing::error!("a commit failed: {why}");
                fail_held(
                    held,
                    format!("moa couldn't save that ({why}); nothing was changed, try again"),
                );
                held.push(error_event(
                    "moa couldn't save a change; it reloaded from disk",
                ));
                refuse = self.backoff(&why);
            }
            Some(CommitError::Stopped) => {
                tracing::error!("the store thread stopped");
                fail_held(
                    held,
                    "moa couldn't save that: the store stopped".to_string(),
                );
                self.reopen_store().await?;
                restarted = true;
            }
            None => {}
        }
        if panicked {
            held.push(error_event("moa hit a bug and reloaded"));
        }
        self.reload(refuse, held, restarted).await?;
        // A status asked in the failed drain shows the reloaded state, not the failed one.
        let now = self.clock.wall();
        for h in held.iter_mut() {
            if let Held::Status(_, data, caller) = h {
                *data = self.state.status(caller, now);
            }
        }
        Ok(())
    }

    /// How long writes are refused after this failure, if they are: nothing for the first
    /// two in a row, then 1 s, 2 s, 4 s ... up to 30 s.
    fn backoff(&self, why: &str) -> Option<RefuseWrites> {
        let n = self.failures_in_row;
        if n < 3 {
            return None;
        }
        let secs = (1u64 << (n - 3).min(5)).min(30);
        let wait = i64::try_from(secs * 1000).unwrap_or(i64::MAX);
        Some(RefuseWrites {
            until: UtcMs(self.clock.wall().0.saturating_add(wait)),
            reason: why.to_string(),
        })
    }

    /// Loads the store's state back into the core and publishes the whole model. When the
    /// store can't even snapshot, its thread is restarted once. A reload doesn't count as a
    /// failure.
    async fn reload(
        &mut self,
        refuse_writes: Option<RefuseWrites>,
        held: &mut Vec<Held>,
        mut restarted: bool,
    ) -> Result<(), String> {
        let snapshot = match self.store().snapshot().await {
            Ok(s) => s,
            Err(_) => {
                self.reopen_store().await?;
                restarted = true;
                self.store()
                    .snapshot()
                    .await
                    .map_err(|e| cannot_open(&e.to_string()))?
            }
        };
        let input = Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes,
        };
        // A reload persists nothing.
        let mut scratch = Vec::new();
        if !matches!(self.step(input, &mut scratch, held), Stepped::Done) {
            return Err(BUG_LOADING.to_string());
        }
        self.generation_tx.send_modify(|g| *g += 1);
        self.stats.reloads.fetch_add(1, Ordering::SeqCst);
        if restarted {
            // The message stays out: it may carry contents.
            let input = Input::ActorFailed {
                actor: "the store",
                message: String::new(),
            };
            let _ = self.step(input, &mut scratch, held);
        }
        Ok(())
    }

    /// Opens the database again on a new store thread. `Err` is the line to stop with.
    async fn reopen_store(&mut self) -> Result<(), String> {
        let (db, backups) = (self.db.clone(), self.backups_dir.clone());
        let opened = tokio::task::spawn_blocking(move || Store::open(&db, &backups)).await;
        let store = match opened {
            Ok(Ok(store)) => store,
            Ok(Err(e)) => return Err(cannot_open(&e.to_string())),
            Err(e) => return Err(cannot_open(&e.to_string())),
        };
        *lock(&self.store) = StoreHandle::spawn(store, self.backups_dir.clone());
        Ok(())
    }

    /// The core is done for: say why once (event and log), answer what the drain held, and
    /// let the loop end. A held error becomes `line`; a held `Ok` stays: its commit already
    /// happened (`fail_held` has turned the replies of a drain that didn't save into errors).
    fn fatal(&mut self, line: String, held: Vec<Held>) -> Flow {
        tracing::error!("the core stops: {line}");
        // Before anything is dropped: a `start()` woken by a dropped barrier must find it.
        self.finished_tx.send_replace(Some(Err(line.clone())));
        let _ = self.events.send(CoreEvent::Error {
            message: line.clone(),
        });
        for h in held {
            if let Held::Reply { conn, req, result } = h {
                let result = result.map_err(|_| OpError(line.clone()));
                reply_to(&self.conns, conn, req, result);
            }
        }
        Flow::Fatal
    }

    /// One input through `handle`, inside `catch_unwind`; its effects go to the batch, the
    /// held list, or run now. A panicking input's partial changes die with its unwound call:
    /// `handle` returns effects only when it finishes.
    fn step(&mut self, input: Input, batch: &mut Vec<Change>, held: &mut Vec<Held>) -> Stepped {
        let loading = matches!(input, Input::Startup(_) | Input::Reload { .. });
        let target = match &input {
            Input::Client { conn, req, .. } => Some((*conn, *req)),
            _ => None,
        };
        let now = Now::at(self.clock.wall(), self.clock.mono(), self.tz.clone());
        self.stats.inputs.fetch_add(1, Ordering::SeqCst);
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
        for effect in effects {
            if effect.runs_at_once() {
                self.run_now(effect);
                continue;
            }
            match effect {
                Effect::Persist(changes) => batch.extend(changes),
                Effect::Backup { day } => self.start_backup(day),
                Effect::Publish(p) => held.push(Held::Publish(p)),
                Effect::Reply { conn, req, result } => held.push(Held::Reply { conn, req, result }),
                // Handled above.
                Effect::SetTimer { .. } | Effect::CancelTimer(_) | Effect::Log(_) => {}
            }
        }
        Stepped::Done
    }

    fn run_now(&self, effect: Effect) {
        match effect {
            Effect::SetTimer { key, at } => {
                let _ = self.sched.send(SchedCmd::Set(key, at));
            }
            Effect::CancelTimer(key) => {
                let _ = self.sched.send(SchedCmd::Cancel(key));
            }
            Effect::Log(LogLine { level, message }) => match level {
                LogLevel::Info => tracing::info!("{message}"),
                LogLevel::Warn => tracing::warn!("{message}"),
                LogLevel::Error => tracing::error!("{message}"),
            },
            _ => {}
        }
    }

    /// The backup runs on its own task so the core never waits for the copy. The log line
    /// has counts only.
    fn start_backup(&self, day: Date) {
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
    async fn wait_for_backups(&self) {
        self.backups.close();
        self.backups.wait().await;
        self.backups.reopen();
    }

    async fn release(&mut self, held: Vec<Held>) -> Option<oneshot::Sender<()>> {
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
                Held::Barrier(reply) => {
                    self.store().sync().await;
                    sync_scheduler(&self.sched).await;
                    self.wait_for_backups().await;
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

fn error_event(message: &str) -> Held {
    Held::Publish(Publish::Event(CoreEvent::Error {
        message: message.to_string(),
    }))
}

/// The drain's callers are told it wasn't saved, and nothing it would have said is said:
/// every held reply (a read may have seen the failed state) becomes `line`, and held
/// publishes are dropped.
fn fail_held(held: &mut Vec<Held>, line: String) {
    held.retain(|h| !matches!(h, Held::Publish(_)));
    for h in held.iter_mut() {
        if let Held::Reply { result, .. } = h {
            *result = Err(OpError(line.clone()));
        }
    }
}

fn cannot_open(why: &str) -> String {
    format!(
        "the store can't be opened: {}",
        why.lines().next().unwrap_or("")
    )
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
