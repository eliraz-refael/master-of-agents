//! Row <-> struct for every table: reading a row the way slice 1's import reads one (text that
//! isn't UTF-8 is read lossily), and writing a row as an upsert.
//!
//! Reading never fails on content. Text this build doesn't know loads as the import's default
//! for that column and is listed as a [`LoadNote`], whose key is what [`Change::row`] gives for
//! that row. Notes name a table, a key and a fixed reason; never record contents.

use std::borrow::Cow;
use std::str::FromStr;

use jiff::civil::Date;
use moa_core::UtcMs;
use moa_core::change::{Change, conversation_key, nag_key};
use moa_core::model::*;
use moa_core::state::LoadNote;
use moa_core::time::parse_date;
use rusqlite::{Transaction, params};

use crate::import::SrcRow;

const R_LOSSY: &str = "text wasn't valid UTF-8; loaded with replacement characters";
const R_PROJECT_STATUS: &str = "status isn't a known one; loaded as active";
const R_DATE: &str = "a date isn't a date; loaded as none";
const R_TASK_STATUS: &str = "status isn't a known one; loaded as todo";
const R_TASK_WHO: &str = "who isn't a known one; loaded as none";
const R_TASK_PRIORITY: &str = "priority isn't a known one; loaded as none";
const R_SESSION_REF: &str = "session_id isn't a session id; loaded as none";
const R_SUGG_STATUS: &str = "status isn't a known one; loaded as open";
const R_SUGG_ACTIONS: &str = "actions isn't a list of actions; loaded as no actions";
const R_AGENT_KIND: &str = "agent_kind isn't a known one; loaded as claude";
const R_LIFECYCLE: &str = "lifecycle isn't a known one; loaded as ended";
const R_ACTIVITY: &str = "activity isn't a known one; loaded as idle";
const R_LOCATION: &str = "location isn't a known one; loaded as here";
const R_REASON: &str = "reason isn't a known one; loaded as import";
const R_ACTIVITY_WHO: &str = "who isn't a known one; loaded as brain";
const R_ASK_TARGET: &str = "a session isn't moa or a session id; loaded as none";
const R_ASK_STATUS: &str = "status isn't a known one; loaded as failed";
const R_THREAD_KIND: &str = "kind isn't a known one; loaded as manual";
const R_OPENED_BY: &str = "opened_by isn't a known one; loaded as found";
const R_BAD_KEY: &str = "key isn't usable; row not loaded";

/// Parses `text` only when printing the result gives `text` back. `Uuid::parse_str` also takes
/// upper case, braces and urns, and `parse_date` a trailing time; a value like that would load
/// under a key the disk doesn't hold, so for key columns it counts as unusable.
fn canonical<T: FromStr + std::fmt::Display>(text: &str) -> Option<T> {
    text.parse::<T>().ok().filter(|v| v.to_string() == text)
}

fn canonical_date(text: &str) -> Option<Date> {
    parse_date(text).ok().filter(|d| d.to_string() == text)
}

/// What reading one row found wrong, as fixed reasons.
struct Flags(Vec<&'static str>);

impl Flags {
    fn new(r: &SrcRow) -> Flags {
        Flags(if r.lossy() { vec![R_LOSSY] } else { Vec::new() })
    }

    fn closed<T: FromStr>(&mut self, text: Option<&str>, default: T, why: &'static str) -> T {
        match text.map(str::parse) {
            Some(Ok(v)) => v,
            _ => {
                self.0.push(why);
                default
            }
        }
    }

    fn closed_opt<T: FromStr>(&mut self, text: Option<&str>, why: &'static str) -> Option<T> {
        let t = text?;
        t.parse().map_or_else(
            |_| {
                self.0.push(why);
                None
            },
            Some,
        )
    }

    fn date(&mut self, text: Option<&str>) -> Option<Date> {
        let t = text.filter(|t| !t.trim().is_empty())?;
        parse_date(t).map_or_else(
            |_| {
                self.0.push(R_DATE);
                None
            },
            Some,
        )
    }

    fn session(&mut self, text: Option<&str>) -> Option<SessionId> {
        let t = text.filter(|t| !t.is_empty())?;
        canonical::<SessionId>(t).map_or_else(
            || {
                self.0.push(R_SESSION_REF);
                None
            },
            Some,
        )
    }

    fn target(&mut self, text: Option<&str>) -> Option<AskTarget> {
        let t = text.filter(|t| !t.is_empty())?;
        canonical::<AskTarget>(t).map_or_else(
            || {
                self.0.push(R_ASK_TARGET);
                None
            },
            Some,
        )
    }

    /// Lists each reason against the row; `row` is only called when there is one. The key is
    /// whatever [`Change::row`] says, so a note always names its row the way a change does.
    fn finish(self, notes: &mut Vec<LoadNote>, row: impl FnOnce() -> Change) {
        if self.0.is_empty() {
            return;
        }
        let (table, key) = row()
            .row()
            .expect("every loaded table's row has a table and key");
        notes.extend(self.0.into_iter().map(|reason| LoadNote {
            table,
            key: key.clone(),
            reason,
        }));
    }
}

/// A row whose key can't be read can't become a [`Change`]; its note names the table and the
/// key as stored.
fn unusable(notes: &mut Vec<LoadNote>, table: &'static str, key: Option<String>) {
    notes.push(LoadNote {
        table,
        key: key.unwrap_or_default(),
        reason: R_BAD_KEY,
    });
}

fn txt(r: &SrcRow, col: &str) -> Option<String> {
    r.text_cow(col).map(Cow::into_owned)
}

fn txt_or_empty(r: &SrcRow, col: &str) -> String {
    txt(r, col).unwrap_or_default()
}

fn ms(r: &SrcRow, col: &str) -> UtcMs {
    UtcMs(r.int(col).unwrap_or(0))
}

fn ms_opt(r: &SrcRow, col: &str) -> Option<UtcMs> {
    r.int(col).map(UtcMs)
}

fn flag(r: &SrcRow, col: &str) -> bool {
    r.int(col).unwrap_or(0) != 0
}

fn project_ref(r: &SrcRow, col: &str) -> Option<ProjectId> {
    r.int(col).map(ProjectId)
}

fn session_key(r: &SrcRow) -> Result<SessionId, Option<String>> {
    let raw = txt(r, "session_id");
    raw.as_deref().and_then(canonical).ok_or(raw)
}

pub(crate) const PROJECTS: &[&str] = &[
    "id",
    "name",
    "path",
    "status",
    "summary",
    "last_activity_on",
    "created_at",
    "updated_at",
];

pub(crate) fn project(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Project {
    let mut f = Flags::new(r);
    let p = Project {
        id: ProjectId(r.int("id").unwrap_or(0)),
        name: txt_or_empty(r, "name"),
        path: txt(r, "path"),
        status: f.closed(
            r.text_cow("status").as_deref(),
            ProjectStatus::Active,
            R_PROJECT_STATUS,
        ),
        summary: txt(r, "summary"),
        last_activity_on: f.date(r.text_cow("last_activity_on").as_deref()),
        created_at: ms(r, "created_at"),
        updated_at: ms(r, "updated_at"),
    };
    f.finish(notes, || Change::Project(p.clone()));
    p
}

pub(crate) const TASKS: &[&str] = &[
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
    "done_at",
];

pub(crate) fn task(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Task {
    let mut f = Flags::new(r);
    let t = Task {
        id: TaskId(r.int("id").unwrap_or(0)),
        project_id: project_ref(r, "project_id"),
        title: txt_or_empty(r, "title"),
        status: f.closed(
            r.text_cow("status").as_deref(),
            TaskStatus::Todo,
            R_TASK_STATUS,
        ),
        blocked_on: txt(r, "blocked_on"),
        today: f.date(r.text_cow("today").as_deref()),
        session_id: f.session(r.text_cow("session_id").as_deref()),
        note: txt(r, "note"),
        who: f.closed_opt(r.text_cow("who").as_deref(), R_TASK_WHO),
        due: f.date(r.text_cow("due").as_deref()),
        priority: f.closed_opt(r.text_cow("priority").as_deref(), R_TASK_PRIORITY),
        created_at: ms(r, "created_at"),
        updated_at: ms(r, "updated_at"),
        done_at: ms_opt(r, "done_at"),
    };
    f.finish(notes, || Change::Task(t.clone()));
    t
}

pub(crate) const NOTES: &[&str] = &["id", "project_id", "kind", "text", "created_at"];

pub(crate) fn note(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Note {
    let f = Flags::new(r);
    let n = Note {
        id: NoteId(r.int("id").unwrap_or(0)),
        project_id: project_ref(r, "project_id"),
        kind: NoteKind(txt_or_empty(r, "kind")),
        text: txt_or_empty(r, "text"),
        created_at: ms(r, "created_at"),
    };
    f.finish(notes, || Change::Note(n.clone()));
    n
}

pub(crate) const SUGGESTIONS: &[&str] = &[
    "id",
    "text",
    "status",
    "actions",
    "actions_raw",
    "actions_invalid",
    "result",
    "created_at",
    "resolved_at",
];

pub(crate) fn suggestion(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Suggestion {
    let mut f = Flags::new(r);
    let actions = match r.text_cow("actions") {
        None => Vec::new(),
        Some(a) => serde_json::from_str(&a).unwrap_or_else(|_| {
            f.0.push(R_SUGG_ACTIONS);
            Vec::new()
        }),
    };
    let s = Suggestion {
        id: SuggestionId(r.int("id").unwrap_or(0)),
        text: txt_or_empty(r, "text"),
        status: f.closed(
            r.text_cow("status").as_deref(),
            SuggestionStatus::Open,
            R_SUGG_STATUS,
        ),
        actions,
        actions_raw: txt(r, "actions_raw"),
        actions_invalid: txt(r, "actions_invalid"),
        result: txt(r, "result"),
        created_at: ms(r, "created_at"),
        resolved_at: ms_opt(r, "resolved_at"),
    };
    f.finish(notes, || Change::Suggestion(s.clone()));
    s
}

pub(crate) const SESSIONS: &[&str] = &[
    "id",
    "agent_kind",
    "account",
    "cwd",
    "start_cwd",
    "project_id",
    "title",
    "title_locked",
    "group_path",
    "lifecycle",
    "activity",
    "activity_at",
    "activity_why",
    "last_prompt",
    "last_turn_end_at",
    "seen_at",
    "asleep_at",
    "archived_at",
    "location",
    "resume_failed",
    "created_at",
    "last_seen_at",
];

/// `None` when the id isn't a session id: nothing can point at such a row.
pub(crate) fn session(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<Session> {
    let raw = txt(r, "id");
    let Some(id) = raw.as_deref().and_then(canonical::<SessionId>) else {
        unusable(notes, "sessions", raw);
        return None;
    };
    let mut f = Flags::new(r);
    let s = Session {
        id,
        agent_kind: f.closed(
            r.text_cow("agent_kind").as_deref(),
            AgentKind::Claude,
            R_AGENT_KIND,
        ),
        account: txt(r, "account"),
        cwd: txt(r, "cwd"),
        start_cwd: txt(r, "start_cwd"),
        project_id: project_ref(r, "project_id"),
        title: txt(r, "title"),
        title_locked: flag(r, "title_locked"),
        group_path: txt(r, "group_path"),
        lifecycle: f.closed(
            r.text_cow("lifecycle").as_deref(),
            Lifecycle::Ended,
            R_LIFECYCLE,
        ),
        activity: f.closed(
            r.text_cow("activity").as_deref(),
            Activity::Idle,
            R_ACTIVITY,
        ),
        activity_at: ms_opt(r, "activity_at"),
        activity_why: txt(r, "activity_why"),
        last_prompt: txt(r, "last_prompt"),
        last_turn_end_at: ms_opt(r, "last_turn_end_at"),
        seen_at: ms_opt(r, "seen_at"),
        asleep_at: ms_opt(r, "asleep_at"),
        archived_at: ms_opt(r, "archived_at"),
        location: f.closed(
            r.text_cow("location").as_deref(),
            Location::Here,
            R_LOCATION,
        ),
        resume_failed: flag(r, "resume_failed"),
        created_at: ms(r, "created_at"),
        last_seen_at: ms_opt(r, "last_seen_at"),
    };
    f.finish(notes, || Change::Session(s.clone()));
    Some(s)
}

pub(crate) const CONVERSATIONS: &[&str] = &[
    "agent_kind",
    "conversation_id",
    "session_id",
    "store_dir",
    "spike_account",
    "reason",
    "first_seen_at",
    "last_seen_at",
    "transcript_path",
    "transcript_present",
    "transcript_checked_at",
];

/// `None` when the agent kind or session id can't be read. (An unknown agent kind can't be
/// defaulted: it is part of the key, and `claude` would name another conversation.)
pub(crate) fn conversation(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<Conversation> {
    let kind = r
        .text_cow("agent_kind")
        .and_then(|k| k.parse::<AgentKind>().ok());
    let (Some(agent_kind), Ok(session_id)) = (kind, session_key(r)) else {
        let key = conversation_key(
            txt_or_empty(r, "agent_kind"),
            &txt_or_empty(r, "conversation_id"),
        );
        unusable(notes, "conversations", Some(key));
        return None;
    };
    let mut f = Flags::new(r);
    let c = Conversation {
        agent_kind,
        conversation_id: txt_or_empty(r, "conversation_id"),
        session_id,
        store_dir: txt(r, "store_dir"),
        spike_account: txt(r, "spike_account"),
        reason: f.closed(
            r.text_cow("reason").as_deref(),
            ConversationReason::Import,
            R_REASON,
        ),
        first_seen_at: ms(r, "first_seen_at"),
        last_seen_at: ms(r, "last_seen_at"),
        transcript_path: txt(r, "transcript_path"),
        transcript_present: r.int("transcript_present").map(|n| n != 0),
        transcript_checked_at: ms_opt(r, "transcript_checked_at"),
    };
    f.finish(notes, || Change::Conversation(c.clone()));
    Some(c)
}

pub(crate) const PANE_BINDINGS: &[&str] = &[
    "session_id",
    "server_pid",
    "server_start",
    "socket",
    "pane_id",
    "pane_pid",
    "bound_at",
    "tracked",
];

pub(crate) fn pane_binding(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<PaneBinding> {
    let session_id = match session_key(r) {
        Ok(id) => id,
        Err(raw) => {
            unusable(notes, "pane_bindings", raw);
            return None;
        }
    };
    let p = PaneBinding {
        session_id,
        server_pid: r.int("server_pid").unwrap_or(0),
        server_start: r.int("server_start").unwrap_or(0),
        socket: txt_or_empty(r, "socket"),
        pane_id: txt_or_empty(r, "pane_id"),
        pane_pid: r.int("pane_pid").unwrap_or(0),
        bound_at: ms(r, "bound_at"),
        tracked: r.int("tracked").is_none_or(|n| n != 0),
    };
    Flags::new(r).finish(notes, || Change::PaneBinding(p.clone()));
    Some(p)
}

pub(crate) const TERMINALS: &[&str] = &[
    "id",
    "server_pid",
    "server_start",
    "pane_id",
    "opened_by",
    "cwd",
    "session_id",
    "opened_at",
    "touched_at",
    "name",
    "note",
    "timeout_at",
    "shell",
    "command",
    "command_since",
    "closed_at",
];

pub(crate) fn terminal(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Terminal {
    let mut f = Flags::new(r);
    let t = Terminal {
        id: TermId(r.int("id").unwrap_or(0)),
        server_pid: r.int("server_pid").unwrap_or(0),
        server_start: r.int("server_start").unwrap_or(0),
        pane_id: txt_or_empty(r, "pane_id"),
        opened_by: f.closed(
            r.text_cow("opened_by").as_deref(),
            OpenedBy::Found,
            R_OPENED_BY,
        ),
        cwd: txt_or_empty(r, "cwd"),
        session_id: f.session(r.text_cow("session_id").as_deref()),
        opened_at: ms(r, "opened_at"),
        touched_at: ms_opt(r, "touched_at"),
        name: txt_or_empty(r, "name"),
        note: txt(r, "note"),
        timeout_at: ms_opt(r, "timeout_at"),
        shell: txt_or_empty(r, "shell"),
        command: txt_or_empty(r, "command"),
        command_since: ms(r, "command_since"),
        closed_at: ms_opt(r, "closed_at"),
    };
    f.finish(notes, || Change::Terminal(t.clone()));
    t
}

pub(crate) const SPIKE_PANES: &[&str] = &["session_id", "pane_id", "tmux", "imported_at"];

pub(crate) fn spike_pane(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<SpikePane> {
    let session_id = match session_key(r) {
        Ok(id) => id,
        Err(raw) => {
            unusable(notes, "spike_panes", raw);
            return None;
        }
    };
    let p = SpikePane {
        session_id,
        pane_id: txt_or_empty(r, "pane_id"),
        tmux: txt(r, "tmux"),
        imported_at: ms(r, "imported_at"),
    };
    Flags::new(r).finish(notes, || Change::SpikePane(p.clone()));
    Some(p)
}

pub(crate) const ACTIVITY: &[&str] = &["id", "at", "who", "text", "ref_kind", "ref_id"];

pub(crate) fn activity(r: &SrcRow, notes: &mut Vec<LoadNote>) -> ActivityRow {
    let mut f = Flags::new(r);
    let a = ActivityRow {
        id: ActivityId(r.int("id").unwrap_or(0)),
        at: ms(r, "at"),
        who: f.closed(
            r.text_cow("who").as_deref(),
            ActivityWho::Brain,
            R_ACTIVITY_WHO,
        ),
        text: txt_or_empty(r, "text"),
        ref_kind: txt(r, "ref_kind"),
        ref_id: txt(r, "ref_id"),
    };
    f.finish(notes, || Change::Activity(a.clone()));
    a
}

pub(crate) const GROUPS: &[&str] = &["path", "collapsed", "created_at"];

pub(crate) fn group(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Group {
    let g = Group {
        path: txt_or_empty(r, "path"),
        collapsed: flag(r, "collapsed"),
        created_at: ms(r, "created_at"),
    };
    Flags::new(r).finish(notes, || Change::Group(g.clone()));
    g
}

pub(crate) const ASKS: &[&str] = &[
    "id",
    "at",
    "from_session",
    "from_name",
    "to_session",
    "text",
    "status",
    "reply",
    "claimed_at",
    "answered_at",
];

pub(crate) fn ask(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Ask {
    let mut f = Flags::new(r);
    let a = Ask {
        id: AskId(r.int("id").unwrap_or(0)),
        at: ms(r, "at"),
        from_session: f.target(r.text_cow("from_session").as_deref()),
        from_name: txt(r, "from_name"),
        to_session: f.target(r.text_cow("to_session").as_deref()),
        text: txt_or_empty(r, "text"),
        status: f.closed(
            r.text_cow("status").as_deref(),
            AskStatus::Failed,
            R_ASK_STATUS,
        ),
        reply: txt(r, "reply"),
        claimed_at: ms_opt(r, "claimed_at"),
        answered_at: ms_opt(r, "answered_at"),
    };
    f.finish(notes, || Change::Ask(a.clone()));
    a
}

pub(crate) const MOA_THREADS: &[&str] = &["thread_id", "account", "day", "kind", "started_at"];

/// `None` when `day` isn't a date: a thread without its day can't be told apart.
pub(crate) fn thread(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<MoaThread> {
    let Some(day) = r.text_cow("day").and_then(|d| canonical_date(&d)) else {
        unusable(notes, "moa_threads", txt(r, "thread_id"));
        return None;
    };
    let mut f = Flags::new(r);
    let t = MoaThread {
        thread_id: txt_or_empty(r, "thread_id"),
        account: txt_or_empty(r, "account"),
        day,
        kind: f.closed(
            r.text_cow("kind").as_deref(),
            ThreadKind::Manual,
            R_THREAD_KIND,
        ),
        started_at: ms(r, "started_at"),
    };
    f.finish(notes, || Change::Thread(t.clone()));
    Some(t)
}

pub(crate) const META: &[&str] = &["key", "value"];

const R_META_NULL: &str = "value was missing; loaded as empty";

/// A meta pair. Meta rows have no [`Change::row`] key, so a note names the meta key itself.
pub(crate) fn meta(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<(String, String)> {
    let Some(key) = txt(r, "key") else {
        unusable(notes, "meta", None);
        return None;
    };
    let mut reasons = Flags::new(r).0;
    let value = txt(r, "value").unwrap_or_else(|| {
        reasons.push(R_META_NULL);
        String::new()
    });
    notes.extend(reasons.into_iter().map(|reason| LoadNote {
        table: "meta",
        key: key.clone(),
        reason,
    }));
    Some((key, value))
}

pub(crate) const NAGS: &[&str] = &["task_id", "due", "day"];

pub(crate) fn nag(r: &SrcRow, notes: &mut Vec<LoadNote>) -> Option<Nag> {
    let date = |col: &str| r.text_cow(col).and_then(|d| canonical_date(&d));
    let (Some(task_id), Some(due), Some(day)) = (r.int("task_id"), date("due"), date("day")) else {
        let key = nag_key(
            r.int("task_id").unwrap_or(0),
            txt_or_empty(r, "due"),
            txt_or_empty(r, "day"),
        );
        unusable(notes, "nags", Some(key));
        return None;
    };
    Some(Nag {
        task_id: TaskId(task_id),
        due,
        day,
    })
}

// ---------- writing ----------

/// A plain `INSERT`, for append-only history (events, activity): an id that is already taken
/// fails the whole commit instead of overwriting an old row, and the core reloads.
fn insert(table: &str, cols: &[&str]) -> String {
    let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.join(", "),
        marks.join(", ")
    )
}

/// `INSERT ... ON CONFLICT(pk) DO UPDATE SET <every other column>`. Never `INSERT OR REPLACE`:
/// that deletes the row first, and the `ON DELETE SET NULL` keys that point at it would
/// unlink their rows.
fn upsert(table: &str, cols: &[&str], pk: &[&str]) -> String {
    let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let sets: Vec<String> = cols
        .iter()
        .filter(|c| !pk.contains(c))
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();
    let action = if sets.is_empty() {
        "DO NOTHING".to_string()
    } else {
        format!("DO UPDATE SET {}", sets.join(", "))
    };
    format!(
        "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT({}) {action}",
        cols.join(", "),
        marks.join(", "),
        pk.join(", ")
    )
}

fn date_text(d: Option<Date>) -> Option<String> {
    d.map(|d| d.to_string())
}

fn ms_n(t: Option<UtcMs>) -> Option<i64> {
    t.map(|t| t.0)
}

const EVENTS: &[&str] = &[
    "id",
    "at",
    "session_id",
    "conversation_id",
    "account",
    "cwd",
    "kind",
    "text",
    "detail",
];

const FEED: &[&str] = &[
    "id",
    "thread_id",
    "at",
    "kind",
    "message_id",
    "automatic",
    "body",
    "state",
];

/// Writes one change: the exact disk mirror of `State::apply`.
pub(crate) fn write(tx: &Transaction, change: &Change) -> rusqlite::Result<()> {
    match change {
        Change::Project(p) => {
            tx.prepare_cached(&upsert("projects", PROJECTS, &["id"]))?
                .execute(params![
                    p.id.0,
                    p.name,
                    p.path,
                    p.status.as_str(),
                    p.summary,
                    date_text(p.last_activity_on),
                    p.created_at.0,
                    p.updated_at.0
                ])?;
        }
        Change::Task(t) => {
            tx.prepare_cached(&upsert("tasks", TASKS, &["id"]))?
                .execute(params![
                    t.id.0,
                    t.project_id.map(|p| p.0),
                    t.title,
                    t.status.as_str(),
                    t.blocked_on,
                    date_text(t.today),
                    t.session_id.map(|s| s.to_string()),
                    t.note,
                    t.who.map(|w| w.as_str()),
                    date_text(t.due),
                    t.priority.map(|p| p.as_str()),
                    t.created_at.0,
                    t.updated_at.0,
                    ms_n(t.done_at)
                ])?;
        }
        Change::Note(n) => {
            tx.prepare_cached(&upsert("notes", NOTES, &["id"]))?
                .execute(params![
                    n.id.0,
                    n.project_id.map(|p| p.0),
                    n.kind.0,
                    n.text,
                    n.created_at.0
                ])?;
        }
        Change::Suggestion(s) => {
            let actions = serde_json::to_string(&s.actions).unwrap_or_else(|_| "[]".into());
            tx.prepare_cached(&upsert("suggestions", SUGGESTIONS, &["id"]))?
                .execute(params![
                    s.id.0,
                    s.text,
                    s.status.as_str(),
                    actions,
                    s.actions_raw,
                    s.actions_invalid,
                    s.result,
                    s.created_at.0,
                    ms_n(s.resolved_at)
                ])?;
        }
        Change::Session(s) => {
            tx.prepare_cached(&upsert("sessions", SESSIONS, &["id"]))?
                .execute(params![
                    s.id.to_string(),
                    s.agent_kind.as_str(),
                    s.account,
                    s.cwd,
                    s.start_cwd,
                    s.project_id.map(|p| p.0),
                    s.title,
                    s.title_locked,
                    s.group_path,
                    s.lifecycle.as_str(),
                    s.activity.as_str(),
                    ms_n(s.activity_at),
                    s.activity_why,
                    s.last_prompt,
                    ms_n(s.last_turn_end_at),
                    ms_n(s.seen_at),
                    ms_n(s.asleep_at),
                    ms_n(s.archived_at),
                    s.location.as_str(),
                    s.resume_failed,
                    s.created_at.0,
                    ms_n(s.last_seen_at)
                ])?;
        }
        Change::Conversation(c) => {
            tx.prepare_cached(&upsert(
                "conversations",
                CONVERSATIONS,
                &["agent_kind", "conversation_id"],
            ))?
            .execute(params![
                c.agent_kind.as_str(),
                c.conversation_id,
                c.session_id.to_string(),
                c.store_dir,
                c.spike_account,
                c.reason.as_str(),
                c.first_seen_at.0,
                c.last_seen_at.0,
                c.transcript_path,
                c.transcript_present,
                ms_n(c.transcript_checked_at)
            ])?;
        }
        Change::PaneBinding(p) => {
            tx.prepare_cached(&upsert("pane_bindings", PANE_BINDINGS, &["session_id"]))?
                .execute(params![
                    p.session_id.to_string(),
                    p.server_pid,
                    p.server_start,
                    p.socket,
                    p.pane_id,
                    p.pane_pid,
                    p.bound_at.0,
                    p.tracked
                ])?;
        }
        Change::SpikePane(p) => {
            tx.prepare_cached(&upsert("spike_panes", SPIKE_PANES, &["session_id"]))?
                .execute(params![
                    p.session_id.to_string(),
                    p.pane_id,
                    p.tmux,
                    p.imported_at.0
                ])?;
        }
        Change::Event(e) => {
            let detail = e.detail.as_ref().map(serde_json::Value::to_string);
            tx.prepare_cached(&insert("events", EVENTS))?
                .execute(params![
                    e.id.0,
                    e.at.0,
                    e.session_id.map(|s| s.to_string()),
                    e.conversation_id,
                    e.account,
                    e.cwd,
                    e.kind.0,
                    e.text,
                    detail
                ])?;
        }
        Change::Activity(a) => {
            tx.prepare_cached(&insert("activity", ACTIVITY))?
                .execute(params![
                    a.id.0,
                    a.at.0,
                    a.who.as_str(),
                    a.text,
                    a.ref_kind,
                    a.ref_id
                ])?;
        }
        Change::Group(g) => {
            tx.prepare_cached(&upsert("groups", GROUPS, &["path"]))?
                .execute(params![g.path, g.collapsed, g.created_at.0])?;
        }
        Change::Ask(a) => {
            tx.prepare_cached(&upsert("asks", ASKS, &["id"]))?
                .execute(params![
                    a.id.0,
                    a.at.0,
                    a.from_session.map(|t| t.to_string()),
                    a.from_name,
                    a.to_session.map(|t| t.to_string()),
                    a.text,
                    a.status.as_str(),
                    a.reply,
                    ms_n(a.claimed_at),
                    ms_n(a.answered_at)
                ])?;
        }
        Change::Thread(t) => {
            tx.prepare_cached(&upsert("moa_threads", MOA_THREADS, &["thread_id"]))?
                .execute(params![
                    t.thread_id,
                    t.account,
                    t.day.to_string(),
                    t.kind.as_str(),
                    t.started_at.0
                ])?;
        }
        Change::Feed(f) => {
            tx.prepare_cached(&upsert("feed", FEED, &["id"]))?
                .execute(params![
                    f.id,
                    f.thread_id,
                    f.at.0,
                    f.kind,
                    f.message_id,
                    f.automatic,
                    f.body,
                    f.state
                ])?;
        }
        Change::Nag(n) => {
            tx.prepare_cached(&upsert("nags", NAGS, &["task_id", "due", "day"]))?
                .execute(params![n.task_id.0, n.due.to_string(), n.day.to_string()])?;
        }
        Change::Terminal(t) => {
            tx.prepare_cached(&upsert("terminals", TERMINALS, &["id"]))?
                .execute(params![
                    t.id.0,
                    t.server_pid,
                    t.server_start,
                    t.pane_id,
                    t.opened_by.as_str(),
                    t.cwd,
                    t.session_id.map(|s| s.to_string()),
                    t.opened_at.0,
                    ms_n(t.touched_at),
                    t.name,
                    t.note,
                    ms_n(t.timeout_at),
                    t.shell,
                    t.command,
                    t.command_since.0,
                    ms_n(t.closed_at)
                ])?;
        }
        Change::Meta {
            key,
            value: Some(v),
        } => {
            tx.prepare_cached(&upsert("meta", &["key", "value"], &["key"]))?
                .execute(params![key, v])?;
        }
        Change::Meta { key, value: None } => {
            tx.prepare_cached("DELETE FROM meta WHERE key = ?1")?
                .execute([key])?;
        }
        Change::DeletePaneBinding(id) => {
            tx.prepare_cached("DELETE FROM pane_bindings WHERE session_id = ?1")?
                .execute([id.to_string()])?;
        }
        Change::DeleteSpikePane(id) => {
            tx.prepare_cached("DELETE FROM spike_panes WHERE session_id = ?1")?
                .execute([id.to_string()])?;
        }
        Change::DeleteGroup(path) => {
            tx.prepare_cached("DELETE FROM groups WHERE path = ?1")?
                .execute([path])?;
        }
    }
    Ok(())
}
