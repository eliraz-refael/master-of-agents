//! The status panel (V16, the spike's `status`): every live session on one line, grouped by
//! what it needs from you, the most urgent group first. Compact enough to hold ~30 sessions.

use std::collections::BTreeMap;

use moa_core::live::DisplayState;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, fit, fit_right};
use crate::views::{ViewUi, animates, label, live, state_glyph, tree};

/// What the panel says when no session is live.
pub const EMPTY: &str = "no live sessions: Ctrl-a / finds one to resume";

/// A live state's group heading (V16).
fn word(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Waiting => "needs you",
        DisplayState::Question => "asks you",
        DisplayState::Failed => "failed",
        DisplayState::Done => "done",
        DisplayState::Working => "working",
        DisplayState::Quiet => "quiet",
        _ => "idle",
    }
}

/// A group's tone, and whether it is bold: the states that want you stand out. Quiet is cyan
/// here only (V16).
fn look(state: DisplayState) -> (Tone, bool) {
    match state {
        DisplayState::Waiting => (Tone::Warn, true),
        DisplayState::Question => (Tone::Ask, true),
        DisplayState::Failed => (Tone::Fail, true),
        DisplayState::Done => (Tone::Ok, true),
        DisplayState::Working => (Tone::Work, false),
        DisplayState::Quiet => (Tone::Quiet, false),
        _ => (Tone::Dim, false),
    }
}

/// The status panel's lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, _ui: &ViewUi, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let mut lines = Vec::new();
    // `DisplayState`'s order is the urgency order.
    let mut groups: BTreeMap<DisplayState, Vec<&SessionRow>> = BTreeMap::new();
    for row in data.sessions.values().filter(|r| live(r.display_state)) {
        groups.entry(row.display_state).or_default().push(row);
    }
    for (state, mut members) in groups {
        members.sort_by(|a, b| {
            b.session
                .activity_at
                .cmp(&a.session.activity_at)
                .then(a.session.id.cmp(&b.session.id))
        });
        lines.push(heading(state, members.len(), width));
        for row in members {
            lines.push(session_line(row, ctx, width));
        }
    }
    if lines.is_empty() {
        lines.push(Line::plain(cut(EMPTY, width), Tone::Dim));
    }
    lines
}

/// `<word> ──── <n>`, a cell short of the width, in its group's tone; idle's is a heading's.
fn heading(state: DisplayState, count: usize, width: usize) -> Line {
    let (word, n) = (word(state), count.to_string());
    let rule = width.saturating_sub(text::width(word) + n.len() + 3).max(2);
    let text = cut(&format!("{word} {} {n}", "─".repeat(rule)), width);
    let (tone, bold) = if state == DisplayState::Idle {
        (Tone::Head, true)
    } else {
        look(state)
    };
    Line::new(vec![Span::styled(text, tone, bold)])
}

/// One session (the spike's `status_line`): its glyph, its title, its group's last part in 14
/// columns from a width of 44, the age of its last activity in 4, and its why from 72. The
/// title takes the rest, at most 26 columns once the why shows, and at least 8.
fn session_line(row: &SessionRow, ctx: &Ctx, width: usize) -> Line {
    let s = &row.session;
    let gw = if width >= 44 { 14 } else { 0 };
    let rest = width.saturating_sub(8 + gw);
    let tw = if width >= 72 { rest.min(26) } else { rest }.max(8);
    let group = s
        .group_path
        .as_deref()
        .and_then(tree::tidy)
        .and_then(|g| g.rsplit('/').next().map(str::to_string))
        .or_else(|| row.project.clone())
        .map(|g| clean(&g))
        .unwrap_or_default();
    let mut text = format!(
        "{} {} {}{}",
        state_glyph(row.display_state, ctx.tick),
        fit(&label(row), tw),
        fit(&cut(&group, gw.saturating_sub(1)), gw),
        fit_right(&text::ago(s.activity_at, ctx.now), 4)
    );
    let why = clean(&row.why);
    if width >= 72 && !why.trim().is_empty() {
        text.push_str("  ");
        text.push_str(&why);
    }
    let (tone, bold) = look(row.display_state);
    let span = Span::styled(cut(&text, width), tone, bold);
    let mut line = Line::new(vec![span]).with_item(Item::Session(s.id));
    line.animated = animates(row.display_state);
    line.changes_at = s.activity_at.map(|at| text::next_ago_change(at, ctx.now));
    line
}
