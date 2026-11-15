use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod adopt;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::AdoptSpike(args)) => adopt::adopt(&args),
        Some(Command::ExportSpike(args)) => adopt::export(&args),
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
