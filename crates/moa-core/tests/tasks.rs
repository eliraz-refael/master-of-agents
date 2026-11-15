//! `task ls|add|set` and `note`, on the typical brain (today is Sunday 2026-11-15, noon in
//! New York).

use jiff::civil::date;
use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::BrainName;
use moa_core::effect::{Effect, Input, handle_checked, reply_of};
use moa_core::model::{Priority, TaskId, TaskStatus, Who};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId, TaskAdd, TaskSet};
use moa_core::state::{CallerInfo, LoadNote, State};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::{TypicalBrain, run};
use moa_testkit::ids::*;

fn typical() -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let st = b.state();
    (b, st)
}

fn ok(st: &mut State, op: Op, now: &Now) -> Done {
    run(st, op, now).unwrap()
}

fn err(st: &mut State, op: Op, now: &Now) -> String {
    run(st, op, now).unwrap_err().0
}

fn effects(st: &mut State, op: Op, now: &Now) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(1),
            caller: CallerInfo::default(),
            op,
        },
        now.clone(),
    )
}

fn changes(fx: &[Effect]) -> Vec<&Change> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn activity_texts(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

fn persisted(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Persist(_)))
}

fn h(conv: &str) -> &str {
    &conv[..8]
}

fn s(text: &str) -> Option<String> {
    Some(text.to_string())
}

fn task_add(project: &str, title: &str) -> TaskAdd {
    TaskAdd {
        project: project.into(),
        title: title.into(),
        ..TaskAdd::default()
    }
}

fn task_set(id: i64) -> TaskSet {
    TaskSet {
        id,
        ..TaskSet::default()
    }
}

fn ls(st: &mut State, now: &Now, project: Option<&str>, all: bool) -> String {
    ok(
        st,
        Op::TaskLs {
            project: project.map(Into::into),
            all,
        },
        now,
    )
    .text
}

fn task(st: &State, id: i64) -> moa_core::model::Task {
    st.tasks().find(|t| t.id == TaskId(id)).unwrap().clone()
}

// ---------- ls ----------

#[test]
fn task_ls_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let text = ls(&mut st, &b.now(), None, false);
    assert!(text.starts_with("## -\n"), "{text}");
    assert!(
        text.contains(&format!(
            "## Lantern\n- #1 [todo] Order the dimmer board  (P1, today, who=you, due=2026-11-16, session={})",
            h(S1_WORKING)
        )),
        "{text}"
    );
    insta::assert_snapshot!("task_ls", text);
}

#[test]
fn task_ls_all_keeps_done_and_dropped_and_filters_by_project() {
    let (b, mut st) = typical();
    let now = b.now();
    let all = ls(&mut st, &now, None, true);
    assert!(all.contains("- #2 [done] Draft the wiring diagram  (who=agent)"));
    assert!(all.contains("## Driftwood\n- #4 [dropped] Carve the sign"));
    assert!(!ls(&mut st, &now, None, false).contains("Carve the sign"));
    // Exact name, any case.
    let lantern = ls(&mut st, &now, Some("LANTERN"), false);
    assert!(lantern.starts_with("## Lantern\n- #1"), "{lantern}");
    assert!(!lantern.contains("Tidepool") && !lantern.contains("## -"));
    // A part of a name is not a match.
    assert_eq!(ls(&mut st, &now, Some("lant"), false), "no open tasks");
    assert_eq!(ls(&mut st, &now, Some("Driftwood"), false), "no open tasks");
}

#[test]
fn task_ls_data_has_the_rows_and_project_names() {
    let (b, mut st) = typical();
    let d = ok(
        &mut st,
        Op::TaskLs {
            project: None,
            all: false,
        },
        &b.now(),
    )
    .data;
    let rows = d.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], 5);
    assert!(rows[0]["project"].is_null());
    assert_eq!(rows[1]["id"], 1);
    assert_eq!(rows[1]["project"], "Lantern");
    assert_eq!(rows[1]["due"], "2026-11-16");
}

#[test]
fn today_is_only_shown_for_the_brains_local_day() {
    let (b, mut st) = typical();
    // Task 1 has `today = 2026-11-15`; a day later it isn't today any more.
    let next = Now::at(
        UtcMs(b.now().wall.0 + 24 * 3600 * 1000),
        std::time::Duration::ZERO,
        b.now().tz,
    );
    assert!(!ls(&mut st, &next, None, false).contains("today"));
}

// ---------- add ----------

#[test]
fn task_add_with_relative_due_and_session() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        Op::TaskAdd(TaskAdd {
            due: s("fri"),
            session: s("dimmer"),
            priority: s("p0"),
            ..task_add("lantern", "Solder the board")
        }),
        &now,
    );
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap().text,
        "task #6 added"
    );
    let t = task(&st, 6);
    assert_eq!(t.due, Some(date(2026, 11, 20)));
    assert_eq!(t.who, Some(Who::You));
    assert_eq!(t.status, TaskStatus::Todo);
    assert_eq!(t.priority, Some(Priority::P0));
    assert_eq!(t.session_id, Some(st.resolve(h(S1_WORKING)).unwrap()));
    assert_eq!(t.project_id.map(|p| p.0), Some(1));
    assert_eq!((t.created_at, t.updated_at), (now.wall, now.wall));
    assert_eq!((t.today, t.done_at), (None, None));
    assert_eq!(
        activity_texts(&fx),
        ["new task \"Solder the board\" in Lantern"]
    );
}

#[test]
fn task_add_status_defaults_and_done_at() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            blocked_on: s("parts"),
            ..task_add("Lantern", "A")
        }),
        &now,
    );
    assert_eq!(task(&st, 6).status, TaskStatus::Blocked);
    assert_eq!(task(&st, 6).blocked_on.as_deref(), Some("parts"));
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            blocked_on: s("parts"),
            status: s("waiting"),
            who: s("agent"),
            priority: s(""),
            ..task_add("Lantern", "B")
        }),
        &now,
    );
    let t = task(&st, 7);
    assert_eq!(
        (t.status, t.who, t.priority),
        (TaskStatus::Waiting, Some(Who::Agent), None)
    );
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            status: s("done"),
            today: true,
            ..task_add("Lantern", "C")
        }),
        &now,
    );
    let t = task(&st, 8);
    assert_eq!(t.done_at, Some(now.wall));
    assert_eq!(t.today, Some(date(2026, 11, 15)));
}

#[test]
fn task_add_refusals() {
    let (b, mut st) = typical();
    let now = b.now();
    let ambiguous = st.resolve("lantern").unwrap_err().0;
    let cases: Vec<(TaskAdd, String)> = vec![
        (
            task_add("nope", "x"),
            "no project 'nope' (moactl project NAME to check)".into(),
        ),
        (task_add("Lantern", ""), "a task needs a title".into()),
        (task_add("Lantern", "   "), "a task needs a title".into()),
        (
            TaskAdd {
                status: s("someday"),
                ..task_add("Lantern", "x")
            },
            "--status: 'someday' isn't one of idea, todo, active, waiting, blocked, done, dropped"
                .into(),
        ),
        (
            TaskAdd {
                due: s("next week"),
                ..task_add("Lantern", "x")
            },
            "--due: 'next week' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)"
                .into(),
        ),
        (
            TaskAdd {
                session: s("lantern"),
                ..task_add("Lantern", "x")
            },
            ambiguous,
        ),
        (
            TaskAdd {
                session: s("   "),
                ..task_add("Lantern", "x")
            },
            "a session name can't be empty".into(),
        ),
        (
            TaskAdd {
                who: s("robot"),
                ..task_add("Lantern", "x")
            },
            "--who: 'robot' isn't one of you, agent, other".into(),
        ),
        (
            TaskAdd {
                priority: s("p9"),
                ..task_add("Lantern", "x")
            },
            "--priority: 'p9' isn't one of p0, p1, p2 (or empty for none)".into(),
        ),
    ];
    for (a, want) in cases {
        let fx = effects(&mut st, Op::TaskAdd(a.clone()), &now);
        assert_eq!(
            reply_of(&fx).unwrap().as_ref().unwrap_err().0,
            want,
            "{a:?}"
        );
        assert!(!persisted(&fx), "{a:?}");
    }
    assert_eq!(st.tasks().count(), 5);
}

#[test]
fn due_words_are_not_case_sensitive() {
    let (b, mut st) = typical();
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            due: s("TOMORROW"),
            ..task_add("Lantern", "x")
        }),
        &b.now(),
    );
    assert_eq!(task(&st, 6).due, Some(date(2026, 11, 16)));
}

#[test]
fn due_today_at_half_past_eleven_is_today() {
    let (b, mut st) = typical();
    // 04:30 UTC on the 16th is 23:30 on the 15th in New York.
    let at: jiff::Timestamp = "2026-11-16T04:30:00Z".parse().unwrap();
    let now = Now::at(
        UtcMs(at.as_millisecond()),
        std::time::Duration::ZERO,
        TimeZone::posix(moa_testkit::brain::TYPICAL_TZ).unwrap(),
    );
    assert_eq!(now.local_day, date(2026, 11, 15));
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            due: s("today"),
            today: true,
            ..task_add("Lantern", "Late")
        }),
        &now,
    );
    let t = task(&st, 6);
    assert_eq!(t.due, Some(date(2026, 11, 15)));
    assert_eq!(t.today, Some(date(2026, 11, 15)));
    drop(b);
}

// ---------- set ----------

#[test]
fn task_set_fields_and_clears() {
    let (b, mut st) = typical();
    let now = b.now();
    assert_eq!(task(&st, 1).due, Some(date(2026, 11, 16)));
    assert_eq!(task(&st, 1).priority, Some(Priority::P1));
    let d = ok(
        &mut st,
        Op::TaskSet(TaskSet {
            due: s(""),
            priority: s(""),
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(d.text, "ok");
    assert_eq!((task(&st, 1).due, task(&st, 1).priority), (None, None));
    assert_eq!(task(&st, 1).updated_at, now.wall);

    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            status: s("done"),
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(
        activity_texts(&fx),
        ["task \"Order the dimmer board\": todo → done"]
    );
    assert_eq!(task(&st, 1).done_at, Some(now.wall));

    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            status: s("active"),
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(
        activity_texts(&fx),
        ["task \"Order the dimmer board\": done → active"]
    );
    assert_eq!(task(&st, 1).done_at, None);

    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            blocked_on: s("parts"),
            ..task_set(1)
        }),
        &now,
    );
    let t = task(&st, 1);
    assert_eq!(
        (t.status, t.blocked_on.as_deref()),
        (TaskStatus::Blocked, Some("parts"))
    );
    // An explicit status wins over the blocked default; "" clears the reason.
    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            blocked_on: s("more parts"),
            status: s("waiting"),
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(task(&st, 1).status, TaskStatus::Waiting);
    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            blocked_on: s(""),
            session: s(""),
            note: s("n"),
            who: s("other"),
            ..task_set(1)
        }),
        &now,
    );
    let t = task(&st, 1);
    assert_eq!(
        (t.blocked_on, t.session_id, t.who),
        (None, None, Some(Who::Other))
    );
    assert_eq!(t.note.as_deref(), Some("n"));
    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            note: s(""),
            session: s("dimmer"),
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(task(&st, 1).note, None);
    assert_eq!(
        task(&st, 1).session_id,
        Some(st.resolve(h(S1_WORKING)).unwrap())
    );
}

#[test]
fn task_set_validates_every_value_before_applying_any() {
    let (b, mut st) = typical();
    let now = b.now();
    for (a, want) in [
        (
            TaskSet {
                status: s("someday"),
                title: s("New"),
                ..task_set(1)
            },
            "--status: 'someday' isn't one of idea, todo, active, waiting, blocked, done, dropped",
        ),
        (
            TaskSet {
                priority: s("high"),
                title: s("New"),
                ..task_set(1)
            },
            "--priority: 'high' isn't one of p0, p1, p2 (or empty for none)",
        ),
        (
            TaskSet {
                due: s("soon"),
                title: s("New"),
                ..task_set(1)
            },
            "--due: 'soon' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)",
        ),
    ] {
        let fx = effects(&mut st, Op::TaskSet(a), &now);
        assert_eq!(reply_of(&fx).unwrap().as_ref().unwrap_err().0, want);
        assert!(!persisted(&fx));
    }
    assert_eq!(task(&st, 1).title, "Order the dimmer board");
}

#[test]
fn task_set_title_and_today_wording() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            title: s("Survey again"),
            today: true,
            ..task_set(3)
        }),
        &now,
    );
    assert_eq!(activity_texts(&fx), ["added to today: \"Survey again\""]);
    assert_eq!(task(&st, 3).today, Some(date(2026, 11, 15)));
    // Already today: no second line. A status change names the title as it was before.
    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            title: s("Survey once more"),
            today: true,
            status: s("active"),
            ..task_set(3)
        }),
        &now,
    );
    assert_eq!(
        activity_texts(&fx),
        ["task \"Survey again\": blocked → active"]
    );
}

#[test]
fn today_wins_over_not_today() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            not_today: true,
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(task(&st, 1).today, None);
    ok(
        &mut st,
        Op::TaskSet(TaskSet {
            today: true,
            not_today: true,
            ..task_set(1)
        }),
        &now,
    );
    assert_eq!(task(&st, 1).today, Some(date(2026, 11, 15)));
}

#[test]
fn nothing_changed_keeps_updated_at() {
    let (b, mut st) = typical();
    let before = task(&st, 1);
    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            status: s("todo"),
            ..task_set(1)
        }),
        &b.now(),
    );
    assert!(reply_of(&fx).unwrap().is_ok());
    assert!(!persisted(&fx));
    assert_eq!(task(&st, 1), before);
}

#[test]
fn no_task_and_empty_title() {
    let (b, mut st) = typical();
    let now = b.now();
    assert_eq!(err(&mut st, Op::TaskSet(task_set(99)), &now), "no task #99");
    for t in ["", "  "] {
        assert_eq!(
            err(
                &mut st,
                Op::TaskSet(TaskSet {
                    title: s(t),
                    ..task_set(1)
                }),
                &now
            ),
            "--title can't be empty"
        );
    }
}

#[test]
fn an_empty_who_is_refused_not_ignored() {
    let (b, mut st) = typical();
    let now = b.now();
    let before = task(&st, 1);
    for w in ["", "  "] {
        let set = TaskSet {
            who: s(w),
            status: s("done"),
            ..task_set(1)
        };
        let fx = effects(&mut st, Op::TaskSet(set), &now);
        assert_eq!(
            reply_of(&fx).unwrap().as_ref().unwrap_err().0,
            "--who can't be empty; a task always has someone on it"
        );
        assert!(!persisted(&fx));
    }
    assert_eq!(task(&st, 1), before);
}

#[test]
fn a_project_name_with_spaces_around_it_still_finds_the_project() {
    let (b, mut st) = typical();
    let now = b.now();
    let text = ok(
        &mut st,
        Op::TaskAdd(task_add("  lantern ", "Trim me")),
        &now,
    )
    .text;
    let id: i64 = text
        .strip_prefix("task #")
        .and_then(|t| t.strip_suffix(" added"))
        .unwrap()
        .parse()
        .unwrap();
    let lantern = st.projects().find(|p| p.name == "Lantern").unwrap().id;
    assert_eq!(task(&st, id).project_id, Some(lantern));
    assert_eq!(
        ok(&mut st, note_op("n", Some(" Lantern "), None), &now).text,
        "noted"
    );
}

#[test]
fn a_task_with_an_unknown_value_is_not_edited() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    snapshot.unknown.push(LoadNote {
        table: "tasks",
        key: "#3".into(),
        reason: "x",
    });
    let mut st = State::new(BrainName::default_brain(), b.config());
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), b.now());
    let fx = effects(
        &mut st,
        Op::TaskSet(TaskSet {
            status: s("done"),
            ..task_set(3)
        }),
        &b.now(),
    );
    let e: &Result<Done, OpError> = reply_of(&fx).unwrap();
    insta::assert_snapshot!("unknown_task", e.as_ref().unwrap_err().0);
    assert!(!persisted(&fx));
    assert_eq!(task(&st, 3).status, TaskStatus::Blocked);
}

// ---------- notes ----------

fn note_op(text: &str, project: Option<&str>, kind: Option<&str>) -> Op {
    Op::NoteAdd {
        text: text.into(),
        project: project.map(Into::into),
        kind: kind.map(Into::into),
    }
}

#[test]
fn a_note_defaults_to_fact_and_keeps_any_kind() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(
        &mut st,
        note_op("lamp is 12 V", Some("lantern"), None),
        &now,
    );
    let d = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(d.text, "noted");
    assert_eq!(d.data["kind"], "fact");
    assert_eq!(d.data["project_id"], 1);
    assert_eq!(activity_texts(&fx), ["noted (fact): lamp is 12 V"]);
    let fx = effects(&mut st, note_op("a thing", None, Some("milestone")), &now);
    assert_eq!(activity_texts(&fx), ["noted (milestone): a thing"]);
    let n = st.notes().last().unwrap();
    assert_eq!((n.kind.0.as_str(), n.project_id), ("milestone", None));
    assert_eq!(n.created_at, now.wall);
    // The activity line keeps 100 characters.
    let long = "x".repeat(150);
    let fx = effects(&mut st, note_op(&long, None, None), &now);
    assert_eq!(
        activity_texts(&fx),
        [format!("noted (fact): {}", "x".repeat(100))]
    );
    assert_eq!(st.notes().last().unwrap().text, long);
}

#[test]
fn note_with_unknown_project_is_kept() {
    let (b, mut st) = typical();
    let fx = effects(
        &mut st,
        note_op("lamp is 12 V", Some("nope"), Some("fact")),
        &b.now(),
    );
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap().text,
        "noted (no project 'nope'; saved without one)"
    );
    let n = st.notes().last().unwrap();
    assert_eq!((n.text.as_str(), n.project_id), ("lamp is 12 V", None));
    assert_eq!(activity_texts(&fx), ["noted (fact): lamp is 12 V"]);
}

#[test]
fn an_empty_note_is_refused() {
    let (b, mut st) = typical();
    assert_eq!(
        err(&mut st, note_op("  ", None, None), &b.now()),
        "a note needs some text"
    );
    assert_eq!(st.notes().count(), 4);
}
