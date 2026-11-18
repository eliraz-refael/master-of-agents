//! A `Change` is one edit to one row. The store turns it into SQL and the state applies it in
//! memory, so both stay in step.

use crate::model::{
    ActivityRow, Ask, Conversation, Event, FeedEntry, Group, MoaThread, Nag, Note, PaneBinding,
    Project, Session, SessionId, SpikePane, Suggestion, Task,
};
use crate::publish::Topic;

#[derive(Clone, PartialEq, Debug)]
pub enum Change {
    Project(Project),
    Task(Task),
    Note(Note),
    Suggestion(Suggestion),
    Session(Session),
    Conversation(Conversation),
    PaneBinding(PaneBinding),
    SpikePane(SpikePane),
    Event(Event),
    Activity(ActivityRow),
    Group(Group),
    Ask(Ask),
    Thread(MoaThread),
    Feed(FeedEntry),
    Nag(Nag),
    /// `None` deletes the key.
    Meta {
        key: String,
        value: Option<String>,
    },
    DeletePaneBinding(SessionId),
    DeleteSpikePane(SessionId),
    DeleteGroup(String),
}

/// The key of a `conversations` row: `<agent_kind>/<conversation_id>`. The store names rows it
/// can't load with this too, so a skipped row and a later write share one spelling.
pub fn conversation_key(agent_kind: impl std::fmt::Display, conversation_id: &str) -> String {
    format!("{agent_kind}/{conversation_id}")
}

/// The key of a `nags` row: `#<task_id>/<due>/<day>`.
pub fn nag_key(
    task_id: impl std::fmt::Display,
    due: impl std::fmt::Display,
    day: impl std::fmt::Display,
) -> String {
    format!("#{task_id}/{due}/{day}")
}

impl Change {
    /// The topics this change can alter.
    pub fn topics(&self) -> &'static [Topic] {
        match self {
            Change::Session(_) => &[Topic::Sessions, Topic::Status],
            // A session row names its conversation and says whether its binding is adopted.
            Change::Conversation(_) | Change::PaneBinding(_) | Change::DeletePaneBinding(_) => {
                &[Topic::Sessions]
            }
            Change::Project(_) => &[Topic::Projects, Topic::Sessions],
            Change::Activity(_) => &[Topic::Activity],
            Change::Task(_) => &[Topic::Tasks],
            Change::Suggestion(_) => &[Topic::Suggestions],
            Change::Ask(_) => &[Topic::Asks],
            Change::Group(_) | Change::DeleteGroup(_) => &[Topic::Groups],
            Change::Event(_)
            | Change::Note(_)
            | Change::SpikePane(_)
            | Change::Thread(_)
            | Change::Feed(_)
            | Change::Nag(_)
            | Change::DeleteSpikePane(_) => &[],
            // Folds are the only meta the views read.
            Change::Meta { .. } => &[Topic::Brain],
        }
    }

    /// `(table, key)` of the row this edits. `.0` is always the SQL table name. Keys: `#<id>` for
    /// integer ids; the moa id for sessions and their panes; the path for groups; the thread id
    /// for `moa_threads`; `<agent_kind>/<conversation_id>` for conversations (the table's primary
    /// key, kind as its SQL text); `#<task_id>/<due>/<day>` for nags. `None` for `Meta`.
    pub fn row(&self) -> Option<(&'static str, String)> {
        Some(match self {
            Change::Project(r) => ("projects", format!("#{}", r.id)),
            Change::Task(r) => ("tasks", format!("#{}", r.id)),
            Change::Note(r) => ("notes", format!("#{}", r.id)),
            Change::Suggestion(r) => ("suggestions", format!("#{}", r.id)),
            Change::Session(r) => ("sessions", r.id.to_string()),
            Change::Conversation(r) => (
                "conversations",
                conversation_key(r.agent_kind, &r.conversation_id),
            ),
            Change::PaneBinding(r) => ("pane_bindings", r.session_id.to_string()),
            Change::SpikePane(r) => ("spike_panes", r.session_id.to_string()),
            Change::Event(r) => ("events", format!("#{}", r.id)),
            Change::Activity(r) => ("activity", format!("#{}", r.id)),
            Change::Group(r) => ("groups", r.path.clone()),
            Change::Ask(r) => ("asks", format!("#{}", r.id)),
            Change::Thread(r) => ("moa_threads", r.thread_id.clone()),
            Change::Feed(r) => ("feed", format!("#{}", r.id)),
            Change::Nag(r) => ("nags", nag_key(r.task_id, r.due, r.day)),
            Change::Meta { .. } => return None,
            Change::DeletePaneBinding(id) => ("pane_bindings", id.to_string()),
            Change::DeleteSpikePane(id) => ("spike_panes", id.to_string()),
            Change::DeleteGroup(path) => ("groups", path.clone()),
        })
    }
}
