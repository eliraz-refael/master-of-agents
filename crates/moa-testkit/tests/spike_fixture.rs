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
}
