use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use moa_proto::cli::{Moactl, VERB_NAMES};

mod adopt;
mod ctl;
mod daemon;
mod front;
mod service;
mod sql;

#[derive(Parser)]
#[command(name = "moa", version, args_conflicts_with_subcommands = true)]
struct Cli {
    /// The brain to open; the default is `default`.
    brain: Option<String>,
    /// Open a moa of its own under ~/.local/state/moa-sandbox, apart from your real one.
    #[arg(long)]
    sandbox: bool,
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
    Status(ctl::StatusArgs),
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
            return front::front(&front::FrontArgs {
                brain: cli.brain,
                sandbox: cli.sandbox,
            });
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use moa_core::config::RESERVED_BRAIN_NAMES;

    use super::*;

    /// T18: `moa <subcommand>` can't also open a brain of that name.
    #[test]
    fn every_verb_and_subcommand_is_reserved() {
        let mut cli = Cli::command();
        cli.build();
        let names: Vec<&str> = cli.get_subcommands().map(|c| c.get_name()).collect();
        assert!(names.contains(&"help"), "{names:?}");
        for name in names {
            assert!(
                RESERVED_BRAIN_NAMES.contains(&name),
                "{name} isn't reserved"
            );
        }
    }

    #[test]
    fn a_brain_or_a_subcommand_but_not_both() {
        let cli = Cli::try_parse_from(["moa", "work"]).unwrap();
        assert_eq!(cli.brain.as_deref(), Some("work"));
        assert!(cli.command.is_none() && !cli.sandbox);
        let cli = Cli::try_parse_from(["moa", "--sandbox"]).unwrap();
        assert!(cli.sandbox && cli.brain.is_none());
        let cli = Cli::try_parse_from(["moa", "status"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Status(_))));
        assert!(cli.brain.is_none());
        assert!(Cli::try_parse_from(["moa", "work", "status"]).is_err());
        // After a flag only a brain can follow; the front door points to the command.
        let cli = Cli::try_parse_from(["moa", "--sandbox", "status"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.brain.as_deref(), Some("status"));
    }
}
