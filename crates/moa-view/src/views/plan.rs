//! The big picture (the spike's `plan()`): each project's open tasks and whose move each one
//! is, the projects that need you first; then the recently active projects with nothing open.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::{Priority, Task, TaskStatus, Who};
use moa_core::time::start_of_next_day;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, days_left, when};
use crate::views::{ViewUi, spinner, state_glyph};

/// The heading of the tasks with no project.
pub const NO_PROJECT: &str = "-";

/// Whose move a task is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    You,
    Agent,
    Wait,
}

/// Waiting or blocked, or on someone else, is a wait; an agent's or an active task is the
/// agents'; the rest are yours.
fn kind(t: &Task) -> Kind {
    if matches!(t.status, TaskStatus::Waiting | TaskStatus::Blocked) || t.who == Some(Who::Other) {
        Kind::Wait
    } else if t.who == Some(Who::Agent) || t.status == TaskStatus::Active {
        Kind::Agent
    } else {
        Kind::You
    }
}

/// P0 urgent, P1 important, then the rest.
fn prio(t: &Task) -> u8 {
    match t.priority {
        Some(Priority::P0) => 0,
        Some(Priority::P1) => 1,
        _ => 2,
    }
}

/// A due date that sorts after every real one when there is none.
fn due_key(due: Option<Date>) -> Date {
    due.unwrap_or(Date::MAX)
}

/// Open: not done, dropped or an idea.
fn is_open(t: &Task) -> bool {
    t.status.is_open() && t.status != TaskStatus::Idea
}

/// One project's share of the picture.
#[derive(Default)]
struct Share<'a> {
    tasks: Vec<&'a Task>,
    waiting: Vec<&'a SessionRow>,
}

impl Share<'_> {
    /// Needs you first, then the best priority, then a task on you, then the earliest due date.
    fn rank(&self) -> (bool, u8, bool, Date) {
        let mine = self.tasks.iter().any(|t| kind(t) == Kind::You);
        (
            self.waiting.is_empty(),
            self.tasks.iter().map(|t| prio(t)).min().unwrap_or(2),
            !mine,
            due_key(self.tasks.iter().filter_map(|t| t.due).min()),
        )
    }

    fn count(&self, k: Kind) -> usize {
        self.tasks.iter().filter(|t| kind(t) == k).count()
    }
}

/// The local date of `at` in `tz`.
fn local_day(at: UtcMs, tz: &TimeZone) -> Option<Date> {
    let ts = jiff::Timestamp::from_millisecond(at.0).ok()?;
    Some(tz.to_datetime(ts).date())
}

/// The plan's lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, _ui: &ViewUi, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let midnight = start_of_next_day(ctx.today, &ctx.tz);
    let mut lines = vec![header(data, ctx, width, midnight)];

    // By project name, `None` for no project: a project named like that heading stays apart.
    let mut by: BTreeMap<Option<String>, Share> = BTreeMap::new();
    let project_name = |id| data.projects.get(&id).map(|p| clean(&p.name));
    for t in data.tasks.values().filter(|t| is_open(t)) {
        let name = t.project_id.and_then(project_name);
        by.entry(name).or_default().tasks.push(t);
    }
    let waiting = data
        .sessions
        .values()
        .filter(|r| r.display_state == DisplayState::Waiting);
    for row in waiting {
        // By its project's id, as tasks are, so the two meet under one heading.
        let name = row.session.project_id.and_then(project_name);
        let name = name.or_else(|| row.project.as_deref().map(clean));
        by.entry(name).or_default().waiting.push(row);
    }

    if by.is_empty() {
        lines.push(Line::plain(
            cut(
                "  nothing tracked yet: moa fills this in as sessions report",
                width,
            ),
            Tone::Dim,
        ));
    }
    let mut order: Vec<(&Option<String>, &Share)> = by.iter().collect();
    order.sort_by_cached_key(|(name, g)| (g.rank(), *name));
    let cx = Cx {
        data,
        ctx,
        width,
        midnight,
    };
    for (name, g) in order {
        lines.push(Line::new(Vec::new()));
        lines.push(project_head(
            name.as_deref().unwrap_or(NO_PROJECT),
            g,
            width,
        ));
        let mut waiting = g.waiting.clone();
        waiting.sort_by_key(|r| r.session.id);
        for row in waiting {
            cx.waiting(row, &mut lines);
        }
        let mut tasks = g.tasks.clone();
        tasks.sort_by_key(|t| (kind(t), prio(t), due_key(t.due), t.id));
        for t in tasks {
            cx.task(t, &mut lines);
        }
    }

    let quiet = quiet_projects(data, ctx, &by);
    if !quiet.is_empty() {
        lines.push(Line::new(Vec::new()));
        let text = format!("no open tasks: {}", quiet.join(", "));
        for line in text::wrap(&text, width.saturating_sub(1).max(10)) {
            // Which projects were active in the last 7 days changes with the day.
            let mut line = Line::plain(cut(&line, width), Tone::Dim);
            line.changes_at = Some(midnight);
            lines.push(line);
        }
    }
    lines.push(Line::new(Vec::new()));
    lines.push(Line::plain(
        cut("↵ or click: jump to its session · d done · x drop", width),
        Tone::Dim,
    ));
    lines
}

/// `Fri 13 Nov`, and how many tasks were done today (by the local day). It changes at
/// midnight.
fn header(data: &Data, ctx: &Ctx, width: usize, midnight: UtcMs) -> Line {
    let done = data
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Done)
        .filter(|t| t.done_at.and_then(|at| local_day(at, &ctx.tz)) == Some(ctx.today))
        .count();
    let mut head = ctx.today.strftime("%a %d %b").to_string();
    if done > 0 {
        head.push_str(&format!(" · ✓ {done} done today"));
    }
    let mut line = Line::new(vec![Span::bold(cut(&head, width), Tone::Head)]);
    line.changes_at = Some(midnight);
    line
}

/// `<name> ──── <n> on you · <n> with agents · <n> waiting`: a group's colour when something
/// is on you, else a heading's.
fn project_head(name: &str, g: &Share, width: usize) -> Line {
    let on_you = g.count(Kind::You) + g.waiting.len();
    let counts: Vec<String> = [
        (on_you, "on you"),
        (g.count(Kind::Agent), "with agents"),
        (g.count(Kind::Wait), "waiting"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    let counts = counts.join(" · ");
    let title = format!("{name} ");
    let rule = width
        .saturating_sub(text::width(&title) + text::width(&counts) + 3)
        .max(2);
    let text = format!("{title}{} {counts}", "─".repeat(rule));
    let tone = if on_you > 0 { Tone::Group } else { Tone::Head };
    Line::new(vec![Span::bold(cut(&text, width), tone)])
}

/// The projects active in the last 7 days with nothing open, and not listed above; the most
/// recent first. A project with an open task is in `by`, so `by` rules it out.
fn quiet_projects(data: &Data, ctx: &Ctx, by: &BTreeMap<Option<String>, Share>) -> Vec<String> {
    let mut quiet: Vec<(Date, String)> = data
        .projects
        .values()
        .filter_map(|p| Some((p.last_activity_on?, clean(&p.name))))
        .filter(|(d, _)| days_left(*d, ctx.today) >= -7)
        .filter(|(_, name)| !by.contains_key(&Some(name.clone())))
        .collect();
    quiet.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    quiet.into_iter().map(|(_, name)| name).collect()
}

/// A due date as its own column: overdue and today stand out, the rest fade with distance.
fn due_chip(due: Date, today: Date) -> Span {
    let words = when(due, today);
    match days_left(due, today) {
        d if d < 0 => Span::bold(format!(" {words} "), Tone::Late),
        0 => Span::bold(format!(" {words} "), Tone::Today),
        d if d < 7 => Span::new(words, Tone::Soon),
        _ => Span::new(words, Tone::Dim),
    }
}

/// What every row is built against.
struct Cx<'c> {
    data: &'c Data,
    ctx: &'c Ctx,
    width: usize,
    midnight: UtcMs,
}

/// A row's look: its mark, title, tail and tone.
struct Row {
    mark: String,
    title: String,
    tail: String,
    tone: Tone,
    bold: bool,
}

impl Cx<'_> {
    /// `<title or project>: the session needs you`, blinking.
    fn waiting(&self, row: &SessionRow, out: &mut Vec<Line>) {
        let title = row.session.title.as_deref().filter(|t| !t.is_empty());
        let name = title
            .or(row.project.as_deref())
            .unwrap_or(row.handle.as_str());
        let look = Row {
            mark: state_glyph(DisplayState::Waiting, self.ctx.tick).to_string(),
            title: format!("{}: the session needs you", clean(name)),
            tail: String::new(),
            tone: Tone::Warn,
            bold: true,
        };
        let first = out.len();
        self.item(look, None, Item::Session(row.session.id), out);
        out[first].animated = true;
    }

    /// A task: its priority leads the title, P0 in red whoever's move it is.
    fn task(&self, t: &Task, out: &mut Vec<Line>) {
        let title = clean(&t.title);
        let title = match t.priority {
            Some(Priority::P0) => format!("P0 {title}"),
            Some(Priority::P1) => format!("P1 {title}"),
            _ => title,
        };
        let red = t.priority == Some(Priority::P0);
        let (mark, tail, tone, animated) = match kind(t) {
            Kind::Wait => {
                let on = t.blocked_on.as_deref().filter(|b| !b.is_empty()).map(clean);
                let other = (t.who == Some(Who::Other)).then(|| "someone else".to_string());
                let tail = on.or(other).unwrap_or_default();
                ("◷".to_string(), tail, Tone::Dim, false)
            }
            Kind::Agent => {
                let working = t
                    .session_id
                    .and_then(|id| self.data.sessions.get(&id))
                    .is_some_and(|s| s.display_state == DisplayState::Working);
                if working {
                    let mark = spinner(self.ctx.tick).to_string();
                    (mark, "agent working".to_string(), Tone::Ok, true)
                } else {
                    ("◐".to_string(), "agent".to_string(), Tone::Ok, false)
                }
            }
            Kind::You => {
                let mark = if t.today == Some(self.ctx.today) {
                    "★"
                } else {
                    "▸"
                };
                (mark.to_string(), String::new(), Tone::Text, false)
            }
        };
        let look = Row {
            mark,
            title,
            tail,
            tone: if red { Tone::P0 } else { tone },
            bold: red,
        };
        let first = out.len();
        self.item(look, t.due, Item::Task(t.id), out);
        out[first].animated = animated;
        // The due chip and the ★ follow the local day.
        if t.due.is_some() || t.today.is_some() {
            out[first].changes_at = Some(self.midnight);
        }
    }

    /// A row that wraps under its mark, only its first line selectable, its due date on the
    /// right. Too narrow for a date column, the date joins the tail, and overdue or today
    /// still stands out.
    fn item(&self, mut look: Row, due: Option<Date>, item: Item, out: &mut Vec<Line>) {
        let mut chip = due.map(|d| due_chip(d, self.ctx.today));
        if let Some(c) = &chip
            && self.width.saturating_sub(6 + text::width(&c.text)) < 16
        {
            if matches!(c.tone, Tone::Late | Tone::Today) {
                look.tone = Tone::Warn;
                look.bold = false;
            }
            let date = c.text.trim();
            look.tail = [look.tail.as_str(), date]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            chip = None;
        }
        let text = if look.tail.is_empty() {
            look.title
        } else {
            format!("{}  ({})", look.title, look.tail)
        };
        let chip_w = chip.as_ref().map_or(0, |c| text::width(&c.text) + 1);
        let wrap_w = self.width.saturating_sub(5 + chip_w).max(10);
        let span = |s: String| Span::styled(cut(&s, self.width), look.tone, look.bold);
        let mut wrapped = text::wrap(&text, wrap_w).into_iter();
        let first = wrapped.next().unwrap_or_default();
        let mut line = Line::new(vec![span(format!("  {} {first}", look.mark))]).with_item(item);
        line.chip = chip;
        out.push(line);
        for rest in wrapped {
            out.push(Line::wrapped(vec![span(format!("    {rest}"))]));
        }
    }
}
