//! What every moa client shows, without the drawing: the typed mirror of the daemon's topics,
//! the clock a frame is built at, and text measured by display width. No IO.

pub mod ctx;
pub mod data;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod text;
