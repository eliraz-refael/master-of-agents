//! The typed mirror of the topics a view draws from (V2). A client replaces a topic from a
//! snapshot and patches it after; rows it can't read are left out and counted (V18).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use moa_core::live::DisplayState;
use moa_core::model::{
    ActivityId, ActivityRow, Group, Project, ProjectId, Session, SessionId, Suggestion,
    SuggestionId, Task, TaskId,
};
use moa_core::publish::{BrainRow, PanelRow, TermRow, Topic};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The topics a view follows.
pub const TOPICS: &[Topic] = &[
    Topic::Sessions,
    Topic::Groups,
    Topic::Projects,
    Topic::Tasks,
    Topic::Suggestions,
    Topic::Activity,
    Topic::Brain,
    Topic::Panels,
    Topic::Terms,
];

/// A `sessions` row: the session and what the core adds to it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SessionRow {
    #[serde(flatten)]
    pub session: Session,
    /// The short id shown for it: the first 8 characters of its conversation (or moa) id.
    pub handle: String,
    /// Its project's name.
    pub project: Option<String>,
    pub display_state: DisplayState,
    /// One line of context beside the state.
    pub why: String,
    /// An agent moa adopted rather than started (an untracked binding). Absent from an older
    /// daemon's rows.
    #[serde(default)]
    pub adopted: bool,
}

/// What the daemon published, by topic and key.
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub sessions: BTreeMap<SessionId, SessionRow>,
    pub groups: BTreeMap<String, Group>,
    pub projects: BTreeMap<ProjectId, Project>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub suggestions: BTreeMap<SuggestionId, Suggestion>,
    pub activity: BTreeMap<ActivityId, ActivityRow>,
    pub brain: Option<BrainRow>,
    pub panels: BTreeMap<String, PanelRow>,
    /// The terminals moa opened, by tab id (`%N`).
    pub terms: BTreeMap<String, TermRow>,
    /// The keys of rows that didn't decode, by topic.
    unreadable: BTreeSet<(Topic, String)>,
    /// See [`Data::revision`].
    revision: u64,
}

/// The last revision handed out, by any `Data`: two different contents never share one.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// A row's `key` field; a row without one is kept under the empty key.
fn key_of(row: &Value) -> String {
    row.get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn int_key(key: &str) -> Option<i64> {
    key.parse().ok()
}

/// Puts the decoded row under `key`. A row that doesn't decode, or a key that isn't one,
/// leaves the key empty: an old version of the row would show stale data.
fn put<K: Ord, T: DeserializeOwned>(map: &mut BTreeMap<K, T>, key: Option<K>, row: Value) -> bool {
    let Some(key) = key else {
        return false;
    };
    match serde_json::from_value(row) {
        Ok(v) => {
            map.insert(key, v);
            true
        }
        Err(_) => {
            map.remove(&key);
            false
        }
    }
}

fn drop_key<K: Ord, T>(map: &mut BTreeMap<K, T>, key: Option<K>) {
    if let Some(key) = key {
        map.remove(&key);
    }
}

impl Data {
    /// A number that changes with every [`replace`](Data::replace),
    /// [`upsert`](Data::upsert) and [`delete`](Data::delete), and is never the same for two
    /// different contents: what a view keeps worked out from the data is good while it
    /// stays. A direct edit of a map calls [`touch`](Data::touch).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The data changed (an edit made directly to a map): a new revision.
    pub fn touch(&mut self) {
        self.revision = REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    /// Takes a topic's snapshot: its rows become exactly these.
    pub fn replace(&mut self, topic: Topic, rows: Vec<Value>) {
        if !TOPICS.contains(&topic) {
            return;
        }
        self.touch();
        match topic {
            Topic::Sessions => self.sessions.clear(),
            Topic::Groups => self.groups.clear(),
            Topic::Projects => self.projects.clear(),
            Topic::Tasks => self.tasks.clear(),
            Topic::Suggestions => self.suggestions.clear(),
            Topic::Activity => self.activity.clear(),
            Topic::Brain => self.brain = None,
            Topic::Panels => self.panels.clear(),
            Topic::Terms => self.terms.clear(),
            Topic::Asks | Topic::Status | Topic::Layouts => {}
        }
        self.unreadable.retain(|(t, _)| *t != topic);
        self.upsert(topic, rows);
    }

    /// Adds or replaces rows by their keys.
    pub fn upsert(&mut self, topic: Topic, rows: Vec<Value>) {
        if !TOPICS.contains(&topic) {
            return;
        }
        self.touch();
        for row in rows {
            let key = key_of(&row);
            if self.put(topic, &key, row) {
                self.unreadable.remove(&(topic, key));
            } else {
                self.unreadable.insert((topic, key));
            }
        }
    }

    fn put(&mut self, topic: Topic, key: &str, row: Value) -> bool {
        match topic {
            Topic::Sessions => put(&mut self.sessions, key.parse().ok(), row),
            Topic::Groups => put(&mut self.groups, Some(key.to_string()), row),
            Topic::Projects => put(&mut self.projects, int_key(key).map(ProjectId), row),
            Topic::Tasks => put(&mut self.tasks, int_key(key).map(TaskId), row),
            Topic::Suggestions => put(&mut self.suggestions, int_key(key).map(SuggestionId), row),
            Topic::Activity => put(&mut self.activity, int_key(key).map(ActivityId), row),
            Topic::Brain => {
                self.brain = serde_json::from_value(row).ok();
                self.brain.is_some()
            }
            Topic::Panels => put(&mut self.panels, Some(key.to_string()), row),
            Topic::Terms => put(&mut self.terms, Some(key.to_string()), row),
            Topic::Asks | Topic::Status | Topic::Layouts => true,
        }
    }

    /// Removes rows by key.
    pub fn delete(&mut self, topic: Topic, keys: &[String]) {
        if !TOPICS.contains(&topic) {
            return;
        }
        self.touch();
        for key in keys {
            match topic {
                Topic::Sessions => drop_key(&mut self.sessions, key.parse().ok()),
                Topic::Groups => drop_key(&mut self.groups, Some(key.clone())),
                Topic::Projects => drop_key(&mut self.projects, int_key(key).map(ProjectId)),
                Topic::Tasks => drop_key(&mut self.tasks, int_key(key).map(TaskId)),
                Topic::Suggestions => {
                    drop_key(&mut self.suggestions, int_key(key).map(SuggestionId))
                }
                Topic::Activity => drop_key(&mut self.activity, int_key(key).map(ActivityId)),
                // The topic's one row.
                Topic::Brain => self.brain = None,
                Topic::Panels => drop_key(&mut self.panels, Some(key.clone())),
                Topic::Terms => drop_key(&mut self.terms, Some(key.clone())),
                Topic::Asks | Topic::Status | Topic::Layouts => {}
            }
            self.unreadable.remove(&(topic, key.clone()));
        }
    }

    /// How many rows couldn't be read: the footer says so until it is 0.
    pub fn unreadable(&self) -> usize {
        self.unreadable.len()
    }
}
