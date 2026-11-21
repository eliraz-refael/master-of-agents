//! `moa panel [VIEW] [--brain B]`: a live view in any terminal (V15).

use std::io::Write;
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
/// Every panel's log, in moa's log folder: a line per start, give-up, signal and panic (L14).
const PANEL_LOG: &str = "panel.log";

#[derive(Args)]
pub struct PanelArgs {
    /// The view: tree, plan, status, today, projects, activity or picture. Without one, the
    /// view this tab last had, else the picture.
    view: Option<String>,
    /// Which brain; the default is the one this folder belongs to.
    #[arg(long)]
    brain: Option<String>,
    /// Draw in a popup and end when it closes: the picker or help, as Ctrl-a's popups do.
    #[arg(long)]
    once: bool,
}

#[derive(Args)]
pub struct PickArgs {
    /// Which brain; the default is the one this folder belongs to.
    #[arg(long)]
    brain: Option<String>,
}

/// What `--once` says to a view other than the picker and help.
const ONCE_VIEWS: &str = "moa panel: --once works only for the picker and help";

fn parse_view(word: &str) -> Result<View, String> {
    word.parse()
        .map_err(|_| format!("moa panel: no view called {word}; views: {VIEWS}"))
}

/// Whether moa placed this panel itself: `MOA_NO_AUTOSTART` is exactly `1`, as moa sets it.
fn placed_by_moa(mark: Option<&std::ffi::OsStr>) -> bool {
    mark.is_some_and(|v| v == "1")
}

/// Runs the panel until it quits. Everything that can refuse does so before the terminal is
/// taken, as one line.
pub fn panel(args: &PanelArgs) -> Result<(), String> {
    let view = args.view.as_deref().map(parse_view).transpose()?;
    if args.once && !matches!(view, Some(View::Picker | View::Help)) {
        return Err(ONCE_VIEWS.to_string());
    }
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
    // A popup claims no tab: the one under it has a panel or an agent of its own (S8).
    let pane = (!args.once)
        .then(|| {
            claim_pane(
                var("TMUX").as_deref(),
                var("TMUX_PANE").as_deref(),
                &socket,
                tmux_tmpdir.as_deref().map(Path::new),
                current_uid(),
            )
        })
        .flatten();
    let mut client = here.options(brain);
    client.client = "panel";
    // A panel moa placed itself never starts moa (it may have been stopped on purpose): its
    // first connect waits for moa as a reconnect does. One started by hand starts it.
    let placed = placed_by_moa(std::env::var_os(moa_core::cockpit::NO_AUTOSTART).as_deref());
    client.autostart = (!placed).then_some(exe);
    client.on_mismatch = OnMismatch::Accept;
    let opts = PanelOptions {
        client,
        view,
        pane,
        home: here.paths.home().to_path_buf(),
        tz: TimeZone::system(),
        // Beside moa's own logs: what `see panel.log` points to.
        log: Some(here.paths.log_dir().join(PANEL_LOG)),
        once: args.once,
    };
    let runtime = runtime()?;
    let ended = runtime.block_on(run(opts));
    if args.once {
        // A popup exits here, its runtime and its moa connection still alive (its loop
        // leaves the connection open): the connection then closes in the same exit that
        // closes the popup's terminal. moa hears the popup go only once tmux can reap it,
        // so the reap nudge moa sends then on tmux 3.4 and 3.5 comes after the exit, not
        // before it (S8). The terminal is given back and the end line written by now.
        let code = match ended {
            Ok(()) => 0,
            Err(message) => {
                // Not `eprintln!`, which panics when the terminal is gone.
                let _ = writeln!(std::io::stderr(), "{message}");
                1
            }
        };
        std::process::exit(code);
    }
    ended
}

/// `moa pick`: `moa panel picker --once`, the picker as `Ctrl-a /`'s popup runs it.
pub fn pick(args: &PickArgs) -> Result<(), String> {
    panel(&PanelArgs {
        view: Some(View::Picker.word().to_string()),
        brain: args.brain.clone(),
        once: true,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::placed_by_moa;

    /// `MOA_NO_AUTOSTART` marks a panel moa placed only when it is exactly `1`.
    #[test]
    fn only_exactly_1_marks_a_placed_panel() {
        assert!(placed_by_moa(Some(OsStr::new("1"))));
        for v in [
            None,
            Some(""),
            Some("0"),
            Some("yes"),
            Some("1 "),
            Some("11"),
        ] {
            assert!(!placed_by_moa(v.map(OsStr::new)), "{v:?}");
        }
    }
}
