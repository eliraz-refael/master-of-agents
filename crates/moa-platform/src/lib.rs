//! moa's platform layer: paths, the build id, the counting spawner.

pub mod build_id;
pub mod paths;
pub mod spawn;
#[allow(unsafe_code)]
mod sys;

pub use build_id::{build_id, build_id_of, canonical, fnv1a64};
pub use paths::{Paths, PathsError, ensure_private_dir};
pub use spawn::Spawner;
