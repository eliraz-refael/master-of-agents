use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "moa", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(command) => match command {},
        None => {
            eprintln!("moa v1 is still being built; use the spike's moa for now.");
            ExitCode::from(2)
        }
    }
}

fn  badly_formatted( ) {}
