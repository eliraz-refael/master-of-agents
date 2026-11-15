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
