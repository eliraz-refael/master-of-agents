//! The status panel and today, on the Lantern fixture: their models, the status panel's groups
//! and columns, today's sections, and the keys on their rows.

use std::path::PathBuf;

use moa_core::model::{SuggestionId, TaskId};
use moa_core::op::{Answer, Op};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, session};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Tone, debug_lines};
use moa_view::panel::{Out, Panel};
use moa_view::text;
use moa_view::views::{self, ViewUi};

fn status(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    views::status::build(data, ctx, &ViewUi::default(), width)
}

fn today(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    views::today::build(data, ctx, &ViewUi::default(), width)
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

/// Changes a row of `topic` as the daemon would publish it.
fn reshape<T: serde::Serialize>(
    data: &mut Data,
    topic: Topic,
    row: &T,
    key: String,
    f: impl FnOnce(&mut serde_json::Value),
) {
    let mut row = serde_json::to_value(row).expect("a row");
    row["key"] = serde_json::json!(key);
    f(&mut row);
    data.upsert(topic, vec![row]);
}

fn reshape_session(data: &mut Data, n: u8, f: impl FnOnce(&mut serde_json::Value)) {
    let row = data.sessions[&session(n)].clone();
    reshape(data, Topic::Sessions, &row, session(n).to_string(), f);
}

fn reshape_task(data: &mut Data, id: i64, f: impl FnOnce(&mut serde_json::Value)) {
    let row = data.tasks[&TaskId(id)].clone();
    reshape(data, Topic::Tasks, &row, id.to_string(), f);
}

// ---------- the models ----------

#[test]
fn status_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("status_model", debug_lines(&status(&data, &ctx, 80)));
}

#[test]
fn today_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("today_model", debug_lines(&today(&data, &ctx, 80)));
}

// ---------- status ----------

#[test]
fn status_groups_by_urgency_with_failed_and_quiet() {
    let (data, ctx) = lantern();
    let lines = status(&data, &ctx, 80);
    let heads: Vec<String> = texts(&lines)
        .into_iter()
        .filter(|l| l.contains("──"))
        .collect();
    let words: Vec<&str> = heads
        .iter()
        .map(|h| h.split(" ──").next().unwrap_or_default())
        .collect();
    assert_eq!(
        words,
        [
            "needs you",
            "asks you",
            "failed",
            "done",
            "working",
            "quiet",
            "idle"
        ]
    );
    // `<word> ──── <n>`, a cell short of the width.
    assert!(heads[0].ends_with("── 1"), "{heads:#?}");
    assert!(heads[6].ends_with("── 2"), "{heads:#?}");
    for head in &heads {
        assert_eq!(text::width(head), 79, "{head}");
    }

    // Each heading in its group's tone; idle's is a heading's.
    let tone = |needle: &str| {
        let span = &line_with(&lines, needle).spans[0];
        (span.tone, span.bold)
    };
    assert_eq!(tone("needs you ──"), (Tone::Warn, true));
    assert_eq!(tone("asks you ──"), (Tone::Ask, true));
    assert_eq!(tone("failed ──"), (Tone::Fail, true));
    assert_eq!(tone("done ──"), (Tone::Ok, true));
    assert_eq!(tone("working ──"), (Tone::Work, false));
    assert_eq!(tone("quiet ──"), (Tone::Quiet, false));
    assert_eq!(tone("idle ──"), (Tone::Head, true));

    // Failed is ✗ in red, quiet ◌ in cyan; idle rows are dim.
    assert!(line_with(&lines, "Migrate DB").text().starts_with("✗ "));
    assert_eq!(tone("Migrate DB"), (Tone::Fail, true));
    assert!(
        line_with(&lines, "Import the whole")
            .text()
            .starts_with("◌ ")
    );
    assert_eq!(tone("Import the whole"), (Tone::Quiet, false));
    assert_eq!(tone("Dev server"), (Tone::Dim, false));

    // Each group's sessions follow its heading, the most recent activity first.
    let order = [
        "needs you ──",
        "Fix login",
        "asks you ──",
        "Release notes",
        "failed ──",
        "Migrate DB",
        "done ──",
        "設計レビュー",
        "working ──",
        "Rust scaffold",
        "quiet ──",
        "Import the whole",
        "idle ──",
        "Dev server",
        "Hand started",
    ];
    let found: Vec<usize> = order.iter().map(|n| at(&lines, n)).collect();
    assert_eq!(found, (0..order.len()).collect::<Vec<_>>(), "{order:?}");
    assert_eq!(lines.len(), order.len(), "only live sessions");

    // A session's line stands for it; the spinner and the blink tick.
    let fix = line_with(&lines, "Fix login");
    assert_eq!(fix.item, Some(Item::Session(session(1))));
    assert!(fix.animated);
    assert!(line_with(&lines, "Rust scaffold").animated);
    assert!(!line_with(&lines, "Migrate DB").animated);
    assert!(lines[0].item.is_none());
}

#[test]
fn status_columns_by_width() {
    let (data, ctx) = lantern();
    let fix = |w: u16| line_with(&status(&data, &ctx, w), "Fix login").text();

    // At 80: the title in 26, the group's last part in 14, the age in 4, then the why.
    assert_eq!(
        fix(80),
        format!(
            "! {}{}  1m  Bash: cargo test ⚠",
            text::fit("Fix login", 27),
            text::fit("Lantern", 14)
        )
    );
    // At 60: the group but no why; the title takes the rest.
    let at_60 = fix(60);
    assert_eq!(
        at_60,
        format!(
            "! {}{}  1m",
            text::fit("Fix login", 39),
            text::fit("Lantern", 14)
        )
    );
    assert!(!at_60.contains("Bash"));
    // At 40: neither.
    let at_40 = fix(40);
    assert_eq!(at_40, format!("! {}  1m", text::fit("Fix login", 33)));
    assert!(!at_40.contains("Lantern"));

    // The age is since the session's last activity, not since it was seen.
    let scaffold = line_with(&status(&data, &ctx, 80), "Rust scaffold").text();
    assert!(scaffold.ends_with("  2m"), "{scaffold:?}");
    // A session with no group shows its project's name.
    let mut data = data;
    reshape_session(&mut data, 1, |row| {
        row["group_path"] = serde_json::Value::Null
    });
    let line = line_with(&status(&data, &ctx, 60), "Fix login").text();
    assert!(line.contains(&text::fit("Lantern", 14)), "{line:?}");
}

#[test]
fn no_width_or_text_breaks_a_status_or_today_line() {
    let (mut data, ctx) = lantern();
    reshape_session(&mut data, 1, |row| {
        row["title"] = serde_json::json!("a\u{1b}[31mred\ttitle 👨‍👩‍👧");
        row["why"] = serde_json::json!("the\nwhy\u{7}");
        row["last_prompt"] = serde_json::json!("a\nprompt\u{1b}");
    });
    reshape_task(&mut data, 1, |row| {
        row["title"] = serde_json::json!("the\ntask\u{1b}");
    });
    for width in [0, 1, 5, 10, 16, 21, 22, 30, 43, 44, 71, 72, 80, 200] {
        for line in status(&data, &ctx, width)
            .into_iter()
            .chain(today(&data, &ctx, width))
        {
            assert!(
                text::width(&line.text()) <= usize::from(width),
                "{width}: {line:?}"
            );
            assert!(!line.text().chars().any(char::is_control), "{line:?}");
            assert!(!line.text().contains('\u{200D}'), "{line:?}");
        }
    }
}

#[test]
fn an_empty_status_says_how_to_find_one() {
    let (_, ctx) = lantern();
    let lines = status(&Data::default(), &ctx, 80);
    assert_eq!(
        texts(&lines),
        ["no live sessions: Ctrl-a / finds one to resume"]
    );
    assert_eq!(lines[0].spans[0].tone, Tone::Dim);
    assert!(lines[0].item.is_none());

    // Asleep, ended and archived sessions aren't live.
    let (mut data, ctx) = lantern();
    data.sessions
        .retain(|id, _| [session(9), session(10), session(11)].contains(id));
    assert_eq!(
        texts(&status(&data, &ctx, 80)),
        ["no live sessions: Ctrl-a / finds one to resume"]
    );
}

// ---------- today ----------

#[test]
fn today_lists_tasks_sessions_and_suggestions() {
    let (mut data, ctx) = lantern();
    // A second task for today, done: it comes last, dimmed.
    reshape_task(&mut data, 7, |row| {
        row["today"] = serde_json::json!(ctx.today)
    });
    let lines = today(&data, &ctx, 80);

    let head = &lines[0];
    assert_eq!(head.text(), "TODAY · Fri 13 Nov");
    assert_eq!((head.spans[0].tone, head.spans[0].bold), (Tone::Head, true));
    assert!(head.changes_at.is_some(), "it changes at midnight");

    assert_eq!(lines[1].text(), "▸ Lantern: Fix the login loop  [todo]");
    assert_eq!(lines[1].spans[0].tone, Tone::Text);
    assert_eq!(lines[1].item, Some(Item::Task(TaskId(1))));
    assert_eq!(lines[2].text(), "✓ Kite: Proofread chapter 1  [done]");
    assert_eq!(lines[2].spans[0].tone, Tone::Dim);

    // Live sessions waiting on you, seen in the last day, the newest first.
    let waiting = at(&lines, "WAITING ON YOU");
    assert_eq!(lines[waiting - 1].text(), "");
    let order = [
        "Fix login",
        "Release notes",
        "設計レビュー",
        "Dev server",
        "Migrate DB",
        "Hand started",
    ];
    for (i, name) in order.iter().enumerate() {
        assert_eq!(at(&lines, name), waiting + 1 + i, "{name}");
    }
    let fix = &lines[waiting + 1];
    assert_eq!(fix.text(), "! Fix login [acct-a]  1m");
    assert_eq!(fix.spans[0].tone, Tone::Warn);
    assert_eq!(fix.item, Some(Item::Session(session(1))));
    assert!(fix.animated);
    let migrate = line_with(&lines, "Migrate DB");
    assert_eq!(migrate.text(), "✗ Migrate DB [acct-b]  5h");
    assert_eq!(migrate.spans[0].tone, Tone::Text);

    let working = at(&lines, "WORKING");
    let scaffold = &lines[working + 1];
    assert_eq!(
        scaffold.text(),
        "⠸ Rust scaffold [acct-a]  1h  split the store"
    );
    assert_eq!(scaffold.spans[0].tone, Tone::Ok);

    let suggestions = at(&lines, "SUGGESTIONS  (y accept · n reject)");
    assert!(working < suggestions);
    assert_eq!(
        texts(&lines[suggestions + 1..]),
        ["? #1 Archive 3 ended sessions?", "? #2 Set Kite's due date"]
    );
    let first = &lines[suggestions + 1];
    assert_eq!(first.spans[0].tone, Tone::Brain);
    assert_eq!(first.item, Some(Item::Suggestion(SuggestionId(1))));
    for heading in ["WAITING ON YOU", "WORKING", "SUGGESTIONS"] {
        let span = &line_with(&lines, heading).spans[0];
        assert_eq!((span.tone, span.bold), (Tone::Head, true), "{heading}");
    }
}

#[test]
fn a_quiet_session_is_still_working_in_today() {
    // Quiet is a working turn gone silent: the agent isn't waiting on you.
    let (mut data, ctx) = lantern();
    let lines = today(&data, &ctx, 80);
    let (waiting, working) = (at(&lines, "WAITING ON YOU"), at(&lines, "WORKING"));
    let quiet = at(&lines, "Import the whole");
    assert!(working < quiet, "{}", debug_lines(&lines));
    assert!(quiet < at(&lines, "SUGGESTIONS"));
    assert!(
        !texts(&lines[waiting..working])
            .iter()
            .any(|l| l.contains("Import the whole"))
    );
    // Newest seen first, after the working session; dim outside the status panel.
    assert_eq!(quiet, working + 2);
    assert_eq!(
        lines[quiet].text(),
        "◌ Import the whole archive of old notes into the new format [acct-a]  6h"
    );
    assert_eq!(lines[quiet].spans[0].tone, Tone::Dim);

    // A quiet session alone still shows the section.
    data.sessions.remove(&session(2));
    let lines = today(&data, &ctx, 80);
    assert_eq!(at(&lines, "Import the whole"), at(&lines, "WORKING") + 1);
}

#[test]
fn an_empty_today_says_so_in_each_section() {
    let (mut data, ctx) = lantern();
    data.tasks.clear();
    data.suggestions.clear();
    // Seen over a day ago: not waiting on you.
    let day_ago = moa_core::UtcMs(ctx.now.0 - 25 * 3_600_000);
    // Nothing working or quiet, so no WORKING section.
    data.sessions
        .retain(|id, _| ![session(2), session(7)].contains(id));
    for row in data.sessions.values_mut() {
        row.session.last_seen_at = Some(day_ago);
    }
    // An answered suggestion isn't listed.
    let (full, _) = lantern();
    let mut answered = full.suggestions[&SuggestionId(1)].clone();
    answered.status = moa_core::model::SuggestionStatus::Accepted;
    data.suggestions.insert(answered.id, answered);

    assert_eq!(
        texts(&today(&data, &ctx, 80)),
        [
            "TODAY · Fri 13 Nov",
            "  (nothing yet: set today's goals with moa)",
            "",
            "WAITING ON YOU",
            "  (nothing)",
            "",
            "SUGGESTIONS  (y accept · n reject)",
            "  (none)",
        ]
    );
}

// ---------- the panel's keys ----------

const SIZE: Size = Size { w: 80, h: 30 };

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    /// A panel of `view` with `item` selected.
    fn on(view: View, item: Item) -> T {
        let (data, ctx) = lantern();
        let mut panel = Panel::new(view);
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

    fn footer(&mut self) -> String {
        self.panel.screen(&self.data, &self.ctx, SIZE).footer.text
    }
}

fn send(op: Op) -> Vec<Out> {
    vec![Out::Send(op)]
}

fn answer(id: i64, answer: Answer) -> Vec<Out> {
    send(Op::SuggestionAnswer {
        id: Some(id),
        answer,
    })
}

#[test]
fn y_and_n_answer_a_suggestion() {
    let suggestion = |id| Item::Suggestion(SuggestionId(id));
    let mut t = T::on(View::Today, suggestion(1));
    assert_eq!(t.char('y'), answer(1, Answer::Yes));
    let mut t = T::on(View::Today, suggestion(2));
    assert_eq!(t.char('n'), answer(2, Answer::No));
    // Enter on a suggestion does nothing.
    assert!(t.key(Key::Enter).is_empty());
    // `y` answers only a suggestion.
    let mut t = T::on(View::Today, Item::Session(session(1)));
    assert!(t.char('y').is_empty());
    let mut t = T::on(View::Today, Item::Task(TaskId(1)));
    assert!(t.char('y').is_empty());
}

#[test]
fn enter_jumps_from_status_and_today() {
    let focus = |n| {
        send(Op::SessionFocus {
            q: session(n).to_string(),
        })
    };
    let mut t = T::on(View::Status, Item::Session(session(6)));
    assert_eq!(t.key(Key::Enter), focus(6));
    let mut t = T::on(View::Today, Item::Session(session(2)));
    assert_eq!(t.key(Key::Enter), focus(2));
    let mut t = T::on(View::Today, Item::Task(TaskId(1)));
    assert!(t.key(Key::Enter).is_empty());
    assert_eq!(t.footer(), "no session linked to this task");
}

#[test]
fn session_row_keys_in_status_and_today() {
    let mut t = T::on(View::Status, Item::Session(session(1)));
    assert!(t.char('x').is_empty());
    assert_eq!(t.footer(), "close Fix login? y/N");
    assert_eq!(
        t.char('y'),
        send(Op::SessionClose {
            q: session(1).to_string()
        })
    );
    let mut t = T::on(View::Status, Item::Session(session(3)));
    assert_eq!(
        t.char('a'),
        send(Op::SessionArchive {
            qs: vec![session(3).to_string()]
        })
    );

    // `n` on a session row in today opens a session in its project's folder.
    let mut t = T::on(View::Today, Item::Session(session(3)));
    assert_eq!(
        t.char('n'),
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/kite"),
            account: None,
            title: None,
            group: None,
            prompt: None,
            bg: false,
        })
    );
    // A session with no project says so.
    let mut t = T::on(View::Today, Item::Session(session(7)));
    assert!(t.char('n').is_empty());
    assert_eq!(t.footer(), "this row has no project");
    // `x` in today asks too.
    let mut t = T::on(View::Today, Item::Session(session(1)));
    assert!(t.char('x').is_empty());
    assert_eq!(t.footer(), "close Fix login? y/N");
}
