//! The per-table writers of an export.

use std::collections::HashSet;
use std::path::PathBuf;

use moa_core::model::{AgentKind, EventKind, SuggestionAction};
use rusqlite::{Transaction, params};

use super::*;

const R_NO_CONVERSATION: &str =
    "session has no conversation, so the spike can't name it; not exported";
const R_NO_PANE: &str = "live session has no pane; the spike would resume it in a new window";
const R_FAILED: &str = "activity is failed; the spike shows it as idle";
const R_EVENT_KIND: &str = "event kind has no spike equivalent; skipped";
const R_ASK_TARGET: &str = "target session has no spike identity; exported as failed";
const R_DUP_CONVERSATION: &str = "conversation id is used by another session; not exported";

impl Exporter<'_> {
    pub(super) fn projects(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self.src.prepare(
            "SELECT id, name, path, status, summary, last_activity_on, created_at, updated_at \
             FROM projects ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, name, path, status, summary, last, created, updated) in &rows {
            tx.execute(
                "INSERT INTO projects (id, name, path, area, status, summary, last_activity, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, 'private', ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    name,
                    path,
                    status,
                    summary,
                    last,
                    self.local(*created),
                    self.local(*updated)
                ],
            )?;
        }
        self.finish("projects", rows.len(), rows.len(), 0);
        Ok(())
    }

    pub(super) fn sessions(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        struct S {
            rowid: i64,
            id: String,
            agent_kind: String,
            account: Option<String>,
            cwd: Option<String>,
            project: Option<i64>,
            title: Option<String>,
            group: Option<String>,
            lifecycle: String,
            activity: String,
            asleep: Option<i64>,
            archived: Option<i64>,
            location: String,
            last_prompt: Option<String>,
            created: i64,
            last_seen: Option<i64>,
            binding: Option<(String, String)>,
            spike_pane: Option<(String, Option<String>)>,
        }
        let mut stmt = self.src.prepare(
            "SELECT s.id, s.agent_kind, s.account, s.cwd, s.project_id, s.title, s.group_path, \
             s.lifecycle, s.activity, s.asleep_at, s.archived_at, s.location, s.last_prompt, \
             s.created_at, s.last_seen_at, b.pane_id, b.socket, p.pane_id, p.tmux, s.rowid \
             FROM sessions s LEFT JOIN pane_bindings b ON b.session_id = s.id \
             LEFT JOIN spike_panes p ON p.session_id = s.id ORDER BY s.rowid",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let binding = match (
                    r.get::<_, Option<String>>(15)?,
                    r.get::<_, Option<String>>(16)?,
                ) {
                    (Some(pane), Some(socket)) => Some((pane, socket)),
                    _ => None,
                };
                let spike_pane = r
                    .get::<_, Option<String>>(17)?
                    .map(|pane| Ok::<_, rusqlite::Error>((pane, r.get::<_, Option<String>>(18)?)))
                    .transpose()?;
                Ok(S {
                    rowid: r.get(19)?,
                    id: r.get(0)?,
                    agent_kind: r.get(1)?,
                    account: r.get(2)?,
                    cwd: r.get(3)?,
                    project: r.get(4)?,
                    title: r.get(5)?,
                    group: r.get(6)?,
                    lifecycle: r.get(7)?,
                    activity: r.get(8)?,
                    asleep: r.get(9)?,
                    archived: r.get(10)?,
                    location: r.get(11)?,
                    last_prompt: r.get(12)?,
                    created: r.get(13)?,
                    last_seen: r.get(14)?,
                    binding,
                    spike_pane,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let (mut exported, mut warned) = (0, 0);
        let mut used: HashSet<String> = HashSet::new();
        for s in &rows {
            let Some(cur) = self.current.get(&s.id) else {
                self.note(Level::Warn, "sessions", &key_of(s.rowid), R_NO_CONVERSATION);
                warned += self.end_record();
                continue;
            };
            let conv = cur.conversation_id.clone();
            let key = prefix8(&conv);
            if !used.insert(conv.clone()) {
                self.note(Level::Warn, "sessions", &key, R_DUP_CONVERSATION);
                warned += self.end_record();
                continue;
            }
            // The spike launches and resumes with exactly this string.
            let account = match &cur.spike_account {
                Some(a) => Some(a.clone()),
                None if s.agent_kind == AgentKind::Codex.as_str() => Some("codex".to_string()),
                None => s.account.as_ref().map(|name| match &cur.store_dir {
                    Some(dir) => abs(PathBuf::from(dir)).to_string_lossy().into_owned(),
                    None => self.account_string(name),
                }),
            };
            let state = if s.archived.is_some() {
                "archived"
            } else if s.location == "elsewhere" {
                "elsewhere"
            } else if s.lifecycle == "ended" {
                "ended"
            } else {
                match s.activity.as_str() {
                    "working" => "working",
                    "needs_permission" => "waiting",
                    other => {
                        if other == "failed" {
                            self.note(Level::Info, "sessions", &key, R_FAILED);
                        }
                        "idle"
                    }
                }
            };
            let pane = s
                .binding
                .as_ref()
                .map(|(p, sock)| (p.clone(), Some(sock.clone())))
                .or_else(|| s.spike_pane.clone());
            if pane.is_none() && s.lifecycle == "live" {
                self.note(Level::Warn, "sessions", &key, R_NO_PANE);
            }
            tx.execute(
                "INSERT INTO sessions (session_id, account, cwd, project_id, title, state, \
                 last_prompt, first_seen, last_seen, pane, tmux, grp, asleep_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    conv,
                    account,
                    s.cwd,
                    s.project,
                    s.title,
                    state,
                    s.last_prompt,
                    self.local(s.created),
                    self.local_opt(s.last_seen),
                    pane.as_ref().map(|p| p.0.clone()),
                    pane.as_ref().and_then(|p| p.1.clone()),
                    s.group,
                    self.local_opt(s.asleep),
                ],
            )?;
            exported += 1;
            warned += self.end_record();
        }
        self.finish("sessions", rows.len(), exported, warned);
        Ok(())
    }

    pub(super) fn tasks(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self.src.prepare(
            "SELECT id, project_id, title, status, blocked_on, today, session_id, note, who, due, \
             priority, created_at, updated_at FROM tasks ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, i64>(11)?,
                    r.get::<_, i64>(12)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, project, title, status, blocked, today, session, note, who, due, prio, c, u) in
            &rows
        {
            let conv = session.as_deref().and_then(|s| self.conversation_of(s));
            tx.execute(
                "INSERT INTO tasks (id, project_id, title, status, blocked_on, today, session_id, \
                 note, who, due, priority, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id,
                    project,
                    title,
                    status,
                    blocked,
                    today,
                    conv,
                    note,
                    who,
                    due,
                    prio,
                    self.local(*c),
                    self.local(*u)
                ],
            )?;
        }
        self.finish("tasks", rows.len(), rows.len(), 0);
        Ok(())
    }

    pub(super) fn notes_table(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self
            .src
            .prepare("SELECT id, project_id, kind, text, created_at FROM notes ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, project, kind, text, created) in &rows {
            tx.execute(
                "INSERT INTO notes (id, project_id, kind, text, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, project, kind, text, self.local(*created)],
            )?;
        }
        self.finish("notes", rows.len(), rows.len(), 0);
        Ok(())
    }

    /// Rewrites an argument to a session's current conversation id when the spike would
    /// otherwise not find it: it must be at least 8 characters, a prefix of exactly one v1
    /// session id, and not a prefix of any conversation id (a spike prefix passes through).
    fn rewrite_arg(&self, arg: &str) -> String {
        if arg.len() < 8 || self.all_conversations.iter().any(|c| c.starts_with(arg)) {
            return arg.to_string();
        }
        let mut hits = self.all_sessions.iter().filter(|s| s.starts_with(arg));
        match (hits.next(), hits.next()) {
            (Some(one), None) => self
                .conversation_of(one)
                .map_or_else(|| arg.to_string(), str::to_string),
            _ => arg.to_string(),
        }
    }

    pub(super) fn suggestions(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self.src.prepare(
            "SELECT id, text, status, actions, actions_raw, actions_invalid, created_at, \
             resolved_at FROM suggestions ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, text, status, actions, raw, invalid, created, resolved) in &rows {
            let action: Option<String> = if invalid.is_some() {
                raw.clone()
            } else {
                let parsed: Vec<SuggestionAction> =
                    serde_json::from_str(actions).unwrap_or_default();
                if parsed.is_empty() {
                    raw.clone().filter(|r| !r.trim().is_empty())
                } else {
                    let lines: Vec<String> = parsed
                        .iter()
                        .map(|a| {
                            let args = a.args.iter().map(|x| self.rewrite_arg(x));
                            shell_words::join(std::iter::once(a.verb.clone()).chain(args))
                        })
                        .collect();
                    Some(serde_json::to_string(&lines).unwrap_or_else(|_| "[]".into()))
                }
            };
            tx.execute(
                "INSERT INTO suggestions (id, text, status, action, created_at, resolved_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    text,
                    status,
                    action,
                    self.local(*created),
                    self.local_opt(*resolved)
                ],
            )?;
        }
        self.finish("suggestions", rows.len(), rows.len(), 0);
        Ok(())
    }

    /// Exports the events with a spike kind and returns the newest id exported.
    pub(super) fn events(&mut self, tx: &Transaction) -> Result<Option<i64>, ExportError> {
        let mut stmt = self.src.prepare(
            "SELECT id, at, session_id, conversation_id, account, cwd, kind, text \
             FROM events ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let (mut exported, mut newest) = (0, None);
        let mut skipped_kinds: HashSet<String> = HashSet::new();
        for (id, at, session, conv, account, cwd, kind, text) in &rows {
            let spike_kind = match kind.as_str() {
                EventKind::SESSION_STARTED => "start",
                EventKind::TURN_STARTED => "prompt",
                EventKind::TURN_ENDED => "stop",
                EventKind::IDLE_PING | EventKind::NEEDS_PERMISSION => "waiting",
                EventKind::SESSION_ENDED => "end",
                EventKind::UI => "ui",
                // v1's own kinds are dotted. One without a dot came from the spike, which
                // import kept as written, so it goes back as written.
                other if !other.contains('.') => other,
                _ => {
                    if skipped_kinds.insert(kind.clone()) {
                        self.note(Level::Info, "events", &key_of(*id), R_EVENT_KIND);
                    }
                    continue;
                }
            };
            let conversation = conv.clone().or_else(|| {
                session
                    .as_deref()
                    .and_then(|s| self.conversation_of(s))
                    .map(str::to_string)
            });
            // The spike's own string for the conversation when it has one (real brains name
            // accounts by name), else the store path or `codex`.
            let account = account.as_deref().map(|a| {
                conversation
                    .as_ref()
                    .and_then(|c| self.conversation_accounts.get(c))
                    .cloned()
                    .unwrap_or_else(|| self.account_string(a))
            });
            tx.execute(
                "INSERT INTO events (id, ts, session_id, account, cwd, kind, text) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    self.local(*at),
                    conversation,
                    account,
                    cwd,
                    spike_kind,
                    text
                ],
            )?;
            exported += 1;
            newest = Some(*id);
        }
        self.finish("events", rows.len(), exported, 0);
        Ok(newest)
    }

    pub(super) fn asks(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self.src.prepare(
            "SELECT id, at, from_session, from_name, to_session, text, status, reply, answered_at \
             FROM asks ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut warned = 0;
        for (id, at, from, from_name, to, text, status, reply, answered) in &rows {
            let from_sid = self.target(from.as_deref());
            let to_sid = self.target(to.as_deref());
            let status = if to_sid.is_none() {
                if to.is_some() {
                    self.note(Level::Warn, "asks", &key_of(*id), R_ASK_TARGET);
                }
                "failed"
            } else {
                status.as_str()
            };
            tx.execute(
                "INSERT INTO asks (id, ts, from_sid, from_name, to_sid, text, status, reply, \
                 answered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    self.local(*at),
                    from_sid,
                    from_name,
                    to_sid.unwrap_or_default(),
                    text,
                    status,
                    reply,
                    self.local_opt(*answered)
                ],
            )?;
            warned += self.end_record();
        }
        self.finish("asks", rows.len(), rows.len(), warned);
        Ok(())
    }

    pub(super) fn groups(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self
            .src
            .prepare("SELECT path, created_at FROM groups ORDER BY path")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for (path, created) in &rows {
            tx.execute(
                "INSERT INTO groups (path, created_at) VALUES (?1, ?2)",
                params![path, self.local(*created)],
            )?;
        }
        self.finish("groups", rows.len(), rows.len(), 0);
        Ok(())
    }

    pub(super) fn activity(&mut self, tx: &Transaction) -> Result<(), ExportError> {
        let mut stmt = self
            .src
            .prepare("SELECT id, at, who, text FROM activity ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, at, who, text) in &rows {
            tx.execute(
                "INSERT INTO activity (id, ts, who, text) VALUES (?1, ?2, ?3, ?4)",
                params![id, self.local(*at), who, text],
            )?;
        }
        self.finish("activity", rows.len(), rows.len(), 0);
        Ok(())
    }
}
