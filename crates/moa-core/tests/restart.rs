//! `restart Q` (R24): an adopted session, whose agent the user started by hand, restarted
//! under moa: its old tab killed once that is saved, and its conversation resumed in a new
//! tab moa tracks. The typical brain, its link played by hand; the transcript and folder are
//! real files under the brain's temp home.

mod common;

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use moa_agents::claude::{find_transcript, project_slot};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::live::is_asleep;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, EventKind, Lifecycle, Location,
    PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{Done, Op, OpError, ReqId};
use moa_core::sleep::why_not;
use moa_core::state::{Deps, LoadNote, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::link::NOT_READY;
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

use common::*;

const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// The adopted session S, and its conversation.
const S: &str = "00000000-0000-4000-8000-0000000000d1";
const C9: &str = "C9";
const TITLE: &str = "boards";

fn s_id() -> SessionId {
    S.parse().unwrap()
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

thread_local! {
    /// Whether S's agent still runs in its shell tab, %5.
    static AGENT_RUNS: Cell<bool> = const { Cell::new(true) };
}

/// S's agent in %5 (pid 505), started by hand: no `MOA_SESSION`, while it runs.
fn s_in_a_shell(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 505 && AGENT_RUNS.with(Cell::get)).then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: Some(C9.into()),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    })
}

/// The world before loading: the typical brain plus S, live in `<home>/work/lantern` on
/// conversation C9 (recorded in acct-a's store, whose transcript is there), adopted in shell
/// tab %5 of this server.
struct World {
    b: TypicalBrain,
    s: Session,
    binding: Option<PaneBinding>,
    deps: Deps,
    /// S's binding was loaded with a value this build doesn't know: no write to it is saved.
    unknown: bool,
}

impl World {
    fn new() -> World {
        AGENT_RUNS.with(|r| r.set(true));
        let b = TypicalBrain::new();
        let lantern = b.home.join("work/lantern");
        fs::create_dir_all(&lantern).unwrap();
        let at = UtcMs(b.now().wall.0 - 3_600_000);
        let s = Session {
            id: s_id(),
            agent_kind: AgentKind::Claude,
            account: Some("acct-a".into()),
            cwd: Some(shown(&lantern)),
            start_cwd: Some(shown(&lantern)),
            project_id: None,
            title: Some(TITLE.into()),
            title_locked: false,
            group_path: None,
            lifecycle: Lifecycle::Live,
            activity: Activity::Idle,
            activity_at: Some(at),
            activity_why: None,
            last_prompt: None,
            last_turn_end_at: None,
            seen_at: Some(at),
            asleep_at: None,
            archived_at: None,
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(1),
            last_seen_at: Some(at),
        };
        let binding = PaneBinding {
            session_id: s_id(),
            server_pid: generation().pid,
            server_start: generation().start,
            socket: "moa".into(),
            pane_id: "%5".into(),
            pane_pid: 505,
            bound_at: at,
            tracked: false,
        };
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            transcript: find_transcript,
            is_dir: Path::is_dir,
            inspect: s_in_a_shell,
            ..Deps::default()
        };
        World {
            b,
            s,
            binding: Some(binding),
            deps,
            unknown: false,
        }
    }

    /// S's tab is one moa started: a tracked binding.
    fn tracked() -> World {
        let mut w = World::new();
        w.binding.as_mut().unwrap().tracked = true;
        w
    }

    /// S asleep, with no tab.
    fn asleep() -> World {
        let mut w = World::new();
        w.s.lifecycle = Lifecycle::Ended;
        w.s.asleep_at = w.s.activity_at;
        w.binding = None;
        w
    }

    /// Loaded, linked, drained and reconciled: S's tab is listed, alive and tagged.
    fn reconciled(self) -> Rig {
        let lantern = self.b.home.join("work/lantern");
        let store = self.b.home.join(".claude-a");
        let dir = store.join("projects").join(project_slot(&lantern));
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join(format!("{C9}.jsonl"));
        fs::write(&transcript, "{\"type\":\"summary\",\"summary\":\"fake\"}\n").unwrap();
        let conv = Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: C9.into(),
            session_id: s_id(),
            store_dir: Some(shown(&store)),
            spike_account: None,
            reason: ConversationReason::Adopt,
            first_seen_at: UtcMs(1),
            last_seen_at: UtcMs(1),
            transcript_path: Some(shown(&transcript)),
            transcript_present: None,
            transcript_checked_at: None,
        };
        let line = self.binding.as_ref().map(|b| tab(b, &shown(&lantern)));
        let mut snap = self.b.snapshot();
        snap.sessions.push(self.s);
        snap.conversations.push(conv);
        snap.pane_bindings.extend(self.binding);
        if self.unknown {
            snap.unknown.push(LoadNote {
                table: "pane_bindings",
                key: S.into(),
                reason: "a value this build doesn't know",
            });
        }
        let mut st = loaded_with(&self.b, TOML, self.deps, snap);
        link_up(&self.b, &mut st);
        handle_checked(&mut st, Input::StartDrained, self.b.now());
        let lines: Vec<&str> = line.iter().map(String::as_str).collect();
        listed(&mut st, &self.b, &lines, &[]);
        assert!(st.tmux().reconciled());
        Rig { b: self.b, st }
    }
}

/// The `PANE_INFO` line of binding `b`'s tab, alive in `cwd` with its tags.
fn tab(b: &PaneBinding, cwd: &str) -> String {
    let adopted = if b.tracked { "" } else { "1" };
    format!(
        "I\t{}\t{}\t0\t\t{adopted}\t{S}\t{cwd}",
        b.pane_id, b.pane_pid
    )
}

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    /// `restart S` as request `req`.
    fn restart(&mut self, req: u64) -> Vec<Effect> {
        send(
            &self.b,
            &mut self.st,
            req,
            Op::SessionRestart { q: S.into() },
        )
    }

    /// tmux answers the restart's pane query: the adopted tab %5, alive.
    fn listed(&mut self) -> Vec<Effect> {
        let lantern = shown(&self.b.home.join("work/lantern"));
        let line = format!("I\t%5\t505\t0\t\t1\t{S}\t{lantern}");
        listed(&mut self.st, &self.b, &[&line], &[])
    }

    /// `restart S`, its pane query answered: the commit, the kill and the window.
    fn committed(&mut self, req: u64) -> Vec<Effect> {
        let fx = self.restart(req);
        assert!(queried(&fx), "{fx:?}");
        self.listed()
    }

    /// tmux says the new tab is `pane`.
    fn bound(&mut self, pane: &str) -> Vec<Effect> {
        feed(
            &self.b,
            &mut self.st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Open(s_id()),
                results: vec![Ok(vec![format!("P {pane} 4242")])],
            },
        )
    }

    fn pane_exited(&mut self, pane: &str) -> Vec<Effect> {
        feed(
            &self.b,
            &mut self.st,
            TmuxIn::Notification(Notification::Message(format!(
                "moa:hook pane-exited {pane}"
            ))),
        )
    }

    fn s(&self) -> &Session {
        self.st.session(s_id()).unwrap()
    }
}

/// Whether `fx` asks tmux a pane query.
fn queried(fx: &[Effect]) -> bool {
    fx.iter().any(|e| {
        matches!(
            e,
            Effect::Tmux {
                token: Some(TmuxToken::Query(_)),
                ..
            }
        )
    })
}

/// Every tmux command sent, untokened or not.
fn sent(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn kill(pane: &str) -> Cmd {
    Cmd::new("kill-pane").args(["-t", pane])
}

fn kills(fx: &[Effect]) -> Vec<&Cmd> {
    sent(fx)
        .into_iter()
        .filter(|c| c.name() == "kill-pane")
        .collect()
}

fn windows(fx: &[Effect]) -> Vec<&Cmd> {
    sent(fx)
        .into_iter()
        .filter(|c| c.name() == "new-window")
        .collect()
}

fn restarted() -> Result<Done, OpError> {
    Ok(Done {
        text: format!("restarted {TITLE} under moa"),
        data: json!({ "id": s_id(), "title": TITLE }),
    })
}

/// The one reply in `fx`, which is a refusal: its text.
fn refusal(fx: &[Effect]) -> String {
    match replies(fx).as_slice() {
        [(_, Err(e))] => e.0.clone(),
        other => panic!("one refusal, got {other:?}"),
    }
}

fn activity_rows(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

/// S's old agent says it ended, `ms` after the typical now.
fn old_agent_ended(r: &mut Rig, ms: i64) -> Vec<Effect> {
    let at = r.b.now().wall.0 + ms;
    handle_checked(
        &mut r.st,
        Input::Hook {
            reply: None,
            envelope: HookEnvelope {
                agent: AgentKind::Claude,
                slot: None,
                brain: Some("default".into()),
                store_dir: Some(shown(&r.b.home.join(".claude-a"))),
                conversation_id: C9.into(),
                kind: HookKind::SessionEnded {
                    reason: "other".into(),
                },
                at: UtcMs(at),
                at_ns: at * 1_000_000,
                pid: 505,
                seq: 1,
                cwd: None,
                transcript_path: None,
            },
        },
        r.b.now(),
    )
}

#[test]
fn restart_kills_then_resumes() {
    let mut r = World::new().reconciled();
    let fx = r.restart(1);
    // R13 step 6's one pane query, and nothing else yet.
    assert!(queried(&fx), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(kills(&fx).is_empty(), "{fx:?}");

    let fx = r.listed();
    // One commit: the old binding deleted and the row written; the session stays live.
    let persists: Vec<usize> = fx
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Effect::Persist(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(persists.len(), 1, "{fx:?}");
    let saved = changes(&fx);
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(id) if *id == s_id())),
        "{fx:?}"
    );
    assert!(
        !saved.iter().any(|c| matches!(c, Change::Session(_))),
        "{fx:?}"
    );
    assert_eq!(activity_rows(&fx), [format!("restarted {TITLE} under moa")]);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(r.st.binding(s_id()).is_none());
    assert_eq!(why_not(&r.st, s_id(), r.b.now().wall, 0), Some("opening"));

    // Then the kill of the old tab, then the resume window, on the link's connection.
    let killed = fx
        .iter()
        .position(
            |e| matches!(e, Effect::Tmux { conn: 1, cmds, token: None } if cmds == &[kill("%5")]),
        )
        .expect("the old tab is killed");
    let window = fx
        .iter()
        .position(|e| {
            matches!(
                e,
                Effect::Tmux {
                    conn: 1,
                    token: Some(TmuxToken::Open(_)),
                    ..
                }
            )
        })
        .expect("the resume window");
    assert!(persists[0] < killed && killed < window, "{fx:?}");
    assert!(kill("%5").check().is_ok());
    let (id, cmds) = the_open(&fx);
    assert_eq!(id, s_id());
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(argv[at + 1], C9);
    assert!(
        argv.iter().any(|a| *a == format!("MOA_SESSION={S}")),
        "{argv:?}"
    );
    moa_core::effect::check_order(&fx).unwrap();
    // The reply waits for the new tab.
    assert!(replies(&fx).is_empty(), "{fx:?}");

    // Bound: tracked now, and answered.
    let fx = r.bound("%7");
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::PaneBinding(p) if p.session_id == s_id() && p.tracked)),
        "{fx:?}"
    );
    assert_eq!(replies(&fx), [(ReqId(1), restarted())]);
    let b = r.st.binding(s_id()).unwrap();
    assert!(b.tracked && b.pane_id == "%7", "{b:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert_ne!(why_not(&r.st, s_id(), r.b.now().wall, 0), Some("opening"));
}

/// The old tab's exit, before the new tab's answer or after it, isn't the session's end:
/// its binding was deleted first.
#[test]
fn the_old_tabs_exit_changes_nothing() {
    for before in [true, false] {
        let mut r = World::new().reconciled();
        r.committed(1);
        let mut fx = Vec::new();
        if before {
            fx.extend(r.pane_exited("%5"));
            fx.extend(r.bound("%7"));
        } else {
            fx.extend(r.bound("%7"));
            fx.extend(r.pane_exited("%5"));
        }
        assert_eq!(replies(&fx), [(ReqId(1), restarted())], "before: {before}");
        assert!(
            !changes(&fx)
                .iter()
                .any(|c| matches!(c, Change::Session(_) | Change::DeletePaneBinding(_))),
            "before: {before}: {fx:?}"
        );
        assert!(!changes(&fx).iter().any(|c| matches!(c, Change::Event(e)
            if e.kind.0 == EventKind::SESSION_ENDED)));
        assert_eq!(r.s().lifecycle, Lifecycle::Live, "before: {before}");
        assert!(!is_asleep(r.s()));
        let b = r.st.binding(s_id()).unwrap();
        assert!(b.tracked && b.pane_id == "%7", "before: {before}: {b:?}");
    }
}

/// An old agent whose hooks reach moa says `other` as moa kills it: while the restart's
/// tab opens, that end is history only.
#[test]
fn the_old_agents_end_while_restarting_is_history_only() {
    let mut r = World::new().reconciled();
    r.committed(1);
    let fx = old_agent_ended(&mut r, 1000);
    let saved = changes(&fx);
    assert!(
        matches!(saved.as_slice(), [Change::Event(e)]
            if e.kind.0 == EventKind::SESSION_ENDED && e.session_id == Some(s_id())),
        "{fx:?}"
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    let fx = r.bound("%7");
    assert_eq!(replies(&fx), [(ReqId(1), restarted())]);
    assert!(r.st.binding(s_id()).is_some_and(|b| b.tracked));
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// R31: an adopted agent that left its shell tab isn't killed with it. The session goes
/// to sleep, the tab, left to the user, loses moa's tags, and nothing is launched.
#[test]
fn restart_refuses_when_the_adopted_agent_is_gone() {
    let mut r = World::new().reconciled();
    AGENT_RUNS.with(|a| a.set(false));
    let fx = r.restart(1);
    assert_eq!(
        refusal(&fx),
        format!("{TITLE}'s agent isn't running in that tab any more")
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(windows(&fx).is_empty(), "{fx:?}");
    assert!(!queried(&fx), "{fx:?}");
    assert_eq!(unsets(&fx), untagged("%5").iter().collect::<Vec<_>>());
    assert!(is_asleep(r.s()), "{:?}", r.s());
    assert!(r.st.binding(s_id()).is_none());
    moa_core::effect::check_order(&fx).unwrap();
}

/// The agent leaves while the restart looks at the tabs: it is checked again then.
#[test]
fn restart_checks_the_agent_again_once_the_tabs_answer() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.restart(1)));
    AGENT_RUNS.with(|a| a.set(false));
    let fx = r.listed();
    assert_eq!(
        refusal(&fx),
        format!("{TITLE}'s agent isn't running in that tab any more")
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(windows(&fx).is_empty(), "{fx:?}");
    assert!(is_asleep(r.s()));
}

/// A restart the store refuses kills nothing and launches nothing (R15).
#[test]
fn restart_with_writes_refused_kills_nothing() {
    let mut w = World::new();
    w.unknown = true;
    let mut r = w.reconciled();
    let fx = r.committed(1);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert!(
        refusal(&fx).ends_with("has a value this build doesn't know; update moa"),
        "{fx:?}"
    );
    assert!(r.st.binding(s_id()).is_some_and(|b| !b.tracked));
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert_eq!(
        why_not(&r.st, s_id(), r.b.now().wall, 0),
        Some("not tracked")
    );
}

#[test]
fn restart_refusals() {
    // Already a tab moa started.
    let mut r = World::tracked().reconciled();
    let fx = r.restart(1);
    assert_eq!(
        refusal(&fx),
        format!("{TITLE} is already tracked; nothing to restart")
    );
    assert!(sent(&fx).is_empty() && changes(&fx).is_empty(), "{fx:?}");

    // Not running.
    let mut r = World::asleep().reconciled();
    let fx = r.restart(1);
    assert_eq!(
        refusal(&fx),
        format!("{TITLE} isn't running; resume it with moactl resume")
    );
    assert!(sent(&fx).is_empty() && changes(&fx).is_empty(), "{fx:?}");

    // Before the tabs are reconciled (R10).
    let w = World::new();
    let mut snap = w.b.snapshot();
    snap.sessions.push(w.s.clone());
    let mut st = loaded_with(&w.b, TOML, w.deps, snap);
    link_up(&w.b, &mut st);
    let fx = send(&w.b, &mut st, 1, Op::SessionRestart { q: S.into() });
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(NOT_READY.into())))]);
}

/// Resume's checks (R13 step 5): a conversation whose transcript is gone can't be resumed,
/// so the running agent is left alone.
#[test]
fn restart_refuses_a_missing_transcript() {
    let mut r = World::new().reconciled();
    let lantern = r.b.home.join("work/lantern");
    let file =
        r.b.home
            .join(".claude-a/projects")
            .join(project_slot(&lantern))
            .join(format!("{C9}.jsonl"));
    fs::remove_file(file).unwrap();
    let fx = r.restart(1);
    assert_eq!(
        refusal(&fx),
        "its conversation isn't in acct-a's folder any more, so it can't be resumed"
    );
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert!(r.st.binding(s_id()).is_some());
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// A session being restarted can't be put to sleep, by name or by idle time, between the
/// restart's look at the tabs and its new tab.
#[test]
fn a_restarting_session_doesnt_sleep() {
    let mut r = World::new().reconciled();
    let still = Err(OpError("it's still opening; try again in a moment".into()));
    assert!(queried(&r.restart(1)));
    let now = r.b.now().wall;
    assert_eq!(why_not(&r.st, s_id(), now, 0), Some("opening"));
    let fx = send(
        &r.b,
        &mut r.st,
        2,
        Op::SessionSleep {
            qs: vec![S.into()],
            idle_ms: None,
            dry_run: false,
        },
    );
    assert_eq!(replies(&fx), [(ReqId(2), still.clone())]);

    r.listed();
    assert_eq!(why_not(&r.st, s_id(), now, 0), Some("opening"));
    let fx = send(
        &r.b,
        &mut r.st,
        3,
        Op::SessionSleep {
            qs: vec![S.into()],
            idle_ms: None,
            dry_run: false,
        },
    );
    assert_eq!(replies(&fx), [(ReqId(3), still)]);
    let fx = send(
        &r.b,
        &mut r.st,
        4,
        Op::SessionSleep {
            qs: Vec::new(),
            idle_ms: Some(0),
            dry_run: false,
        },
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// Two restarts asked before either's pane query came back kill one tab and open one.
#[test]
fn two_restarts_in_flight_open_one_tab() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.restart(1)));
    assert!(queried(&r.restart(2)));
    let lantern = shown(&r.b.home.join("work/lantern"));
    let line = format!("I\t%5\t505\t0\t\t1\t{S}\t{lantern}");
    let asked: Vec<u64> = r.st.tmux().queries().collect();
    let mut fx = Vec::new();
    for n in asked {
        fx.extend(feed(
            &r.b,
            &mut r.st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Query(n),
                results: vec![Ok(vec![line.clone()]), Ok(vec![])],
            },
        ));
    }
    assert_eq!(kills(&fx).len(), 1, "{fx:?}");
    assert_eq!(windows(&fx).len(), 1, "{fx:?}");
    // The second joined the first's launch, as a second resume does.
    let fx = r.bound("%7");
    let to: Vec<ReqId> = replies(&fx).into_iter().map(|(req, _)| req).collect();
    assert_eq!(to, [ReqId(1), ReqId(2)]);
    assert_eq!(replies(&fx)[0], (ReqId(1), restarted()));
}

/// R13a: a restart whose new tab tmux couldn't open leaves the session asleep and
/// resumable, not failed.
#[test]
fn a_restart_whose_tab_fails_sleeps() {
    let mut r = World::new().reconciled();
    r.committed(1);
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(s_id()),
            results: vec![Err("create window failed".into())],
        },
    );
    assert_eq!(refusal(&fx), "couldn't open a tab for it; see moa's log");
    let s = r.s();
    assert!(is_asleep(s), "{s:?}");
    assert_eq!(s.asleep_at, Some(r.b.now().wall));
    assert!(!s.resume_failed);
}
