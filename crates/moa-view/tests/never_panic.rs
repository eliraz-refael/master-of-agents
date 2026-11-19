//! No view panics for any data, size or input, and nothing a frame shows holds a control
//! character, a variation selector or a joiner (V9, V18).

use std::path::{Path, PathBuf};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::live::DisplayState;
use moa_core::model::{
    Activity, ActivityId, ActivityRow, ActivityWho, AgentKind, Group, Lifecycle, Location,
    Priority, Project, ProjectId, ProjectStatus, Session, SessionId, Suggestion, SuggestionId,
    SuggestionStatus, Task, TaskId, TaskStatus, Who,
};
use moa_core::panel::View;
use moa_core::publish::BrainRow;
use moa_view::ctx::Ctx;
use moa_view::data::{Data, SessionRow};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::line::{Line, Span};
use moa_view::panel::{Mouse, Panel, Screen};
use moa_view::text::width;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

/// Thu 19 Nov 2026, 10:00 UTC.
const NOW: i64 = 1_795_082_400_000;
const DAY: i64 = 86_400_000;

fn s(text: &str) -> String {
    text.to_string()
}

/// A piece of text that has broken terminals or measured wrong.
fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9 /<~.-]{0,8}",
        1 => select(vec![
            s("\u{1b}[31m"),
            s("\u{1b}"),
            s("\n"),
            s("\r\n"),
            s("\t"),
            s("\u{7f}"),
            s("\u{9b}"),
            s("\u{0}"),
            s("設計レビュー"),
            s("👨‍👩‍👧"),
            s("👍🏽"),
            s("⚠️"),
            s("🇮🇱"),
            s("🇮"),
            s("\u{200d}"),
            s("\u{fe0f}"),
            s("\u{fe0e}"),
            s("\u{e0041}"),
            s("🏴\u{e0067}\u{e0062}\u{e007f}"),
            s("\u{202e}"),
            s("\u{2028}"),
            s("e\u{301}"),
            s("\u{301}"),
            s("💤"),
            s("claude-"),
            s(""),
        ]),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => vec(piece(), 0..6).prop_map(|parts| parts.concat()),
        1 => any::<String>(),
    ]
}

/// A moment: mostly within a year of now, sometimes at the ends of time.
fn at() -> impl Strategy<Value = UtcMs> {
    prop_oneof![
        8 => (NOW - 400 * DAY..NOW + 2 * DAY).prop_map(UtcMs),
        1 => select(vec![i64::MIN, i64::MAX, 0, -1]).prop_map(UtcMs),
        1 => any::<i64>().prop_map(UtcMs),
    ]
}

fn date() -> impl Strategy<Value = Date> {
    let today = jiff::civil::date(2026, 11, 19);
    prop_oneof![
        8 => (-400i64..400).prop_map(move |d| today
            .checked_add(jiff::Span::new().days(d))
            .expect("in range")),
        1 => select(vec![Date::MIN, Date::MAX]),
    ]
}

fn session_id(n: u8) -> SessionId {
    SessionId(uuid::Uuid::from_u128(
        0x5e55_0000_0000_4000_8000_0000_0000_0000 | u128::from(n),
    ))
}

fn folder() -> impl Strategy<Value = String> {
    prop_oneof![
        select(vec![
            s("/h"),
            s("/h/private/lantern"),
            s("/a/b/c/d/e"),
            s("~/private"),
            s("/"),
        ]),
        text(),
    ]
}

fn group_path() -> impl Strategy<Value = String> {
    prop_oneof![
        select(vec![
            s("Private"),
            s("Private/Lantern"),
            s("Work/Harbor/Deep/Er"),
            s("A//B/"),
            s("/"),
            s("Loose"),
        ]),
        text(),
    ]
}

fn session() -> impl Strategy<Value = SessionRow> {
    let who = (
        option::of(text()),
        option::of(folder()),
        option::of((1i64..6).prop_map(ProjectId)),
        option::of(text()),
        option::of(group_path()),
        select(vec![Lifecycle::Live, Lifecycle::Ended]),
        select(vec![
            Activity::Working,
            Activity::Idle,
            Activity::NeedsPermission,
            Activity::NeedsAnswer,
            Activity::Failed,
        ]),
        option::of(text()),
        option::of(text()),
    );
    let when = (
        option::of(at()),
        option::of(at()),
        option::of(at()),
        option::of(at()),
        at(),
        option::of(at()),
        select(vec![Location::Here, Location::Elsewhere]),
    );
    let row = (
        text(),
        option::of(text()),
        select(DisplayState::ALL.to_vec()),
        text(),
        any::<bool>(),
    );
    (who, when, row).prop_map(|(who, when, row)| {
        let (account, cwd, project_id, title, group_path, lifecycle, activity, why, prompt) = who;
        let (activity_at, last_turn_end_at, seen_at, asleep_at, created_at, last_seen_at, location) =
            when;
        let (handle, project, display_state, row_why, adopted) = row;
        SessionRow {
            session: Session {
                id: session_id(0),
                agent_kind: AgentKind::Claude,
                account,
                start_cwd: cwd.clone(),
                cwd,
                project_id,
                title,
                title_locked: false,
                group_path,
                lifecycle,
                activity,
                activity_at,
                activity_why: why,
                last_prompt: prompt,
                last_turn_end_at,
                seen_at,
                asleep_at,
                archived_at: None,
                location,
                resume_failed: false,
                created_at,
                last_seen_at,
            },
            handle,
            project,
            display_state,
            why: row_why,
            adopted,
        }
    })
}

fn task() -> impl Strategy<Value = Task> {
    let what = (
        option::of((1i64..6).prop_map(ProjectId)),
        text(),
        select(vec![
            TaskStatus::Idea,
            TaskStatus::Todo,
            TaskStatus::Active,
            TaskStatus::Waiting,
            TaskStatus::Blocked,
            TaskStatus::Done,
            TaskStatus::Dropped,
        ]),
        option::of(text()),
        option::of(text()),
        option::of(select(vec![Who::You, Who::Agent, Who::Other])),
        option::of(select(vec![Priority::P0, Priority::P1, Priority::P2])),
        option::of((0u8..14).prop_map(session_id)),
    );
    let when = (
        option::of(date()),
        option::of(date()),
        at(),
        option::of(at()),
    );
    (what, when).prop_map(|(what, when)| {
        let (project_id, title, status, blocked_on, note, who, priority, session_id) = what;
        let (today, due, created_at, done_at) = when;
        Task {
            id: TaskId(0),
            project_id,
            title,
            status,
            blocked_on,
            today,
            session_id,
            note,
            who,
            due,
            priority,
            created_at,
            updated_at: created_at,
            done_at,
        }
    })
}

fn project() -> impl Strategy<Value = Project> {
    (
        text(),
        option::of(folder()),
        select(vec![
            ProjectStatus::Idea,
            ProjectStatus::Active,
            ProjectStatus::Paused,
            ProjectStatus::Done,
        ]),
        option::of(text()),
        option::of(date()),
        at(),
    )
        .prop_map(
            |(name, path, status, summary, last_activity_on, created_at)| Project {
                id: ProjectId(0),
                name,
                path,
                status,
                summary,
                last_activity_on,
                created_at,
                updated_at: created_at,
            },
        )
}

fn suggestion() -> impl Strategy<Value = Suggestion> {
    (
        text(),
        select(vec![
            SuggestionStatus::Open,
            SuggestionStatus::Accepted,
            SuggestionStatus::Rejected,
            SuggestionStatus::Later,
        ]),
        option::of(text()),
        at(),
    )
        .prop_map(|(text, status, result, created_at)| Suggestion {
            id: SuggestionId(0),
            text,
            status,
            actions: Vec::new(),
            actions_raw: None,
            actions_invalid: None,
            result,
            created_at,
            resolved_at: None,
        })
}

fn activity_row() -> impl Strategy<Value = ActivityRow> {
    (
        at(),
        select(vec![
            ActivityWho::Brain,
            ActivityWho::Session,
            ActivityWho::You,
        ]),
        text(),
    )
        .prop_map(|(at, who, text)| ActivityRow {
            id: ActivityId(0),
            at,
            who,
            text,
            ref_kind: None,
            ref_id: None,
        })
}

fn brain() -> impl Strategy<Value = BrainRow> {
    (
        text(),
        vec(folder(), 0..3),
        vec(text(), 0..3),
        option::of(text()),
        vec(group_path(), 0..3),
    )
        .prop_map(|(name, folders, accounts, open_account, folded)| BrainRow {
            name,
            folders,
            accounts,
            open_account,
            folded,
        })
}

fn data() -> impl Strategy<Value = Data> {
    (
        vec(session(), 0..14),
        vec(group_path(), 0..4),
        vec(project(), 0..6),
        vec(task(), 0..10),
        vec(suggestion(), 0..4),
        vec(activity_row(), 0..12),
        option::of(brain()),
    )
        .prop_map(
            |(sessions, groups, projects, tasks, suggestions, activity, brain)| {
                let mut data = Data::default();
                for (n, mut row) in (0u8..).zip(sessions) {
                    row.session.id = session_id(n);
                    data.sessions.insert(row.session.id, row);
                }
                for path in groups {
                    let group = Group {
                        path: path.clone(),
                        collapsed: false,
                        created_at: UtcMs(NOW),
                    };
                    data.groups.insert(path, group);
                }
                for (n, mut p) in (1i64..).zip(projects) {
                    p.id = ProjectId(n);
                    data.projects.insert(p.id, p);
                }
                for (n, mut t) in (1i64..).zip(tasks) {
                    t.id = TaskId(n);
                    data.tasks.insert(t.id, t);
                }
                for (n, mut sg) in (1i64..).zip(suggestions) {
                    sg.id = SuggestionId(n);
                    data.suggestions.insert(sg.id, sg);
                }
                for (n, mut a) in (1i64..).zip(activity) {
                    a.id = ActivityId(n);
                    data.activity.insert(a.id, a);
                }
                data.brain = brain;
                data
            },
        )
}

fn yes(_: &Path) -> bool {
    true
}

fn no(_: &Path) -> bool {
    false
}

fn ctx() -> impl Strategy<Value = Ctx> {
    (
        NOW - 400 * DAY..NOW + 400 * DAY,
        any::<bool>(),
        any::<u64>(),
        any::<bool>(),
    )
        .prop_map(|(now, utc, tick, dirs)| {
            let tz = if utc {
                TimeZone::UTC
            } else {
                TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").expect("a zone")
            };
            let is_dir: fn(&Path) -> bool = if dirs { yes } else { no };
            Ctx::at(UtcMs(now), tz, tick, PathBuf::from("/h"), is_dir)
        })
}

#[derive(Clone, Debug)]
enum Input {
    Key(Key),
    Mouse(Mouse),
    Blur,
    Paste(String),
    Reply(String),
}

fn key() -> impl Strategy<Value = Key> {
    prop_oneof![
        4 => select(vec![
            Key::Enter,
            Key::Esc,
            Key::Backspace,
            Key::Tab,
            Key::BackTab,
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Ctrl('u'),
            Key::Ctrl('w'),
        ]),
        6 => select("jkgGnN+mrDaxRdyq123456hl /~ ".chars().collect::<Vec<_>>()).prop_map(Key::Char),
        1 => any::<char>().prop_map(Key::Char),
        1 => any::<char>().prop_map(Key::Ctrl),
    ]
}

fn input() -> impl Strategy<Value = Input> {
    let cell = (0u16..260, 0u16..70);
    prop_oneof![
        10 => key().prop_map(Input::Key),
        2 => cell.prop_map(|(x, y)| Input::Mouse(Mouse::Click { x, y })),
        2 => (0u16..260, 0u16..70).prop_map(|(x, y)| Input::Mouse(Mouse::Move { x, y })),
        1 => Just(Input::Mouse(Mouse::WheelUp)),
        1 => Just(Input::Mouse(Mouse::WheelDown)),
        1 => Just(Input::Blur),
        1 => text().prop_map(Input::Paste),
        1 => text().prop_map(Input::Reply),
    ]
}

/// What makes a span unsafe to draw.
fn bad(c: char) -> bool {
    c.is_control() || c == '\u{fe0f}' || c == '\u{200d}'
}

fn check_span(span: &Span, what: &str) -> Result<(), TestCaseError> {
    prop_assert!(
        !span.text.chars().any(bad),
        "{what} holds a control character: {:?}",
        span.text
    );
    Ok(())
}

fn check_line(line: &Line, max: usize, what: &str) -> Result<(), TestCaseError> {
    for span in &line.spans {
        check_span(span, what)?;
    }
    if let Some(chip) = &line.chip {
        check_span(chip, what)?;
    }
    let w = width(&line.text());
    prop_assert!(
        w <= max,
        "{what} is {w} cells, over {max}: {:?}",
        line.text()
    );
    Ok(())
}

fn check(screen: &Screen) -> Result<(), TestCaseError> {
    let g = screen.geometry;
    if let Some(title) = &screen.title {
        prop_assert!(!title.chars().any(bad), "title {title:?}");
    }
    if let Some((names, _)) = &screen.tabs {
        for name in names {
            prop_assert!(!name.chars().any(bad), "tab {name:?}");
        }
    }
    if let Some(brand) = &screen.brand {
        check_span(brand, "the brand")?;
    }
    let body = usize::from(g.body.w.max(10));
    prop_assert!(screen.body.len() <= usize::from(g.body.h));
    for (line, _) in &screen.body {
        check_line(line, body, "a body line")?;
    }
    let strip = g.strip.unwrap_or_default();
    prop_assert!(screen.strip.len() <= usize::from(strip.h));
    for line in screen.strip.iter().chain(&screen.rule) {
        check_line(line, usize::from(strip.w), "a strip line")?;
    }
    check_span(&screen.footer, "the footer")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn views_never_panic_or_leak_control_characters(
        data in data(),
        ctx in ctx(),
        size in (0u16..250, 0u16..60).prop_map(|(w, h)| Size { w, h }),
        inputs in vec(input(), 20),
    ) {
        for &view in View::ALL {
            let mut panel = Panel::new(view);
            check(&panel.screen(&data, &ctx, size))?;
            for input in &inputs {
                match input {
                    Input::Key(k) => {
                        panel.key(*k, &data, &ctx, size);
                    }
                    Input::Mouse(m) => {
                        panel.mouse(*m, &data, &ctx, size);
                    }
                    Input::Blur => {
                        panel.blur();
                    }
                    Input::Paste(text) => panel.paste(text),
                    Input::Reply(text) => panel.replied(Err(text.clone())),
                }
                check(&panel.screen(&data, &ctx, size))?;
            }
        }
    }
}
