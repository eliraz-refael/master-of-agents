//! What an import tells the user. It names records by table and `#id` (or an 8-character
//! conversation prefix) and says why in fixed sentences. It never carries record contents.

use std::fmt::Write as _;

use serde::Serialize;

/// How much a note matters. There is no "skip": every record is kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct TableCounts {
    pub table: &'static str,
    pub source: usize,
    pub imported: usize,
    /// Records that got at least one warning.
    pub warned: usize,
}

/// One thing worth saying about one record. `reason` is a `&'static str` so it cannot be
/// built from data.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct RecordNote {
    pub table: &'static str,
    pub key: String,
    pub level: Level,
    pub reason: &'static str,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct ImportReport {
    pub source_name: String,
    pub tables: Vec<TableCounts>,
    pub notes: Vec<RecordNote>,
    /// Account names in first-seen order. Printed only with `--names`.
    #[serde(skip)]
    pub accounts: Vec<String>,
    /// Accounts the spike's setup skipped. Printed only with `--names`.
    #[serde(skip)]
    pub setup_skipped: Vec<String>,
}

impl ImportReport {
    pub fn has_warnings(&self) -> bool {
        self.notes.iter().any(|n| n.level == Level::Warn)
    }

    /// The report as JSON. Account names are included only when `names` is true.
    pub fn to_json(&self, names: bool) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if names && let Some(o) = v.as_object_mut() {
            o.insert("accounts".into(), serde_json::json!(self.accounts));
            o.insert(
                "setup_skipped".into(),
                serde_json::json!(self.setup_skipped),
            );
        }
        v
    }

    /// The report for a terminal. Account names appear only when `names` is true.
    pub fn render_text(&self, names: bool) -> String {
        let mut out = format!("Imported from {}\n", self.source_name);
        for t in &self.tables {
            let _ = writeln!(
                out,
                "  {:<14} {} of {} imported, {} with warnings",
                t.table, t.imported, t.source, t.warned
            );
        }
        for n in &self.notes {
            let label = match n.level {
                Level::Info => "note",
                Level::Warn => "warning",
            };
            let _ = writeln!(out, "  {label}: {} {}: {}", n.table, n.key, n.reason);
        }
        for (i, a) in self.accounts.iter().enumerate() {
            if names {
                let _ = writeln!(out, "  account #{} is {a}", i + 1);
            } else {
                let _ = writeln!(out, "  account #{}", i + 1);
            }
        }
        if !self.setup_skipped.is_empty() {
            if names {
                let _ = writeln!(out, "  setup skipped: {}", self.setup_skipped.join(", "));
            } else {
                let _ = writeln!(
                    out,
                    "  setup skipped for {} account(s)",
                    self.setup_skipped.len()
                );
            }
        }
        out
    }
}

// The fixed reasons. A reason names a column at most and never a value.

pub const R_GROUP_PATH_FIXED: &str = "group path had empty parts; imported cleaned up";
pub const R_GROUP_PATH_EMPTY: &str = "group path is empty; not imported";

pub const R_PROJECT_NAME_CLASH: &str =
    "name clashes with another project ignoring case; renamed with a suffix";
pub const R_PROJECT_NAME_NULL: &str = "name was missing; imported as empty";
pub const R_PROJECT_STATUS: &str = "status isn't a known one; imported as active";
pub const R_PROJECT_LAST_ACTIVITY: &str = "last_activity isn't a date; imported as none";
pub const R_PROJECT_CREATED: &str = "created_at isn't a time; imported as the import time";
pub const R_PROJECT_UPDATED: &str = "updated_at isn't a time; imported as the import time";

pub const R_NOTE_PROJECT: &str = "project_id points at no project; imported as none";
pub const R_NOTE_KIND_NULL: &str = "kind was missing; imported as empty";
pub const R_NOTE_TEXT_NULL: &str = "text was missing; imported as empty";
pub const R_NOTE_CREATED: &str = "created_at isn't a time; imported as the import time";

pub const R_GROUP_CREATED: &str = "created_at isn't a time; imported as the import time";

pub const R_ACTIVITY_TS: &str = "ts isn't a time; imported as the import time";
pub const R_ACTIVITY_WHO: &str = "who isn't a known one; imported as brain";
pub const R_ACTIVITY_TEXT_NULL: &str = "text was missing; imported as empty";

pub const R_META_CURSOR: &str = "meta cursor not carried (v1 has one channel)";
pub const R_META_OTHER: &str = "meta key not carried";
pub const R_META_THREAD_BAD: &str = "console_thread isn't usable; no console thread imported";
pub const R_META_SKIPPED_BAD: &str = "setup_skipped isn't a list of names; ignored";

// Sessions and what points at them.

pub const R_SESSION_ACCOUNT_NULL: &str = "account unknown; imported as claude with no account";
pub const R_SESSION_CREATED: &str =
    "first_seen is missing or isn't a time; imported as another time of the session";
pub const R_SESSION_LAST_SEEN: &str = "last_seen isn't a time; imported as none";
pub const R_SESSION_PROJECT: &str = "project_id points at no project; imported as none";
pub const R_SESSION_STATE: &str = "unknown state; imported as ended";
pub const R_SESSION_ASLEEP: &str = "asleep_at isn't a time; imported as none";
pub const R_SESSION_ASLEEP_DROPPED: &str = "stale asleep mark dropped";
pub const R_PANE_UNVERIFIED: &str = "pane not verified yet (checked at switch-over)";

pub const R_CONV_UNSAFE: &str =
    "conversation id can't be used as a file name; transcript not checked";
pub const R_CONV_UNCHECKED: &str = "transcript not checked";

pub const R_EVENT_TS: &str = "ts isn't a time; imported as the import time";
pub const R_EVENT_KIND_NULL: &str = "kind was missing; imported as empty";
pub const R_EVENT_KIND_UNKNOWN: &str = "kind isn't a known one; kept as written";

pub const R_TASK_TITLE_NULL: &str = "title was missing; imported as empty";
pub const R_TASK_STATUS: &str = "status isn't a known one; imported as todo";
pub const R_TASK_PROJECT: &str = "project_id points at no project; imported as none";
pub const R_TASK_SESSION: &str = "session_id points at no session; imported as none";
pub const R_TASK_TODAY: &str = "today isn't a date; imported as none";
pub const R_TASK_DUE: &str = "due isn't a date; imported as none";
pub const R_TASK_WHO: &str = "who isn't a known one; imported as none";
pub const R_TASK_PRIORITY: &str = "priority isn't a known one; imported as none";
pub const R_TASK_CREATED: &str = "created_at isn't a time; imported as the import time";
pub const R_TASK_UPDATED: &str = "updated_at isn't a time; imported as the import time";

pub const R_SUGG_TEXT_NULL: &str = "text was missing; imported as empty";
pub const R_SUGG_STATUS: &str = "status isn't a known one; imported as open";
pub const R_SUGG_ACTION: &str = "action isn't a list of known commands; imported as no actions";
pub const R_SUGG_CREATED: &str = "created_at isn't a time; imported as the import time";
pub const R_SUGG_RESOLVED: &str = "resolved_at isn't a time; imported as none";
/// Stored in `suggestions.actions_invalid` when the spike's action could not be read.
pub const ACTIONS_INVALID_TEXT: &str = "the action was not a list of known commands";

pub const R_ASK_TS: &str = "ts isn't a time; imported as the import time";
pub const R_ASK_ANSWERED: &str = "answered_at isn't a time; imported as none";
pub const R_ASK_STATUS: &str = "status isn't a known one; imported as failed";
pub const R_ASK_TEXT_NULL: &str = "text was missing; imported as empty";
pub const R_ASK_TO: &str = "target session isn't in this brain";
pub const R_ASK_FROM: &str = "from_sid points at no session; imported as none";
pub const R_ASK_PENDING: &str = "pending ask dropped (nobody waits for it after the switch)";
pub const R_ASK_IN_FLIGHT: &str =
    "ask in flight closed as failed (nobody waits for it after the switch)";
/// The reply stored on an ask that was in flight at the switch.
pub const ASK_SWITCHED_REPLY: &str = "moa switched over before it was answered";

/// The report line for a missing transcript. Accounts are numbered in the report, so the
/// sentence names one by its number; the numbers are fixed text, never a name.
pub fn r_transcript_missing(account: usize) -> &'static str {
    const R: [&str; 12] = [
        "transcript missing; resume will fail (account #1)",
        "transcript missing; resume will fail (account #2)",
        "transcript missing; resume will fail (account #3)",
        "transcript missing; resume will fail (account #4)",
        "transcript missing; resume will fail (account #5)",
        "transcript missing; resume will fail (account #6)",
        "transcript missing; resume will fail (account #7)",
        "transcript missing; resume will fail (account #8)",
        "transcript missing; resume will fail (account #9)",
        "transcript missing; resume will fail (account #10)",
        "transcript missing; resume will fail (account #11)",
        "transcript missing; resume will fail (account #12)",
    ];
    R.get(account.wrapping_sub(1))
        .copied()
        .unwrap_or("transcript missing; resume will fail (another account)")
}
