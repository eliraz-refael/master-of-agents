//! The daemon's upgrade in the core (L4): it waits for moa's turn to end, goes out once, and
//! after that no new turn of moa's starts. The next start says what the upgrade needs (L7).

mod common;

use common::central::{ended, first_turn, moa, prompt, thread, toml};
use common::{at, feed, link_up, listed, replies};
use moa_core::UtcMs;
use moa_core::central::feed::kind;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Down, Effect, Input, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::model::{AgentKind, Inspected, Lifecycle, Location};
use moa_core::op::OpError;
use moa_core::state::{Deps, LoadNote, Snapshot, State};
use moa_core::upgrade::{
    BUILD_KEY, NOTICE_NOTHING, NOTICE_REATTACH, NOTICE_RESTARTED, UPGRADING, UpgradeFacts, WAITING,
    Why, notice_hooks, status,
};
use moa_testkit::brain::TypicalBrain;

fn upgrade(b: &TypicalBrain, st: &mut State, why: Why) -> Vec<Effect> {
    handle_checked(st, Input::Upgrade(why), b.now())
}

fn execs(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::Exec)).count()
}

/// Wanted mid-turn, the upgrade waits (and says so); a second want changes nothing; the
/// turn's end lets it go, once.
#[test]
fn an_upgrade_waits_for_moas_turn_then_goes_once() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    first_turn(&mut st, &b.now(), "th-1", false);
    assert_eq!(status(&st), None);

    assert_eq!(execs(&upgrade(&b, &mut st, Why::Rebuilt)), 0);
    assert_eq!(status(&st), Some(WAITING));
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 0);

    let generation = st.central().generation;
    let fx = handle_checked(&mut st, Input::Acp(ended(generation, 1)), b.now());
    assert_eq!(execs(&fx), 1, "{fx:?}");
    assert_eq!(status(&st), None, "it has gone out");
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 0, "once");
}

/// M1: once the exec has gone out, a prompt is refused rather than starting a turn the
/// wind-down would cut off.
#[test]
fn a_prompt_after_the_exec_is_refused() {
    let b = TypicalBrain::new();
    let mut st = moa(&b);
    assert_eq!(execs(&upgrade(&b, &mut st, Why::Asked)), 1);
    let fx = prompt(&b, &mut st, 2, "are you there?");
    let got = replies(&fx);
    assert_eq!(got.len(), 1, "{fx:?}");
    assert_eq!(got[0].1, Err(OpError(UPGRADING.into())));
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Acp(_))),
        "moa's agent isn't started: {fx:?}"
    );
}

// --- Restart notices (L7, §6.15.5) ---------------------------------------------------------

/// The build saved by the last start, and this image's.
const OLD: &str = "build-old";
const NEW: &str = "build-new";

fn facts(reexec: bool, plugin_changed: bool, reattach: bool) -> UpgradeFacts {
    UpgradeFacts {
        build: NEW.into(),
        reexec,
        plugin_changed,
        reattach,
    }
}

/// What a start loads: the typical brain with `live` of its Claude sessions running
/// elsewhere (so reconcile leaves them live) and the rest ended, `saved` as the last start's
/// build, and moa's one thread `th-1` when `thread_too` (none otherwise).
fn snapshot(b: &TypicalBrain, saved: Option<&str>, thread_too: bool, live: usize) -> Snapshot {
    let mut snap = b.snapshot();
    let mut left = live;
    for s in &mut snap.sessions {
        if left > 0 && s.agent_kind == AgentKind::Claude {
            left -= 1;
            s.lifecycle = Lifecycle::Live;
            s.location = Location::Elsewhere;
            s.archived_at = None;
        } else {
            s.lifecycle = Lifecycle::Ended;
        }
    }
    assert_eq!(left, 0, "{live} Claude sessions to make live");
    snap.meta
        .extend(saved.map(|v| (BUILD_KEY.to_string(), v.to_string())));
    snap.threads.clear();
    snap.feed_tail.clear();
    snap.feed_keys.clear();
    if thread_too {
        let started = b.now().wall.0 - 60_000;
        snap.threads
            .push(thread("th-1", "acct-a", b.now().local_day, started));
    }
    snap
}

/// The daemon's core with `facts` (and `deps`, at the brain's home), not yet started.
fn fresh(b: &TypicalBrain, deps: Deps, facts: UpgradeFacts) -> State {
    let config = Config::parse(&toml("acct-a"), &b.home).unwrap();
    let deps = Deps {
        home: b.home.clone(),
        ..deps
    };
    State::new(BrainName::default_brain(), config)
        .with_deps(deps)
        .with_upgrade(facts)
}

/// The daemon starts on `snap` with `facts`, links and drains its start: its reconcile's
/// listing waits.
fn drained(b: &TypicalBrain, snap: Snapshot, facts: UpgradeFacts) -> (State, Vec<Effect>) {
    let mut st = fresh(b, Deps::default(), facts);
    let mut fx = handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    link_up(b, &mut st);
    fx.extend(handle_checked(&mut st, Input::StartDrained, b.now()));
    (st, fx)
}

/// [`drained`], then the listing answered with no tabs: what the whole start said.
fn started(b: &TypicalBrain, snap: Snapshot, facts: UpgradeFacts) -> (State, Vec<Effect>) {
    let (mut st, mut fx) = drained(b, snap, facts);
    fx.extend(listed(&mut st, b, &[], &[]));
    assert!(st.tmux().reconciled());
    (st, fx)
}

/// The activity rows `fx` saves: their texts.
fn activity(fx: &[Effect]) -> Vec<String> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

/// The `system` lines `fx` saves: `(thread, text)`.
fn system(fx: &[Effect]) -> Vec<(String, String)> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Feed(r) if r.kind == kind::SYSTEM => {
                let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                Some((
                    r.thread_id.clone(),
                    body["text"].as_str().unwrap().to_string(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The builds `fx` saves as `meta.daemon.build`.
fn builds(fx: &[Effect]) -> Vec<Option<String>> {
    common::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Meta { key, value } if key == BUILD_KEY => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// The one commit that saves the build: the notice is in it.
fn build_commit(fx: &[Effect]) -> &Vec<Change> {
    let commits: Vec<&Vec<Change>> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .filter(|c| {
            c.iter()
                .any(|c| matches!(c, Change::Meta { key, .. } if key == BUILD_KEY))
        })
        .collect();
    assert_eq!(commits.len(), 1, "{fx:?}");
    commits[0]
}

/// `text` said once: a `system` line in moa's thread and an activity row, in the commit
/// that saves the new build.
fn said_once(fx: &[Effect], text: &str) {
    assert_eq!(activity(fx), [text], "{fx:?}");
    assert_eq!(system(fx), [("th-1".to_string(), text.to_string())]);
    assert_eq!(builds(fx), [Some(NEW.to_string())]);
    let commit = build_commit(fx);
    assert!(commit.iter().any(|c| matches!(c, Change::Activity(_))));
    assert!(
        commit
            .iter()
            .any(|c| matches!(c, Change::Feed(r) if r.kind == kind::SYSTEM))
    );
}

/// An input at `ms`: the status clock's.
fn clock(st: &mut State, ms: i64) -> Vec<Effect> {
    handle_checked(st, Input::Timer(TimerKey::Clock), at(UtcMs(ms)))
}

/// tmux answers the reconcile's listing waiting, at `ms`, with no tabs.
fn no_tabs(st: &mut State, ms: i64) -> Vec<Effect> {
    let asked: Vec<u64> = st.tmux().queries().collect();
    let [n] = asked[..] else {
        panic!("one pane query waiting, not {asked:?}");
    };
    let t = TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results: vec![Ok(vec![]), Ok(vec![])],
    };
    handle_checked(st, Input::Tmux(t), at(UtcMs(ms)))
}

/// An input a second after `ms`: nothing more is said.
fn nothing_more(st: &mut State, ms: i64) {
    let fx = clock(st, ms + 1_000);
    assert!(activity(&fx).is_empty() && builds(&fx).is_empty(), "{fx:?}");
}

/// An upgrade in place: the same pid, nothing else to do.
#[test]
fn a_reexec_says_nothing_to_do() {
    let b = TypicalBrain::new();
    let (mut st, fx) = drained(
        &b,
        snapshot(&b, Some(OLD), true, 0),
        facts(true, false, false),
    );
    assert!(
        builds(&fx).is_empty(),
        "nothing before the reconcile: {fx:?}"
    );
    let fx = listed(&mut st, &b, &[], &[]);
    said_once(&fx, NOTICE_NOTHING);
    assert_eq!(st.meta(BUILD_KEY), Some(NEW));
    nothing_more(&mut st, b.now().wall.0);
}

/// A new pid on a new build: the old daemon stopped, crashed, or predates upgrades in place.
#[test]
fn a_new_pid_says_restarted_itself() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 0),
        facts(false, false, false),
    );
    said_once(&fx, NOTICE_RESTARTED);
}

/// The session plugin changed: the sessions live at the reconcile keep the old hooks.
#[test]
fn a_changed_plugin_counts_running_sessions() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 3),
        facts(true, true, false),
    );
    let text =
        "moa upgraded: 3 running sessions use the old hooks; they get the new ones when resumed";
    assert_eq!(notice_hooks(3), text);
    said_once(&fx, text);
}

/// An option tmux reads only when a terminal attaches changed: that beats the rest.
#[test]
fn an_attach_option_says_reattach() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 3),
        facts(false, true, true),
    );
    said_once(&fx, NOTICE_REATTACH);
}

/// The build that started last time: no line, no row, no write.
#[test]
fn the_same_build_says_nothing_at_all() {
    let b = TypicalBrain::new();
    let facts = UpgradeFacts {
        build: OLD.into(),
        ..facts(false, true, true)
    };
    let (mut st, fx) = started(&b, snapshot(&b, Some(OLD), true, 3), facts);
    assert!(activity(&fx).is_empty(), "{fx:?}");
    assert!(system(&fx).is_empty(), "{fx:?}");
    assert!(builds(&fx).is_empty(), "{fx:?}");
    nothing_more(&mut st, b.now().wall.0);
}

/// Ruling D: no build saved yet (a new root, or the first start of a build that saves it) is
/// saved, and nothing is said: a first start isn't an upgrade.
#[test]
fn a_first_start_saves_its_build_quietly() {
    let b = TypicalBrain::new();
    let (mut st, fx) = started(&b, snapshot(&b, None, true, 3), facts(false, true, true));
    assert_eq!(builds(&fx), [Some(NEW.to_string())]);
    assert!(activity(&fx).is_empty(), "{fx:?}");
    assert!(system(&fx).is_empty(), "{fx:?}");
    nothing_more(&mut st, b.now().wall.0);
}

/// With no thread of moa's, the activity row says it all the same.
#[test]
fn no_thread_still_leaves_the_activity_row() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), false, 0),
        facts(true, false, false),
    );
    assert_eq!(activity(&fx), [NOTICE_NOTHING], "{fx:?}");
    assert!(system(&fx).is_empty(), "{fx:?}");
    assert_eq!(builds(&fx), [Some(NEW.to_string())]);
}

/// The store refused the commit that said it: the core reloads what was saved, and the
/// first input once writes are accepted again says it, once.
#[test]
fn a_refused_commit_says_it_next_time() {
    let b = TypicalBrain::new();
    let snap = snapshot(&b, Some(OLD), true, 0);
    let (mut st, fx) = started(&b, snap.clone(), facts(true, false, false));
    said_once(&fx, NOTICE_NOTHING);

    let until = b.now().wall.0 + 60_000;
    let reload = Input::Reload {
        snapshot: Box::new(snap),
        refuse_writes: Some(RefuseWrites {
            until: UtcMs(until),
            reason: "the disk is full".into(),
        }),
    };
    let fx = handle_checked(&mut st, reload, b.now());
    assert!(builds(&fx).is_empty(), "{fx:?}");
    let fx = clock(&mut st, until - 1);
    assert!(builds(&fx).is_empty(), "writes are refused: {fx:?}");

    // Writes are accepted again: the tabs are reconciled again, and the notice is said.
    let mut fx = clock(&mut st, until + 1);
    fx.extend(no_tabs(&mut st, until + 2));
    said_once(&fx, NOTICE_NOTHING);
    nothing_more(&mut st, until + 2);
}

/// The session plugin changed with sessions running: that beats a new pid.
#[test]
fn old_hooks_beat_restarted_itself() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 2),
        facts(false, true, false),
    );
    let text =
        "moa upgraded: 2 running sessions use the old hooks; they get the new ones when resumed";
    assert_eq!(notice_hooks(2), text);
    said_once(&fx, text);
}

/// One session running: the line says it in the singular.
#[test]
fn one_running_session_is_said_in_the_singular() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 1),
        facts(true, true, false),
    );
    let text =
        "moa upgraded: 1 running session uses the old hooks; it gets the new ones when resumed";
    assert_eq!(notice_hooks(1), text);
    said_once(&fx, text);
}

/// The plugin changed with no session running: nobody has the old hooks, so the next line
/// says it.
#[test]
fn a_changed_plugin_with_no_sessions_falls_through() {
    let b = TypicalBrain::new();
    let (_, fx) = started(
        &b,
        snapshot(&b, Some(OLD), true, 0),
        facts(false, true, false),
    );
    said_once(&fx, NOTICE_RESTARTED);
}

/// The plugin is Claude Code's: Codex sessions running don't count as on the old hooks.
#[test]
fn live_codex_sessions_use_no_old_hooks() {
    let b = TypicalBrain::new();
    let mut snap = snapshot(&b, Some(OLD), true, 0);
    let mut codex = 0;
    for s in snap
        .sessions
        .iter_mut()
        .filter(|s| s.agent_kind == AgentKind::Codex)
    {
        codex += 1;
        s.lifecycle = Lifecycle::Live;
        s.location = Location::Elsewhere;
        s.archived_at = None;
    }
    assert!(codex > 0, "the typical brain has a Codex session");
    let (st, fx) = started(&b, snap, facts(true, true, false));
    let live = st.sessions().filter(|s| s.lifecycle == Lifecycle::Live);
    assert_eq!(live.count(), codex, "they are live at the reconcile");
    said_once(&fx, NOTICE_NOTHING);
}

/// No tmux server was running: no terminal is attached under the old options, and the next
/// attach reads the new ones, so the next line says it.
#[test]
fn no_server_needs_no_reattach() {
    let b = TypicalBrain::new();
    let mut st = fresh(&b, Deps::default(), facts(true, false, true));
    let snap = snapshot(&b, Some(OLD), true, 0);
    handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    feed(&b, &mut st, TmuxIn::Down(Down::NoServer));
    let fx = handle_checked(&mut st, Input::StartDrained, b.now());
    said_once(&fx, NOTICE_NOTHING);
}

/// A link still connecting when the start drain ends: nothing is said until the tabs are
/// reconciled.
#[test]
fn a_link_still_connecting_says_nothing_yet() {
    let b = TypicalBrain::new();
    let mut st = fresh(&b, Deps::default(), facts(true, false, false));
    let snap = snapshot(&b, Some(OLD), true, 0);
    let fx = handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    assert!(builds(&fx).is_empty(), "{fx:?}");
    let fx = handle_checked(&mut st, Input::StartDrained, b.now());
    assert!(
        builds(&fx).is_empty(),
        "the link is still connecting: {fx:?}"
    );
    let fx = clock(&mut st, b.now().wall.0 + 1_000);
    assert!(builds(&fx).is_empty(), "{fx:?}");

    // Up, its listing not yet answered: still nothing.
    link_up(&b, &mut st);
    let fx = clock(&mut st, b.now().wall.0 + 2_000);
    assert!(builds(&fx).is_empty(), "{fx:?}");
    let fx = listed(&mut st, &b, &[], &[]);
    said_once(&fx, NOTICE_NOTHING);
}

/// The log lines `fx` says.
fn logs(fx: &[Effect]) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l.message.as_str()),
            _ => None,
        })
        .collect()
}

/// A process read that always panics, so every reconcile that reads a tab does.
fn panicking(_: i64, _: &[String]) -> Inspected {
    panic!("a process read that always panics")
}

/// The reconcile is given up on after it panicked three times: none comes on
/// this connection, so the notice isn't held for one. It is said once, and the build saved.
#[test]
fn a_given_up_reconcile_still_says_it() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let b = TypicalBrain::new();
    let deps = Deps {
        inspect: panicking,
        ..Deps::default()
    };
    let mut st = fresh(&b, deps, facts(true, false, false));
    let snap = snapshot(&b, Some(OLD), true, 0);
    handle_checked(&mut st, Input::Startup(Box::new(snap.clone())), b.now());
    link_up(&b, &mut st);
    handle_checked(&mut st, Input::StartDrained, b.now());
    let mut said = Vec::new();
    for attempt in 1..=3 {
        // One untagged tab: reconcile reads its process, and panics.
        let info = ["I\t%9\t909\t0\t\t\t\t/tmp"];
        let run = catch_unwind(AssertUnwindSafe(|| listed(&mut st, &b, &info, &[])));
        assert!(run.is_err(), "attempt {attempt} didn't panic");
        // As the daemon does after a panic: the core reloads.
        let reload = Input::Reload {
            snapshot: Box::new(snap.clone()),
            refuse_writes: None,
        };
        let fx = handle_checked(&mut st, reload, b.now());
        if attempt < 3 {
            assert!(
                builds(&fx).is_empty(),
                "a reconcile is still coming: {fx:?}"
            );
        } else {
            let given_up =
                "reconciling the tabs failed 3 times; moa tries again at the next connection";
            assert!(logs(&fx).contains(&given_up), "{fx:?}");
        }
        said.extend(fx);
    }
    assert!(!st.tmux().reconciled());
    assert!(matches!(
        st.tmux().link,
        moa_core::tmux::link::Link::Up { .. }
    ));
    said.extend(clock(&mut st, b.now().wall.0 + 1_000));
    said_once(&said, NOTICE_NOTHING);
    assert_eq!(st.meta(BUILD_KEY), Some(NEW));
}

/// A notice whose commit fails for another reason than refused writes (here a row this
/// build doesn't know where its activity row goes) stays owed and is tried at each input,
/// but the log says so once.
#[test]
fn a_notice_that_cant_be_saved_is_logged_once() {
    const UNSAVED: &str = "the upgrade's notice wasn't saved; moa says it at the next input";
    let b = TypicalBrain::new();
    let mut snap = snapshot(&b, Some(OLD), false, 0);
    snap.unknown.push(LoadNote {
        table: "activity",
        key: format!("#{}", snap.next_ids.activity),
        reason: "a value this build doesn't know",
    });
    let (mut st, mut fx) = started(&b, snap, facts(true, false, false));
    fx.extend(clock(&mut st, b.now().wall.0 + 1_000));
    fx.extend(clock(&mut st, b.now().wall.0 + 2_000));
    assert!(builds(&fx).is_empty() && activity(&fx).is_empty(), "{fx:?}");
    let unsaved = logs(&fx).into_iter().filter(|l| *l == UNSAVED).count();
    assert_eq!(unsaved, 1, "{fx:?}");
}
