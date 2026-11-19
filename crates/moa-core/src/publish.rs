//! What the core publishes: topics of keyed JSON rows, rebuilt only when their data changed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::layout::{current_layout, leaves};
use crate::live::{DisplayState, display_state};
use crate::panel::{View, key_of};
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
    Brain,
    Panels,
    Layouts,
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
        Topic::Brain,
        Topic::Panels,
        Topic::Layouts,
    ];
}

/// The meta key holding the folded tree paths.
pub const FOLDED_KEY: &str = "tree.folded";

/// The folded tree paths, sorted. A value that doesn't parse reads as empty; the next fold
/// rewrites it.
pub fn folded(state: &State) -> Vec<String> {
    state
        .meta(FOLDED_KEY)
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default()
}

/// The `brain` topic's one row (key `brain`): the brain's name, its folders as absolute
/// paths, the listed accounts with a launcher, the account sessions open with, and the
/// folded tree paths.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BrainRow {
    pub name: String,
    pub folders: Vec<String>,
    pub accounts: Vec<String>,
    pub open_account: Option<String>,
    pub folded: Vec<String>,
}

/// A `panels` row (key `c<conn>`): one connected panel, its tab and its view.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PanelRow {
    pub pane: Option<String>,
    pub view: View,
}

/// A `layouts` row (key: its name): its place in the layouts' order (the rows are by name),
/// the layout's leaves in order, whether it is the brain's current one, and why it can't be
/// used, when it can't (no leaves then).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LayoutRow {
    pub name: String,
    pub index: usize,
    pub leaves: Vec<String>,
    pub current: bool,
    pub error: Option<String>,
}

/// The `layouts` row with key `file`, there while `layouts.json` doesn't parse.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LayoutFileRow {
    pub error: String,
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
                    m.insert(
                        "adopted".into(),
                        self.binding(s.id).is_some_and(|b| !b.tracked).into(),
                    );
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

    fn brain_rows(&self) -> BTreeMap<String, Value> {
        let config = self.config();
        let brain = config.brain(self.brain()).unwrap_or_default();
        let row = BrainRow {
            name: self.brain().as_str().to_string(),
            folders: brain
                .folders
                .iter()
                .filter(|f| !f.as_os_str().is_empty())
                .map(|f| f.display().to_string())
                .collect(),
            accounts: config
                .accounts
                .iter()
                .filter(|(_, a)| a.launcher.is_some())
                .map(|(name, _)| name.as_str().to_string())
                .collect(),
            open_account: brain.open_account.map(|a| a.as_str().to_string()),
            folded: folded(self),
        };
        BTreeMap::from([("brain".to_string(), keyed("brain", &row))])
    }

    fn panels_rows(&self) -> BTreeMap<String, Value> {
        self.panels
            .iter()
            .map(|(conn, reg)| {
                let key = key_of(*conn);
                let row = PanelRow {
                    pane: reg.pane.map(|p| p.to_string()),
                    view: reg.view,
                };
                let v = keyed(&key, &row);
                (key, v)
            })
            .collect()
    }

    /// Every layout, in order.
    pub(crate) fn layout_rows(&self) -> Vec<LayoutRow> {
        let current = current_layout(self);
        self.layouts
            .names()
            .enumerate()
            .map(|(index, name)| {
                let (leaves, error) = match self.layouts.node(name) {
                    Ok(node) => (leaves(&node).iter().map(|l| l.to_string()).collect(), None),
                    Err(e) => (Vec::new(), Some(e.to_string())),
                };
                LayoutRow {
                    name: name.to_string(),
                    index,
                    leaves,
                    current: name == current,
                    error,
                }
            })
            .collect()
    }

    fn layouts_rows(&self) -> BTreeMap<String, Value> {
        let rows = self.layout_rows();
        let mut by_name = by_key(rows.iter().map(|r| (r.name.clone(), r)));
        if let Some(error) = &self.layouts.file_error {
            let row = LayoutFileRow {
                error: error.clone(),
            };
            by_name.insert("file".into(), keyed("file", &row));
        }
        by_name
    }

    fn build(&self, topic: Topic) -> BTreeMap<String, Value> {
        match topic {
            Topic::Brain => self.brain_rows(),
            Topic::Panels => self.panels_rows(),
            Topic::Layouts => self.layouts_rows(),
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
        let built: Vec<_> = topics.iter().map(|t| (*t, self.build(*t))).collect();
        Some(self.put(built))
    }

    /// Publishes `topic` only when its rows differ from the ones last published.
    pub(crate) fn publish_changed(&mut self, topic: Topic) -> Option<Arc<Model>> {
        let rows = self.build(topic);
        if self.model.topics.get(&topic).is_some_and(|t| **t == rows) {
            return None;
        }
        Some(self.put(vec![(topic, rows)]))
    }

    /// Replaces these topics' rows and bumps `rev`.
    fn put(&mut self, built: Vec<(Topic, BTreeMap<String, Value>)>) -> Arc<Model> {
        let mut next = (*self.model).clone();
        for (t, rows) in built {
            next.topics.insert(t, Arc::new(rows));
        }
        next.rev += 1;
        self.model = Arc::new(next);
        self.model.clone()
    }
}
