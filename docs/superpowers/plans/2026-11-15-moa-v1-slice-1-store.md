# moa v1 slice 1: store, spike import and export

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The v1 database (schema, migrations, backups), plus two offline tools:
- `moa adopt-spike` imports a spike brain into a **new** v1 DB and prints a report with no
  record contents in it;
- `moa export-spike` writes a v1 brain back in the spike's schema, which the fallback needs.

The user's real brains must import cleanly, and the spike's data must never change.

**Architecture:**
- `moa-core` gets the domain model (ids, enums, row structs) and time conversion. It has no
  IO.
- `moa-store` owns SQLite: open, migrate, back up, import, export.
- `moa-agents` starts with one std-only function, the Claude transcript lookup. The binary
  passes it into the import, so `moa-store` never depends on `moa-agents`.
- The `moa` binary gets the two subcommands.
- There's no daemon yet. These tools write SQLite directly and refuse to overwrite anything
  (roadmap P9).

```
 spike brain.db (+ -wal) ──read-only open──► VACUUM INTO ──► snapshot.db (temp)
                                                               │ named-column reads,
                                                               │ missing column = NULL
                                                               ▼
   ImportEnv{tz, now, accounts, ids, transcript_check} ──► import_spike ──► v1 moa.db
                                                               │  one transaction
                                                               ▼
                                              ImportReport (counts + reasons, no contents)

 v1 moa.db ──► export_spike ──► spike-schema.db (fresh path; data, then triggers;
                                 checkpoint; no -wal left)    used by `moa switch back`
```

**Tech Stack:**
- **SQLite:** `rusqlite` **0.41** (features `bundled`, `backup`, `hooks`, `functions`;
  held back because `rusqlite_migration` 2.7 requires `^0.41`), `rusqlite_migration` 2.7.
- **Time and ids:** `jiff` with its default features, `uuid` (moa-core: `serde` only;
  moa-store: `v7`), `sha2`.
- **Parsing:** `serde`/`serde_json`, `shell-words`.
- **Tests:** `tempfile`, `insta`, `proptest`.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.5: the store, the schema table and the mapping;
- §9: the dry-run report, the export and the fallback;
- §11.3.

Also R1 §1 (the spike's data model), and the roadmap's Global Constraints and P5–P9 and
P13–P19.

## Global Constraints

The roadmap's Global Constraints apply. The ones that matter most here:

- **The spike's data is never changed.** The spike's DB is opened
  `SQLITE_OPEN_READ_ONLY` without `CREATE`, with `busy_timeout=5000`, and copied with
  `VACUUM INTO` before it's read.
  - The bytes of its main file and its `-wal` never change. `-shm` is SQLite's shared
    memory: a reader may touch it, and SQLite may create an empty `-wal`/`-shm` if they're
    missing.
  - A missing DB is an error and is never created.
- **No output is ever overwritten.** `--into` must not exist, and neither may
  `--into`-wal, `-shm` or `-journal`.
- **Reports carry no record contents** (roadmap P8, made precise here):
  - Report keys are a table name plus `#<integer id>`, or the first 8 characters of the
    spike conversation id.
  - Reasons are fixed sentences that name a column at most. They never quote a value: no
    titles, text, names, group paths, paths, account names or unknown status strings.
  - Accounts appear as `account #n`. The name mapping prints only with `--names`, which is
    for the user's own terminal.
  - The source shows by file name only.
- **Instants** are `INTEGER` UTC ms. **Dates** are local `YYYY-MM-DD`. Tables are
  `STRICT`, with no `CHECK` on enum columns (P5).
- **Spike integer ids are kept** (P5). **Session ids are derived**, so they're the same
  in every run for the same brain (P17).
- **Fixtures are made up.** They use Lantern, `acct-a`, `acct-b` and `~/private`.
- **Real brains** are only read through a copy. Only the user runs the work brain's check,
  in their own terminal.

## Review Focus

1. **Real spike data that doesn't fit v1's types.**
   - Expected: every record is kept with a Warn and the import never fails. The general
     rules G1–G6 below cover this.
   - Pinned by the `import_never_fails` property over generated brains (Task 7), plus
     `awkward_brain_imports_with_warnings`.
2. **The fallback starting a second copy of a running agent.**
   - The spike resumes any live session it thinks has no pane.
   - Expected: the spike's pane links survive import → export. They're kept as unverified
     candidates (P18).
   - Pinned by `typical_round_trips` with pane/tmux compared (Task 8).
3. **Idle sessions showing "needs you".** The spike stores Claude's one-minute idle ping as
   `waiting`.
   - Expected: the same state the spike shows today.
   - Pinned by `waiting_follows_the_spikes_rule` (Task 7).
4. **The source DB while it's being written, with its data mostly in the `-wal`.** The
   user's private brain is a 139 KB file with a 3.2 MB `-wal`.
   - Expected: the snapshot includes rows that exist only in the WAL, and the main file and
     `-wal` bytes are unchanged.
   - Pinned by `snapshot_sees_wal_rows_and_changes_nothing` (Task 6).
5. **Switching back.** Each of these is pinned in Task 8:
   - the spike's `meta.cursor` = the newest event id actually exported (otherwise moa would
     replay old events, or never see new ones);
   - the activity triggers exist (the spike's `track.sh` writes without the Python setup);
   - leftover `-wal`/`-shm` files at the target are refused;
   - accounts come back as the exact strings the spike can launch.

## General import rules (every table)

These rules apply everywhere. The per-table rules only add to them.

| # | Input | Result |
|---|---|---|
| G1 | An instant column (spike local text) | Parsed with `spike_local_to_utc`. NULL or `''` → NULL, with no note. Unparsable: a nullable column → NULL + Warn; a `NOT NULL` column → that table's fallback (see below; default `env.now`) + Warn |
| G2 | A date column (`due`, `today`, `last_activity`, `date`) | `parse_date`. NULL or `''` → NULL, with no note. Unparsable → NULL + Warn |
| G3 | A closed-enum column | Unknown → that table's stated default + Warn |
| G4 | A reference to a project or session that isn't there | NULL + Warn. Foreign keys are on, so this is what keeps one dangling row from failing the transaction |
| G5 | NULL in a v1 `NOT NULL` text column | `""` + Warn. The spike's `a_event` trigger writes NULL activity text when cwd is NULL |
| G6 | A column or table missing from an older spike DB | Read as NULL or as empty, with no note |

A Warn's reason names the column, for example `#7: due isn't a date; imported as none`, and
never the value.

---

### Task 1: `moa-core` time conversion

**Files:**
- Create: `crates/moa-core/Cargo.toml`, `crates/moa-core/src/lib.rs`, `crates/moa-core/src/time.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)] pub struct UtcMs(pub i64);`
  - `pub fn spike_local_to_utc(text: &str, tz: &jiff::tz::TimeZone) -> Result<UtcMs, TimeError>`
    - Accepts `YYYY-MM-DD HH:MM:SS`, the `T` separator, and an optional fraction of a
      second. A date alone means local midnight.
    - **Rejects any offset or zone suffix** (`Z`, `+02:00`). jiff's civil parser would
      silently drop it.
    - Resolves ambiguous times with `Disambiguation::Compatible`.
  - `pub fn utc_to_spike_local(t: UtcMs, tz: &jiff::tz::TimeZone) -> String`: always
    `YYYY-MM-DD HH:MM:SS`, with the milliseconds truncated.
  - `pub fn parse_date(text: &str) -> Result<jiff::civil::Date, TimeError>`: accepts
    `YYYY-MM-DD` or the date part of a timestamp text.
  - `pub fn is_ambiguous_local(text: &str, tz: &TimeZone) -> bool`: true in a fold or a gap.
    Tests and import notes use it.
  - `pub enum TimeError { Unparsable }`. It carries no input, so error text never echoes
    data.
- `moa-core` dependencies: `jiff`, `serde`, `serde_json`, `uuid` (the `serde` feature only:
  moa-core never generates ids), `thiserror`. Dev-dependency: `proptest`.

- [ ] **Step 1: Write the failing tests** (in `time.rs`)

```rust
const ZONES: &[&str] = &["EST5EDT,M3.2.0,M11.1.0", "AEST-10AEDT,M10.1.0,M4.1.0/3",
                         "LHST-10:30LHDT-11,M10.1.0,M4.1.0", "UTC0"];
fn tz(s: &str) -> TimeZone { TimeZone::posix(s).unwrap() }
fn ny() -> TimeZone { tz(ZONES[0]) }
fn utc(s: &str) -> UtcMs { UtcMs(s.parse::<jiff::Timestamp>().unwrap().as_millisecond()) }

#[test] fn summer_and_winter() {
    assert_eq!(spike_local_to_utc("2026-07-01 12:00:00", &ny()).unwrap(), utc("2026-07-01T16:00:00Z"));
    assert_eq!(spike_local_to_utc("2026-12-01 12:00:00", &ny()).unwrap(), utc("2026-12-01T17:00:00Z"));
}
#[test] fn t_separator_fraction_and_date_only() {
    assert_eq!(spike_local_to_utc("2026-07-01T12:00:00.250", &ny()).unwrap(), utc("2026-07-01T16:00:00.250Z"));
    assert_eq!(spike_local_to_utc("2026-07-01", &ny()).unwrap(), utc("2026-07-01T04:00:00Z"));
}
#[test] fn fold_takes_the_earlier_instant() { assert_eq!(spike_local_to_utc("2026-11-01 01:30:00", &ny()).unwrap(), utc("2026-11-01T05:30:00Z")); }
#[test] fn gap_shifts_forward() { assert_eq!(spike_local_to_utc("2026-03-08 02:30:00", &ny()).unwrap(), utc("2026-03-08T07:30:00Z")); }
#[test] fn garbage_and_offsets_are_errors() {
    for bad in ["", "next tuesday", "2026-07-01 12:00:00+02:00", "2026-07-01T12:00:00Z", "2026-13-01"] {
        assert!(spike_local_to_utc(bad, &ny()).is_err(), "{bad}");
    }
}
#[test] fn parse_date_accepts_timestamp_text() { assert_eq!(parse_date("2026-07-01 12:00:00").unwrap(), jiff::civil::date(2026, 7, 1)); }

proptest! {
    #[test] fn local_text_round_trips(ms in 1_600_000_000_000i64..2_000_000_000_000i64, z in 0..ZONES.len()) {
        let zone = tz(ZONES[z]);
        let t = UtcMs(ms);
        let text = utc_to_spike_local(t, &zone);
        let back = spike_local_to_utc(&text, &zone).unwrap();
        prop_assert_eq!(utc_to_spike_local(back, &zone), text.clone());   // text is stable
        prop_assert!(back <= t);                                           // never later
        if !is_ambiguous_local(&text, &zone) {
            prop_assert_eq!(back.0, ms.div_euclid(1000) * 1000);           // exact to the second
        }
    }
    #[test] fn never_panics(s in ".*", z in 0..ZONES.len()) {
        let _ = spike_local_to_utc(&s, &tz(ZONES[z]));
        let _ = parse_date(&s);
    }
}
```

- [ ] **Step 2: Run them and see them fail** (`cargo nextest run -p moa-core`: compile errors)
- [ ] **Step 3: Implement `time.rs`**
  - Before parsing, reject a trailing `Z` or `[+-]HH:MM`.
  - Parse with `jiff::civil::DateTime` (falling back to `jiff::civil::Date`).
  - Convert with `tz.to_ambiguous_zoned(dt).compatible()`.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-core time conversion for spike local text (DST-safe)"`

---

### Task 2: `moa-core` model (ids, enums, rows)

**Files:**
- Create: `crates/moa-core/src/model.rs`

**Interfaces:**
- Produces:
  - **Ids.**
    - `SessionId(pub uuid::Uuid)`. Its `Display` is the hyphenated lowercase form.
    - `ProjectId`, `TaskId`, `NoteId`, `SuggestionId`, `AskId`, `EventId` and
      `ActivityId`, each `(pub i64)`.
  - **Closed enums**, declared with one `macro_rules! str_enum`. The macro takes the name,
    a human `what`, and the variants with their strings. It generates:
    - `ALL`, `as_str`, `FromStr` (whose error is
      `UnknownValue { what: &'static str }`, which carries no value), `Display`, and serde
      as a string;
    - a `#[cfg(test)]` round-trip test per enum.

    The strings are exactly:

    | Enum | Values |
    |---|---|
    | `ProjectStatus` | `idea active paused done` |
    | `TaskStatus` | `idea todo active waiting blocked done dropped` |
    | `Who` | `you agent other` |
    | `Priority` | `p0 p1 p2` |
    | `SuggestionStatus` | `open accepted rejected later` |
    | `AgentKind` | `claude codex` |
    | `Lifecycle` | `live ended` |
    | `Activity` | `working idle needs_permission needs_answer failed` |
    | `Location` | `here elsewhere` |
    | `AskStatus` | `pending sent received answered failed dropped` |
    | `ActivityWho` | `brain session you` |
    | `ConversationReason` | `start clear fork resume import` |
    | `ThreadKind` | `daily manual` |
  - **Open kinds.** The real data has values outside the documented lists, such as a
    `milestone` note. Each is `pub struct X(pub String)` with `pub const` known values and
    `is_known()`.
    - `NoteKind`: `FACT`, `PREFERENCE`, `ROUTINE`, `OBSERVATION`, `THREAD_SUMMARY`
      (`"thread-summary"`).
    - `EventKind`: `SESSION_STARTED="session.started"`, `SESSION_ENDED`, `TURN_STARTED`,
      `TURN_ENDED`, `TURN_FAILED`, `TURN_INTERRUPTED`, `NEEDS_PERMISSION`, `NEEDS_ANSWER`,
      `NEEDS_CLEARED`, `CWD_CHANGED`, `MODEL_CHANGED`, `UI="ui"`, and
      **`IDLE_PING="idle.ping"`** (Claude's "waiting for your input" notification: kept,
      never "needs you").
  - **Suggestion actions.** `SuggestionAction { verb: String, args: Vec<String> }`, plus
    `pub const ACTION_VERBS: &[&str] = &["task","set","archive","close","group","note","project"]`.
  - **`AskTarget`.** Either `Moa` or `Session(SessionId)`, stored as `"moa"` or the uuid.
    `Ask.to_session` is `Option<AskTarget>` (None means the target is gone: a failed ask
    kept for history). `Ask.from_session` is `Option<AskTarget>`.
  - **Row structs**, one per v1 table (Task 3's SQL). Field names equal column names. The
    types are:
    - newtypes for ids;
    - `UtcMs` for instants;
    - `jiff::civil::Date` for dates;
    - the enums above for enum columns;
    - typed JSON (`Vec<SuggestionAction>`, `serde_json::Value`);
    - `Option` for nullable columns.

    The structs are `Project`, `Task`, `Note`, `Suggestion`, `Session`, `Conversation`,
    `PaneBinding`, `SpikePane`, `Event`, `ActivityRow`, `Group`, `Ask`, `MoaThread`,
    `FeedEntry` and `Nag`.

- [ ] **Step 1: Write the failing tests**

```rust
// generated by str_enum! for every enum:  for v in X::ALL { assert_eq!(v.as_str().parse::<X>(), Ok(*v)); }
//                                          and serde_json round trip
proptest! {
    #[test] fn task_status_parses_exactly_its_strings(s in "[a-z_]{0,12}") {
        prop_assert_eq!(s.parse::<TaskStatus>().is_ok(), TaskStatus::ALL.iter().any(|v| v.as_str() == s));
    }
}
#[test] fn unknown_value_names_the_enum_not_the_value() {
    assert_eq!("someday".parse::<TaskStatus>().unwrap_err().to_string(), "not a known task status");
}
#[test] fn open_kinds_keep_unknown_values() {
    assert!(!NoteKind("milestone".into()).is_known());
    assert!(NoteKind(NoteKind::FACT.into()).is_known());
}
#[test] fn ask_target_text() {
    assert_eq!(AskTarget::Moa.to_string(), "moa");
    assert_eq!("moa".parse::<AskTarget>().unwrap(), AskTarget::Moa);
}
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `model.rs`**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-core domain model (ids, enums, rows)"`

---

### Task 3: `moa-store` schema, migrations, `Store::open`, and the Nix source filter

**Files:**
- Create: `crates/moa-store/Cargo.toml`, `src/lib.rs`, `src/schema.rs`,
  `src/sql/0001_v1.sql`, `tests/open.rs`
- Modify:
  - `Cargo.toml` (workspace deps: `rusqlite = { version = "0.41", features = [...] }  # held at 0.41 by rusqlite_migration 2.7`);
  - `flake.nix` (the source filter).

**Interfaces:**
- Produces:
  - `pub const SCHEMA_VERSION: usize = 1;`
  - `pub fn migrations() -> rusqlite_migration::Migrations<'static>`
  - `pub struct Store { conn: rusqlite::Connection, pub ahead_of_build: Option<usize> }`
  - `Store::open(db: &Path, backups: &Path) -> Result<Store, StoreError>`:
    - creates the DB and any missing parent directories with mode `0700`;
    - sets the PRAGMAs `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`,
      `busy_timeout=5000`, `temp_store=MEMORY`;
    - reads `migrations().current_version(&conn)` and acts on it:
      - `Outside(v)` (newer than this build) → **does not call `to_latest`**, which would
        fail with `DatabaseTooFarAhead`. It opens anyway with `ahead_of_build = Some(v)`;
        the additive-only rule (C5) makes that safe.
      - `Inside(v)` with `0 < v < SCHEMA_VERSION` → `backup::vacuum_into(&conn,
        backups/pre-v<SCHEMA_VERSION>.db)` (adding `-<unix ms>` if that name exists), then
        `to_latest`.
      - `NoneSet` → `to_latest`.
  - `Store::open_read_only(db: &Path) -> Result<Store, StoreError>`: never creates, and a
    missing file is `StoreError::Missing`.
  - `Store::conn(&self)` and `Store::conn_mut(&mut self)`.
  - `pub enum StoreError { Missing(PathBuf), Sqlite(rusqlite::Error), Migration(rusqlite_migration::Error), Io(std::io::Error) }`,
    each with a one-line human `Display`.
  - A crate-private `Store::open_with(db, backups, &Migrations, target)`, for tests.

- [ ] **Step 1: Write `src/sql/0001_v1.sql`** exactly as below. It's spec §6.5 plus the
  deviations in roadmap P18 and P19: `suggestions.actions_raw`,
  `conversations.spike_account`, the `spike_panes` table, and a nullable
  `asks.to_session`.

```sql
CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE, path TEXT,
  status TEXT NOT NULL DEFAULT 'active', summary TEXT, last_activity_on TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE sessions (id TEXT PRIMARY KEY, agent_kind TEXT NOT NULL, account TEXT, cwd TEXT, start_cwd TEXT,
  project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL, title TEXT, title_locked INTEGER NOT NULL DEFAULT 0,
  group_path TEXT, lifecycle TEXT NOT NULL, activity TEXT NOT NULL DEFAULT 'idle', activity_at INTEGER,
  activity_why TEXT, last_prompt TEXT, last_turn_end_at INTEGER, seen_at INTEGER, asleep_at INTEGER,
  archived_at INTEGER, location TEXT NOT NULL DEFAULT 'here', resume_failed INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, last_seen_at INTEGER) STRICT;
CREATE TABLE tasks (id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
  title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'todo', blocked_on TEXT, today TEXT,
  session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL, note TEXT, who TEXT, due TEXT, priority TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, done_at INTEGER) STRICT;
CREATE TABLE notes (id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
  kind TEXT NOT NULL DEFAULT 'fact', text TEXT NOT NULL, created_at INTEGER NOT NULL) STRICT;
CREATE TABLE suggestions (id INTEGER PRIMARY KEY, text TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'open',
  actions TEXT NOT NULL DEFAULT '[]', actions_raw TEXT, actions_invalid TEXT, result TEXT,
  created_at INTEGER NOT NULL, resolved_at INTEGER) STRICT;
CREATE TABLE conversations (agent_kind TEXT NOT NULL, conversation_id TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES sessions(id), store_dir TEXT, spike_account TEXT, reason TEXT NOT NULL,
  first_seen_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, transcript_path TEXT,
  transcript_present INTEGER, transcript_checked_at INTEGER, PRIMARY KEY (agent_kind, conversation_id)) STRICT;
CREATE INDEX conversations_session ON conversations(session_id, first_seen_at);
CREATE TABLE pane_bindings (session_id TEXT PRIMARY KEY REFERENCES sessions(id), server_pid INTEGER NOT NULL,
  server_start INTEGER NOT NULL, socket TEXT NOT NULL, pane_id TEXT NOT NULL, pane_pid INTEGER NOT NULL,
  bound_at INTEGER NOT NULL) STRICT;
CREATE TABLE spike_panes (session_id TEXT PRIMARY KEY REFERENCES sessions(id), pane_id TEXT NOT NULL,
  tmux TEXT, imported_at INTEGER NOT NULL) STRICT;
CREATE TABLE events (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, session_id TEXT, conversation_id TEXT,
  account TEXT, cwd TEXT, kind TEXT NOT NULL, text TEXT, detail TEXT) STRICT;
CREATE INDEX events_session ON events(session_id, id);
CREATE TABLE activity (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, who TEXT NOT NULL, text TEXT NOT NULL,
  ref_kind TEXT, ref_id TEXT) STRICT;
CREATE TABLE groups (path TEXT PRIMARY KEY, collapsed INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL) STRICT;
CREATE TABLE asks (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, from_session TEXT, from_name TEXT,
  to_session TEXT, text TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', reply TEXT,
  claimed_at INTEGER, answered_at INTEGER) STRICT;
CREATE TABLE moa_threads (thread_id TEXT PRIMARY KEY, account TEXT NOT NULL, day TEXT NOT NULL,
  kind TEXT NOT NULL, started_at INTEGER NOT NULL) STRICT;
CREATE TABLE feed (id INTEGER PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES moa_threads(thread_id),
  at INTEGER NOT NULL, kind TEXT NOT NULL, message_id TEXT, automatic INTEGER NOT NULL DEFAULT 0,
  body TEXT NOT NULL, state TEXT) STRICT;
CREATE TABLE nags (task_id INTEGER NOT NULL, due TEXT NOT NULL, day TEXT NOT NULL,
  PRIMARY KEY (task_id, due, day)) STRICT;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT) STRICT;
```

- [ ] **Step 2: Widen the flake's source filter.** crane's `cleanCargoSource` keeps only
  `*.rs`, `*.toml` and `Cargo.lock`. Change `src` in `flake.nix` to:

  ```nix
  src = lib.fileset.toSource { root = ./.; fileset = lib.fileset.unions [
    (craneLib.fileset.commonCargoSources ./.)
    (lib.fileset.fileFilter (f: f.hasExt "sql" || f.hasExt "snap") ./crates)
    ./spike/master-chat/schema.sql ./spike/moa-proto/picture.py   # read by Task 5's DDL check; removed with the spike
  ]; };
  ```
- [ ] **Step 3: Write the failing tests** (`tests/open.rs`):
  - `migrations_are_valid`: `migrations().validate()` passes.
  - `new_db_is_at_the_current_version_with_every_table`: the version is `SCHEMA_VERSION`,
    with exactly these 16 tables: activity, asks, conversations, events, feed, groups, meta,
    moa_threads, nags, notes, pane_bindings, projects, sessions, spike_panes, suggestions,
    tasks. Also `journal_mode` is `wal` and `foreign_keys` is 1.
  - `created_dirs_are_private`: the created parent dir has mode `0o700`.
  - `project_names_are_unique_ignoring_case`: inserting "Lantern" and then "lantern" fails
    with UNIQUE.
  - `migrating_an_older_db_backs_it_up_first`: `open_with` a 1-migration list and insert a
    project, then reopen with a 2-migration list. `backups/pre-v2.db` exists, has
    `user_version` 1 and has the project.
  - `a_db_from_a_newer_build_opens_without_migrating`: set `user_version` to 99, then
    reopen. It's `Ok`, `ahead_of_build == Some(99)`, and the version is still 99.
  - `read_only_open_never_creates`: a missing path gives `Missing`, and the path still
    doesn't exist.
- [ ] **Step 4: Run them and see them fail**
- [ ] **Step 5: Implement** `lib.rs` and `schema.rs`. Under P6, `0001_v1.sql` is edited in
  place until the switch-over.
- [ ] **Step 6: Run them and see them pass**, both with `cargo nextest run -p moa-store`
  and with `nix flake check -L`. The second run proves the `.sql` files reach the Nix
  build.
- [ ] **Step 7: Commit** `git commit -m "v1: moa-store schema v1, migrations, Store::open; nix source keeps .sql/.snap"`

---

### Task 4: Backups

**Files:**
- Create: `crates/moa-store/src/backup.rs`, `crates/moa-store/tests/backup.rs`

**Interfaces:**
- Produces:
  - `pub fn vacuum_into(conn: &Connection, dest: &Path) -> Result<(), StoreError>`. It
    refuses an existing `dest` (or `dest`-wal/-shm/-journal) **before** running any SQL.
    It creates missing parents with mode `0700`.
  - `pub fn daily_backup(store: &Store, dir: &Path, today: Date) -> Result<Option<PathBuf>, StoreError>`:
    writes `daily-YYYY-MM-DD.db` if it doesn't exist yet.
  - `pub fn backups_to_delete(existing: &[Date]) -> Vec<Date>`. It keeps:
    - the 7 newest dates;
    - of the rest, the newest date in each of the 4 most recent ISO weeks.

    It deletes the rest.
  - `pub fn prune_backups(dir: &Path) -> io::Result<Vec<PathBuf>>`: applies that rule to
    `daily-*.db` files only.

- [ ] **Step 1: Write the failing tests**

```rust
fn dates() -> impl Strategy<Value = Vec<Date>> {
    proptest::collection::btree_set(0i32..400, 0..60)
        .prop_map(|s| s.into_iter().map(|d| date(2026, 1, 1) + d.days()).collect())
}
proptest! {
    #[test] fn deletes_a_subset_and_keeps_the_newest_seven(ds in dates()) {
        let del = backups_to_delete(&ds);
        prop_assert!(del.iter().all(|d| ds.contains(d)));
        let mut sorted = ds.clone(); sorted.sort();
        for d in sorted.iter().rev().take(7) { prop_assert!(!del.contains(d)); }
        prop_assert!(ds.len() - del.len() <= 11);
    }
    #[test] fn is_idempotent_and_order_free(mut ds in dates()) {
        let kept: Vec<Date> = ds.iter().copied().filter(|d| !backups_to_delete(&ds).contains(d)).collect();
        prop_assert!(backups_to_delete(&kept).is_empty());
        ds.reverse();
        prop_assert_eq!(backups_to_delete(&ds).len(), ds.len() - kept.len());
    }
    #[test] fn pruning_daily_equals_pruning_once(ds in dates()) {
        // add the dates one by one in order, pruning after each; the survivors equal one batch prune
    }
}
#[test] fn prune_ignores_pre_migration_and_switch_backups() { /* 20 daily-*.db + pre-v2.db + pre-switch-1.db: only daily files go */ }
#[test] fn vacuum_into_refuses_an_existing_file_or_leftover_wal() { /* dest "x" exists → Err, unchanged; dest absent but dest-wal present → Err */ }
#[test] fn daily_backup_is_once_per_day() { /* Some(path) whose rows match; a second call that day → None */ }
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `backup.rs`.** The SQL is `VACUUM INTO ?1`.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: VACUUM INTO backups and 7-daily/4-weekly rotation"`

---

### Task 5: The frozen spike DDL, fixture brains, and a spike-brain generator

**Files:**
- Create:
  - `crates/moa-store/src/spike/{mod.rs, schema.sql, triggers.sql}`
    - `schema.sql` holds the `CREATE TABLE`/`INDEX` statements from
      `spike/master-chat/schema.sql`, plus those in `SETUP` in
      `spike/moa-proto/picture.py`.
    - `triggers.sql` holds the `CREATE TRIGGER` statements from `SETUP`.
    - Both are **with every SQL comment removed**. The spike's comments name real groups.
  - `crates/moa-testkit/src/spike_fixture.rs`, `crates/moa-testkit/src/spike_gen.rs`
  - `crates/moa-testkit/tests/spike_fixture.rs`
- Modify: `crates/moa-testkit/Cargo.toml`. Add `moa-store`, `rusqlite`, `serde_json` and
  `proptest`.

**Interfaces:**
- Produces:
  - `moa_store::spike::{SCHEMA_SQL, TRIGGERS_SQL}: &str`
  - `moa_testkit::SpikeFixture::write(path: &Path, scenario: Scenario, home: &Path) -> rusqlite::Result<()>`
    - It builds the DB the way the spike does: `SCHEMA_SQL`, then `TRIGGERS_SQL`, then the
      rows. The triggers fill `activity` as they do in real brains.
    - It creates the transcripts under `home`.
    - It leaves the connection **open with uncheckpointed WAL frames** only when asked
      (`SpikeFixture::write_holding_wal`, which returns the open `Connection`).
  - `pub enum Scenario { Typical, Awkward, Old }`
  - `pub mod ids`: the scenarios' fixed **conversation** ids (`S1_WORKING` … `S9_WAITING_IDLE`,
    and `AWK_*`). They're random-looking v4 UUIDs, so their 8-character prefixes are
    unique.
  - `moa_testkit::spike_gen`:
    - `pub fn brain_valid() -> impl Strategy<Value = SpikeBrain>`: well-formed data that
      should round-trip.
    - `pub fn brain_any() -> impl Strategy<Value = SpikeBrain>`. It also includes unknown
      enum strings, garbage and `''` times and dates, NULLs, dangling references, Unicode
      and newlines, group paths with empty segments, and the `Old` schema.
    - **Every free-text, name, path, group and status string is a unique 16-character
      token**, so a contents-free check can search for all of them.
    - `SpikeBrain::write(&self, path: &Path, home: &Path)`, and
      `SpikeBrain::tokens(&self) -> Vec<String>`.

  The dev-dependency cycle (`moa-store` → `moa-testkit` → `moa-store`) works for
  `moa-store/tests/*.rs`. **Never use `moa_testkit` from a `#[cfg(test)]` module inside
  `moa-store/src`**: that compiles a second copy of `moa_store`, and its types won't unify.

**`Typical`.** All data is made up. Times are `EST5EDT` local text. `home` holds the stores
`<home>/.acct-a` and `<home>/.acct-b`, and project paths `<home>/private/…`.

| Table | Rows |
|---|---|
| projects | 1 Lantern (active, `<home>/private/lantern`), 2 Tidepool (idea), 3 Driftwood (paused, path NULL) |
| sessions | S1 working, acct-a, grp `Private/Lantern`, pane `%3`, tmux `/tmp/x/moa`; S2 idle, acct-b, pane `%4`; S3 waiting acct-a (last event: a permission notification); **S9 waiting acct-a whose last event is the idle ping** (text containing "waiting for your input"); S4 ended; S5 ended with `asleep_at`; S6 `archived` (with a stale `asleep_at`); S7 `elsewhere`; S8 account `codex` ended. All have a title, cwd and `first_seen`/`last_seen` |
| events | S1–S4 and S9: `start` (text `startup`), `prompt`, `stop` (S2's stop text ends in `?`), `waiting` (S3: a permission message; S9: the idle ping), `end` (S4); three `ui` rows with session NULL |
| tasks | 1 Lantern todo you p1, due `2026-11-16`, today `2026-11-15`, session S1; 2 done agent; 3 blocked other, `blocked_on` set; 4 dropped; 5 no project, waiting, who/priority/due NULL |
| notes | `fact` (project 1), `preference`, `milestone`, `thread-summary` |
| suggestions | 1 open `["task 1 --status done"]`; 2 open `["set <S1 8-char prefix> --title \"New name\""]`; 3 rejected, action NULL; 4 later `["note --kind fact \"x y\""]` |
| groups | `Private` in the table; `Private/Lantern` only on S1 |
| asks | 1 S2→S1 answered with a reply; 2 S1→`moa` pending; 3 from NULL to S3, sent |
| meta | `cursor` = the max event id; `console_thread` = `{"sid":"<uuid>","date":"2026-11-15","account":"<home>/.acct-b"}`; `setup_skipped` = `["acct-c"]` |

Every Claude session except S2 has `<home>/.acct-*/projects/-x/<id>.jsonl`. S8 is Codex.

**`Awkward`** contains:
- projects `Lantern` and `lantern`;
- tasks with: status `someday`, due `next week`, due `''`, an unknown session, and an
  unknown project;
- a project with `created_at` set to `garbage`;
- a session with `first_seen` NULL and events (so the earliest event's time is used);
- suggestions with action `not json` and with `["rm -rf x"]`;
- sessions with state `weird`, account NULL, account `acct-z` (which has no store), and id
  `../../etc`;
- an ask to an unknown session;
- an event at the fold time `2026-11-01 01:30:00`;
- an activity row with NULL text;
- `console_thread` = `{`;
- group paths `Private/` and `Private` (which collide after normalising), and `A//B`;
- a title with a newline and CJK/emoji.

**`Old`** uses the spike's original `schema.sql` tables only, kept as `OLD_SCHEMA_SQL` in
the testkit:
- `sessions` has no `pane`, `tmux`, `grp` or `asleep_at`;
- `tasks` has no `who`, `due` or `priority`;
- `suggestions` has no `action`;
- there are no `groups`, `activity` or `asks` tables;
- there are 3 rows each in projects, tasks and sessions.

- [ ] **Step 1: Write the failing tests:**
  - `typical_has_trigger_made_activity`: the activity count is greater than 0, and some rows
    have `who='session'`.
  - `typical_matches_the_documented_counts`: projects 3, sessions 9, tasks 5, notes 4,
    suggestions 4, asks 3, meta 3.
  - `old_lacks_late_columns`: `table_info(sessions)` has no `grp`, and there's no `groups`
    table.
  - `the_spike_ddl_is_still_verbatim`:
    - For every `CREATE` statement in `spike/master-chat/schema.sql` and in `SETUP` in
      `picture.py`, strip the comments, collapse whitespace and drop `IF NOT EXISTS`. The
      result must equal the matching statement in `SCHEMA_SQL`/`TRIGGERS_SQL`, normalised
      the same way.
    - It reads the spike files from `CARGO_MANIFEST_DIR/../../spike`, and is deleted along
      with the spike.
  - `generated_brains_write_and_reopen`: a proptest with 16 cases. Every `brain_any()` value
    writes a DB that `PRAGMA integrity_check` passes.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement** the DDL files, the fixture writer and the generator.
- [ ] **Step 4: Run them and see them pass**, with `cargo nextest run -p moa-testkit` and
  with `nix flake check -L`.
- [ ] **Step 5: Commit** `git commit -m "v1: frozen spike DDL, fixture brains, spike-brain generator"`

---

### Task 6: Accounts, transcripts, the snapshot, and the import skeleton (simple tables)

**Files:**
- Create:
  - `crates/moa-store/src/accounts.rs`
  - `crates/moa-agents/{Cargo.toml, src/lib.rs, src/claude.rs}` (std only)
  - `crates/moa-store/src/import/{mod.rs, report.rs, rules.rs, simple.rs}`
  - `crates/moa-store/tests/{accounts.rs, import_simple.rs}`

**Interfaces:**
- **Transcripts.**
  - `moa_agents::claude::find_transcript(store: &Path, conversation_id: &str) -> TranscriptCheck`
  - `pub enum TranscriptCheck { Present(PathBuf), Missing, UnsafeId, NotChecked }`
    - `UnsafeId` means the id doesn't match `^[A-Za-z0-9-]{1,64}$`.
    - Otherwise it reads `store/projects/*/` and checks whether `<dir>/<id>.jsonl` is a
      file. It checks existence only and never reads a transcript.
- **Accounts.** `moa_store::accounts::AccountMap`:
  - `AccountMap::new(home: PathBuf)`
  - `.with(name, store)`
  - `AccountMap::parse_flag(s, home) -> Result<(String, PathBuf), String>`: takes
    `NAME=STORE` and expands `~`.
  - `.resolve(spike_account: &str) -> ResolvedAccount { name: String, store: Option<PathBuf>, kind: AgentKind }`
  - `.store_for(name) -> Option<PathBuf>`
  - `.index_of(name) -> usize`: the `#n` used in reports, in first-seen order.
- **Import types** in `moa_store::import`:
  - `ImportEnv { tz: TimeZone, now: UtcMs, brain: String, accounts: AccountMap, ids: IdSource, transcript: fn(&Path, &str) -> TranscriptCheck }`
  - `IdSource`:
    - `Derived`: `Uuid` v7 built with `uuid::Builder::from_unix_timestamp_millis(created_at_ms, &rand)`,
      where `rand` = the first 10 bytes of `SHA-256(brain ‖ 0x00 ‖ conversation_id)`. The
      same brain and conversation always give the same id, and ids sort by start time
      (P17).
    - `Sequential(u64)`: `00000000-0000-7000-8000-{n:012x}`, for unit tests.
- **Import functions:**
  - `snapshot_spike(src: &Path, dest: &Path) -> Result<(), ImportError>`
  - `import_spike(spike_db: &Path, dst: &mut Store, env: &ImportEnv) -> Result<ImportReport, ImportError>`
  - `ImportError { SpikeMissing(PathBuf), NotEmpty, Store(StoreError), Sqlite(rusqlite::Error), Io(io::Error) }`
- **Report types:**
  - `ImportReport { source_name: String, tables: Vec<TableCounts>, notes: Vec<RecordNote>, accounts: Vec<String>, setup_skipped: Vec<String> }`
    - It derives `Serialize`.
    - `accounts` and `setup_skipped` are printed only by `render_text(names: bool)` with
      `names = true`, and are left out of JSON unless `--names` is given.
    - Methods: `render_text(&self, names: bool) -> String` and
      `has_warnings(&self) -> bool`.
  - `TableCounts { table: &'static str, source: usize, imported: usize, warned: usize }`
  - `RecordNote { table: &'static str, key: String, level: Level, reason: &'static str }`
    - `Level::{Info, Warn}`. There is no Skip: every record is kept.
    - `reason` is a `&'static str`, so a reason can't carry data, and the type system
      enforces it.
- **Rules module.** `import::rules` holds G1–G6 as small pure functions, each returning
  `(value, Option<&'static str>)`: `instant`, `instant_or`, `date`, `closed`,
  `reference`, `text_not_null` and `group_path`. Every table importer is built from these
  helpers, and none repeats the logic.

- [ ] **Step 1: Write the failing tests**

`accounts.rs`, as examples:

| Input | Expected |
|---|---|
| `acct-a` with `<home>/.acct-a` present | `acct-a`, store present |
| `work`, with an explicit entry `work` → `<home>/.other` | store `<home>/.other` |
| `other`, where `work` owns `<home>/.other` | `work` |
| absolute `<home>/.acct-b` | `acct-b` |
| `codex` | Codex, store None |
| `acct-z` with no dir | store None |

And one property:

```rust
proptest! { #[test] fn resolve_is_idempotent(name in "[a-z][a-z0-9-]{0,12}") {
    let m = fixture_map(); let once = m.resolve(&name).name;
    prop_assert_eq!(m.resolve(&once).name, once);
} }
```

`find_transcript`: `Present` when the file is there; `Missing` when it's absent or there's
no `projects/`; `UnsafeId` for `../../etc` and for `""`.

`snapshot_sees_wal_rows_and_changes_nothing`:
1. `SpikeFixture::write_holding_wal`, with the writer set to `wal_autocheckpoint=0` and
   holding WAL frames. Then insert 50 more projects through the open writer.
2. Hash the main file and `-wal`, and list the directory.
3. `snapshot_spike`.
4. The snapshot has all 53 projects. The main file and `-wal` hashes are unchanged, and no
   new file names appear except `-wal`/`-shm`.
5. Calling it on a missing path gives `SpikeMissing`, and the path still doesn't exist.

`simple_tables_import` (Typical):
- projects 1–3 are imported with converted times, and `last_activity_on` as a date;
- 4 notes, with `milestone` kept;
- groups `["Private", "Private/Lantern"]`;
- the activity count and ids equal the source;
- 1 `moa_threads` row (`acct-b`, day `2026-11-15`, kind daily, `started_at` = that local
  midnight);
- `report.setup_skipped == ["acct-c"]`;
- `meta.spike_import` holds `{source_name, at, tables, setup_skipped}`;
- an Info note: "meta cursor not carried (v1 has one channel)".

`import_refuses_a_non_empty_target`: a second import gives `NotEmpty`, and the first
import's rows are unchanged.

`rules.rs` unit tests, one per G rule: for each, the value it gives and whether it Warns.

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
  - **`AccountMap::resolve` order:**
    1. `codex`.
    2. An explicit entry by that name.
    3. An absolute path: the explicit entry whose store equals it (after `~` expansion and
       best-effort `canonicalize`). Otherwise the name is the basename minus a leading
       `.`, and the store is that path if it's a directory.
    4. `<home>/.<name>` is a directory: the explicit entry that owns it, otherwise the name
       itself.
    5. Otherwise the name, with no store.
  - **`snapshot_spike`:**
    1. Check `exists()`.
    2. Open with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` and set `busy_timeout`
       to 5000.
    3. `VACUUM INTO dest`.
  - **`import_spike`:**
    1. Snapshot into a `TempDir`.
    2. Open the snapshot read-only.
    3. Read with named columns. G6 uses `pragma table_info` and `sqlite_master`.
    4. Write everything in **one** `dst` transaction.
    5. `NotEmpty` means any row in projects, sessions or tasks.
  - **Per-table rules for this task**, on top of G1–G6:
    - **projects:**
      - A name that clashes ignoring case gets ` (2)`, ` (3)` and so on, plus the Warn
        "name clashes with another project ignoring case; renamed with a suffix".
      - An unknown status → `active`.
      - Missing times fall back to `env.now`.
      - `last_activity` → G2.
    - **notes:** the kind is kept verbatim.
    - **groups:**
      - Take the union of `groups.path` and the distinct `sessions.grp` values.
      - Normalise: trim `/` and drop empty segments. Warn when that changes the path.
      - **Then de-duplicate.**
      - `collapsed` = 0. `created_at` = the source's value, or `env.now`.
    - **activity:** an unknown `who` → `brain`. NULL text → G5. A bad `at` falls back to
      `env.now`.
    - **meta:**
      - `console_thread` → one `moa_threads` row; a malformed value gives a Warn and no
        row.
      - `setup_skipped` → the report and `meta.spike_import`.
      - `cursor` → an Info note. Other keys → the Info "meta key not carried".
  - **Report keys:** `#<id>` for the integer tables, and the spike conversation id's first
    8 characters for sessions and conversations.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: import skeleton, snapshot, accounts, transcripts, rules, simple tables"`

---

### Task 7: Import sessions and everything that points at them

**Files:**
- Create: `crates/moa-store/src/import/sessions.rs`, `crates/moa-store/tests/import_sessions.rs`
- Modify: `crates/moa-store/src/import/mod.rs`. The order is projects → sessions → the rest.

**Interfaces:**
- Consumes: Task 6's types and rules.
- Produces: `import_spike` now also fills sessions, conversations, `spike_panes`, events,
  tasks, suggestions and asks.

**Sessions.** Each spike session gets `SessionId` from `env.ids`, keyed by its spike
`session_id` (the conversation id).

| v1 column | Value |
|---|---|
| agent_kind, account | from `accounts.resolve(account)`; a NULL account → `claude`/NULL + Warn "account unknown" |
| cwd, title, last_prompt, last_seen_at (= last_seen) | copied (G1 for times) |
| created_at | `first_seen` → else the earliest event `ts` → else `last_seen` → else `env.now`, + Warn when it fell back |
| start_cwd | cwd of the earliest event with a cwd, else cwd |
| project_id | G4 |
| title_locked | 1 when there's a title |
| group_path | the normalised `grp` |
| activity_at | the `ts` of the latest event among prompt, stop and waiting (excluding the idle ping), else `last_seen` |
| activity_why | the spike's "why" (`picture.py:live`): working → `last_prompt` on one line; needs_permission → that event's text on one line, or `needs you`; otherwise the last line of the latest stop text, stripped of ` *_#`. Blank if it starts with `<` |
| last_turn_end_at | the max `ts` of its `stop` events |
| seen_at | `env.now`, so nothing shows "done, unseen" after the import |
| asleep_at | copied **only when the state is `ended`**; otherwise NULL + Info "stale asleep mark dropped" |
| resume_failed | 0 |

**State**, following the spike's own reading (`picture.py` `LIVE_SQL` and `live()`). Here
"latest" means the newest event among prompt, stop and waiting, ignoring idle pings, which
are waiting events whose text contains `waiting for your input`.

| spike state | latest | lifecycle | activity | location | archived_at |
|---|---|---|---|---|---|
| working | any | live | working | here | NULL |
| waiting | a `waiting` event | live | needs_permission | here | NULL |
| waiting | anything else | live | idle | here | NULL |
| idle | any | live | idle | here | NULL |
| ended | — | ended | idle | here | NULL |
| archived | — | ended | idle | here | last_seen |
| elsewhere | — | ended | idle | elsewhere | NULL |
| other / NULL | — | ended | idle | here | NULL, + Warn "unknown state; imported as ended" |

"Asks you" (the spike's `question`) isn't stored. The slice-2 live model derives it from
`activity_why` with the spike's `asks()` rule: the last line, stripped of `` *_`)"' ``, ends
in `?`.

**Other tables:**
- **spike_panes:** a session with a non-NULL `pane` gets
  `(session_id, pane, tmux, env.now)`. A live session gets Info "pane not verified yet
  (checked at switch-over)". Slice 4's reconcile and slice 10's step 6 verify these or
  clear them (P18).
- **conversations:** one row per session, with:
  - `conversation_id` = the spike `session_id`;
  - `reason` = import;
  - `store_dir` from `resolve`;
  - **`spike_account` = the raw spike `account` string**, which the export gives back
    exactly;
  - `env.transcript(store, id)` → `transcript_present`, `transcript_path` and
    `transcript_checked_at`.

  `Missing` → Warn "transcript missing; resume will fail (account #n)". `UnsafeId` → Warn.
  Codex or no store → NULL fields, with Info "transcript not checked".
- **events:** ids are kept.
  - `session_id` = the mapped id, or NULL with no note.
  - `conversation_id` = the spike's session id.
  - `account` = the resolved name.
  - Kinds map: `start`→`session.started`, `prompt`→`turn.started`, `stop`→`turn.ended`,
    `waiting` → `needs.permission`, **or `idle.ping`** when the text contains
    `waiting for your input`, `end`→`session.ended`, `ui`→`ui`. Any other kind is kept
    verbatim, with a Warn.
- **tasks:** ids are kept.
  - G4 applies to both `session_id` and `project_id`.
  - Unknown values: status → `todo`, `who`/`priority` → NULL. G2 for `due` and `today`.
  - The time fallback is `env.now`.
  - `done_at` = `updated_at` when the status is done. That's approximate, because the spike
    bumps `updated_at` on any edit.
- **suggestions:** ids are kept.
  - **`actions_raw` = the spike `action` string, verbatim.**
  - A JSON list of strings → each one through `shell_words::split` → `{verb, args}`.
  - Not JSON, not a list of strings, a split error, or a verb outside `ACTION_VERBS` →
    `actions = []` and `actions_invalid` = a fixed reason, + Warn.
  - Session references in args stay as written: spike conversation prefixes. They resolve
    because slice 2's name lookup accepts conversation-id prefixes (roadmap slice 2,
    item 8).
- **asks:** ids are kept.
  - `to_sid`: `moa` → `Moa`; a known session → mapped; unknown → **NULL with status
    `failed`** + Warn "target session isn't in this brain".
  - `from_sid`: `moa` → `Moa`; a known session → mapped; anything else → NULL.
  - **In-flight asks are closed**, because nobody is waiting for them after the switch:
    - `pending` → `dropped`;
    - `sent` and `received` → `failed`, with the reply "moa switched over before it was
      answered". Each gets an Info note.
  - `claimed_at` = `ts` for asks that were sent, received or answered (the spike restamps
    `ts` when it types an ask in).
  - `at` = `ts`.
  - An unknown status → `failed` + Warn.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn typical_brain_imports_cleanly() {
    // tz EST5EDT, now fixed, ids Derived{brain:"default"}
    // the report has exactly one Warn: S2's transcript missing; Info notes allowed
    // 9 sessions; S1 live/working, group_path "Private/Lantern", title_locked 1
    // S3 needs_permission; S9 live/idle (idle ping) and its waiting event has kind idle.ping
    // S2 idle with activity_why = the last line of its stop text (ends in "?")
    // S5 ended + asleep_at; S6 archived_at set and asleep_at NULL; S7 location elsewhere
    // S8 agent_kind codex, transcript fields NULL
    // spike_panes: S1 (%3, /tmp/x/moa) and S2 (%4)
    // conversations: 9, conversation_id == spike id, spike_account == the raw string
    // task #1.session_id == S1's moa id; suggestion #2 args[0] == S1's 8-char conversation prefix
    // asks: #1 answered with claimed_at; #2 dropped (was pending); #3 failed (was sent)
    // TableCounts: source == imported for every table
}
#[test] fn ids_are_the_same_on_every_run() {
    // import Typical twice into two stores with Derived{brain:"default"}: identical session ids
    // a different brain name → different ids; ids sort in created_at order
}
#[test] fn waiting_follows_the_spikes_rule() { /* S3 vs S9 as above, also with the ping older than a permission event */ }
#[test] fn awkward_brain_imports_with_warnings() {
    // Ok; Warn per documented case; the ask to an unknown session is kept as failed with to_session NULL
    // "Lantern" / "lantern (2)"; groups contain "Private" once and "A/B"; the fold event at 05:30Z
    // the activity row with NULL text → ""; the session with NULL first_seen → created_at = its earliest event
}
#[test] fn old_brain_without_late_columns_imports() { /* 3+3+3 imported, group_path NULL, no Warn */ }
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test] fn import_never_fails(b in brain_any()) { /* write b, import → Ok, every source row counted as imported */ }
    #[test] fn reports_are_contents_free(b in brain_any()) {
        // render_text(false) and serde_json::to_string(&report) contain none of b.tokens() and no home path
    }
}
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `import/sessions.rs`** to the tables above.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: import sessions, conversations, spike panes, events, tasks, suggestions, asks"`

---

### Task 8: Export back to the spike schema, and the round trip

**Files:**
- Create: `crates/moa-store/src/export.rs`, `crates/moa-store/tests/round_trip.rs`

**Interfaces:**
- Produces:
  - `ExportEnv { tz: TimeZone, accounts: AccountMap }`. `setup_skipped` comes from
    `meta.spike_import`, overridable by the CLI.
  - `export_spike(src: &Store, dst: &Path, env: &ExportEnv) -> Result<ExportReport, ExportError>`
  - `ExportReport { tables: Vec<TableCounts>, notes: Vec<RecordNote> }`, with the same
    `render_text`.
  - `ExportError { Exists(PathBuf), Store(StoreError), Sqlite(rusqlite::Error) }`

**Steps:**
1. Refuse when `dst`, `dst-wal`, `dst-shm` or `dst-journal` exists, or when `dst`'s name
   ends in one of those suffixes.
2. Create it with `SCHEMA_SQL` and `journal_mode=WAL`, and insert the data.
3. **Then run `TRIGGERS_SQL`.** That's what the spike's `connect()` would do, and it
   creates no duplicate activity rows because it runs after the data is in. The spike's
   `track.sh` writes without `connect()`, so the triggers have to exist already.
4. `PRAGMA wal_checkpoint(TRUNCATE)`, close, and assert that no `dst-wal` remains, or that
   it's empty.

**Rules:**
- **sessions:**
  - `session_id` = the **current** conversation (latest `first_seen_at`, ties broken by
    rowid). A session with no conversation → Warn and not exported. It has no spike
    identity.
  - **state:**
    - `archived_at` → `archived`;
    - `elsewhere` → `elsewhere`;
    - `ended` → `ended`;
    - live `working` → `working`;
    - live `needs_permission` → `waiting`;
    - live `idle`, `needs_answer` or `failed` → `idle`, with Info for `failed`. The spike
      derives "question" from the stop text by itself.
  - **pane/tmux:** from `pane_bindings` (`pane_id`, `socket`), or else from `spike_panes`.
    A live session with neither → Warn "live session has no pane; the spike would resume it
    in a new window".
  - **account:**
    - The `spike_account` of its current conversation, verbatim.
    - For a session v1 created, the absolute store path, which the spike's launch and
      resume handle.
    - A Codex session → exactly `codex`.
  - `grp` = `group_path`. Times use `utc_to_spike_local`.
- **tasks/asks:** session references → the current conversation id, or `moa`. An ask with
  `to_session` NULL → `to_sid` set to its spike placeholder `''`, status failed.
- **events:**
  - `session_id` = the row's `conversation_id`.
  - Kinds are reversed: `idle.ping` → `waiting` with its text. Other kinds have no spike
    equivalent; they're skipped and counted, with one Info per kind.
- **suggestions:**
  - If `actions_invalid` is set → `actions_raw`.
  - Otherwise, a JSON list of `shell_words::join([verb] + args)`. Any arg that's a unique
    prefix of a v1 moa session id is rewritten to that session's current conversation id.
- **projects:** `area` = `private`, `last_activity` = the date text.
- **activity, notes, groups:** copied.
- **meta:**
  - `cursor` = **the largest event id actually exported**;
  - `console_thread` = the newest `moa_threads` row as
    `{sid, date: day, account: abspath(expanduser(store))}`, not canonicalised, which is
    how `console.py` compares it;
  - `setup_skipped` as above.
- **Not exported** (one Info each): feed, nags, conversation history beyond the current
  conversation, the generation fields of `pane_bindings`, `tasks.done_at`,
  `asks.claimed_at`, `activity.ref_*`, `groups.collapsed`, `sessions.seen_at`,
  `title_locked`, `activity_why`, and `resume_failed`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn typical_round_trips() {
    // Typical → import → export, then compare with the fixture table by table, rows ordered by PK,
    // EVERY column (including sessions.pane/tmux and account), except:
    //   meta.cursor (asserted == max exported event id)
    //   groups: the export == {"Private","Private/Lantern"} ⊇ the source
    //   asks: status follows Task 7's closing rules (#2 dropped, #3 failed + its reply)
    //   suggestions.action: compared after JSON-parse + shell_words::split per entry
    // every timestamp is equal as text
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test] fn valid_brains_round_trip(b in brain_valid()) { /* the same comparison as above, generically */ }
}
#[test] fn bindings_win_over_spike_panes() { /* add a pane_bindings row for S1 (%9) → the export has %9; S2 keeps %4 from spike_panes */ }
#[test] fn export_follows_rotation() { /* a 2nd conversation for S1 → the spike session id and task #1 use the new id */ }
#[test] fn cursor_is_the_newest_exported_event() { /* the newest v1 event is turn.failed (skipped) → cursor == the id before it */ }
#[test] fn triggers_exist_and_activity_is_not_duplicated() {
    // sqlite_master has all 9 triggers; activity count == v1's; inserting an event row afterwards adds one activity row
}
#[test] fn export_refuses_existing_targets_and_leftovers() { /* dst exists; dst-wal exists; dst named x.db-wal → each Exists, nothing changed */ }
#[test] fn export_leaves_no_wal() { /* after the export: no dst-wal, or it's 0 bytes */ }
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `export.rs`**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: export to the spike schema (fallback) with pane links, triggers, cursor; round-trip properties"`

---

### Task 9: `moa adopt-spike` and `moa export-spike`

**Files:**
- Create: `crates/moa/src/adopt.rs`, `crates/moa/tests/adopt.rs`,
  `crates/moa-store/tests/local_brain.rs`
- Modify: `crates/moa/src/main.rs` (subcommands), `crates/moa/Cargo.toml` (`moa-store`,
  `moa-agents`, `moa-core`)

**Interfaces:**
- Consumes: `import_spike`, `export_spike`, `AccountMap::parse_flag`,
  `moa_agents::claude::find_transcript`, `Store::open` and `Store::open_read_only`.
- Produces the CLI:
  - `moa adopt-spike --from <SPIKE_DB> [--into <V1_DB>] [--brain NAME] [--account NAME=STORE]... [--tz ZONE] [--names] [--json]`
  - `moa export-spike --from <V1_DB> --into <SPIKE_DB> [--account NAME=STORE]... [--tz ZONE] [--setup-skipped NAME]...`

**Behaviour:**
- **Dry run.** Without `--into`, the import goes into a temp dir. The tool prints the
  report, ends with `dry run: nothing written`, and deletes the temp dir.
- **`--into`:**
  - It refuses an existing path, or a leftover `-wal`/`-shm`/`-journal` next to it:
    `<file> already exists; moa never overwrites a brain` (exit 1).
  - It refuses `--into` equal to `--from` after canonicalising.
- **A missing `--from`:** `no spike brain at <file>` (exit 1), and nothing is created.
- **`--brain`** defaults to `default`. It feeds `IdSource::Derived`.
- **`--tz`** accepts an IANA name (`TimeZone::get`) or a POSIX string
  (`TimeZone::posix`). The default is `TimeZone::try_system()`. If that fails:
  `can't tell your time zone; pass --tz` (exit 1). It never falls back to UTC silently.
- **Exit codes:** 0 when the import ran, warnings included. 1 on a hard error.
- **`export-spike`** has the same refusals, plus `no v1 brain at <file>`.

- [ ] **Step 1: Write the failing tests** (`crates/moa/tests/adopt.rs`). They use
  assert_cmd and the Typical fixture in a temp home. The child process gets `HOME`, and
  every invocation passes `--tz EST5EDT,M3.2.0,M11.1.0`.

```rust
#[test] fn dry_run_prints_counts_and_writes_nothing() { /* "projects", "sessions", "dry run: nothing written"; dir listing unchanged; fixture db + wal hashes unchanged */ }
#[test] fn into_writes_a_v1_brain() { /* exit 0; Store::open_read_only(into) has 9 sessions */ }
#[test] fn into_an_existing_file_or_leftover_wal_is_refused() { /* exit 1, "already exists; moa never overwrites a brain", nothing changed */ }
#[test] fn missing_source_is_refused_and_not_created() { /* exit 1, "no spike brain at", path not created */ }
#[test] fn bad_tz_is_refused() { /* --tz Nowhere/Land → exit 1 */ }
#[test] fn output_is_contents_free_without_names() { /* stdout+stderr of a dry run and of --json contain none of SECRET texts, account names, or the home path */ }
#[test] fn names_flag_shows_accounts() { /* --names → "acct-a" appears */ }
#[test] fn export_spike_round_trip_via_cli() { /* adopt-spike --into, then export-spike --into → 9 sessions, triggers present */ }
```

`crates/moa-store/tests/local_brain.rs`:
- **Local-only.** It runs only when `MOA_LOCAL_BRAIN_COPY=<path>` is set, and returns
  immediately otherwise.
- It imports the copy, asserts `Ok`, and asserts the round trip with the same comparison as
  `typical_round_trips`.
- It prints only the counts and the reasons.
- It also needs `MOA_LOCAL_BRAIN_ACCOUNTS="NAME=STORE,…"` and `MOA_LOCAL_BRAIN_TZ`.

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `adopt.rs`** (clap subcommands, wiring, `render_text`)
- [ ] **Step 4: Run them and see them pass**, and run the whole workspace with clippy and
  `nix flake check -L`.
- [ ] **Step 5: The private brain, on a copy** (done by the implementer)

```bash
python3 -c "import sqlite3; s=sqlite3.connect('file:spike/master-chat/brain.db?mode=ro',uri=True); d=sqlite3.connect('/tmp/moa-private-copy.db'); s.backup(d)"
MOA_LOCAL_BRAIN_COPY=/tmp/moa-private-copy.db MOA_LOCAL_BRAIN_TZ=<your zone> MOA_LOCAL_BRAIN_ACCOUNTS=<pairs> \
  cargo nextest run -p moa-store --test local_brain --no-capture
nix run . -- adopt-spike --from /tmp/moa-private-copy.db --account <pairs…>
```

The account pairs are for accounts whose store isn't `~/.<name>`. They're in the spike's
`CONFIG_DIRS` (`spike/moa-proto/moactl.py`). Type them on the command line only, never into
a file, test or PR text.

Expected:
- the test passes;
- the dry run exits 0;
- every Warn reason is understood, and listed in the PR description **as the reason plus a
  count**.

Delete the copy afterwards.
- [ ] **Step 6: The work brain** (the user runs this)

Ask the user to run the same two commands in their own terminal, with the work brain's
path, and to report back:
- whether the test passed;
- any Warn reason that surprised them.

Don't run it from this session.
- [ ] **Step 7: Commit, push, and open the PR**

```bash
git commit -m "v1: moa adopt-spike (dry run by default) and moa export-spike"
git push -u origin v1/slice-1-store
gh pr create --title "v1 slice 1: store, spike import and export" --body "<summary; the private-brain warnings as reason + count; Restart needed: nothing; the spike is untouched>"
```

- [ ] **Step 8: Review and hand over.**
  1. Run `/code-review medium --fix`, push the fixes, and wait for CI to go green.
  2. Then tell the user how to dry-run a brain:
     `nix run . -- adopt-spike --from <copy of brain.db> [--account NAME=STORE]`.
  3. Say "Restart needed: nothing; the spike is untouched."
