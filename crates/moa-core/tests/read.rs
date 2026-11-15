//! `brief`, `attention` and `find` on the typical brain (today is Sunday 2026-11-15, noon in
//! New York).

use moa_core::Now;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Input, handle_checked};
use moa_core::model::{ProjectId, TaskId, TaskStatus};
use moa_core::op::{Done, Op, TaskAdd};
use moa_core::state::State;
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

/// The temp dir is random; the layout is what the snapshots pin.
fn mask(b: &TypicalBrain, text: &str) -> String {
    text.replace(b.home.to_str().unwrap(), "<home>")
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

fn find(st: &mut State, q: &str, now: &Now) -> Done {
    ok(st, Op::SessionFind { q: q.into() }, now)
}

#[test]
fn brief_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let text = ok(&mut st, Op::Brief, &b.now()).text;
    assert_eq!(
        lines(&text)[0],
        "# Brief (Sun 15 Nov 12:00). More: moactl attention | project NAME | ls | find Q"
    );
    for want in [
        "\n## Today: ",
        "\n## Needs attention\n- ⚠ ",
        "\n## Active this week: ",
        "\n## Live sessions\n- ",
        "\n## Asleep (idle, window closed to save memory; moactl resume ID): ",
        "\n## What you know about the user (latest notes)\n- (",
    ] {
        assert!(text.contains(want), "{want:?} in {text}");
    }
    insta::assert_snapshot!("brief_matches_the_spike_layout", mask(&b, &text));
}

#[test]
fn attention_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let d = ok(&mut st, Op::Attention, &b.now());
    assert_eq!(d.data["text"], d.text);
    insta::assert_snapshot!("attention_matches_the_spike_layout", mask(&b, &d.text));
}

#[test]
fn attention_names_the_waiting_session_first() {
    let (b, mut st) = typical();
    let text = ok(&mut st, Op::Attention, &b.now()).text;
    let first = lines(&text)[0];
    assert!(
        first.starts_with("- ⚠ Fix the flicker [Lantern] is blocked on you"),
        "{first}"
    );
    assert!(
        first.ends_with(&format!("  → moactl resume {}", &S3_WAITING[..8])),
        "{first}"
    );
    // S9 only sent the idle ping: it never gets a ⚠ line.
    let warned = lines(&text).into_iter().filter(|l| l.starts_with("- ⚠"));
    assert_eq!(warned.count(), 1, "{text}");
    assert!(
        !text.contains(&format!("moactl resume {}", &S9_WAITING_IDLE[..8]))
            || text
                .lines()
                .filter(|l| l.contains(&S9_WAITING_IDLE[..8]))
                .all(|l| l.starts_with("- ●")),
        "{text}"
    );
}

#[test]
fn archived_and_elsewhere_sessions_need_no_attention() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        Op::SessionArchive {
            qs: vec![S3_WAITING.into()],
        },
        &now,
    );
    let text = ok(&mut st, Op::Attention, &now).text;
    assert!(!text.contains('⚠'), "{text}");
    for gone in [S3_WAITING, S7_ELSEWHERE, S6_ARCHIVED] {
        assert!(!text.contains(&gone[..8]), "{text}");
    }
}

#[test]
fn an_empty_brain_has_nothing_to_say() {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    let b = TypicalBrain::new();
    let now = b.now();
    assert_eq!(
        ok(&mut st, Op::Attention, &now).text,
        "nothing needs you right now"
    );
    let brief = ok(&mut st, Op::Brief, &now).text;
    let l = lines(&brief);
    assert_eq!(l[1], "## Today: nothing set yet");
    assert_eq!(&l[2..4], ["## Needs attention", "- nothing"]);
    assert_eq!(l[4], "## Active this week: none");
    assert_eq!(
        &l[5..],
        [
            "## Live sessions",
            "- none",
            "## What you know about the user (latest notes)"
        ]
    );
    assert_eq!(find(&mut st, "x", &now).text, "");
}

#[test]
fn find_lists_matches_with_state_and_age() {
    let (b, mut st) = typical();
    let now = b.now();
    let d = find(&mut st, "lantern", &now);
    let ls = lines(&d.text);
    assert_eq!(ls.len(), 4, "{}", d.text);
    assert!(
        ls[0].starts_with("3f9a1c52  Wire up the dimmer [Private/Lantern]  working  "),
        "{}",
        ls[0]
    );
    let cwd = b.home.join("private/lantern");
    assert!(ls[0].ends_with(cwd.to_str().unwrap()), "{}", ls[0]);
    let rows = d.data.as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["state"], "working");
    assert_eq!(rows[0]["conversation"], S1_WORKING);
    assert_eq!(rows[0]["group"], "Private/Lantern");
    assert_eq!(rows[0]["cwd"], cwd.to_str().unwrap());
    // No match, and an empty name, print nothing.
    for q in ["zzzz", ""] {
        let none = find(&mut st, q, &now);
        assert_eq!(
            (none.text.as_str(), &none.data),
            ("", &serde_json::json!([]))
        );
    }
}

#[test]
fn today_tasks_show_up_in_the_brief_and_attention() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        Op::TaskAdd(TaskAdd {
            project: "Tidepool".into(),
            title: "Walk the shore".into(),
            today: true,
            ..TaskAdd::default()
        }),
        &now,
    );
    let brief = ok(&mut st, Op::Brief, &now).text;
    let today = lines(&brief)[1];
    assert!(
        today.contains("; ") && today.contains("#6 Tidepool: Walk the shore [todo]"),
        "{today}"
    );
    let att = ok(&mut st, Op::Attention, &now).text;
    assert!(
        att.contains("- ▸ today: Tidepool: Walk the shore [todo]"),
        "{att}"
    );
}

/// The typical brain plus one open task for Driftwood (last activity 2026-10-20).
fn with_driftwood_task(b: &TypicalBrain, st: &mut State, status: TaskStatus) {
    let mut snap = b.snapshot();
    let mut task = snap.tasks[0].clone();
    task.id = TaskId(90);
    task.title = "Sand the lid".into();
    task.status = status;
    task.today = None;
    task.blocked_on = None;
    task.project_id = Some(ProjectId(3));
    snap.tasks.push(task);
    let _ = handle_checked(
        st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        b.now(),
    );
}

#[test]
fn attention_orders_its_groups() {
    let (b, mut st) = typical();
    with_driftwood_task(&b, &mut st, TaskStatus::Active);
    let att = ok(&mut st, Op::Attention, &b.now()).text;
    let marks = ["- ⚠", "- ●", "- ▸", "- ⧗", "- ◷", "- ?"];
    let at: Vec<usize> = marks.iter().map(|m| att.find(m).expect(m)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{att}");
    assert!(
        att.contains("- ⧗ blocked: Tidepool: Survey the tide flats on a permit"),
        "{att}"
    );
    assert!(
        att.contains("- ◷ Driftwood: 1 open task(s), untouched since 2026-10-20"),
        "{att}"
    );
    // Tidepool has no date and Lantern is fresh: neither is stale.
    assert_eq!(att.matches("- ◷").count(), 1, "{att}");
}

#[test]
fn a_week_is_counted_in_civil_days() {
    let (b, mut st) = typical();
    with_driftwood_task(&b, &mut st, TaskStatus::Todo);
    let at = |day: &str| {
        let wall = format!("{day}T17:00:00Z")
            .parse::<jiff::Timestamp>()
            .unwrap();
        Now::at(
            moa_core::UtcMs(wall.as_millisecond()),
            std::time::Duration::ZERO,
            b.now().tz,
        )
    };
    // Last activity 2026-10-20: exactly seven days earlier is not "more than 7 days ago".
    let seven = ok(&mut st, Op::Attention, &at("2026-10-27")).text;
    assert!(!seven.contains("- ◷"), "{seven}");
    let eight = ok(&mut st, Op::Attention, &at("2026-10-28")).text;
    assert!(eight.contains("- ◷ Driftwood"), "{eight}");
    let brief = ok(&mut st, Op::Brief, &at("2026-10-27")).text;
    assert!(
        brief.contains("## Active this week: Lantern, Driftwood\n"),
        "{brief}"
    );
}
