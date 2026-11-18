//! One builder per view: the data, the clock and the panel's own state in, the view's lines
//! out (V2), built for the body's width.

use std::collections::BTreeSet;

use moa_core::live::DisplayState;
use moa_core::model::ProjectId;
use moa_core::panel::View;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::Line;
use crate::text::SPIN;

pub mod tree;

/// What a view keeps in the panel rather than in the brain: the projects opened in the
/// projects view, and the tab the picture shows.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ViewUi {
    pub expanded: BTreeSet<ProjectId>,
    pub tab: usize,
}

/// The lines of `view`, `width` cells wide.
pub fn build(view: View, data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    match view {
        View::Tree => tree::build(data, ctx, ui, width),
        // The other views come with their builders.
        _ => Vec::new(),
    }
}

/// The spinner's frame at `tick`.
pub fn spinner(tick: u64) -> &'static str {
    SPIN[(tick % SPIN.len() as u64) as usize]
}

/// A state's glyph at `tick`: working spins and waiting blinks every half second, `!` then
/// `⚠`, as the spike's did (`"⚠" if int(t * 2) % 2 else "!"`); the rest are still.
pub fn state_glyph(state: DisplayState, tick: u64) -> &'static str {
    match state {
        DisplayState::Working => spinner(tick),
        DisplayState::Waiting if (tick / 5) % 2 == 1 => "⚠",
        DisplayState::Waiting => "!",
        _ => state.glyph(),
    }
}

/// Whether a state's glyph moves.
pub fn animates(state: DisplayState) -> bool {
    matches!(state, DisplayState::Working | DisplayState::Waiting)
}

/// A session running now, in any state (the spike's `LIVE`, with v1's question, failed,
/// done and quiet).
pub fn live(state: DisplayState) -> bool {
    matches!(
        state,
        DisplayState::Waiting
            | DisplayState::Question
            | DisplayState::Failed
            | DisplayState::Done
            | DisplayState::Working
            | DisplayState::Quiet
            | DisplayState::Idle
    )
}
