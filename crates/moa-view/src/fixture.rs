//! The Lantern fixture: the data every view test draws from. It is built by feeding JSON rows,
//! shaped as the daemon publishes them, through [`Data::upsert`], so it also tests decoding.

use std::path::PathBuf;

use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::model::SessionId;
use moa_core::publish::Topic;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::data::Data;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

/// The fixture's zone: New York's rules, written out.
const TZ: &str = "EST5EDT,M3.2.0,M11.1.0";

/// The fixture's session `n` (1 to 11, in the order the fixture lists them): its moa id.
pub fn session(n: u8) -> SessionId {
    let id = format!("5e5500{n:02x}-0000-4000-8000-0000000000{n:02x}");
    SessionId(uuid::Uuid::parse_str(&id).expect("a fixture session id is a uuid"))
}

fn tz() -> TimeZone {
    TimeZone::posix(TZ).expect("the fixture zone parses")
}

/// A local wall time in the fixture's zone.
fn local(day: Date, hour: i8, minute: i8) -> UtcMs {
    let at = day.at(hour, minute, 0, 0);
    let ms = tz()
        .to_ambiguous_zoned(at)
        .compatible()
        .expect("a fixture time is in range")
        .timestamp()
        .as_millisecond();
    UtcMs(ms)
}

/// Fri 13 Nov 2026, 10:00.
fn now() -> UtcMs {
    local(date(2026, 11, 13), 10, 0)
}

/// One fixture session, as a `sessions` row.
struct S {
    title: &'static str,
    group: Option<&'static str>,
    project: Option<(i64, &'static str, &'static str)>,
    /// The display state and the core's why text.
    state: &'static str,
    why: &'static str,
    account: &'static str,
    adopted: bool,
}

const LANTERN: Option<(i64, &str, &str)> = Some((1, "Lantern", "/h/private/lantern"));
const KITE: Option<(i64, &str, &str)> = Some((2, "Kite", "/h/private/kite"));
const HARBOR: Option<(i64, &str, &str)> = Some((3, "Harbor", "/h/private/harbor"));

fn s(
    title: &'static str,
    group: Option<&'static str>,
    project: Option<(i64, &'static str, &'static str)>,
    state: &'static str,
    why: &'static str,
) -> S {
    S {
        title,
        group,
        project,
        state,
        why,
        account: "acct-a",
        adopted: false,
    }
}

fn sessions() -> Vec<S> {
    vec![
        s(
            "Fix login",
            Some("Private/Lantern"),
            LANTERN,
            "waiting",
            "Bash: cargo test ⚠️",
        ),
        s(
            "Rust scaffold",
            Some("Private/Lantern"),
            LANTERN,
            "working",
            "",
        ),
        s(
            "Release notes",
            Some("Private/Kite"),
            KITE,
            "question",
            "Ship it today?",
        ),
        s("設計レビュー", Some("Private/Kite"), KITE, "done", ""),
        s("Dev server 👍🏽", Some("Private/Harbor"), HARBOR, "idle", ""),
        S {
            account: "acct-b",
            ..s(
                "Migrate DB",
                Some("Work/Harbor"),
                HARBOR,
                "failed",
                "API error",
            )
        },
        s(
            "Import the whole archive of old notes into the new format",
            Some("Private/Lantern"),
            None,
            "quiet",
            "quiet 361m",
        ),
        S {
            adopted: true,
            ..s("Hand started", Some("Private/Harbor"), None, "idle", "")
        },
        s("Old spike", None, None, "asleep", ""),
        s("Closed thing 👨‍👩‍👧", Some("Private/Kite"), None, "ended", ""),
        s("Gone", None, None, "archived", ""),
    ]
}

/// The row the daemon would publish for session `n`, seen `seen` and created two hours before.
fn session_row(n: u8, s: &S, seen: UtcMs) -> Value {
    let id = session(n).to_string();
    let cwd = s.project.map_or("/h/private", |(_, _, path)| path);
    // The stored fields agree with the display state the core derived from them.
    let (lifecycle, activity, activity_why) = match s.state {
        "waiting" => ("live", "needs_permission", Some(s.why)),
        "question" => ("live", "needs_answer", Some(s.why)),
        "failed" => ("live", "failed", Some(s.why)),
        "working" | "quiet" => ("live", "working", None),
        "done" | "idle" => ("live", "idle", None),
        _ => ("ended", "idle", None),
    };
    let (turn_end, seen_at) = match s.state {
        // The turn ended after it was seen.
        "done" => (Some(seen.0), Some(seen.0 - MINUTE)),
        "idle" => (Some(seen.0 - MINUTE), Some(seen.0)),
        _ => (None, None),
    };
    // Working, not quiet: its last activity was two minutes ago.
    let activity_at = if s.state == "working" {
        now().0 - 2 * MINUTE
    } else {
        seen.0
    };
    json!({
        "key": id,
        "id": id,
        "agent_kind": "claude",
        "account": s.account,
        "cwd": cwd,
        "start_cwd": cwd,
        "project_id": s.project.map(|(pid, _, _)| pid),
        "title": s.title,
        "title_locked": false,
        "group_path": s.group,
        "lifecycle": lifecycle,
        "activity": activity,
        "activity_at": activity_at,
        "activity_why": activity_why,
        "last_prompt": (s.state == "working").then_some("split the store"),
        "last_turn_end_at": turn_end,
        "seen_at": seen_at,
        "asleep_at": (s.state == "asleep").then_some(seen.0),
        "archived_at": (s.state == "archived").then_some(seen.0),
        "location": "here",
        "resume_failed": false,
        "created_at": seen.0 - 2 * HOUR,
        "last_seen_at": seen.0,
        "handle": &id[..8],
        "conversation": null,
        "project": s.project.map(|(_, name, _)| name),
        "display_state": s.state,
        "why": s.why,
        "adopted": s.adopted,
    })
}

fn project(id: i64, name: &str, path: Option<&str>, status: &str, last: Option<Date>) -> Value {
    let created = local(date(2026, 10, 1), 9, 0).0;
    json!({
        "key": id.to_string(),
        "id": id,
        "name": name,
        "path": path,
        "status": status,
        "summary": null,
        "last_activity_on": last,
        "created_at": created,
        "updated_at": created,
    })
}

struct T {
    project: Option<i64>,
    title: &'static str,
    status: &'static str,
    who: Option<&'static str>,
    priority: Option<&'static str>,
    due: Option<Date>,
}

fn t(project: Option<i64>, title: &'static str, who: Option<&'static str>, due: Option<Date>) -> T {
    T {
        project,
        title,
        status: "todo",
        who,
        priority: None,
        due,
    }
}

fn task_row(id: i64, t: &T, extra: &[(&str, Value)]) -> Value {
    let created = local(date(2026, 11, 2), 9, 0).0;
    let mut row = json!({
        "key": id.to_string(),
        "id": id,
        "project_id": t.project,
        "title": t.title,
        "status": t.status,
        "blocked_on": null,
        "today": null,
        "session_id": null,
        "note": null,
        "who": t.who,
        "due": t.due,
        "priority": t.priority,
        "created_at": created,
        "updated_at": created,
        "done_at": null,
    });
    for (k, v) in extra {
        row[*k] = v.clone();
    }
    row
}

fn tasks() -> Vec<Value> {
    let today = date(2026, 11, 13);
    let (lantern, kite) = (Some(1), Some(2));
    vec![
        task_row(
            1,
            &T {
                priority: Some("p0"),
                ..t(
                    lantern,
                    "Fix the login loop",
                    Some("you"),
                    Some(date(2026, 11, 12)),
                )
            },
            &[("today", json!(today))],
        ),
        task_row(
            2,
            &T {
                status: "active",
                ..t(
                    lantern,
                    "Write the README",
                    Some("agent"),
                    Some(date(2026, 11, 13)),
                )
            },
            &[("session_id", json!(session(2)))],
        ),
        task_row(
            3,
            &T {
                status: "waiting",
                ..t(lantern, "Wait for review", None, Some(date(2026, 11, 14)))
            },
            &[("blocked_on", json!("design team"))],
        ),
        task_row(
            4,
            &T {
                priority: Some("p1"),
                ..t(kite, "Pick a cover", Some("you"), Some(date(2026, 11, 17)))
            },
            &[],
        ),
        task_row(
            5,
            &t(kite, "Order copies", Some("other"), Some(date(2026, 12, 3))),
            &[],
        ),
        task_row(
            6,
            &t(
                kite,
                "Write the back-cover text for both the paperback and the e-book editions",
                Some("you"),
                None,
            ),
            &[],
        ),
        task_row(
            7,
            &T {
                status: "done",
                ..t(kite, "Proofread chapter 1", None, None)
            },
            &[("done_at", json!(local(today, 9, 0)))],
        ),
        task_row(8, &t(None, "Renew the domain", Some("other"), None), &[]),
    ]
}

fn suggestion(id: i64, text: &str, status: &str, created: UtcMs) -> Value {
    json!({
        "key": id.to_string(),
        "id": id,
        "text": text,
        "status": status,
        "actions": [],
        "actions_raw": null,
        "actions_invalid": null,
        "result": null,
        "created_at": created,
        "resolved_at": null,
    })
}

fn activity(id: i64, ago: i64, who: &str, text: &str, of: Option<(&str, String)>) -> Value {
    let (ref_kind, ref_id) = of.map_or((None, None), |(k, v)| (Some(k), Some(v)));
    json!({
        "key": id.to_string(),
        "id": id,
        "at": now().0 - ago,
        "who": who,
        "text": text,
        "ref_kind": ref_kind,
        "ref_id": ref_id,
    })
}

/// The fixture: three projects with folders and an idea, eleven sessions in every display
/// state, eight tasks, two suggestions, an hour of activity, one brain and the four preset
/// layouts, at Fri 13 Nov 2026 10:00 in New York time.
pub fn lantern() -> (Data, Ctx) {
    let now = now();
    let mut data = Data::default();

    let rows = sessions()
        .iter()
        .zip(1u8..)
        .map(|(s, n)| {
            let seen = UtcMs(now.0 - MINUTE - i64::from(n - 1) * HOUR);
            session_row(n, s, seen)
        })
        .collect();
    data.upsert(Topic::Sessions, rows);

    data.upsert(
        Topic::Groups,
        vec![json!({
            "key": "Private/Ideas",
            "path": "Private/Ideas",
            "collapsed": false,
            "created_at": local(date(2026, 11, 1), 9, 0),
        })],
    );

    let today = date(2026, 11, 13);
    data.upsert(
        Topic::Projects,
        vec![
            {
                let mut lantern = project(
                    1,
                    "Lantern",
                    Some("/h/private/lantern"),
                    "active",
                    Some(today),
                );
                lantern["summary"] = json!("A small lamp for the terminal: the Rust rewrite");
                lantern
            },
            project(2, "Kite", Some("/h/private/kite"), "active", Some(today)),
            project(
                3,
                "Harbor",
                Some("/h/private/harbor"),
                "active",
                Some(date(2026, 11, 10)),
            ),
            project(4, "Beacon", None, "idea", None),
        ],
    );

    data.upsert(Topic::Tasks, tasks());

    data.upsert(
        Topic::Suggestions,
        vec![
            suggestion(
                1,
                "Archive 3 ended sessions?",
                "open",
                UtcMs(now.0 - 20 * MINUTE),
            ),
            suggestion(2, "Set Kite's due date", "later", UtcMs(now.0 - 26 * HOUR)),
        ],
    );

    let sid = |n: u8| Some(("session", session(n).to_string()));
    let project_ref = |id: i64| Some(("project", id.to_string()));
    data.upsert(
        Topic::Activity,
        vec![
            activity(
                1,
                58 * MINUTE,
                "brain",
                "new task \"Write the README\" in Lantern",
                project_ref(1),
            ),
            activity(
                2,
                51 * MINUTE,
                "you",
                "added to today: \"Fix the login loop\"",
                None,
            ),
            activity(
                3,
                44 * MINUTE,
                "session",
                "linked session 5e550002 → Lantern",
                sid(2),
            ),
            activity(
                4,
                33 * MINUTE,
                "you",
                "task \"Proofread chapter 1\": active → done",
                None,
            ),
            activity(
                5,
                20 * MINUTE,
                "brain",
                "suggests #1: Archive 3 ended sessions?",
                None,
            ),
            activity(
                6,
                12 * MINUTE,
                "session",
                "Release notes asks: Ship it today?",
                sid(3),
            ),
            activity(
                7,
                4 * MINUTE,
                "you",
                "project Beacon: active → idea",
                project_ref(4),
            ),
            activity(8, 2_000, "session", "Fix login needs you", sid(1)),
        ],
    );

    data.upsert(
        Topic::Brain,
        vec![json!({
            "key": "brain",
            "name": "default",
            "folders": ["/h/private"],
            "accounts": ["acct-a", "acct-b"],
            "open_account": "acct-a",
            "folded": [],
        })],
    );

    // The presets, by name as the daemon keys them; the current one is the cockpit.
    let layout = |name: &str, index: usize, leaves: &[&str]| {
        json!({
            "key": name,
            "name": name,
            "index": index,
            "leaves": leaves,
            "current": name == "cockpit",
            "error": null,
        })
    };
    data.upsert(
        Topic::Layouts,
        vec![
            layout("classic", 2, &["console", "plan", "tree"]),
            layout("cockpit", 0, &["tree", "console", "plan"]),
            layout("focus", 1, &["tree", "plan", "agent", "console"]),
            layout("triage", 3, &["status", "console", "plan"]),
        ],
    );

    let ctx = Ctx::at(now, tz(), 3, PathBuf::from("/h"), |_| true);
    (data, ctx)
}

/// Four terminals for the terminals view, as the daemon publishes them, as of the fixture's
/// `now`: `dev server` (in front of you, `node` for 2 hours, with a note and a timeout 90
/// minutes away), `kite` (a shell touched 10 minutes ago), `harbor logs` (`tail` in a session's
/// tab, untouched for 5 hours: idle) and `scratch` (a shell never touched, open for 3 hours:
/// idle). Their rows are in neither the picker's nor any other view's fixture.
pub fn terminals(data: &mut Data, ctx: &Ctx) {
    let at = |ago: i64| ctx.now.0 - ago;
    let term = |pane: &str, id: i64, name: &str, folder: &str| {
        json!({
            "key": pane, "name": name, "id": id, "folder": folder,
            "command": "zsh", "running": false, "since": at(0), "opened_at": at(HOUR),
            "touched_at": null, "active": false, "opened_by": "you",
            "session": null, "note": null, "timeout_at": null,
        })
    };
    let mut dev = term("%5", 1, "dev server", "/h/private/lantern");
    dev["command"] = json!("node");
    dev["running"] = json!(true);
    dev["since"] = json!(at(2 * HOUR));
    dev["opened_at"] = json!(at(5 * HOUR));
    dev["touched_at"] = json!(at(3 * MINUTE));
    dev["active"] = json!(true);
    dev["note"] = json!("port 3000");
    dev["timeout_at"] = json!(ctx.now.0 + 90 * MINUTE);
    let mut kite = term("%7", 2, "kite", "/h/private/kite");
    kite["opened_at"] = json!(at(24 * HOUR));
    kite["touched_at"] = json!(at(10 * MINUTE));
    let mut harbor = term("%9", 3, "harbor logs", "/h/private/harbor");
    harbor["command"] = json!("tail");
    harbor["running"] = json!(true);
    harbor["since"] = json!(at(6 * HOUR));
    harbor["opened_at"] = json!(at(48 * HOUR));
    harbor["touched_at"] = json!(at(5 * HOUR));
    harbor["opened_by"] = json!("moa");
    harbor["session"] = json!("Fix login");
    harbor["note"] = json!("watching the deploy");
    let mut scratch = term("%11", 4, "scratch", "/h/private/beacon");
    scratch["opened_at"] = json!(at(3 * HOUR));
    data.upsert(Topic::Terms, vec![dev, kite, harbor, scratch]);
}
