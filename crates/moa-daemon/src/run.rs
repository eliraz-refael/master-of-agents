//! The daemon process: the lock, the socket, the core, and the loop that keeps them up.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::{BrainName, CONSTITUTION_NAME, Config, UiConfig};
use moa_core::model::AgentKind;
use moa_core::state::{Deps, PanelLaunch};
use moa_core::upgrade::UpgradeFacts;
use moa_platform::fs::write_if_changed;
use moa_platform::{Inherited, Paths, WallTimer, ZoneWatch, ensure_private_dir, which};
use moa_tmux::conf;
use tokio::net::UnixListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::clock::Clock;
use crate::core_task::{CoreHandle, CoreOptions, DaemonError};
use crate::follow::Follow;
use crate::logging;
use crate::server::accept_loop;
use crate::session::Program;
use crate::supervise::supervise;
use crate::tmux::{TmuxLink, tmux_env};

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
    /// Send the socket's path and the core (tests that queue store faults).
    #[doc(hidden)]
    Core(oneshot::Sender<(PathBuf, CoreHandle)>),
    Nothing,
}

/// How the daemon hands its socket and lock to the next build when it upgrades (L5).
pub enum Exec {
    /// Replace this process with the daemon's program, keeping both open.
    Real,
    /// Send them to a test instead, which starts the next image itself: a test can never
    /// replace its own process.
    Test(mpsc::Sender<Inherited>),
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
    /// The daemon's program ([`daemon_exe`]): what it watches, runs its panels and hooks
    /// from, and becomes when it upgrades.
    pub exe: PathBuf,
    /// This image's build: `moa_platform::build_id()`. The daemon is stale once its
    /// program's build differs.
    pub build: String,
    /// How an upgrade hands over to the next build.
    pub exec: Exec,
    /// What the previous image handed over, when this one is its upgrade
    /// (`moa_platform::take_inherited`): its socket and lock are taken instead of made.
    pub inherited: Option<Inherited>,
}

/// Reads `moa.toml` (no file is the default config), with folders in their canonical form
/// and every Claude account's launcher filled in where `PATH` has one. Shared by
/// `moa daemon` and the client that starts it.
pub fn load_config(paths: &Paths) -> Result<Config, DaemonError> {
    let text = paths.read_config()?;
    let mut config =
        Config::from_file_text(text.as_deref(), paths.home(), moa_platform::canonical)?;
    fill_launchers(&mut config, std::env::var_os("PATH").as_deref());
    Ok(config)
}

/// H16: a Claude account with no `launcher` uses the `claude` on `path`, looked for once,
/// here, since the core can't search `PATH`. One still without a launcher can't open
/// sessions.
pub(crate) fn fill_launchers(config: &mut Config, path: Option<&OsStr>) {
    let mut found: Option<Option<PathBuf>> = None;
    for a in config
        .accounts
        .values_mut()
        .filter(|a| a.kind == AgentKind::Claude && a.launcher.is_none())
    {
        a.launcher = found.get_or_insert_with(|| which("claude", path)).clone();
    }
}

/// Everything that has to work before the daemon says `ready`.
struct Up {
    /// Held for the daemon's life: the lock is released when this closes, or handed to the
    /// next image by an upgrade.
    lock: File,
    /// Shared with the accept loop.
    listener: Arc<UnixListener>,
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

/// Writes the brain's two tmux config files when their text changed (T6), and says where
/// its server is, and whether a terminal attached under the old `moa.tmux` must attach again
/// ([`conf::needs_reattach`], L7; never when there was none). The brain's folder is checked
/// like the other private folders. Its popups start panels as the core does, from `launch`,
/// at home.
fn tmux_link(
    paths: &Paths,
    b: &str,
    ui: &UiConfig,
    launch: &PanelLaunch,
) -> Result<(TmuxLink, bool), DaemonError> {
    ensure_private_dir(&paths.brain_dir(b))?;
    let moa_tmux = paths.moa_tmux(b);
    let text = conf::moa_tmux(b, ui, launch, paths.home());
    // An old file that can't be read is rewritten as before, and asks for no re-attach.
    let reattach = read_text(&moa_tmux)
        .ok()
        .flatten()
        .is_some_and(|old| conf::needs_reattach(&old, &text));
    write_if_changed(&moa_tmux, &text)?;
    write_if_changed(&paths.tmux_conf(b), &conf::tmux_conf(&moa_tmux))?;
    let link = TmuxLink {
        socket: paths.tmux_socket(b)?,
        moa_tmux,
        env: tmux_env(std::env::vars_os()),
    };
    Ok((link, reattach))
}

/// The daemon's own program, read once and handed to everything that runs it (the session
/// plugin's hooks, the panels), watched for new builds and upgraded into (L2): `MOA_EXE`, the
/// path the client that started it ran, links kept, when that is a full path that exists.
/// Otherwise its real path. Linux names a binary replaced under a running daemon
/// `<path> (deleted)`; the program to run is the one now at `<path>`.
pub fn daemon_exe() -> io::Result<PathBuf> {
    if let Some(exe) = std::env::var_os("MOA_EXE").map(PathBuf::from)
        && exe.is_absolute()
        && exe.exists()
    {
        return Ok(exe);
    }
    let exe = undeleted(std::env::current_exe()?);
    // A replaced binary's new path may be missing for a moment: the path as read, then.
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// `p` without Linux's trailing ` (deleted)`.
fn undeleted(p: PathBuf) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    match p.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
        Some(kept) => PathBuf::from(std::ffi::OsStr::from_bytes(kept)),
        None => p,
    }
}

/// How the core starts a panel (L7): the daemon's program `exe`, and, explicitly (lesson 5),
/// `MOA_ROOT` when sandboxed, so a sandboxed panel talks to this daemon, never the real one,
/// or `MOA_REAL_HOME` when the daemon may use the real home, so the panel may too.
fn panel_launch(paths: &Paths, exe: &Path) -> PanelLaunch {
    let env = if paths.is_sandboxed() {
        vec![("MOA_ROOT".to_string(), paths.home().display().to_string())]
    } else if paths.real_home_ok() {
        vec![("MOA_REAL_HOME".to_string(), "1".to_string())]
    } else {
        Vec::new()
    };
    PanelLaunch {
        exe: exe.to_path_buf(),
        env,
    }
}

/// The plugin's folder as `CLAUDE_CODE_PLUGIN_DIRS` needs it: absolute, since Claude Code
/// skips a relative entry without a word.
fn session_plugin_dir(paths: &Paths) -> io::Result<PathBuf> {
    std::path::absolute(paths.session_plugin())
}

/// Writes the Claude Code plugin that sessions are started with, when its text changed, and
/// says where it is and whether this changed it (L7: the sessions running keep the old one).
/// Its `hooks.json` names a command Claude runs, and its skill is read by the agent, so
/// every folder in it is checked like the other private folders: ours, real, and closed to
/// other users.
fn write_session_plugin(paths: &Paths, exe: &Path) -> Result<(PathBuf, bool), DaemonError> {
    let dir = session_plugin_dir(paths)?;
    ensure_private_dir(&dir)?;
    let mut changed = false;
    for (rel, text) in moa_agents::claude::session_plugin(exe) {
        let rel = Path::new(rel);
        // Outermost first: `skills`, then `skills/moa-peers`.
        let mut folders: Vec<&Path> = rel
            .ancestors()
            .skip(1)
            .filter(|a| !a.as_os_str().is_empty())
            .collect();
        folders.reverse();
        for folder in folders {
            ensure_private_dir(&dir.join(folder))?;
        }
        changed |= write_if_changed(&dir.join(rel), text.as_str())?;
    }
    Ok((dir, changed))
}

/// The brain's lock, taken: the one the previous image handed over (the same open file, so
/// it is still ours), or a new one.
fn take_lock(
    paths: &Paths,
    brain: &BrainName,
    inherited: Option<File>,
) -> Result<File, DaemonError> {
    let lock = match inherited {
        Some(lock) => lock,
        None => OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(paths.lock(brain.as_str()))?,
    };
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(DaemonError::AlreadyRunning(brain.clone())),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The listening socket: the one the previous image handed over, still bound and with the
/// connections made meanwhile in its backlog, or a new one at `sock`, whatever was there
/// before being stale (the lock is held).
fn listen(sock: &Path, inherited: Option<StdUnixListener>) -> Result<UnixListener, DaemonError> {
    if let Some(listener) = inherited {
        listener.set_nonblocking(true)?;
        return Ok(UnixListener::from_std(listener)?);
    }
    match std::fs::remove_file(sock) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(UnixListener::bind(sock)?)
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
    exe: &Path,
    build: &str,
    inherited: Option<Inherited>,
) -> Result<Up, DaemonError> {
    let b = brain.as_str();
    if config.brain(brain).is_none() {
        return Err(DaemonError::NoSuchBrain(brain.clone(), paths.config_file()));
    }

    let sock = paths.socket(b)?;
    ensure_private_dir(&paths.run_dir())?;
    if let Some(dir) = sock.parent() {
        ensure_private_dir(dir)?;
    }
    ensure_private_dir(&paths.log_dir())?;

    let (listener, lock, from) = match inherited {
        Some(i) => (Some(i.listener), Some(i.lock), Some(i.from_build)),
        None => (None, None, None),
    };
    let lock = take_lock(paths, brain, lock)?;
    let listener = listen(&sock, listener)?;
    // From here a failure leaves no socket file behind.
    let up = async {
        let term = signal(SignalKind::terminate())?;
        let int = signal(SignalKind::interrupt())?;
        let hup = signal(SignalKind::hangup())?;
        logging::init(&paths.log(b))?;
        match &from {
            Some(from) => tracing::info!("moa daemon upgraded from build {from} for brain {b}"),
            None => tracing::info!("moa daemon starting for brain {b}"),
        }
        if !config.skipped_tables.is_empty() {
            tracing::warn!(
                "moa.toml has tables moa doesn't know, which it skips: {}",
                config.skipped_tables.join(", ")
            );
        }
        let launch = panel_launch(paths, exe);
        // T21: until the switch-over, only a sandbox's own server is ever touched.
        let (tmux, reattach) = if paths.is_sandboxed() {
            let (link, reattach) = tmux_link(paths, b, &config.ui, &launch)?;
            (Some(link), reattach)
        } else {
            tracing::info!("the terminal link waits for the switch-over");
            (None, false)
        };
        let (session_plugin, plugin_changed) = write_session_plugin(paths, exe)?;
        // What the core's notice says after an upgrade (L7): the build it compares with the
        // last start's is this image's, as `RunOptions` says it.
        let facts = UpgradeFacts {
            build: build.to_string(),
            reexec: from.is_some(),
            plugin_changed,
            reattach,
        };
        // The client passes the real home on only from a sandbox, for moa's agent's login.
        let agent_home = std::env::var_os("MOA_AGENT_HOME")
            .map(PathBuf::from)
            .filter(|h| paths.is_sandboxed() && h.is_absolute());
        let deps = Deps {
            parse_action: Some(moa_proto::cli::parse_action),
            parse_command: Some(moa_proto::cli::parse_command),
            canonical: moa_platform::canonical,
            home: paths.home().to_path_buf(),
            new_uuid: moa_store::new_uuid,
            session_plugin,
            inspect: crate::inspect::inspect_pane,
            process_name: moa_platform::proc::name,
            transcript: moa_agents::claude::find_transcript,
            is_dir: std::path::Path::is_dir,
            newest_conversation: moa_agents::claude::newest_conversation,
            read_text,
            layouts_file: paths.config_dir().join("layouts.json"),
            panel: launch,
            // Set only in the packaged build; a plain `cargo` build (every test's) knows none.
            acp_adapter: moa_agents::acp::BUILT_IN_ADAPTER.map(PathBuf::from),
            agent_home,
            agent_dir: paths.agent_dir(b),
            scope_file: paths.config_dir().join(format!("{b}.md")),
            constitution_file: paths.config_dir().join(format!("{CONSTITUTION_NAME}.md")),
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
            tmux,
            spool: Some(paths.spool(b)),
            facts: Some(facts),
        })
        .await?;
        Ok::<_, DaemonError>((core, Signals { term, int, hup }))
    }
    .await;
    match up {
        Ok((core, sigs)) => Ok(Up {
            lock,
            listener: Arc::new(listener),
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

/// A text file's contents; `None` when there is no file.
fn read_text(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
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

fn announce_ready(ready: Ready, sock: &Path, core: &CoreHandle) {
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
        Ready::Core(tx) => {
            let _ = tx.send((sock.to_path_buf(), core.clone()));
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
        exe,
        build,
        exec,
        inherited,
    } = opts;
    // Hash this binary now: a binary replaced before the first hello must not make this old
    // daemon claim the new build.
    let _ = tokio::task::spawn_blocking(moa_platform::build_id).await;

    let up = bring_up(
        &paths,
        &brain,
        config,
        tz,
        clock,
        alarm,
        events_capacity,
        &exe,
        &build,
        inherited,
    )
    .await;
    let up = match up {
        Ok(up) => up,
        Err(e) => {
            if matches!(ready, Ready::Stdout) {
                announce_failure(&e);
            }
            return Err(e);
        }
    };
    // Armed before `ready`, so a new build put in place right after it is heard.
    let follow = Follow::new(exe.clone(), build.clone());
    announce_ready(ready, &up.sock, &up.core);
    tracing::info!("ready on {}", up.sock.display());

    let my_uid = uid_override.unwrap_or_else(moa_platform::current_uid);
    let program = Arc::new(Program { exe, build });
    match keep_up(up, brain.clone(), program.clone(), my_uid, zone, follow).await? {
        None => Ok(()),
        Some(handoff) => hand_over(handoff, &brain, &program, exec),
    }
}

/// What an upgrade hands the next image: the listening socket and the brain's lock, both
/// still open.
struct Handoff {
    /// A copy of the listener's descriptor.
    listener: OwnedFd,
    lock: File,
    sock: PathBuf,
}

/// Hands the socket and the lock to the next image of the daemon (L5): its program,
/// re-exec'd with them, or the test that stands in for it. A failed exec ends the daemon
/// (L7): the next client starts a fresh one.
fn hand_over(
    handoff: Handoff,
    brain: &BrainName,
    program: &Program,
    exec: Exec,
) -> Result<(), DaemonError> {
    let Handoff {
        listener,
        lock,
        sock,
    } = handoff;
    match exec {
        Exec::Real => {
            tracing::info!("upgrading into {}", program.exe.display());
            let args = ["daemon", "--brain", brain.as_str()];
            let e = moa_platform::reexec(
                &program.exe,
                &args,
                listener.as_fd(),
                lock.as_fd(),
                &program.build,
            );
            tracing::error!("the upgrade couldn't start {}: {e}", program.exe.display());
            remove_socket(&sock);
            Err(e.into())
        }
        Exec::Test(tx) => {
            let inherited = Inherited {
                listener: StdUnixListener::from(listener),
                lock,
                from_build: program.build.clone(),
            };
            if tx.try_send(inherited).is_err() {
                tracing::warn!("nobody took the handoff");
            }
            tracing::info!("handed over");
            Ok(())
        }
    }
}

/// Accepts clients, follows the program and watches for the end, then winds down: one wind
/// down, whether the daemon stops or upgrades. `Ok(None)` after a signal or a `shutdown`, the
/// socket file removed; `Ok(Some)` at the core's word to upgrade, with what the next image
/// takes over; `Err` when the core can't go on.
async fn keep_up(
    up: Up,
    brain: BrainName,
    program: Arc<Program>,
    my_uid: u32,
    mut zone: Option<ZoneWatch>,
    follow: Follow,
) -> Result<Option<Handoff>, DaemonError> {
    let Up {
        lock,
        listener,
        sock,
        core,
        mut sigs,
    } = up;
    // `stop` ends the accept loop (and is what `shutdown` cancels); `close` ends the
    // sessions, and only after the core is done, so each gets its reply or the stopping
    // line first.
    let stop = CancellationToken::new();
    let close = CancellationToken::new();
    let sessions = TaskTracker::new();
    let accept = tokio::spawn(supervise(
        "accept loop",
        {
            let (listener, core, brain, stop, close, sessions) = (
                listener.clone(),
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
                    program.clone(),
                    stop.clone(),
                    close.clone(),
                    sessions.clone(),
                    my_uid,
                )
            }
        },
        {
            let core = core.clone();
            move |name, _message| core.actor_failed(name)
        },
    ));
    // Hooks that gave up while the daemon started (a line bigger than the socket's buffer,
    // written before anything accepted it) spooled their lines after the drain at start.
    core.drain_spool();
    let following = tokio::spawn(follow.run(core.clone()));

    let outcome = loop {
        tokio::select! {
            _ = sigs.term.recv() => {
                tracing::info!("SIGTERM: stopping");
                break Ok(End::Stop);
            }
            _ = sigs.int.recv() => {
                tracing::info!("SIGINT: stopping");
                break Ok(End::Stop);
            }
            _ = sigs.hup.recv() => tracing::info!("SIGHUP ignored"),
            _ = stop.cancelled() => {
                tracing::info!("asked to stop");
                break Ok(End::Stop);
            }
            () = core.exec_wanted() => {
                tracing::info!("upgrading: winding down");
                break Ok(End::Upgrade);
            }
            ended = core.finished() => {
                if let Err(e) = &ended {
                    tracing::error!("the core stopped the daemon: {e}");
                }
                break ended.map(|()| End::Stop);
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
    following.abort();
    stop.cancel();
    // At a stop, lines still in the kernel's accept backlog are dropped with the socket: an
    // accepted window. At an upgrade they wait there for the next image.
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
    let outcome = match outcome {
        // The next image gets a copy of the listener: tokio keeps its own until it drops.
        Ok(End::Upgrade) => match listener.as_fd().try_clone_to_owned() {
            Ok(listener) => {
                return Ok(Some(Handoff {
                    listener,
                    lock,
                    sock,
                }));
            }
            Err(e) => Err(e.into()),
        },
        outcome => outcome,
    };
    remove_socket(&sock);
    tracing::info!("stopped");
    outcome.map(|_| None)
}

/// Why `keep_up`'s loop ended.
enum End {
    /// A signal or `shutdown`.
    Stop,
    /// The core wants the daemon upgraded.
    Upgrade,
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
