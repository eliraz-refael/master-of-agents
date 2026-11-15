//! The words of the activity feed. They are the spike triggers' texts, so rows written by the
//! old triggers and by the core read the same.

pub fn new_project(name: &str, status: &str) -> String {
    format!("new project {name} ({status})")
}

pub fn project_status(name: &str, old: &str, new: &str) -> String {
    format!("project {name}: {old} → {new}")
}

pub fn new_task(title: &str, project: Option<&str>) -> String {
    match project {
        Some(p) => format!("new task \"{title}\" in {p}"),
        None => format!("new task \"{title}\""),
    }
}

pub fn task_status(title: &str, old: &str, new: &str) -> String {
    format!("task \"{title}\": {old} → {new}")
}

pub fn added_to_today(title: &str) -> String {
    format!("added to today: \"{title}\"")
}

/// The text is cut at 100 characters, as SQLite's `substr` counts them.
pub fn noted(kind: &str, text: &str) -> String {
    let head: String = text.chars().take(100).collect();
    format!("noted ({kind}): {head}")
}

pub fn suggests(id: i64, text: &str) -> String {
    format!("suggests #{id}: {text}")
}

pub fn linked(handle: &str, project: &str) -> String {
    format!("linked session {handle} → {project}")
}
