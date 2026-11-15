//! The tables that stand alone: projects, notes, groups, activity and meta.

use std::collections::HashSet;

use moa_core::model::{ActivityWho, ProjectStatus};
use moa_core::time::{parse_date, spike_local_to_utc};
use rusqlite::{Transaction, params};

use super::report::*;
use super::rules;
use super::{ImportError, Run, conversation_key, id_key};

pub(super) fn projects(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "projects",
        &[
            "id",
            "name",
            "path",
            "status",
            "summary",
            "last_activity",
            "created_at",
            "updated_at",
        ],
        "id",
    )?;
    let (env_now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    let mut used: HashSet<String> = HashSet::new();
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let name = run.rule(
            "projects",
            &key,
            rules::text_not_null(r.text("name"), R_PROJECT_NAME_NULL),
        );
        let name = unique_name(run, &mut used, &key, name);
        let status = run.rule(
            "projects",
            &key,
            rules::closed(
                r.text("status").as_deref(),
                ProjectStatus::Active,
                R_PROJECT_STATUS,
            ),
        );
        let last_activity = run.rule(
            "projects",
            &key,
            rules::date(r.text("last_activity").as_deref(), R_PROJECT_LAST_ACTIVITY),
        );
        let created = run.rule(
            "projects",
            &key,
            rules::instant_or(
                r.text("created_at").as_deref(),
                &tz,
                env_now,
                R_PROJECT_CREATED,
            ),
        );
        let updated = run.rule(
            "projects",
            &key,
            rules::instant_or(
                r.text("updated_at").as_deref(),
                &tz,
                env_now,
                R_PROJECT_UPDATED,
            ),
        );
        tx.execute(
            "INSERT INTO projects (id, name, path, status, summary, last_activity_on, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                name,
                r.text("path"),
                status.as_str(),
                r.text("summary"),
                last_activity.map(|d| d.to_string()),
                created.0,
                updated.0
            ],
        )?;
        run.project_ids.insert(id);
        imported += 1;
        warned += run.end_record();
    }
    run.finish("projects", rows.len(), imported, warned);
    Ok(())
}

/// v1 project names are unique ignoring (ASCII) case; the spike's are unique exactly.
fn unique_name(run: &mut Run, used: &mut HashSet<String>, key: &str, name: String) -> String {
    let fold = |s: &str| s.to_ascii_lowercase();
    if used.insert(fold(&name)) {
        return name;
    }
    run.warn("projects", key, R_PROJECT_NAME_CLASH);
    let mut n = 2;
    loop {
        let candidate = format!("{name} ({n})");
        if used.insert(fold(&candidate)) {
            return candidate;
        }
        n += 1;
    }
}

pub(super) fn notes(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "notes",
        &["id", "project_id", "kind", "text", "created_at"],
        "id",
    )?;
    let (env_now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let project = run.rule(
            "notes",
            &key,
            rules::reference(
                r.int("project_id"),
                |p| run.project_ids.contains(&p),
                R_NOTE_PROJECT,
            ),
        );
        let kind = run.rule(
            "notes",
            &key,
            rules::text_not_null(r.text("kind"), R_NOTE_KIND_NULL),
        );
        let text = run.rule(
            "notes",
            &key,
            rules::text_not_null(r.text("text"), R_NOTE_TEXT_NULL),
        );
        let created = run.rule(
            "notes",
            &key,
            rules::instant_or(
                r.text("created_at").as_deref(),
                &tz,
                env_now,
                R_NOTE_CREATED,
            ),
        );
        tx.execute(
            "INSERT INTO notes (id, project_id, kind, text, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, project, kind, text, created.0],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("notes", rows.len(), imported, warned);
    Ok(())
}

/// Groups are the union of `groups.path` and the distinct `sessions.grp` values, cleaned up
/// (see [`rules::group_path`]) and then de-duplicated. A group made only by a session's
/// `grp` is keyed by the conversation prefix. The sessions importer cleans the same way and
/// must not warn about it again.
pub(super) fn groups(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    struct Item {
        raw: String,
        key: String,
        created: Option<String>,
    }
    let (env_now, tz) = (run.env.now, run.env.tz.clone());
    let mut items: Vec<Item> = Vec::new();
    let mut raws: HashSet<String> = HashSet::new();
    for r in run
        .src
        .rows("groups", &["rowid", "path", "created_at"], "rowid")?
    {
        let raw = r.text("path").unwrap_or_default();
        raws.insert(raw.clone());
        items.push(Item {
            raw,
            key: id_key(r.int("rowid").unwrap_or(0)),
            created: r.text("created_at"),
        });
    }
    for r in run.src.rows("sessions", &["session_id", "grp"], "rowid")? {
        let Some(raw) = r.text("grp").filter(|g| !g.is_empty()) else {
            continue;
        };
        if raws.insert(raw.clone()) {
            items.push(Item {
                raw,
                key: conversation_key(&r.text("session_id").unwrap_or_default()),
                created: None,
            });
        }
    }

    let (mut imported, mut warned) = (0, 0);
    let mut done: HashSet<String> = HashSet::new();
    for item in &items {
        let (path, fixed) = rules::group_path(&item.raw);
        if path.is_empty() {
            run.warn("groups", &item.key, R_GROUP_PATH_EMPTY);
        } else {
            if let Some(reason) = fixed {
                run.warn("groups", &item.key, reason);
            }
            if done.insert(path.clone()) {
                let created = run.rule(
                    "groups",
                    &item.key,
                    rules::instant_or(item.created.as_deref(), &tz, env_now, R_GROUP_CREATED),
                );
                tx.execute(
                    "INSERT INTO groups (path, collapsed, created_at) VALUES (?1, 0, ?2)",
                    params![path, created.0],
                )?;
                imported += 1;
            }
        }
        warned += run.end_record();
    }
    run.finish("groups", items.len(), imported, warned);
    Ok(())
}

pub(super) fn activity(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run
        .src
        .rows("activity", &["id", "ts", "who", "text"], "id")?;
    let (env_now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let at = run.rule(
            "activity",
            &key,
            rules::instant_or(r.text("ts").as_deref(), &tz, env_now, R_ACTIVITY_TS),
        );
        let who = run.rule(
            "activity",
            &key,
            rules::closed(r.text("who").as_deref(), ActivityWho::Brain, R_ACTIVITY_WHO),
        );
        let text = run.rule(
            "activity",
            &key,
            rules::text_not_null(r.text("text"), R_ACTIVITY_TEXT_NULL),
        );
        tx.execute(
            "INSERT INTO activity (id, at, who, text) VALUES (?1, ?2, ?3, ?4)",
            params![id, at.0, who.as_str(), text],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("activity", rows.len(), imported, warned);
    Ok(())
}

#[derive(serde::Deserialize)]
struct ConsoleThread {
    sid: String,
    date: String,
    account: String,
}

pub(super) fn meta(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows("meta", &["rowid", "key", "value"], "key")?;
    let (mut carried, mut warned) = (0, 0);
    let (mut threads_source, mut threads_imported, mut threads_warned) = (0, 0, 0);
    for r in &rows {
        let key = id_key(r.int("rowid").unwrap_or(0));
        let value = r.text("value").unwrap_or_default();
        match r.text("key").unwrap_or_default().as_str() {
            "console_thread" => {
                threads_source += 1;
                match console_thread(run, &value) {
                    Some(t) => {
                        tx.execute(
                            "INSERT INTO moa_threads (thread_id, account, day, kind, started_at) \
                             VALUES (?1, ?2, ?3, 'daily', ?4)",
                            params![t.0, t.1, t.2, t.3],
                        )?;
                        threads_imported += 1;
                        carried += 1;
                    }
                    None => {
                        run.warn("meta", &key, R_META_THREAD_BAD);
                        threads_warned += 1;
                    }
                }
            }
            "setup_skipped" => match serde_json::from_str::<Vec<String>>(&value) {
                Ok(names) => {
                    run.setup_skipped = names;
                    carried += 1;
                }
                Err(_) if value.trim().is_empty() => carried += 1,
                Err(_) => run.warn("meta", &key, R_META_SKIPPED_BAD),
            },
            "cursor" => run.info("meta", &key, R_META_CURSOR),
            _ => run.info("meta", &key, R_META_OTHER),
        }
        warned += run.end_record();
    }
    run.finish(
        "moa_threads",
        threads_source,
        threads_imported,
        threads_warned,
    );
    run.finish("meta", rows.len(), carried, warned);
    Ok(())
}

/// `(thread_id, account name, day, started_at)` from a `console_thread` value, or `None`
/// when it is malformed.
fn console_thread(run: &mut Run, value: &str) -> Option<(String, String, String, i64)> {
    let t: ConsoleThread = serde_json::from_str(value).ok()?;
    if t.sid.is_empty() {
        return None;
    }
    let day = parse_date(&t.date).ok()?;
    let started = spike_local_to_utc(&day.to_string(), &run.env.tz).ok()?;
    let name = run.env.accounts.resolve(&t.account).name;
    run.env.accounts.index_of(&name);
    Some((t.sid, name, day.to_string(), started.0))
}
