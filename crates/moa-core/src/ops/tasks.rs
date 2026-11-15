//! `task ls|add|set` and `note`.

use std::fmt::Display;
use std::str::FromStr;

use jiff::civil::Date;
use serde_json::json;

use super::{brain_row, project_named};
use crate::Now;
use crate::activity;
use crate::change::Change;
use crate::core::Tx;
use crate::due::{Due, parse_due};
use crate::model::{Note, NoteId, NoteKind, Priority, SessionId, Task, TaskId, TaskStatus, Who};
use crate::op::{Done, OpError, TaskAdd, TaskSet};
use crate::state::{State, Table};

/// `Some("")` and `None` are both "nothing".
fn non_empty(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|t| !t.is_empty())
}

/// Case doesn't matter; the error lists the choices.
fn parse_enum<T: FromStr + Display>(
    text: &str,
    flag: &str,
    all: &[T],
    tail: &str,
) -> Result<T, OpError> {
    text.trim().to_lowercase().parse().map_err(|_| {
        let names: Vec<String> = all.iter().map(ToString::to_string).collect();
        OpError(format!(
            "{flag}: '{text}' isn't one of {}{tail}",
            names.join(", ")
        ))
    })
}

fn parse_status(text: &str) -> Result<TaskStatus, OpError> {
    parse_enum(text, "--status", TaskStatus::ALL, "")
}

fn parse_who(text: &str) -> Result<Who, OpError> {
    parse_enum(text, "--who", Who::ALL, "")
}

fn parse_priority(text: &str) -> Result<Priority, OpError> {
    parse_enum(text, "--priority", Priority::ALL, " (or empty for none)")
}

fn parse_due_text(text: &str, now: &Now) -> Result<Option<Date>, OpError> {
    match parse_due(text, now.local_day).map_err(|e| OpError(e.to_string()))? {
        Due::Clear => Ok(None),
        Due::On(d) => Ok(Some(d)),
    }
}

/// The row, with its project's name beside it.
fn row_json(state: &State, t: &Task) -> serde_json::Value {
    let mut v = serde_json::to_value(t).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        let name = t
            .project_id
            .and_then(|p| state.projects.get(&p))
            .map(|p| p.name.clone());
        o.insert("project".into(), json!(name));
    }
    v
}

pub(crate) fn ls(
    state: &State,
    now: &Now,
    project: Option<&str>,
    all: bool,
) -> Result<Done, OpError> {
    let want = project.map(str::to_lowercase);
    let mut rows: Vec<(Option<&str>, &Task)> = state
        .tasks()
        .filter(|t| all || !matches!(t.status, TaskStatus::Done | TaskStatus::Dropped))
        .map(|t| {
            let name = t
                .project_id
                .and_then(|p| state.projects.get(&p))
                .map(|p| p.name.as_str());
            (name, t)
        })
        .filter(|(name, _)| match &want {
            Some(w) => name.is_some_and(|n| n.to_lowercase() == *w),
            None => true,
        })
        .collect();
    // No project first, then by name in byte order, then by id.
    rows.sort_by_key(|(name, t)| (*name, t.id));
    if rows.is_empty() {
        return Ok(Done {
            text: "no open tasks".into(),
            data: json!([]),
        });
    }
    let mut lines = Vec::new();
    let mut last: Option<Option<&str>> = None;
    for (name, t) in &rows {
        if last != Some(*name) {
            lines.push(format!("## {}", name.unwrap_or("-")));
            last = Some(*name);
        }
        let mut extra: Vec<String> = Vec::new();
        if let Some(p) = t.priority {
            extra.push(p.as_str().to_uppercase());
        }
        if t.today == Some(now.local_day) {
            extra.push("today".into());
        }
        if let Some(w) = t.who {
            extra.push(format!("who={w}"));
        }
        if let Some(d) = t.due {
            extra.push(format!("due={d}"));
        }
        if let Some(b) = non_empty(&t.blocked_on) {
            extra.push(format!("on={b}"));
        }
        if let Some(s) = t.session_id {
            extra.push(format!("session={}", state.handle_of(s)));
        }
        let tail = if extra.is_empty() {
            String::new()
        } else {
            format!("  ({})", extra.join(", "))
        };
        lines.push(format!("- #{} [{}] {}{tail}", t.id, t.status, t.title));
    }
    Ok(Done {
        text: lines.join("\n"),
        data: serde_json::Value::Array(rows.iter().map(|(_, t)| row_json(state, t)).collect()),
    })
}

pub(crate) fn add(state: &mut State, tx: &mut Tx, now: &Now, a: &TaskAdd) -> Result<Done, OpError> {
    let project = project_named(state, &a.project).ok_or_else(|| {
        OpError(format!(
            "no project '{}' (moactl project NAME to check)",
            a.project
        ))
    })?;
    let (project_id, project_name) = (project.id, project.name.clone());
    if a.title.trim().is_empty() {
        return Err(OpError("a task needs a title".into()));
    }
    let blocked_on = non_empty(&a.blocked_on).map(str::to_string);
    let status = match &a.status {
        Some(s) => parse_status(s)?,
        None if blocked_on.is_some() => TaskStatus::Blocked,
        None => TaskStatus::Todo,
    };
    let who = match non_empty(&a.who) {
        Some(w) => parse_who(w)?,
        None => Who::You,
    };
    let priority = match non_empty(&a.priority) {
        Some(p) => Some(parse_priority(p)?),
        None => None,
    };
    let due = match &a.due {
        Some(d) => parse_due_text(d, now)?,
        None => None,
    };
    let session_id: Option<SessionId> = match non_empty(&a.session) {
        Some(q) => Some(state.resolve(q)?),
        None => None,
    };
    let id = TaskId(tx.next_id(state, Table::Task));
    let task = Task {
        id,
        project_id: Some(project_id),
        title: a.title.clone(),
        status,
        blocked_on,
        today: a.today.then_some(now.local_day),
        session_id,
        note: non_empty(&a.note).map(str::to_string),
        who: Some(who),
        due,
        priority,
        created_at: now.wall,
        updated_at: now.wall,
        done_at: (status == TaskStatus::Done).then_some(now.wall),
    };
    let row = brain_row(
        state,
        tx,
        now,
        activity::new_task(&task.title, Some(&project_name)),
    );
    let data = row_json(state, &task);
    tx.commit(state, vec![Change::Task(task), row])?;
    Ok(Done {
        text: format!("task #{id} added"),
        data,
    })
}

pub(crate) fn set(state: &mut State, tx: &mut Tx, now: &Now, a: &TaskSet) -> Result<Done, OpError> {
    let old = state
        .tasks
        .get(&TaskId(a.id))
        .cloned()
        .ok_or_else(|| OpError(format!("no task #{}", a.id)))?;
    if a.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err(OpError("--title can't be empty".into()));
    }
    // Every task has someone on it, so an empty `--who` is refused rather than ignored.
    if a.who.as_deref().is_some_and(|w| w.trim().is_empty()) {
        return Err(OpError(
            "--who can't be empty; a task always has someone on it".into(),
        ));
    }
    // Every value is checked before any is applied.
    let given_status = a.status.as_deref().map(parse_status).transpose()?;
    let who = a.who.as_deref().map(parse_who).transpose()?;
    let priority = match a.priority.as_deref() {
        Some("") => Some(None),
        Some(p) => Some(Some(parse_priority(p)?)),
        None => None,
    };
    let due = a
        .due
        .as_deref()
        .map(|d| parse_due_text(d, now))
        .transpose()?;
    let session = match a.session.as_deref() {
        Some("") => Some(None),
        Some(q) => Some(Some(state.resolve(q)?)),
        None => None,
    };

    let mut t = old.clone();
    if let Some(title) = &a.title {
        t.title.clone_from(title);
    }
    if let Some(w) = who {
        t.who = Some(w);
    }
    if let Some(p) = priority {
        t.priority = p;
    }
    if let Some(d) = due {
        t.due = d;
    }
    if let Some(s) = session {
        t.session_id = s;
    }
    if let Some(n) = &a.note {
        t.note = (!n.is_empty()).then(|| n.clone());
    }
    if let Some(b) = &a.blocked_on {
        t.blocked_on = (!b.is_empty()).then(|| b.clone());
        if !b.is_empty() && given_status.is_none() {
            t.status = TaskStatus::Blocked;
        }
    }
    if let Some(s) = given_status {
        t.status = s;
    }
    // `today` wins over `not_today`.
    if a.today {
        t.today = Some(now.local_day);
    } else if a.not_today {
        t.today = None;
    }
    if t.status != old.status {
        t.done_at = (t.status == TaskStatus::Done).then_some(now.wall);
    }

    let mut changes = Vec::new();
    if t != old {
        t.updated_at = now.wall;
        changes.push(Change::Task(t.clone()));
        if t.status != old.status {
            // The title as it was before this call.
            let text = activity::task_status(&old.title, old.status.as_str(), t.status.as_str());
            changes.push(brain_row(state, tx, now, text));
        }
        if t.today.is_some() && t.today != old.today {
            let text = activity::added_to_today(&t.title);
            changes.push(brain_row(state, tx, now, text));
        }
    }
    tx.commit(state, changes)?;
    Ok(Done {
        text: "ok".into(),
        data: row_json(state, &t),
    })
}

pub(crate) fn note(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    text: &str,
    project: Option<&str>,
    kind: Option<&str>,
) -> Result<Done, OpError> {
    if text.trim().is_empty() {
        return Err(OpError("a note needs some text".into()));
    }
    let kind = kind
        .filter(|k| !k.is_empty())
        .unwrap_or(NoteKind::FACT)
        .to_string();
    let wanted = project.filter(|p| !p.is_empty());
    let found = wanted.and_then(|p| project_named(state, p)).map(|p| p.id);
    let reply = match (wanted, found) {
        (Some(p), None) => format!("noted (no project '{p}'; saved without one)"),
        _ => "noted".to_string(),
    };
    let note = Note {
        id: NoteId(tx.next_id(state, Table::Note)),
        project_id: found,
        kind: NoteKind(kind.clone()),
        text: text.to_string(),
        created_at: now.wall,
    };
    let row = brain_row(state, tx, now, activity::noted(&kind, text));
    let data = serde_json::to_value(&note).unwrap_or_default();
    tx.commit(state, vec![Change::Note(note), row])?;
    Ok(Done { text: reply, data })
}
