//! moa's terminal client: the palette, and each `moa_view::Screen` drawn into a ratatui
//! buffer. The terminal and its loop come later; nothing here does IO.

pub mod draw;
pub mod theme;
