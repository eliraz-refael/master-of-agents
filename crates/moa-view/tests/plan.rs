//! The big picture, on the Lantern fixture: its model, how projects and tasks sort, the due
//! chips and wrapping, and the keys on its rows.

use std::path::PathBuf;

use jiff::civil::date;
use moa_core::UtcMs;
use moa_core::model::TaskId;
use moa_core::op::{Op, TaskSet};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, session};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Span, Tone, debug_lines};
use moa_view::panel::{Out, Panel};
use moa_view::text;
use moa_view::views::{self, ViewUi};

fn plan(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    views::plan::build(data, ctx, &ViewUi::default(), width)
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

/// The index of the line holding `needle`.
fn at(lines: &[Line], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.text().contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle:?}:\n{}", debug_lines(lines)))
}

fn line_with<'a>(lines: &'a [Line], needle: &str) -> &'a Line {
    &lines[at(lines, needle)]
}

/// Changes task `id`'s row as the daemon would publish it.
fn reshape_task(data: &mut Data, id: i64, f: impl FnOnce(&mut serde_json::Value)) {
    let mut row = serde_json::to_value(&data.tasks[&TaskId(id)]).expect("a row");
    row["key"] = serde_json::json!(id.to_string());
    f(&mut row);
    data.upsert(Topic::Tasks, vec![row]);
}

/// A local wall time in `ctx`'s zone.
fn local(ctx: &Ctx, day: jiff::civil::Date, hour: i8, minute: i8) -> UtcMs {
    let zoned = day
        .at(hour, minute, 0, 0)
        .to_zoned(ctx.tz.clone())
        .expect("in range");
    UtcMs(zoned.timestamp().as_millisecond())
}

// ---------- the model ----------

#[test]
fn plan_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("plan_model", debug_lines(&plan(&data, &ctx, 80)));
}

#[test]
fn projects_sort_needs_you_first_then_priority_then_you_tasks_then_due_then_name() {
    let (data, ctx) = lantern();
    let lines = plan(&data, &ctx, 80);
    let heads: Vec<String> = texts(&lines)
        .into_iter()
        .filter(|l| l.contains("──"))
        .collect();
    assert_eq!(heads.len(), 3, "{heads:#?}");
    // Lantern has a session waiting on you; Kite a P1 task on you; `-` neither.
    assert!(heads[0].starts_with("Lantern ──"), "{heads:#?}");
    assert!(
        heads[0].ends_with(" 2 on you · 1 with agents · 1 waiting"),
        "{heads:#?}"
    );
    assert!(heads[1].starts_with("Kite ──"), "{heads:#?}");
    assert!(heads[1].ends_with(" 2 on you · 1 waiting"), "{heads:#?}");
    assert!(heads[2].starts_with("- ──"), "{heads:#?}");
    assert!(heads[2].ends_with(" 1 waiting"), "{heads:#?}");
    // A heading is the width less two, and stands out as a group when something is on you.
    assert_eq!(text::width(&heads[0]), 78);
    let lantern_head = &line_with(&lines, "Lantern ──").spans[0];
    assert_eq!((lantern_head.tone, lantern_head.bold), (Tone::Group, true));
    assert_eq!(line_with(&lines, "- ──").spans[0].tone, Tone::Head);
    // Each project follows a blank line.
    for head in ["Lantern ──", "Kite ──", "- ──"] {
        assert_eq!(lines[at(&lines, head) - 1].text(), "", "{head}");
    }

    assert!(!heads.iter().any(|h| h.contains("Harbor")));
    let quiet = line_with(&lines, "no open tasks:");
    assert_eq!(quiet.text(), "no open tasks: Harbor");
    assert_eq!(quiet.spans[0].tone, Tone::Dim);
    assert!(
        !texts(&lines).iter().any(|l| l.contains("Beacon")),
        "an idea with no activity isn't listed"
    );
}

#[test]
fn task_rows_sort_and_mark() {
    let (data, ctx) = lantern();
    let lines = plan(&data, &ctx, 80);
    let waiting = at(&lines, "Fix login: the session needs you");
    let fix = at(&lines, "Fix the login loop");
    let readme = at(&lines, "Write the README");
    let review = at(&lines, "Wait for review");
    assert!(at(&lines, "Lantern ──") < waiting && waiting < fix && fix < readme && readme < review);

    // The waiting session blinks, `!` first.
    let w = &lines[waiting];
    assert_eq!(w.text(), "  ! Fix login: the session needs you");
    assert_eq!((w.spans[0].tone, w.spans[0].bold), (Tone::Warn, true));
    assert_eq!(w.item, Some(Item::Session(session(1))));
    assert!(w.animated);

    // On today's list, P0, red whoever's move it is.
    let f = &lines[fix];
    assert_eq!(f.text(), "  ★ P0 Fix the login loop");
    assert_eq!((f.spans[0].tone, f.spans[0].bold), (Tone::P0, true));
    assert_eq!(f.item, Some(Item::Task(TaskId(1))));

    // Its session is working: the spinner.
    let r = &lines[readme];
    assert_eq!(r.text(), "  ⠸ Write the README  (agent working)");
    assert_eq!(r.spans[0].tone, Tone::Ok);
    assert!(r.animated);

    let v = &lines[review];
    assert_eq!(v.text(), "  ◷ Wait for review  (design team)");
    assert_eq!(v.spans[0].tone, Tone::Dim);

    // Kite: a P1 on you, then one on you without a priority, then one on someone else.
    let cover = at(&lines, "Pick a cover");
    let back = at(&lines, "Write the back-cover text");
    let copies = at(&lines, "Order copies");
    assert!(cover < back && back < copies);
    assert_eq!(lines[cover].text(), "  ▸ P1 Pick a cover");
    assert_eq!(lines[cover].spans[0].tone, Tone::Text);
    assert_eq!(lines[copies].text(), "  ◷ Order copies  (someone else)");

    // An agent's task whose session isn't working is half done.
    let (mut data, ctx) = lantern();
    reshape_task(&mut data, 2, |row| {
        row["session_id"] = serde_json::Value::Null
    });
    let lines = plan(&data, &ctx, 80);
    assert_eq!(
        line_with(&lines, "Write the README").text(),
        "  ◐ Write the README  (agent)"
    );
}

#[test]
fn due_chips() {
    let (data, ctx) = lantern();
    let lines = plan(&data, &ctx, 80);
    let chip = |needle: &str| line_with(&lines, needle).chip.clone();
    assert_eq!(
        chip("Fix the login loop"),
        Some(Span::bold(" 1d overdue ", Tone::Late))
    );
    assert_eq!(
        chip("Write the README"),
        Some(Span::bold(" today ", Tone::Today))
    );
    assert_eq!(
        chip("Wait for review"),
        Some(Span::new("tomorrow", Tone::Soon))
    );
    assert_eq!(chip("Pick a cover"), Some(Span::new("Tue", Tone::Soon)));
    assert_eq!(chip("Order copies"), Some(Span::new("3 Dec", Tone::Dim)));
    assert_eq!(chip("Write the back-cover"), None);
    // The day's end changes every chip.
    let midnight = moa_core::time::start_of_next_day(ctx.today, &ctx.tz);
    assert_eq!(line_with(&lines, "Pick a cover").changes_at, Some(midnight));
}

#[test]
fn a_narrow_plan_puts_the_date_in_the_text() {
    let (data, ctx) = lantern();
    let lines = plan(&data, &ctx, 30);
    let fix = at(&lines, "Fix the login loop");
    assert_eq!(lines[fix].text(), "  ★ P0 Fix the login loop");
    assert_eq!(lines[fix + 1].text(), "    (1d overdue)");
    assert!(lines[fix + 1].more);
    for line in &lines[fix..=fix + 1] {
        assert_eq!(line.spans[0].tone, Tone::Warn, "{line:?}");
        assert_eq!(line.chip, None);
    }
    // Today stands out too; a later date keeps its column.
    let readme = at(&lines, "Write the README");
    assert_eq!(lines[readme].chip, Some(Span::bold(" today ", Tone::Today)));
    assert!(line_with(&lines, "Pick a cover").chip.is_some());
}

#[test]
fn long_titles_wrap_under_their_mark() {
    let (data, ctx) = lantern();
    let lines = plan(&data, &ctx, 40);
    let first = at(&lines, "▸ Write the back-cover");
    assert!(lines[first].selectable());
    let more: Vec<&Line> = lines[first + 1..].iter().take_while(|l| l.more).collect();
    assert!(more.len() >= 2, "{}", debug_lines(&lines));
    for line in &more {
        assert!(line.text().starts_with("    "), "{line:?}");
        assert!(!line.text().starts_with("     "), "{line:?}");
        assert_eq!(line.item, None);
    }
    let whole: Vec<String> = std::iter::once(&lines[first])
        .chain(more.iter().copied())
        .map(|l| l.text().chars().skip(4).collect::<String>())
        .collect();
    assert_eq!(
        whole.join(" "),
        "Write the back-cover text for both the paperback and the e-book editions"
    );
    // A line with a chip wraps short of it.
    for width in [10u16, 30, 40, 80, 200] {
        for line in plan(&data, &ctx, width) {
            let chip = line.chip.as_ref().map_or(0, |c| text::width(&c.text) + 1);
            assert!(
                text::width(&line.text()) + chip <= usize::from(width),
                "{width}: {line:?}"
            );
        }
    }
}

#[test]
fn done_today_counts_by_local_day() {
    let (mut data, ctx) = lantern();
    assert_eq!(
        plan(&data, &ctx, 80)[0].text(),
        "Fri 13 Nov · ✓ 1 done today"
    );
    let head = &plan(&data, &ctx, 80)[0];
    assert_eq!((head.spans[0].tone, head.spans[0].bold), (Tone::Head, true));
    assert_eq!(
        head.changes_at,
        Some(moa_core::time::start_of_next_day(ctx.today, &ctx.tz))
    );

    // 23:30 yesterday in New York is already today in UTC.
    let late = local(&ctx, date(2026, 11, 12), 23, 30);
    reshape_task(&mut data, 7, |row| row["done_at"] = serde_json::json!(late));
    assert_eq!(plan(&data, &ctx, 80)[0].text(), "Fri 13 Nov");

    let early = local(&ctx, date(2026, 11, 13), 0, 10);
    reshape_task(&mut data, 7, |row| {
        row["done_at"] = serde_json::json!(early)
    });
    assert_eq!(
        plan(&data, &ctx, 80)[0].text(),
        "Fri 13 Nov · ✓ 1 done today"
    );

    // Done, not dropped.
    reshape_task(&mut data, 7, |row| {
        row["status"] = serde_json::json!("dropped")
    });
    assert_eq!(plan(&data, &ctx, 80)[0].text(), "Fri 13 Nov");
}

#[test]
fn an_empty_plan_says_so() {
    let (_, ctx) = lantern();
    let lines = plan(&Data::default(), &ctx, 80);
    assert_eq!(
        texts(&lines),
        vec![
            "Fri 13 Nov",
            "  nothing tracked yet: moa fills this in as sessions report",
            "",
            "↵ or click: jump to its session · d done · x drop",
        ]
    );
    assert_eq!(lines[1].spans[0].tone, Tone::Dim);
    assert_eq!(lines[3].spans[0].tone, Tone::Dim);
    assert!(lines.iter().all(|l| l.item.is_none()));
}

#[test]
fn no_width_or_title_breaks_a_plan_line() {
    let (mut data, ctx) = lantern();
    reshape_task(&mut data, 1, |row| {
        row["title"] = serde_json::json!("a\u{1b}[31mred\ttitle 👨‍👩‍👧");
    });
    reshape_task(&mut data, 3, |row| {
        row["blocked_on"] = serde_json::json!("the\nteam");
    });
    for width in [0, 1, 5, 10, 16, 21, 22, 30, 80, 200] {
        for line in plan(&data, &ctx, width) {
            assert!(!line.text().chars().any(char::is_control), "{line:?}");
            assert!(!line.text().contains('\u{200D}'), "{line:?}");
        }
    }
}

// ---------- the panel's keys ----------

const SIZE: Size = Size { w: 80, h: 30 };

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    /// A plan panel with `item` selected.
    fn on(item: Item) -> T {
        let (data, ctx) = lantern();
        let mut panel = Panel::new(View::Plan);
        panel.screen(&data, &ctx, SIZE);
        assert!(panel.select(&item), "{item:?} isn't listed");
        T { panel, data, ctx }
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, SIZE)
    }

    fn char(&mut self, c: char) -> Vec<Out> {
        self.key(Key::Char(c))
    }

    fn answer(&mut self, text: &str) -> Vec<Out> {
        let mut out = self.key(Key::Ctrl('u'));
        for c in text.chars() {
            out.extend(self.char(c));
        }
        out.extend(self.key(Key::Enter));
        out
    }

    fn footer(&mut self) -> String {
        self.panel.screen(&self.data, &self.ctx, SIZE).footer.text
    }
}

fn task(id: i64) -> Item {
    Item::Task(TaskId(id))
}

fn send(op: Op) -> Vec<Out> {
    vec![Out::Send(op)]
}

fn set_status(id: i64, status: &str) -> Vec<Out> {
    send(Op::TaskSet(TaskSet {
        id,
        status: Some(status.to_string()),
        ..TaskSet::default()
    }))
}

#[test]
fn d_and_x_set_the_tasks_status() {
    let mut t = T::on(task(1));
    assert_eq!(t.char('d'), set_status(1, "done"));
    assert_eq!(t.char('x'), set_status(1, "dropped"));
    // Not on a session row.
    let mut t = T::on(Item::Session(session(1)));
    assert!(t.char('d').is_empty());
}

#[test]
fn enter_jumps_to_the_tasks_session_or_says_none() {
    let mut t = T::on(task(2));
    assert_eq!(
        t.key(Key::Enter),
        send(Op::SessionFocus {
            q: session(2).to_string()
        })
    );
    let mut t = T::on(task(1));
    assert!(t.key(Key::Enter).is_empty());
    assert_eq!(t.footer(), "no session linked to this task");
    // A waiting session's row jumps to it.
    let mut t = T::on(Item::Session(session(1)));
    assert_eq!(
        t.key(Key::Enter),
        send(Op::SessionFocus {
            q: session(1).to_string()
        })
    );
}

#[test]
fn session_row_keys_in_the_plan() {
    let mut t = T::on(Item::Session(session(1)));
    assert_eq!(
        t.char('a'),
        send(Op::SessionArchive {
            qs: vec![session(1).to_string()]
        })
    );
    // In the big picture `x` only drops tasks.
    assert!(t.char('x').is_empty());
    assert_eq!(t.footer(), moa_view::keys::hint(View::Plan, false));

    let open = |account: Option<&str>| {
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/kite"),
            account: account.map(str::to_string),
            title: None,
            group: None,
            prompt: None,
            bg: false,
        })
    };
    let mut t = T::on(task(4));
    assert_eq!(t.char('n'), open(None));

    let mut t = T::on(task(4));
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "account (acct-a / acct-b): acct-a");
    assert_eq!(t.answer("acct-b"), open(Some("acct-b")));

    // A session row opens in its project's folder too.
    let mut t = T::on(Item::Session(session(1)));
    let Out::Send(Op::SessionOpen { path, .. }) = &t.char('n')[0] else {
        panic!("no open");
    };
    assert_eq!(path, &PathBuf::from("/h/private/lantern"));

    // The task with no project, under `-`.
    let mut t = T::on(task(8));
    assert!(t.char('n').is_empty());
    assert_eq!(t.footer(), "this row has no project");
    let mut t = T::on(task(8));
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "this row has no project");

    // V7's folder check holds.
    let mut t = T::on(task(4));
    t.ctx.is_dir = |_| false;
    assert!(t.char('n').is_empty());
    assert_eq!(t.footer(), "no folder ~/private/kite");
}
