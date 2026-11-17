//! The core task: it runs inputs through `handle` in drains and commits each drain as one
//! batch, with at most one batch in flight (group commit).
//!
//! A drain is the carried-over items, then whatever else is already waiting, up to
//! [`DRAIN_CAP`]. Every change the drain's inputs make goes into one `commit`; nothing the
//! drain would say (a model, an event, a reply) is said until that commit is done. Inputs
//! that arrive meanwhile wait in the inbox and make up the next drain.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jiff::tz::TimeZone;
use moa_agents::claude;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Input, Publish, TmuxToken};
use moa_core::hook::{HookEnvelope, HookLine};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Model};
use moa_core::state::{CallerInfo, Deps, State, StatusData};
use moa_core::tmux::cmd::Cmd;
use moa_platform::spool;
use moa_store::actor::{Fault, StoreHandle};
use moa_store::{Store, StoreError};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::scheduler::{self, SchedCmd, Wake};
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
    /// The brain's hook spool, drained at start and again when hooks may have been spooled
    /// while the daemon was up. `None` only in tests that opt out.
    pub spool: Option<PathBuf>,
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
    /// Inputs whose handling panicked.
    pub panics: AtomicU64,
    /// Views of saved state for the terminal server that were released after a commit.
    pub views_released: AtomicU64,
    /// Hook events kept from the core while writes were refused: spooled, or left in the
    /// spool's `.draining` file.
    pub refused_hooks: AtomicU64,
    /// Drains of the hook spool that have run, at start and after.
    pub spool_drains: AtomicU64,
}

/// One reply on a connection.
pub type ConnReply = (ReqId, Result<Done, OpError>);
type ConnTx = mpsc::UnboundedSender<ConnReply>;
type Conns = Arc<Mutex<HashMap<ConnId, ConnTx>>>;

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What waits in the core's inbox. One `Input` per request, so boxing buys nothing.
#[allow(clippy::large_enum_variant)]
enum Item {
    Input(Input),
    /// A hook event. `line` is the raw line, kept so that an event the core stops before
    /// applying can be spooled; the drain of the spool itself sends `None`, since the
    /// lines are still in `.draining`.
    Hook {
        reply: Option<(ConnId, ReqId)>,
        envelope: HookEnvelope,
        line: Option<Box<HookLine>>,
    },
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

/// Appends hook lines to the spool, as `moa hook` does when the daemon is down. A line that
/// can't be written is counted, never shown.
pub(crate) fn spool_hooks<'a>(path: Option<&Path>, lines: impl Iterator<Item = &'a HookLine>) {
    let Some(path) = path else { return };
    let mut lost = 0usize;
    for line in lines {
        let kept = serde_json::to_vec(line)
            .map_err(io::Error::other)
            .and_then(|bytes| spool::append(path, &bytes));
        if kept.is_err() {
            lost += 1;
        }
    }
    if lost > 0 {
        tracing::warn!("{lost} hook events could not be spooled");
    }
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
    /// A view of saved state for the terminal server, or (tokened) an `open`'s new window.
    Tmux {
        conn: u64,
        cmds: Vec<Cmd>,
        token: Option<TmuxToken>,
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
    /// Where a hook the core can't take is kept for the next start.
    spool: Option<PathBuf>,
    /// Asks the spool's drainer for another drain; `None` without a spool.
    drains: Option<mpsc::UnboundedSender<()>>,
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
        Self::start_inner(opts, store, faults, None, 0).await
    }

    /// Like `start_with_store`, with the first `panics` inputs after `Startup` (the spool's
    /// drain) panicking where `handle` is called.
    #[doc(hidden)]
    pub async fn start_with_drain_panics(
        opts: CoreOptions,
        store: Store,
        panics: u32,
    ) -> Result<CoreHandle, DaemonError> {
        Self::start_inner(opts, store, Vec::new(), None, panics).await
    }

    /// `link`, when given, stands in for the terminal link that `opts.tmux` would start
    /// (tests that watch what the core sends it).
    pub(crate) async fn start_inner(
        mut opts: CoreOptions,
        store: Store,
        faults: Vec<Fault>,
        link: Option<mpsc::UnboundedSender<TmuxCmd>>,
        drain_panics: u32,
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

        let (drains, asks) = mpsc::unbounded_channel();
        // Weak: the core owns the scheduler, so a strong sender here would be a cycle that
        // keeps the inbox open after every handle has gone. The drainer's likewise.
        let fire_inbox = inbox.downgrade();
        let fire_drains = drains.downgrade();
        let sched = scheduler::start(
            opts.clock.clone(),
            opts.alarm,
            stats.clone(),
            move |wake| match wake {
                Wake::Core(key) => {
                    if let Some(inbox) = fire_inbox.upgrade() {
                        let _ = inbox.send(Item::Input(Input::Timer(key)));
                    }
                }
                Wake::Spool => {
                    if let Some(drains) = fire_drains.upgrade() {
                        let _ = drains.send(());
                    }
                }
            },
            report_failure(&inbox),
        );

        // Weak for the same reason: the core owns the link.
        let link = link.or_else(|| {
            let heard = inbox.downgrade();
            opts.tmux.take().map(|l| {
                tmux::start(
                    l,
                    move |input| {
                        if let Some(inbox) = heard.upgrade() {
                            let _ = inbox.send(Item::Input(Input::Tmux(input)));
                        }
                    },
                    report_failure(&inbox),
                )
            })
        });

        let spool = opts.spool.take();
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
            spool: spool.clone(),
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
            spool: spool.clone(),
            drains: spool.is_some().then_some(drains),
        };
        // Startup is the first item, so this answers after its commit, not after its daily
        // backup (the store thread is busy copying; `barrier()` and `stop()` wait for it). If
        // Startup brought the core down, the answer is dropped and returns, and the line is here.
        handle.committed().await;
        handle.alive()?;
        if let Some(path) = spool {
            let drainer = Drainer {
                inbox: handle.inbox.downgrade(),
                sched: handle.sched.clone(),
                stats: handle.stats.clone(),
                finished: handle.finished.clone(),
                generation: handle.generation.clone(),
                path,
            };
            handle.panic_next.store(drain_panics, Ordering::SeqCst);
            drainer.drain().await;
            // The drain can bring the core down too.
            handle.alive()?;
            // Started only now, so the drain at start is never run beside another one.
            tokio::spawn(drainer.run(asks, handle.ended.clone()));
        }
        Ok(handle)
    }

    /// `Err` with the core's last line when it has stopped.
    fn alive(&self) -> Result<(), DaemonError> {
        match self.finished.borrow().clone() {
            Some(Err(line)) => Err(DaemonError::Fatal(line)),
            _ => Ok(()),
        }
    }

    /// Asks for another drain of the spool: one runs at a time, and one asked for while
    /// another runs follows it.
    pub(crate) fn drain_spool(&self) {
        if let Some(drains) = &self.drains {
            let _ = drains.send(());
        }
    }

    /// After a live hook: lines in the spool are hooks that gave up on this daemon while it
    /// was up (a full accept backlog, a line bigger than the socket's buffer during start).
    /// One `stat` tells.
    pub(crate) fn drain_spool_if_any(&self) {
        let waiting = self
            .spool
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .is_some_and(|m| m.len() > 0);
        if waiting {
            self.drain_spool();
        }
    }
}

/// Drains the hook spool into the core. The one at start runs before the drainer task is
/// started; after that only the task drains, one drain at a time, so two drains never share
/// the spool's `.draining` file.
struct Drainer {
    /// Weak, so the drainer never keeps the core up.
    inbox: mpsc::WeakUnboundedSender<Item>,
    sched: mpsc::UnboundedSender<SchedCmd>,
    stats: Arc<Stats>,
    finished: watch::Receiver<Option<Result<(), String>>>,
    generation: watch::Receiver<u64>,
    path: PathBuf,
}

impl Drainer {
    /// Drains once per ask (asks that come during a drain make one more), until every
    /// handle has gone or the core has ended.
    async fn run(self, mut asks: mpsc::UnboundedReceiver<()>, mut ended: watch::Receiver<()>) {
        loop {
            tokio::select! {
                ask = asks.recv() => if ask.is_none() {
                    return;
                },
                _ = ended.changed() => return,
            }
            while asks.try_recv().is_ok() {}
            self.drain().await;
        }
    }

    async fn drain(&self) {
        self.drain_once().await;
        self.stats.spool_drains.fetch_add(1, Ordering::SeqCst);
    }

    /// Feeds the events hooks spooled (while the daemon was down, or couldn't take them) to
    /// the core, oldest first, and deletes the spool's `.draining` file once they are
    /// committed. A drain whose commit failed, whose core stopped or whose events a refusal
    /// held back leaves it for a later drain. Nothing here logs a line's contents: an
    /// unreadable line is counted.
    async fn drain_once(&self) {
        let Some(inbox) = self.inbox.upgrade() else {
            return;
        };
        let path = self.path.clone();
        let taken = match tokio::task::spawn_blocking(move || spool::take(&path)).await {
            Ok(Ok(taken)) => taken,
            Ok(Err(e)) => {
                tracing::warn!("the hook spool couldn't be read: {e}");
                return;
            }
            Err(_) => return,
        };
        let mut unreadable = 0usize;
        let mut envelopes: Vec<_> = taken
            .lines
            .iter()
            .filter_map(|line| {
                let envelope = serde_json::from_str::<HookLine>(line)
                    .ok()
                    .and_then(|l| claude::normalize(&l, &claude::last_assistant_text));
                if envelope.is_none() {
                    unreadable += 1;
                }
                envelope
            })
            .collect();
        envelopes.sort_by_key(|e| (e.at_ns, e.pid, e.seq));
        let drained = envelopes.len();
        let failures = self.stats.commit_failures.load(Ordering::SeqCst);
        let refused = self.stats.refused_hooks.load(Ordering::SeqCst);
        let panics = self.stats.panics.load(Ordering::SeqCst);
        let generation = *self.generation.borrow();
        for envelope in envelopes {
            // The lines are still in `.draining`: none goes with the event.
            let _ = inbox.send(Item::Hook {
                reply: None,
                envelope,
                line: None,
            });
        }
        committed(&inbox, &self.sched).await;
        drop(inbox);
        let kept = || {
            if drained + unreadable > 0 {
                tracing::warn!(
                    "{} spooled hook events were kept for a later drain",
                    drained + unreadable
                );
            }
        };
        // A failed commit, a stopped store, an ended core or refused writes: the lines are
        // fine, try again later (the end of a refusal drains again). Every failure reloads
        // the core, which bumps the generation.
        let unsaved = self.stats.commit_failures.load(Ordering::SeqCst) != failures
            || self.stats.refused_hooks.load(Ordering::SeqCst) != refused
            || self.finished.borrow().is_some();
        if unsaved {
            kept();
            return;
        }
        // The core panicked on a line: it would again, every start. Set the file aside.
        if self.stats.panics.load(Ordering::SeqCst) != panics {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            match taken.set_aside(&at.to_string()) {
                Ok(file) => tracing::warn!(
                    "set aside {} spooled hook events after the core failed on them; see {}",
                    drained + unreadable,
                    file.file_name().unwrap_or_default().to_string_lossy()
                ),
                Err(e) => tracing::warn!("the failing hook spool couldn't be set aside: {e}"),
            }
            return;
        }
        if *self.generation.borrow() != generation {
            kept();
            return;
        }
        if let Err(e) = taken.done() {
            tracing::warn!("the drained hook spool couldn't be removed: {e}");
        }
        if drained + unreadable > 0 {
            let more = if unreadable > 0 {
                format!(" + {unreadable} unreadable")
            } else {
                String::new()
            };
            tracing::info!("drained {drained} spooled hook events{more}");
        }
    }
}

impl CoreHandle {
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

    /// A hook event. With `reply`, the core answers it once, after the commit. `line` is the
    /// raw line it came from: when the core has stopped, or stops before applying it, the
    /// line goes to the spool and the next start applies it.
    pub fn hook(
        &self,
        reply: Option<(ConnId, ReqId)>,
        envelope: HookEnvelope,
        line: Option<HookLine>,
    ) {
        let item = Item::Hook {
            reply,
            envelope,
            line: line.map(Box::new),
        };
        if let Err(mpsc::error::SendError(Item::Hook { line, .. })) = self.inbox.send(item) {
            spool_hooks(self.spool.as_deref(), line.iter().map(|l| &**l));
            if let Some((conn, req)) = reply {
                reply_to(&self.conns, conn, req, Err(OpError(STOPPING.into())));
            }
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
        committed(&self.inbox, &self.sched).await;
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

/// What a supervised actor calls when it panicked: the core hears `ActorFailed` through a
/// weak sender. The raw message stays here: it may carry contents (2a's no-contents rule).
fn report_failure(
    inbox: &mpsc::UnboundedSender<Item>,
) -> impl Fn(&'static str, String) + Send + 'static {
    let inbox = inbox.downgrade();
    move |actor, _message| {
        if let Some(inbox) = inbox.upgrade() {
            let _ = inbox.send(Item::Input(Input::ActorFailed {
                actor,
                message: String::new(),
            }));
        }
    }
}

/// Answers once everything sent to `inbox` before it has been committed and released.
async fn committed(inbox: &mpsc::UnboundedSender<Item>, sched: &mpsc::UnboundedSender<SchedCmd>) {
    sync_scheduler(sched).await;
    let (reply, answer) = oneshot::channel();
    if inbox.send(Item::Committed(reply)).is_ok() {
        let _ = answer.await;
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
