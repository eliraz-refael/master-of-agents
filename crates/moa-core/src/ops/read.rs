//! `brief`, `attention` and `session.find`: the read-only views moa asks for first. Text
//! follows the spike's moactl; sessions are chosen by `spike_state`, as the spike chose them
//! by its one `state` column.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use jiff::Span;
use jiff::civil::Date;
use serde_json::json;

use super::cut;
use crate::Now;
use crate::live::spike_state;
use crate::model::{NoteKind, ProjectId, Session, SuggestionStatus, TaskStatus};
use crate::names::{is_live_spike, label, newest_first, place, title_or_prompt};
use crate::op::{Done, OpError};
use crate::state::State;
use crate::time::{age, days_ago, local_format, local_text};

/// The label's cut, as the spike's `label` did.
const LABEL_CUT: usize = 50;
/// How many lines `attention` prints, and how many of them the brief carries.
const ATTENTION_SHOWN: usize = 20;
const BRIEF_ATTENTION: usize = 8;
/// How many matches `find` lists.
const FIND_SHOWN: usize = 15;
/// How many live and asleep sessions the brief lists.
const BRIEF_SESSIONS: usize = 12;
/// How many notes the brief lists, and how much of each.
const BRIEF_NOTES: usize = 10;
const NOTE_CUT: usize = 200;
const THREAD_CUT: usize = 600;

/// The local day a week before today, for comparing calendar dates.
fn week_ago(now: &Now) -> Option<Date> {
    now.local_day.checked_sub(Span::new().days(7)).ok()
}

fn project_of(state: &State, id: Option<ProjectId>) -> String {
    id.and_then(|p| state.projects.get(&p))
        .map_or_else(|| "-".to_string(), |p| p.name.clone())
}

fn sessions_in<'a>(state: &'a State, wanted: &[&str]) -> Vec<&'a Session> {
    state
        .sessions()
        .filter(|s| wanted.contains(&spike_state(s)))
        .collect()
}

pub(crate) fn find(state: &State, now: &Now, q: &str) -> Result<Done, OpError> {
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for id in state.candidates(q).into_iter().take(FIND_SHOWN) {
        let Some(s) = state.session(id) else { continue };
        let handle = state.handle_of(id);
        let cwd = s.cwd.as_deref().unwrap_or("-");
        let when = age(s.last_seen_at, now.wall);
        lines.push(format!(
            "{handle}  {}  {}  {when}  {cwd}",
            label(state, s, LABEL_CUT),
            spike_state(s)
        ));
        let group = place(state, s);
        data.push(json!({
            "id": s.id,
            "conversation": state.current_conversation(id).map(|c| &c.conversation_id),
            "title": title_or_prompt(s, LABEL_CUT),
            "group": (group != "-").then_some(group),
            "state": spike_state(s),
            "age": when,
            "cwd": s.cwd,
        }));
    }
    Ok(Done {
        text: lines.join("\n"),
        data: serde_json::Value::Array(data),
    })
}

/// The attention lines in the spike's order, cut to `limit`.
fn attention_lines(state: &State, now: &Now, limit: usize) -> Vec<String> {
    let mut out = Vec::new();

    // Oldest first; SQL's ascending order puts a missing time first, and `None` sorts first.
    let mut waiting = sessions_in(state, &["waiting"]);
    waiting.sort_by_key(|s| (s.last_seen_at, s.id));
    for s in waiting {
        out.push(format!(
            "- ⚠ {} is blocked on you (permission/question) for {}  → moactl resume {}",
            label(state, s, LABEL_CUT),
            age(s.last_seen_at, now.wall),
            state.handle_of(s.id)
        ));
    }

    let since = days_ago(now, 1);
    let mut idle: Vec<&Session> = sessions_in(state, &["idle"])
        .into_iter()
        .filter(|s| s.last_seen_at.is_some_and(|t| t >= since))
        .collect();
    idle.sort_by_key(|s| (s.last_seen_at, s.id));
    for s in idle {
        out.push(format!(
            "- ● {} finished its turn {} ago; your move  → moactl resume {}",
            label(state, s, LABEL_CUT),
            age(s.last_seen_at, now.wall),
            state.handle_of(s.id)
        ));
    }

    for t in state
        .tasks()
        .filter(|t| t.today == Some(now.local_day) && t.status.is_open())
    {
        out.push(format!(
            "- ▸ today: {}: {} [{}]",
            project_of(state, t.project_id),
            t.title,
            t.status
        ));
    }

    for t in state.tasks().filter(|t| t.status == TaskStatus::Blocked) {
        out.push(format!(
            "- ⧗ blocked: {}: {} on {}",
            project_of(state, t.project_id),
            t.title,
            t.blocked_on
                .as_deref()
                .filter(|b| !b.is_empty())
                .unwrap_or("?")
        ));
    }

    if let Some(week) = week_ago(now) {
        // One pass over the tasks, not one per project.
        let mut open: BTreeMap<ProjectId, usize> = BTreeMap::new();
        for t in state.tasks().filter(|t| {
            matches!(
                t.status,
                TaskStatus::Todo | TaskStatus::Active | TaskStatus::Waiting
            )
        }) {
            if let Some(p) = t.project_id {
                *open.entry(p).or_default() += 1;
            }
        }
        let mut stale: Vec<_> = state
            .projects()
            .filter_map(|p| {
                // A project with no date is skipped, as SQL's `NULL < x` skipped it.
                let on = p.last_activity_on.filter(|d| *d < week)?;
                let n = open.get(&p.id).copied().unwrap_or(0);
                (n > 0).then_some((on, p.id, &p.name, n))
            })
            .collect();
        stale.sort_by_key(|(on, id, ..)| (*on, *id));
        for (on, _, name, n) in stale {
            out.push(format!(
                "- ◷ {name}: {n} open task(s), untouched since {on}"
            ));
        }
    }

    for s in state
        .suggestions()
        .filter(|s| s.status == SuggestionStatus::Open)
    {
        out.push(format!("- ? suggestion #{}: {}", s.id, s.text));
    }

    out.truncate(limit);
    out
}

pub(crate) fn attention(state: &State, now: &Now) -> Result<Done, OpError> {
    let lines = attention_lines(state, now, ATTENTION_SHOWN);
    let text = if lines.is_empty() {
        "nothing needs you right now".to_string()
    } else {
        lines.join("\n")
    };
    Ok(Done {
        data: json!({ "text": text }),
        text,
    })
}

pub(crate) fn brief(state: &State, now: &Now) -> Result<Done, OpError> {
    let mut out = vec![format!(
        "# Brief ({}). More: moactl attention | project NAME | ls | find Q",
        local_format(now.wall, &now.tz, "%a %d %b %H:%M")
    )];

    let today: Vec<String> = state
        .tasks()
        .filter(|t| t.today == Some(now.local_day))
        .map(|t| {
            format!(
                "#{} {}: {} [{}]",
                t.id,
                project_of(state, t.project_id),
                t.title,
                t.status
            )
        })
        .collect();
    out.push(format!(
        "## Today: {}",
        if today.is_empty() {
            "nothing set yet".to_string()
        } else {
            today.join("; ")
        }
    ));

    out.push("## Needs attention".to_string());
    let attention = attention_lines(state, now, BRIEF_ATTENTION);
    if attention.is_empty() {
        out.push("- nothing".to_string());
    } else {
        out.extend(attention);
    }

    let mut hot: Vec<_> = week_ago(now)
        .map(|week| {
            state
                .projects()
                .filter(|p| p.last_activity_on.is_some_and(|d| d >= week))
                .collect()
        })
        .unwrap_or_default();
    hot.sort_by_key(|p| (Reverse(p.last_activity_on), p.id));
    out.push(format!(
        "## Active this week: {}",
        if hot.is_empty() {
            "none".to_string()
        } else {
            hot.iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));

    out.push("## Live sessions".to_string());
    let mut live: Vec<&Session> = state.sessions().filter(|s| is_live_spike(s)).collect();
    newest_first(&mut live);
    if live.is_empty() {
        out.push("- none".to_string());
    }
    for s in live.into_iter().take(BRIEF_SESSIONS) {
        out.push(format!(
            "- {} {} {} {}",
            state.handle_of(s.id),
            label(state, s, LABEL_CUT),
            spike_state(s),
            age(s.last_seen_at, now.wall)
        ));
    }

    let mut asleep: Vec<&Session> = state
        .sessions()
        .filter(|s| s.asleep_at.is_some() && spike_state(s) == "ended")
        .collect();
    asleep.sort_by_key(|s| (Reverse(s.asleep_at), s.id));
    if !asleep.is_empty() {
        let listed: Vec<String> = asleep
            .iter()
            .take(BRIEF_SESSIONS)
            .map(|s| format!("{} {}", state.handle_of(s.id), label(state, s, LABEL_CUT)))
            .collect();
        let more = if asleep.len() > BRIEF_SESSIONS {
            " …"
        } else {
            ""
        };
        out.push(format!(
            "## Asleep (idle, window closed to save memory; moactl resume ID): {}{more}",
            listed.join(", ")
        ));
    }

    out.push("## What you know about the user (latest notes)".to_string());
    let known = [NoteKind::PREFERENCE, NoteKind::ROUTINE, NoteKind::FACT];
    let mut notes: Vec<_> = state
        .notes()
        .filter(|n| known.contains(&n.kind.0.as_str()))
        .collect();
    notes.sort_by_key(|n| Reverse(n.id));
    for n in notes.into_iter().take(BRIEF_NOTES) {
        out.push(format!("- ({}) {}", n.kind.0, cut(&n.text, NOTE_CUT)));
    }

    let last = state
        .notes()
        .filter(|n| n.kind.0 == NoteKind::THREAD_SUMMARY)
        .max_by_key(|n| n.id);
    if let Some(n) = last {
        out.push(format!(
            "## Last thread ({}): {}",
            cut(&local_text(n.created_at, &now.tz), 10),
            cut(&n.text, THREAD_CUT)
        ));
    }

    let text = out.join("\n");
    Ok(Done {
        data: json!({ "text": text }),
        text,
    })
}
