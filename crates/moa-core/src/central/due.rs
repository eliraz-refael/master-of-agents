//! What's due (D4, C20): the open tasks due by tomorrow that moa hasn't been told of today.
//! A wake-up carries them ([`super::prompt`]), and its commit saves a `nags` row for each, so
//! each (task, due date, day) reaches moa once. They never go on their own (U18).

use jiff::civil::Date;

use super::reports::heading_field;
use crate::Now;
use crate::change::Change;
use crate::model::{Nag, Task};
use crate::state::State;

/// The most tasks one prompt carries; the rest go the next time.
pub const DUE_MAX: usize = 20;

/// Every open task due by tomorrow, its (due date, today) untold, by due date then id.
fn untold<'a>(state: &'a State, now: &Now) -> Vec<(Date, &'a Task)> {
    let tomorrow = now.local_day.tomorrow().unwrap_or(Date::MAX);
    let mut due: Vec<(Date, &Task)> = state
        .tasks()
        .filter(|t| t.status.is_open())
        .filter_map(|t| t.due.map(|d| (d, t)))
        .filter(|(d, t)| *d <= tomorrow && !state.nagged(t.id, *d, now.local_day))
        .collect();
    due.sort_by_key(|(d, t)| (*d, t.id));
    due
}

/// The tasks a prompt now carries: open, due by tomorrow, untold today; due date then id
/// first, at most [`DUE_MAX`].
pub fn due_now<'a>(state: &'a State, now: &Now) -> Vec<&'a Task> {
    let due = untold(state, now).into_iter().take(DUE_MAX);
    due.map(|(_, t)| t).collect()
}

/// The due block (D10) of `due`: a line for each lateness, its tasks joined by `; `, each
/// title one line of at most 80 characters; then how many more wait, if any.
pub fn block(state: &State, now: &Now, due: &[&Task]) -> String {
    let mut lines: Vec<(String, Vec<String>)> = Vec::new();
    for t in due {
        let Some(d) = t.due else { continue };
        let label = lateness(now.local_day, d);
        let title = heading_field(&t.title);
        match lines.last_mut() {
            Some((l, titles)) if *l == label => titles.push(title),
            _ => lines.push((label, vec![title])),
        }
    }
    let mut text = "Due by tomorrow (end your reply with these lines as they are):".to_string();
    for (label, titles) in lines {
        text += &format!("\n⏰ {label}: {}", titles.join("; "));
    }
    let more = untold(state, now).len().saturating_sub(due.len());
    if more > 0 {
        text += &format!("\n(and {more} more: moactl task ls)");
    }
    text
}

/// How late a task due on `due` is on `today`: `N days overdue`, `1 day overdue`, `Due
/// today` or `Due tomorrow`.
fn lateness(today: Date, due: Date) -> String {
    let days = (today - due).get_days();
    match days {
        ..=-1 => "Due tomorrow".to_string(),
        0 => "Due today".to_string(),
        1 => "1 day overdue".to_string(),
        n => format!("{n} days overdue"),
    }
}

/// A `nags` row for each of `due`, told on `now`'s day, for the commit of the prompt that
/// carries them.
pub(crate) fn told(now: &Now, due: &[&Task]) -> Vec<Change> {
    let nag = |t: &&Task| {
        t.due.map(|due| {
            Change::Nag(Nag {
                task_id: t.id,
                due,
                day: now.local_day,
            })
        })
    };
    due.iter().filter_map(nag).collect()
}
