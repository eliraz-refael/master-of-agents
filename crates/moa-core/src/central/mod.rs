//! moa's own agent, the central one: the core's side of its conversation. No IO of its own:
//! the files it is told (`prompt`) are read through `Deps::read_text`, as the layout ops do.

pub mod feed;
pub mod launch;
pub mod prompt;
pub mod wire;

use crate::publish::MoaRow;

/// Where moa's agent is.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    #[default]
    Off,
    Starting,
    Ready,
    Thinking,
    Failed(String),
}

/// The core's state for moa's agent: its phase and which run of it (`generation`) the core is
/// listening to. The rest of what the conversation needs is added where it is used.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Moa {
    pub phase: Phase,
    pub generation: u64,
}

impl Moa {
    /// The `state` row of the `feed` topic.
    pub fn row(&self) -> MoaRow {
        let (phase, why) = match &self.phase {
            Phase::Off => ("off", None),
            Phase::Starting => ("starting", None),
            Phase::Ready => ("ready", None),
            Phase::Thinking => ("thinking", None),
            Phase::Failed(why) => ("failed", Some(why.clone())),
        };
        MoaRow {
            phase: phase.into(),
            thread: None,
            why,
        }
    }
}
