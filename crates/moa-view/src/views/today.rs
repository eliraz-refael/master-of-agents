//! Today (the spike's `Today` view): today's tasks, the live sessions waiting on you, the ones
//! working, and moa's suggestions to answer with `y` or `n`.

use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::{SuggestionStatus, Task, TaskStatus};
use moa_core::time::start_of_next_day;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, short_account};
use crate::views::plan::NO_PROJECT;
use crate::views::{ViewUi, animates, label, live, state_glyph};

/// How long a session that isn't working stays on the list after it was last seen.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

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

    // Live sessions that aren't working, seen in the last day, the newest first.
    let since = UtcMs(ctx.now.0.saturating_sub(DAY_MS));
    let need = newest_first(data, |r| {
        live(r.display_state)
            && r.display_state != DisplayState::Working
            && r.session.last_seen_at.is_some_and(|seen| seen >= since)
    });
    lines.push(Line::new(Vec::new()));
    lines.push(head("WAITING ON YOU"));
    if need.is_empty() {
        lines.push(dim("  (nothing)"));
    }
    for row in need {
        let tone = match row.display_state {
            DisplayState::Waiting => Tone::Warn,
            // Quiet is cyan only in the status panel (V16).
            DisplayState::Quiet => Tone::Dim,
            _ => Tone::Text,
        };
        lines.push(session_line(row, ctx, width, tone));
    }

    let working = newest_first(data, |r| r.display_state == DisplayState::Working);
    if !working.is_empty() {
        lines.push(Line::new(Vec::new()));
        lines.push(head("WORKING"));
        for row in working {
            lines.push(session_line(row, ctx, width, Tone::Ok));
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

/// The sessions `keep` keeps, the most recently seen first.
fn newest_first(data: &Data, keep: impl Fn(&SessionRow) -> bool) -> Vec<&SessionRow> {
    let mut rows: Vec<&SessionRow> = data.sessions.values().filter(|r| keep(r)).collect();
    rows.sort_by(|a, b| {
        b.session
            .last_seen_at
            .cmp(&a.session.last_seen_at)
            .then(a.session.id.cmp(&b.session.id))
    });
    rows
}

/// `<glyph> <title or project> [<account>] <age>  <prompt>` (the spike's `session_line`); a
/// prompt that is a command or a system message (`<…`) isn't words for you, so it is left out.
fn session_line(row: &SessionRow, ctx: &Ctx, width: usize, tone: Tone) -> Line {
    let s = &row.session;
    let account = clean(short_account(s.account.as_deref().unwrap_or_default()));
    let age = text::ago(s.last_seen_at, ctx.now);
    let mut text = format!(
        "{} {} [{account}] {age:>3}",
        state_glyph(row.display_state, ctx.tick),
        label(row)
    );
    let prompt = s.last_prompt.as_deref().map(clean).unwrap_or_default();
    if !prompt.trim().is_empty() && !prompt.starts_with('<') {
        text.push_str("  ");
        text.push_str(&prompt);
    }
    let mut line = Line::plain(cut(&text, width), tone).with_item(Item::Session(s.id));
    line.animated = animates(row.display_state);
    line.changes_at = s
        .last_seen_at
        .map(|seen| text::next_ago_change(seen, ctx.now));
    line
}
