//! `moa panel [VIEW] [--brain B]`: a live view in any terminal (V15).

use std::path::Path;

use clap::Args;
use jiff::tz::TimeZone;
use moa_client::{OnMismatch, own_exe};
use moa_core::panel::View;
use moa_platform::current_uid;
use moa_tui::run::{PanelOptions, claim_pane, run};

use crate::ctl::{Here, pick_brain, runtime};

/// The views `moa panel` names when it doesn't know one.
const VIEWS: &str = "tree, plan, status, today, projects, activity, picture";

#[derive(Args)]
pub struct PanelArgs {
    /// The view: tree, plan, status, today, projects, activity or picture. Without one, the
    /// view this tab last had, else the picture.
    view: Option<String>,
    /// Which brain; the default is the one this folder belongs to.
    #[arg(long)]
    brain: Option<String>,
}

fn parse_view(word: &str) -> Result<View, String> {
    word.parse()
        .map_err(|_| format!("moa panel: no view called {word}; views: {VIEWS}"))
}

/// Runs the panel until it quits. Everything that can refuse does so before the terminal is
/// taken, as one line.
pub fn panel(args: &PanelArgs) -> Result<(), String> {
    let view = args.view.as_deref().map(parse_view).transpose()?;
    let here = Here::new()?;
    let config = here.config()?;
    // A panel isn't an agent's call: it only reads where the folder routes.
    let brain = pick_brain(&here, &config, args.brain.as_deref(), false)?;
    let exe = own_exe().map_err(|e| format!("can't find this program: {e}"))?;
    let socket = here
        .paths
        .tmux_socket(brain.as_str())
        .map_err(|e| e.to_string())?;
    let var = |k: &str| std::env::var(k).ok();
    let tmux_tmpdir = std::env::var_os("TMUX_TMPDIR").filter(|v| !v.is_empty());
    let pane = claim_pane(
        var("TMUX").as_deref(),
        var("TMUX_PANE").as_deref(),
        &socket,
        tmux_tmpdir.as_deref().map(Path::new),
        current_uid(),
    );
    let mut client = here.options(brain);
    client.client = "panel";
    // A panel moa placed itself never starts moa (it may have been stopped on purpose): its
    // first connect waits for moa as a reconnect does. One started by hand starts it.
    let placed = std::env::var_os(moa_core::cockpit::NO_AUTOSTART).is_some_and(|v| !v.is_empty());
    client.autostart = (!placed).then_some(exe);
    client.on_mismatch = OnMismatch::Accept;
    let opts = PanelOptions {
        client,
        view,
        pane,
        home: here.paths.home().to_path_buf(),
        tz: TimeZone::system(),
    };
    runtime()?.block_on(run(opts))
}
