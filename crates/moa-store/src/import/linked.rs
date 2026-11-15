//! The tables that point at sessions and projects: tasks, suggestions and asks.

use moa_core::model::{
    ACTION_VERBS, AskStatus, AskTarget, Priority, SuggestionAction, SuggestionStatus, TaskStatus,
    Who,
};
use rusqlite::{Transaction, params};

use super::report::*;
use super::rules;
use super::{ImportError, Run, id_key};

pub(super) fn import(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    tasks(run, tx)?;
    suggestions(run, tx)?;
    asks(run, tx)
}

fn tasks(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "tasks",
        &[
            "id",
            "project_id",
            "title",
            "status",
            "blocked_on",
            "today",
            "session_id",
            "note",
            "who",
            "due",
            "priority",
            "created_at",
            "updated_at",
        ],
        "id",
    )?;
    let (now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let project = run.rule(
            "tasks",
            &key,
            rules::reference(
                r.int("project_id"),
                |p| run.project_ids.contains(&p),
                R_TASK_PROJECT,
            ),
        );
        let title = run.rule(
            "tasks",
            &key,
            rules::text_not_null(r.text("title"), R_TASK_TITLE_NULL),
        );
        let status = run.rule(
            "tasks",
            &key,
            rules::closed(r.text("status").as_deref(), TaskStatus::Todo, R_TASK_STATUS),
        );
        let today = run.rule(
            "tasks",
            &key,
            rules::date(r.text("today").as_deref(), R_TASK_TODAY),
        );
        let session = run.rule(
            "tasks",
            &key,
            rules::reference_text(
                r.text("session_id"),
                |s| run.session_ids.contains_key(s),
                R_TASK_SESSION,
            ),
        );
        let session = session.and_then(|s| run.session_ids.get(&s).map(ToString::to_string));
        let who = run.rule(
            "tasks",
            &key,
            rules::closed_opt::<Who>(r.text("who").as_deref(), R_TASK_WHO),
        );
        let due = run.rule(
            "tasks",
            &key,
            rules::date(r.text("due").as_deref(), R_TASK_DUE),
        );
        let priority = run.rule(
            "tasks",
            &key,
            rules::closed_opt::<Priority>(r.text("priority").as_deref(), R_TASK_PRIORITY),
        );
        let created = run.rule(
            "tasks",
            &key,
            rules::instant_or(r.text("created_at").as_deref(), &tz, now, R_TASK_CREATED),
        );
        let updated = run.rule(
            "tasks",
            &key,
            rules::instant_or(r.text("updated_at").as_deref(), &tz, now, R_TASK_UPDATED),
        );
        // The spike has no completion time; its last edit is the closest thing.
        let done_at = (status == TaskStatus::Done).then_some(updated.0);
        tx.execute(
            "INSERT INTO tasks (id, project_id, title, status, blocked_on, today, session_id, \
             note, who, due, priority, created_at, updated_at, done_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                project,
                title,
                status.as_str(),
                r.text("blocked_on"),
                today.map(|d| d.to_string()),
                session,
                r.text("note"),
                who.map(|w| w.as_str()),
                due.map(|d| d.to_string()),
                priority.map(|p| p.as_str()),
                created.0,
                updated.0,
                done_at,
            ],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("tasks", rows.len(), imported, warned);
    Ok(())
}

/// The spike stores a suggestion's action as a JSON list of command lines. Each becomes a
/// verb and its arguments; anything else is not usable.
fn parse_actions(raw: &str) -> Option<Vec<SuggestionAction>> {
    let lines: Vec<String> = serde_json::from_str(raw).ok()?;
    lines
        .iter()
        .map(|line| {
            let mut words = shell_words::split(line).ok()?.into_iter();
            let verb = words
                .next()
                .filter(|v| ACTION_VERBS.contains(&v.as_str()))?;
            Some(SuggestionAction {
                verb,
                args: words.collect(),
            })
        })
        .collect()
}

fn suggestions(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "suggestions",
        &[
            "id",
            "text",
            "status",
            "action",
            "created_at",
            "resolved_at",
        ],
        "id",
    )?;
    let (now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let text = run.rule(
            "suggestions",
            &key,
            rules::text_not_null(r.text("text"), R_SUGG_TEXT_NULL),
        );
        let status = run.rule(
            "suggestions",
            &key,
            rules::closed(
                r.text("status").as_deref(),
                SuggestionStatus::Open,
                R_SUGG_STATUS,
            ),
        );
        let created = run.rule(
            "suggestions",
            &key,
            rules::instant_or(r.text("created_at").as_deref(), &tz, now, R_SUGG_CREATED),
        );
        let resolved = run.rule(
            "suggestions",
            &key,
            rules::instant(r.text("resolved_at").as_deref(), &tz, R_SUGG_RESOLVED),
        );
        let raw = r.text("action");
        let (actions, invalid) = match raw.as_deref().filter(|a| !a.trim().is_empty()) {
            None => (Vec::new(), None),
            Some(a) => match parse_actions(a) {
                Some(actions) => (actions, None),
                None => {
                    run.warn("suggestions", &key, R_SUGG_ACTION);
                    (Vec::new(), Some(ACTIONS_INVALID_TEXT))
                }
            },
        };
        tx.execute(
            "INSERT INTO suggestions (id, text, status, actions, actions_raw, actions_invalid, \
             result, created_at, resolved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
            params![
                id,
                text,
                status.as_str(),
                serde_json::to_string(&actions).unwrap_or_else(|_| "[]".into()),
                raw,
                invalid,
                created.0,
                resolved.map(|t| t.0),
            ],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("suggestions", rows.len(), imported, warned);
    Ok(())
}

/// `moa` or a session known to this brain.
fn ask_target(run: &Run, raw: Option<&str>) -> Option<AskTarget> {
    match raw? {
        "moa" => Some(AskTarget::Moa),
        s => run.session_ids.get(s).map(|id| AskTarget::Session(*id)),
    }
}

fn asks(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "asks",
        &[
            "id",
            "ts",
            "from_sid",
            "from_name",
            "to_sid",
            "text",
            "status",
            "reply",
            "answered_at",
        ],
        "id",
    )?;
    let (now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for r in &rows {
        let Some(id) = r.int("id") else { continue };
        let key = id_key(id);
        let at = run.rule(
            "asks",
            &key,
            rules::instant_or(r.text("ts").as_deref(), &tz, now, R_ASK_TS),
        );
        let answered = run.rule(
            "asks",
            &key,
            rules::instant(r.text("answered_at").as_deref(), &tz, R_ASK_ANSWERED),
        );
        let text = run.rule(
            "asks",
            &key,
            rules::text_not_null(r.text("text"), R_ASK_TEXT_NULL),
        );
        let from_raw = r.text("from_sid");
        let from = ask_target(run, from_raw.as_deref());
        if from.is_none() && from_raw.is_some() {
            run.warn("asks", &key, R_ASK_FROM);
        }
        let to = ask_target(run, r.text("to_sid").as_deref());
        let spike_status = r.text("status");
        let (status, unknown) = match spike_status.as_deref().map(str::parse::<AskStatus>) {
            Some(Ok(s)) => (s, false),
            _ => (AskStatus::Failed, true),
        };
        if unknown {
            run.warn("asks", &key, R_ASK_STATUS);
        }
        // The spike restamps `ts` when it types an ask in, so for an ask that was taken up
        // it is the closest thing to the time it was claimed.
        let claimed = matches!(
            status,
            AskStatus::Sent | AskStatus::Received | AskStatus::Answered
        )
        .then_some(at.0);
        let mut reply = r.text("reply");
        let status = if to.is_none() {
            run.warn("asks", &key, R_ASK_TO);
            AskStatus::Failed
        } else {
            match status {
                AskStatus::Pending => {
                    run.info("asks", &key, R_ASK_PENDING);
                    AskStatus::Dropped
                }
                AskStatus::Sent | AskStatus::Received => {
                    run.info("asks", &key, R_ASK_IN_FLIGHT);
                    reply = Some(ASK_SWITCHED_REPLY.to_string());
                    AskStatus::Failed
                }
                s => s,
            }
        };
        tx.execute(
            "INSERT INTO asks (id, at, from_session, from_name, to_session, text, status, reply, \
             claimed_at, answered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                at.0,
                from.map(|t| t.to_string()),
                r.text("from_name"),
                to.map(|t| t.to_string()),
                text,
                status.as_str(),
                reply,
                claimed,
                answered.map(|t| t.0),
            ],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("asks", rows.len(), imported, warned);
    Ok(())
}
