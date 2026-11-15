//! The import -> export comparison, shared by `round_trip.rs` (fixtures and generated brains)
//! and the local real-brain test. It derives what the export should hold from each source row
//! and the import's own rules, never from fixture ids.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::tz::TimeZone;
use moa_core::time::{parse_date, spike_local_to_utc};
use moa_store::accounts::AccountMap;
use moa_store::import::report::ASK_SWITCHED_REPLY;
use moa_store::import::rules::group_path;
use moa_store::import::{Level, RecordNote};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags};

/// How times (and dates) are compared.
#[derive(Clone)]
pub enum TimeMode {
    /// Equal as text. For fixtures and generated brains, whose times are in the export's format.
    Text,
    /// Both sides parse to the same instant. For real brains, which may use a `T` separator
    /// or fractions.
    Instant(TimeZone),
}

type Row = BTreeMap<String, Value>;

pub fn open_ro(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn has_table(c: &Connection, table: &str) -> bool {
    c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// Every row of a table (empty when an older spike DB lacks it), with `rowid` as `__rowid`.
fn dump(c: &Connection, table: &str) -> Vec<Row> {
    if !has_table(c, table) {
        return Vec::new();
    }
    let mut stmt = c
        .prepare(&format!(
            "SELECT rowid AS __rowid, * FROM {table} ORDER BY rowid"
        ))
        .unwrap();
    let cols: Vec<String> = stmt.column_names().map(str::to_string).collect();
    stmt.query_map([], |r| {
        let mut row = Row::new();
        for (i, col) in cols.iter().enumerate() {
            // Real brains hold text that isn't valid UTF-8; read it lossily.
            let v = match r.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::Integer(n),
                ValueRef::Real(f) => Value::Real(f),
                ValueRef::Text(b) | ValueRef::Blob(b) => {
                    Value::Text(String::from_utf8_lossy(b).into_owned())
                }
            };
            row.insert(col.clone(), v);
        }
        Ok(row)
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn text_of(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Text(t)) => Some(t),
        _ => None,
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Integer(i)) => *i,
        _ => 0,
    }
}

/// A time column as it is compared. Blank is NULL, as the import reads it.
fn time(v: &Value, mode: &TimeMode) -> Value {
    match (v, mode) {
        (Value::Text(t), _) if t.trim().is_empty() && matches!(mode, TimeMode::Instant(_)) => {
            Value::Null
        }
        (Value::Text(t), TimeMode::Instant(tz)) => match spike_local_to_utc(t, tz) {
            Ok(ms) => Value::Integer(ms.0),
            Err(_) => v.clone(),
        },
        _ => v.clone(),
    }
}

fn date(v: &Value, mode: &TimeMode) -> Value {
    match (v, mode) {
        (Value::Text(t), TimeMode::Instant(_)) => {
            if t.trim().is_empty() {
                Value::Null
            } else {
                parse_date(t).map_or_else(|_| v.clone(), |d| Value::Text(d.to_string()))
            }
        }
        _ => v.clone(),
    }
}

/// Suggestion actions compare as lists of word lists, not as text.
fn actions(v: &Value) -> Value {
    match v {
        Value::Text(raw) if raw.trim().is_empty() => Value::Null,
        Value::Text(raw) => match serde_json::from_str::<Vec<String>>(raw) {
            Ok(lines) => {
                let split: Vec<Vec<String>> = lines
                    .iter()
                    .map(|l| shell_words::split(l).unwrap())
                    .collect();
                Value::Text(serde_json::to_string(&split).unwrap())
            }
            Err(_) => v.clone(),
        },
        other => other.clone(),
    }
}

/// Panics with `loud()` (which may show values) or, when `private`, with `quiet` only. A
/// private run is a real brain: the message then holds a table, a report key and a column,
/// never a value, a long id, a path or a name.
fn ensure(private: bool, same: bool, quiet: String, loud: impl FnOnce() -> String) {
    if !same {
        panic!("{}", if private { quiet } else { loud() });
    }
}

struct Table {
    name: &'static str,
    pk: &'static str,
    times: &'static [&'static str],
    dates: &'static [&'static str],
}

const TABLES: [Table; 8] = [
    Table {
        name: "projects",
        pk: "id",
        times: &["created_at", "updated_at"],
        dates: &["last_activity"],
    },
    Table {
        name: "tasks",
        pk: "id",
        times: &["created_at", "updated_at"],
        dates: &["today", "due"],
    },
    Table {
        name: "notes",
        pk: "id",
        times: &["created_at"],
        dates: &[],
    },
    Table {
        name: "suggestions",
        pk: "id",
        times: &["created_at", "resolved_at"],
        dates: &[],
    },
    Table {
        name: "sessions",
        pk: "session_id",
        times: &["first_seen", "last_seen", "asleep_at"],
        dates: &[],
    },
    Table {
        name: "events",
        pk: "id",
        times: &["ts"],
        dates: &[],
    },
    Table {
        name: "asks",
        pk: "id",
        times: &["ts", "answered_at"],
        dates: &[],
    },
    Table {
        name: "activity",
        pk: "id",
        times: &["ts"],
        dates: &[],
    },
];

/// How a report names a source row: the first 8 characters of a session's id, else `#id`
/// (`#rowid` for tables without an integer id).
fn key(table: &str, row: &Row) -> String {
    match table {
        "sessions" => text_of(row.get("session_id"))
            .unwrap_or_default()
            .chars()
            .take(8)
            .collect(),
        "groups" | "meta" => format!("#{}", int_of(row.get("__rowid"))),
        _ => format!("#{}", int_of(row.get("id"))),
    }
}

/// The spike's own reading of `waiting`: it holds only while the session's newest prompt,
/// stop or waiting event is a waiting event that is not the idle ping.
fn still_waiting(events: &[Row], session: &str) -> bool {
    let newest = events.iter().rfind(|e| {
        text_of(e.get("session_id")) == Some(session)
            && matches!(text_of(e.get("kind")), Some("prompt" | "stop" | "waiting"))
            && !(text_of(e.get("kind")) == Some("waiting")
                && text_of(e.get("text"))
                    .is_some_and(|t| t.to_ascii_lowercase().contains("waiting for your input")))
    });
    newest.is_some_and(|e| text_of(e.get("kind")) == Some("waiting"))
}

/// What the export should hold for a source row: the import's rules applied to it.
fn expected(table: &Table, src: &Row, events: &[Row]) -> Row {
    let mut row = src.clone();
    match table.name {
        "sessions" => {
            let state = text_of(src.get("state")).map(str::to_string);
            if state.as_deref() == Some("waiting")
                && !still_waiting(events, text_of(src.get("session_id")).unwrap_or_default())
            {
                row.insert("state".into(), Value::Text("idle".into()));
            }
            // Import keeps an asleep mark only on an ended session.
            if state.as_deref() != Some("ended") {
                row.insert("asleep_at".into(), Value::Null);
            }
            // The spike reads `tmux` only together with a matching pane, and moactl.py clears
            // `pane` on other sessions when a pane is reused but leaves their `tmux`. So tmux
            // is compared only where the source pane is set; elsewhere the export's is NULL.
            if matches!(src.get("pane"), None | Some(Value::Null)) {
                row.insert("tmux".into(), Value::Null);
            }
        }
        "asks" => match text_of(src.get("status")) {
            Some("pending") => {
                row.insert("status".into(), Value::Text("dropped".into()));
            }
            Some("sent" | "received") => {
                row.insert("status".into(), Value::Text("failed".into()));
                row.insert("reply".into(), Value::Text(ASK_SWITCHED_REPLY.into()));
            }
            _ => {}
        },
        _ => {}
    }
    row
}

fn json(c: &Connection, key: &str) -> Option<serde_json::Value> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|v| serde_json::from_str(&v).ok())
}

/// Compares the export at `out` with the spike brain at `spike` it came from, every column of
/// every table, by the rules above. A source record that the import warned about (same table
/// and key in `import_notes`) is left out. Returns how many records were left out, so the
/// caller can assert on it or print it.
pub fn assert_round_trip(
    spike: &Path,
    out: &Path,
    import_notes: &[RecordNote],
    mode: &TimeMode,
) -> usize {
    assert_round_trip_with(spike, out, import_notes, mode, None)
}

/// Like [`assert_round_trip`]. With `accounts`, a console thread's account that isn't equal on
/// both sides must be an absolute path equal to the store path of the source account; without
/// it, an absolute path is accepted as it stands.
///
/// The failure messages hold no contents (see [`ensure`]) whenever `mode` is
/// [`TimeMode::Instant`] or `accounts` is given: those are the real-brain runs.
pub fn assert_round_trip_with(
    spike: &Path,
    out: &Path,
    import_notes: &[RecordNote],
    mode: &TimeMode,
    accounts: Option<&AccountMap>,
) -> usize {
    let private = matches!(mode, TimeMode::Instant(_)) || accounts.is_some();
    let (a, b) = (open_ro(spike), open_ro(out));
    let warned: BTreeSet<(&str, &str)> = import_notes
        .iter()
        .filter(|n| n.level == Level::Warn)
        .map(|n| (n.table, n.key.as_str()))
        .collect();
    let events = dump(&a, "events");
    let mut excluded = 0;

    for table in &TABLES {
        let src_rows = dump(&a, table.name);
        let out_rows = dump(&b, table.name);
        let by_pk: BTreeMap<String, &Row> = out_rows
            .iter()
            .map(|r| (format!("{:?}", r[table.pk]), r))
            .collect();
        if let (Some(s), Some(o)) = (src_rows.first(), out_rows.first()) {
            let names = |r: &Row| -> Vec<String> {
                r.keys().filter(|k| *k != "__rowid").cloned().collect()
            };
            ensure(
                private,
                names(s) == names(o),
                format!("{}: column names differ", table.name),
                || {
                    format!(
                        "{}: column names {:?} vs {:?}",
                        table.name,
                        names(s),
                        names(o)
                    )
                },
            );
        }
        let mut seen = BTreeSet::new();
        let mut skipped_pks = BTreeSet::new();
        for src in &src_rows {
            let pk = format!("{:?}", src[table.pk]);
            let rkey = key(table.name, src);
            if warned.contains(&(table.name, rkey.as_str())) {
                excluded += 1;
                skipped_pks.insert(pk);
                continue;
            }
            seen.insert(pk.clone());
            let want = expected(table, src, &events);
            let Some(got) = by_pk.get(&pk) else {
                let who = if private { &rkey } else { &pk };
                panic!("{} {who}: missing from the export", table.name);
            };
            for (col, g) in got.iter().filter(|(c, _)| *c != "__rowid") {
                let w = want.get(col).cloned().unwrap_or(Value::Null);
                let (w, g) = if table.times.contains(&col.as_str()) {
                    (time(&w, mode), time(g, mode))
                } else if table.dates.contains(&col.as_str()) {
                    (date(&w, mode), date(g, mode))
                } else if table.name == "suggestions" && col == "action" {
                    (actions(&w), actions(g))
                } else {
                    (w, g.clone())
                };
                ensure(
                    private,
                    w == g,
                    format!("{} {rkey} column {col} differs", table.name),
                    || format!("{} {pk} column {col}: {w:?} vs {g:?}", table.name),
                );
            }
        }
        // Nothing is invented. An excluded record may or may not be exported.
        for (pk, row) in &by_pk {
            if !(seen.contains(pk) || skipped_pks.contains(pk)) {
                let who = if private {
                    key(table.name, row)
                } else {
                    pk.clone()
                };
                panic!(
                    "{} {who}: the export has a row the source lacks",
                    table.name
                );
            }
        }
    }

    // groups: a superset of the normalised source (a session's grp adds its own group).
    let mut want: BTreeSet<String> = BTreeSet::new();
    for r in dump(&a, "groups") {
        want.extend(text_of(r.get("path")).map(|p| group_path(p).0));
    }
    for r in dump(&a, "sessions") {
        want.extend(text_of(r.get("grp")).map(|p| group_path(p).0));
    }
    want.remove("");
    let got: BTreeSet<String> = dump(&b, "groups")
        .iter()
        .filter_map(|r| text_of(r.get("path")).map(str::to_string))
        .collect();
    assert!(want.is_subset(&got), "a source group is missing");

    // meta: the cursor is the newest exported event; the other keys are the same JSON.
    let max_event: Option<i64> = b
        .query_row("SELECT max(id) FROM events", [], |r| r.get(0))
        .unwrap();
    let cursor: Option<String> = b
        .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |r| {
            r.get(0)
        })
        .ok();
    ensure(
        private,
        cursor == max_event.map(|m| m.to_string()),
        "meta cursor differs".into(),
        || format!("meta cursor {cursor:?} vs {max_event:?}"),
    );
    let meta_warned = |k: &str| {
        dump(&a, "meta").iter().any(|r| {
            text_of(r.get("key")) == Some(k) && warned.contains(&("meta", key("meta", r).as_str()))
        })
    };
    for k in ["console_thread", "setup_skipped"] {
        if meta_warned(k) {
            excluded += 1;
            continue;
        }
        let mut want = json(&a, k);
        let mut got = json(&b, k);
        if k == "setup_skipped" && want == Some(serde_json::json!([])) {
            want = None;
        }
        if k == "console_thread"
            && matches!(mode, TimeMode::Instant(_))
            && want != got
            && let (Some(w), Some(g)) = (want.as_mut(), got.as_mut())
        {
            // A real brain may name the account by name; the export gives its store path.
            let exported = g["account"].as_str().unwrap_or_default().to_string();
            let source = w["account"].as_str().unwrap_or_default().to_string();
            ensure(
                private,
                Path::new(&exported).is_absolute(),
                "meta console_thread account is not a path".into(),
                || format!("console_thread account {exported:?} is not a path"),
            );
            if let Some(accounts) = accounts {
                let store = accounts.resolve(&source).store;
                ensure(
                    private,
                    store.as_deref() == Some(Path::new(&exported)),
                    "meta console_thread account differs".into(),
                    || format!("console_thread account {store:?} vs {exported:?}"),
                );
            }
            w["account"] = g["account"].clone();
        }
        ensure(private, want == got, format!("meta {k} differs"), || {
            format!("meta {k}: {want:?} vs {got:?}")
        });
    }
    let keys: BTreeSet<String> = dump(&b, "meta")
        .iter()
        .filter_map(|r| text_of(r.get("key")).map(str::to_string))
        .collect();
    let allowed: BTreeSet<String> = ["cursor", "console_thread", "setup_skipped"]
        .map(str::to_string)
        .into();
    assert!(
        keys.is_subset(&allowed),
        "the export has a meta key it should not"
    );
    excluded
}
