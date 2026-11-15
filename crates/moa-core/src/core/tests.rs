use super::*;
use crate::change::Change;
use crate::config::{BrainName, Config};
use crate::effect::RefuseWrites;
use crate::model::Group;
use crate::state::{LoadNote, Snapshot};
use jiff::tz::TimeZone;
use std::time::Duration;

fn now() -> Now {
    Now::at(
        UtcMs(1_000_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn state_with_unknown(unknown: Vec<LoadNote>) -> State {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    s.load(Snapshot {
        unknown,
        ..Snapshot::default()
    });
    s
}

fn group(path: &str) -> Change {
    Change::Group(Group {
        path: path.into(),
        collapsed: false,
        created_at: UtcMs(1),
    })
}

#[test]
fn a_row_with_an_unknown_value_is_not_edited() {
    let mut s = state_with_unknown(vec![LoadNote {
        table: "groups",
        key: "Private".into(),
        reason: "x",
    }]);
    let mut tx = Tx::new(&now());
    let err = tx.commit(&mut s, vec![group("Other"), group("Private")]);
    assert_eq!(
        err.unwrap_err().0,
        "group Private has a value this build doesn't know; update moa"
    );
    // Everything is checked before anything is applied: "Other" is not there either.
    assert_eq!(s.groups().count(), 0);
    assert!(tx.changes.is_empty());
    assert!(tx.commit(&mut s, vec![group("Other")]).is_ok());
    assert_eq!(s.groups().count(), 1);
}

#[test]
fn unknown_rows_are_named_the_way_people_say_them() {
    let s = state_with_unknown(vec![LoadNote {
        table: "tasks",
        key: "#3".into(),
        reason: "x",
    }]);
    let mut tx = Tx::new(&now());
    let task = Change::Task(crate::model::Task {
        id: crate::model::TaskId(3),
        project_id: None,
        title: "t".into(),
        status: crate::model::TaskStatus::Todo,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: None,
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    });
    let mut s = s;
    assert_eq!(
        tx.commit(&mut s, vec![task]).unwrap_err().0,
        "task #3 has a value this build doesn't know; update moa"
    );
}

#[test]
fn ids_advance_within_one_transaction() {
    let s = state_with_unknown(vec![]);
    let mut tx = Tx::new(&now());
    assert_eq!(tx.next_id(&s, Table::Event), 1);
    assert_eq!(tx.next_id(&s, Table::Event), 2);
    assert_eq!(tx.next_id(&s, Table::Activity), 1);
}

#[test]
fn a_refused_commit_applies_nothing() {
    let mut s = state_with_unknown(vec![]);
    s.refuse_writes = Some(RefuseWrites {
        until: UtcMs(2_000_000),
        reason: "disk full".into(),
    });
    let mut tx = Tx::new(&now());
    assert!(tx.commit(&mut s, vec![group("A")]).is_err());
    assert_eq!(s.groups().count(), 0);
}
