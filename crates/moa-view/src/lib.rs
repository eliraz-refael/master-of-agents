//! What every moa client shows, without the drawing: the typed mirror of the daemon's topics,
//! the clock a frame is built at, text measured by display width, the lines a view is made
//! of, scrolling, the keymap and the footer's editor. No IO.

pub mod ctx;
pub mod data;
pub mod editor;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod keys;
pub mod line;
pub mod list;
pub mod text;
