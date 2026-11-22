//! `project show|add`.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{brain_row, project_named};
use crate::Now;
use crate::activity;
use crate::change::Change;
use crate::core::Tx;
use crate::live::spike_state;
use crate::model::{Project, ProjectId, ProjectStatus, Session, SessionId};
use crate::names::{label, newest_first};
use crate::op::{Done, OpError};
use crate::route::{is_under, is_under_any};
use crate::state::{State, Table};
use crate::text::cut;
use crate::time::age;

/// How much of a last prompt `show` prints.
const PROMPT_CUT: usize = 80;
/// How much of a note `show` prints.
const NOTE_CUT: usize = 200;
/// How many sessions and notes `show` lists.
const LISTED: usize = 10;
/// The label's cut, as the spike's `label` did.
const LABEL_CUT: usize = 50;

/// The project named `name` exactly (ignoring case and surrounding spaces), else the
/// lowest-id one whose name contains it.
fn find<'a>(state: &'a State, name: &str) -> Option<&'a Project> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    project_named(state, name).or_else(|| {
        let want = name.to_lowercase();
        state
            .projects()
            .find(|p| p.name.to_lowercase().contains(&want))
    })
}

/// The project a session working in `path` belongs to: the one whose folder is the longest
/// whole-component prefix of it. A stored folder is tried as written and canonical, since
/// projects from the spike were never canonicalised.
pub(crate) fn project_for(state: &State, path: &Path) -> Option<ProjectId> {
    let canonical = state.deps().canonical;
    state
        .projects()
        .filter_map(|p| {
            let folder = Path::new(p.path.as_deref()?);
            [folder.to_path_buf(), canonical(folder)]
                .into_iter()
                .filter(|f| is_under(path, f))
                .map(|f| f.components().count())
                .max()
                .map(|depth| (depth, p.id))
        })
        // The longest folder; two projects for one folder go to the older one.
        .max_by_key(|(depth, id)| (*depth, std::cmp::Reverse(*id)))
        .map(|(_, id)| id)
}

pub(crate) fn show(state: &State, now: &Now, name: &str) -> Result<Done, OpError> {
    let p = find(state, name).ok_or_else(|| OpError(format!("no project matches '{name}'")))?;
    let mut lines = vec![
        format!(
            "{} [{}] {}  last activity {}",
            p.name,
            p.status,
            p.path.as_deref().unwrap_or("-"),
            p.last_activity_on
                .map_or_else(|| "-".to_string(), |d| d.to_string())
        ),
        p.summary.clone().unwrap_or_default(),
    ];
    let tasks: Vec<_> = state
        .tasks()
        .filter(|t| t.project_id == Some(p.id))
        .filter(|t| t.status.is_open())
        .collect();
    for t in &tasks {
        let blocked = t
            .blocked_on
            .as_deref()
            .filter(|b| !b.is_empty())
            .map(|b| format!(" blocked on {b}"))
            .unwrap_or_default();
        lines.push(format!(
            "  task #{} {} [{}]{blocked}",
            t.id, t.title, t.status
        ));
    }
    let mut sessions: Vec<&Session> = state
        .sessions()
        .filter(|s| s.project_id == Some(p.id))
        .collect();
    newest_first(&mut sessions);
    sessions.truncate(LISTED);
    for s in &sessions {
        lines.push(format!(
            "  session {} {} {} {}  last prompt: {}",
            state.handle_of(s.id),
            label(state, s, LABEL_CUT),
            spike_state(s),
            age(s.last_seen_at, now.wall),
            cut(s.last_prompt.as_deref().unwrap_or(""), PROMPT_CUT)
        ));
    }
    let mut notes: Vec<_> = state
        .notes()
        .filter(|n| n.project_id == Some(p.id))
        .collect();
    notes.sort_by_key(|n| std::cmp::Reverse(n.id));
    notes.truncate(LISTED);
    for n in &notes {
        lines.push(format!("  note ({}) {}", n.kind.0, cut(&n.text, NOTE_CUT)));
    }
    Ok(Done {
        text: lines.join("\n"),
        data: json!({ "project": p, "tasks": tasks, "sessions": sessions, "notes": notes }),
    })
}

pub(crate) fn add(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    name: &str,
    path: &Path,
    summary: Option<&str>,
) -> Result<Done, OpError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(OpError("a project needs a name".into()));
    }
    let folders = state
        .config()
        .brain(state.brain())
        .map(|b| b.folders)
        .unwrap_or_default();
    if folders.is_empty() {
        return Err(OpError(format!(
            "the {} brain has no folders yet; add them to moa.toml",
            state.brain().as_str()
        )));
    }
    // One spelling per folder: `/a/b/` and `/a/./b` are `/a/b`, then the canonical form, the
    // same step a path in a suggestion's action goes through. Projects imported from the
    // spike were never canonicalised, so the duplicate check and the session back-link
    // also take the path as typed.
    let typed: PathBuf = path.components().collect();
    let canon = (state.deps().canonical)(&typed);
    let path = canon.as_path();
    let shown = path.display().to_string();
    let typed_shown = typed.display().to_string();
    if !is_under_any(path, &folders) {
        return Err(OpError(format!(
            "{shown} isn't in this brain's folders; this brain doesn't track it"
        )));
    }
    // Brains' folders may nest (`default = ["~"]` beside `work = ["~/work"]`); the most
    // specific one owns the path, so a work folder never lands in the private brain.
    if let Some(owner) = state.config().brain_for_folder(path)
        && owner != *state.brain()
    {
        return Err(OpError(format!(
            "{shown} belongs to the {owner} brain; add it from there"
        )));
    }
    // The store keeps names unique ignoring case; refuse here so memory never gets ahead of
    // the disk.
    // A stored path may be another spelling of the same folder (a spike row reached through a
    // symlink), so it is compared canonical too.
    let canonical = state.deps().canonical;
    if project_named(state, name).is_some()
        || state.projects().any(|p| {
            p.path
                .as_deref()
                .is_some_and(|q| q == shown || q == typed_shown || canonical(Path::new(q)) == canon)
        })
    {
        return Err(OpError(format!(
            "there's already a project named '{name}' or for {shown}"
        )));
    }
    let id = ProjectId(tx.next_id(state, Table::Project));
    let project = Project {
        id,
        name: name.to_string(),
        path: Some(shown.clone()),
        status: ProjectStatus::Active,
        summary: summary.filter(|s| !s.is_empty()).map(str::to_string),
        last_activity_on: Some(now.local_day),
        created_at: now.wall,
        updated_at: now.wall,
    };
    let mut changes = vec![
        Change::Project(project.clone()),
        brain_row(
            state,
            tx,
            now,
            activity::new_project(name, ProjectStatus::Active.as_str()),
        ),
    ];
    // Sessions already working in the folder (whole components) join the project.
    let mut linked: Vec<SessionId> = state
        .sessions()
        .filter(|s| s.project_id.is_none())
        .filter(|s| {
            s.cwd
                .as_deref()
                .is_some_and(|c| is_under(Path::new(c), path) || is_under(Path::new(c), &typed))
        })
        .map(|s| s.id)
        .collect();
    linked.sort();
    for sid in &linked {
        let Some(mut s) = state.session(*sid).cloned() else {
            continue;
        };
        s.project_id = Some(id);
        changes.push(Change::Session(s));
        let text = activity::linked(&state.handle_of(*sid), name);
        changes.push(brain_row(state, tx, now, text));
    }
    tx.ui(state, changes, format!("new project {name} ({shown})"))?;
    let n = linked.len();
    let tail = match n {
        0 => String::new(),
        1 => "; 1 session linked".to_string(),
        n => format!("; {n} sessions linked"),
    };
    Ok(Done {
        text: format!("project {name} added for {shown}{tail}"),
        data: json!({ "project": project, "linked": linked }),
    })
}
