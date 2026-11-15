//! The activity texts the core writes equal the spike triggers' texts, so old and new rows
//! read the same.

use moa_core::activity;
use moa_store::spike::{SCHEMA_SQL, TRIGGERS_SQL};
use rusqlite::Connection;

fn last(c: &Connection) -> String {
    c.query_row(
        "SELECT text FROM activity ORDER BY id DESC LIMIT 1",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn run(c: &Connection, sql: &str) {
    c.execute(sql, []).unwrap();
}

#[test]
fn activity_texts_match_the_spike_triggers() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(SCHEMA_SQL).unwrap();
    c.execute_batch(TRIGGERS_SQL).unwrap();

    run(
        &c,
        "INSERT INTO projects(name,status) VALUES('Lantern','idea')",
    );
    assert_eq!(last(&c), activity::new_project("Lantern", "idea"));
    run(
        &c,
        "UPDATE projects SET status='active' WHERE name='Lantern'",
    );
    assert_eq!(
        last(&c),
        activity::project_status("Lantern", "idea", "active")
    );

    run(
        &c,
        "INSERT INTO tasks(title,project_id) VALUES('Fix the wick',1)",
    );
    assert_eq!(
        last(&c),
        activity::new_task("Fix the wick", Some("Lantern"))
    );
    run(&c, "INSERT INTO tasks(title) VALUES('Buy oil')");
    assert_eq!(last(&c), activity::new_task("Buy oil", None));
    run(&c, "UPDATE tasks SET status='done' WHERE title='Buy oil'");
    assert_eq!(last(&c), activity::task_status("Buy oil", "todo", "done"));
    run(
        &c,
        "UPDATE tasks SET today='2026-11-15' WHERE title='Buy oil'",
    );
    assert_eq!(last(&c), activity::added_to_today("Buy oil"));

    let long = "é".repeat(150);
    c.execute("INSERT INTO notes(kind,text) VALUES('fact', ?1)", [&long])
        .unwrap();
    assert_eq!(last(&c), activity::noted("fact", &long));
    assert_eq!(last(&c).chars().count(), "noted (fact): ".len() + 100);

    run(&c, "INSERT INTO suggestions(text) VALUES('Try the lamp')");
    assert_eq!(last(&c), activity::suggests(1, "Try the lamp"));

    run(
        &c,
        "INSERT INTO sessions(session_id) VALUES('0123456789abcdef')",
    );
    run(
        &c,
        "UPDATE sessions SET project_id=1 WHERE session_id='0123456789abcdef'",
    );
    assert_eq!(last(&c), activity::linked("01234567", "Lantern"));
}
