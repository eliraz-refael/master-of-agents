//! `Deps::inspect`: the agent a tab runs, read in-process (C22) from the tab's process, its
//! direct children and the start command tmux kept (R8, R9). Nothing read here is logged.

use std::collections::VecDeque;

use moa_agents::claude;
use moa_core::model::{Inspected, PaneAgent};
use moa_platform::proc::{self, Reader, Unread};

use crate::clock::utc_ms;

/// The agent in the tab whose process is `pid` and whose start command is `start`: the start
/// command's view merged with the first of the process and its direct children that runs
/// Claude, whose start time is the agent's. A read that mattered failed is `Unknown`, whatever
/// the start command says: the agent it names has no start time, which a later look would
/// take for gone (R31). Otherwise a start command that runs Claude names the agent, and it
/// is `Gone` when every read answered (or the tab's process doesn't exist).
pub fn inspect_pane(pid: i64, start: &[String]) -> Inspected {
    let start_view = claude::pane_agent(start, &[]);
    let process_view = match u32::try_from(pid) {
        Ok(pid) => process_agent(pid),
        Err(_) => Inspected::Gone,
    };
    combine(start_view, process_view)
}

/// The start command's view of a tab's agent, `start_view`, with what its processes showed.
fn combine(start_view: Option<PaneAgent>, process_view: Inspected) -> Inspected {
    match (start_view, process_view) {
        (start, Inspected::Agent(process)) => {
            claude::merge(start, Some(process)).map_or(Inspected::Gone, Inspected::Agent)
        }
        (_, Inspected::Unknown) => Inspected::Unknown,
        (Some(start), Inspected::Gone) => Inspected::Agent(start),
        (None, Inspected::Gone) => Inspected::Gone,
    }
}

/// The tab's process, or else the first of its direct children, that runs Claude. A child
/// that exited mid-look isn't one, nor is another user's process: an agent moa takes on runs
/// as its user, and one moa can't read it couldn't adopt anyway. What runs under another
/// user's process is looked at too, as it may be an agent of ours. A read that failed
/// leaves the answer `Unknown` unless an agent turns up after it.
fn process_agent(pid: u32) -> Inspected {
    let mut reader = Reader::new();
    let agent_of = |p: u32| -> Result<Option<PaneAgent>, Unread> {
        let args = reader.args(p)?;
        Ok(claude::pane_agent(&args.argv, &args.env).map(|mut agent| {
            agent.started_at = proc::started(p).map(utc_ms);
            agent
        }))
    };
    process_agent_with(pid, agent_of, proc::children)
}

/// How deep a look goes below the tab's process through other users' processes: a `sudo`
/// and its monitor, with room to spare. One still another user's there proves nothing.
const FOREIGN_DEPTH: usize = 4;

/// [`process_agent`], with each process's agent read by `agent_of` and its children listed
/// by `children`.
fn process_agent_with(
    pid: u32,
    mut agent_of: impl FnMut(u32) -> Result<Option<PaneAgent>, Unread>,
    children: impl Fn(u32) -> Result<Vec<u32>, Unread>,
) -> Inspected {
    let mut failed = false;
    match agent_of(pid) {
        Ok(Some(agent)) => return Inspected::Agent(agent),
        Ok(None) | Err(Unread::Foreign) => {}
        Err(Unread::Gone) => return Inspected::Gone,
        Err(Unread::Failed) => failed = true,
    }
    // A child list fails only as gone or unread: `children` never says whose a process is.
    let mut todo: VecDeque<(u32, usize)> = match children(pid) {
        Ok(kids) => kids.into_iter().map(|k| (k, 1)).collect(),
        Err(Unread::Gone) => return Inspected::Gone,
        Err(_) => return Inspected::Unknown,
    };
    while let Some((p, depth)) = todo.pop_front() {
        match agent_of(p) {
            Ok(Some(agent)) => return Inspected::Agent(agent),
            Ok(None) | Err(Unread::Gone) => {}
            Err(Unread::Failed) => failed = true,
            // Another user's process (`sudo -u $USER claude`, a setuid wrapper) isn't an
            // agent, but one of ours may run under it.
            Err(Unread::Foreign) if depth < FOREIGN_DEPTH => match children(p) {
                Ok(kids) => todo.extend(kids.into_iter().map(|k| (k, depth + 1))),
                Err(Unread::Gone) => {}
                Err(_) => failed = true,
            },
            Err(Unread::Foreign) => failed = true,
        }
    }
    if failed {
        Inspected::Unknown
    } else {
        Inspected::Gone
    }
}

#[cfg(test)]
mod tests {
    use moa_core::model::AgentKind;

    use super::*;

    /// What each made-up process (by pid) reads as: 10 is the tab's, 11 and 12 its children.
    fn reads(
        tab: Result<Option<PaneAgent>, Unread>,
        kid_11: Result<Option<PaneAgent>, Unread>,
        kid_12: Result<Option<PaneAgent>, Unread>,
    ) -> Inspected {
        let mut by_pid = [(10, tab), (11, kid_11), (12, kid_12)];
        process_agent_with(
            10,
            |p| {
                let i = by_pid.iter().position(|(q, _)| *q == p).unwrap();
                std::mem::replace(&mut by_pid[i].1, Err(Unread::Gone))
            },
            |p| Ok(if p == 10 { vec![11, 12] } else { Vec::new() }),
        )
    }

    /// Another user's process is no agent of ours, the tab's or a child: with every other
    /// read answered, the tab runs none. A read that failed still proves nothing, and an
    /// agent found anywhere is the answer.
    #[test]
    fn another_users_processes_are_no_agent_of_ours() {
        let foreign = || Err(Unread::Foreign);
        assert_eq!(reads(foreign(), Ok(None), Ok(None)), Inspected::Gone);
        assert_eq!(reads(Ok(None), foreign(), Ok(None)), Inspected::Gone);
        assert_eq!(reads(foreign(), foreign(), foreign()), Inspected::Gone);
        assert_eq!(
            reads(foreign(), foreign(), Err(Unread::Failed)),
            Inspected::Unknown
        );
        let agent = started_claude().unwrap();
        assert_eq!(
            reads(foreign(), foreign(), Ok(Some(agent.clone()))),
            Inspected::Agent(agent)
        );
    }

    type Read = Result<Option<PaneAgent>, Unread>;
    type Kids = Result<Vec<u32>, Unread>;

    /// The tab whose process is pid 10, over made-up processes: what each pid reads as
    /// (any other is gone), and the children each lists (any other has none).
    fn tree(reads: Vec<(u32, Read)>, kids: Vec<(u32, Kids)>) -> Inspected {
        let mut reads = reads;
        process_agent_with(
            10,
            |p| match reads.iter().position(|(q, _)| *q == p) {
                Some(i) => reads.remove(i).1,
                None => Err(Unread::Gone),
            },
            |p| {
                kids.iter()
                    .find(|(q, _)| *q == p)
                    .map_or(Ok(Vec::new()), |(_, k)| k.clone())
            },
        )
    }

    /// Another user's process between the tab's shell and the agent (`sudo -u $USER claude`,
    /// a setuid wrapper) isn't an agent, but what runs under it is looked at: an agent there
    /// is the answer, and a list of its children that fails proves nothing.
    #[test]
    fn what_runs_under_another_users_process_is_looked_at() {
        let agent = started_claude().unwrap();
        let foreign = || Err(Unread::Foreign);
        // shell 10 → sudo 11 → claude 13.
        let found = tree(
            vec![
                (10, Ok(None)),
                (11, foreign()),
                (13, Ok(Some(agent.clone()))),
            ],
            vec![(10, Ok(vec![11])), (11, Ok(vec![13]))],
        );
        assert_eq!(found, Inspected::Agent(agent.clone()));
        // shell 10 → sudo 11 → sudo's monitor 12 → claude 13.
        let found = tree(
            vec![
                (10, Ok(None)),
                (11, foreign()),
                (12, foreign()),
                (13, Ok(Some(agent.clone()))),
            ],
            vec![(10, Ok(vec![11])), (11, Ok(vec![12])), (12, Ok(vec![13]))],
        );
        assert_eq!(found, Inspected::Agent(agent.clone()));
        // sudo's children can't be listed.
        let found = tree(
            vec![(10, Ok(None)), (11, foreign())],
            vec![(10, Ok(vec![11])), (11, Err(Unread::Failed))],
        );
        assert_eq!(found, Inspected::Unknown);
        // sudo exited while it was looked at; nothing else runs.
        let found = tree(
            vec![(10, Ok(None)), (11, foreign())],
            vec![(10, Ok(vec![11])), (11, Err(Unread::Gone))],
        );
        assert_eq!(found, Inspected::Gone);
        // A process of ours that isn't an agent is as deep as a look goes, as before.
        let found = tree(
            vec![(10, Ok(None)), (11, Ok(None)), (13, Ok(Some(agent)))],
            vec![(10, Ok(vec![11])), (11, Ok(vec![13]))],
        );
        assert_eq!(found, Inspected::Gone);
    }

    /// A start command that runs Claude, naming no conversation.
    fn started_claude() -> Option<PaneAgent> {
        let agent = claude::pane_agent(&["claude".to_string()], &[]);
        assert!(agent.as_ref().is_some_and(|a| a.kind == AgentKind::Claude));
        agent
    }

    /// A failed read proves nothing, whatever the start command says: the agent it would name
    /// has no start time, and a later look would take that for gone (R31).
    #[test]
    fn a_failed_read_stays_unknown_whatever_the_start_command_says() {
        assert_eq!(
            combine(started_claude(), Inspected::Unknown),
            Inspected::Unknown
        );
        assert_eq!(combine(None, Inspected::Unknown), Inspected::Unknown);
    }

    /// Reads that answered: the start command still names the agent when no process does.
    #[test]
    fn reads_that_answered_go_by_the_start_command() {
        assert_eq!(
            combine(started_claude(), Inspected::Gone),
            Inspected::Agent(started_claude().unwrap())
        );
        assert_eq!(combine(None, Inspected::Gone), Inspected::Gone);
    }
}
