//! `Deps::inspect`: the agent a tab runs, read in-process (C22) from the tab's process, its
//! direct children and the start command tmux kept (R8, R9). Nothing read here is logged.

use moa_agents::claude;
use moa_core::model::PaneAgent;
use moa_platform::proc::{self, Reader};

use crate::clock::utc_ms;

/// The agent in the tab whose process is `pid` and whose start command is `start`: the start
/// command's view merged with the first of the process and its direct children that runs
/// Claude, whose start time is the agent's.
pub fn inspect_pane(pid: i64, start: &[String]) -> Option<PaneAgent> {
    let start_view = claude::pane_agent(start, &[]);
    let process_view = u32::try_from(pid).ok().and_then(process_agent);
    claude::merge(start_view, process_view)
}

/// The tab's process, or else the first of its direct children, that runs Claude.
fn process_agent(pid: u32) -> Option<PaneAgent> {
    let mut reader = Reader::new();
    let mut agent_of = |p: u32| {
        let args = reader.args(p)?;
        let mut agent = claude::pane_agent(&args.argv, &args.env)?;
        agent.started_at = proc::started(p).map(utc_ms);
        Some(agent)
    };
    agent_of(pid).or_else(|| proc::children(pid).into_iter().find_map(agent_of))
}
