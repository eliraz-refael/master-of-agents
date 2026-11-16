//! `moa service install|uninstall`: register a brain's daemon to start at login.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use moa_platform::service;

use crate::ctl::{Here, pick_brain};

#[derive(Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Start this brain's moa at login (launchd on macOS, systemd --user on Linux).
    Install(Target),
    /// Stop starting this brain's moa at login.
    Uninstall(Target),
}

#[derive(Args)]
struct Target {
    /// Which brain; the default is `default`.
    #[arg(long)]
    brain: Option<String>,
    /// The moa to register; the default is the profile's, else this program.
    #[arg(long)]
    exe: Option<PathBuf>,
}

pub fn service(args: &ServiceArgs) -> Result<(), String> {
    let here = Here::new()?;
    // A service is per brain and explicit, so no routing by folder.
    let (target, install) = match &args.action {
        Action::Install(t) => (t, true),
        Action::Uninstall(t) => (t, false),
    };
    here.paths.check_v1_allowed().map_err(|e| e.to_string())?;
    let config = here.read_config()?;
    let flag = target.brain.as_deref().unwrap_or("default");
    let brain = pick_brain(&here, &config, Some(flag), false)?;
    let outcome = if install {
        let exe = service::stable_exe(target.exe.as_deref(), here.paths.home())
            .map_err(|e| e.to_string())?;
        service::install(&here.paths, brain.as_str(), &exe)
    } else {
        service::uninstall(&here.paths, brain.as_str())
    }
    .map_err(|e| e.to_string())?;
    println!("{}", outcome.message);
    Ok(())
}
