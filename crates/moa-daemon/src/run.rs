//! The daemon process: the lock, the socket, the core, and the loop that keeps them up.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::state::Deps;
use moa_platform::{Paths, WallTimer, ZoneWatch, ensure_private_dir};
use tokio::net::UnixListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::core_task::{CoreHandle, CoreOptions, DaemonError};
use crate::logging;
use crate::server::accept_loop;
use crate::supervise::supervise;

/// How long a stop waits for the core to finish what it holds (backups included).
const STOP_WAIT: Duration = Duration::from_secs(30);
/// How long `run` waits, after closing the sessions, for them to finish writing what they
/// owe (a session's own grace is 1 s).
const SESSIONS_WAIT: Duration = Duration::from_millis(2500);

/// How `run` says it is ready for clients.
pub enum Ready {
    /// Print `ready` on stdout, then point stdout at `/dev/null`. A failure before that
    /// prints `error: <line>` (or `already running`) instead.
    Stdout,
    /// Send the socket's path (tests).
    Channel(oneshot::Sender<PathBuf>),
    Nothing,
}

pub struct RunOptions {
    pub paths: Paths,
    pub brain: BrainName,
    pub config: Config,
    pub tz: TimeZone,
    pub clock: Arc<dyn Clock>,
    pub alarm: Box<dyn WallTimer>,
    pub zone: Option<ZoneWatch>,
    pub ready: Ready,
    pub events_capacity: usize,
    /// Tests: pretend to be another user, to see that this one is turned away.
    #[doc(hidden)]
    pub uid_override: Option<u32>,
}

/// Reads `moa.toml` (no file is the default config), with folders in their canonical form.
/// Shared by `moa daemon` and the client that starts it.
pub fn load_config(paths: &Paths) -> Result<Config, DaemonError> {
    let text = paths.read_config()?;
    Ok(Config::from_file_text(
        text.as_deref(),
        paths.home(),
        moa_platform::canonical,
    )?)
}

/// Everything that has to work before the daemon says `ready`.
struct Up {
    /// Held for the daemon's life: the lock is released when this closes.
    _lock: File,
    listener: UnixListener,
    sock: PathBuf,
    core: CoreHandle,
    sigs: Signals,
}

/// The signals the daemon answers.
struct Signals {
    term: Signal,
    int: Signal,
    hup: Signal,
}

fn remove_socket(sock: &Path) {
    if let Err(e) = std::fs::remove_file(sock)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!("couldn't remove the socket file: {}", e.kind());
    }
}

#[allow(clippy::too_many_arguments)]
async fn bring_up(
    paths: &Paths,
    brain: &BrainName,
    config: Config,
    tz: TimeZone,
    clock: Arc<dyn Clock>,
    alarm: Box<dyn WallTimer>,
    events_capacity: usize,
) -> Result<Up, DaemonError> {
    let b = brain.as_str();
    config
        .require_brain(brain, &paths.config_file())
        .map_err(|_| DaemonError::NoSuchBrain(brain.clone(), paths.config_file()))?;

    let sock = paths.socket(b)?;
    ensure_private_dir(&paths.run_dir())?;
    if let Some(dir) = sock.parent() {
        ensure_private_dir(dir)?;
    }
    ensure_private_dir(&paths.log_dir())?;

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.lock(b))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(DaemonError::AlreadyRunning(brain.clone())),
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }

    // Holding the lock, whatever is at the socket's path is stale.
    match std::fs::remove_file(&sock) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let listener = UnixListener::bind(&sock)?;
    // From here a failure leaves no socket file behind.
    let up = async {
        let term = signal(SignalKind::terminate())?;
        let int = signal(SignalKind::interrupt())?;
        let hup = signal(SignalKind::hangup())?;
        logging::init(&paths.log(b))?;
        tracing::info!("moa daemon starting for brain {b}");
        if !config.skipped_tables.is_empty() {
            tracing::warn!(
                "moa.toml has tables moa doesn't know, which it skips: {}",
                config.skipped_tables.join(", ")
            );
        }
        let deps = Deps {
            parse_action: Some(moa_proto::cli::parse_action),
            canonical: moa_platform::canonical,
            home: paths.home().to_path_buf(),
        };
        let core = CoreHandle::start(CoreOptions {
            brain: brain.clone(),
            config,
            deps,
            tz,
            clock,
            alarm,
            db: paths.db(b),
            backups: paths.backups(b),
            events_capacity,
        })
        .await?;
        Ok::<_, DaemonError>((core, Signals { term, int, hup }))
    }
    .await;
    match up {
        Ok((core, sigs)) => Ok(Up {
            _lock: lock,
            listener,
            sock,
            core,
            sigs,
        }),
        Err(e) => {
            remove_socket(&sock);
            Err(e)
        }
    }
}

/// What a failure before `ready` looks like on stdout.
fn announce_failure(e: &DaemonError) {
    let mut out = io::stdout();
    let _ = match e {
        DaemonError::AlreadyRunning(_) => writeln!(out, "already running"),
        e => writeln!(out, "error: {e}"),
    };
    let _ = out.flush();
}

fn announce_ready(ready: Ready, sock: &Path) {
    match ready {
        Ready::Stdout => {
            let mut out = io::stdout();
            let _ = writeln!(out, "ready");
            let _ = out.flush();
            if let Err(e) = moa_platform::stdout_to_null() {
                tracing::warn!("couldn't point stdout at /dev/null: {}", e.kind());
            }
        }
        Ready::Channel(tx) => {
            let _ = tx.send(sock.to_path_buf());
        }
        Ready::Nothing => {}
    }
}

/// Resolves when the system zone may have changed; never, without a watch.
async fn zone_changed(zone: &mut Option<ZoneWatch>) -> io::Result<()> {
    match zone {
        Some(z) => z.changed().await,
        None => std::future::pending().await,
    }
}

/// Runs the daemon until it is told to stop (`Ok`) or the core can't go on (`Err`).
pub async fn run(opts: RunOptions) -> Result<(), DaemonError> {
    let RunOptions {
        paths,
        brain,
        config,
        tz,
        clock,
        alarm,
        zone,
        ready,
        events_capacity,
        uid_override,
    } = opts;
    // Hash this binary now: a binary replaced before the first hello must not make this old
    // daemon claim the new build.
    let _ = tokio::task::spawn_blocking(moa_platform::build_id).await;

    let up = bring_up(&paths, &brain, config, tz, clock, alarm, events_capacity).await;
    let Up {
        _lock,
        listener,
        sock,
        core,
        sigs,
    } = match up {
        Ok(up) => up,
        Err(e) => {
            if matches!(ready, Ready::Stdout) {
                announce_failure(&e);
            }
            return Err(e);
        }
    };
    announce_ready(ready, &sock);
    tracing::info!("ready on {}", sock.display());

    let my_uid = uid_override.unwrap_or_else(moa_platform::current_uid);
    keep_up(core, listener, sock, brain, my_uid, zone, sigs).await
}

/// Accepts clients and watches for the end, then winds down. `Ok` after a signal or a
/// `shutdown`; `Err` when the core can't go on.
async fn keep_up(
    core: CoreHandle,
    listener: UnixListener,
    sock: PathBuf,
    brain: BrainName,
    my_uid: u32,
    mut zone: Option<ZoneWatch>,
    mut sigs: Signals,
) -> Result<(), DaemonError> {
    // `stop` ends the accept loop (and is what `shutdown` cancels); `close` ends the
    // sessions, and only after the core is done, so each gets its reply or the stopping
    // line first.
    let stop = CancellationToken::new();
    let close = CancellationToken::new();
    let sessions = TaskTracker::new();
    let listener = Arc::new(listener);
    let accept = tokio::spawn(supervise(
        "accept loop",
        {
            let (core, brain, stop, close, sessions) = (
                core.clone(),
                brain.clone(),
                stop.clone(),
                close.clone(),
                sessions.clone(),
            );
            move || {
                accept_loop(
                    listener.clone(),
                    core.clone(),
                    brain.clone(),
                    stop.clone(),
                    close.clone(),
                    sessions.clone(),
                    my_uid,
                )
            }
        },
        |name, _message| tracing::error!("the {name} panicked and was restarted"),
    ));

    let outcome = loop {
        tokio::select! {
            _ = sigs.term.recv() => {
                tracing::info!("SIGTERM: stopping");
                break Ok(());
            }
            _ = sigs.int.recv() => {
                tracing::info!("SIGINT: stopping");
                break Ok(());
            }
            _ = sigs.hup.recv() => tracing::info!("SIGHUP ignored"),
            _ = stop.cancelled() => {
                tracing::info!("asked to stop");
                break Ok(());
            }
            ended = core.finished() => {
                if let Err(e) = &ended {
                    tracing::error!("the core stopped the daemon: {e}");
                }
                break ended;
            }
            changed = zone_changed(&mut zone) => match changed {
                Ok(()) => {
                    // jiff caches the system zone for minutes: drop that first.
                    jiff::tz::reset_system_time_zone_cache();
                    match TimeZone::try_system() {
                        Ok(tz) => core.zone_changed(tz),
                        Err(e) => tracing::warn!("the zone changed but can't be read; keeping the old one: {e}"),
                    }
                }
                Err(e) => {
                    tracing::warn!("the zone watch failed and is off: {}", e.kind());
                    zone = None;
                }
            },
        }
    };

    // Stop accepting, then let the core finish: after a fatal stop `finished()` has spoken
    // but the core is still answering its queue, so wait for its end either way.
    stop.cancel();
    accept.abort();
    let wound_down = tokio::time::timeout(STOP_WAIT, async {
        core.stop().await;
        core.ended().await;
    })
    .await;
    if wound_down.is_err() {
        tracing::error!("the core didn't finish within {} s", STOP_WAIT.as_secs());
    }
    // Sessions that still wait for something are closed now; the rest already closed when
    // the core dropped their reply channels.
    close.cancel();
    // Wait for them, bounded: the process may exit right after this returns, and a session
    // cut off mid-write leaves its client with EOF instead of its line.
    sessions.close();
    if tokio::time::timeout(SESSIONS_WAIT, sessions.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            "some sessions were still open after {} ms",
            SESSIONS_WAIT.as_millis()
        );
    }
    remove_socket(&sock);
    tracing::info!("stopped");
    outcome
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
