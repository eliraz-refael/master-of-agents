//! `group add|rename|rm`.

use serde_json::json;

use crate::Now;
use crate::change::Change;
use crate::core::Tx;
use crate::model::{Group, Session, normalize_group_path};
use crate::op::{Done, OpError};
use crate::state::State;

pub(crate) fn add(state: &mut State, tx: &mut Tx, now: &Now, raw: &str) -> Result<Done, OpError> {
    let path = normalize_group_path(raw);
    if path.is_empty() {
        return Err(OpError("a group needs a name".into()));
    }
    let mut changes = Vec::new();
    if !state.groups.contains_key(&path) {
        changes.push(Change::Group(Group {
            path: path.clone(),
            collapsed: false,
            created_at: now.wall,
        }));
    }
    // Logged either way, as the spike did.
    tx.ui(state, changes, format!("new group {path}"))?;
    Ok(Done {
        text: "ok".into(),
        data: json!({ "path": path }),
    })
}

/// `candidate` is `root` or lies below it.
fn in_subtree(candidate: &str, root: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `candidate` with its `old` prefix replaced by `new`.
fn moved(candidate: &str, old: &str, new: &str) -> String {
    format!("{new}{}", &candidate[old.len()..])
}

pub(crate) fn rename(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    old: &str,
    new: &str,
) -> Result<Done, OpError> {
    let old = normalize_group_path(old);
    let new = normalize_group_path(new);
    if old.is_empty() || new.is_empty() {
        return Err(OpError("a group needs a name".into()));
    }
    let old_groups: Vec<Group> = state
        .groups()
        .filter(|g| in_subtree(&g.path, &old))
        .cloned()
        .collect();
    let sessions: Vec<Session> = state
        .sessions()
        .filter(|s| s.group_path.as_deref().is_some_and(|g| in_subtree(g, &old)))
        .cloned()
        .collect();
    if old_groups.is_empty() && sessions.is_empty() {
        return Err(OpError(format!("no group {old}")));
    }
    let mut changes = Vec::new();
    if old != new {
        // Deletes first: a moved group may land on a path that is itself being moved away.
        for g in &old_groups {
            changes.push(Change::DeleteGroup(g.path.clone()));
        }
        for g in &old_groups {
            let path = moved(&g.path, &old, &new);
            // A group already there outside the subtree keeps its own row.
            let taken = state.groups.contains_key(&path) && !in_subtree(&path, &old);
            if !taken {
                changes.push(Change::Group(Group { path, ..g.clone() }));
            }
        }
        for mut s in sessions {
            s.group_path = s.group_path.map(|g| moved(&g, &old, &new));
            changes.push(Change::Session(s));
        }
    }
    let exists_after = if old == new {
        state.groups.contains_key(&new)
    } else {
        changes
            .iter()
            .any(|c| matches!(c, Change::Group(g) if g.path == new))
            || (state.groups.contains_key(&new) && !in_subtree(&new, &old))
    };
    if !exists_after {
        changes.push(Change::Group(Group {
            path: new.clone(),
            collapsed: false,
            created_at: now.wall,
        }));
    }
    tx.ui(state, changes, format!("group {old} → {new}"))?;
    Ok(Done {
        text: "ok".into(),
        data: json!({ "old": old, "new": new }),
    })
}

pub(crate) fn rm(state: &mut State, tx: &mut Tx, path: &str) -> Result<Done, OpError> {
    let path = normalize_group_path(path);
    if path.is_empty() {
        return Err(OpError("a group needs a name".into()));
    }
    let n = state
        .sessions()
        .filter(|s| s.archived_at.is_none())
        .filter(|s| {
            s.group_path
                .as_deref()
                .is_some_and(|g| in_subtree(g, &path))
        })
        .count();
    if n > 0 {
        return Err(OpError(format!(
            "{path} still has {n} session(s); move or archive them first"
        )));
    }
    let changes: Vec<Change> = state
        .groups()
        .filter(|g| in_subtree(&g.path, &path))
        .map(|g| Change::DeleteGroup(g.path.clone()))
        .collect();
    // A typo removes nothing, so it mustn't log "removed group" either (rename refuses too).
    if changes.is_empty() {
        return Err(OpError(format!("no group {path}")));
    }
    tx.ui(state, changes, format!("removed group {path}"))?;
    Ok(Done {
        text: "ok".into(),
        data: json!({ "path": path }),
    })
}
