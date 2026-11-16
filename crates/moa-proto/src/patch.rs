//! Snapshots and patches of the published model, as sent to a subscriber.

use std::collections::BTreeMap;
use std::sync::Arc;

use moa_core::publish::{Model, Topic};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The full rows of the subscribed topics at one rev.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotMsg {
    pub rev: u64,
    pub topics: BTreeMap<Topic, Vec<Value>>,
}

/// What changed since the subscriber's last rev: rows to insert or replace (each carries its
/// `"key"`) and keys to remove, by topic. A topic that did not change is absent.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Patch {
    pub rev: u64,
    pub upsert: BTreeMap<Topic, Vec<Value>>,
    pub delete: BTreeMap<Topic, Vec<String>>,
}

/// The rows of `topics` in `model`, in key order. A topic the model lacks is empty.
pub fn snapshot_of(model: &Model, topics: &[Topic]) -> SnapshotMsg {
    let topics = topics
        .iter()
        .map(|t| {
            let rows = model
                .topics
                .get(t)
                .map(|rows| rows.values().cloned().collect())
                .unwrap_or_default();
            (*t, rows)
        })
        .collect();
    SnapshotMsg {
        rev: model.rev,
        topics,
    }
}

impl Patch {
    /// The difference from `old` to `new` over `topics`; `None` when nothing differs.
    pub fn between(old: &Model, new: &Model, topics: &[Topic]) -> Option<Patch> {
        let empty = Arc::new(BTreeMap::new());
        let mut upsert = BTreeMap::new();
        let mut delete = BTreeMap::new();
        for t in topics {
            let before = old.topics.get(t).unwrap_or(&empty);
            let after = new.topics.get(t).unwrap_or(&empty);
            // Unchanged topics keep the same Arc across revs.
            if Arc::ptr_eq(before, after) {
                continue;
            }
            let changed: Vec<Value> = after
                .iter()
                .filter(|(k, v)| before.get(*k) != Some(*v))
                .map(|(_, v)| v.clone())
                .collect();
            let gone: Vec<String> = before
                .keys()
                .filter(|k| !after.contains_key(*k))
                .cloned()
                .collect();
            if !changed.is_empty() {
                upsert.insert(*t, changed);
            }
            if !gone.is_empty() {
                delete.insert(*t, gone);
            }
        }
        if upsert.is_empty() && delete.is_empty() {
            return None;
        }
        Some(Patch {
            rev: new.rev,
            upsert,
            delete,
        })
    }
}
