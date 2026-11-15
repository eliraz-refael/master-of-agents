use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use moa_store::spike::{SCHEMA_SQL, TRIGGERS_SQL};
use moa_testkit::spike_gen::{brain_any, brain_valid};
use moa_testkit::{OLD_SCHEMA_SQL, Scenario, SpikeFixture, ids};
use proptest::prelude::*;
use rusqlite::Connection;

fn build(scenario: Scenario) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("brain.db");
    SpikeFixture::write(&db, scenario, dir.path()).unwrap();
    let conn = Connection::open(&db).unwrap();
    (dir, conn)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn typical_has_trigger_made_activity() {
    let (_dir, conn) = build(Scenario::Typical);
    assert!(count(&conn, "SELECT count(*) FROM activity") > 0);
    assert!(count(&conn, "SELECT count(*) FROM activity WHERE who='session'") > 0);
}

#[test]
fn typical_matches_the_documented_counts() {
    let (_dir, conn) = build(Scenario::Typical);
    for (table, n) in [
        ("projects", 3),
        ("sessions", 9),
        ("tasks", 5),
        ("notes", 4),
        ("suggestions", 4),
        ("asks", 3),
        ("meta", 3),
    ] {
        assert_eq!(
            count(&conn, &format!("SELECT count(*) FROM {table}")),
            n,
            "{table}"
        );
    }
    let cursor: String = conn
        .query_row("SELECT value FROM meta WHERE key='cursor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        cursor,
        count(&conn, "SELECT max(id) FROM events").to_string()
    );
}

#[test]
fn typical_writes_transcripts_except_for_s2_and_codex() {
    let dir = tempfile::tempdir().unwrap();
    SpikeFixture::write(&dir.path().join("b.db"), Scenario::Typical, dir.path()).unwrap();
    let has = |acct: &str, id: &str| {
        dir.path()
            .join(acct)
            .join("projects/-x")
            .join(format!("{id}.jsonl"))
            .exists()
    };
    assert!(has(".acct-a", ids::S1_WORKING));
    assert!(has(".acct-b", ids::S4_ENDED));
    assert!(!has(".acct-b", ids::S2_IDLE));
    assert!(!has(".acct-a", ids::S8_CODEX) && !has(".acct-b", ids::S8_CODEX));
}

#[test]
fn old_lacks_late_columns() {
    let (_dir, conn) = build(Scenario::Old);
    let has_col = conn
        .prepare("SELECT name FROM pragma_table_info('sessions')")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .any(|c| c.unwrap() == "grp");
    assert!(!has_col);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name='groups'"
        ),
        0
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM sessions"), 3);
    assert!(OLD_SCHEMA_SQL.contains("CREATE TABLE"));
}

#[test]
fn awkward_opens_and_has_its_oddities() {
    let (_dir, conn) = build(Scenario::Awkward);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM activity WHERE text IS NULL"),
        1
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sessions WHERE session_id='../../etc'"
        ),
        1
    );
}

#[test]
fn write_holding_wal_leaves_frames_in_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.db");
    let _held = SpikeFixture::write_holding_wal(&db, Scenario::Typical, dir.path()).unwrap();
    let wal = fs::metadata(dir.path().join("w.db-wal")).unwrap();
    assert!(wal.len() > 0);
}

fn read(p: &Path) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../spike")
            .join(p),
    )
    .unwrap()
}

fn strip_comments(sql: &str) -> String {
    sql.lines()
        .map(|l| l.split("--").next().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalise(sql: &str) -> String {
    strip_comments(sql)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("IF NOT EXISTS ", "")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// Every `CREATE` statement of a script, normalised.
fn creates(sql: &str) -> BTreeSet<String> {
    let flat = normalise(sql);
    let mut starts: Vec<usize> = flat.match_indices("CREATE ").map(|(i, _)| i).collect();
    starts.push(flat.len());
    starts
        .windows(2)
        .map(|w| {
            flat[w[0]..w[1]]
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string()
        })
        .collect()
}

fn columns_of(create_table: &str) -> Vec<String> {
    let body = &create_table[create_table.find('(').unwrap() + 1..create_table.rfind(')').unwrap()];
    let (mut depth, mut cur, mut out) = (0, String::new(), Vec::new());
    for ch in body.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out.iter()
        .map(|c| c.split_whitespace().next().unwrap().to_string())
        .collect()
}

#[test]
fn the_spike_ddl_is_still_verbatim() {
    let schema = read(Path::new("master-chat/schema.sql"));
    let picture = read(Path::new("moa-proto/picture.py"));
    let setup = picture
        .split("SETUP = \"\"\"")
        .nth(1)
        .unwrap()
        .split("\"\"\"")
        .next()
        .unwrap();

    let spike: BTreeSet<String> = creates(&schema).union(&creates(setup)).cloned().collect();
    let ours: BTreeSet<String> = creates(SCHEMA_SQL)
        .union(&creates(TRIGGERS_SQL))
        .cloned()
        .collect();
    assert_eq!(spike, ours);
    assert_eq!(creates(TRIGGERS_SQL).len(), 9);
    assert!(!SCHEMA_SQL.contains("--") && !TRIGGERS_SQL.contains("--"));

    // The columns the spike adds with ALTER TABLE exist in our CREATE TABLEs.
    let mut pairs: Vec<(String, String)> = Vec::new();
    let tuple_list = picture
        .split("for table, col in (")
        .nth(1)
        .unwrap()
        .split("):")
        .next()
        .unwrap();
    for part in tuple_list.split("(\"").skip(1) {
        let mut q = part.split('"');
        let table = q.next().unwrap().to_string();
        let col = q.nth(1).unwrap().to_string();
        pairs.push((table, col));
    }
    for line in picture
        .lines()
        .filter(|l| l.contains("ADD COLUMN ") && l.contains("TEXT"))
    {
        let words: Vec<&str> = line.split_whitespace().collect();
        let at = words.iter().position(|w| *w == "TABLE").unwrap();
        if words[at + 1].starts_with('{') {
            continue;
        }
        pairs.push((words[at + 1].to_string(), words[at + 4].to_string()));
    }
    assert!(pairs.len() >= 8, "found {pairs:?}");
    assert!(pairs.contains(&("sessions".into(), "asleep_at".into())));
    let ours = creates(SCHEMA_SQL);
    for (table, col) in pairs {
        let stmt = ours
            .iter()
            .find(|s| s.starts_with(&format!("CREATE TABLE {table} (")))
            .unwrap_or_else(|| panic!("no table {table}"));
        assert!(columns_of(stmt).contains(&col), "{table}.{col}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn generated_brains_write_and_reopen(brain in brain_any()) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.db");
        brain.write(&db, dir.path());
        let conn = Connection::open(&db).unwrap();
        let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        prop_assert_eq!(ok, "ok");
        for t in brain.tokens() {
            prop_assert_eq!(t.chars().count(), 16);
        }
    }

    #[test]
    fn valid_brains_have_unique_tokens_and_only_private_areas(brain in brain_valid()) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("v.db");
        brain.write(&db, dir.path());
        let conn = Connection::open(&db).unwrap();
        prop_assert_eq!(count(&conn, "SELECT count(*) FROM projects WHERE area != 'private'"), 0);
        let tokens = brain.tokens();
        let unique: BTreeSet<_> = tokens.iter().collect();
        prop_assert_eq!(unique.len(), tokens.len());
    }

    #[test]
    fn brain_valid_meets_the_round_trip_contract(brain in brain_valid()) {
        const TIME: &str = "'[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]'";
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        brain.write(&db, dir.path());
        let conn = Connection::open(&db).unwrap();
        let bad_times = [
            ("sessions", "first_seen"), ("sessions", "last_seen"), ("projects", "created_at"),
            ("projects", "updated_at"), ("tasks", "created_at"), ("tasks", "updated_at"),
            ("asks", "ts"), ("groups", "created_at"), ("events", "ts"), ("notes", "created_at"),
            ("suggestions", "created_at"), ("activity", "ts"),
        ];
        let mut violations: Vec<(String, i64)> = bad_times
            .iter()
            .map(|(t, c)| (
                format!("{t}.{c} is not a time"),
                count(&conn, &format!("SELECT count(*) FROM {t} WHERE {c} IS NULL OR NOT {c} GLOB {TIME}")),
            ))
            .collect();
        let checks = [
            ("event without a session", "SELECT count(*) FROM events WHERE kind != 'ui' AND (session_id IS NULL OR session_id NOT IN (SELECT session_id FROM sessions))"),
            ("ui event with a session", "SELECT count(*) FROM events WHERE kind = 'ui' AND session_id IS NOT NULL"),
            ("task session dangles", "SELECT count(*) FROM tasks WHERE session_id IS NOT NULL AND session_id NOT IN (SELECT session_id FROM sessions)"),
            ("ask session dangles", "SELECT count(*) FROM asks WHERE (from_sid IS NOT NULL AND from_sid NOT IN (SELECT session_id FROM sessions)) OR (to_sid != 'moa' AND to_sid NOT IN (SELECT session_id FROM sessions))"),
            ("project reference dangles", "SELECT (SELECT count(*) FROM tasks WHERE project_id IS NOT NULL AND project_id NOT IN (SELECT id FROM projects)) + (SELECT count(*) FROM notes WHERE project_id IS NOT NULL AND project_id NOT IN (SELECT id FROM projects)) + (SELECT count(*) FROM sessions WHERE project_id IS NOT NULL AND project_id NOT IN (SELECT id FROM projects))"),
            ("unknown or missing session state", "SELECT count(*) FROM sessions WHERE state IS NULL OR state NOT IN ('working','idle','waiting','ended','archived','elsewhere')"),
            ("waiting session the spike would show otherwise", "SELECT count(*) FROM sessions s WHERE state = 'waiting' AND coalesce((SELECT kind = 'waiting' AND text NOT LIKE '%waiting for your input%' FROM events e WHERE e.session_id = s.session_id AND kind IN ('prompt','stop','waiting') ORDER BY id DESC LIMIT 1), 0) = 0"),
            ("asleep but not ended", "SELECT count(*) FROM sessions WHERE asleep_at IS NOT NULL AND state != 'ended'"),
            ("open ask", "SELECT count(*) FROM asks WHERE status NOT IN ('answered','failed','dropped')"),
            ("unknown task fields", "SELECT count(*) FROM tasks WHERE status NOT IN ('idea','todo','active','waiting','blocked','done','dropped') OR (who IS NOT NULL AND who NOT IN ('you','agent','other')) OR (priority IS NOT NULL AND priority NOT IN ('p0','p1','p2'))"),
            ("unknown project fields", "SELECT count(*) FROM projects WHERE status NOT IN ('idea','active','paused','done') OR area != 'private'"),
            ("project names collide ignoring case", "SELECT count(*) - count(DISTINCT lower(name)) FROM projects"),
            ("bad activity row", "SELECT count(*) FROM activity WHERE text IS NULL OR who NOT IN ('brain','session','you')"),
            ("group path not normalised", "SELECT count(*) FROM groups WHERE path LIKE '%//%' OR path LIKE '/%' OR path LIKE '%/'"),
            ("suggestion action", "SELECT count(*) FROM suggestions WHERE action IS NOT NULL AND (json_valid(action) = 0 OR json_type(action) != 'array' OR substr(json_extract(action, '$[0]'), 1, instr(json_extract(action, '$[0]') || ' ', ' ') - 1) NOT IN ('task','set','archive','close','group','note','project'))"),
        ];
        violations.extend(checks.iter().map(|(why, sql)| (why.to_string(), count(&conn, sql))));
        for (why, n) in violations {
            prop_assert_eq!(n, 0, "{}", why);
        }

        let mut accounts = conn.prepare("SELECT DISTINCT account FROM sessions").unwrap();
        let accounts: Vec<Option<String>> = accounts.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        for account in accounts {
            let account = account.expect("a valid session has an account");
            if account == "codex" {
                continue;
            }
            // A path under home, or the bare name real brains use.
            let path = if account.starts_with("acct-") {
                dir.path().join(format!(".{account}"))
            } else {
                let rel = Path::new(&account).strip_prefix(dir.path()).expect("under home");
                prop_assert!(rel.to_string_lossy().starts_with(".acct-"));
                Path::new(&account).to_path_buf()
            };
            prop_assert!(path.is_dir());
        }
        let thread: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='console_thread'", [], |r| r.get(0))
            .ok();
        if let Some(thread) = thread {
            let v: serde_json::Value = serde_json::from_str(&thread).unwrap();
            let sid = v["sid"].as_str().unwrap();
            let found = count(&conn, &format!("SELECT count(*) FROM sessions WHERE session_id='{sid}'"));
            prop_assert_eq!(found, 1);
            prop_assert!(Path::new(v["account"].as_str().unwrap()).is_dir());
        }
    }
}
