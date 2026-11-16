//! The core task: it runs inputs through `handle` in drains and commits each drain as one
//! batch, with at most one batch in flight (group commit).
//!
//! A drain is the carried-over items, then whatever else is already waiting, up to
//! [`DRAIN_CAP`]. Every change the drain's inputs make goes into one `commit`; nothing the
//! drain would say (a model, an event, a reply) is said until that commit is done. Inputs
//! that arrive meanwhile wait in the inbox and make up the next drain.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, LogLevel, LogLine, Publish, check_order};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Model};
use moa_core::state::{CallerInfo, Deps, State, StatusData};
use moa_core::{Now, handle};
use moa_store::actor::{Fault, StoreHandle};
use moa_store::{Store, StoreError};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::scheduler::{self, SchedCmd};

/// The most items one drain takes.
pub const DRAIN_CAP: usize = 64;

const STOPPING: &str = "moa is stopping; try again";

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
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
}

/// What a drain says after its commit, in order.
enum Held {
    Publish(Publish),
    Reply {
        conn: ConnId,
        req: ReqId,
        result: Result<Done, OpError>,
    },
    Status(oneshot::Sender<StatusData>, StatusData),
    Barrier(oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct CoreHandle {
    inbox: mpsc::UnboundedSender<Item>,
    sched: mpsc::UnboundedSender<SchedCmd>,
    conns: Conns,
    next_conn: Arc<AtomicU64>,
    store: StoreHandle,
    model: watch::Receiver<Arc<Model>>,
    events: broadcast::Sender<CoreEvent>,
    generation: watch::Receiver<u64>,
    stats: Arc<Stats>,
}

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
        let stats = Arc::new(Stats::default());
        let (inbox, inbox_rx) = mpsc::unbounded_channel();
        let state = State::new(opts.brain, opts.config).with_deps(opts.deps);
        let (model_tx, model) = watch::channel(state.model());
        let (events, _) = broadcast::channel(opts.events_capacity.max(1));
        let (generation_tx, generation) = watch::channel(0);

        let fire_inbox = inbox.clone();
        let failed_inbox = inbox.clone();
        let sched = scheduler::start(
            opts.clock.clone(),
            opts.alarm,
            stats.clone(),
            move |key| {
                let _ = fire_inbox.send(Item::Input(Input::Timer(key)));
            },
            // The raw message stays here: it may carry contents (2a's no-contents rule).
            move |actor, _message| {
                let _ = failed_inbox.send(Item::Input(Input::ActorFailed {
                    actor,
                    message: String::new(),
                }));
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
        };
        // Startup is the first item, so this barrier answers after its commit.
        handle.barrier().await;
        Ok(handle)
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

    /// Answered in drain order, after the drain's commit. When the core has stopped, an
    /// empty answer.
    pub async fn status(&self, caller: CallerInfo) -> StatusData {
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Item::Status { caller, reply }).is_err() {
            return StatusData::default();
        }
        answer.await.unwrap_or_default()
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

    /// Tests only.
    #[doc(hidden)]
    pub fn store_fault(&self, f: Fault) {
        self.store.fault(f);
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
    store: StoreHandle,
    clock: Arc<dyn Clock>,
    tz: TimeZone,
    conns: Conns,
    stats: Arc<Stats>,
    model_tx: watch::Sender<Arc<Model>>,
    events: broadcast::Sender<CoreEvent>,
    // Task 5 bumps it on every reload.
    #[allow(dead_code)]
    generation_tx: watch::Sender<u64>,
    sched: mpsc::UnboundedSender<SchedCmd>,
    backups: TaskTracker,
}

impl Core {
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Item>) {
        // Items a drain left for the next one (Task 5: after a panic or a failed commit).
        let mut carry: VecDeque<Item> = VecDeque::new();
        let stop = loop {
            if carry.is_empty() {
                match inbox.recv().await {
                    Some(item) => carry.push_back(item),
                    None => break None,
                }
            }
            let work = take_drain(&mut carry, &mut inbox);
            self.stats.drains.fetch_add(1, Ordering::SeqCst);
            if let Some(stop) = self.drain(work).await {
                break Some(stop);
            }
        };
        self.wait_for_backups().await;
        // Nobody waits for the process to exit: what is left is told to try again, and the
        // connections' senders drop.
        inbox.close();
        let rest: Vec<Item> = carry
            .into_iter()
            .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
            .collect();
        for item in rest {
            if let Item::Input(Input::Client { conn, req, .. }) = item {
                reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
            }
        }
        lock(&self.conns).clear();
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
    }

    /// Runs one drain: every item, then one commit, then what it held. `Some` when the drain
    /// held a `stop`; the sender answers once the loop has ended.
    async fn drain(&mut self, mut work: VecDeque<Item>) -> Option<oneshot::Sender<()>> {
        let mut batch: Vec<Change> = Vec::new();
        let mut held: Vec<Held> = Vec::new();
        while let Some(item) = work.pop_front() {
            match item {
                Item::Input(input) => self.step(input, &mut batch, &mut held),
                Item::Zone(tz) => {
                    self.tz = tz;
                    self.step(Input::ZoneChanged, &mut batch, &mut held);
                }
                Item::Status { caller, reply } => {
                    self.state.set_now(self.clock.wall());
                    let data = self.state.status(&caller);
                    held.push(Held::Status(reply, data));
                }
                Item::Barrier(reply) => held.push(Held::Barrier(reply)),
                Item::Stop(reply) => held.push(Held::Stop(reply)),
            }
        }
        if !batch.is_empty() {
            self.stats.commits.fetch_add(1, Ordering::SeqCst);
            if let Err(why) = self.store.commit(batch).await {
                self.commit_failed(&why, &mut held);
            }
        }
        self.release(held).await
    }

    /// One input through `handle`; its effects go to the batch, the held list, or run now.
    fn step(&mut self, input: Input, batch: &mut Vec<Change>, held: &mut Vec<Held>) {
        let now = Now::at(self.clock.wall(), self.clock.mono(), self.tz.clone());
        let effects = handle(&mut self.state, input, now);
        debug_assert!(check_order(&effects).is_ok(), "effects out of order");
        self.stats.inputs.fetch_add(1, Ordering::SeqCst);
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
        let store = self.store.clone();
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

    /// Task 5 replaces this with a reload from the store. Until then the drain's replies say
    /// it was not saved and nothing it held is published.
    fn commit_failed(&self, why: &moa_store::actor::CommitError, held: &mut Vec<Held>) {
        self.stats.commit_failures.fetch_add(1, Ordering::SeqCst);
        tracing::error!("a commit failed: {why}");
        held.retain(|h| !matches!(h, Held::Publish(_)));
        for h in held.iter_mut() {
            if let Held::Reply { result, .. } = h {
                *result = Err(OpError("moa could not save that; try again".into()));
            }
        }
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
                Held::Status(reply, data) => {
                    let _ = reply.send(data);
                }
                Held::Barrier(reply) => {
                    self.store.sync().await;
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
