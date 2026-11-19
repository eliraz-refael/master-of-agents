//! Activity (the spike's `Activity` view): what moa, you and the sessions did, the newest
//! first, one line each. Nothing on it is selectable; keys scroll it (V17).

use moa_core::model::ActivityWho;
use moa_core::time::local_format;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Line, Tone};
use crate::text::{clean, cut};
use crate::views::ViewUi;

/// How many rows the view lists, the newest.
pub const LIMIT: usize = 300;

/// Activity's lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, _ui: &ViewUi, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    data.activity
        .values()
        .rev()
        .take(LIMIT)
        .map(|row| {
            let at = local_format(row.at, &ctx.tz, "%H:%M");
            Line::plain(
                cut(&format!("{at} {}", clean(&row.text)), width),
                tone(row.who),
            )
        })
        .collect()
}

/// A row's tone, by who did it: moa, you, or a session.
pub fn tone(who: ActivityWho) -> Tone {
    match who {
        ActivityWho::Brain => Tone::Brain,
        ActivityWho::You => Tone::You,
        ActivityWho::Session => Tone::Dim,
    }
}
