//! moa's terminal client: the palette, each `moa_view::Screen` drawn into a ratatui buffer,
//! and the loop that runs a panel in a terminal (`moa panel`).

pub mod draw;
pub mod run;
pub mod theme;
