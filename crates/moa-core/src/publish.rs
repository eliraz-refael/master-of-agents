//! What the core publishes: topics of keyed JSON rows, rebuilt only when their data changed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::live::{DisplayState, display_state};
use crate::state::State;

/// A named set of rows a view can subscribe to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Topic {
    Sessions,
    Groups,
    Projects,
    Tasks,
    Suggestions,
    Asks,
    Activity,
    Status,
}

impl Topic {
    pub const ALL: &'static [Topic] = &[
        Topic::Sessions,
        Topic::Groups,
        Topic::Projects,
        Topic::Tasks,
        Topic::Suggestions,
        Topic::Asks,
        Topic::Activity,
        Topic::Status,
    ];
}

/// One published rows-by-key map per topic. Unchanged topics keep the same `Arc` across revs.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Model {
    pub rev: u64,
    pub topics: BTreeMap<Topic, Arc<BTreeMap<String, Value>>>,
}

/// Something the core tells its views that is not a row.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CoreEvent {
    Error { message: String },
    Ui { text: String },
}

/// The newest activity rows published.
pub const ACTIVITY_SHOWN: usize = 300;

/// A row as JSON with its `key` field added.
fn keyed<T: Serialize>(key: &str, row: &T) -> Value {
    let mut v = serde_json::to_value(row).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("key".into(), Value::String(key.to_string()));
    }
    v
}

fn by_key<'a, T: Serialize + 'a>(
    rows: impl Iterator<Item = (String, &'a T)>,
) -> BTreeMap<String, Value> {
    rows.map(|(k, r)| {
        let v = keyed(&k, r);
        (k, v)
    })
    .collect()
}

/// The state word for a display state, as it appears in JSON.
pub fn word(state: DisplayState) -> String {
    match serde_json::to_value(state) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

impl State {
    fn sessions_rows(&self) -> BTreeMap<String, Value> {
        self.sessions()
            .map(|s| {
                let (state, why) = display_state(s, self.now);
                let mut v = keyed(&s.id.to_string(), s);
                if let Value::Object(m) = &mut v {
                    m.insert("handle".into(), self.handle_of(s.id).into());
                    m.insert(
                        "conversation".into(),
                        self.current_conversation(s.id)
                            .map(|c| c.conversation_id.clone())
                            .into(),
                    );
                    m.insert(
                        "project".into(),
                        s.project_id
                            .and_then(|p| self.projects.get(&p))
                            .map(|p| p.name.clone())
                            .into(),
                    );
                    m.insert("display_state".into(), word(state).into());
                    m.insert("why".into(), why.into());
                }
                (s.id.to_string(), v)
            })
            .collect()
    }

    fn status_rows(&self) -> BTreeMap<String, Value> {
        let mut counts: BTreeMap<DisplayState, u64> = DisplayState::ALL
            .iter()
            .filter(|d| !matches!(d, DisplayState::Ended | DisplayState::Archived))
            .map(|d| (*d, 0))
            .collect();
        for s in self.sessions() {
            let (state, _) = display_state(s, self.now);
            if let Some(n) = counts.get_mut(&state) {
                *n += 1;
            }
        }
        let mut row = Map::new();
        for (state, n) in counts {
            row.insert(word(state), n.into());
        }
        row.insert("key".into(), "counts".into());
        BTreeMap::from([("counts".to_string(), Value::Object(row))])
    }

    fn build(&self, topic: Topic) -> BTreeMap<String, Value> {
        match topic {
            Topic::Sessions => self.sessions_rows(),
            Topic::Status => self.status_rows(),
            Topic::Groups => by_key(self.groups().map(|g| (g.path.clone(), g))),
            Topic::Projects => by_key(self.projects().map(|p| (p.id.to_string(), p))),
            Topic::Tasks => by_key(self.tasks().map(|t| (t.id.to_string(), t))),
            Topic::Suggestions => by_key(self.suggestions().map(|s| (s.id.to_string(), s))),
            Topic::Asks => by_key(self.asks.values().map(|a| (a.id.to_string(), a))),
            // The state keeps only the newest ACTIVITY_SHOWN rows.
            Topic::Activity => by_key(self.activity.values().map(|a| (a.id.to_string(), a))),
        }
    }

    /// Rebuilds `topics` from the rows and bumps `rev`. `None` (and no new rev) when the set is empty.
    pub(crate) fn publish(&mut self, topics: &BTreeSet<Topic>) -> Option<Arc<Model>> {
        if topics.is_empty() {
            return None;
        }
        let mut next = (*self.model).clone();
        for t in topics {
            next.topics.insert(*t, Arc::new(self.build(*t)));
        }
        next.rev += 1;
        self.model = Arc::new(next);
        Some(self.model.clone())
    }
}
