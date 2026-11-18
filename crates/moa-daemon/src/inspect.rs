//! `Deps::inspect`: the agent a tab runs, read in-process (C22) from the tab's process, its
//! direct children and the start command tmux kept (R8, R9). Nothing read here is logged.

use moa_agents::claude;
use moa_core::model::{Inspected, PaneAgent};
use moa_platform::proc::{self, Reader, Unread};

use crate::clock::utc_ms;

/// The agent in the tab whose process is `pid` and whose start command is `start`: the start
/// command's view merged with the first of the process and its direct children that runs
/// Claude, whose start time is the agent's. A start command that runs Claude names the
/// agent whatever the processes say. Otherwise it is `Gone` only when every read answered
/// (or the tab's process doesn't exist), and `Unknown` when one that mattered failed.
pub fn inspect_pane(pid: i64, start: &[String]) -> Inspected {
    let start_view = claude::pane_agent(start, &[]);
    let process_view = match u32::try_from(pid) {
        Ok(pid) => process_agent(pid),
        Err(_) => Inspected::Gone,
    };
    match (start_view, process_view) {
        (start, Inspected::Agent(process)) => {
            claude::merge(start, Some(process)).map_or(Inspected::Gone, Inspected::Agent)
        }
        (Some(start), _) => Inspected::Agent(start),
        (None, other) => other,
    }
}

/// The tab's process, or else the first of its direct children, that runs Claude. A child
/// that exited mid-look isn't one; a read that failed leaves the answer `Unknown` unless an
/// agent turns up after it.
fn process_agent(pid: u32) -> Inspected {
    let mut reader = Reader::new();
    let mut agent_of = |p: u32| -> Result<Option<PaneAgent>, Unread> {
        let args = reader.args(p)?;
        Ok(claude::pane_agent(&args.argv, &args.env).map(|mut agent| {
            agent.started_at = proc::started(p).map(utc_ms);
            agent
        }))
    };
    let mut failed = false;
    match agent_of(pid) {
        Ok(Some(agent)) => return Inspected::Agent(agent),
        Ok(None) => {}
        Err(Unread::Gone) => return Inspected::Gone,
        Err(Unread::Failed) => failed = true,
    }
    let kids = match proc::children(pid) {
        Ok(kids) => kids,
        Err(Unread::Gone) => return Inspected::Gone,
        Err(Unread::Failed) => return Inspected::Unknown,
    };
    for kid in kids {
        match agent_of(kid) {
            Ok(Some(agent)) => return Inspected::Agent(agent),
            Ok(None) | Err(Unread::Gone) => {}
            Err(Unread::Failed) => failed = true,
        }
    }
    if failed {
        Inspected::Unknown
    } else {
        Inspected::Gone
    }
}
