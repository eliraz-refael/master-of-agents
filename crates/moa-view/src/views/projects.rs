//! Projects (the spike's `Projects` view): every project on one line, the ideas last. An
//! opened one (Enter, kept in [`ViewUi::expanded`]) lists its summary, its open tasks and its
//! last six sessions under it.

use std::cmp::Reverse;

use moa_core::UtcMs;
use moa_core::model::{Project, ProjectStatus};
use moa_core::time::start_of_next_day;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Item, Line, Tone};
use crate::text::{self, clean, cut};
use crate::views::{ViewUi, live, newest_first, session_line};

/// A project idle longer than this many days is dimmed.
const STALE_DAYS: i64 = 60;

/// How many of an open project's sessions are listed, the last seen.
const SESSIONS: usize = 6;

/// What an open project's rows are indented by.
const INDENT: &str = "    ";

/// The projects' lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let midnight = start_of_next_day(ctx.today, &ctx.tz);
    // Ideas last, then the most recently active (none last), then by name.
    let mut projects: Vec<&Project> = data.projects.values().collect();
    projects.sort_by(|a, b| {
        let key = |p: &Project| (p.status == ProjectStatus::Idea, Reverse(p.last_activity_on));
        key(a)
            .cmp(&key(b))
            .then_with(|| a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });

    let mut lines = Vec::new();
    for p in projects {
        let open = ui.expanded.contains(&p.id);
        lines.push(project_line(data, ctx, p, open, width, midnight));
        if open {
            opened(&mut lines, data, ctx, p, width);
        }
    }
    lines
}

/// `▸/▾ <name>  <status> <idle>d  <n> live · <n> tasks` (the spike's): the idle days only
/// when the project has had activity, each count only when it isn't 0. An idea is in the
/// brain's colour, a project idle over 60 days dim.
fn project_line(
    data: &Data,
    ctx: &Ctx,
    p: &Project,
    open: bool,
    width: usize,
    midnight: UtcMs,
) -> Line {
    let idle = p
        .last_activity_on
        .map(|day| (-text::days_left(day, ctx.today)).max(0));
    let running = data
        .sessions
        .values()
        .filter(|r| r.session.project_id == Some(p.id) && live(r.display_state))
        .count();
    let tasks = data
        .tasks
        .values()
        .filter(|t| t.project_id == Some(p.id) && t.status.is_open())
        .count();

    let arrow = if open { "▾" } else { "▸" };
    let mut text = format!("{arrow} {}  {}", clean(&p.name), p.status.as_str());
    if let Some(days) = idle {
        text.push_str(&format!(" {days}d"));
    }
    let counts: Vec<String> = [(running, "live"), (tasks, "tasks")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
    if !counts.is_empty() {
        text.push_str("  ");
        text.push_str(&counts.join(" · "));
    }

    let tone = if p.status == ProjectStatus::Idea {
        Tone::Brain
    } else if idle.is_some_and(|d| d > STALE_DAYS) {
        Tone::Dim
    } else {
        Tone::Text
    };
    let mut line = Line::plain(cut(&text, width), tone).with_item(Item::Project(p.id));
    // The idle days count up at local midnight.
    line.changes_at = idle.map(|_| midnight);
    line
}

/// An open project's rows: its summary (dim), its open tasks by id, and its last six
/// sessions, the last seen first, dim when not running.
fn opened(lines: &mut Vec<Line>, data: &Data, ctx: &Ctx, p: &Project, width: usize) {
    let summary = p.summary.as_deref().map(clean).unwrap_or_default();
    if !summary.trim().is_empty() {
        lines.push(Line::plain(
            cut(&format!("{INDENT}{summary}"), width),
            Tone::Dim,
        ));
    }
    let tasks = data
        .tasks
        .values()
        .filter(|t| t.project_id == Some(p.id) && t.status.is_open());
    for t in tasks {
        let mut text = format!("{INDENT}▸ {}  [{}]", clean(&t.title), t.status.as_str());
        let blocked = t.blocked_on.as_deref().map(clean).unwrap_or_default();
        if !blocked.trim().is_empty() {
            text.push_str(&format!(" ⧗ {blocked}"));
        }
        lines.push(Line::plain(cut(&text, width), Tone::Text).with_item(Item::Task(t.id)));
    }
    let sessions = newest_first(data, |r| r.session.project_id == Some(p.id));
    for row in sessions.into_iter().take(SESSIONS) {
        let tone = if live(row.display_state) {
            Tone::Text
        } else {
            Tone::Dim
        };
        lines.push(session_line(row, ctx, width, tone, INDENT));
    }
}
