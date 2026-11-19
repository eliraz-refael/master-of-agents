//! When a panel wakes (V10): the exact moment a visible text changes, and ticking only while
//! something that moves is on screen. Each test builds its own small data.

use std::path::PathBuf;

use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_core::time::start_of_next_day;
use moa_view::ctx::Ctx;
use moa_view::data::Data;
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::panel::Panel;
use serde_json::{Value, json};

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;

/// Thu 19 Nov 2026, 10:00 UTC.
const NOW: UtcMs = UtcMs(1_795_082_400_000);

fn ctx() -> Ctx {
    Ctx::at(NOW, TimeZone::UTC, 0, PathBuf::from("/h"), |_| true)
}

fn id(n: u32) -> String {
    format!("5e55{n:04x}-0000-4000-8000-{n:012x}")
}

/// A live session row `n` in `state`, last seen and active at `seen`.
fn session(n: u32, state: &str, seen: UtcMs) -> Value {
    let id = id(n);
    let activity = if state == "working" {
        "working"
    } else {
        "idle"
    };
    json!({
        "key": id,
        "id": id,
        "agent_kind": "claude",
        "account": "acct-a",
        "cwd": "/h/private/lantern",
        "start_cwd": "/h/private/lantern",
        "project_id": null,
        "title": format!("session {n}"),
        "title_locked": false,
        "group_path": "Private",
        "lifecycle": "live",
        "activity": activity,
        "activity_at": seen,
        "activity_why": null,
        "last_prompt": null,
        "last_turn_end_at": null,
        "seen_at": null,
        "asleep_at": null,
        "archived_at": null,
        "location": "here",
        "resume_failed": false,
        "created_at": seen.0 - 60 * MINUTE,
        "last_seen_at": seen,
        "handle": &id[..8],
        "conversation": null,
        "project": null,
        "display_state": state,
        "why": "",
        "adopted": false,
    })
}

fn activity(n: i64, at: UtcMs) -> Value {
    json!({
        "key": n.to_string(),
        "id": n,
        "at": at,
        "who": "brain",
        "text": format!("row {n}"),
        "ref_kind": null,
        "ref_id": null,
    })
}

fn data(sessions: Vec<Value>, activity: Vec<Value>) -> Data {
    let mut data = Data::default();
    data.upsert(Topic::Sessions, sessions);
    data.upsert(Topic::Activity, activity);
    data
}

fn ago(ms: i64) -> UtcMs {
    UtcMs(NOW.0 - ms)
}

#[test]
fn text_changes_at_exact_deadlines() {
    let ctx = ctx();
    let size = Size { w: 80, h: 16 };

    // The tree: `5m` turns `6m` 50 s from now.
    let seen = ago(5 * MINUTE + 10 * SECOND);
    let d = data(vec![session(1, "idle", seen)], Vec::new());
    let screen = Panel::new(View::Tree).screen(&d, &ctx, size);
    assert_eq!(screen.wake_at, Some(UtcMs(NOW.0 + 50 * SECOND)));
    assert!(!screen.animate);

    // The plan's date changes at local midnight, if nothing changes sooner.
    let midnight = start_of_next_day(ctx.today, &ctx.tz);
    let screen = Panel::new(View::Plan).screen(&d, &ctx, size);
    let wake = screen.wake_at.expect("the plan wakes");
    assert!(wake <= midnight, "{wake:?} after {midnight:?}");

    // A row 1 s old in the picture's strip: its flash ends 3 s from now.
    let at = ago(SECOND);
    let d = data(
        vec![session(1, "idle", seen)],
        vec![activity(1, ago(30 * MINUTE)), activity(2, at)],
    );
    let screen = Panel::new(View::Picture).screen(&d, &ctx, Size { w: 80, h: 24 });
    assert_eq!(screen.wake_at, Some(UtcMs(at.0 + 4 * SECOND)));
    assert!(screen.animate, "the row flashes and the brand spins");

    // A row from a moment ahead flashes from its own time.
    let ahead = UtcMs(NOW.0 + 2 * SECOND);
    let d = data(Vec::new(), vec![activity(1, ahead)]);
    let screen = Panel::new(View::Picture).screen(&d, &ctx, Size { w: 80, h: 24 });
    assert_eq!(screen.wake_at, Some(ahead));
    assert!(!screen.animate);
}

#[test]
fn nothing_animates_off_screen() {
    let ctx = ctx();
    let size = Size { w: 80, h: 12 };

    // One working session above thirty idle ones, in the status panel.
    let mut sessions = vec![session(0, "working", ago(MINUTE))];
    sessions.extend((1..=30).map(|n| session(n, "idle", ago(i64::from(n) * 2 * MINUTE))));
    let d = data(sessions, Vec::new());
    let mut panel = Panel::new(View::Status);
    assert!(panel.screen(&d, &ctx, size).animate, "the spinner shows");
    panel.key(Key::Char('G'), &d, &ctx, size);
    let screen = panel.screen(&d, &ctx, size);
    assert!(
        screen
            .body
            .iter()
            .all(|(l, _)| !l.text().contains("session 0")),
        "scrolled out"
    );
    assert!(!screen.animate, "the spinner is off screen");
    panel.key(Key::Char('g'), &d, &ctx, size);
    assert!(panel.screen(&d, &ctx, size).animate, "back on screen");

    // A picture with nothing working or waiting and no fresh row is still.
    let mut idle: Vec<Value> = (1..=30)
        .map(|n| session(n, "idle", ago(i64::from(n) * 2 * MINUTE)))
        .collect();
    let d = data(idle.clone(), vec![activity(1, ago(10 * MINUTE))]);
    let screen = Panel::new(View::Picture).screen(&d, &ctx, Size { w: 80, h: 24 });
    assert!(!screen.animate);
    let brand = screen.brand.expect("the brand shows at 80 columns");
    assert_eq!(brand.text, "moa ●");

    // A waiting session anywhere spins the brand, whatever the tab shows.
    idle.push(session(31, "waiting", ago(MINUTE)));
    let d = data(idle, Vec::new());
    let mut panel = Panel::new(View::Picture);
    panel.key(Key::Char('5'), &d, &ctx, Size { w: 80, h: 24 });
    let screen = panel.screen(&d, &ctx, Size { w: 80, h: 24 });
    assert!(screen.animate);
    assert_eq!(screen.brand.expect("the brand").text, "moa ⠋");
}
