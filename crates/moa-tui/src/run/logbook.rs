//! `panel.log` (L14): a panel's line per start, give-up, end and panic.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use moa_core::panel::View;
use moa_core::tmux::ids::PaneId;

use super::PanelOptions;

/// A `panel.log` line (L14), without its time: the brain, the view (`-` while none is
/// known; a `popup`'s says so, as `picker (popup)`), the pane (`-` outside the brain's
/// terminal), then what happened, on one line.
pub fn log_line(
    brain: &str,
    view: Option<View>,
    popup: bool,
    pane: Option<PaneId>,
    what: &str,
) -> String {
    let view = match (view, popup) {
        (None, _) => "-".to_string(),
        (Some(v), false) => v.to_string(),
        (Some(v), true) => format!("{v} (popup)"),
    };
    let pane = pane.map_or_else(|| "-".to_string(), |p| p.to_string());
    let what = what.replace(['\n', '\r'], " ");
    format!("[{brain}] panel {view} in {pane}: {what}")
}

/// Where a panel writes its lines, and what they name. Every write is best-effort: a log
/// that can't be written never stops the panel, and never panics (the panic hook writes
/// too).
#[derive(Clone, Debug)]
pub(super) struct Logbook {
    path: PathBuf,
    brain: String,
    pane: Option<PaneId>,
    /// A popup's panel (`--once`).
    popup: bool,
    /// The view the panel shows, once one is known; shared with the panic hook.
    view: Arc<Mutex<Option<View>>>,
}

impl Logbook {
    /// The panel's logbook, when `opts` names a log.
    pub(super) fn new(opts: &PanelOptions) -> Option<Logbook> {
        Some(Logbook {
            path: opts.log.clone()?,
            brain: opts.client.brain.as_str().to_string(),
            pane: opts.pane,
            popup: opts.once,
            view: Arc::new(Mutex::new(opts.view)),
        })
    }

    /// Appends `what` as a line, after the time. The folder isn't made: one that is gone
    /// stays gone (L15).
    pub(super) fn write(&self, what: &str) {
        // `try_lock`: the panic may have struck while the view was being set.
        let view = self.view.try_lock().ok().and_then(|v| *v);
        let line = log_line(&self.brain, view, self.popup, self.pane, what);
        // No `Timestamp::now`, which panics on a clock out of its range.
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map_or_else(|| "-".to_string(), |t| t.to_string());
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path);
        if let Ok(mut file) = file {
            let _ = file.write_all(format!("{at} {line}\n").as_bytes());
        }
    }

    /// The view the next lines name.
    pub(super) fn shows(&self, view: Option<View>) {
        if let Ok(mut shown) = self.view.lock() {
            *shown = view;
        }
    }
}

/// A panic's message, or a stand-in when it carries none.
pub(super) fn panic_message<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}
