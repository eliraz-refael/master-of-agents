//! The core's in-memory copy of the brain: every row the views need, kept in step with the
//! store by [`Change`]s and published as a [`Model`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::UtcMs;
use crate::change::Change;
use crate::config::{BrainName, Config};
use crate::model::*;
use crate::publish::{ACTIVITY_SHOWN, Model};

/// The next id for each integer-id table. SQLite rowids start at 1, so does this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NextIds {
    pub project: i64,
    pub task: i64,
    pub note: i64,
    pub suggestion: i64,
    pub ask: i64,
    pub event: i64,
    pub activity: i64,
    pub feed: i64,
}

impl Default for NextIds {
    fn default() -> Self {
        NextIds {
            project: 1,
            task: 1,
            note: 1,
            suggestion: 1,
            ask: 1,
            event: 1,
            activity: 1,
            feed: 1,
        }
    }
}

/// An integer-id table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    Project,
    Task,
    Note,
    Suggestion,
    Ask,
    Event,
    Activity,
    Feed,
}

/// A row loaded with a default for text this build doesn't know. `key` is what
/// [`Change::row`] gives for that row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoadNote {
    pub table: &'static str,
    pub key: String,
    pub reason: &'static str,
}

/// Everything the store hands the core at startup.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub notes: Vec<Note>,
    pub suggestions: Vec<Suggestion>,
    pub sessions: Vec<Session>,
    pub conversations: Vec<Conversation>,
    pub groups: Vec<Group>,
    pub asks: Vec<Ask>,
    pub pane_bindings: Vec<PaneBinding>,
    pub spike_panes: Vec<SpikePane>,
    pub threads: Vec<MoaThread>,
    pub nags: Vec<Nag>,
    pub activity_tail: Vec<ActivityRow>,
    pub meta: Vec<(String, String)>,
    pub next_ids: NextIds,
    pub unknown: Vec<LoadNote>,
}

/// What a caller says about itself (the hook environment).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CallerInfo {
    pub moa_session: Option<String>,
    pub claude_session: Option<String>,
    pub cwd: Option<PathBuf>,
}

const OUTSIDE_MOA: &str = "an agent outside moa";

/// Who is asking.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Caller {
    Moa,
    /// The session and its display handle.
    Session(SessionId, String),
    Unknown {
        folder: String,
    },
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Caller::Moa => f.write_str("moa"),
            Caller::Session(_, handle) => f.write_str(handle),
            Caller::Unknown { folder } if folder == OUTSIDE_MOA => f.write_str(OUTSIDE_MOA),
            Caller::Unknown { folder } => write!(f, "unknown in {folder}"),
        }
    }
}

fn first8(s: &str) -> String {
    s.chars().take(8).collect()
}

type ConvKey = (&'static str, String);

fn nag_key(n: &Nag) -> (TaskId, Date, Date) {
    (n.task_id, n.due, n.day)
}

fn conv_key(c: &Conversation) -> ConvKey {
    (c.agent_kind.as_str(), c.conversation_id.clone())
}

pub struct State {
    brain: BrainName,
    config: Config,
    pub(crate) now: UtcMs,
    pub(crate) model: Arc<Model>,
    pub(crate) projects: BTreeMap<ProjectId, Project>,
    pub(crate) tasks: BTreeMap<TaskId, Task>,
    notes: BTreeMap<NoteId, Note>,
    pub(crate) suggestions: BTreeMap<SuggestionId, Suggestion>,
    pub(crate) sessions: BTreeMap<SessionId, Session>,
    conversations: BTreeMap<ConvKey, Conversation>,
    pub(crate) groups: BTreeMap<String, Group>,
    pub(crate) asks: BTreeMap<AskId, Ask>,
    pane_bindings: BTreeMap<SessionId, PaneBinding>,
    spike_panes: BTreeMap<SessionId, SpikePane>,
    threads: BTreeMap<String, MoaThread>,
    nags: BTreeMap<(TaskId, Date, Date), Nag>,
    pub(crate) activity: BTreeMap<ActivityId, ActivityRow>,
    meta: BTreeMap<String, String>,
    next_ids: NextIds,
    unknown: Vec<LoadNote>,
}

impl State {
    /// Empty until a snapshot is loaded.
    pub fn new(brain: BrainName, config: Config) -> State {
        State {
            brain,
            config,
            now: UtcMs(0),
            model: Arc::new(Model::default()),
            projects: BTreeMap::new(),
            tasks: BTreeMap::new(),
            notes: BTreeMap::new(),
            suggestions: BTreeMap::new(),
            sessions: BTreeMap::new(),
            conversations: BTreeMap::new(),
            groups: BTreeMap::new(),
            asks: BTreeMap::new(),
            pane_bindings: BTreeMap::new(),
            spike_panes: BTreeMap::new(),
            threads: BTreeMap::new(),
            nags: BTreeMap::new(),
            activity: BTreeMap::new(),
            meta: BTreeMap::new(),
            next_ids: NextIds::default(),
            unknown: Vec::new(),
        }
    }

    pub fn brain(&self) -> &BrainName {
        &self.brain
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn model(&self) -> Arc<Model> {
        self.model.clone()
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    pub fn projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.values()
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.values()
    }

    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.notes.values()
    }

    pub fn suggestions(&self) -> impl Iterator<Item = &Suggestion> {
        self.suggestions.values()
    }

    pub fn conversations_of(&self, id: SessionId) -> impl Iterator<Item = &Conversation> {
        self.conversations
            .values()
            .filter(move |c| c.session_id == id)
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(String::as_str)
    }

    /// Rows loaded with a default for text this build doesn't know.
    pub fn unknown(&self) -> &[LoadNote] {
        &self.unknown
    }

    /// The id the next row of `table` takes.
    #[cfg_attr(not(test), allow(dead_code))] // first caller: Tasks 9-11
    pub(crate) fn next_id(&self, table: Table) -> i64 {
        let n = &self.next_ids;
        match table {
            Table::Project => n.project,
            Table::Task => n.task,
            Table::Note => n.note,
            Table::Suggestion => n.suggestion,
            Table::Ask => n.ask,
            Table::Event => n.event,
            Table::Activity => n.activity,
            Table::Feed => n.feed,
        }
    }

    /// The time views are computed at. The caller sets it from `Now` before publishing.
    #[allow(dead_code)] // first caller: Tasks 9-11
    pub(crate) fn set_now(&mut self, now: UtcMs) {
        self.now = now;
    }

    /// The session's current conversation: the latest first seen.
    pub(crate) fn current_conversation(&self, id: SessionId) -> Option<&Conversation> {
        self.conversations_of(id)
            .max_by_key(|c| (c.first_seen_at, c.last_seen_at))
    }

    /// What a session is called to people: its current conversation id's first 8 characters,
    /// or its own id's when it has none.
    pub fn handle_of(&self, id: SessionId) -> String {
        match self.current_conversation(id) {
            Some(c) => first8(&c.conversation_id),
            None => first8(&id.to_string()),
        }
    }

    /// Finds who is asking: moa, a known session (by moa id, then by conversation), or an
    /// unknown agent named by its folder.
    pub fn caller(&self, c: &CallerInfo) -> Caller {
        if c.moa_session.as_deref() == Some("moa") {
            return Caller::Moa;
        }
        let known = |id: SessionId| Caller::Session(id, self.handle_of(id));
        if let Some(id) = c
            .moa_session
            .as_deref()
            .and_then(|s| s.parse::<SessionId>().ok())
            .filter(|id| self.sessions.contains_key(id))
        {
            return known(id);
        }
        if let Some(conv) = c
            .claude_session
            .as_deref()
            .and_then(|s| self.conversations.values().find(|v| v.conversation_id == s))
        {
            return known(conv.session_id);
        }
        let folder = c.cwd.as_ref().and_then(|p| p.file_name()).map_or_else(
            || OUTSIDE_MOA.to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Caller::Unknown { folder }
    }

    /// Replaces every row. The published rev is kept.
    #[cfg_attr(not(test), allow(dead_code))] // first caller: Tasks 9-11
    pub(crate) fn load(&mut self, snap: Snapshot) {
        fn by<K: Ord, V>(rows: Vec<V>, key: impl Fn(&V) -> K) -> BTreeMap<K, V> {
            rows.into_iter().map(|r| (key(&r), r)).collect()
        }
        self.projects = by(snap.projects, |r| r.id);
        self.tasks = by(snap.tasks, |r| r.id);
        self.notes = by(snap.notes, |r| r.id);
        self.suggestions = by(snap.suggestions, |r| r.id);
        self.sessions = by(snap.sessions, |r| r.id);
        self.conversations = by(snap.conversations, conv_key);
        self.groups = by(snap.groups, |r| r.path.clone());
        self.asks = by(snap.asks, |r| r.id);
        self.pane_bindings = by(snap.pane_bindings, |r| r.session_id);
        self.spike_panes = by(snap.spike_panes, |r| r.session_id);
        self.threads = by(snap.threads, |r| r.thread_id.clone());
        self.nags = by(snap.nags, nag_key);
        self.activity = by(snap.activity_tail, |r| r.id);
        self.trim_activity();
        self.meta = snap.meta.into_iter().collect();
        self.next_ids = snap.next_ids;
        self.unknown = snap.unknown;
    }

    #[cfg_attr(not(test), allow(dead_code))] // first caller: Tasks 9-11
    fn trim_activity(&mut self) {
        while self.activity.len() > ACTIVITY_SHOWN {
            self.activity.pop_first();
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // first caller: Tasks 9-11
    fn bump(next: &mut i64, id: i64) {
        *next = (*next).max(id.saturating_add(1));
    }

    /// The in-memory twin of the store's SQL for this change. Events and feed entries are
    /// not kept, only their ids are counted.
    #[cfg_attr(not(test), allow(dead_code))] // first caller: Tasks 9-11
    pub(crate) fn apply(&mut self, c: &Change) {
        match c {
            Change::Project(r) => {
                Self::bump(&mut self.next_ids.project, r.id.0);
                self.projects.insert(r.id, r.clone());
            }
            Change::Task(r) => {
                Self::bump(&mut self.next_ids.task, r.id.0);
                self.tasks.insert(r.id, r.clone());
            }
            Change::Note(r) => {
                Self::bump(&mut self.next_ids.note, r.id.0);
                self.notes.insert(r.id, r.clone());
            }
            Change::Suggestion(r) => {
                Self::bump(&mut self.next_ids.suggestion, r.id.0);
                self.suggestions.insert(r.id, r.clone());
            }
            Change::Session(r) => {
                self.sessions.insert(r.id, r.clone());
            }
            Change::Conversation(r) => {
                self.conversations.insert(conv_key(r), r.clone());
            }
            Change::PaneBinding(r) => {
                self.pane_bindings.insert(r.session_id, r.clone());
            }
            Change::SpikePane(r) => {
                self.spike_panes.insert(r.session_id, r.clone());
            }
            Change::Event(r) => Self::bump(&mut self.next_ids.event, r.id.0),
            Change::Activity(r) => {
                Self::bump(&mut self.next_ids.activity, r.id.0);
                self.activity.insert(r.id, r.clone());
                self.trim_activity();
            }
            Change::Group(r) => {
                self.groups.insert(r.path.clone(), r.clone());
            }
            Change::Ask(r) => {
                Self::bump(&mut self.next_ids.ask, r.id.0);
                self.asks.insert(r.id, r.clone());
            }
            Change::Thread(r) => {
                self.threads.insert(r.thread_id.clone(), r.clone());
            }
            Change::Feed(r) => Self::bump(&mut self.next_ids.feed, r.id),
            Change::Nag(r) => {
                self.nags.insert(nag_key(r), *r);
            }
            Change::Meta { key, value } => match value {
                Some(v) => {
                    self.meta.insert(key.clone(), v.clone());
                }
                None => {
                    self.meta.remove(key);
                }
            },
            Change::DeletePaneBinding(id) => {
                self.pane_bindings.remove(id);
            }
            Change::DeleteSpikePane(id) => {
                self.spike_panes.remove(id);
            }
            Change::DeleteGroup(path) => {
                self.groups.remove(path);
            }
        }
    }
}

#[cfg(test)]
mod tests;
