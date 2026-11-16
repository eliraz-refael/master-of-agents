//! How people refer to sessions: the label, the live-state filter, and name resolution (spec
//! §6.10). Case is ignored in every match.

use std::cmp::Reverse;

use crate::live::spike_state;
use crate::model::{Session, SessionId};
use crate::op::OpError;
use crate::state::State;

/// How many lines of an ambiguous match are listed.
const LISTED: usize = 8;

/// The spike states that count as live: a session someone might be talking to.
pub fn is_live_spike(s: &Session) -> bool {
    matches!(spike_state(s), "working" | "idle" | "waiting" | "elsewhere")
}

/// The title, else the first `cut` characters of the last prompt (empty when neither).
pub fn title_or_prompt(s: &Session, cut: usize) -> String {
    match s.title.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => s
            .last_prompt
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(cut)
            .collect(),
    }
}

/// The group, else the project's name, else `-`.
pub fn place(state: &State, s: &Session) -> String {
    s.group_path
        .clone()
        .filter(|g| !g.is_empty())
        .or_else(|| {
            s.project_id
                .and_then(|p| state.projects.get(&p))
                .map(|p| p.name.clone())
        })
        .unwrap_or_else(|| "-".to_string())
}

/// `title | first <cut> chars of last_prompt | handle`, then ` [group|project|-]`.
pub fn label(state: &State, s: &Session, cut: usize) -> String {
    let name = title_or_prompt(s, cut);
    let name = if name.is_empty() {
        state.handle_of(s.id)
    } else {
        name
    };
    format!("{name} [{}]", place(state, s))
}

/// Newest `last_seen_at` first, `None` last, then by id so the order is stable.
pub(crate) fn newest_first(list: &mut [&Session]) {
    list.sort_by_key(|s| (s.last_seen_at.is_none(), Reverse(s.last_seen_at), s.id));
}

impl State {
    /// Sessions a name can mean, as the spike's `candidates` did: first by prefix of the moa
    /// id or of any conversation id (archived ones too); failing that by substring of the
    /// title, project name or group of a non-archived session, where an exact title wins.
    pub fn candidates(&self, q: &str) -> Vec<SessionId> {
        // An empty name is a prefix of everything: it never means anyone.
        if q.trim().is_empty() {
            return Vec::new();
        }
        let q = q.to_lowercase();
        let ids = |mut list: Vec<&Session>| {
            newest_first(&mut list);
            list.into_iter().map(|s| s.id).collect::<Vec<_>>()
        };
        let by_prefix: Vec<&Session> = self
            .sessions()
            .filter(|s| {
                s.id.to_string().starts_with(&q)
                    || self
                        .conversations_of(s.id)
                        .any(|c| c.conversation_id.to_lowercase().starts_with(&q))
            })
            .collect();
        if !by_prefix.is_empty() {
            return ids(by_prefix);
        }
        let has = |text: Option<&str>| text.is_some_and(|t| t.to_lowercase().contains(&q));
        let by_text: Vec<&Session> = self
            .sessions()
            .filter(|s| s.archived_at.is_none())
            .filter(|s| {
                has(s.title.as_deref())
                    || has(s
                        .project_id
                        .and_then(|p| self.projects.get(&p))
                        .map(|p| p.name.as_str()))
                    || has(s.group_path.as_deref())
            })
            .collect();
        let exact: Vec<&Session> = by_text
            .iter()
            .copied()
            .filter(|s| s.title.as_deref().is_some_and(|t| t.to_lowercase() == q))
            .collect();
        ids(if exact.is_empty() { by_text } else { exact })
    }

    /// The one session `q` means, or why not.
    pub fn resolve(&self, q: &str) -> Result<SessionId, OpError> {
        if q.trim().is_empty() {
            return Err(OpError("a session name can't be empty".into()));
        }
        let found = self.candidates(q);
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(OpError(format!("no session matches '{q}'"))),
            many => {
                let mut text = format!("{} sessions match '{q}'; be more specific:", many.len());
                for id in many.iter().take(LISTED) {
                    let Some(s) = self.session(*id) else { continue };
                    let title = s.title.as_deref().filter(|t| !t.is_empty()).unwrap_or("-");
                    text.push_str(&format!(
                        "\n  {}  {title}  ({}, {})",
                        self.handle_of(*id),
                        place(self, s),
                        spike_state(s)
                    ));
                }
                Err(OpError(text))
            }
        }
    }
}
