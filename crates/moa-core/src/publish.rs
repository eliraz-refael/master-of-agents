//! What the core publishes: topics of keyed JSON rows, rebuilt only when their data changed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{Form, UiConfig};
use crate::layout::{current_layout, leaves};
use crate::live::{DisplayState, display_state};
use crate::model::{FeedEntry, SessionId, Terminal};
use crate::ops::launch::title_of;
use crate::panel::{View, key_of};
use crate::state::{EventWhat, SessionEvent, State};
use crate::terminals::{is_active, open_by_pane};

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
    Terms,
    Feed,
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
        Topic::Terms,
        Topic::Feed,
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
/// paths, the listed accounts with a launcher, the account sessions open with, the folded
/// tree paths, and the form `Ctrl-a /` opens the picker in (`[ui] picker`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BrainRow {
    pub name: String,
    pub folders: Vec<String>,
    pub accounts: Vec<String>,
    pub open_account: Option<String>,
    pub folded: Vec<String>,
    /// Absent from an older daemon's rows: the popup, the default.
    #[serde(default)]
    pub picker: Form,
    /// `[ui] terminal_idle_hours` in milliseconds, `0` for never (5d). Absent from an older
    /// daemon's rows: the default, 2 hours.
    #[serde(default = "default_idle_ms")]
    pub terminal_idle_ms: i64,
}

pub fn default_idle_ms() -> i64 {
    UiConfig::default().terminal_idle_ms
}

/// A `panels` row (key `c<conn>`): one connected panel, its tab and its view, and whether
/// that view is moa's (the tab carries `@moa_view`, L8). The panel follows its own row.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PanelRow {
    pub pane: Option<String>,
    pub view: View,
    /// Absent from an older daemon's rows.
    #[serde(default)]
    pub managed: bool,
    /// A sheet's open (S9): its number while the sheet in this row's tab is open, which a new
    /// open changes; `None` (absent) while it is closed, and for every other panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<i64>,
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

/// A `terms` row (key: its tab id, `%N`): a terminal (`@moa_term`), from the topology, so the
/// picker never loses one whose record wasn't saved. One running a session's agent is that
/// session, and has no row. The rest is its open record's (T9): with none, only `name` (its
/// label without the `⌨ `) is set. Every field but `name` is absent from an older daemon's
/// rows.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TermRow {
    /// The record's name, else the label's.
    pub name: String,
    /// The record's number (`term ls`'s `#<id>`); 0 with none.
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub folder: String,
    /// The program in front, as of the last listing.
    #[serde(default)]
    pub command: String,
    /// A program other than the shell is in front.
    #[serde(default)]
    pub running: bool,
    /// Since when moa has seen `command` in front.
    #[serde(default)]
    pub since: i64,
    #[serde(default)]
    pub opened_at: i64,
    #[serde(default)]
    pub touched_at: Option<i64>,
    /// A user is looking at it now.
    #[serde(default)]
    pub active: bool,
    /// `you`, `moa` or `found`.
    #[serde(default)]
    pub opened_by: String,
    /// The title of the session it belongs to.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub timeout_at: Option<i64>,
}

/// The `layouts` row with key `""` (no layout can be named that), there while
/// `layouts.json` doesn't parse.
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

/// The newest feed rows published (and kept in the state). Paging older ones is 6b's.
pub const FEED_SHOWN: usize = 100;

/// A `feed` row (key `#<id>`): one line of moa's conversation. `body` is the stored JSON
/// (`null` when the stored text doesn't parse).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FeedRow {
    pub id: i64,
    pub thread: String,
    pub at: i64,
    pub kind: String,
    pub message_id: Option<String>,
    pub automatic: bool,
    pub body: Value,
    pub state: Option<String>,
}

impl From<&FeedEntry> for FeedRow {
    fn from(e: &FeedEntry) -> FeedRow {
        FeedRow {
            id: e.id,
            thread: e.thread_id.clone(),
            at: e.at.0,
            kind: e.kind.clone(),
            message_id: e.message_id.clone(),
            automatic: e.automatic,
            body: serde_json::from_str(&e.body).unwrap_or(Value::Null),
            state: e.state.clone(),
        }
    }
}

/// The `feed` topic's row with key `state`: whether moa's agent is running, which thread it is
/// on, and why it failed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MoaRow {
    /// `off`, `starting`, `ready`, `thinking` or `failed`.
    pub phase: String,
    pub thread: Option<String>,
    pub why: Option<String>,
}

/// A `feed` row (key `ev:<seq>`): a session event (B3), from memory. The console folds them,
/// one line per session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EventRow {
    pub seq: u64,
    pub at: i64,
    pub session: SessionId,
    pub what: EventWhat,
}

impl From<&SessionEvent> for EventRow {
    fn from(e: &SessionEvent) -> EventRow {
        EventRow {
            seq: e.seq,
            at: e.at.0,
            session: e.session,
            what: e.what,
        }
    }
}

/// The key of session event `seq`'s `feed` row.
fn event_key(seq: u64) -> String {
    format!("ev:{seq}")
}

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
            picker: config.ui.picker,
            terminal_idle_ms: config.ui.terminal_idle_ms,
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
                    managed: reg.managed,
                    sheet: reg.pane.and_then(|p| self.sheets.get(&p).copied()),
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
        // A hand-written entry named `""` is listed (as broken), but `""` is the file's row.
        let mut by_name = by_key(
            rows.iter()
                .filter(|r| !r.name.is_empty())
                .map(|r| (r.name.clone(), r)),
        );
        if let Some(error) = &self.layouts.file_error {
            let row = LayoutFileRow {
                error: error.clone(),
            };
            by_name.insert(String::new(), keyed("", &row));
        }
        by_name
    }

    fn terms_rows(&self) -> BTreeMap<String, Value> {
        let records = open_by_pane(self);
        self.tmux
            .topology
            .terms()
            .map(|(pane, name)| {
                let key = pane.to_string();
                let row = match records.get(key.as_str()) {
                    Some(t) => self.term_row(t),
                    None => TermRow {
                        name: name.to_string(),
                        ..TermRow::default()
                    },
                };
                let v = keyed(&key, &row);
                (key, v)
            })
            .collect()
    }

    fn feed_rows(&self) -> BTreeMap<String, Value> {
        let mut rows: BTreeMap<String, Value> = self
            .feed_tail()
            .map(|e| {
                let key = format!("#{}", e.id);
                let v = keyed(&key, &FeedRow::from(e));
                (key, v)
            })
            .collect();
        rows.extend(self.session_events.iter().map(|e| {
            let key = event_key(e.seq);
            let v = keyed(&key, &EventRow::from(e));
            (key, v)
        }));
        rows.insert("state".into(), keyed("state", &self.central.row()));
        rows
    }

    /// Open record `t`'s `terms` row.
    pub(crate) fn term_row(&self, t: &Terminal) -> TermRow {
        TermRow {
            name: t.name.clone(),
            id: t.id.0,
            folder: t.cwd.clone(),
            command: t.command.clone(),
            running: t.command != t.shell,
            since: t.command_since.0,
            opened_at: t.opened_at.0,
            touched_at: t.touched_at.map(|at| at.0),
            active: is_active(self, t),
            opened_by: t.opened_by.as_str().to_string(),
            session: t
                .session_id
                .and_then(|id| self.session(id))
                .map(|s| title_of(self, s)),
            note: t.note.clone(),
            timeout_at: t.timeout_at.map(|at| at.0),
        }
    }

    fn build(&self, topic: Topic) -> BTreeMap<String, Value> {
        match topic {
            Topic::Brain => self.brain_rows(),
            Topic::Panels => self.panels_rows(),
            Topic::Layouts => self.layouts_rows(),
            Topic::Terms => self.terms_rows(),
            Topic::Feed => self.feed_rows(),
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

    /// Publishes the `feed` topic when moa's `state` row differs from the one last published.
    /// Its other rows change only with a commit, which publishes them, or a load, which
    /// publishes everything: only the `state` row is compared, and the topic is built only
    /// when it moved.
    pub(crate) fn publish_moa_state(&mut self) -> Option<Arc<Model>> {
        let row = keyed("state", &self.central.row());
        let last = self
            .model
            .topics
            .get(&Topic::Feed)
            .and_then(|t| t.get("state"));
        if last == Some(&row) {
            return None;
        }
        let rows = self.feed_rows();
        Some(self.put(vec![(Topic::Feed, rows)]))
    }

    /// Publishes the `feed` topic when a session event was kept since it was last published.
    /// Events are pushed only by a commit, in order, and every build of the topic carries all
    /// of them: the newest one's row is missing exactly when the topic is behind.
    pub(crate) fn publish_session_events(&mut self) -> Option<Arc<Model>> {
        let newest = event_key(self.session_events.back()?.seq);
        if self
            .model
            .topics
            .get(&Topic::Feed)
            .is_some_and(|t| t.contains_key(&newest))
        {
            return None;
        }
        let rows = self.feed_rows();
        Some(self.put(vec![(Topic::Feed, rows)]))
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
