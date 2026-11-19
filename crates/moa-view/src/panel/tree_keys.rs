//! The tree's own keys (V7): `n` from a group, `+`, `m`, `r` and `D`, and the group they act
//! in. The keys every view shares on a session row stay with the panel.

use moa_core::op::Op;

use super::ask::{Ask, default_account};
use super::{Out, Panel};
use crate::ctx::Ctx;
use crate::data::Data;
use crate::keys::Action;
use crate::line::Item;
use crate::text::tilde;
use crate::views::tree;

/// What `r` and `D` say on a node made from a project's name, or on `Loose`: only a real
/// group can be renamed or deleted, so nothing is asked or sent.
fn not_a_group(path: &str) -> String {
    format!("{} isn't a group", leaf(path))
}

/// The last part of a tree path: what a group's rename prompt is prefilled with.
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

impl Panel {
    /// Does one of the tree's own actions to the selected item; `None` when it isn't one, or
    /// doesn't apply to it.
    pub(super) fn tree_key(&mut self, action: Action, data: &Data, ctx: &Ctx) -> Option<Vec<Out>> {
        let selected = self.selected().cloned();
        let session = self.selected_session(data);
        match action {
            Action::NewSession => Some(self.new_session(data, ctx)),
            Action::NewGroup => {
                let prefill = self
                    .selected_group(data)
                    .map_or(String::new(), |g| format!("{g}/"));
                Some(self.ask("new group:".to_string(), prefill, Ask::NewGroup))
            }
            Action::Move => {
                let row = session?;
                let label = format!("move {} to group:", tree::name(row));
                let prefill = self.selected_group(data).unwrap_or_default();
                let q = row.session.id.to_string();
                Some(self.ask(label, prefill, Ask::Move { q }))
            }
            Action::Rename => {
                if let Some(row) = session {
                    let prefill = row.session.title.clone().unwrap_or_default();
                    let q = row.session.id.to_string();
                    return Some(self.ask(
                        "rename session:".to_string(),
                        prefill,
                        Ask::RenameSession { q },
                    ));
                }
                let Item::Group(old) = selected? else {
                    return None;
                };
                if !tree::is_group(data, &old) {
                    return Some(self.say(&not_a_group(&old)));
                }
                let prefill = leaf(&old).to_string();
                Some(self.ask(
                    "rename group:".to_string(),
                    prefill,
                    Ask::RenameGroup { old },
                ))
            }
            Action::DeleteGroup => {
                let Item::Group(path) = selected? else {
                    return None;
                };
                if !tree::is_group(data, &path) {
                    return Some(self.say(&not_a_group(&path)));
                }
                Some(vec![Out::Send(Op::GroupRm { path })])
            }
            _ => None,
        }
    }

    /// The group the selection is in: a node's own path when it is a real group, or a
    /// session's group. A node made from a project's name, or `Loose`, is the top level: a new
    /// session there must not make it a permanent group (V5).
    fn selected_group(&self, data: &Data) -> Option<String> {
        match self.selected()? {
            Item::Group(path) if tree::is_group(data, path) => Some(path.clone()),
            Item::Session(id) => data
                .sessions
                .get(id)?
                .session
                .group_path
                .as_deref()
                .and_then(tree::tidy),
            _ => None,
        }
    }

    /// The tree's `n` (V7): asks the folder, prefilled from the newest session in the
    /// selected group or below it, else the selected session's, else the brain's first
    /// folder, else `~/`.
    fn new_session(&mut self, data: &Data, ctx: &Ctx) -> Vec<Out> {
        let group = self.selected_group(data);
        let from = match self.selected() {
            Some(Item::Group(path)) => tree::listed(data)
                .filter(|r| tree::is_under(&tree::node_of(r), path))
                .max_by_key(|r| (r.session.last_seen_at, r.session.id)),
            Some(Item::Session(id)) => data.sessions.get(id),
            _ => None,
        };
        // Field by field: a session without a folder still lends its account.
        let brain = data.brain.as_ref();
        let folder = from
            .and_then(|r| r.session.cwd.clone())
            .or_else(|| brain.and_then(|b| b.folders.first().cloned()));
        let account = from
            .and_then(|r| r.session.account.clone())
            .or_else(|| brain.and_then(|b| b.open_account.clone()));
        let folder = folder.map_or("~/".to_string(), |f| tilde(&f, &ctx.home));
        let label = format!(
            "new session in {} · folder:",
            group.as_deref().unwrap_or("top level")
        );
        let account = default_account(data, account.as_deref());
        self.ask(label, folder, Ask::Folder { group, account })
    }
}
