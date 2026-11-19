//! The picture (the spike's `picture.py` without `--panel`): six tabs over the other views,
//! the `moa` brand at the right of the tabs, and the activity strip under a rule.

use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::ActivityRow;
use moa_core::panel::View;
use moa_core::time::local_format;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Line, Span, Tone};
use crate::text::{clean, cut, width};
use crate::views::{self, ViewUi, activity, spinner};

/// The tabs, in the spike's order, with the view each shows.
pub const TABS: [(&str, View); 6] = [
    ("Plan", View::Plan),
    ("Today", View::Today),
    ("Projects", View::Projects),
    ("Sessions", View::Tree),
    ("Activity", View::Activity),
    ("Status", View::Status),
];

/// How long a new activity row flashes, in milliseconds (the spike's `fresh`).
pub const FLASH: i64 = 4_000;

/// The view the shown tab draws.
pub fn tab_view(ui: &ViewUi) -> View {
    TABS[ui.tab % TABS.len()].1
}

/// The tabs' names.
pub fn tab_names() -> Vec<&'static str> {
    TABS.iter().map(|(name, _)| *name).collect()
}

/// The tab row as the spike wrote it: the shown tab in brackets, the others between spaces,
/// two spaces apart.
pub fn tabs_text(shown: usize) -> String {
    TABS.iter()
        .enumerate()
        .map(|(i, (name, _))| {
            if i == shown {
                format!("[{name}]")
            } else {
                format!(" {name} ")
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// The shown tab's lines, `width` cells wide.
pub fn build(data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    views::build(tab_view(ui), data, ctx, ui, width)
}

/// Where a row's flash stands at `now`.
enum Flash {
    /// Under 4 s old: it flashes until then.
    On {
        ends: UtcMs,
    },
    /// From a moment ahead of the clock: it flashes from then.
    Ahead {
        starts: UtcMs,
    },
    Off,
}

fn flash(row: &ActivityRow, now: UtcMs) -> Flash {
    if row.at > now {
        Flash::Ahead { starts: row.at }
    } else if now.0.saturating_sub(row.at.0) < FLASH {
        Flash::On {
            ends: UtcMs(row.at.0.saturating_add(FLASH)),
        }
    } else {
        Flash::Off
    }
}

impl Flash {
    fn changes_at(&self) -> Option<UtcMs> {
        match *self {
            Flash::On { ends } => Some(ends),
            Flash::Ahead { starts } => Some(starts),
            Flash::Off => None,
        }
    }

    fn on(&self) -> bool {
        matches!(self, Flash::On { .. })
    }
}

/// The newest `n` activity rows, oldest first.
fn newest(data: &Data, n: usize) -> Vec<&ActivityRow> {
    let mut rows: Vec<&ActivityRow> = data.activity.values().rev().take(n).collect();
    rows.reverse();
    rows
}

/// The strip's `rows` lines, `width` cells wide: the newest activity rows, oldest first, as
/// `HH:MM  <text>`; a row under 4 s old reads `HH:MM ✦<text>` in bold, reversed on every
/// other 250 ms, and moves until its flash ends.
pub fn strip(data: &Data, ctx: &Ctx, width: u16, rows: u16) -> Vec<Line> {
    newest(data, usize::from(rows))
        .into_iter()
        .map(|row| {
            let flash = flash(row, ctx.now);
            let at = local_format(row.at, &ctx.tz, "%H:%M");
            let mark = if flash.on() { "✦" } else { " " };
            let text = cut(
                &format!("{at} {mark}{}", clean(&row.text)),
                usize::from(width),
            );
            let mut span = Span::styled(text, activity::tone(row.who), flash.on());
            // The spike's `int(time.monotonic() * 4) % 2`, with a tick of 100 ms.
            span.reversed = flash.on() && (u128::from(ctx.tick) * 2 / 5) % 2 == 1;
            let mut line = Line::new(vec![span]);
            line.animated = flash.on();
            line.changes_at = flash.changes_at();
            line
        })
        .collect()
}

/// The rule above the strip: `─ activity ───…`, `width` cells wide.
pub fn rule(width: u16) -> Line {
    let width = usize::from(width);
    Line::plain(
        cut(&format!("─ activity {}", "─".repeat(width)), width),
        Tone::Dim,
    )
}

/// The brand (V21): `moa <spinner>` while any session is working or waiting or one of the
/// strip's rows flashes, else `moa ●`; bold, in the brain's colour. It changes when a flash
/// ends or starts.
pub fn brand(data: &Data, ctx: &Ctx) -> Line {
    let rows = newest(data, usize::from(crate::geometry::STRIP));
    let flashes: Vec<Flash> = rows.iter().map(|row| flash(row, ctx.now)).collect();
    let busy = data.sessions.values().any(|r| {
        matches!(
            r.display_state,
            DisplayState::Working | DisplayState::Waiting
        )
    });
    let spins = busy || flashes.iter().any(Flash::on);
    let mark = if spins { spinner(ctx.tick) } else { "●" };
    let mut line = Line::new(vec![Span::bold(format!("moa {mark}"), Tone::Brain)]);
    line.animated = spins;
    line.changes_at = flashes.iter().filter_map(Flash::changes_at).min();
    line
}

/// Whether the brand fits beside the tabs on a row `w` cells wide (the spike's test).
pub fn brand_fits(shown: usize, brand: &Line, w: u16) -> bool {
    usize::from(w) > width(&tabs_text(shown)) + width(&brand.text()) + 2
}
