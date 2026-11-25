//! The session reports queue in the core: which hook events queue a report, what the row
//! holds, and the one channel that carries them to moa's agent: a wake-up once they settle,
//! or your next prompt. Every input goes through `handle_checked`.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::central::{
    Disk, acps, body_of, caps, commits, end_turn, fire, hear_at, opened, play, prompt_at,
    prompts_sent, sent, started as up, wake_timer, you_commit, you_row,
};
use jiff::ToSpan;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::central::carry::YES_MAX;
use moa_core::central::channel::{AUTO_KEY, auto_on};
use moa_core::central::reports::{EARLIER_MAX, LAST_MAX, TURNS_KEPT};
use moa_core::central::wire::{AcpCmd, AcpIn, Opened, PermKind, PermOption, ThreadHow};
use moa_core::central::{PROMPT_MAX, Phase};
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, RefuseWrites, TimerKey, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, Project, ProjectId,
    ProjectStatus, Report, ReportKind, Session, SessionId, Suggestion, SuggestionAction,
    SuggestionId, SuggestionStatus, Task, TaskId, TaskStatus, YES_DONE, YES_TOLD, YES_WAITING,
};
use moa_core::op::{Answer, ConnId, Done, Op, OpError, PermAnswer, ReqId, TaskSet};
use moa_core::publish::Topic;
use moa_core::state::{Deps, LoadNote, Snapshot, State};
use moa_core::time::{HOUR, start_of_next_day};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use proptest::prelude::*;
use serde_json::Value;

const S: &str = "0195f3a2-1111-7000-8000-000000000001";
const C: &str = "a1b2c300-0000-4000-8000-000000000001";
const T0: i64 = 1_800_000_000_000;

fn now() -> Now {
    Now::at(
        UtcMs(T0 + 600_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn sid() -> SessionId {
    S.parse().unwrap()
}

/// Session `n`'s id: [`S`] for the first.
fn sid_of(n: usize) -> SessionId {
    format!("0195f3a2-1111-7000-8000-{:012x}", n + 1)
        .parse()
        .unwrap()
}

/// Session `n`'s conversation: [`C`] for the first. Each has a handle of its own.
fn conv_of(n: usize) -> String {
    format!("a1b2c3{n:02x}-0000-4000-8000-000000000001")
}

/// Session `n`, titled `title`, in `cwd`, idle since `T0`, and its conversation.
fn session(n: usize, title: Option<&str>, cwd: &str) -> (Session, Conversation) {
    let session = Session {
        id: sid_of(n),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.into()),
        start_cwd: Some(cwd.into()),
        project_id: None,
        title: title.map(str::to_string),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(T0)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(UtcMs(T0)),
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(T0)),
    };
    let conversation = Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conv_of(n),
        session_id: sid_of(n),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    };
    (session, conversation)
}

/// A state under `config` and `deps`, started at `now` from `snapshot`, and what its start
/// said.
fn load(config: Config, deps: Deps, mut snapshot: Snapshot, now: Now) -> (State, Vec<Effect>) {
    common::central::opened_today(&mut snapshot, &now);
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    let fx = handle_checked(&mut st, Input::Startup(Box::new(snapshot)), now);
    (st, fx)
}

/// Lantern's session in `cwd`, idle since `T0`, started with `deps`.
fn started(cwd: &str, deps: Deps) -> State {
    let (session, conversation) = session(0, None, cwd);
    let snapshot = Snapshot {
        sessions: vec![session],
        conversations: vec![conversation],
        ..Snapshot::default()
    };
    load(Config::default(), deps, snapshot, now()).0
}

fn lantern() -> State {
    started("/work/Lantern", Deps::default())
}

fn hook(seq: u32, plus_ms: i64, kind: HookKind) -> HookEnvelope {
    common::envelope(S, C, UtcMs(T0 + plus_ms), seq, kind)
}

fn turn(seq: u32, plus_ms: i64, text: &str) -> HookEnvelope {
    hook(
        seq,
        plus_ms,
        HookKind::TurnEnded {
            last_message: text.into(),
        },
    )
}

fn feed(st: &mut State, envelope: HookEnvelope) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Hook {
            reply: None,
            envelope,
        },
        now(),
    )
}

/// The reports `fx` saves.
fn saved(fx: &[Effect]) -> Vec<Report> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Report(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn waiting(st: &State) -> Vec<Report> {
    st.reports().cloned().collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_finished_turn_queues_a_report() {
    let mut st = lantern();
    let env = turn(1, 1_000, "Merged PR #3");
    let fx = feed(&mut st, env.clone());
    // One commit holds the session and its report.
    let persists = commits(&fx);
    assert_eq!(persists.len(), 1);
    assert!(matches!(persists[0][0], Change::Session(_)));
    let reports = saved(&fx);
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!((r.session_id, r.kind), (sid(), ReportKind::TurnEnded));
    assert_eq!(r.turns, strs(&["Merged PR #3"]));
    assert_eq!((r.queued_at, r.last_at), (env.at, env.at));
    assert_eq!(waiting(&st), reports);
}

#[test]
fn six_turns_are_kept() {
    let mut st = lantern();
    for n in 1..=8u32 {
        feed(&mut st, turn(n, 1_000 * i64::from(n), &format!("turn {n}")));
    }
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(TURNS_KEPT, 6);
    assert_eq!(
        rows[0].turns,
        strs(&["turn 3", "turn 4", "turn 5", "turn 6", "turn 7", "turn 8"])
    );
    assert_eq!(rows[0].queued_at, UtcMs(T0 + 1_000));
    assert_eq!(rows[0].last_at, UtcMs(T0 + 8_000));
}

#[test]
fn needs_you_keeps_the_latest() {
    let mut st = lantern();
    feed(
        &mut st,
        hook(
            1,
            1_000,
            HookKind::NeedsPermission {
                tool: "Bash".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows[0].kind, ReportKind::NeedsYou);
    assert_eq!(rows[0].turns, strs(&["permission: Bash"]));
    feed(
        &mut st,
        hook(
            2,
            2_000,
            HookKind::NeedsAnswer {
                what: "Which branch?".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turns, strs(&["Which branch?"]));
    assert_eq!(rows[0].queued_at, UtcMs(T0 + 1_000));
    assert_eq!(rows[0].last_at, UtcMs(T0 + 2_000));
}

#[test]
fn a_resolved_need_leaves_the_queue() {
    let mut st = lantern();
    let needs = HookKind::NeedsPermission {
        tool: "Bash".into(),
    };
    feed(&mut st, hook(1, 1_000, needs));
    let id = waiting(&st)[0].id;
    // The permission is given: the session works again, and the need goes in that commit.
    let fx = feed(&mut st, hook(2, 2_000, HookKind::NeedsCleared));
    assert!(waiting(&st).is_empty());
    let commits = commits(&fx);
    assert_eq!(commits.len(), 1);
    assert!(matches!(&commits[0][0], Change::Session(s) if s.activity == Activity::Working));
    assert!(commits[0].contains(&Change::DeleteReport(id)));
}

#[test]
fn a_turn_after_a_question_leaves_only_the_turn() {
    let mut st = lantern();
    let needs = HookKind::NeedsAnswer {
        what: "Which branch?".into(),
    };
    feed(&mut st, hook(1, 1_000, needs));
    feed(&mut st, turn(2, 2_000, "Went with main"));
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, ReportKind::TurnEnded);
    assert_eq!(rows[0].turns, strs(&["Went with main"]));
}

#[test]
fn a_need_after_a_need_still_waits() {
    let mut st = lantern();
    let permission = HookKind::NeedsPermission {
        tool: "Bash".into(),
    };
    let question = HookKind::NeedsAnswer {
        what: "Which branch?".into(),
    };
    feed(&mut st, hook(1, 1_000, permission));
    let fx = feed(&mut st, hook(2, 2_000, question));
    assert!(
        !common::changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::DeleteReport(_)))
    );
    let rows = waiting(&st);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, ReportKind::NeedsYou);
    assert_eq!(rows[0].turns, strs(&["Which branch?"]));
}

#[test]
fn a_turn_and_a_question_queue_apart() {
    let mut st = lantern();
    feed(&mut st, turn(1, 1_000, "Done"));
    feed(
        &mut st,
        hook(
            2,
            2_000,
            HookKind::NeedsAnswer {
                what: "Which branch?".into(),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        [ReportKind::TurnEnded, ReportKind::NeedsYou]
    );
    assert_ne!(rows[0].id, rows[1].id);
    assert!(rows.iter().all(|r| r.session_id == sid()));
}

#[test]
fn a_long_turn_is_cut_to_2600() {
    let mut st = lantern();
    assert_eq!((LAST_MAX, EARLIER_MAX), (2600, 500));
    // Exactly the limit is kept whole; one more is cut, and says so.
    feed(&mut st, turn(1, 1_000, &"é".repeat(2_600)));
    assert_eq!(waiting(&st)[0].turns, vec!["é".repeat(2_600)]);
    feed(&mut st, turn(2, 2_000, &"é".repeat(5_000)));
    let turns = waiting(&st)[0].turns.clone();
    // The first is an earlier turn now: cut to 500.
    assert_eq!(turns[0], format!("{}…", "é".repeat(500)));
    assert_eq!(turns[1], format!("{}…", "é".repeat(2_600)));
    // An earlier turn of exactly 500 is kept whole.
    let mut st = lantern();
    feed(&mut st, turn(1, 1_000, &"a".repeat(500)));
    feed(&mut st, turn(2, 2_000, "next"));
    assert_eq!(
        waiting(&st)[0].turns,
        vec!["a".repeat(500), "next".to_string()]
    );
    // What a session needs is cut the same way.
    feed(
        &mut st,
        hook(
            3,
            3_000,
            HookKind::NeedsAnswer {
                what: "x".repeat(5_000),
            },
        ),
    );
    let rows = waiting(&st);
    assert_eq!(rows[1].turns, vec![format!("{}…", "x".repeat(2_600))]);
}

#[test]
fn not_every_event_is_a_report() {
    // An empty (or blank) last message says nothing.
    let mut st = lantern();
    let fx = feed(&mut st, turn(1, 1_000, "  \n "));
    assert!(saved(&fx).is_empty());
    // A duplicate hook is applied once.
    let fx = feed(&mut st, turn(2, 2_000, "Done"));
    assert_eq!(saved(&fx).len(), 1);
    let fx = feed(&mut st, turn(2, 2_000, "Done"));
    assert!(saved(&fx).is_empty());
    assert_eq!(waiting(&st)[0].turns, strs(&["Done"]));
    // An event older than the session's last activity is history only.
    let mut st = lantern();
    let fx = feed(&mut st, turn(1, -1_000, "Old news"));
    assert!(saved(&fx).is_empty());
    // Its history row was saved, so it was history only, not dropped.
    assert!(
        common::changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Event(_)))
    );
    assert!(waiting(&st).is_empty());
    // Starting, a failure and a cleared need say nothing to moa either.
    for (seq, kind) in [
        (
            2,
            HookKind::TurnFailed {
                error: "boom".into(),
            },
        ),
        (3, HookKind::NeedsCleared),
        (
            4,
            HookKind::TurnStarted {
                prompt: "go".into(),
            },
        ),
    ] {
        assert!(saved(&feed(&mut st, hook(seq, 1_000 * i64::from(seq), kind))).is_empty());
    }
}

#[test]
fn moas_own_folder_queues_nothing() {
    let deps = || Deps {
        agent_dir: PathBuf::from("/work/moa-agent"),
        ..Deps::default()
    };
    for cwd in ["/work/moa-agent", "/work/moa-agent/notes"] {
        let mut st = started(cwd, deps());
        let fx = feed(&mut st, turn(1, 1_000, "Done"));
        assert!(saved(&fx).is_empty(), "{cwd}");
    }
    // A sibling whose name starts the same isn't under it.
    let mut st = started("/work/moa-agent-2", deps());
    assert_eq!(saved(&feed(&mut st, turn(1, 1_000, "Done"))).len(), 1);
}

#[test]
fn the_state_row_counts_them() {
    let mut st = lantern();
    let queued = |st: &State| st.model().topics[&Topic::Feed]["state"]["queued"].clone();
    assert_eq!(queued(&st), 0);
    feed(&mut st, turn(1, 1_000, "Done"));
    feed(&mut st, turn(2, 2_000, "Again"));
    assert_eq!(queued(&st), 1);
    feed(
        &mut st,
        hook(
            3,
            3_000,
            HookKind::NeedsPermission {
                tool: "Bash".into(),
            },
        ),
    );
    // Rows, not sessions.
    assert_eq!(queued(&st), 2);
}

#[test]
fn reports_come_in_line_order() {
    // Spooled hooks from several sessions arrive out of order: ids don't follow `queued_at`.
    let report = |id, queued_at| Report {
        id,
        session_id: sid(),
        kind: ReportKind::TurnEnded,
        queued_at: UtcMs(queued_at),
        last_at: UtcMs(queued_at),
        turns: vec![format!("report {id}")],
    };
    let mut st = lantern();
    handle_checked(
        &mut st,
        Input::Startup(Box::new(Snapshot {
            reports: vec![report(1, 5_000), report(2, 3_000), report(3, 3_000)],
            ..Snapshot::default()
        })),
        now(),
    );
    assert_eq!(
        waiting(&st).iter().map(|r| r.id).collect::<Vec<_>>(),
        [2, 3, 1]
    );
}

// ---------- the one channel: wake-ups, and reports riding on your prompt ----------

const HOME: &str = "/tmp/moa-reports-home";
const SEC: i64 = 1_000;
/// The first report: after every session's last activity.
const T: i64 = T0 + SEC;
const WAKE: &str = "(moa · a wake-up, not typed by the user) ";
const TITLES: [&str; 10] = [
    "Lantern",
    "Kite",
    "Harbor",
    "Beacon",
    "Lantern 2",
    "Kite 2",
    "Harbor 2",
    "Beacon 2",
    "Lantern 3",
    "Kite 3",
];

fn at(ms: i64) -> Now {
    common::at(UtcMs(ms))
}

fn kite_project() -> Project {
    Project {
        id: ProjectId(1),
        name: "Kite".into(),
        path: None,
        status: ProjectStatus::Active,
        summary: None,
        last_activity_on: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
    }
}

/// One session per title (session `n` titled `titles[n]`), idle since `T0`, the second in
/// project Kite; and `meta`.
fn moa_snapshot(titles: &[&str], meta: Vec<(String, String)>) -> Snapshot {
    let (mut sessions, mut conversations) = (Vec::new(), Vec::new());
    for (n, title) in titles.iter().enumerate() {
        let (mut s, c) = session(n, Some(title), "/work/project");
        if n == 1 {
            s.project_id = Some(ProjectId(1));
        }
        sessions.push(s);
        conversations.push(c);
    }
    Snapshot {
        sessions,
        conversations,
        projects: vec![kite_project()],
        meta,
        ..Snapshot::default()
    }
}

/// moa's agent on `acct-a`, with `settings` (TOML, `""` for `[moa]` at its defaults), started
/// at `T0` from `snapshot`; what the start said too.
fn moa_load(snapshot: Snapshot, settings: &str) -> (State, Vec<Effect>) {
    let toml = format!("{}{settings}", common::central::toml("acct-a"));
    let config = Config::parse(&toml, Path::new(HOME)).unwrap();
    let deps = Deps {
        home: PathBuf::from(HOME),
        ..Deps::default()
    };
    load(config, deps, snapshot, at(T0))
}

/// [`moa_load`] of [`moa_snapshot`], at the default settings.
fn moa_brain_with(titles: &[&str], meta: Vec<(String, String)>) -> (State, Vec<Effect>) {
    moa_load(moa_snapshot(titles, meta), "")
}

/// [`moa_brain_with`] the first `n` of [`TITLES`].
fn moa_brain(n: usize) -> State {
    moa_brain_with(&TITLES[..n], vec![]).0
}

static SEQ: AtomicU32 = AtomicU32::new(1);

/// Session `n`'s hook of `kind`, at `ms`, as the core hears it then.
fn hook_at(st: &mut State, n: usize, ms: i64, kind: HookKind) -> Vec<Effect> {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let slot = sid_of(n).to_string();
    let envelope = common::envelope(&slot, &conv_of(n), UtcMs(ms), seq, kind);
    handle_checked(
        st,
        Input::Hook {
            reply: None,
            envelope,
        },
        at(ms),
    )
}

/// Session `n` finishes a turn at `ms`, its last message `text`.
fn finished(st: &mut State, n: usize, ms: i64, text: &str) -> Vec<Effect> {
    let kind = HookKind::TurnEnded {
        last_message: text.into(),
    };
    hook_at(st, n, ms, kind)
}

fn queued(st: &State) -> Value {
    st.model().topics[&Topic::Feed]["state"]["queued"].clone()
}

/// From `ms` until `until` the store refuses writes; the core reloads what it saved.
fn refuse_writes(st: &mut State, ms: i64, until: i64) -> Vec<Effect> {
    let snapshot = Snapshot {
        sessions: st.sessions().cloned().collect(),
        conversations: st
            .sessions()
            .flat_map(|s| st.conversations_of(s.id))
            .cloned()
            .collect(),
        projects: st.projects().cloned().collect(),
        reports: st.reports().cloned().collect(),
        threads: st.threads().cloned().collect(),
        ..Snapshot::default()
    };
    handle_checked(
        st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(until),
                reason: "the disk is full".into(),
            }),
        },
        at(ms),
    )
}

#[test]
fn a_report_waits_for_it_to_settle() {
    let mut st = moa_brain(1);
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(T + 45 * SEC))));
    let fx = fire(&mut st, T + 44 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(T + 45 * SEC))));
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let fx = play(&mut st, T + 45 * SEC, fx);
    let text = sent(&fx);
    let head = format!("{WAKE}1 session report(s) since your last look:\n\n### Lantern · ");
    assert!(text.starts_with(&head), "{text}");
    assert_eq!(st.reports().count(), 0);
    assert_eq!(queued(&st), 0);
}

#[test]
fn a_steady_stream_still_wakes_after_the_gap() {
    let mut st = moa_brain(1);
    for k in 0..6 {
        let fx = finished(&mut st, 0, T + 30 * k * SEC, &format!("turn {k}"));
        // 45 s after the last, until the oldest has waited the whole gap.
        let due = (30 * k + 45).min(180);
        assert_eq!(wake_timer(&fx), Some(Some(UtcMs(T + due * SEC))), "{k}");
    }
    let fx = fire(&mut st, T + 180 * SEC);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
}

#[test]
fn wakes_are_180_s_apart() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "the first news");
    let w = T + 45 * SEC;
    let fx = fire(&mut st, w);
    let fx = play(&mut st, w, fx);
    assert!(sent(&fx).starts_with(WAKE));
    end_turn(&mut st, w + 5 * SEC);
    let fx = finished(&mut st, 0, w + 10 * SEC, "the second news");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(w + 180 * SEC))));
    // moa's agent runs, its thread open: the wake goes at once.
    let fx = fire(&mut st, w + 180 * SEC);
    let text = sent(&fx);
    assert!(text.contains("the second news"), "{text}");
    assert!(!text.contains("the first news"), "{text}");
}

#[test]
fn a_wake_takes_8_sessions() {
    let mut st = moa_brain(10);
    for (n, title) in TITLES.iter().enumerate() {
        finished(
            &mut st,
            n,
            T + n as i64 * SEC,
            &format!("news from {title}"),
        );
    }
    // 45 s after the last.
    let w = T + 54 * SEC;
    let fx = fire(&mut st, w);
    let fx = play(&mut st, w, fx);
    let text = sent(&fx);
    assert!(
        text.starts_with(&format!("{WAKE}8 session report(s)")),
        "{text}"
    );
    let headings = text.lines().filter(|l| l.starts_with("### ")).count();
    assert_eq!(headings, 8);
    for title in &TITLES[..8] {
        assert!(text.contains(&format!("### {title} · ")), "{title}");
    }
    for title in &TITLES[8..] {
        assert!(!text.contains(&format!("### {title} · ")), "{title}");
    }
    assert_eq!(queued(&st), 2);
}

#[test]
fn a_newer_report_keeps_its_place() {
    let mut st = moa_brain(2);
    finished(&mut st, 0, T, "Lantern's first");
    finished(&mut st, 1, T + 10 * SEC, "Kite's");
    finished(&mut st, 0, T + 20 * SEC, "Lantern's second");
    let w = T + 65 * SEC;
    let fx = fire(&mut st, w);
    let text = sent(&play(&mut st, w, fx));
    let lantern = text.find("### Lantern · ").unwrap();
    let kite = text.find("### Kite · ").unwrap();
    assert!(lantern < kite, "{text}");
}

#[test]
fn the_wake_text() {
    let mut st = moa_brain(2);
    let long = |word: &str| format!("{word} ").repeat(120);
    finished(&mut st, 0, T, &long("first"));
    finished(&mut st, 0, T + 60 * SEC, &long("second"));
    let what = "Which branch should I push to?".to_string();
    hook_at(&mut st, 1, T + 90 * SEC, HookKind::NeedsAnswer { what });
    finished(
        &mut st,
        0,
        T + 120 * SEC,
        "Merged PR #3; the release notes are next.",
    );
    let w = T + 165 * SEC;
    let fx = fire(&mut st, w);
    let text = sent(&play(&mut st, w, fx));
    // Each earlier turn is its first 500 characters, then `…`; each turn is quoted.
    let earlier = format!("> (earlier) {}…", &long("first")[..500]);
    assert!(text.contains(&format!("{earlier}\n\n")), "{text}");
    insta::assert_snapshot!(text);
}

#[test]
fn reports_ride_on_your_prompt() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let id = st.reports().next().unwrap().id;
    let fx = prompt_at(&mut st, at(T + SEC), 1, "hi", false);
    let fx = play(&mut st, T + SEC, fx);
    // One commit saves the row and deletes the report it carries, before the prompt goes.
    let saves_you = |e: &Effect| match e {
        Effect::Persist(c) => c
            .iter()
            .any(|c| matches!(c, Change::Feed(r) if r.kind == "you")),
        _ => false,
    };
    let saved = fx.iter().position(saves_you).expect("the you row is saved");
    let Effect::Persist(changes) = &fx[saved] else {
        unreachable!()
    };
    assert!(changes.contains(&Change::DeleteReport(id)), "{changes:?}");
    let sent_at = fx
        .iter()
        .position(|e| matches!(e, Effect::Acp(AcpCmd::Prompt { .. })))
        .unwrap();
    assert!(saved < sent_at);
    let you = you_row(&fx);
    assert!(!you.automatic);
    let body = body_of(you);
    assert_eq!(body["text"], "hi");
    assert_eq!(body["reports"], 1);
    let carried = body["carried"].as_str().unwrap();
    let head = "(moa · 1 session report(s) since your last look, sent with the user's message)\n\n### Lantern · ";
    assert!(carried.starts_with(head), "{carried}");
    let text = sent(&fx);
    assert!(text.ends_with("\n\n---\n\nhi"), "{text}");
    assert_eq!(text, format!("{carried}\n\n---\n\nhi"));
    assert_eq!(st.reports().count(), 0);
    assert_eq!(queued(&st), 0);
}

#[test]
fn a_prompt_with_nothing_queued_carries_nothing() {
    let mut st = moa_brain(1);
    let fx = prompt_at(&mut st, at(T), 1, "hi", false);
    let fx = play(&mut st, T, fx);
    assert_eq!(body_of(you_row(&fx)), serde_json::json!({ "text": "hi" }));
    assert_eq!(sent(&fx), "hi");
}

#[test]
fn the_wake_row_is_automatic() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let w = T + 45 * SEC;
    let fx = fire(&mut st, w);
    let fx = play(&mut st, w, fx);
    let you = you_row(&fx);
    assert!(you.automatic);
    let turn = st.central().turn.as_ref().expect("moa answers the wake");
    assert_eq!(you.message_id.as_deref(), Some(turn.message_id.as_str()));
    assert_eq!(turn.waiter, None);
    let body = body_of(you);
    assert_eq!(body["reports"], 1);
    assert_eq!(body["text"].as_str(), Some(sent(&fx).as_str()));
    assert!(body.get("carried").is_none());
    assert!(common::replies(&fx).is_empty());
}

#[test]
fn auto_off_never_wakes_but_prompts_still_carry() {
    let meta = vec![(AUTO_KEY.to_string(), "off".to_string())];
    let (mut st, fx) = moa_brain_with(&TITLES[..1], meta);
    assert_eq!(wake_timer(&fx), None);
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    assert_eq!(wake_timer(&fx), None);
    // A stray firing wakes nothing either.
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    let fx = prompt_at(&mut st, at(T + 50 * SEC), 1, "hi", false);
    let fx = play(&mut st, T + 50 * SEC, fx);
    assert_eq!(body_of(you_row(&fx))["reports"], 1);
    assert_eq!(st.reports().count(), 0);
}

/// `/auto` (`console.auto`) at `ms`, as request `req`: `on`, else a flip.
fn auto_at(st: &mut State, ms: i64, req: u64, on: Option<bool>) -> Vec<Effect> {
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(req),
        caller: Default::default(),
        op: Op::ConsoleAuto { on },
    };
    handle_checked(st, input, at(ms))
}

/// `/auto` flips the wake-ups, or sets them, at once: the `Wake` timer and the console's
/// `state` row follow. Set as it is, nothing is saved.
#[test]
fn auto_flips_and_sets() {
    const ON: &str = "moa reads session reports on its own";
    const OFF: &str =
        "moa won't read session reports on its own; they ride on your next prompt (/auto on)";
    let mut st = moa_brain(1);
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    let due = Some(Some(UtcMs(T + 45 * SEC)));
    assert_eq!(wake_timer(&fx), due);
    let said = |fx: &[Effect]| match &common::replies(fx)[..] {
        [(_, Ok(done))] => done.text.clone(),
        other => panic!("one answer: {other:?}"),
    };
    let meta = |value: Option<&str>| Change::Meta {
        key: AUTO_KEY.into(),
        value: value.map(str::to_string),
    };
    let auto = |fx: &[Effect]| common::central::published_state(fx).map(|s| s["auto"].clone());

    let fx = auto_at(&mut st, T + SEC, 1, None);
    assert_eq!(said(&fx), OFF);
    assert_eq!(common::changes(&fx), [&meta(Some("off"))]);
    assert_eq!(wake_timer(&fx), Some(None));
    assert_eq!(auto(&fx), Some(Value::Bool(false)));

    let fx = auto_at(&mut st, T + 2 * SEC, 2, Some(false));
    assert_eq!(said(&fx), OFF);
    assert!(common::changes(&fx).is_empty(), "{fx:?}");

    let fx = auto_at(&mut st, T + 3 * SEC, 3, None);
    assert_eq!(said(&fx), ON);
    assert_eq!(common::changes(&fx), [&meta(None)]);
    assert_eq!(wake_timer(&fx), due);
    assert_eq!(auto(&fx), Some(Value::Bool(true)));

    let fx = auto_at(&mut st, T + 4 * SEC, 4, Some(false));
    assert_eq!(said(&fx), OFF);
    assert_eq!(common::changes(&fx), [&meta(Some("off"))]);
    let fx = auto_at(&mut st, T + 5 * SEC, 5, Some(true));
    assert_eq!(said(&fx), ON);
    assert_eq!(common::changes(&fx), [&meta(None)]);
    assert_eq!(auto(&fx), Some(Value::Bool(true)));

    // Typed in the console, too.
    let typed = |st: &mut State, req: u64, line: &str| {
        let input = Input::Client {
            conn: ConnId(1),
            req: ReqId(req),
            caller: Default::default(),
            op: Op::ConsoleCommand { line: line.into() },
        };
        handle_checked(st, input, at(T + 6 * SEC))
    };
    assert_eq!(said(&typed(&mut st, 6, "/auto off")), OFF);
    assert_eq!(said(&typed(&mut st, 7, "/auto")), ON);
}

/// The property's find: `/auto off` while a wake-up's agent starts. The wake gives up at
/// its commit: no row, no prompt, and the reports wait for your next prompt.
#[test]
fn auto_off_stops_a_wake_under_way() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Start { .. }]),
        "the wake starts moa's agent: {fx:?}"
    );
    auto_at(&mut st, T + 46 * SEC, 1, Some(false));
    let fx = play(&mut st, T + 47 * SEC, fx);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert!(
        common::central::feed_rows(&fx)
            .iter()
            .all(|r| r.kind != "you"),
        "{fx:?}"
    );
    assert_eq!(st.reports().count(), 1);
    assert_eq!(st.central().phase, Phase::Ready);
}

#[test]
fn no_wake_while_moa_answers() {
    let mut st = moa_brain(1);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    assert_eq!(st.central().phase, Phase::Thinking);
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    assert_eq!(wake_timer(&fx), None);
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    // Due since T + 45 s: it goes with the turn's end.
    let fx = end_turn(&mut st, T + 60 * SEC);
    assert!(sent(&fx).starts_with(WAKE));
    assert_eq!(st.reports().count(), 0);
}

#[test]
fn no_wake_while_restarting() {
    let mut st = moa_brain(1);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    finished(&mut st, 0, T, "Merged PR #3");
    // The turn hangs: the watchdog cancels it, then restarts moa's agent.
    let dog = |st: &mut State, ms| handle_checked(st, Input::Timer(TimerKey::Watchdog), at(ms));
    dog(&mut st, T0 + 300 * SEC);
    let restart = T0 + 330 * SEC;
    let fx = dog(&mut st, restart);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    assert!(st.central().restarting.is_some());
    // Due, but the restart is reopening today's thread: no wake races it.
    let generation = st.central().generation;
    let fx = hear_at(&mut st, at(restart), up(generation, caps(true, true, true)));
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Open { .. }]),
        "only the reopen: {fx:?}"
    );
    assert_eq!(wake_timer(&fx), None);
    assert_eq!(st.reports().count(), 1);
    // Reopened, the wake goes.
    let thread = st.threads().next().unwrap().thread_id.clone();
    let fx = hear_at(
        &mut st,
        at(restart),
        opened(generation, &thread, Opened::Resumed),
    );
    assert!(sent(&fx).starts_with(WAKE));
    assert_eq!(st.reports().count(), 0);
}

#[test]
fn no_wake_while_a_permission_waits() {
    let mut st = moa_brain(1);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    let turn = st.central().turn.as_ref().expect("a turn");
    let ask = AcpIn::Permission {
        generation: st.central().generation,
        req: 7,
        thread: turn.thread.clone(),
        title: "Run git push".into(),
        options: vec![PermOption {
            id: "no".into(),
            kind: PermKind::RejectOnce,
            name: "No".into(),
        }],
    };
    hear_at(&mut st, at(T0), ask);
    assert_eq!(st.central().asking.len(), 1);
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    assert_eq!(wake_timer(&fx), None);
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");

    // Answered, the turn goes on; its end lets the wake go.
    let answer = Op::ConsolePermission {
        id: None,
        answer: PermAnswer::No,
    };
    let fx = handle_checked(
        &mut st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(2),
            caller: Default::default(),
            op: answer,
        },
        at(T + 50 * SEC),
    );
    assert!(st.central().asking.is_empty(), "{fx:?}");
    let fx = end_turn(&mut st, T + 60 * SEC);
    assert!(sent(&fx).starts_with(WAKE));
    assert_eq!(st.reports().count(), 0);
}

#[test]
fn no_wake_after_a_failed_start() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let w = T + 45 * SEC;
    let fx = fire(&mut st, w);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let generation = st.central().generation;
    let why = "no login".to_string();
    let fx = hear_at(&mut st, at(w), AcpIn::Exited { generation, why });
    assert!(matches!(st.central().phase, Phase::Failed(_)));
    assert!(!matches!(wake_timer(&fx), Some(Some(_))), "{fx:?}");
    let fx = fire(&mut st, w + 600 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.reports().count(), 1);
}

#[test]
fn a_wake_whose_thread_cant_open_waits_the_gap() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let w = T + 45 * SEC;
    fire(&mut st, w);
    let generation = st.central().generation;
    hear_at(&mut st, at(w), up(generation, caps(true, true, true)));
    let result = Err("not logged in".to_string());
    let fx = hear_at(&mut st, at(w), AcpIn::Opened { generation, result });
    // Not tried again at once: the next wake is a gap later.
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(w + 180 * SEC))));
    assert_eq!(st.reports().count(), 1);
}

#[test]
fn a_refused_commit_delivers_nothing() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    refuse_writes(&mut st, T + SEC, T + 600 * SEC);
    let fx = prompt_at(&mut st, at(T + 2 * SEC), 1, "hi", false);
    let fx = play(&mut st, T + 2 * SEC, fx);
    assert!(
        matches!(common::replies(&fx)[..], [(ReqId(1), Err(_))]),
        "{fx:?}"
    );
    assert!(prompts_sent(&fx).is_empty());
    assert!(common::changes(&fx).is_empty());
    assert_eq!(st.reports().count(), 1);
}

#[test]
fn no_wake_while_writes_are_refused() {
    let mut st = moa_brain(1);
    finished(&mut st, 0, T, "Merged PR #3");
    let until = T + 600 * SEC;
    let fx = refuse_writes(&mut st, T + SEC, until);
    let set = Effect::SetTimer {
        key: TimerKey::Wake,
        at: UtcMs(until),
    };
    assert_eq!(common::timers(&fx, TimerKey::Wake), [&set]);
    // Due, but nothing can be saved: it waits for the refusal's end, and tries nothing.
    let fx = fire(&mut st, T + 300 * SEC);
    assert_eq!(common::timers(&fx, TimerKey::Wake), [&set]);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    let fx = fire(&mut st, until);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
}

#[test]
fn the_biggest_wake_is_bounded() {
    let title = "Lantern ".repeat(12_500);
    let titles: Vec<&str> = std::iter::once(title.as_str())
        .chain(TITLES[1..8].iter().copied())
        .collect();
    let mut st = moa_brain_with(&titles, vec![]).0;
    let huge = "x".repeat(100_000);
    let mut ms = T;
    for n in 0..8 {
        for _ in 0..6 {
            finished(&mut st, n, ms, &huge);
            let what = huge.clone();
            hook_at(&mut st, n, ms + 1, HookKind::NeedsAnswer { what });
            ms += SEC;
        }
    }
    assert_eq!(st.reports().count(), 16);
    let w = ms + 180 * SEC;
    let fx = fire(&mut st, w);
    let text = sent(&play(&mut st, w, fx));
    assert!(text.starts_with(&format!("{WAKE}16 session report(s)")));
    assert_eq!(text.matches("\n### ").count(), 16);
    // Under the longest prompt.
    const { assert!(90_000 <= PROMPT_MAX) };
    let n = text.chars().count();
    assert!(n <= 90_000, "{n} characters");
}

#[test]
fn odd_sessions_still_report() {
    let titles = ["Lantern\nfix the\tbuild", "Kite"];
    let mut st = moa_brain_with(&titles, vec![]).0;
    let odd = "\x1b[31mred\x1b[0m and \u{202e}שלום עולם\u{202c}";
    finished(&mut st, 0, T, odd);
    // A heading of the session's own is quoted, not taken for another report.
    finished(&mut st, 0, T + SEC, "Done.\n\n### Next steps\n- ship it");
    finished(&mut st, 1, T + SEC, "Kite is done");
    // Kite is archived before the wake.
    let qs = vec![st.handle_of(sid_of(1))];
    let fx = handle_checked(
        &mut st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(9),
            caller: Default::default(),
            op: Op::SessionArchive { qs },
        },
        at(T + 2 * SEC),
    );
    assert!(matches!(common::replies(&fx)[..], [(_, Ok(_))]), "{fx:?}");
    let w = T + 46 * SEC;
    let fx = fire(&mut st, w);
    let text = sent(&play(&mut st, w, fx));
    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("### ")).collect();
    assert_eq!(headings.len(), 2, "{text}");
    assert!(headings[0].starts_with("### Lantern fix the build · project (none) · session "));
    assert!(headings[1].starts_with("### Kite · project Kite · session "));
    assert!(
        headings.iter().all(|h| h.contains(" at 03:00")),
        "{headings:?}"
    );
    assert!(text.contains(odd));
    assert!(
        text.contains("> Done.\n>\n> ### Next steps\n> - ship it"),
        "{text}"
    );
    assert_eq!(st.reports().count(), 0);
}

#[test]
fn a_wake_follows_a_turn_whose_agent_exited() {
    let mut st = moa_brain(1);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    let generation = st.central().generation;
    let why = "killed".to_string();
    hear_at(&mut st, at(T0), AcpIn::Exited { generation, why });
    // Its end never comes.
    assert!(st.central().turn.is_some());
    let fx = finished(&mut st, 0, T, "Merged PR #3");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(T + 45 * SEC))));
    let fx = fire(&mut st, T + 45 * SEC);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let text = sent(&play(&mut st, T + 45 * SEC, fx));
    assert!(text.starts_with(WAKE), "{text}");
    assert_eq!(st.reports().count(), 0);
}

#[test]
fn a_wake_whose_commit_fails_waits_the_gap() {
    // The queued report's row has a value this build doesn't know: no commit can delete it.
    let report = Report {
        id: 1,
        session_id: sid_of(0),
        kind: ReportKind::TurnEnded,
        queued_at: UtcMs(T),
        last_at: UtcMs(T),
        turns: vec!["Merged PR #3".into()],
    };
    let unknown = LoadNote {
        table: "reports",
        key: "#1".into(),
        reason: "kind isn't a known one",
    };
    let snapshot = Snapshot {
        reports: vec![report],
        unknown: vec![unknown],
        ..moa_snapshot(&TITLES[..1], vec![])
    };
    let (mut st, _) = moa_load(snapshot, "");
    let w = T + 45 * SEC;
    let fx = fire(&mut st, w);
    let fx = play(&mut st, w, fx);
    assert!(prompts_sent(&fx).is_empty());
    // Not tried again at once: the next wake is a gap later.
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(w + 180 * SEC))), "{fx:?}");
    assert_eq!(st.reports().count(), 1);
}

#[test]
fn a_wake_stays_within_the_longest_prompt() {
    let titles: Vec<String> = (0..100).map(|n| format!("{} {n}", TITLES[n % 4])).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (mut st, _) = moa_load(moa_snapshot(&titles, vec![]), "[moa]\nwake_batch = 100\n");
    let long = "word ".repeat(1_000);
    for n in 0..100 {
        finished(&mut st, n, T + n as i64 * 100, &long);
    }
    let w = T + 300 * SEC;
    let fx = fire(&mut st, w);
    let text = sent(&play(&mut st, w, fx));
    let n = text.chars().count();
    assert!(n <= PROMPT_MAX, "{n} characters");
    // The oldest went; the rest wait for the next wake.
    let carried = text.lines().filter(|l| l.starts_with("### ")).count();
    assert!(carried > 1 && carried < 100, "{carried}");
    assert!(text.contains(&format!("### {} · ", titles[0])));
    assert_eq!(st.reports().count(), 100 - carried);
}

#[test]
fn no_timer_with_nothing_queued() {
    let (mut st, fx) = moa_brain_with(&TITLES[..1], vec![]);
    assert!(common::timers(&fx, TimerKey::Wake).is_empty());
    let started = HookKind::TurnStarted {
        prompt: "go".into(),
    };
    let mut all = hook_at(&mut st, 0, T, started);
    all.extend(finished(&mut st, 0, T + SEC, "  "));
    let fx = prompt_at(&mut st, at(T + 2 * SEC), 1, "hello", false);
    all.extend(play(&mut st, T + 2 * SEC, fx));
    all.extend(end_turn(&mut st, T + 3 * SEC));
    assert!(common::timers(&all, TimerKey::Wake).is_empty(), "{all:?}");
}

// ---------- a plain yes reaches moa (D1, D5) ----------

const KITE_YES: &str = "Kite has 3 overdue tasks blocked on the domain; move them to Friday?";
const YES_BLOCK: &str = "(moa · the user said yes to your suggestion(s), not typed by the user)\n\
                         - #12: Kite has 3 overdue tasks blocked on the domain; move them to \
                         Friday?\nDo what each one proposed.";

/// Suggestion `id`, open, with `text` and `actions`.
fn open_suggestion(id: i64, text: &str, actions: Vec<SuggestionAction>) -> Suggestion {
    Suggestion {
        id: SuggestionId(id),
        text: text.into(),
        status: SuggestionStatus::Open,
        actions,
        actions_raw: None,
        actions_invalid: None,
        result: None,
        created_at: UtcMs(T0),
        resolved_at: None,
    }
}

/// [`moa_brain`] of one session, with `meta`, the moactl grammar, and open suggestion #12
/// ([`KITE_YES`], `actions` its actions).
fn moa_suggesting(meta: Vec<(String, String)>, actions: Vec<SuggestionAction>) -> State {
    let snapshot = Snapshot {
        suggestions: vec![open_suggestion(12, KITE_YES, actions)],
        ..moa_snapshot(&TITLES[..1], meta)
    };
    let config = Config::parse(&common::central::toml("acct-a"), Path::new(HOME)).unwrap();
    let deps = Deps {
        home: PathBuf::from(HOME),
        parse_action: Some(moa_proto::cli::parse_action),
        ..Deps::default()
    };
    load(config, deps, snapshot, at(T0)).0
}

/// `suggestion yes ID` at `ms`, as request `req`.
fn yes_at(st: &mut State, ms: i64, req: u64, id: i64) -> Vec<Effect> {
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(req),
        caller: Default::default(),
        op: Op::SuggestionAnswer {
            id: Some(id),
            answer: Answer::Yes,
        },
    };
    handle_checked(st, input, at(ms))
}

/// The suggestion rows `fx` saves.
fn suggestion_rows(fx: &[Effect]) -> Vec<&Suggestion> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Suggestion(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Suggestion #12's result now.
fn result_of_12(st: &State) -> Option<String> {
    let s = st.suggestions().find(|s| s.id == SuggestionId(12));
    s.and_then(|s| s.result.clone())
}

/// D5: yes on a suggestion with no actions is a signal for moa. It waits for moa, and moa,
/// idle, hears it at once in an automatic prompt; that prompt's commit marks it told.
#[test]
fn a_yes_without_actions_tells_moa() {
    let mut st = moa_suggesting(vec![], vec![]);
    let fx = yes_at(&mut st, T, 1, 12);
    match &common::replies(&fx)[..] {
        [(ReqId(1), Ok(done))] => assert_eq!(done.text, "yes; moa acts on it when it's free"),
        other => panic!("one answer: {other:?}"),
    }
    let saved = suggestion_rows(&fx);
    assert_eq!(saved[0].status, SuggestionStatus::Accepted);
    assert_eq!(saved[0].result.as_deref(), Some(YES_WAITING));
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let fx = play(&mut st, T, fx);
    assert_eq!(sent(&fx), YES_BLOCK);
    let commit = you_commit(&fx);
    let told = commit.iter().find_map(|c| match c {
        Change::Suggestion(s) if s.id == SuggestionId(12) => s.result.as_deref(),
        _ => None,
    });
    assert_eq!(told, Some(YES_TOLD), "{commit:?}");
    let you = you_row(&fx);
    assert!(you.automatic);
    let body = body_of(you);
    assert_eq!(
        body,
        serde_json::json!({ "text": YES_BLOCK, "yeses": [12] }),
        "{body}"
    );
    assert_eq!(result_of_12(&st).as_deref(), Some(YES_TOLD));
    assert_eq!(st.yeses().count(), 0);
    // Told once: the turn's end sends nothing more.
    let fx = end_turn(&mut st, T + SEC);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
}

/// Review focus 2: a yes waits for moa: while moa answers it goes at the turn's end; while
/// moa's agent restarts it goes once the thread reopens; after a failed start it rides on
/// your next prompt. A second yes is refused.
#[test]
fn a_yes_waits_for_moa() {
    // While moa answers.
    let mut st = moa_suggesting(vec![], vec![]);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    let fx = yes_at(&mut st, T, 2, 12);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(wake_timer(&fx), None);
    let again = yes_at(&mut st, T, 3, 12);
    match &common::replies(&again)[..] {
        [(ReqId(3), Err(e))] => assert_eq!(e.0, "suggestion #12 is already accepted"),
        other => panic!("one refusal: {other:?}"),
    }
    let fx = end_turn(&mut st, T + SEC);
    assert_eq!(sent(&fx), YES_BLOCK);
    assert!(you_row(&fx).automatic);

    // While moa's agent restarts.
    let mut st = moa_suggesting(vec![], vec![]);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    let dog = |st: &mut State, ms| handle_checked(st, Input::Timer(TimerKey::Watchdog), at(ms));
    dog(&mut st, T0 + 300 * SEC);
    let restart = T0 + 330 * SEC;
    dog(&mut st, restart);
    assert!(st.central().restarting.is_some());
    let fx = yes_at(&mut st, restart, 2, 12);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    let generation = st.central().generation;
    let fx = hear_at(&mut st, at(restart), up(generation, caps(true, true, true)));
    assert!(
        matches!(acps(&fx)[..], [AcpCmd::Open { .. }]),
        "only the reopen: {fx:?}"
    );
    let thread = st.threads().next().unwrap().thread_id.clone();
    let reopened = opened(generation, &thread, Opened::Resumed);
    let fx = hear_at(&mut st, at(restart), reopened);
    assert_eq!(sent(&fx), YES_BLOCK);

    // After a failed start.
    let mut st = moa_suggesting(vec![], vec![]);
    let fx = yes_at(&mut st, T, 1, 12);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let generation = st.central().generation;
    let why = "no login".to_string();
    let fx = hear_at(&mut st, at(T), AcpIn::Exited { generation, why });
    assert!(matches!(st.central().phase, Phase::Failed(_)));
    assert!(!matches!(wake_timer(&fx), Some(Some(_))), "{fx:?}");
    let fx = fire(&mut st, T + 600 * SEC);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(result_of_12(&st).as_deref(), Some(YES_WAITING));
    let fx = prompt_at(&mut st, at(T + 601 * SEC), 2, "hi", false);
    let fx = play(&mut st, T + 601 * SEC, fx);
    assert_eq!(sent(&fx), format!("{YES_BLOCK}\n\n---\n\nhi"));
    assert_eq!(result_of_12(&st).as_deref(), Some(YES_TOLD));
}

/// `/auto` stops the wake-ups only: a yes goes all the same.
#[test]
fn a_yes_goes_with_auto_off() {
    let meta = vec![(AUTO_KEY.to_string(), "off".to_string())];
    let mut st = moa_suggesting(meta, vec![]);
    let fx = yes_at(&mut st, T, 1, 12);
    let fx = play(&mut st, T, fx);
    assert_eq!(sent(&fx), YES_BLOCK);
    assert!(you_row(&fx).automatic);
}

/// D1, ruling R1: your prompt, on a thread that had its first prompt, carries the waiting
/// yes before your text, in the one commit that saves its row.
#[test]
fn your_prompt_carries_yeses() {
    let mut st = moa_suggesting(vec![], vec![]);
    let fx = prompt_at(&mut st, at(T0), 1, "hello", false);
    play(&mut st, T0, fx);
    end_turn(&mut st, T0 + SEC);
    let thread = st.threads().next().unwrap().thread_id.clone();
    // moa's agent stops; the yes's start of it fails: the yes waits for you.
    let generation = st.central().generation;
    let why = "killed".to_string();
    hear_at(&mut st, at(T), AcpIn::Exited { generation, why });
    let fx = yes_at(&mut st, T + SEC, 2, 12);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    let generation = st.central().generation;
    let why = "no login".to_string();
    hear_at(&mut st, at(T + SEC), AcpIn::Exited { generation, why });
    assert_eq!(st.yeses().count(), 1);

    let mut fx = prompt_at(&mut st, at(T + 2 * SEC), 3, "hi", false);
    let generation = st.central().generation;
    fx.extend(hear_at(
        &mut st,
        at(T + 2 * SEC),
        up(generation, caps(true, true, true)),
    ));
    let reopened = opened(generation, &thread, Opened::Resumed);
    fx.extend(hear_at(&mut st, at(T + 2 * SEC), reopened));
    let commit = you_commit(&fx);
    assert!(
        commit.iter().any(|c| matches!(c,
            Change::Suggestion(s) if s.result.as_deref() == Some(YES_TOLD))),
        "{commit:?}"
    );
    let you = you_row(&fx);
    assert!(!you.automatic);
    assert_eq!(you.thread_id, thread);
    let body = body_of(you);
    assert_eq!(body["text"], "hi");
    assert_eq!(body["yeses"], serde_json::json!([12]));
    assert_eq!(body["carried"], YES_BLOCK);
    assert!(body.get("reports").is_none(), "{body}");
    assert_eq!(sent(&fx), format!("{YES_BLOCK}\n\n---\n\nhi"));
}

/// D1: a wake-up under way when a yes comes carries it too, before the reports.
#[test]
fn a_wake_carries_yeses_too() {
    let mut st = moa_suggesting(vec![], vec![]);
    finished(&mut st, 0, T, "Merged PR #3");
    let w = T + 45 * SEC;
    let fx = fire(&mut st, w);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Start { .. }]), "{fx:?}");
    // moa's agent starts for the wake: the yes waits for it.
    let yes = yes_at(&mut st, w, 1, 12);
    assert!(acps(&yes).is_empty(), "{yes:?}");
    let fx = play(&mut st, w, fx);
    let text = sent(&fx);
    let head = format!("{YES_BLOCK}\n\n---\n\n{WAKE}1 session report(s) since your last look:");
    assert!(text.starts_with(&head), "{text}");
    let you = you_row(&fx);
    assert!(you.automatic);
    let body = body_of(you);
    assert_eq!(body["text"].as_str(), Some(text.as_str()));
    assert_eq!(body["reports"], 1);
    assert_eq!(body["yeses"], serde_json::json!([12]));
    assert_eq!(st.reports().count(), 0);
    assert_eq!(st.yeses().count(), 0);
}

/// A yes whose prompt can't reach moa's agent (its thread can't open) isn't tried again at
/// once: the next try is a gap later, and the yes still waits.
#[test]
fn a_yes_whose_thread_cant_open_waits_the_gap() {
    let mut st = moa_suggesting(vec![], vec![]);
    yes_at(&mut st, T, 1, 12);
    let generation = st.central().generation;
    hear_at(&mut st, at(T), up(generation, caps(true, true, true)));
    let result = Err("not logged in".to_string());
    let fx = hear_at(&mut st, at(T), AcpIn::Opened { generation, result });
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(T + 180 * SEC))));
    assert_eq!(st.yeses().count(), 1);
    let fx = fire(&mut st, T + 180 * SEC);
    assert!(matches!(acps(&fx)[..], [AcpCmd::Open { .. }]), "{fx:?}");
}

/// One backoff for every automatic source: a wake-up whose thread can't open holds back the
/// yes that waits too, for the same gap; then one prompt carries both.
#[test]
fn a_failed_try_holds_back_every_source() {
    let mut st = moa_suggesting(vec![], vec![]);
    finished(&mut st, 0, T, "Merged PR #3");
    let w = T + 45 * SEC;
    fire(&mut st, w);
    yes_at(&mut st, w, 1, 12);
    let generation = st.central().generation;
    hear_at(&mut st, at(w), up(generation, caps(true, true, true)));
    let result = Err("not logged in".to_string());
    let fx = hear_at(&mut st, at(w), AcpIn::Opened { generation, result });
    assert!(acps(&fx).is_empty(), "the yes waits too: {fx:?}");
    assert_eq!(wake_timer(&fx), Some(Some(UtcMs(w + 180 * SEC))));
    let fx = fire(&mut st, w + 180 * SEC);
    let text = sent(&play(&mut st, w + 180 * SEC, fx));
    assert!(
        text.starts_with(&format!("{YES_BLOCK}\n\n---\n\n{WAKE}")),
        "{text}"
    );
}

/// A long suggestion's line in the yes block is its first 500 characters, then `…`: the
/// suggestion itself keeps all of it.
#[test]
fn a_long_yes_is_cut() {
    let long = "Kite ".repeat(1_000);
    let snapshot = Snapshot {
        suggestions: vec![open_suggestion(12, &long, vec![])],
        ..moa_snapshot(&TITLES[..1], vec![])
    };
    let (mut st, _) = moa_load(snapshot, "");
    let fx = yes_at(&mut st, T, 1, 12);
    let text = sent(&play(&mut st, T, fx));
    const { assert!(YES_MAX == 500) };
    let line = format!("- #12: {}…\n", &long[..YES_MAX]);
    assert!(text.contains(&line), "{text}");
    assert_eq!(
        text.chars().count(),
        YES_BLOCK.chars().count() - KITE_YES.len() + 501
    );
    let kept = st.suggestions().find(|s| s.id == SuggestionId(12)).unwrap();
    assert_eq!(kept.text, long);
}

/// A yes with actions runs them: moa hears nothing of it.
#[test]
fn a_yes_with_actions_tells_moa_nothing() {
    let note = SuggestionAction {
        verb: "note".into(),
        args: vec!["Kite waits on the domain".into()],
    };
    let mut st = moa_suggesting(vec![], vec![note]);
    let fx = yes_at(&mut st, T, 1, 12);
    match &common::replies(&fx)[..] {
        [(ReqId(1), Ok(done))] => assert_eq!(done.text, "done"),
        other => panic!("one answer: {other:?}"),
    }
    assert_eq!(result_of_12(&st).as_deref(), Some(YES_DONE));
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.yeses().count(), 0);
    let fx = prompt_at(&mut st, at(T + SEC), 2, "hi", false);
    let fx = play(&mut st, T + SEC, fx);
    assert_eq!(sent(&fx), "hi");
}

// ---------- exactly once, across restarts ----------

/// The world of [`a_yes_reaches_moa_once`]: settle, gap and batch small.
const SMALL: &str = "[moa]\nwake_settle_seconds = 5\nwake_gap_seconds = 20\nwake_batch = 2\n";

/// One step of the property.
#[derive(Clone, Debug)]
enum Step {
    /// Session `n` finishes a turn, its text a token of its own.
    Turn(usize),
    /// Session `n` needs the user, what it asks a token of its own.
    NeedsYou(usize),
    /// Your prompt.
    Prompt,
    /// Time moves this many seconds, and the `Wake` timer fires if it is due.
    Advance(i64),
    /// moa's agent answers what it has been asked.
    Answer,
    /// `/auto` is flipped, as `console.auto`: at once, mid-turn or not.
    Auto,
    /// `/auto` is flipped by hand on the disk, then the daemon restarts to read it.
    AutoOnDisk,
    /// The next input's writes are refused.
    Refuse,
    /// The daemon restarts after the first k of the last input's commits reach the disk, k
    /// from 0 to all of them (this index picks it): the rest are lost, and nothing it said
    /// reaches moa's agent.
    Restart(prop::sample::Index),
    /// moa suggests, with no actions, a token of its own its text.
    Suggest,
    /// You answer yes to one of moa's suggestions (this index picks it), answered or not.
    Yes(prop::sample::Index),
    /// One of [`DUE_TASKS`] (this index picks it) becomes due this many days from today, -2
    /// to 1: a task becomes due, or its date moves.
    Due(prop::sample::Index, i64),
    /// The day turns: time moves to the next day's first hour, and the `Wake` timer fires if
    /// it is due.
    Day,
}

/// The daemon's tasks with no due date at first, titled `due-<id>`, by id.
const DUE_TASKS: [i64; 3] = [101, 102, 103];

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        6 => (0..4usize).prop_map(Step::Turn),
        2 => (0..4usize).prop_map(Step::NeedsYou),
        1 => Just(Step::Prompt),
        3 => (0..=60i64).prop_map(Step::Advance),
        3 => Just(Step::Answer),
        1 => Just(Step::Auto),
        1 => Just(Step::AutoOnDisk),
        1 => Just(Step::Refuse),
        2 => any::<prop::sample::Index>().prop_map(Step::Restart),
        1 => Just(Step::Suggest),
        2 => any::<prop::sample::Index>().prop_map(Step::Yes),
        2 => (any::<prop::sample::Index>(), -2..=1i64).prop_map(|(k, d)| Step::Due(k, d)),
        1 => Just(Step::Day),
    ]
}

/// Each token in `text`, and how many times it is there.
fn tokens(text: &str) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for (i, _) in text.match_indices("tok-") {
        *found.entry(text[i..i + 8].to_string()).or_default() += 1;
    }
    found
}

/// The tokens a `you` row's body carries, in its text and in what rode on it, and how many
/// times.
fn row_tokens(body: &str) -> BTreeMap<String, usize> {
    let body: Value = serde_json::from_str(body).unwrap();
    let mut found = BTreeMap::new();
    for text in ["text", "carried"].iter().filter_map(|k| body[k].as_str()) {
        for (t, n) in tokens(text) {
            *found.entry(t).or_default() += n;
        }
    }
    found
}

/// Each token in `found` is there once.
fn once(found: &BTreeMap<String, usize>, what: &str) -> Result<(), TestCaseError> {
    for (t, n) in found {
        prop_assert_eq!(*n, 1, "{} is {} times in {}", t, n, what);
    }
    Ok(())
}

/// What the expected queue takes once its commit reaches the disk: a hook event's report, or
/// your yes to the suggestion whose text is `token`.
enum Event {
    Report {
        session: usize,
        kind: ReportKind,
        token: String,
    },
    Yes(String),
}

/// The queue as K2's rules alone keep it, commit by commit as the disk takes them.
#[derive(Default)]
struct Expected {
    /// Each session's tokens of each kind, waiting.
    queues: HashMap<(usize, ReportKind), Vec<String>>,
    /// The yeses waiting for moa, by their suggestion's token.
    yeses: BTreeSet<String>,
    /// Carried in a saved `you` row.
    carried: BTreeSet<String>,
    /// Pushed out by a 7th turn, replaced by a newer need, resolved by a turn, or never
    /// saved: no row has them.
    gone: BTreeSet<String>,
}

impl Expected {
    fn queue(&mut self, e: Event) {
        let (session, kind, token) = match e {
            Event::Report {
                session,
                kind,
                token,
            } => (session, kind, token),
            Event::Yes(token) => {
                self.yeses.insert(token);
                return;
            }
        };
        let q = self.queues.entry((session, kind)).or_default();
        match kind {
            ReportKind::TurnEnded => {
                q.push(token);
                if q.len() > TURNS_KEPT {
                    self.gone.insert(q.remove(0));
                }
                // The turn ended, so the session needs nothing now: its need is deleted.
                if let Some(need) = self.queues.remove(&(session, ReportKind::NeedsYou)) {
                    self.gone.extend(need);
                }
            }
            ReportKind::NeedsYou => {
                self.gone.extend(q.drain(..));
                q.push(token);
            }
        }
    }

    /// A `you` row carrying `tokens` is saved: each was waiting, each yes goes once, and each
    /// report it takes goes whole.
    fn carry(&mut self, tokens: &BTreeMap<String, usize>) -> Result<(), TestCaseError> {
        let mut reports = BTreeMap::new();
        for (t, n) in tokens {
            if self.yeses.remove(t) {
                self.carried.insert(t.clone());
                continue;
            }
            prop_assert!(
                self.queues.values().any(|q| q.contains(t)),
                "{} is carried, but isn't waiting: carried twice, or never queued",
                t
            );
            reports.insert(t.clone(), *n);
        }
        let tokens = &reports;
        let taken: Vec<(usize, ReportKind)> = self
            .queues
            .iter()
            .filter(|(_, q)| q.iter().any(|t| tokens.contains_key(t)))
            .map(|(k, _)| *k)
            .collect();
        for k in taken {
            let q = self.queues.remove(&k).unwrap();
            prop_assert!(
                q.iter().all(|t| tokens.contains_key(t)),
                "part of a report is carried: {:?} of {:?}",
                tokens,
                q
            );
            self.carried.extend(q);
        }
        Ok(())
    }
}

/// The daemon of the property: the core, the disk, the clock and the `Wake` timer, and moa's
/// agent played by hand. An input's effects are executed only when the next step comes, so
/// a restart can come between an input and its effects.
struct Daemon {
    disk: Disk,
    b: TypicalBrain,
    st: State,
    now: i64,
    /// Four of the typical brain's live sessions: their ids and conversations.
    sessions: Vec<(SessionId, String)>,
    /// The `Wake` timer's deadline, as executed.
    wake_at: Option<i64>,
    /// What moa's agent has been asked and hasn't answered.
    out: VecDeque<AcpCmd>,
    /// The last input's effects, not executed yet, and the event they bring the queue.
    last: Vec<Effect>,
    event: Option<Event>,
    refuse_next: bool,
    /// What the last input's `/auto` must leave on the disk, `true` for on, once its effects
    /// are all executed; `None` when it wasn't `/auto`, or its writes were refused.
    auto_set: Option<bool>,
    /// The last hook's `seq`, token, request and thread.
    seq: u32,
    token: u32,
    req: u64,
    thread: u32,
    /// The ids of moa's suggestions asked for. One whose commit was lost may be another's
    /// now: its token is its text.
    suggested: Vec<i64>,
    expected: Expected,
    /// Each (task title, due date, day) a prompt sent to moa's agent told was due.
    told_due: BTreeSet<(String, Date, Date)>,
}

impl Daemon {
    fn new() -> Result<Daemon, TestCaseError> {
        let b = TypicalBrain::new();
        let mut disk = Disk::new(&b, SMALL);
        let tasks: Vec<Change> = DUE_TASKS
            .iter()
            .map(|&id| Change::Task(due_task(id)))
            .collect();
        disk.apply(&tasks);
        let now = b.now().wall.0;
        let (st, last) = disk.restart(&b, at(now));
        let sessions: Vec<(SessionId, String)> = st
            .sessions()
            .filter(|s| s.lifecycle == Lifecycle::Live && s.location == Location::Here)
            .filter(|s| s.archived_at.is_none())
            .filter_map(|s| {
                let c = st
                    .conversations_of(s.id)
                    .find(|c| c.agent_kind == AgentKind::Claude)?;
                Some((s.id, c.conversation_id.clone()))
            })
            .take(4)
            .collect();
        assert_eq!(sessions.len(), 4, "the typical brain's live sessions");
        let mut d = Daemon {
            disk,
            b,
            st,
            now,
            sessions,
            wake_at: None,
            out: VecDeque::new(),
            last,
            event: None,
            refuse_next: false,
            auto_set: None,
            seq: 0,
            token: 0,
            req: 0,
            thread: 0,
            suggested: Vec::new(),
            expected: Expected::default(),
            told_due: BTreeSet::new(),
        };
        d.flush()?;
        Ok(d)
    }

    /// Executes the last input's effects in order: its commits reach the disk (the queue
    /// takes its event with the first), the `Wake` timer is set, and its commands reach moa's
    /// agent.
    fn flush(&mut self) -> Result<(), TestCaseError> {
        self.execute(None)
    }

    /// The last input's commits in `self.last`.
    fn commits(&self) -> usize {
        let persist = |e: &&Effect| matches!(e, Effect::Persist(_));
        self.last.iter().filter(persist).count()
    }

    /// Executes the last input's effects as [`Daemon::flush`] does, but with `commits`, the
    /// daemon dies once that many of its commits are on the disk: nothing after them is
    /// executed, and no command reaches moa's agent. An event whose commit didn't reach the
    /// disk was never saved.
    ///
    /// Each `you` row carries each of its tokens once, and each prompt that reaches moa's
    /// agent holds exactly the tokens of the row saved with it, each once. A wake-up's row is
    /// saved only while the disk has `/auto` on, and an `/auto` whose effects all ran left
    /// what it said on the disk.
    fn execute(&mut self, commits: Option<usize>) -> Result<(), TestCaseError> {
        let mut event = self.event.take();
        let auto_set = self.auto_set.take();
        let mut left = commits;
        let mut row: Option<BTreeMap<String, usize>> = None;
        for e in std::mem::take(&mut self.last) {
            match e {
                Effect::Persist(changes) => {
                    match &mut left {
                        Some(0) => break,
                        Some(n) => *n -= 1,
                        None => {}
                    }
                    if let Some(e) = event.take() {
                        self.expected.queue(e);
                    }
                    // An automatic prompt goes with /auto off for a yes, but carries no reports.
                    let reads = changes.iter().any(|c| {
                        matches!(c, Change::Feed(r) if r.kind == "you" && r.automatic
                            && serde_json::from_str::<Value>(&r.body).unwrap()["reports"].is_u64())
                    });
                    prop_assert!(
                        !reads || self.disk.meta(AUTO_KEY).as_deref() != Some("off"),
                        "a wake-up was saved with /auto off"
                    );
                    self.disk.apply(&changes);
                    for c in &changes {
                        if let Change::Feed(r) = c
                            && r.kind == "you"
                        {
                            let carried = row_tokens(&r.body);
                            once(&carried, "its row")?;
                            self.expected.carry(&carried)?;
                            row = Some(carried);
                        }
                    }
                }
                Effect::Acp(cmd) if commits.is_none() => {
                    if let AcpCmd::Prompt { text, .. } = &cmd {
                        let saved = row.take();
                        prop_assert!(saved.is_some(), "a prompt went before its row was saved");
                        let sent = tokens(text);
                        once(&sent, "its prompt")?;
                        prop_assert_eq!(Some(sent), saved);
                        let day = at(self.now).local_day;
                        for told in told_due(text, day) {
                            prop_assert!(
                                self.told_due.insert(told.clone()),
                                "{:?} is in two prompts",
                                told
                            );
                        }
                    }
                    self.out.push_back(cmd);
                }
                Effect::SetTimer {
                    key: TimerKey::Wake,
                    at,
                } => self.wake_at = Some(at.0),
                Effect::CancelTimer(TimerKey::Wake) => self.wake_at = None,
                _ => {}
            }
        }
        // A report whose event wasn't saved is gone; a yes that wasn't saved leaves its
        // suggestion open, to be answered again.
        if let Some(Event::Report { token, .. }) = event {
            self.expected.gone.insert(token);
        }
        if let (Some(on), None) = (auto_set, commits) {
            let off = self.disk.meta(AUTO_KEY).as_deref() == Some("off");
            prop_assert_eq!(!off, on, "/auto didn't leave what it said on the disk");
        }
        Ok(())
    }

    /// The next input's moment, a millisecond after the last. The last input's effects are
    /// executed first, and when the step before said so, writes are refused at this moment
    /// only. Whether they are.
    fn next(&mut self) -> Result<bool, TestCaseError> {
        self.flush()?;
        self.now += 1;
        let refused = std::mem::take(&mut self.refuse_next);
        if refused {
            let reload = Input::Reload {
                snapshot: Box::new(self.disk.snapshot()),
                refuse_writes: Some(RefuseWrites {
                    until: UtcMs(self.now + 1),
                    reason: "the disk is full".into(),
                }),
            };
            self.last = handle_checked(&mut self.st, reload, at(self.now));
            self.flush()?;
        }
        Ok(refused)
    }

    fn run(&mut self, input: Input) {
        self.last = handle_checked(&mut self.st, input, at(self.now));
    }

    /// Session `n`'s hook of `kind`, a new token its text. The queue takes it when it is
    /// saved: unless writes are refused, every one is, by the sessions chosen and the clock.
    fn hook(&mut self, n: usize, kind: ReportKind) -> Result<(), TestCaseError> {
        let refused = self.next()?;
        self.token += 1;
        self.seq += 1;
        let token = format!("tok-{:04}", self.token);
        let hook = match kind {
            ReportKind::TurnEnded => HookKind::TurnEnded {
                last_message: token.clone(),
            },
            ReportKind::NeedsYou => HookKind::NeedsAnswer {
                what: token.clone(),
            },
        };
        let (id, conversation) = &self.sessions[n];
        let at = UtcMs(self.now);
        let envelope = common::envelope(&id.to_string(), conversation, at, self.seq, hook);
        self.run(Input::Hook {
            reply: None,
            envelope,
        });
        let queued = common::changes(&self.last)
            .iter()
            .any(|c| matches!(c, Change::Report(r) if r.turns.last() == Some(&token)));
        prop_assert_eq!(queued, !refused, "{}", token);
        match refused {
            true => {
                self.expected.gone.insert(token);
            }
            false => {
                self.event = Some(Event::Report {
                    session: n,
                    kind,
                    token,
                })
            }
        }
        Ok(())
    }

    /// moa's agent answers each thing it has been asked: it starts, opens the thread asked
    /// for, and ends each prompt's turn. What its answers ask in turn is answered too, but a
    /// new prompt's turn ends at the next answer: the last input is often the one that sends
    /// a prompt.
    fn answer(&mut self) -> Result<(), TestCaseError> {
        self.flush()?;
        let mut asked = std::mem::take(&mut self.out);
        loop {
            let Some(cmd) = asked.pop_front() else {
                let opening = |c: &AcpCmd| matches!(c, AcpCmd::Start { .. } | AcpCmd::Open { .. });
                if !acps(&self.last).into_iter().any(opening) {
                    break;
                }
                self.flush()?;
                let (opens, later) = self.out.drain(..).partition(opening);
                (asked, self.out) = (opens, later);
                continue;
            };
            let heard = match cmd {
                AcpCmd::Start { generation, .. } => up(generation, caps(true, true, true)),
                AcpCmd::Open {
                    generation, how, ..
                } => {
                    let result = match how {
                        ThreadHow::New { .. } => {
                            self.thread += 1;
                            (format!("thread-{}", self.thread), Opened::New)
                        }
                        ThreadHow::Resume { thread } => (thread, Opened::Resumed),
                        ThreadHow::Load { thread } => (thread, Opened::Loaded),
                    };
                    AcpIn::Opened {
                        generation,
                        result: Ok(result),
                    }
                }
                AcpCmd::Prompt {
                    generation, prompt, ..
                } => common::central::ended(generation, prompt),
                _ => continue,
            };
            self.next()?;
            self.run(Input::Acp(heard));
        }
        Ok(())
    }

    /// The daemon starts again from what the disk holds; moa's agent, and whatever it was
    /// asked, went with the old one.
    fn restart(&mut self) -> Result<(), TestCaseError> {
        self.out.clear();
        self.wake_at = None;
        self.now += 1;
        let (st, fx) = self.disk.restart(&self.b, at(self.now));
        self.st = st;
        self.last = fx;
        self.flush()
    }

    /// The daemon dies once `k` of the last input's commits are on the disk, and starts
    /// again.
    fn restart_after(&mut self, k: usize) -> Result<(), TestCaseError> {
        self.execute(Some(k))?;
        self.restart()
    }

    /// One of `console`'s requests, its reply.
    fn client(&mut self, op: Op) -> Option<Result<Done, OpError>> {
        self.req += 1;
        self.run(Input::Client {
            conn: ConnId(1),
            req: ReqId(self.req),
            caller: Default::default(),
            op,
        });
        common::replies(&self.last).pop().map(|(_, r)| r)
    }

    /// moa suggests a token of its own, with no actions.
    fn suggest(&mut self) -> Result<(), TestCaseError> {
        self.next()?;
        self.token += 1;
        let token = format!("tok-{:04}", self.token);
        let op = Op::Suggest {
            text: token.clone(),
            actions: Vec::new(),
        };
        if let Some(Ok(done)) = self.client(op) {
            let id = done.data["id"].as_i64().expect("its id");
            if !self.suggested.contains(&id) {
                self.suggested.push(id);
            }
        }
        Ok(())
    }

    /// Yes to the suggestion `k` picks: one that waits is accepted, its yes waiting for moa
    /// once its commit reaches the disk; one already answered is refused.
    fn yes(&mut self, k: prop::sample::Index) -> Result<(), TestCaseError> {
        if self.suggested.is_empty() {
            return Ok(());
        }
        let refused = self.next()?;
        let id = self.suggested[k.index(self.suggested.len())];
        let was = self.st.suggestions().find(|s| s.id.0 == id).cloned();
        let op = Op::SuggestionAnswer {
            id: Some(id),
            answer: Answer::Yes,
        };
        let reply = self.client(op);
        match (was, reply) {
            (Some(s), Some(Ok(done))) => {
                prop_assert!(!refused, "a yes went while writes were refused");
                prop_assert_eq!(s.status, SuggestionStatus::Open);
                prop_assert_eq!(done.text, "yes; moa acts on it when it's free");
                self.event = Some(Event::Yes(s.text));
            }
            (Some(s), Some(Err(e))) if s.status == SuggestionStatus::Accepted => {
                prop_assert_eq!(e.0, format!("suggestion #{id} is already accepted"));
            }
            (Some(_), Some(Err(e))) => {
                prop_assert!(refused, "an open suggestion's yes was refused: {}", e.0);
                let why = "moa can't save right now (the disk is full); nothing was changed";
                prop_assert_eq!(e.0, why);
            }
            // Its commit was lost to a restart: no suggestion has the id.
            (None, Some(Err(e))) => prop_assert_eq!(e.0, "moactl suggestion yes|no|later ID"),
            (was, reply) => prop_assert!(false, "{:?} answered {:?}", was, reply),
        }
        Ok(())
    }

    /// `/auto` set by hand on the disk, `None` for on, and the daemon restarted to read it.
    fn set_auto(&mut self, value: Option<&str>) -> Result<(), TestCaseError> {
        self.flush()?;
        let key = AUTO_KEY.to_string();
        let value = value.map(str::to_string);
        self.disk.apply(&[Change::Meta { key, value }]);
        self.restart()
    }

    fn step(&mut self, step: Step) -> Result<(), TestCaseError> {
        match step {
            Step::Turn(n) => self.hook(n, ReportKind::TurnEnded),
            Step::NeedsYou(n) => self.hook(n, ReportKind::NeedsYou),
            Step::Prompt => {
                self.next()?;
                self.req += 1;
                self.last = prompt_at(&mut self.st, at(self.now), self.req, "hi", false);
                Ok(())
            }
            Step::Advance(s) => {
                self.flush()?;
                self.now += s * SEC;
                self.fire_due()
            }
            Step::Day => {
                self.flush()?;
                let today = at(self.now).local_day;
                self.now = start_of_next_day(today, &common::tz()).0 + HOUR;
                self.fire_due()
            }
            Step::Due(k, days) => {
                self.next()?;
                let today = at(self.now).local_day;
                let due = today.checked_add(days.days()).unwrap();
                let op = Op::TaskSet(TaskSet {
                    id: DUE_TASKS[k.index(DUE_TASKS.len())],
                    due: Some(due.to_string()),
                    ..TaskSet::default()
                });
                self.client(op);
                Ok(())
            }
            Step::Answer => self.answer(),
            Step::Auto => {
                let refused = self.next()?;
                self.auto_set = (!refused).then(|| !auto_on(&self.st));
                self.req += 1;
                self.run(Input::Client {
                    conn: ConnId(1),
                    req: ReqId(self.req),
                    caller: Default::default(),
                    op: Op::ConsoleAuto { on: None },
                });
                Ok(())
            }
            Step::AutoOnDisk => {
                self.flush()?;
                self.set_auto(auto_on(&self.st).then_some("off"))
            }
            Step::Refuse => {
                self.refuse_next = true;
                Ok(())
            }
            Step::Restart(k) => {
                let k = k.index(self.commits() + 1);
                self.restart_after(k)
            }
            Step::Suggest => self.suggest(),
            Step::Yes(k) => self.yes(k),
        }
    }

    /// The `Wake` timer fires, if it is due now.
    fn fire_due(&mut self) -> Result<(), TestCaseError> {
        if self.wake_at.is_some_and(|w| w <= self.now) {
            self.next()?;
            // The scheduler forgets a timer that fired.
            self.wake_at = None;
            self.run(Input::Timer(TimerKey::Wake));
        }
        Ok(())
    }

    /// `/auto` on, then a minute at a time, moa's agent answering all it is asked, until
    /// nothing waits.
    fn drain(&mut self) -> Result<(), TestCaseError> {
        self.set_auto(None)?;
        for _ in 0..50 {
            if self.st.reports().next().is_none() && self.st.yeses().next().is_none() {
                break;
            }
            self.step(Step::Advance(60))?;
            for _ in 0..20 {
                self.flush()?;
                if self.out.is_empty() {
                    break;
                }
                self.answer()?;
            }
        }
        self.flush()
    }

    /// What the disk holds at the end: no report or yes waits, no token is in two `you` rows,
    /// each token kept is in one, and each pushed out or never saved is in none.
    fn check(&self) -> Result<(), TestCaseError> {
        let disk = self.disk.snapshot();
        prop_assert!(disk.reports.is_empty(), "reports still wait");
        prop_assert!(
            disk.suggestions
                .iter()
                .all(|s| s.result.as_deref() != Some(YES_WAITING)),
            "yeses still wait"
        );
        prop_assert!(
            self.expected.queues.values().all(Vec::is_empty),
            "never carried: {:?}",
            self.expected.queues
        );
        prop_assert!(
            self.expected.yeses.is_empty(),
            "never told: {:?}",
            self.expected.yeses
        );
        let mut rows: BTreeMap<String, usize> = BTreeMap::new();
        for body in self.disk.you_bodies() {
            for (t, n) in row_tokens(&body) {
                *rows.entry(t).or_default() += n;
            }
        }
        for (t, n) in &rows {
            prop_assert_eq!(*n, 1, "{} is {} times in the rows", t, n);
        }
        for t in &self.expected.carried {
            prop_assert!(rows.contains_key(t), "{} is in no row", t);
        }
        for t in &self.expected.gone {
            prop_assert!(
                !rows.contains_key(t),
                "{} was pushed out, or never saved",
                t
            );
        }
        Ok(())
    }
}

/// Task `id` of [`DUE_TASKS`]: open, titled `due-<id>`, with no due date yet.
fn due_task(id: i64) -> Task {
    Task {
        id: TaskId(id),
        project_id: None,
        title: format!("due-{id}"),
        status: TaskStatus::Todo,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: None,
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    }
}

/// Each of [`DUE_TASKS`] that prompt `text`, sent on `day`, tells is due: its title, its due
/// date (from its line's lateness) and `day`.
fn told_due(text: &str, day: Date) -> Vec<(String, Date, Date)> {
    let mut told = Vec::new();
    for line in text.lines() {
        let Some((label, titles)) = line.strip_prefix("⏰ ").and_then(|l| l.split_once(": "))
        else {
            continue;
        };
        let late: i64 = match label {
            "Due tomorrow" => -1,
            "Due today" => 0,
            "1 day overdue" => 1,
            l => l.trim_end_matches(" days overdue").parse().unwrap(),
        };
        let due = day.checked_sub(late.days()).unwrap();
        let ours = titles.split("; ").filter(|t| t.starts_with("due-"));
        told.extend(ours.map(|t| (t.to_string(), due, day)));
    }
    told
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever the sessions report, whatever you prompt or say yes to, whatever comes due,
    /// however the days turn and wherever the daemon restarts, each report that isn't pushed
    /// out, and each yes, reaches moa's agent in exactly one saved row (Review focus 2:
    /// `a_yes_reaches_moa_once`); and each (task, due date, day) is in at most one prompt
    /// sent (Review focus 3: `nags_are_once_a_day`).
    #[test]
    fn a_yes_reaches_moa_once(steps in prop::collection::vec(step(), 1..=60)) {
        let mut d = Daemon::new()?;
        for s in steps {
            d.step(s)?;
        }
        d.drain()?;
        d.check()?;
    }
}

/// The crash the property may or may not draw: right after the commit that saves your
/// prompt's row, before the prompt reaches moa's agent. The report went with that commit: it
/// is carried once, and none is left.
#[test]
fn a_restart_right_after_the_prompt_commit_loses_nothing() -> Result<(), TestCaseError> {
    let mut d = Daemon::new()?;
    for s in [Step::Turn(0), Step::Prompt, Step::Answer] {
        d.step(s)?;
    }
    // The last input, the thread opened, saved the row and sent the prompt.
    assert_eq!(prompts_sent(&d.last).len(), 1);
    let saves_you = |e: &Effect| {
        matches!(e, Effect::Persist(c)
            if c.iter().any(|c| matches!(c, Change::Feed(r) if r.kind == "you")))
    };
    let mut commits = d.last.iter().filter(|e| matches!(e, Effect::Persist(_)));
    let k = 1 + commits.position(saves_you).expect("the row is saved");
    d.restart_after(k)?;
    let rows: Vec<BTreeMap<String, usize>> =
        d.disk.you_bodies().iter().map(|b| row_tokens(b)).collect();
    let carrying: Vec<usize> = rows
        .iter()
        .filter_map(|r| r.get("tok-0001"))
        .copied()
        .collect();
    assert_eq!(carrying, [1]);
    assert!(d.disk.snapshot().reports.is_empty());
    assert_eq!(d.st.reports().count(), 0);
    d.drain()?;
    d.check()
}
