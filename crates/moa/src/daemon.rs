//! `moa daemon`: the long-lived process behind every client.

use std::process::ExitCode;

use clap::Args;
use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_daemon::clock::SystemClock;
use moa_daemon::{DaemonError, Exec, Ready, RunOptions, daemon_exe, load_config, run};
use moa_platform::{Inherited, Paths, WallAlarm, ZoneWatch, build_id, take_inherited};

/// How many events a slow console may fall behind by before it is sent a fresh snapshot.
const EVENTS_CAPACITY: usize = 256;

#[derive(Args)]
pub struct DaemonArgs {
    /// Which brain's daemon to run.
    #[arg(long, default_value = "default")]
    brain: String,
    /// Print `ready` on stdout once the socket is up, so the one starting this can wait.
    #[arg(long)]
    notify_ready: bool,
}

/// A panic's message can carry record contents, and stderr is the service manager's log:
/// say where it happened and nothing else.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let at = info.location().map_or_else(
            || "an unknown place".to_string(),
            |l| format!("{}:{}", l.file(), l.line()),
        );
        tracing::error!("moa hit a bug at {at}; see the log");
        eprintln!("moa hit a bug at {at}; see the log");
    }));
}

/// A failure before `ready`: on stdout under `--notify-ready` (the starter reads it there),
/// else on stderr.
fn fail(notify: bool, line: &str) -> ExitCode {
    if notify {
        println!("error: {line}");
    } else {
        eprintln!("{line}");
    }
    ExitCode::from(1)
}

pub fn daemon(args: &DaemonArgs) -> ExitCode {
    // Before any thread starts (the runtime's): an upgrade's previous image handed over its
    // socket and lock in the environment.
    let inherited = match take_inherited() {
        Ok(i) => i,
        Err(e) => return fail(args.notify_ready, &e.to_string()),
    };
    install_panic_hook();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(args.notify_ready, &format!("can't start: {e}")),
    };
    runtime.block_on(go(args, inherited))
}

async fn go(args: &DaemonArgs, inherited: Option<Inherited>) -> ExitCode {
    let notify = args.notify_ready;
    let brain = match BrainName::parse(&args.brain) {
        Ok(b) => b,
        Err(e) => return fail(notify, &e.to_string()),
    };
    let paths = match Paths::from_env().and_then(|p| p.check_v1_allowed().map(|()| p)) {
        Ok(p) => p,
        Err(e) => return fail(notify, &e.to_string()),
    };
    let config = match load_config(&paths) {
        Ok(c) => c,
        Err(e) => return fail(notify, &e.to_string()),
    };
    let Ok(tz) = TimeZone::try_system() else {
        return fail(notify, "can't tell your time zone; set TZ");
    };
    let alarm = match WallAlarm::new() {
        Ok(a) => a,
        Err(e) => return fail(notify, &format!("can't set an alarm: {e}")),
    };
    let zone = match ZoneWatch::system() {
        Ok(z) => Some(z),
        Err(e) => {
            // No log yet; this goes to the service manager's. Moa runs without the watch.
            eprintln!(
                "moa can't watch for time-zone changes ({}); going on without",
                e.kind()
            );
            None
        }
    };
    let exe = match daemon_exe() {
        Ok(exe) => exe,
        Err(e) => return fail(notify, &format!("can't tell where moa runs from: {e}")),
    };
    let opts = RunOptions {
        paths,
        brain,
        config,
        tz,
        clock: std::sync::Arc::new(SystemClock::new()),
        alarm: Box::new(alarm),
        zone,
        ready: if notify {
            Ready::Stdout
        } else {
            Ready::Nothing
        },
        events_capacity: EVENTS_CAPACITY,
        uid_override: None,
        exe,
        build: build_id().to_string(),
        exec: Exec::Real,
        inherited,
        probe_wait: moa_daemon::ProbeWait::default(),
    };
    match run(opts).await {
        Ok(()) => ExitCode::SUCCESS,
        // Already said on stdout under --notify-ready; either way a second daemon is no
        // failure (D3).
        Err(e @ DaemonError::AlreadyRunning(_)) => {
            if !notify {
                eprintln!("{e}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Before `ready` run said it on stdout; this is the service manager's log (and
            // the only place a later failure shows besides the daemon's own).
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
