//! The data mirror: snapshots, patches, unreadable rows and the fixture.

use moa_core::live::DisplayState;
use moa_core::model::SessionId;
use moa_core::publish::Topic;
use moa_view::data::{Data, TOPICS};
use moa_view::fixture::lantern;
use serde_json::{Value, json};

fn id(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

/// A session row as the daemon publishes it.
fn session(n: u128, title: &str) -> Value {
    let id = id(n).to_string();
    json!({
        "key": id,
        "id": id,
        "agent_kind": "claude",
        "account": "acct-a",
        "cwd": "/h/private",
        "start_cwd": "/h/private",
        "project_id": null,
        "title": title,
        "title_locked": false,
        "group_path": null,
        "lifecycle": "live",
        "activity": "idle",
        "activity_at": null,
        "activity_why": null,
        "last_prompt": null,
        "last_turn_end_at": null,
        "seen_at": null,
        "asleep_at": null,
        "archived_at": null,
        "location": "here",
        "resume_failed": false,
        "created_at": 1,
        "last_seen_at": null,
        "handle": "00000000",
        "conversation": null,
        "project": null,
        "display_state": "idle",
        "why": "",
        "adopted": false,
    })
}

fn titles(data: &Data) -> Vec<(SessionId, String)> {
    data.sessions
        .iter()
        .map(|(id, row)| (*id, row.session.title.clone().unwrap_or_default()))
        .collect()
}

#[test]
fn data_follows_a_snapshot_then_patches() {
    let mut data = Data::default();
    data.replace(Topic::Sessions, vec![session(1, "one"), session(2, "two")]);
    assert_eq!(
        titles(&data),
        vec![(id(1), "one".into()), (id(2), "two".into())]
    );

    data.upsert(
        Topic::Sessions,
        vec![session(2, "two, renamed"), session(3, "three")],
    );
    data.delete(Topic::Sessions, &[id(1).to_string()]);
    assert_eq!(
        titles(&data),
        vec![(id(2), "two, renamed".into()), (id(3), "three".into())]
    );
    assert_eq!(data.sessions[&id(2)].handle, "00000000");
    assert_eq!(data.sessions[&id(2)].display_state, DisplayState::Idle);
    assert!(!data.sessions[&id(2)].adopted);

    // A new snapshot drops what it doesn't hold.
    data.replace(Topic::Sessions, vec![session(3, "three")]);
    assert_eq!(titles(&data), vec![(id(3), "three".into())]);
}

#[test]
fn an_unreadable_row_is_skipped_and_counted() {
    let mut data = Data::default();
    let mut bad = session(1, "zombie");
    bad["lifecycle"] = json!("zombie");
    data.upsert(Topic::Sessions, vec![bad.clone(), session(2, "fine")]);
    assert!(!data.sessions.contains_key(&id(1)));
    assert!(data.sessions.contains_key(&id(2)));
    assert_eq!(data.unreadable(), 1);

    data.upsert(Topic::Sessions, vec![session(1, "alive again")]);
    assert_eq!(data.unreadable(), 0);
    assert_eq!(
        data.sessions[&id(1)].session.title.as_deref(),
        Some("alive again")
    );

    // A good row that turns bad is left out, not kept stale.
    data.upsert(Topic::Sessions, vec![bad]);
    assert!(!data.sessions.contains_key(&id(1)));
    assert_eq!(data.unreadable(), 1);
    // Deleting the key clears its count; so does a snapshot without it.
    data.delete(Topic::Sessions, &[id(1).to_string()]);
    assert_eq!(data.unreadable(), 0);
    data.upsert(Topic::Tasks, vec![json!({"key": "9", "id": 9})]);
    assert_eq!(data.unreadable(), 1);
    data.replace(Topic::Tasks, vec![]);
    assert_eq!(data.unreadable(), 0);
}

#[test]
fn the_other_topics_decode_and_a_missing_adopted_reads_false() {
    let mut data = Data::default();
    let mut row = session(1, "old daemon");
    row.as_object_mut().unwrap().remove("adopted");
    data.upsert(Topic::Sessions, vec![row]);
    assert!(!data.sessions[&id(1)].adopted);

    data.upsert(
        Topic::Brain,
        vec![
            json!({"key": "brain", "name": "default", "folders": [], "accounts": [],
                    "open_account": null, "folded": []}),
        ],
    );
    assert_eq!(
        data.brain.as_ref().map(|b| b.name.as_str()),
        Some("default")
    );
    data.delete(Topic::Brain, &["brain".into()]);
    assert!(data.brain.is_none());

    data.upsert(
        Topic::Panels,
        vec![json!({"key": "c7", "pane": "%3", "view": "tree"})],
    );
    assert_eq!(data.panels["c7"].pane.as_deref(), Some("%3"));

    // Topics the views don't follow are ignored, not counted.
    data.upsert(Topic::Status, vec![json!({"key": "counts", "idle": 1})]);
    assert_eq!(data.unreadable(), 0);
    assert!(TOPICS.contains(&Topic::Brain) && !TOPICS.contains(&Topic::Status));
}

#[test]
fn the_fixture_states_are_the_ones_the_core_derives() {
    let (data, ctx) = lantern();
    for row in data.sessions.values() {
        let (state, why) = moa_core::live::display_state(&row.session, ctx.now);
        assert_eq!(
            (state, why.as_str()),
            (row.display_state, row.why.as_str()),
            "{:?}",
            row.session.title
        );
    }
}

#[test]
fn the_fixture_decodes_whole() {
    let (data, ctx) = lantern();
    assert_eq!(data.unreadable(), 0);
    assert_eq!(data.sessions.len(), 11);
    let archived = data
        .sessions
        .values()
        .filter(|s| s.display_state == DisplayState::Archived)
        .count();
    assert_eq!(archived, 1);
    assert_eq!(data.projects.len(), 4);
    assert_eq!(data.tasks.len(), 8);
    assert_eq!(data.suggestions.len(), 2);
    assert_eq!(data.activity.len(), 8);
    assert_eq!(data.groups.len(), 1);
    assert!(data.brain.is_some());

    // Newest first, one minute then an hour apart each.
    let mut seen: Vec<_> = data.sessions.values().collect();
    seen.sort_by_key(|s| std::cmp::Reverse(s.session.last_seen_at));
    let titles: Vec<_> = seen
        .iter()
        .map(|s| s.session.title.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(titles[0], "Fix login");
    assert_eq!(titles[10], "Gone");
    assert_eq!(ctx.now.0 - seen[0].session.last_seen_at.unwrap().0, 60_000);
    assert_eq!(
        seen[0].session.last_seen_at.unwrap().0 - seen[1].session.last_seen_at.unwrap().0,
        3_600_000
    );
    let states: Vec<_> = seen.iter().map(|s| s.display_state).collect();
    use DisplayState::*;
    assert_eq!(
        states,
        vec![
            Waiting, Working, Question, Done, Idle, Failed, Quiet, Idle, Asleep, Ended, Archived
        ]
    );
    assert!(seen[7].adopted);
    assert_eq!(seen[5].session.account.as_deref(), Some("acct-b"));

    // Fri 13 Nov 2026, 10:00 New York time.
    assert_eq!(ctx.today, jiff::civil::date(2026, 11, 13));
    assert_eq!(ctx.tick, 3);
    assert!((ctx.is_dir)(std::path::Path::new("/anywhere")));
    let newest = data.activity.values().map(|a| a.at).max().unwrap();
    assert_eq!(ctx.now.0 - newest.0, 2_000);
}
