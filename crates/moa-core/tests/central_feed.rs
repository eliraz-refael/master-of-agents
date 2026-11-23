//! moa's feed in the core: what loads, what the `feed` topic publishes.

mod common;

use common::loaded_with;
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::model::FeedEntry;
use moa_core::publish::{FEED_SHOWN, Topic};
use moa_core::state::{Deps, Snapshot};
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

fn entry(id: i64, message_id: Option<&str>) -> FeedEntry {
    FeedEntry {
        id,
        thread_id: "th-1".into(),
        at: UtcMs(1_000 + id),
        kind: "moa".into(),
        message_id: message_id.map(Into::into),
        automatic: false,
        body: json!({"text": format!("line {id}")}).to_string(),
        state: None,
    }
}

fn start(feed_tail: Vec<FeedEntry>, feed_keys: Vec<(String, String)>) -> moa_core::state::State {
    let b = TypicalBrain::new();
    let snapshot = Snapshot {
        feed_tail,
        feed_keys,
        ..b.snapshot()
    };
    loaded_with(&b, "", Deps::default(), snapshot)
}

#[test]
fn feed_rows_publish_under_feed() {
    let st = start(vec![entry(7, Some("m-7"))], vec![]);
    let rows = st.model().topics[&Topic::Feed].clone();
    let row = &rows["#7"];
    assert_eq!(row["key"], "#7");
    assert_eq!(row["thread"], "th-1");
    assert_eq!(row["message_id"], "m-7");
    assert_eq!(row["body"], json!({"text": "line 7"}));
    assert_eq!(row["state"], json!(null));
}

#[test]
fn the_tail_keeps_the_newest_100() {
    let all: Vec<_> = (1..=150).map(|i| entry(i, None)).collect();
    let st = start(all, vec![]);
    let rows = st.model().topics[&Topic::Feed].clone();
    // 100 rows, the `state` row and the `bar` row.
    assert_eq!(rows.len(), FEED_SHOWN + 2);
    assert!(rows.contains_key("#150") && rows.contains_key("#51"));
    assert!(!rows.contains_key("#50"));
}

#[test]
fn the_state_row_says_off_at_start() {
    let st = start(vec![], vec![]);
    let rows = st.model().topics[&Topic::Feed].clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows["bar"]["by"], "default");
    assert_eq!(rows["state"]["phase"], "off");
    assert_eq!(rows["state"]["thread"], json!(null));
    assert_eq!(rows["state"]["why"], json!(null));
}

#[test]
fn a_loaded_keys_are_kept_by_thread() {
    let st = start(vec![], vec![("th-1".into(), "m-1".into())]);
    assert!(st.feed_keys("th-1").unwrap().contains("m-1"));
    assert!(st.feed_keys("th-2").is_none());
}

#[test]
fn a_thread_change_publishes_feed() {
    let thread = moa_core::model::MoaThread {
        thread_id: "th-1".into(),
        account: "acct-a".into(),
        day: jiff::civil::date(2026, 11, 22),
        kind: moa_core::model::ThreadKind::Daily,
        started_at: UtcMs(1),
    };
    assert_eq!(Change::Thread(thread).topics(), [Topic::Feed]);
    assert_eq!(Change::Feed(entry(1, None)).topics(), [Topic::Feed]);
}
