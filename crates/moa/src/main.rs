use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use moa_proto::cli::{Moactl, VERB_NAMES};

mod adopt;
mod ctl;
mod daemon;
mod service;
mod sql;

#[derive(Parser)]
#[command(name = "moa", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Import a spike brain into a v1 brain. Without --into it is a dry run.
    AdoptSpike(adopt::AdoptArgs),
    /// Write a v1 brain back out in the spike's format.
    ExportSpike(adopt::ExportArgs),
    /// Run the daemon for a brain (started by the other commands, or by launchd/systemd).
    Daemon(daemon::DaemonArgs),
    /// Say whether the brain's moa is running, and who you are to it.
    Status(ctl::BrainArgs),
    /// Stop the brain's moa.
    Shutdown(ctl::BrainArgs),
    /// A read-only SQL shell on a brain: one statement, or one per line on stdin.
    Sql(sql::SqlArgs),
    /// Start a brain's moa at login: `service install` or `service uninstall`.
    Service(service::ServiceArgs),
}

/// Whether this is a `moactl` call: run as `moactl`, or `moa` followed by a verb (the first
/// word that isn't a flag, so `moa --json ls` works too).
fn is_moactl(args: &[OsString]) -> bool {
    let named_moactl = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|n| n == "moactl");
    named_moactl
        || args
            .iter()
            .skip(1)
            .find(|a| !a.to_string_lossy().starts_with('-'))
            .is_some_and(|a| VERB_NAMES.contains(&&*a.to_string_lossy()))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if is_moactl(&args) {
        // `moa ls` is `moactl ls`: clap sees the name `moactl` either way.
        let rest = args.iter().skip(1).cloned();
        return ctl::run(Moactl::parse_from(
            std::iter::once(OsString::from("moactl")).chain(rest),
        ));
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::AdoptSpike(args)) => adopt::adopt(&args),
        Some(Command::ExportSpike(args)) => adopt::export(&args),
        Some(Command::Daemon(args)) => return daemon::daemon(&args),
        Some(Command::Status(args)) => return ctl::status(&args),
        Some(Command::Shutdown(args)) => return ctl::shutdown(&args),
        Some(Command::Sql(args)) => sql::sql(&args),
        Some(Command::Service(args)) => service::service(&args),
        None => {
            eprintln!("moa v1 is still being built; use the spike's moa for now.");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}
