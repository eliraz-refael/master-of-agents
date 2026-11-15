//! Made-up spike brains: the prototype's SQLite DB, built the way the prototype builds it.
//!
//! A [`SpikeBrain`] is a list of rows plus the transcript files that belong to its
//! sessions. [`SpikeFixture`] holds the three fixed scenarios; `spike_gen` makes random ones.
//! Text values may contain `{home}`, which `write` replaces with the home directory.

use std::fs;
use std::path::Path;

use moa_store::spike::{SCHEMA_SQL, TRIGGERS_SQL};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

/// The spike's original `schema.sql` tables, from before the late columns and tables.
pub const OLD_SCHEMA_SQL: &str = "
CREATE TABLE projects (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  path TEXT,
  area TEXT NOT NULL DEFAULT 'private',
  status TEXT NOT NULL DEFAULT 'active',
  summary TEXT,
  last_activity TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY,
  project_id INTEGER REFERENCES projects(id),
  title TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'todo',
  blocked_on TEXT,
  today TEXT,
  session_id TEXT,
  note TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);
CREATE TABLE notes (
  id INTEGER PRIMARY KEY,
  project_id INTEGER REFERENCES projects(id),
  kind TEXT NOT NULL DEFAULT 'fact',
  text TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);
CREATE TABLE suggestions (
  id INTEGER PRIMARY KEY,
  text TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open',
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  resolved_at TEXT
);
CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY,
  account TEXT,
  cwd TEXT,
  project_id INTEGER REFERENCES projects(id),
  title TEXT,
  state TEXT,
  last_prompt TEXT,
  first_seen TEXT,
  last_seen TEXT
);
CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  session_id TEXT,
  account TEXT,
  cwd TEXT,
  kind TEXT NOT NULL,
  text TEXT
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
";

/// Columns an `Old` brain does not have, and tables it does not have.
const LATE_COLUMNS: [(&str, &[&str]); 3] = [
    ("sessions", &["pane", "tmux", "grp", "asleep_at"]),
    ("tasks", &["who", "due", "priority"]),
    ("suggestions", &["action"]),
];
const LATE_TABLES: [&str; 3] = ["groups", "activity", "asks"];

/// Conversation ids of the fixed scenarios. Random-looking v4 UUIDs with distinct
/// 8-character prefixes.
pub mod ids {
    pub const S1_WORKING: &str = "3f9a1c52-7d4e-4b86-9a31-c0e5d82f6b17";
    pub const S2_IDLE: &str = "b84e07d1-2a93-4c5f-8e60-17f3a9d2c4e8";
    pub const S3_WAITING: &str = "d1c6a9f3-58be-4e27-b4a0-9e72f1038c5d";
    pub const S4_ENDED: &str = "6a2f8e40-c1d7-4930-a5b8-3d90e47f12ac";
    pub const S5_ASLEEP: &str = "e57b3d96-0f48-4a1c-97d2-8b61c5a0f3e9";
    pub const S6_ARCHIVED: &str = "29d0c4b7-e385-4f62-8a1d-f6740b9e5c23";
    pub const S7_ELSEWHERE: &str = "90e1f5a8-3b6c-4d79-b2e4-5a8c07d1f946";
    pub const S8_CODEX: &str = "c4a73e19-86f2-4b05-9d3c-12e8b7f50a6d";
    pub const S9_WAITING_IDLE: &str = "7b5d2e83-a09c-4f41-8c76-e3190d6a4b5f";
    pub const AWK_NO_FIRST_SEEN: &str = "f2086b3a-4c91-4e5d-a7b3-0d6e9c185f42";
    pub const AWK_WEIRD_STATE: &str = "158c9e7d-b3a4-4062-8f19-a5d7e20c4b83";
    pub const AWK_NO_ACCOUNT: &str = "a3e47d05-9f12-4b8c-b6d0-7c418e5a29f1";
    pub const AWK_NO_STORE: &str = "4d91b6c2-e07a-4538-9c4f-2b83a6f1d075";
    pub const AWK_FOLD: &str = "8c3f5a71-1d68-4e90-a2b5-d9047e3c6f18";
    /// A session id that is a path, not a UUID.
    pub const AWK_PATH: &str = "../../etc";
    /// Not a session in any brain.
    pub const AWK_GHOST: &str = "5c2e9a04-b7d1-4f36-8e5a-1d93c06b7f28";
    pub const OLD_1: &str = "0b7e49d3-6a52-4c18-8f9e-4a1d70c3b5e6";
    pub const OLD_2: &str = "5e12a8f6-d934-4b7c-a0e3-8c65f91b2d47";
    pub const OLD_3: &str = "ad60c3e5-27f8-4915-b1a9-e4d3072f8c6b";
}

#[derive(Clone, Copy, Debug)]
pub enum Scenario {
    Typical,
    Awkward,
    Old,
}

pub type Row = Vec<(&'static str, Value)>;

/// A spike brain as data: rows in insertion order, and the transcripts to create.
#[derive(Clone, Debug, Default)]
pub struct SpikeBrain {
    /// Built from [`OLD_SCHEMA_SQL`], with no triggers.
    pub old: bool,
    pub rows: Vec<(&'static str, Row)>,
    /// `(store directory name under home, conversation id)`.
    pub transcripts: Vec<(String, String)>,
    pub(crate) tokens: Vec<String>,
}

impl SpikeBrain {
    /// Every generated free-text, name, path, group and status string.
    pub fn tokens(&self) -> Vec<String> {
        self.tokens.clone()
    }

    pub fn write(&self, path: &Path, home: &Path) {
        drop(
            self.write_holding_wal(path, home)
                .expect("write spike brain"),
        );
    }

    /// Writes the DB and returns the open writer, with auto-checkpoint off, so the WAL
    /// still holds the frames.
    pub fn write_holding_wal(&self, path: &Path, home: &Path) -> rusqlite::Result<Connection> {
        let conn = Connection::open(path)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        // The spike's Python connections leave foreign keys off, so dangling references exist.
        conn.pragma_update(None, "foreign_keys", false)?;
        if self.old {
            conn.execute_batch(OLD_SCHEMA_SQL)?;
        } else {
            conn.execute_batch(SCHEMA_SQL)?;
            conn.execute_batch(TRIGGERS_SQL)?;
        }
        let home_str = home.to_string_lossy();
        for (table, row) in &self.rows {
            if self.old && LATE_TABLES.contains(table) {
                continue;
            }
            let late: &[&str] = LATE_COLUMNS
                .iter()
                .find(|(t, _)| t == table)
                .map_or(&[], |(_, c)| c);
            let kept: Vec<&(&str, Value)> = row
                .iter()
                .filter(|(c, _)| !(self.old && late.contains(c)))
                .collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                kept.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", "),
                kept.iter()
                    .map(|(_, v)| if matches!(v, Value::Blob(_)) {
                        "CAST(? AS TEXT)"
                    } else {
                        "?"
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let values = kept.iter().map(|(_, v)| match v {
                Value::Text(t) => Value::Text(t.replace("{home}", &home_str)),
                other => other.clone(),
            });
            conn.execute(&sql, params_from_iter(values))?;
        }
        for (store, id) in &self.transcripts {
            let dir = home.join(store).join("projects/-x");
            fs::create_dir_all(&dir).expect("create transcript dir");
            let line = format!(
                "{{\"type\":\"user\",\"message\":\"hello from {}\"}}\n",
                &id[..8]
            );
            fs::write(dir.join(format!("{id}.jsonl")), line).expect("write transcript");
        }
        Ok(conn)
    }
}

fn s(x: &str) -> Value {
    Value::Text(x.to_string())
}

fn n(x: i64) -> Value {
    Value::Integer(x)
}

const NIL: Value = Value::Null;

fn brain(rows: Vec<(&'static str, Row)>) -> SpikeBrain {
    SpikeBrain {
        rows,
        ..SpikeBrain::default()
    }
}

pub struct SpikeFixture;

impl SpikeFixture {
    pub fn write(path: &Path, scenario: Scenario, home: &Path) -> rusqlite::Result<()> {
        drop(Self::write_holding_wal(path, scenario, home)?);
        Ok(())
    }

    /// Like [`SpikeFixture::write`], but returns the open connection with uncheckpointed
    /// WAL frames.
    pub fn write_holding_wal(
        path: &Path,
        scenario: Scenario,
        home: &Path,
    ) -> rusqlite::Result<Connection> {
        Self::brain(scenario).write_holding_wal(path, home)
    }

    pub fn brain(scenario: Scenario) -> SpikeBrain {
        match scenario {
            Scenario::Typical => typical(),
            Scenario::Awkward => awkward(),
            Scenario::Old => old(),
        }
    }
}

const A: &str = "{home}/.acct-a";
const B: &str = "{home}/.acct-b";
const LANTERN: &str = "{home}/private/lantern";

fn typical() -> SpikeBrain {
    use ids::*;
    let mut rows: Vec<(&'static str, Row)> = Vec::new();
    let t = |d: &str| s(&format!("2026-11-{d}"));
    rows.push((
        "projects",
        vec![
            ("id", n(1)),
            ("name", s("Lantern")),
            ("path", s(LANTERN)),
            ("area", s("private")),
            ("status", s("active")),
            ("summary", s("A small lamp controller")),
            ("last_activity", s("2026-11-15")),
            ("created_at", t("02 08:00:00")),
            ("updated_at", t("15 09:00:00")),
        ],
    ));
    rows.push((
        "projects",
        vec![
            ("id", n(2)),
            ("name", s("Tidepool")),
            ("path", s("{home}/private/tidepool")),
            ("area", s("private")),
            ("status", s("idea")),
            ("summary", NIL),
            ("last_activity", NIL),
            ("created_at", t("05 10:30:00")),
            ("updated_at", t("05 10:30:00")),
        ],
    ));
    rows.push((
        "projects",
        vec![
            ("id", n(3)),
            ("name", s("Driftwood")),
            ("path", NIL),
            ("area", s("private")),
            ("status", s("paused")),
            ("summary", s("Shelved for now")),
            ("last_activity", s("2026-10-20")),
            ("created_at", s("2026-10-01 18:00:00")),
            ("updated_at", s("2026-10-20 18:00:00")),
        ],
    ));

    // (id, account, project, title, state, first_seen, last_seen, pane, tmux, grp, asleep_at)
    #[allow(clippy::type_complexity)]
    let sessions: [(
        &str,
        Value,
        Value,
        &str,
        &str,
        &str,
        &str,
        Value,
        Value,
        Value,
        Value,
    ); 9] = [
        (
            S1_WORKING,
            s(A),
            n(1),
            "Wire up the dimmer",
            "working",
            "2026-11-15 08:55:00",
            "2026-11-15 09:20:00",
            s("%3"),
            s("/tmp/x/moa"),
            s("Private/Lantern"),
            NIL,
        ),
        (
            S2_IDLE,
            s(B),
            NIL,
            "Read the tide tables",
            "idle",
            "2026-11-14 16:00:00",
            "2026-11-14 16:45:00",
            s("%4"),
            s("/tmp/x/moa"),
            NIL,
            NIL,
        ),
        (
            S3_WAITING,
            s(A),
            n(1),
            "Fix the flicker",
            "waiting",
            "2026-11-15 07:40:00",
            "2026-11-15 08:10:00",
            NIL,
            NIL,
            NIL,
            NIL,
        ),
        (
            S4_ENDED,
            s(B),
            n(2),
            "Sketch the harbour",
            "ended",
            "2026-11-13 11:00:00",
            "2026-11-13 12:00:00",
            NIL,
            NIL,
            NIL,
            NIL,
        ),
        (
            S5_ASLEEP,
            s(A),
            n(1),
            "Old lamp notes",
            "ended",
            "2026-11-12 09:00:00",
            "2026-11-12 10:00:00",
            NIL,
            NIL,
            NIL,
            s("2026-11-12 10:30:00"),
        ),
        (
            S6_ARCHIVED,
            s(B),
            n(3),
            "Beach scan",
            "archived",
            "2026-11-10 09:00:00",
            "2026-11-10 09:30:00",
            NIL,
            NIL,
            NIL,
            s("2026-11-10 09:40:00"),
        ),
        (
            S7_ELSEWHERE,
            s(A),
            NIL,
            "Borrowed session",
            "elsewhere",
            "2026-11-11 14:00:00",
            "2026-11-11 14:20:00",
            NIL,
            NIL,
            NIL,
            NIL,
        ),
        (
            S8_CODEX,
            s("codex"),
            NIL,
            "Codex tinkering",
            "ended",
            "2026-11-09 13:00:00",
            "2026-11-09 13:30:00",
            NIL,
            NIL,
            NIL,
            NIL,
        ),
        (
            S9_WAITING_IDLE,
            s(A),
            n(1),
            "Calibrate the sensor",
            "waiting",
            "2026-11-15 06:00:00",
            "2026-11-15 06:30:00",
            NIL,
            NIL,
            NIL,
            NIL,
        ),
    ];
    for (id, account, project, title, state, first, last, pane, tmux, grp, asleep) in sessions {
        rows.push((
            "sessions",
            vec![
                ("session_id", s(id)),
                ("account", account),
                ("cwd", s(LANTERN)),
                ("project_id", project),
                ("title", s(title)),
                ("state", s(state)),
                ("last_prompt", s("Please continue")),
                ("first_seen", s(first)),
                ("last_seen", s(last)),
                ("pane", pane),
                ("tmux", tmux),
                ("grp", grp),
                ("asleep_at", asleep),
            ],
        ));
    }

    // Events, in id order. Each session's last event matches its state.
    let idle_ping = "Claude is waiting for your input";
    let permission = "Claude needs your permission to use Bash";
    let ev: [(&str, Value, &str, &str); 17] = [
        ("2026-11-14 16:00:00", s(S2_IDLE), "start", "startup"),
        (
            "2026-11-14 16:01:00",
            s(S2_IDLE),
            "prompt",
            "What time is high tide",
        ),
        (
            "2026-11-14 16:05:00",
            s(S2_IDLE),
            "stop",
            "Want me to check the next one?",
        ),
        (
            "2026-11-15 06:00:00",
            s(S9_WAITING_IDLE),
            "start",
            "startup",
        ),
        (
            "2026-11-15 06:01:00",
            s(S9_WAITING_IDLE),
            "prompt",
            "Calibrate the sensor",
        ),
        (
            "2026-11-15 06:10:00",
            s(S9_WAITING_IDLE),
            "stop",
            "Calibrated",
        ),
        (
            "2026-11-15 06:11:00",
            s(S9_WAITING_IDLE),
            "waiting",
            idle_ping,
        ),
        ("2026-11-15 07:40:00", s(S3_WAITING), "start", "startup"),
        (
            "2026-11-15 07:41:00",
            s(S3_WAITING),
            "prompt",
            "Fix the flicker",
        ),
        ("2026-11-15 08:10:00", s(S3_WAITING), "waiting", permission),
        ("2026-11-15 08:55:00", s(S1_WORKING), "start", "startup"),
        (
            "2026-11-15 08:56:00",
            s(S1_WORKING),
            "prompt",
            "Wire up the dimmer\nand test it",
        ),
        ("2026-11-13 11:00:00", s(S4_ENDED), "start", "startup"),
        (
            "2026-11-13 11:01:00",
            s(S4_ENDED),
            "prompt",
            "Sketch the harbour",
        ),
        ("2026-11-13 11:50:00", s(S4_ENDED), "stop", "Sketched"),
        ("2026-11-13 12:00:00", s(S4_ENDED), "end", "exit"),
        ("2026-11-15 09:21:00", NIL, "ui", "opened the plan"),
    ];
    for (i, (ts, sid, kind, text)) in ev.into_iter().enumerate() {
        rows.push((
            "events",
            vec![
                ("id", n(i as i64 + 1)),
                ("ts", s(ts)),
                ("session_id", sid.clone()),
                ("account", NIL),
                ("cwd", if kind == "ui" { NIL } else { s(LANTERN) }),
                ("kind", s(kind)),
                ("text", s(text)),
            ],
        ));
    }
    for (i, text) in ["moved the pane", "marked a task done"]
        .into_iter()
        .enumerate()
    {
        rows.push((
            "events",
            vec![
                ("id", n(18 + i as i64)),
                ("ts", s("2026-11-15 09:22:00")),
                ("session_id", NIL),
                ("account", NIL),
                ("cwd", NIL),
                ("kind", s("ui")),
                ("text", s(text)),
            ],
        ));
    }

    let task = |id: i64,
                project: Value,
                title: &str,
                status: &str,
                blocked: Value,
                today: Value,
                session: Value,
                who: Value,
                due: Value,
                prio: Value|
     -> (&'static str, Row) {
        (
            "tasks",
            vec![
                ("id", n(id)),
                ("project_id", project),
                ("title", s(title)),
                ("status", s(status)),
                ("blocked_on", blocked),
                ("today", today),
                ("session_id", session),
                ("note", NIL),
                ("who", who),
                ("due", due),
                ("priority", prio),
                ("created_at", s("2026-11-14 09:00:00")),
                ("updated_at", s("2026-11-15 09:00:00")),
            ],
        )
    };
    rows.push(task(
        1,
        n(1),
        "Order the dimmer board",
        "todo",
        NIL,
        s("2026-11-15"),
        s(S1_WORKING),
        s("you"),
        s("2026-11-16"),
        s("p1"),
    ));
    rows.push(task(
        2,
        n(1),
        "Draft the wiring diagram",
        "done",
        NIL,
        NIL,
        NIL,
        s("agent"),
        NIL,
        NIL,
    ));
    rows.push(task(
        3,
        n(2),
        "Survey the tide flats",
        "blocked",
        s("a permit"),
        NIL,
        NIL,
        s("other"),
        NIL,
        NIL,
    ));
    rows.push(task(
        4,
        n(3),
        "Carve the sign",
        "dropped",
        NIL,
        NIL,
        NIL,
        NIL,
        NIL,
        NIL,
    ));
    rows.push(task(
        5,
        NIL,
        "Reply about the lease",
        "waiting",
        NIL,
        NIL,
        NIL,
        NIL,
        NIL,
        NIL,
    ));

    for (i, (project, kind, text)) in [
        (n(1), "fact", "The lamp runs on 12 volts"),
        (NIL, "preference", "Short answers first"),
        (n(1), "milestone", "First light on the bench"),
        (n(2), "thread-summary", "Talked through the harbour layout"),
    ]
    .into_iter()
    .enumerate()
    {
        rows.push((
            "notes",
            vec![
                ("id", n(i as i64 + 1)),
                ("project_id", project),
                ("kind", s(kind)),
                ("text", s(text)),
                ("created_at", s("2026-11-14 12:00:00")),
            ],
        ));
    }

    let prefix = &S1_WORKING[..8];
    for (id, text, status, action, resolved) in [
        (
            1,
            "Mark the board order done",
            "open",
            s("[\"task 1 --status done\"]"),
            NIL,
        ),
        (
            2,
            "Rename the working session",
            "open",
            s(&format!("[\"set {prefix} --title \\\"New name\\\"\"]")),
            NIL,
        ),
        (
            3,
            "Archive Driftwood",
            "rejected",
            NIL,
            s("2026-11-15 08:00:00"),
        ),
        (
            4,
            "Note the lamp voltage",
            "later",
            s("[\"note --kind fact \\\"x y\\\"\"]"),
            NIL,
        ),
    ] {
        rows.push((
            "suggestions",
            vec![
                ("id", n(id)),
                ("text", s(text)),
                ("status", s(status)),
                ("action", action),
                ("created_at", s("2026-11-15 07:00:00")),
                ("resolved_at", resolved),
            ],
        ));
    }

    rows.push((
        "groups",
        vec![
            ("path", s("Private")),
            ("created_at", s("2026-11-01 09:00:00")),
        ],
    ));

    rows.push((
        "asks",
        vec![
            ("id", n(1)),
            ("ts", s("2026-11-15 08:20:00")),
            ("from_sid", s(S2_IDLE)),
            ("from_name", s("Tide reader")),
            ("to_sid", s(S1_WORKING)),
            ("text", s("Which pin is the dimmer on?")),
            ("status", s("answered")),
            ("reply", s("Pin 7")),
            ("answered_at", s("2026-11-15 08:25:00")),
        ],
    ));
    rows.push((
        "asks",
        vec![
            ("id", n(2)),
            ("ts", s("2026-11-15 08:30:00")),
            ("from_sid", s(S1_WORKING)),
            ("from_name", s("Lamp worker")),
            ("to_sid", s("moa")),
            ("text", s("Is the board order approved?")),
            ("status", s("pending")),
            ("reply", NIL),
            ("answered_at", NIL),
        ],
    ));
    rows.push((
        "asks",
        vec![
            ("id", n(3)),
            ("ts", s("2026-11-15 08:35:00")),
            ("from_sid", NIL),
            ("from_name", NIL),
            ("to_sid", s(S3_WAITING)),
            ("text", s("Please allow the command")),
            ("status", s("sent")),
            ("reply", NIL),
            ("answered_at", NIL),
        ],
    ));

    rows.push(("meta", vec![("key", s("cursor")), ("value", s("19"))]));
    rows.push((
        "meta",
        vec![
            ("key", s("console_thread")),
            (
                "value",
                s(&format!(
                    "{{\"sid\":\"{S1_WORKING}\",\"date\":\"2026-11-15\",\"account\":\"{B}\"}}"
                )),
            ),
        ],
    ));
    rows.push((
        "meta",
        vec![("key", s("setup_skipped")), ("value", s("[\"acct-c\"]"))],
    ));

    let mut b = brain(rows);
    for (store, id) in [
        (".acct-a", S1_WORKING),
        (".acct-a", S3_WAITING),
        (".acct-a", S9_WAITING_IDLE),
        (".acct-b", S4_ENDED),
        (".acct-a", S5_ASLEEP),
        (".acct-b", S6_ARCHIVED),
        (".acct-a", S7_ELSEWHERE),
    ] {
        b.transcripts.push((store.to_string(), id.to_string()));
    }
    b
}

fn awkward() -> SpikeBrain {
    use ids::*;
    let mut rows: Vec<(&'static str, Row)> = Vec::new();
    let ts = "2026-11-14 10:00:00";
    let project = |id: i64, name: &str, created: &str| -> (&'static str, Row) {
        (
            "projects",
            vec![
                ("id", n(id)),
                ("name", s(name)),
                ("path", NIL),
                ("area", s("private")),
                ("status", s("active")),
                ("created_at", s(created)),
                ("updated_at", s(ts)),
            ],
        )
    };
    rows.push(project(1, "Lantern", ts));
    rows.push(project(2, "lantern", ts));
    rows.push(project(3, "Tidepool", "garbage"));

    let task = |id: i64,
                project: Value,
                status: &str,
                due: Value,
                session: Value|
     -> (&'static str, Row) {
        (
            "tasks",
            vec![
                ("id", n(id)),
                ("project_id", project),
                ("title", s(&format!("Odd task {id}"))),
                ("status", s(status)),
                ("session_id", session),
                ("due", due),
                ("created_at", s(ts)),
                ("updated_at", s(ts)),
            ],
        )
    };
    rows.push(task(1, n(1), "someday", NIL, NIL));
    rows.push(task(2, n(1), "todo", s("next week"), NIL));
    rows.push(task(3, n(1), "todo", s(""), NIL));
    rows.push(task(4, n(1), "todo", NIL, s(AWK_GHOST)));
    rows.push(task(5, n(999), "todo", NIL, NIL));

    rows.push((
        "suggestions",
        vec![
            ("id", n(1)),
            ("text", s("Not really an action")),
            ("status", s("open")),
            ("action", s("not json")),
            ("created_at", s(ts)),
        ],
    ));
    rows.push((
        "suggestions",
        vec![
            ("id", n(2)),
            ("text", s("Tidy up")),
            ("status", s("open")),
            ("action", s("[\"rm -rf x\"]")),
            ("created_at", s(ts)),
        ],
    ));

    // (id, account, title, state, first_seen)
    let sessions: [(&str, Value, &str, &str, Value); 5] = [
        (AWK_NO_FIRST_SEEN, s(A), "Has no first seen", "idle", NIL),
        (AWK_WEIRD_STATE, s(A), "Odd state", "weird", s(ts)),
        (AWK_NO_ACCOUNT, NIL, "No account", "ended", s(ts)),
        (
            AWK_NO_STORE,
            s("{home}/.acct-z"),
            "Account without a store",
            "ended",
            s(ts),
        ),
        (AWK_PATH, s(A), "名前 🌊\nsecond line", "ended", s(ts)),
    ];
    for (id, account, title, state, first) in sessions {
        rows.push((
            "sessions",
            vec![
                ("session_id", s(id)),
                ("account", account),
                ("cwd", s(LANTERN)),
                ("title", s(title)),
                ("state", s(state)),
                ("first_seen", first),
                ("last_seen", s(ts)),
                ("grp", NIL),
            ],
        ));
    }
    rows.push((
        "sessions",
        vec![
            ("session_id", s(AWK_FOLD)),
            ("account", s(A)),
            ("cwd", s(LANTERN)),
            ("title", s("Fold time")),
            ("state", s("ended")),
            ("first_seen", s("2026-11-01 01:30:00")),
            ("last_seen", s("2026-11-01 01:45:00")),
        ],
    ));

    let event =
        |id: i64, ts: &str, sid: &str, cwd: Value, kind: &str, text: &str| -> (&'static str, Row) {
            (
                "events",
                vec![
                    ("id", n(id)),
                    ("ts", s(ts)),
                    ("session_id", s(sid)),
                    ("cwd", cwd),
                    ("kind", s(kind)),
                    ("text", s(text)),
                ],
            )
        };
    rows.push(event(
        1,
        "2026-11-14 09:30:00",
        AWK_NO_FIRST_SEEN,
        s(LANTERN),
        "start",
        "startup",
    ));
    rows.push(event(
        2,
        "2026-11-14 09:45:00",
        AWK_NO_FIRST_SEEN,
        s(LANTERN),
        "prompt",
        "hello",
    ));
    rows.push(event(
        3,
        "2026-11-01 01:30:00",
        AWK_FOLD,
        s(LANTERN),
        "prompt",
        "at the fold",
    ));
    rows.push(event(
        4,
        ts,
        AWK_NO_ACCOUNT,
        NIL,
        "stop",
        "no cwd, so no activity text",
    ));

    for path in ["Private/", "Private", "A//B"] {
        rows.push(("groups", vec![("path", s(path))]));
    }

    rows.push((
        "asks",
        vec![
            ("id", n(1)),
            ("ts", s(ts)),
            ("from_sid", s(AWK_WEIRD_STATE)),
            ("to_sid", s(AWK_GHOST)),
            ("text", s("Anyone there?")),
            ("status", s("pending")),
        ],
    ));
    rows.push((
        "meta",
        vec![("key", s("console_thread")), ("value", s("{"))],
    ));

    let mut b = brain(rows);
    b.transcripts
        .push((".acct-a".into(), AWK_NO_FIRST_SEEN.into()));
    b
}

fn old() -> SpikeBrain {
    use ids::*;
    let ts = "2026-10-01 09:00:00";
    let mut rows: Vec<(&'static str, Row)> = Vec::new();
    for (id, name, status) in [
        (1, "Lantern", "active"),
        (2, "Tidepool", "idea"),
        (3, "Driftwood", "paused"),
    ] {
        rows.push((
            "projects",
            vec![
                ("id", n(id)),
                ("name", s(name)),
                ("status", s(status)),
                ("created_at", s(ts)),
                ("updated_at", s(ts)),
            ],
        ));
    }
    for (id, title, status) in [
        (1, "Order parts", "todo"),
        (2, "Write notes", "done"),
        (3, "Plan trip", "waiting"),
    ] {
        rows.push((
            "tasks",
            vec![
                ("id", n(id)),
                ("project_id", n(1)),
                ("title", s(title)),
                ("status", s(status)),
                ("created_at", s(ts)),
                ("updated_at", s(ts)),
            ],
        ));
    }
    for (id, account, state) in [
        (OLD_1, s(A), "ended"),
        (OLD_2, s(B), "idle"),
        (OLD_3, NIL, "working"),
    ] {
        rows.push((
            "sessions",
            vec![
                ("session_id", s(id)),
                ("account", account),
                ("cwd", s(LANTERN)),
                ("title", s("An old session")),
                ("state", s(state)),
                ("first_seen", s(ts)),
                ("last_seen", s(ts)),
            ],
        ));
    }
    SpikeBrain {
        old: true,
        rows,
        ..SpikeBrain::default()
    }
}
