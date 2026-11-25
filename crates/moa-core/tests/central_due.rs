//! What's due, and the newly idle terminals, ride on moa's wake-ups (D1, D4, D6): once per
//! task, per due date, per day, and never on their own.

mod common;

use common::central::{
    body_of, end_turn, fire, play, prompt_at, prompts_sent, sent, toml, wake_timer, you_commit,
    you_row,
};
use common::{at, changes, generation, link_up};
use jiff::ToSpan;
use jiff::civil::Date;
use moa_core::central::{due, idle};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, RefuseWrites, handle_checked};
use moa_core::hook::HookKind;
use moa_core::model::{
    AgentKind, Lifecycle, Location, Nag, OpenedBy, Suggestion, SuggestionId, SuggestionStatus,
    Task, TaskId, TaskStatus, TermId, Terminal, YES_TOLD,
};
use moa_core::op::{Answer, ConnId, Op, ReqId, TaskSet};
use moa_core::state::{Deps, Snapshot, State};
use moa_core::time::start_of_next_day;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;

const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;

/// The due block of [`five_due`] on the typical brain's day (D10).
const DUE_BLOCK: &str = "Due by tomorrow (end your reply with these lines as they are):\n\
                         ⏰ 2 days overdue: Reply to Kite; Renew the domain\n\
                         ⏰ 1 day overdue: Send the invoice\n\
                         ⏰ Due today: Merge PR #3\n\
                         ⏰ Due tomorrow: Book the venue";

/// The suggestion each world has open, with nothing to run: a yes to it waits for moa.
const YES_ID: i64 = 900;

/// The typical brain with the tasks and open terminals given, open suggestion [`YES_ID`],
/// moa's account `acct-a` and `settings` after its `moa.toml`, and the tmux link up; a
/// session of it that reports.
struct World {
    b: TypicalBrain,
    st: State,
    /// What the start said.
    start: Vec<Effect>,
    session: (String, String),
    /// The open terminals it started with.
    terminals: Vec<Terminal>,
    seq: u32,
    req: u64,
}

impl World {
    /// `tasks` made on the brain's day, `terminals` against its home.
    fn new(
        tasks: impl FnOnce(Date) -> Vec<Task>,
        terminals: impl FnOnce(&TypicalBrain) -> Vec<Terminal>,
        settings: &str,
    ) -> World {
        let b = TypicalBrain::new();
        let mut snap = b.snapshot();
        snap.tasks = tasks(b.now().local_day);
        let terminals = terminals(&b);
        snap.next_ids.terminal = terminals.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
        snap.terminals = terminals.clone();
        snap.suggestions.push(Suggestion {
            id: SuggestionId(YES_ID),
            text: "Kite has 3 overdue tasks blocked on the domain; move them to Friday?".into(),
            status: SuggestionStatus::Open,
            actions: vec![],
            actions_raw: None,
            actions_invalid: None,
            result: None,
            created_at: UtcMs(1),
            resolved_at: None,
        });
        let deps = Deps {
            home: b.home.clone(),
            parse_action: Some(moa_proto::cli::parse_action),
            ..Deps::default()
        };
        let settings = format!("{}{settings}", toml("acct-a"));
        let (mut st, start) = common::started_at(&b, &settings, deps, snap, b.now());
        link_up(&b, &mut st);
        let session = st
            .sessions()
            .filter(|s| s.lifecycle == Lifecycle::Live && s.location == Location::Here)
            .filter(|s| s.archived_at.is_none())
            .find_map(|s| {
                let c = st
                    .conversations_of(s.id)
                    .find(|c| c.agent_kind == AgentKind::Claude)?;
                Some((s.id.to_string(), c.conversation_id.clone()))
            })
            .expect("a live session of the typical brain");
        World {
            b,
            st,
            start,
            session,
            terminals,
            seq: 0,
            req: 0,
        }
    }

    /// The typical brain's time, `ms` later.
    fn t(&self, ms: i64) -> i64 {
        self.b.now().wall.0 + ms
    }

    fn today(&self) -> Date {
        self.b.now().local_day
    }

    /// The session finishes a turn at `ms`.
    fn report(&mut self, ms: i64) -> Vec<Effect> {
        self.seq += 1;
        let kind = HookKind::TurnEnded {
            last_message: "Merged PR #3".into(),
        };
        let (id, conversation) = &self.session;
        let envelope = common::envelope(id, conversation, UtcMs(ms), self.seq, kind);
        let input = Input::Hook {
            reply: None,
            envelope,
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }

    /// A wake-up at `ms`: the turn under way ends, the session reports, and the `Wake`
    /// timer fires once the report settles; moa's agent starts and opens what it is asked.
    fn wake(&mut self, ms: i64) -> Vec<Effect> {
        if self.st.central().turn.is_some() {
            end_turn(&mut self.st, ms);
        }
        self.report(ms);
        let w = ms + 45 * SEC;
        let fx = fire(&mut self.st, w);
        play(&mut self.st, w, fx)
    }

    /// Your yes to [`YES_ID`] at `ms`; moa's agent starts and opens what it is asked.
    fn yes(&mut self, ms: i64) -> Vec<Effect> {
        self.req += 1;
        let input = Input::Client {
            conn: ConnId(1),
            req: ReqId(self.req),
            caller: Default::default(),
            op: Op::SuggestionAnswer {
                id: Some(YES_ID),
                answer: Answer::Yes,
            },
        };
        let fx = handle_checked(&mut self.st, input, at(UtcMs(ms)));
        play(&mut self.st, ms, fx)
    }

    /// From `ms` until `until` the store refuses writes; the core reloads what it saved.
    fn refuse_writes(&mut self, ms: i64, until: i64) -> Vec<Effect> {
        let mut snapshot = Snapshot {
            tasks: self.st.tasks().cloned().collect(),
            reports: self.st.reports().cloned().collect(),
            threads: self.st.threads().cloned().collect(),
            terminals: self.terminals.clone(),
            ..self.b.snapshot()
        };
        let next = self.terminals.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
        snapshot.next_ids.terminal = next;
        let input = Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(until),
                reason: "the disk is full".into(),
            }),
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }

    /// `task set ID --due DUE` at `ms`.
    fn set_due(&mut self, ms: i64, id: i64, due: Date) -> Vec<Effect> {
        self.req += 1;
        let op = Op::TaskSet(TaskSet {
            id,
            due: Some(due.to_string()),
            ..TaskSet::default()
        });
        let input = Input::Client {
            conn: ConnId(1),
            req: ReqId(self.req),
            caller: Default::default(),
            op,
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }
}

/// Task `id`, titled `title`, `status`, due `days` from `today` (none for `None`).
fn task(id: i64, title: &str, status: TaskStatus, today: Date, days: Option<i64>) -> Task {
    Task {
        id: TaskId(id),
        project_id: None,
        title: title.into(),
        status,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: days.map(|d| today.checked_add(d.days()).unwrap()),
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    }
}

/// D10's tasks on `today`: two 2 days overdue, one 1 day overdue, one today and one
/// tomorrow; and one due in 2 days and one done, which aren't due by tomorrow.
fn five_due(today: Date) -> Vec<Task> {
    let todo = TaskStatus::Todo;
    vec![
        task(1, "Reply to Kite", todo, today, Some(-2)),
        task(2, "Renew the domain", TaskStatus::Active, today, Some(-2)),
        task(3, "Send the invoice", TaskStatus::Waiting, today, Some(-1)),
        task(4, "Merge PR #3", todo, today, Some(0)),
        task(5, "Book the venue", TaskStatus::Blocked, today, Some(1)),
        task(6, "Pack the booth", todo, today, Some(2)),
        task(7, "File the taxes", TaskStatus::Done, today, Some(-1)),
        task(8, "Plan Harbor", todo, today, None),
    ]
}

/// The `nags` rows `changes` saves.
fn nags(changes: &[Change]) -> Vec<Nag> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Nag(n) => Some(*n),
            _ => None,
        })
        .collect()
}

fn nag(task: i64, due: Date, day: Date) -> Nag {
    Nag {
        task_id: TaskId(task),
        due,
        day,
    }
}

#[test]
fn the_due_block() {
    let w = World::new(five_due, |_| vec![], "");
    let now = w.b.now();
    let due = due::due_now(&w.st, &now);
    let ids: Vec<i64> = due.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5]);
    let block = due::block(&w.st, &now, &due);
    assert_eq!(block, DUE_BLOCK);
    insta::assert_snapshot!(block);
}

#[test]
fn a_wake_carries_whats_due() {
    let mut w = World::new(five_due, |_| vec![], "");
    let today = w.today();
    let fx = w.wake(w.t(MIN));
    let text = sent(&fx);
    let wake = text
        .find("(moa · a wake-up, not typed by the user)")
        .unwrap();
    let due = text.find("Due by tomorrow").expect("the due block");
    assert!(wake < due, "{text}");
    assert!(text.ends_with(&format!("\n\n---\n\n{DUE_BLOCK}")), "{text}");
    let commit = you_commit(&fx);
    let d = |days: i64| today.checked_add(days.days()).unwrap();
    let expected = vec![
        nag(1, d(-2), today),
        nag(2, d(-2), today),
        nag(3, d(-1), today),
        nag(4, d(0), today),
        nag(5, d(1), today),
    ];
    assert_eq!(nags(commit), expected);
    let body = body_of(you_row(&fx));
    assert_eq!(body["text"].as_str(), Some(text.as_str()));
    assert_eq!(body["nags"], 5);
    assert!(body.get("carried").is_none(), "{body}");
    for n in &expected {
        assert!(w.st.nagged(n.task_id, n.due, n.day));
    }
    assert!(due::due_now(&w.st, &w.b.now()).is_empty());
}

#[test]
fn nags_are_once_a_day() {
    let mut w = World::new(five_due, |_| vec![], "");
    let today = w.today();
    let first = sent(&w.wake(w.t(MIN)));
    assert!(first.ends_with(DUE_BLOCK), "{first}");
    // The same day: told already.
    let fx = w.wake(w.t(10 * MIN));
    let again = sent(&fx);
    assert!(!again.contains("Due by tomorrow"), "{again}");
    assert!(nags(you_commit(&fx)).is_empty());
    assert!(body_of(you_row(&fx)).get("nags").is_none());
    // The next day's first wake tells them again, a day later each.
    let tomorrow = start_of_next_day(today, &common::tz()).0 + HOUR;
    let fx = w.wake(tomorrow);
    let next = sent(&fx);
    let block = "Due by tomorrow (end your reply with these lines as they are):\n\
                 ⏰ 3 days overdue: Reply to Kite; Renew the domain\n\
                 ⏰ 2 days overdue: Send the invoice\n\
                 ⏰ 1 day overdue: Merge PR #3\n\
                 ⏰ Due today: Book the venue\n\
                 ⏰ Due tomorrow: Pack the booth";
    assert!(next.ends_with(block), "{next}");
    let day = today.checked_add(1.days()).unwrap();
    assert!(nags(you_commit(&fx)).iter().all(|n| n.day == day));
    assert_eq!(nags(you_commit(&fx)).len(), 6);
    let fx = w.wake(tomorrow + 10 * MIN);
    assert!(!sent(&fx).contains("Due by tomorrow"));
}

/// Review focus 3: a date moved during the day is told again for its new date; moved back,
/// it was told for that one already.
#[test]
fn a_moved_date_is_told_again() {
    let tasks = |today| vec![task(4, "Merge PR #3", TaskStatus::Todo, today, Some(0))];
    let mut w = World::new(tasks, |_| vec![], "");
    let today = w.today();
    let text = sent(&w.wake(w.t(MIN)));
    assert!(text.ends_with("⏰ Due today: Merge PR #3"), "{text}");
    let tomorrow = today.checked_add(1.days()).unwrap();
    let fx = w.set_due(w.t(2 * MIN), 4, tomorrow);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    let fx = w.wake(w.t(10 * MIN));
    let text = sent(&fx);
    assert!(text.ends_with("⏰ Due tomorrow: Merge PR #3"), "{text}");
    assert_eq!(nags(you_commit(&fx)), [nag(4, tomorrow, today)]);
    w.set_due(w.t(11 * MIN), 4, today);
    let text = sent(&w.wake(w.t(20 * MIN)));
    assert!(!text.contains("Due by tomorrow"), "{text}");
}

/// U18: due dates never wake moa: they ride on a wake-up the reports make.
#[test]
fn due_tasks_dont_wake_moa() {
    let mut w = World::new(five_due, |_| vec![], "");
    let today = w.today();
    assert!(
        !matches!(wake_timer(&w.start), Some(Some(_))),
        "{:?}",
        w.start
    );
    assert_eq!(w.st.central().wake_armed, None);
    let fx = w.set_due(w.t(MIN), 6, today);
    assert!(!matches!(wake_timer(&fx), Some(Some(_))), "{fx:?}");
    assert!(prompts_sent(&fx).is_empty());
    // The timer fires all the same: nothing goes.
    let ms = w.t(HOUR);
    let fx = fire(&mut w.st, ms);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert!(nags(&changes(&fx).into_iter().cloned().collect::<Vec<_>>()).is_empty());
}

/// U18, ruling R2: your prompt carries nothing due (one that opens the day is Task 6's).
#[test]
fn your_prompt_carries_nothing_due() {
    let mut w = World::new(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    let fx = prompt_at(&mut w.st, at(UtcMs(ms)), 1, "hi", false);
    let fx = play(&mut w.st, ms, fx);
    assert_eq!(sent(&fx), "hi");
    assert!(nags(you_commit(&fx)).is_empty());
    assert_eq!(body_of(you_row(&fx)), serde_json::json!({ "text": "hi" }));
    assert_eq!(due::due_now(&w.st, &w.b.now()).len(), 5);
}

/// Review focus 3: 50 tasks due, one 300 days overdue with a newline and ANSI escapes in its
/// title, one with a long title: 20 a prompt, the rest the next time; each line one line.
#[test]
fn the_due_list_is_bounded_and_clean() {
    const { assert!(due::DUE_MAX == 20) };
    let todo = TaskStatus::Todo;
    let odd = "Renew\nthe \u{1b}[31mdomain\u{1b}[0m\r";
    let long = "Kite ".repeat(40);
    let tasks = |today| {
        let mut tasks = vec![task(1, odd, todo, today, Some(-300))];
        tasks.push(task(2, &long, todo, today, Some(-1)));
        for n in 3..=50 {
            tasks.push(task(
                n,
                &format!("Kite chore {n}"),
                todo,
                today,
                Some(1 - n),
            ));
        }
        tasks
    };
    let mut w = World::new(tasks, |_| vec![], "");
    let fx = w.wake(w.t(MIN));
    let text = sent(&fx);
    let block = &text[text.find("Due by tomorrow").unwrap()..];
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines.len(), 22, "{block}");
    assert_eq!(lines[1], "⏰ 300 days overdue: Renew the  [31mdomain [0m ");
    assert_eq!(lines[2], "⏰ 49 days overdue: Kite chore 50");
    assert_eq!(lines[21], "(and 30 more: moactl task ls)");
    assert!(
        block.chars().all(|c| c == '\n' || !c.is_control()),
        "{block:?}"
    );
    assert_eq!(nags(you_commit(&fx)).len(), 20);
    // The next 20, then the last 10.
    let text = sent(&w.wake(w.t(10 * MIN)));
    let block = &text[text.find("Due by tomorrow").unwrap()..];
    assert_eq!(block.lines().count(), 22, "{block}");
    assert!(
        block.ends_with("\n(and 10 more: moactl task ls)"),
        "{block}"
    );
    let text = sent(&w.wake(w.t(20 * MIN)));
    let block = &text[text.find("Due by tomorrow").unwrap()..];
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines.len(), 11, "{block}");
    // The long title is cut to 80 characters.
    let cut: String = long.chars().take(80).collect();
    assert_eq!(*lines.last().unwrap(), format!("⏰ 1 day overdue: {cut}"));
    let text = sent(&w.wake(w.t(30 * MIN)));
    assert!(!text.contains("Due by tomorrow"), "{text}");
    assert!(due::due_now(&w.st, &w.b.now()).is_empty());
}

// ---------- idle terminals ----------

/// Open terminal `id` named `name` in `cwd` (under the brain's home), running `command`
/// under `zsh`, opened at `opened` and touched at `touched`.
fn term(
    b: &TypicalBrain,
    id: i64,
    name: &str,
    cwd: &str,
    command: &str,
    opened: i64,
    touched: Option<i64>,
) -> Terminal {
    let g = generation();
    Terminal {
        id: TermId(id),
        server_pid: g.pid,
        server_start: g.start,
        pane_id: format!("%{}", 20 + id),
        opened_by: OpenedBy::Found,
        cwd: b.home.join(cwd).to_string_lossy().into_owned(),
        session_id: None,
        opened_at: UtcMs(opened),
        touched_at: touched.map(UtcMs),
        name: name.into(),
        note: None,
        timeout_at: None,
        shell: "zsh".into(),
        command: command.into(),
        command_since: UtcMs(opened),
        closed_at: None,
    }
}

/// D10's terminals at `b`'s time: #4 at its prompt, untouched for 5 h, and #7 running
/// `tail`, opened 3 h ago and never touched; and #8, touched an hour ago.
fn d10_terminals(b: &TypicalBrain) -> Vec<Terminal> {
    let now = b.now().wall.0;
    vec![
        term(
            b,
            4,
            "kite",
            "private/kite",
            "zsh",
            now - 9 * HOUR,
            Some(now - 5 * HOUR),
        ),
        term(
            b,
            7,
            "logs",
            "private/lantern",
            "tail",
            now - 3 * HOUR,
            None,
        ),
        term(
            b,
            8,
            "beacon",
            "private/beacon",
            "zsh",
            now - 9 * HOUR,
            Some(now - HOUR),
        ),
    ]
}

const IDLE_BLOCK: &str = "Idle terminals (untouched for 2 h or more; offer to close one that \
                          looks forgotten, as a suggestion with --do \"term close ID\"):\n\
                          - #4 kite · ~/private/kite · idle 5 h · at its prompt\n\
                          - #7 logs · ~/private/lantern · idle 3 h · running tail";

#[test]
fn the_idle_block() {
    let w = World::new(|_| vec![], d10_terminals, "");
    let now: Now = w.b.now();
    let terms = idle::idle_terms(&w.st, &now, None);
    let ids: Vec<i64> = terms.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, [4, 7]);
    let block = idle::block(&w.st, &now, &terms);
    assert_eq!(block, IDLE_BLOCK);
    insta::assert_snapshot!(block);
    // Since a moment after #4 went idle, only #7 is newly idle.
    let since = Some((UtcMs(now.wall.0 - 3 * HOUR + MIN), TermId(0)));
    let ids: Vec<i64> = idle::idle_terms(&w.st, &now, since)
        .iter()
        .map(|t| t.id.0)
        .collect();
    assert_eq!(ids, [7]);
    // No idle terminals at all with the setting 0.
    let mut w = World::new(|_| vec![], d10_terminals, "[ui]\nterminal_idle_hours = 0\n");
    assert!(idle::idle_terms(&w.st, &w.b.now(), None).is_empty());
    let text = sent(&w.wake(w.t(MIN)));
    assert!(!text.contains("Idle terminals"), "{text}");
}

/// D6: a wake-up tells the terminals that went idle since the last one: one idle before it
/// isn't told again; one that went idle after it is.
#[test]
fn a_wake_tells_only_newly_idle_terminals() {
    let terms = |b: &TypicalBrain| {
        let now = b.now().wall.0;
        vec![
            term(
                b,
                4,
                "kite",
                "private/kite",
                "zsh",
                now - 9 * HOUR,
                Some(now - 5 * HOUR),
            ),
            term(
                b,
                8,
                "beacon",
                "private/beacon",
                "zsh",
                now - 9 * HOUR,
                Some(now - HOUR),
            ),
        ]
    };
    let mut w = World::new(|_| vec![], terms, "");
    let text = sent(&w.wake(w.t(MIN)));
    let wake = text.find("(moa · a wake-up").unwrap();
    let idle = text.find("Idle terminals").expect("the idle block");
    assert!(wake < idle, "{text}");
    assert!(
        text.ends_with("\n- #4 kite · ~/private/kite · idle 5 h · at its prompt"),
        "{text}"
    );
    assert!(!text.contains("#8 beacon"), "{text}");
    // #8 goes idle an hour after the typical time: the next wake tells it alone.
    let text = sent(&w.wake(w.t(2 * HOUR)));
    assert!(!text.contains("#4 kite"), "{text}");
    assert!(
        text.ends_with("\n- #8 beacon · ~/private/beacon · idle 3 h · at its prompt"),
        "{text}"
    );
    // Nothing went idle since: no idle block.
    let text = sent(&w.wake(w.t(3 * HOUR)));
    assert!(!text.contains("Idle terminals"), "{text}");
}

/// D1's order on a wake-up: the reports, then what's due, then the idle terminals.
#[test]
fn a_wake_carries_due_then_idle() {
    let mut w = World::new(five_due, d10_terminals, "");
    let text = sent(&w.wake(w.t(MIN)));
    let tail = format!("\n\n---\n\n{DUE_BLOCK}\n\n---\n\n");
    let i = text.find(&tail).expect("due, then idle");
    let idle = &text[i + tail.len()..];
    assert!(
        idle.starts_with("Idle terminals (untouched for 2 h"),
        "{text}"
    );
    assert!(idle.contains("- #4 kite · ~/private/kite · idle 5 h · at its prompt\n"));
}

/// Review I1, ruling R6: a yes's prompt that carries the reports, and no idle block, between
/// two wake-ups hides no terminal from the second.
#[test]
fn a_yes_between_wakes_hides_no_idle_terminal() {
    let terms = |b: &TypicalBrain| {
        let now = b.now().wall.0;
        vec![
            term(
                b,
                4,
                "kite",
                "private/kite",
                "zsh",
                now - 9 * HOUR,
                Some(now - 5 * HOUR),
            ),
            term(
                b,
                8,
                "beacon",
                "private/beacon",
                "zsh",
                now - 9 * HOUR,
                Some(now - HOUR),
            ),
        ]
    };
    let mut w = World::new(|_| vec![], terms, "");
    let text = sent(&w.wake(w.t(MIN)));
    assert!(text.contains("- #4 kite"), "{text}");
    // #8 goes idle an hour after the typical time. Then a session reports, and your yes goes
    // at once, carrying the report too.
    let ms = w.t(2 * HOUR);
    end_turn(&mut w.st, ms);
    w.report(ms);
    let fx = w.yes(ms + SEC);
    let text = sent(&fx);
    assert!(text.contains("(moa · the user said yes"), "{text}");
    assert!(text.contains("(moa · a wake-up, not typed by the user) 1 session report(s)"));
    assert!(!text.contains("Idle terminals"), "{text}");
    assert_eq!(body_of(you_row(&fx))["reports"], 1);
    assert!(
        w.st.suggestions()
            .any(|s| s.result.as_deref() == Some(YES_TOLD))
    );
    // The next wake-up tells #8.
    let text = sent(&w.wake(w.t(3 * HOUR)));
    assert!(
        text.ends_with("\n- #8 beacon · ~/private/beacon · idle 4 h · at its prompt"),
        "{text}"
    );
}

/// Review m1: past [`idle::IDLE_MAX`], the rest come with the next wake-up, even the one
/// that went idle at the same moment as the last one told.
#[test]
fn idle_terminals_past_ten_come_next_time() {
    const { assert!(idle::IDLE_MAX == 10) };
    let terms = |b: &TypicalBrain| {
        let now = b.now().wall.0;
        (1..=12)
            .map(|id| {
                // #11 was touched with #10.
                let touched = now - 20 * HOUR + id.min(10) * MIN;
                let name = format!("kite-{id}");
                term(
                    b,
                    id,
                    &name,
                    "private/kite",
                    "zsh",
                    now - 30 * HOUR,
                    Some(touched),
                )
            })
            .collect()
    };
    let mut w = World::new(|_| vec![], terms, "");
    let told = |text: &str| -> Vec<String> {
        let lines = text.lines().filter(|l| l.starts_with("- #"));
        lines
            .map(|l| l.split(' ').nth(1).unwrap().to_string())
            .collect()
    };
    let text = sent(&w.wake(w.t(MIN)));
    let first: Vec<String> = (1..=10).map(|id| format!("#{id}")).collect();
    assert_eq!(told(&text), first, "{text}");
    let text = sent(&w.wake(w.t(10 * MIN)));
    assert_eq!(told(&text), ["#11", "#12"], "{text}");
    let text = sent(&w.wake(w.t(20 * MIN)));
    assert!(!text.contains("Idle terminals"), "{text}");
}

/// Review m2: a wake-up whose commit is refused saves no `nags` row, and what's due goes with
/// the next one once writes are back. The idle terminals it would have told wait too.
#[test]
fn a_refused_wake_saves_no_nag() {
    let mut w = World::new(five_due, d10_terminals, "");
    let ms = w.t(MIN);
    w.report(ms);
    let wake = ms + 45 * SEC;
    let fx = fire(&mut w.st, wake);
    // Writes are refused while moa's agent starts for the wake.
    w.refuse_writes(wake, wake + 10 * MIN);
    let fx = play(&mut w.st, wake, fx);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    let saved = changes(&fx).into_iter().cloned().collect::<Vec<_>>();
    assert!(nags(&saved).is_empty(), "{saved:?}");
    assert_eq!(due::due_now(&w.st, &at(UtcMs(wake))).len(), 5);
    assert_eq!(w.st.central().idle_told, None);
    // Writes are back: the wake goes, with what's due and the idle terminals.
    let back = wake + 10 * MIN;
    let fx = fire(&mut w.st, back);
    let fx = play(&mut w.st, back, fx);
    let text = sent(&fx);
    assert!(text.contains(&format!("\n\n---\n\n{DUE_BLOCK}\n\n---\n\nIdle terminals")));
    assert_eq!(nags(you_commit(&fx)).len(), 5);
}
