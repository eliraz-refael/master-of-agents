//! What every moa client shows, without the drawing: the typed mirror of the daemon's topics,
//! the clock a frame is built at, text measured by display width, the lines a view is made
//! of, scrolling, the keymap and the footer's editor, each view's builder, and the panel that
//! turns input into ops and frames. No IO.

pub mod ctx;
pub mod data;
pub mod editor;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod geometry;
pub mod keys;
pub mod line;
pub mod list;
pub mod markdown;
pub mod panel;
pub mod text;
pub mod views;
