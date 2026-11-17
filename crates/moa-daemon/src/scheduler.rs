//! The wall-clock scheduler: it keeps the core's deadlines, arms one `WallTimer` at the
//! earliest, and tells the core when a deadline has come. It keeps one deadline of the
//! daemon's own too: the end of a refusal of writes, when the hook spool is drained again.
//!
//! The core owns *what* its deadlines are (`SetTimer`, `CancelTimer` effects); this only
//! keeps them and wakes up. After a zone change the core sends fresh `SetTimer`s, which is
//! how every deadline is re-derived and re-armed: macOS absolute timers don't follow
//! wall-clock steps.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use moa_core::UtcMs;
use moa_core::effect::TimerKey;
use moa_platform::WallTimer;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::clock::{Clock, system_time};
use crate::core_task::Stats;
use crate::supervise::supervise;

/// What a deadline wakes: the core, with one of its timers, or the spool's drainer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Wake {
    Core(TimerKey),
    /// A refusal of writes ends: the hooks spooled under it are drained.
    Spool,
}

pub(crate) enum SchedCmd {
    Set(Wake, UtcMs),
    Cancel(Wake),
    /// Answers once every command sent before it has been applied. It fires nothing: only
    /// the alarm does, in tests as in production.
    Sync(oneshot::Sender<()>),
    /// The core has ended: stop, which disarms the alarm.
    Stop,
}

/// Pause after the alarm itself fails, so a broken timer doesn't spin.
const ALARM_RETRY: Duration = Duration::from_secs(1);

struct Scheduler {
    deadlines: BTreeMap<Wake, UtcMs>,
    rx: mpsc::UnboundedReceiver<SchedCmd>,
    alarm: Box<dyn WallTimer>,
    clock: Arc<dyn Clock>,
    stats: Arc<Stats>,
    fire: Box<dyn Fn(Wake) + Send + Sync>,
}

impl Scheduler {
    /// Arms the alarm at the earliest deadline, or disarms it when there is none.
    fn rearm(&mut self) {
        let result = match self.deadlines.values().min() {
            Some(at) => self.alarm.arm(system_time(*at)),
            None => self.alarm.disarm(),
        };
        if let Err(e) = result {
            tracing::warn!("the wall alarm could not be set: {}", e.kind());
        }
    }

    /// Fires, and forgets, every deadline at or before the wall clock. The core answers a
    /// `Timer` with the next `SetTimer`.
    fn fire_due(&mut self) {
        let now = self.clock.wall();
        let due: Vec<Wake> = self
            .deadlines
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(k, _)| *k)
            .collect();
        for key in due {
            self.deadlines.remove(&key);
            self.stats.timer_fires.fetch_add(1, Ordering::SeqCst);
            (self.fire)(key);
        }
    }

    async fn run(&mut self) {
        loop {
            // After every wake-up, due or not: the alarm is one-shot, so a fire with nothing
            // due would otherwise lose the next deadline.
            self.rearm();
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    None => return,
                    Some(SchedCmd::Set(key, at)) => {
                        self.deadlines.insert(key, at);
                    }
                    Some(SchedCmd::Cancel(key)) => {
                        self.deadlines.remove(&key);
                    }
                    Some(SchedCmd::Sync(done)) => {
                        let _ = done.send(());
                    }
                    Some(SchedCmd::Stop) => {
                        let _ = self.alarm.disarm();
                        return;
                    }
                },
                fired = self.alarm.fired() => {
                    if let Err(e) = fired {
                        tracing::warn!("the wall alarm failed: {}", e.kind());
                        tokio::time::sleep(ALARM_RETRY).await;
                    }
                    self.fire_due();
                }
            }
        }
    }
}

/// Starts the scheduler under [`supervise`]. Its deadlines survive a restart. It ends when
/// every sender is dropped. `fire` is called for each due key; `failed` for each panic.
pub(crate) fn start(
    clock: Arc<dyn Clock>,
    alarm: Box<dyn WallTimer>,
    stats: Arc<Stats>,
    fire: impl Fn(Wake) + Send + Sync + 'static,
    failed: impl Fn(&'static str, String) + Send + 'static,
) -> mpsc::UnboundedSender<SchedCmd> {
    let (tx, rx) = mpsc::unbounded_channel();
    let sched = Arc::new(Mutex::new(Scheduler {
        deadlines: BTreeMap::new(),
        rx,
        alarm,
        clock,
        stats,
        fire: Box::new(fire),
    }));
    let make = move || -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        let sched = sched.clone();
        Box::pin(async move { sched.lock().await.run().await })
    };
    tokio::spawn(supervise("scheduler", make, failed));
    tx
}
