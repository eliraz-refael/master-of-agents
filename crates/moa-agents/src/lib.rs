//! What moa knows about the agents it runs. Std only: no IO beyond file existence checks,
//! except the host of moa's own agent (feature `acp`).

#[cfg(feature = "acp")]
pub mod acp;
pub mod claude;
