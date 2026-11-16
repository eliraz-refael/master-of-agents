//! `group add PATH`.

use serde_json::json;

use crate::Now;
use crate::change::Change;
use crate::core::Tx;
use crate::model::{Group, normalize_group_path};
use crate::op::{Done, OpError};
use crate::state::State;

pub(crate) fn add(state: &mut State, tx: &mut Tx, now: &Now, raw: &str) -> Result<Done, OpError> {
    let path = normalize_group_path(raw);
    if path.is_empty() {
        return Err(OpError("a group needs a name".into()));
    }
    let mut changes = Vec::new();
    if !state.groups.contains_key(&path) {
        changes.push(Change::Group(Group {
            path: path.clone(),
            collapsed: false,
            created_at: now.wall,
        }));
    }
    // Logged either way, as the spike did.
    tx.ui(state, changes, format!("new group {path}"))?;
    Ok(Done {
        text: "ok".into(),
        data: json!({ "path": path }),
    })
}
