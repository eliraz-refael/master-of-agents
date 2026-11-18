//! Today (the spike's `Today` view): today's tasks, the live sessions waiting on you, the ones
//! working, and moa's suggestions to answer with `y` or `n`.

use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::{SuggestionStatus, Task, TaskStatus};
use moa_core::time::start_of_next_day;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Item, Line, Span, Tone};
use crate::text::{DAY, clean, cut};
use crate::views::plan::NO_PROJECT;
use crate::views::{ViewUi, live, newest_first, session_line};

/// Today's lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, _ui: &ViewUi, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let midnight = start_of_next_day(ctx.today, &ctx.tz);
    let head = |text: &str| Line::new(vec![Span::bold(cut(text, width), Tone::Head)]);
    let dim = |text: &str| Line::plain(cut(text, width), Tone::Dim);

    let mut title = head(&format!("TODAY · {}", ctx.today.strftime("%a %d %b")));
    title.changes_at = Some(midnight);
    let mut lines = vec![title];

    // Today's tasks, the done ones last.
    let mut tasks: Vec<&Task> = data
        .tasks
        .values()
        .filter(|t| t.today == Some(ctx.today))
        .collect();
    tasks.sort_by_key(|t| (t.status == TaskStatus::Done, t.id));
    if tasks.is_empty() {
        lines.push(dim("  (nothing yet: set today's goals with moa)"));
    }
    for t in tasks {
        let mut line = task_line(data, t, width);
        // Which day is today changes at midnight.
        line.changes_at = Some(midnight);
        lines.push(line);
    }

    // Live sessions waiting on you, seen in the last day, the newest first. A quiet one is a
    // working turn gone silent, not waiting on you: it stays with the working ones.
    let since = UtcMs(ctx.now.0.saturating_sub(DAY));
    let need = newest_first(data, |r| {
        live(r.display_state)
            && !working(r.display_state)
            && r.session.last_seen_at.is_some_and(|seen| seen >= since)
    });
    lines.push(Line::new(Vec::new()));
    lines.push(head("WAITING ON YOU"));
    if need.is_empty() {
        lines.push(dim("  (nothing)"));
    }
    for row in need {
        let tone = if row.display_state == DisplayState::Waiting {
            Tone::Warn
        } else {
            Tone::Text
        };
        lines.push(session_line(row, ctx, width, tone, ""));
    }

    let busy = newest_first(data, |r| working(r.display_state));
    if !busy.is_empty() {
        lines.push(Line::new(Vec::new()));
        lines.push(head("WORKING"));
        for row in busy {
            // Quiet is cyan only in the status panel (V16).
            let tone = if row.display_state == DisplayState::Quiet {
                Tone::Dim
            } else {
                Tone::Ok
            };
            lines.push(session_line(row, ctx, width, tone, ""));
        }
    }

    // Open suggestions and the ones put off, by id.
    lines.push(Line::new(Vec::new()));
    lines.push(head("SUGGESTIONS  (y accept · n reject)"));
    let open = data
        .suggestions
        .values()
        .filter(|s| matches!(s.status, SuggestionStatus::Open | SuggestionStatus::Later));
    let mut any = false;
    for s in open {
        any = true;
        let text = cut(&format!("? #{} {}", s.id.0, clean(&s.text)), width);
        lines.push(Line::plain(text, Tone::Brain).with_item(Item::Suggestion(s.id)));
    }
    if !any {
        lines.push(dim("  (none)"));
    }
    lines
}

/// `✓`/`▸ <project>: <title>  [<status>]`, a done one dimmed.
fn task_line(data: &Data, t: &Task, width: usize) -> Line {
    let done = t.status == TaskStatus::Done;
    let project = t
        .project_id
        .and_then(|id| data.projects.get(&id))
        .map_or_else(|| NO_PROJECT.to_string(), |p| clean(&p.name));
    let text = format!(
        "{} {project}: {}  [{}]",
        if done { "✓" } else { "▸" },
        clean(&t.title),
        t.status.as_str()
    );
    let tone = if done { Tone::Dim } else { Tone::Text };
    Line::plain(cut(&text, width), tone).with_item(Item::Task(t.id))
}

/// Mid-turn: working, or working but silent past the quiet limit (the core's `Quiet`).
fn working(state: DisplayState) -> bool {
    matches!(state, DisplayState::Working | DisplayState::Quiet)
}
