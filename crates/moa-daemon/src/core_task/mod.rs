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
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Input, Publish};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Model};
use moa_core::state::{CallerInfo, Deps, State, StatusData};
use moa_core::tmux::cmd::Cmd;
use moa_store::actor::{Fault, StoreHandle};
use moa_store::{Store, StoreError};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::scheduler::{self, SchedCmd};
use crate::tmux::{self, TmuxCmd, TmuxLink};

mod drain;
mod recover;

use drain::{Core, UNEXPECTED};

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
    /// The terminal link. Without one, every connect is answered `Down(Disabled)` and
    /// commands for the server are dropped.
    pub tmux: Option<TmuxLink>,
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
    /// Views of saved state for the terminal server that were released after a commit.
    pub views_released: AtomicU64,
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
    Committed(oneshot::Sender<()>),
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
    /// A view of saved state for the terminal server.
    Tmux {
        conn: u64,
        cmds: Vec<Cmd>,
    },
    Barrier(oneshot::Sender<()>),
    Committed(oneshot::Sender<()>),
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
        Self::start_with_faults(opts, store, Vec::new()).await
    }

    /// Like `start_with_store`, with `faults` queued on the store thread before `Startup`.
    #[doc(hidden)]
    pub async fn start_with_faults(
        opts: CoreOptions,
        store: Store,
        faults: Vec<Fault>,
    ) -> Result<CoreHandle, DaemonError> {
        Self::start_inner(opts, store, faults, None).await
    }

    /// `link`, when given, stands in for the terminal link that `opts.tmux` would start
    /// (tests that watch what the core sends it).
    pub(crate) async fn start_inner(
        mut opts: CoreOptions,
        store: Store,
        faults: Vec<Fault>,
        link: Option<mpsc::UnboundedSender<TmuxCmd>>,
    ) -> Result<CoreHandle, DaemonError> {
        let store = StoreHandle::spawn(store, opts.backups.clone());
        for fault in faults {
            store.fault(fault);
        }
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

        // Weak for the same reason: the core owns the link.
        let link = link.or_else(|| {
            let (heard, failed) = (inbox.downgrade(), inbox.downgrade());
            opts.tmux.take().map(|l| {
                tmux::start(
                    l,
                    move |input| {
                        if let Some(inbox) = heard.upgrade() {
                            let _ = inbox.send(Item::Input(Input::Tmux(input)));
                        }
                    },
                    // The panic's message stays here: it may carry contents.
                    move |actor, _message| {
                        if let Some(inbox) = failed.upgrade() {
                            let _ = inbox.send(Item::Input(Input::ActorFailed {
                                actor,
                                message: String::new(),
                            }));
                        }
                    },
                )
            })
        });

        let conns = Conns::default();
        let core = Core {
            inbox: inbox.downgrade(),
            link,
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
            due_backups: Vec::new(),
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
        // Startup is the first item, so this answers after its commit, not after its daily
        // backup (the store thread is busy copying; `barrier()` and `stop()` wait for it). If
        // Startup brought the core down, the answer is dropped and returns, and the line is here.
        handle.committed().await;
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

    /// What the terminal link reports. The link has its own way in; tests play it here.
    #[doc(hidden)]
    pub fn tmux_input(&self, input: moa_core::effect::TmuxIn) {
        let _ = self.inbox.send(Item::Input(Input::Tmux(input)));
    }

    pub fn zone_changed(&self, tz: TimeZone) {
        let _ = self.inbox.send(Item::Zone(tz));
    }

    /// An actor outside the core panicked and was restarted: the core publishes an error
    /// event (and logs it). The raw panic message stays with the caller: it may carry
    /// contents.
    pub fn actor_failed(&self, actor: &'static str) {
        let sent = self.inbox.send(Item::Input(Input::ActorFailed {
            actor,
            message: String::new(),
        }));
        if sent.is_err() {
            // The core is gone and can't log it.
            tracing::error!("actor {actor} failed and was restarted");
        }
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

    /// Answers once everything before it has been committed and released, without waiting
    /// for the store's queue or the backups.
    async fn committed(&self) {
        sync_scheduler(&self.sched).await;
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Item::Committed(reply)).is_ok() {
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
