//! The session reports queue in the core: which hook events queue a report, what the row
//! holds, and the one channel that carries them to moa's agent: a wake-up once they settle,
//! or your next prompt. Every input goes through `handle_checked`.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::central::{Disk, acps, caps, hear_at, opened, prompt_at, prompts_sent, started as up};
use jiff::tz::TimeZone;
use moa_core::central::channel::AUTO_KEY;
use moa_core::central::reports::{EARLIER_MAX, LAST_MAX, TURNS_KEPT};
use moa_core::central::wire::{AcpCmd, AcpIn, Opened, ThreadHow};
use moa_core::central::{PROMPT_MAX, Phase};
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, RefuseWrites, TimerKey, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, FeedEntry, Lifecycle, Location, Project,
    ProjectId, ProjectStatus, Report, ReportKind, Session, SessionId,
};
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::publish::Topic;
use moa_core::state::{Deps, LoadNote, Snapshot, State};
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
fn load(config: Config, deps: Deps, snapshot: Snapshot, now: Now) -> (State, Vec<Effect>) {
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
    let persists: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect();
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

/// The `Wake` timer as `fx` leaves it: `Some(Some(at))` set, `Some(None)` cancelled, `None`
/// untouched.
fn wake_timer(fx: &[Effect]) -> Option<Option<UtcMs>> {
    common::timers(fx, TimerKey::Wake).last().map(|e| match e {
        Effect::SetTimer { at, .. } => Some(*at),
        _ => None,
    })
}

/// The `Wake` timer fires at `ms`.
fn fire(st: &mut State, ms: i64) -> Vec<Effect> {
    handle_checked(st, Input::Timer(TimerKey::Wake), at(ms))
}

/// moa's agent answers at `ms` what `fx` asks of it: it starts, and opens a new thread, as
/// each is asked for. Every effect, `fx`'s first.
fn play(st: &mut State, ms: i64, mut fx: Vec<Effect>) -> Vec<Effect> {
    let mut i = 0;
    while i < fx.len() {
        let heard = match &fx[i] {
            Effect::Acp(AcpCmd::Start { generation, .. }) => {
                Some(up(*generation, caps(true, true, true)))
            }
            Effect::Acp(AcpCmd::Open { generation, .. }) => Some(opened(
                *generation,
                &format!("thread-{generation}"),
                Opened::New,
            )),
            _ => None,
        };
        if let Some(input) = heard {
            let more = hear_at(st, at(ms), input);
            fx.extend(more);
        }
        i += 1;
    }
    fx
}

/// The turn under way ends at `ms`.
fn end_turn(st: &mut State, ms: i64) -> Vec<Effect> {
    let generation = st.central().generation;
    let prompt = st.central().turn.as_ref().expect("a turn").prompt;
    hear_at(st, at(ms), common::central::ended(generation, prompt))
}

/// The one prompt `fx` sends.
fn sent(fx: &[Effect]) -> String {
    match prompts_sent(fx)[..] {
        [text] => text.to_string(),
        ref other => panic!("{} prompts sent", other.len()),
    }
}

/// The `you` row `fx` saves.
fn you_row(fx: &[Effect]) -> &FeedEntry {
    common::central::feed_rows(fx)
        .into_iter()
        .find(|r| r.kind == "you")
        .expect("a you row")
}

fn body_of(row: &FeedEntry) -> Value {
    serde_json::from_str(&row.body).unwrap()
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

// ---------- exactly once, across restarts ----------

/// The world of [`each_report_reaches_moa_exactly_once`]: settle, gap and batch small.
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
    /// `/auto` is flipped: by hand on the disk, then a restart, until `/auto` is an op.
    Auto,
    /// The next input's writes are refused.
    Refuse,
    /// The daemon restarts after the first k of the last input's commits reach the disk, k
    /// from 0 to all of them (this index picks it): the rest are lost, and nothing it said
    /// reaches moa's agent.
    Restart(prop::sample::Index),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        6 => (0..4usize).prop_map(Step::Turn),
        2 => (0..4usize).prop_map(Step::NeedsYou),
        1 => Just(Step::Prompt),
        3 => (0..=60i64).prop_map(Step::Advance),
        3 => Just(Step::Answer),
        1 => Just(Step::Auto),
        1 => Just(Step::Refuse),
        2 => any::<prop::sample::Index>().prop_map(Step::Restart),
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

/// A hook event the expected queue takes once its commit reaches the disk.
struct Event {
    session: usize,
    kind: ReportKind,
    token: String,
}

/// The queue as K2's rules alone keep it, commit by commit as the disk takes them.
#[derive(Default)]
struct Expected {
    /// Each session's tokens of each kind, waiting.
    queues: HashMap<(usize, ReportKind), Vec<String>>,
    /// Carried in a saved `you` row.
    carried: BTreeSet<String>,
    /// Pushed out by a 7th turn, replaced by a newer need, or never saved: no row has them.
    gone: BTreeSet<String>,
}

impl Expected {
    fn queue(&mut self, e: Event) {
        let q = self.queues.entry((e.session, e.kind)).or_default();
        match e.kind {
            ReportKind::TurnEnded => {
                q.push(e.token);
                if q.len() > TURNS_KEPT {
                    self.gone.insert(q.remove(0));
                }
            }
            ReportKind::NeedsYou => {
                self.gone.extend(q.drain(..));
                q.push(e.token);
            }
        }
    }

    /// A `you` row carrying `tokens` is saved: each was waiting, and each report it takes
    /// goes whole.
    fn carry(&mut self, tokens: &BTreeMap<String, usize>) -> Result<(), TestCaseError> {
        for t in tokens.keys() {
            prop_assert!(
                self.queues.values().any(|q| q.contains(t)),
                "{} is carried, but isn't waiting: carried twice, or never queued",
                t
            );
        }
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
    auto: bool,
    /// The last hook's `seq`, token, request and thread.
    seq: u32,
    token: u32,
    req: u64,
    thread: u32,
    expected: Expected,
}

impl Daemon {
    fn new() -> Result<Daemon, TestCaseError> {
        let b = TypicalBrain::new();
        let disk = Disk::new(&b, SMALL);
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
            auto: true,
            seq: 0,
            token: 0,
            req: 0,
            thread: 0,
            expected: Expected::default(),
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
    /// agent holds exactly the tokens of the row saved with it, each once.
    fn execute(&mut self, commits: Option<usize>) -> Result<(), TestCaseError> {
        let mut event = self.event.take();
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
        if let Some(e) = event {
            self.expected.gone.insert(e.token);
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
                self.event = Some(Event {
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

    /// `/auto` set by hand on the disk, `None` for on, and the daemon restarted to read it.
    fn set_auto(&mut self, value: Option<&str>) -> Result<(), TestCaseError> {
        self.flush()?;
        self.auto = value.is_none();
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
                if self.wake_at.is_some_and(|w| w <= self.now) {
                    self.next()?;
                    // The scheduler forgets a timer that fired.
                    self.wake_at = None;
                    self.run(Input::Timer(TimerKey::Wake));
                }
                Ok(())
            }
            Step::Answer => self.answer(),
            Step::Auto => match self.auto {
                true => self.set_auto(Some("off")),
                false => self.set_auto(None),
            },
            Step::Refuse => {
                self.refuse_next = true;
                Ok(())
            }
            Step::Restart(k) => {
                let k = k.index(self.commits() + 1);
                self.restart_after(k)
            }
        }
    }

    /// `/auto` on, then a minute at a time, moa's agent answering all it is asked, until
    /// nothing waits.
    fn drain(&mut self) -> Result<(), TestCaseError> {
        self.set_auto(None)?;
        for _ in 0..50 {
            if self.st.reports().next().is_none() {
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

    /// What the disk holds at the end: no report waits, no token is in two `you` rows, each
    /// token kept is in one, and each pushed out or never saved is in none.
    fn check(&self) -> Result<(), TestCaseError> {
        prop_assert!(
            self.disk.snapshot().reports.is_empty(),
            "reports still wait"
        );
        prop_assert!(
            self.expected.queues.values().all(Vec::is_empty),
            "never carried: {:?}",
            self.expected.queues
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever the sessions report, whatever you prompt and wherever the daemon restarts,
    /// each report that isn't pushed out reaches moa's agent in exactly one saved row.
    #[test]
    fn each_report_reaches_moa_exactly_once(steps in prop::collection::vec(step(), 1..=60)) {
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
