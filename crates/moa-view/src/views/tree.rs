//! The sessions tree (spec R1 §2.3, the spike's `session_tree` and `tree_line`): a header of
//! counts, then every session that isn't archived under its group, its project's node or
//! `Loose`, with the brain's empty groups among them.

use std::collections::{BTreeMap, BTreeSet};

use moa_core::UtcMs;
use moa_core::live::DisplayState;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, fit, fit_right, short_account, short_path};
use crate::views::{ViewUi, animates, live, spinner, state_glyph};

/// The node of the sessions with neither a group nor a project.
pub const LOOSE: &str = "Loose";

/// `path` without empty parts (`A//B/` is `A/B`); `None` when nothing is left.
pub fn tidy(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The tree path a session sits under: its group, else its project's name, else `Loose`.
pub fn node_of(row: &SessionRow) -> String {
    [row.session.group_path.as_deref(), row.project.as_deref()]
        .into_iter()
        .flatten()
        .find_map(tidy)
        .unwrap_or_else(|| LOOSE.to_string())
}

/// The sessions the tree lists: every one that isn't archived.
pub fn listed(data: &Data) -> impl Iterator<Item = &SessionRow> {
    data.sessions
        .values()
        .filter(|r| r.display_state != DisplayState::Archived)
}

/// What a prompt calls a session: its title, else its handle; cleaned.
pub fn name(row: &SessionRow) -> String {
    let title = row.session.title.as_deref().filter(|t| !t.is_empty());
    clean(title.unwrap_or(&row.handle))
}

/// What a tree line calls a session: its title, else its last prompt's first 60 characters,
/// else its handle; cleaned.
fn line_title(row: &SessionRow) -> String {
    let title = row.session.title.as_deref().filter(|t| !t.is_empty());
    let prompt = row.session.last_prompt.as_deref().filter(|p| !p.is_empty());
    match (title, prompt) {
        (Some(title), _) => clean(title),
        (None, Some(prompt)) => clean(prompt).chars().take(60).collect(),
        (None, None) => clean(&row.handle),
    }
}

/// The deepest a tree nests.
const MAX_DEPTH: usize = 32;

#[derive(Default)]
struct Node<'a> {
    children: BTreeMap<String, Node<'a>>,
    sessions: Vec<&'a SessionRow>,
}

impl<'a> Node<'a> {
    /// The node at `path` under this one, made as needed. Past `MAX_DEPTH` levels the rest
    /// of the path is one node's name, so no path from data nests the walk without end; the
    /// node's path is still the whole path.
    fn at(&mut self, path: &str) -> &mut Node<'a> {
        path.splitn(MAX_DEPTH, '/').fold(self, |node, part| {
            node.children.entry(part.to_string()).or_default()
        })
    }

    /// Every session at or below this node.
    fn members(&self, out: &mut Vec<&'a SessionRow>) {
        out.extend(self.sessions.iter().copied());
        for child in self.children.values() {
            child.members(out);
        }
    }
}

/// A group line's facts: its members, and the newest moment one was seen.
struct Summary {
    count: usize,
    live: usize,
    busy: bool,
    newest: Option<UtcMs>,
}

fn summary(node: &Node) -> Summary {
    let mut members = Vec::new();
    node.members(&mut members);
    Summary {
        count: members.len(),
        live: members.iter().filter(|m| live(m.display_state)).count(),
        busy: members
            .iter()
            .any(|m| m.display_state == DisplayState::Working),
        newest: members.iter().filter_map(|m| m.session.last_seen_at).max(),
    }
}

/// What every line is built against.
struct Cx<'c> {
    ctx: &'c Ctx,
    width: usize,
    folded: BTreeSet<&'c str>,
}

/// The tree's lines, `width` cells wide.
pub fn build(data: &Data, ctx: &Ctx, _ui: &ViewUi, width: u16) -> Vec<Line> {
    let shown: Vec<&SessionRow> = listed(data).collect();
    let mut lines = vec![header(&shown, ctx.tick)];

    let mut root = Node::default();
    for row in &shown {
        root.at(&node_of(row)).sessions.push(row);
    }
    for path in data.groups.keys().filter_map(|p| tidy(p)) {
        root.at(&path);
    }
    let folded = data
        .brain
        .iter()
        .flat_map(|b| b.folded.iter().map(String::as_str))
        .collect();
    let cx = Cx {
        ctx,
        width: usize::from(width),
        folded,
    };
    walk(&root, "", "", 0, &cx, &mut lines);
    lines
}

/// `<n> sessions · <n> live`, then the asleep, working and waiting counts that aren't zero.
fn header(shown: &[&SessionRow], tick: u64) -> Line {
    let count =
        |f: &dyn Fn(DisplayState) -> bool| shown.iter().filter(|r| f(r.display_state)).count();
    let live = count(&live);
    let asleep = count(&|s| s == DisplayState::Asleep);
    let working = count(&|s| s == DisplayState::Working);
    let waiting = count(&|s| s == DisplayState::Waiting);
    let mut head = format!("{} sessions · {live} live", shown.len());
    if asleep > 0 {
        head.push_str(&format!(" · {asleep} asleep"));
    }
    if working > 0 {
        head.push_str(&format!(" · {} {working} working", spinner(tick)));
    }
    if waiting > 0 {
        head.push_str(&format!(" · {waiting} waiting on you"));
    }
    let mut line = Line::plain(head, Tone::Dim);
    line.animated = working > 0;
    line
}

/// Lays out `node`'s children: its groups (newest member first, empty ones after by name),
/// then its sessions (newest first), each under the guides of the levels above.
fn walk(node: &Node, prefix: &str, path: &str, depth: usize, cx: &Cx, out: &mut Vec<Line>) {
    let mut groups: Vec<(&String, &Node, Summary)> = node
        .children
        .iter()
        .map(|(name, child)| (name, child, summary(child)))
        .collect();
    groups.sort_by(|a, b| {
        (a.2.count == 0)
            .cmp(&(b.2.count == 0))
            .then(b.2.newest.cmp(&a.2.newest))
            .then(a.0.cmp(b.0))
    });
    let mut sessions = node.sessions.clone();
    sessions.sort_by(|a, b| {
        b.session
            .last_seen_at
            .cmp(&a.session.last_seen_at)
            .then(a.session.id.cmp(&b.session.id))
    });

    let n_groups = groups.len();
    let total = n_groups + sessions.len();
    let guides = |i: usize| match (depth, i + 1 == total) {
        (0, _) => ("", ""),
        (_, true) => ("└─ ", "   "),
        (_, false) => ("├─ ", "│  "),
    };
    for (i, (name, child, sum)) in groups.into_iter().enumerate() {
        let (branch, cont) = guides(i);
        let here = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        let folded = cx.folded.contains(here.as_str());
        let mut label = format!(
            "{prefix}{branch}{} {}  {}",
            if folded { "▸" } else { "▾" },
            clean(name),
            sum.count
        );
        if sum.live > 0 {
            label.push_str(&format!(" · {} live", sum.live));
        }
        if sum.busy {
            label.push(' ');
            label.push_str(spinner(cx.ctx.tick));
        }
        let tone = if depth == 0 { Tone::Head } else { Tone::Group };
        let mut line =
            Line::new(vec![Span::bold(label, tone)]).with_item(Item::Group(here.clone()));
        line.animated = sum.busy;
        out.push(line);
        if !folded {
            walk(child, &format!("{prefix}{cont}"), &here, depth + 1, cx, out);
        }
    }
    for (i, row) in sessions.into_iter().enumerate() {
        let (branch, _) = guides(n_groups + i);
        let left = cx
            .width
            .saturating_sub(text::width(prefix) + text::width(branch));
        let text = format!("{prefix}{branch}{}", session_text(row, left, cx.ctx));
        let (tone, bold) = session_tone(row.display_state);
        let span = Span { text, tone, bold };
        let mut line = Line::new(vec![span]).with_item(Item::Session(row.session.id));
        line.animated = animates(row.display_state);
        line.changes_at = row
            .session
            .last_seen_at
            .map(|seen| text::next_ago_change(seen, cx.ctx.now));
        out.push(line);
    }
}

/// A session line's tone, and whether it is bold: the states that want you stand out.
fn session_tone(state: DisplayState) -> (Tone, bool) {
    match state {
        DisplayState::Waiting => (Tone::Warn, true),
        DisplayState::Question => (Tone::Ask, true),
        DisplayState::Done => (Tone::Ok, true),
        DisplayState::Failed => (Tone::Fail, true),
        s if live(s) => (Tone::Ok, false),
        _ => (Tone::Dim, false),
    }
}

/// A session line in `width` cells (the spike's `tree_line`): the glyph and a space (or 💤,
/// two cells on its own), the title, the account in 6 columns from a width of 34, the folder
/// in 23 from 76, and the age right-aligned in 4. Narrow lines keep 8 cells of title.
fn session_text(row: &SessionRow, width: usize, ctx: &Ctx) -> String {
    let s = &row.session;
    let lead = match row.display_state {
        DisplayState::Asleep => "💤".to_string(),
        state => format!("{} ", state_glyph(state, ctx.tick)),
    };
    let mut cols: Vec<(String, usize)> = Vec::new();
    if width >= 34 {
        let account = clean(short_account(s.account.as_deref().unwrap_or_default()));
        cols.push((account, 6));
    }
    if width >= 76 {
        let folder = clean(&short_path(s.cwd.as_deref().unwrap_or_default(), &ctx.home));
        cols.push((folder, 23));
    }
    let used: usize = cols.iter().map(|(_, n)| n).sum();
    let title_w = width.saturating_sub(7 + used).max(8);
    let extra: String = cols
        .iter()
        .map(|(text, n)| fit(&cut(text, n - 1), *n))
        .collect();
    format!(
        "{lead}{} {extra}{}",
        fit(&line_title(row), title_w),
        fit_right(&text::ago(s.last_seen_at, ctx.now), 4)
    )
}
