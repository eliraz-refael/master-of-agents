//! The core's skeleton: `handle` over a small hand-built snapshot.

use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{Timestamp, civil::date};
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Effect, Input, LogLevel, LogLine, Publish, RefuseWrites, TimerKey, check_order, handle_checked,
    reply_of,
};
use moa_core::model::{ActivityWho, EventKind, Group};
use moa_core::op::{ConnId, NoParams, Op, OpError, ReqId};
use moa_core::publish::{CoreEvent, Model, Topic};
use moa_core::state::{CallerInfo, LoadNote, NextIds, Snapshot, State};
use moa_core::{Now, UtcMs};

fn est() -> TimeZone {
    TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()
}

fn ts(s: &str) -> UtcMs {
    UtcMs(s.parse::<Timestamp>().unwrap().as_millisecond())
}

/// 2026-11-15 12:00 EST.
fn noon() -> Now {
    at(ts("2026-11-15T17:00:00Z"))
}

fn at(wall: UtcMs) -> Now {
    Now::at(wall, Duration::from_secs(1), est())
}

fn snapshot() -> Snapshot {
    Snapshot {
        groups: vec![Group {
            path: "Existing".into(),
            collapsed: false,
            created_at: UtcMs(1),
        }],
        next_ids: NextIds {
            event: 5,
            activity: 7,
            ..NextIds::default()
        },
        ..Snapshot::default()
    }
}

fn empty_state() -> State {
    State::new(BrainName::default_brain(), Config::default())
}

fn started() -> State {
    let mut s = empty_state();
    handle_checked(&mut s, Input::Startup(Box::new(snapshot())), noon());
    s
}

fn group_add(state: &mut State, path: &str, now: Now) -> Vec<Effect> {
    handle_checked(
        state,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(1),
            caller: CallerInfo::default(),
            op: Op::GroupAdd { path: path.into() },
        },
        now,
    )
}

fn models(effects: &[Effect]) -> Vec<&Arc<Model>> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => Some(m),
            _ => None,
        })
        .collect()
}

fn persists(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::Persist(_)))
        .count()
}

#[test]
fn startup_arms_the_backup_and_publishes() {
    let mut s = empty_state();
    let fx = handle_checked(&mut s, Input::Startup(Box::new(snapshot())), noon());
    assert!(fx.contains(&Effect::Backup {
        day: date(2026, 11, 15)
    }));
    assert!(fx.contains(&Effect::SetTimer {
        key: TimerKey::Backup,
        at: ts("2026-11-16T05:00:00Z")
    }));
    assert_eq!(models(&fx).len(), 1);
    assert_eq!(models(&fx)[0].rev, 1);
    assert!(models(&fx)[0].topics[&Topic::Groups].contains_key("Existing"));
}

#[test]
fn startup_logs_each_unknown_row_without_contents() {
    let mut snap = snapshot();
    snap.unknown = vec![
        LoadNote {
            table: "groups",
            key: "Private".into(),
            reason: "x",
        },
        LoadNote {
            table: "tasks",
            key: "#3".into(),
            reason: "x",
        },
    ];
    let mut s = empty_state();
    let fx = handle_checked(&mut s, Input::Startup(Box::new(snap)), noon());
    let logs: Vec<&LogLine> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|l| l.level == LogLevel::Warn));
    assert!(logs.iter().all(|l| !l.message.contains("Private")));
    assert!(logs[1].message.contains("tasks") && logs[1].message.contains("#3"));
}

#[test]
fn reload_only_replaces_the_state() {
    let mut s = started();
    let before = s.model().rev;
    let fx = handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::new(snapshot()),
            refuse_writes: None,
        },
        noon(),
    );
    assert_eq!(models(&fx).len(), 1);
    assert_eq!(models(&fx)[0].rev, before + 1);
    assert_eq!(models(&fx)[0].topics.len(), Topic::ALL.len());
    assert!(fx.contains(&Effect::SetTimer {
        key: TimerKey::Backup,
        at: ts("2026-11-16T05:00:00Z")
    }));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Backup { .. })));
    assert_eq!(persists(&fx), 0);
}

#[test]
fn the_backup_timer_backs_up_and_re_arms() {
    let mut s = started();
    let fx = handle_checked(
        &mut s,
        Input::Timer(TimerKey::Backup),
        at(ts("2026-11-16T05:00:00Z")),
    );
    assert_eq!(
        fx,
        vec![
            Effect::Backup {
                day: date(2026, 11, 16)
            },
            Effect::SetTimer {
                key: TimerKey::Backup,
                at: ts("2026-11-17T05:00:00Z")
            }
        ]
    );
}

#[test]
fn a_zone_change_re_arms_with_the_new_zone() {
    let mut s = started();
    let now = Now::at(
        ts("2026-11-15T17:00:00Z"),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    );
    let fx = handle_checked(&mut s, Input::ZoneChanged, now);
    assert_eq!(
        fx,
        vec![Effect::SetTimer {
            key: TimerKey::Backup,
            at: ts("2026-11-16T00:00:00Z")
        }]
    );
}

#[test]
fn group_add_persists_then_publishes_then_replies() {
    let mut s = started();
    let rev = s.model().rev;
    let now = noon();
    let fx = group_add(&mut s, "Lantern/Docs", now.clone());
    assert_eq!(fx.len(), 4, "{fx:?}");
    let Effect::Persist(changes) = &fx[0] else {
        panic!("{fx:?}")
    };
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes[0],
        Change::Group(Group {
            path: "Lantern/Docs".into(),
            collapsed: false,
            created_at: now.wall
        })
    );
    let Change::Event(ev) = &changes[1] else {
        panic!("{changes:?}")
    };
    assert_eq!(ev.id.0, 5);
    assert_eq!(ev.kind.0, EventKind::UI);
    assert_eq!(ev.text.as_deref(), Some("new group Lantern/Docs"));
    let Change::Activity(a) = &changes[2] else {
        panic!("{changes:?}")
    };
    assert_eq!(a.id.0, 7);
    assert_eq!(a.who, ActivityWho::You);
    assert_eq!(a.text, "new group Lantern/Docs");

    let Effect::Publish(Publish::Model(m)) = &fx[1] else {
        panic!("{fx:?}")
    };
    assert_eq!(m.rev, rev + 1);
    assert!(m.topics[&Topic::Groups].contains_key("Lantern/Docs"));
    assert_eq!(
        fx[2],
        Effect::Publish(Publish::Event(CoreEvent::Ui {
            text: "new group Lantern/Docs".into()
        }))
    );
    assert!(matches!(fx[3], Effect::Reply { .. }));
    let reply = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(reply.text, "ok");
    assert_eq!(reply.data, serde_json::json!({"path": "Lantern/Docs"}));

    // The ids advanced: the next write takes the next ones.
    let fx = group_add(&mut s, "Tidepool", now);
    let Effect::Persist(changes) = &fx[0] else {
        panic!("{fx:?}")
    };
    let (Change::Event(ev), Change::Activity(a)) = (&changes[1], &changes[2]) else {
        panic!("{changes:?}")
    };
    assert_eq!((ev.id.0, a.id.0), (6, 8));
}

#[test]
fn adding_an_existing_group_changes_nothing_but_logs() {
    let mut s = started();
    let fx = group_add(&mut s, "/Existing/", noon());
    let Effect::Persist(changes) = &fx[0] else {
        panic!("{fx:?}")
    };
    assert_eq!(changes.len(), 2);
    assert!(matches!(changes[0], Change::Event(_)));
    assert!(matches!(changes[1], Change::Activity(_)));
    assert!(reply_of(&fx).unwrap().is_ok());
    assert_eq!(s.groups().count(), 1);
}

#[test]
fn an_empty_group_name_is_refused_and_nothing_is_applied() {
    let mut s = started();
    let rev = s.model().rev;
    let fx = group_add(&mut s, "//", noon());
    assert_eq!(persists(&fx), 0);
    assert!(models(&fx).is_empty());
    assert_eq!(s.model().rev, rev);
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "a group needs a name"
    );
}

#[test]
fn refused_writes_change_nothing_until_the_deadline() {
    let mut s = started();
    let now = noon();
    let until = UtcMs(now.wall.0 + 2_000);
    handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::new(snapshot()),
            refuse_writes: Some(RefuseWrites {
                until,
                reason: "disk full".into(),
            }),
        },
        now.clone(),
    );
    let fx = group_add(&mut s, "X", now);
    assert_eq!(persists(&fx), 0);
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        "moa can't save right now (disk full); nothing was changed"
    );
    assert!(s.groups().all(|g| g.path != "X"));
    let fx = group_add(&mut s, "X", at(until));
    assert_eq!(persists(&fx), 1);
    assert!(reply_of(&fx).unwrap().is_ok());
}

#[test]
fn check_order_rejects_a_publish_before_a_persist() {
    let publish = Effect::Publish(Publish::Event(CoreEvent::Ui { text: "x".into() }));
    let persist = Effect::Persist(vec![]);
    assert!(check_order(&[persist.clone(), publish.clone()]).is_ok());
    assert!(check_order(&[publish.clone(), persist.clone()]).is_err());
    let reply = Effect::Reply {
        conn: ConnId(1),
        req: ReqId(1),
        result: Err(OpError("no".into())),
    };
    assert!(check_order(&[reply, persist]).is_err());
    assert!(check_order(&[publish]).is_ok());
}

#[test]
fn only_timers_and_logs_run_at_once() {
    let yes = [
        Effect::SetTimer {
            key: TimerKey::Backup,
            at: UtcMs(1),
        },
        Effect::CancelTimer(TimerKey::Backup),
        Effect::Log(LogLine {
            level: LogLevel::Info,
            message: "x".into(),
        }),
    ];
    let no = [
        Effect::Persist(vec![]),
        Effect::Publish(Publish::Model(Arc::new(Model::default()))),
        Effect::Reply {
            conn: ConnId(1),
            req: ReqId(1),
            result: Err(OpError("no".into())),
        },
        Effect::Backup {
            day: date(2026, 11, 15),
        },
    ];
    assert!(yes.iter().all(Effect::runs_at_once));
    assert!(no.iter().all(|e| !e.runs_at_once()));
}

#[test]
fn actor_failure_is_published() {
    let mut s = started();
    let fx = handle_checked(
        &mut s,
        Input::ActorFailed {
            actor: "store",
            message: "secret detail".into(),
        },
        noon(),
    );
    assert_eq!(
        fx[0],
        Effect::Publish(Publish::Event(CoreEvent::Error {
            message: "store stopped and was restarted".into()
        }))
    );
    assert!(matches!(&fx[1], Effect::Log(_)));
    assert!(fx.iter().all(|e| match e {
        Effect::Log(l) => !l.message.contains("secret"),
        _ => true,
    }));
}

#[test]
fn op_methods_match_the_variants() {
    let samples = [
        Op::GroupAdd { path: "A".into() },
        Op::GroupRename {
            old: "A".into(),
            new: "B".into(),
        },
        Op::GroupRm { path: "A".into() },
        Op::GroupFold {
            path: "A".into(),
            collapsed: true,
        },
        Op::SessionLs { all: true },
        Op::SessionSet {
            q: "q".into(),
            title: Some("t".into()),
            group: None,
        },
        Op::SessionArchive {
            qs: vec!["a".into(), "b".into()],
        },
        Op::TaskLs {
            project: Some("P".into()),
            all: true,
        },
        Op::TaskAdd(moa_core::op::TaskAdd {
            project: "P".into(),
            title: "t".into(),
            due: Some("fri".into()),
            ..Default::default()
        }),
        Op::TaskSet(moa_core::op::TaskSet {
            id: 1,
            not_today: true,
            ..Default::default()
        }),
        Op::NoteAdd {
            text: "n".into(),
            project: None,
            kind: Some("fact".into()),
        },
        Op::ProjectShow { name: "P".into() },
        Op::ProjectAdd {
            name: "P".into(),
            path: "/x".into(),
            summary: None,
        },
        Op::SessionFind { q: "q".into() },
        Op::SessionOpen {
            path: "/x".into(),
            account: Some("acct-a".into()),
            title: None,
            group: None,
            prompt: Some("p".into()),
            bg: true,
        },
        Op::SessionFocus { q: "q".into() },
        Op::SessionResume {
            q: "q".into(),
            bg: true,
        },
        Op::SessionClose { q: "q".into() },
        Op::SessionSleep {
            qs: vec!["q".into()],
            idle_ms: Some(1),
            dry_run: true,
        },
        Op::SessionAdopt {
            pane: "%5".into(),
            title: Some("t".into()),
            group: None,
            id: Some("c".into()),
        },
        Op::SessionRestart { q: "q".into() },
        Op::TermOpen {
            path: "/x".into(),
            title: Some("t".into()),
        },
        Op::TermShow { pane: "%5".into() },
        Op::TermLs(NoParams),
        Op::TermName {
            target: "#7".into(),
            name: "Kite server".into(),
        },
        Op::TermNote {
            target: "%5".into(),
            note: "waiting on Harbor".into(),
        },
        Op::TermClose {
            target: "Kite".into(),
        },
        Op::TermTimeout {
            target: "7".into(),
            after_ms: Some(3_600_000),
        },
        Op::Brief(NoParams),
        Op::Attention(NoParams),
        Op::Suggest {
            text: "t".into(),
            actions: vec!["note x".into()],
        },
        Op::SuggestionAnswer {
            id: Some(2),
            answer: moa_core::op::Answer::Later,
        },
        Op::SuggestionLs(NoParams),
        Op::TmuxEnsure {
            tmux: Some("/usr/bin/tmux".into()),
        },
        Op::UiPanel {
            pane: Some("%3".into()),
            view: Some(moa_core::panel::View::Tree),
            placed: false,
            once: false,
        },
        Op::UiHere {
            folder: Some("/x".into()),
        },
        Op::UiClose(NoParams),
        Op::LayoutList(NoParams),
        Op::LayoutShow {
            name: "cockpit".into(),
        },
        Op::LayoutSet {
            name: "mine".into(),
            layout: serde_json::json!({"panel": "tree"}),
        },
        Op::LayoutApply {
            name: Some("focus".into()),
        },
        Op::Goto {
            panel: moa_core::layout::Leaf::Agent,
        },
        Op::UiSelectView {
            pane: "%3".into(),
            view: moa_core::panel::View::Plan,
        },
        Op::ConsolePrompt {
            text: "what's open in Kite?".into(),
            wait: true,
        },
        Op::ConsoleCancel(NoParams),
        Op::ConsolePage {
            before: 201,
            n: 100,
        },
        Op::ConsoleNew(NoParams),
        Op::ConsoleCommand { line: "/ls".into() },
    ];
    // `DebugPanic` is the one variant that is not on the wire: it isn't in METHODS and can't
    // be serialized or parsed.
    assert_eq!(Op::DebugPanic.method(), "debug.panic");
    assert!(!Op::METHODS.contains(&"debug.panic"));
    assert!(serde_json::to_value(Op::DebugPanic).is_err());
    assert!(serde_json::from_str::<Op>(r#"{"method":"debug.panic"}"#).is_err());
    assert_eq!(Op::METHODS.len(), samples.len());
    for op in &samples {
        let v = serde_json::to_value(op).unwrap();
        let m = v["method"].as_str().unwrap();
        assert!(Op::METHODS.contains(&m), "{m}");
        assert_eq!(m, op.method());
        // S17: the owner's list of what writes, by method name.
        assert_eq!(op.writes(), WRITES.contains(&m), "{m}");
        assert_eq!(&serde_json::from_value::<Op>(v).unwrap(), op);
    }
    assert!(WRITES.iter().all(|w| Op::METHODS.contains(w)));
    assert!(!Op::DebugPanic.writes());
}

/// Every op that changes the brain (the rest only read it).
const WRITES: &[&str] = &[
    "group.add",
    "group.rename",
    "group.rm",
    "group.fold",
    "session.set",
    "session.archive",
    "session.open",
    "session.focus",
    "session.resume",
    "session.close",
    "session.sleep",
    "session.adopt",
    "session.restart",
    "term.name",
    "term.note",
    "term.close",
    "term.timeout",
    "task.add",
    "task.set",
    "note.add",
    "project.add",
    "suggest",
    "suggestion.answer",
    "layout.set",
    "layout.apply",
    "console.prompt",
    "console.new",
    "console.command",
];

#[test]
fn session_ops_take_their_flags_as_optional() {
    let ls: Op = serde_json::from_str(r#"{"method":"session.ls","params":{}}"#).unwrap();
    assert_eq!(ls, Op::SessionLs { all: false });
    let set: Op = serde_json::from_str(r#"{"method":"session.set","params":{"q":"x"}}"#).unwrap();
    assert_eq!(
        set,
        Op::SessionSet {
            q: "x".into(),
            title: None,
            group: None
        }
    );
    let prompt: Op =
        serde_json::from_str(r#"{"method":"console.prompt","params":{"text":"hi"}}"#).unwrap();
    assert_eq!(
        prompt,
        Op::ConsolePrompt {
            text: "hi".into(),
            wait: false
        }
    );
    let open: Op =
        serde_json::from_str(r#"{"method":"session.open","params":{"path":"/x"}}"#).unwrap();
    assert_eq!(
        open,
        Op::SessionOpen {
            path: "/x".into(),
            account: None,
            title: None,
            group: None,
            prompt: None,
            bg: false,
        }
    );
}

#[test]
fn ops_without_params_take_missing_null_or_empty_params() {
    for (method, want) in [
        ("brief", Op::Brief(NoParams)),
        ("attention", Op::Attention(NoParams)),
        ("suggestion.ls", Op::SuggestionLs(NoParams)),
        ("layout.list", Op::LayoutList(NoParams)),
        ("ui.close", Op::UiClose(NoParams)),
    ] {
        for text in [
            format!(r#"{{"method":"{method}"}}"#),
            format!(r#"{{"method":"{method}","params":null}}"#),
            format!(r#"{{"method":"{method}","params":{{}}}}"#),
        ] {
            let got: Op = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(got, want, "{text}");
        }
        // It encodes with empty params, and a field it doesn't take is refused.
        assert_eq!(
            serde_json::to_value(&want).unwrap(),
            serde_json::json!({ "method": method, "params": {} })
        );
        let extra = format!(r#"{{"method":"{method}","params":{{"x":1}}}}"#);
        assert!(serde_json::from_str::<Op>(&extra).is_err(), "{extra}");
    }
}

#[test]
fn done_round_trips() {
    let done = moa_core::op::Done {
        text: "task #4 added".into(),
        data: serde_json::json!({ "id": 4, "project": "Lantern", "tags": [] }),
    };
    let v = serde_json::to_value(&done).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "text": "task #4 added", "data": { "id": 4, "project": "Lantern", "tags": [] } })
    );
    assert_eq!(
        serde_json::from_value::<moa_core::op::Done>(v).unwrap(),
        done
    );
}
