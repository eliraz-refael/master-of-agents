//! Projects and activity, on the Lantern fixture: their models, the projects' order, counts and
//! expansion, the activity feed's order and tones, and the keys on both.

use std::path::PathBuf;

use jiff::civil::date;
use jiff::tz::TimeZone;
use moa_core::model::{ProjectId, TaskId};
use moa_core::op::Op;
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_core::time::start_of_next_day;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::fixture::{lantern, session};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Item, Line, Tone, debug_lines};
use moa_view::panel::{Mark, Out, Panel};
use moa_view::text;
use moa_view::views::{self, ViewUi};
use serde_json::json;

fn projects(data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    views::projects::build(data, ctx, ui, width)
}

fn activity(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    views::activity::build(data, ctx, &ViewUi::default(), width)
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

fn expanded(ids: &[i64]) -> ViewUi {
    ViewUi {
        expanded: ids.iter().copied().map(ProjectId).collect(),
        ..ViewUi::default()
    }
}

fn line_with<'a>(lines: &'a [Line], needle: &str) -> &'a Line {
    lines
        .iter()
        .find(|l| l.text().contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle:?}:\n{}", debug_lines(lines)))
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
    row["key"] = json!(key);
    f(&mut row);
    data.upsert(topic, vec![row]);
}

fn reshape_project(data: &mut Data, id: i64, f: impl FnOnce(&mut serde_json::Value)) {
    let row = data.projects[&ProjectId(id)].clone();
    reshape(data, Topic::Projects, &row, id.to_string(), f);
}

fn reshape_session(data: &mut Data, n: u8, f: impl FnOnce(&mut serde_json::Value)) {
    let row = data.sessions[&session(n)].clone();
    reshape(data, Topic::Sessions, &row, session(n).to_string(), f);
}

/// `n` more activity rows after the fixture's eight, a minute apart and up to now, the
/// newest last; each says its id.
fn more_activity(data: &mut Data, ctx: &Ctx, n: i64) {
    let rows = (9..9 + n)
        .map(|id| {
            json!({
                "key": id.to_string(),
                "id": id,
                "at": ctx.now.0 - (8 + n - id) * 60_000,
                "who": "brain",
                "text": format!("row {id}"),
                "ref_kind": null,
                "ref_id": null,
            })
        })
        .collect();
    data.upsert(Topic::Activity, rows);
}

// ---------- the models ----------

#[test]
fn projects_model() {
    let (data, ctx) = lantern();
    let lines = projects(&data, &ctx, &expanded(&[1]), 80);
    insta::assert_snapshot!("projects_model", debug_lines(&lines));
}

#[test]
fn activity_model() {
    let (data, ctx) = lantern();
    insta::assert_snapshot!("activity_model", debug_lines(&activity(&data, &ctx, 80)));
}

// ---------- projects ----------

#[test]
fn projects_ideas_last_then_newest_then_name() {
    let (mut data, ctx) = lantern();
    let lines = projects(&data, &ctx, &ViewUi::default(), 80);
    assert_eq!(
        texts(&lines),
        [
            "▸ Kite  active 0d  2 live · 3 tasks",
            "▸ Lantern  active 0d  2 live · 3 tasks",
            "▸ Harbor  active 3d  2 live",
            "▸ Beacon  idea",
        ]
    );
    let items: Vec<Option<Item>> = lines.iter().map(|l| l.item.clone()).collect();
    assert_eq!(
        items,
        [2, 1, 3, 4].map(|id| Some(Item::Project(ProjectId(id))))
    );

    // An idea is in the brain's colour and last, however recent; the rest are text.
    let tones: Vec<Tone> = lines.iter().map(|l| l.spans[0].tone).collect();
    assert_eq!(tones, [Tone::Text, Tone::Text, Tone::Text, Tone::Brain]);
    reshape_project(&mut data, 4, |row| {
        row["last_activity_on"] = json!(date(2026, 11, 13))
    });
    let lines = projects(&data, &ctx, &ViewUi::default(), 80);
    assert_eq!(lines[3].text(), "▸ Beacon  idea 0d");

    // A project idle over 60 days is dim; 60 days isn't over.
    reshape_project(&mut data, 3, |row| {
        row["last_activity_on"] = json!(date(2026, 9, 14))
    });
    let lines = projects(&data, &ctx, &ViewUi::default(), 80);
    let harbor = line_with(&lines, "Harbor");
    assert_eq!(harbor.text(), "▸ Harbor  active 60d  2 live");
    assert_eq!(harbor.spans[0].tone, Tone::Text);
    reshape_project(&mut data, 3, |row| {
        row["last_activity_on"] = json!(date(2026, 9, 13))
    });
    let lines = projects(&data, &ctx, &ViewUi::default(), 80);
    assert_eq!(line_with(&lines, "Harbor").spans[0].tone, Tone::Dim);
}

#[test]
fn a_projects_idle_days_change_at_midnight() {
    let (data, ctx) = lantern();
    let midnight = start_of_next_day(ctx.today, &ctx.tz);
    let lines = projects(&data, &ctx, &expanded(&[1]), 80);
    assert_eq!(line_with(&lines, "Harbor").changes_at, Some(midnight));
    // A project with no activity has no idle days to change.
    assert_eq!(line_with(&lines, "Beacon").changes_at, None);
    // A session's age changes when its next step comes.
    let fix = line_with(&lines, "Fix login");
    let seen = data.sessions[&session(1)].session.last_seen_at.unwrap();
    assert_eq!(fix.changes_at, Some(text::next_ago_change(seen, ctx.now)));
    assert!(line_with(&lines, "Rust scaffold").animated);

    // At midnight the day turns: Harbor has been idle a day longer.
    let next = Ctx::at(midnight, ctx.tz.clone(), 0, ctx.home.clone(), ctx.is_dir);
    let lines = projects(&data, &next, &ViewUi::default(), 80);
    assert_eq!(
        line_with(&lines, "Harbor").text(),
        "▸ Harbor  active 4d  2 live"
    );
}

#[test]
fn an_expanded_project_shows_its_summary_open_tasks_and_last_six_sessions() {
    let (mut data, ctx) = lantern();
    // Every other session joins Kite: nine in all, the six seen last are shown.
    for n in 5..=11 {
        reshape_session(&mut data, n, |row| {
            row["project_id"] = json!(2);
            row["project"] = json!("Kite");
        });
    }
    // Wide enough for Kite's long task.
    let lines = projects(&data, &ctx, &expanded(&[2]), 120);
    let kite: Vec<&Line> = lines
        .iter()
        .skip_while(|l| !l.text().contains("Kite"))
        .take_while(|l| !l.text().contains("Lantern"))
        .collect();
    // Six of the nine are running.
    assert_eq!(kite[0].text(), "▾ Kite  active 0d  6 live · 3 tasks");
    // Kite has no summary; its open tasks follow by id, the done one isn't listed.
    let tasks: Vec<String> = kite
        .iter()
        .filter(|l| matches!(l.item, Some(Item::Task(_))))
        .map(|l| l.text())
        .collect();
    assert_eq!(
        tasks,
        [
            "    ▸ Pick a cover  [todo]",
            "    ▸ Order copies  [todo]",
            "    ▸ Write the back-cover text for both the paperback and the e-book editions  [todo]",
        ]
    );
    let sessions: Vec<&Line> = kite
        .iter()
        .copied()
        .filter(|l| matches!(l.item, Some(Item::Session(_))))
        .collect();
    let ids: Vec<Option<Item>> = sessions.iter().map(|l| l.item.clone()).collect();
    assert_eq!(
        ids,
        [3, 4, 5, 6, 7, 8].map(|n| Some(Item::Session(session(n))))
    );
    assert_eq!(kite.len(), 1 + 3 + 6, "{kite:#?}");
    // The spike's session line, indented; text unless the session isn't running.
    assert_eq!(sessions[0].text(), "    ? Release notes [acct-a]  2h");
    assert!(sessions.iter().all(|l| l.spans[0].tone == Tone::Text));

    // An ended session is dim, and so is one asleep: seen last, they are listed first.
    for n in [9, 10] {
        reshape_session(&mut data, n, |row| {
            row["last_seen_at"] = json!(ctx.now.0 - 30_000)
        });
    }
    let lines = projects(&data, &ctx, &expanded(&[2]), 80);
    assert_eq!(line_with(&lines, "Old spike").spans[0].tone, Tone::Dim);
    assert_eq!(line_with(&lines, "Closed thing").spans[0].tone, Tone::Dim);
    assert!(
        line_with(&lines, "Closed thing")
            .text()
            .starts_with("    ○ ")
    );

    // A summary is dim, under the project's line.
    let lines = projects(&data, &ctx, &expanded(&[1]), 80);
    let summary = line_with(&lines, "A small lamp");
    assert_eq!(
        summary.text(),
        "    A small lamp for the terminal: the Rust rewrite"
    );
    assert_eq!(summary.spans[0].tone, Tone::Dim);
    assert_eq!(summary.item, None);
    // A blocked task says what it waits on.
    assert_eq!(
        line_with(&lines, "Wait for review").text(),
        "    ▸ Wait for review  [waiting] ⧗ design team"
    );
    assert_eq!(
        line_with(&lines, "Wait for review").item,
        Some(Item::Task(TaskId(3)))
    );
}

#[test]
fn no_width_or_text_breaks_a_projects_or_activity_line() {
    let (mut data, ctx) = lantern();
    reshape_project(&mut data, 1, |row| {
        row["name"] = json!("Lan\u{1b}[31mtern 👨‍👩‍👧");
        row["summary"] = json!("a\nsummary\u{7} 🇮🇱");
    });
    reshape_session(&mut data, 1, |row| {
        row["title"] = json!("a\u{1b}title\t👍🏽");
        row["last_prompt"] = json!("a\nprompt");
    });
    let mut task = data.tasks[&TaskId(3)].clone();
    task.blocked_on = Some("the\ndesign\u{1b} team".to_string());
    data.tasks.insert(task.id, task);
    let mut row = data.activity.values().next().unwrap().clone();
    row.text = "an\nactivity\u{1b}[2J row ⚠️".to_string();
    data.activity.insert(row.id, row);
    for width in [0, 1, 5, 10, 16, 21, 30, 44, 72, 80, 200] {
        for line in projects(&data, &ctx, &expanded(&[1, 2, 3, 4]), width)
            .into_iter()
            .chain(activity(&data, &ctx, width))
        {
            assert!(
                text::width(&line.text()) <= usize::from(width),
                "{width}: {line:?}"
            );
            assert!(!line.text().chars().any(char::is_control), "{line:?}");
            assert!(!line.text().contains('\u{200D}'), "{line:?}");
            assert!(!line.text().contains('\u{FE0F}'), "{line:?}");
        }
    }
}

// ---------- activity ----------

#[test]
fn activity_newest_first_coloured_by_who() {
    let (mut data, ctx) = lantern();
    let lines = activity(&data, &ctx, 80);
    assert_eq!(
        texts(&lines),
        [
            "09:59 Fix login needs you",
            "09:56 project Beacon: active → idea",
            "09:48 Release notes asks: Ship it today?",
            "09:40 suggests #1: Archive 3 ended sessions?",
            "09:27 task \"Proofread chapter 1\": active → done",
            "09:16 linked session 5e550002 → Lantern",
            "09:09 added to today: \"Fix the login loop\"",
            "09:02 new task \"Write the README\" in Lantern",
        ]
    );
    let tones: Vec<Tone> = lines.iter().map(|l| l.spans[0].tone).collect();
    assert_eq!(
        tones,
        [
            Tone::Dim,
            Tone::You,
            Tone::Dim,
            Tone::Brain,
            Tone::You,
            Tone::Dim,
            Tone::You,
            Tone::Brain,
        ]
    );
    // Nothing to select; nothing moves or changes with the clock.
    assert!(lines.iter().all(|l| !l.selectable()));
    assert!(lines.iter().all(|l| !l.animated && l.changes_at.is_none()));

    // Only the newest 300 rows.
    more_activity(&mut data, &ctx, 400);
    let lines = activity(&data, &ctx, 80);
    assert_eq!(lines.len(), 300);
    assert_eq!(lines[0].text(), "10:00 row 408");
    assert_eq!(lines[299].text(), "05:01 row 109");

    // The time is the wall time in the panel's zone.
    let (data, ctx) = lantern();
    let utc = Ctx::at(ctx.now, TimeZone::UTC, 0, ctx.home.clone(), ctx.is_dir);
    assert_eq!(
        activity(&data, &utc, 80)[0].text(),
        "14:59 Fix login needs you"
    );
}

// ---------- the panel's keys ----------

const SIZE: Size = Size { w: 80, h: 16 };

struct T {
    panel: Panel,
    data: Data,
    ctx: Ctx,
}

impl T {
    fn new(view: View) -> T {
        let (data, ctx) = lantern();
        let mut panel = Panel::new(view);
        panel.screen(&data, &ctx, SIZE);
        T { panel, data, ctx }
    }

    /// A panel of `view` with `item` selected.
    fn on(view: View, item: Item) -> T {
        let mut t = T::new(view);
        assert!(t.panel.select(&item), "{item:?} isn't listed");
        t
    }

    fn key(&mut self, k: Key) -> Vec<Out> {
        self.panel.key(k, &self.data, &self.ctx, SIZE)
    }

    fn char(&mut self, c: char) -> Vec<Out> {
        self.key(Key::Char(c))
    }

    fn body(&mut self) -> Vec<(String, Mark)> {
        let screen = self.panel.screen(&self.data, &self.ctx, SIZE);
        screen.body.iter().map(|(l, m)| (l.text(), *m)).collect()
    }

    fn texts(&mut self) -> Vec<String> {
        self.body().into_iter().map(|(t, _)| t).collect()
    }

    fn footer(&mut self) -> String {
        self.panel.screen(&self.data, &self.ctx, SIZE).footer.text
    }
}

fn send(op: Op) -> Vec<Out> {
    vec![Out::Send(op)]
}

#[test]
#[allow(non_snake_case)]
fn enter_expands_a_project_and_N_asks_which_account() {
    let lantern = Item::Project(ProjectId(1));
    let mut t = T::on(View::Projects, lantern.clone());
    assert!(t.key(Key::Enter).is_empty());
    let shown = t.texts();
    assert!(
        shown.contains(&"▾ Lantern  active 0d  2 live · 3 tasks".to_string()),
        "{shown:#?}"
    );
    assert!(shown.contains(&"    ▸ Fix the login loop  [todo]".to_string()));
    // The selection stays on the project, and Enter folds it again.
    assert_eq!(t.panel.selected(), Some(&lantern));
    assert!(t.key(Key::Enter).is_empty());
    let shown = t.texts();
    assert!(shown.contains(&"▸ Lantern  active 0d  2 live · 3 tasks".to_string()));
    assert!(!shown.iter().any(|l| l.contains("Fix the login loop")));
    // Space acts as Enter.
    assert!(t.char(' ').is_empty());
    assert!(t.texts().iter().any(|l| l.starts_with("▾ Lantern")));

    // `N` asks the account, from the brain's own list, then opens in the project's folder.
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "account (acct-a / acct-b): acct-a");
    t.key(Key::Ctrl('u'));
    for c in "acct-b".chars() {
        t.char(c);
    }
    assert_eq!(
        t.key(Key::Enter),
        send(Op::SessionOpen {
            path: PathBuf::from("/h/private/lantern"),
            account: Some("acct-b".to_string()),
            title: None,
            group: None,
            prompt: None,
            bg: false,
        })
    );

    // `n` opens at once under the default account, from a project or one of its rows.
    let open_lantern = send(Op::SessionOpen {
        path: PathBuf::from("/h/private/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: false,
    });
    assert_eq!(t.char('n'), open_lantern);
    assert!(t.panel.select(&Item::Task(TaskId(3))));
    assert_eq!(t.char('n'), open_lantern);
    // A session row in an expanded project takes `x` and `a`, as elsewhere.
    assert!(t.panel.select(&Item::Session(session(1))));
    assert!(t.char('x').is_empty());
    assert_eq!(t.footer(), "close Fix login? y/N");
}

#[test]
fn no_folder_for_a_project_without_a_path() {
    let beacon = Item::Project(ProjectId(4));
    let mut t = T::on(View::Projects, beacon.clone());
    assert!(t.char('n').is_empty());
    assert_eq!(t.footer(), "no folder for Beacon");
    assert!(t.char('N').is_empty());
    assert_eq!(t.footer(), "no folder for Beacon");
    // Expanded, it has nothing under it.
    assert!(t.key(Key::Enter).is_empty());
    let shown = t.texts();
    assert_eq!(shown.last().map(String::as_str), Some("▾ Beacon  idea"));
}

#[test]
fn keys_scroll_the_activity_list() {
    let mut t = T::new(View::Activity);
    let ctx = t.ctx.clone();
    more_activity(&mut t.data, &ctx, 30);
    let all = texts(&activity(&t.data, &ctx, 80));
    assert_eq!(all[0], "10:00 row 38");
    assert_eq!(t.texts()[0], all[0]);
    let no_bar = |body: &[(String, Mark)]| body.iter().all(|(_, m)| *m == Mark::Plain);
    assert!(no_bar(&t.body()));

    // `j` scrolls a line, `k` back.
    assert!(t.char('j').is_empty());
    let body = t.body();
    assert_eq!(body[0].0, all[1]);
    assert!(no_bar(&body));
    t.char('j');
    t.char('k');
    assert_eq!(t.texts()[0], all[1]);
    t.key(Key::Down);
    assert_eq!(t.texts()[0], all[2]);

    // `G` shows the end, `g` the top; PgDn a page down.
    t.char('G');
    assert_eq!(t.texts().last(), all.last());
    t.char('g');
    assert_eq!(t.texts()[0], all[0]);
    t.key(Key::PageDown);
    let paged = t.texts();
    assert_eq!(paged[0], all[paged.len()]);
    assert!(no_bar(&t.body()));
    assert_eq!(t.panel.selected(), None);
    assert_eq!(t.footer(), "j/k scroll");
}
