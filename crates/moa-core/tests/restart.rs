//! `restart Q` (R24): an adopted session, whose agent the user started by hand, restarted
//! under moa: its old tab killed once that is saved, and its conversation resumed in a new
//! tab moa tracks. The typical brain, its link played by hand; the transcript and folder are
//! real files under the brain's temp home.

mod common;

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};

use moa_agents::claude::{find_transcript, project_slot};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::Down;
use moa_core::effect::{Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind, StartSource};
use moa_core::live::is_asleep;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, EventKind, Inspected, Lifecycle,
    Location, PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{Done, Op, OpError, ReqId};
use moa_core::sleep::why_not;
use moa_core::state::{Deps, LoadNote, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::link::NOT_READY;
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
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
    /// Whether %5's process can't be read.
    static UNREADABLE: Cell<bool> = const { Cell::new(false) };
    /// The store S's agent runs with: acct-a's.
    static STORE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// S's agent in %5 is one moa started for S: it carries `MOA_SESSION` = S.
    static MOA_STARTED: Cell<bool> = const { Cell::new(false) };
}

/// S's agent in %5 (pid 505), started by hand: no `MOA_SESSION`, while it runs.
fn s_in_a_shell(pid: i64, _: &[String]) -> Inspected {
    if pid == 505 && UNREADABLE.with(Cell::get) {
        return Inspected::Unknown;
    }
    Inspected::from(
        (pid == 505 && AGENT_RUNS.with(Cell::get)).then(|| PaneAgent {
            kind: AgentKind::Claude,
            store_dir: STORE.with(|s| s.borrow().clone()),
            conversation: Some(C9.into()),
            moa_session: MOA_STARTED.with(Cell::get).then(|| S.to_string()),
            moa_brain: None,
            title: None,
            group: None,
            started_at: None,
        }),
    )
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
        UNREADABLE.with(|r| r.set(false));
        MOA_STARTED.with(|r| r.set(false));
        let b = TypicalBrain::new();
        STORE.with(|s| *s.borrow_mut() = Some(shown(&b.home.join(".claude-a"))));
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

/// A hook on conversation C9, `ms` after the typical now: from the agent moa started for S
/// (`MOA_SESSION` = S) in its new tab %7 when `ours`, else from S's old agent, started by
/// hand in %5. `pid` is the hook process's, which also keeps the events apart.
fn hook(r: &mut Rig, ours: bool, kind: HookKind, ms: i64, pid: u32) -> Vec<Effect> {
    let pane = if ours { "%7" } else { "%5" };
    hook_in(r, ours, Some(pane), kind, ms, pid)
}

/// [`hook`], from an agent in tab `pane` (`TMUX_PANE`), when it says.
fn hook_in(
    r: &mut Rig,
    ours: bool,
    pane: Option<&str>,
    kind: HookKind,
    ms: i64,
    pid: u32,
) -> Vec<Effect> {
    let at = r.b.now().wall.0 + ms;
    handle_checked(
        &mut r.st,
        Input::Hook {
            reply: None,
            envelope: HookEnvelope {
                agent: AgentKind::Claude,
                slot: ours.then(|| S.to_string()),
                pane: pane.map(str::to_string),
                brain: Some("default".into()),
                store_dir: Some(shown(&r.b.home.join(".claude-a"))),
                conversation_id: C9.into(),
                kind,
                at: UtcMs(at),
                at_ns: at * 1_000_000,
                pid,
                seq: 1,
                cwd: None,
                transcript_path: None,
            },
        },
        r.b.now(),
    )
}

fn ended() -> HookKind {
    HookKind::SessionEnded {
        reason: "other".into(),
    }
}

/// S's old agent says it ended, `ms` after the typical now.
fn old_agent_ended(r: &mut Rig, ms: i64) -> Vec<Effect> {
    hook(r, false, ended(), ms, 900)
}

/// `fx` saved S's `SessionEnd` as history, and nothing else.
fn history_only(fx: &[Effect]) {
    let saved = changes(fx);
    assert!(
        matches!(saved.as_slice(), [Change::Event(e)]
            if e.kind.0 == EventKind::SESSION_ENDED && e.session_id == Some(s_id())),
        "{fx:?}"
    );
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
    // Live and here, said outright; what it was doing stays.
    let before = r.s().clone();
    let sessions: Vec<&Session> = saved
        .iter()
        .filter_map(|c| match c {
            Change::Session(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(sessions, [&before], "{fx:?}");
    assert_eq!(
        (before.lifecycle, before.asleep_at, before.location),
        (Lifecycle::Live, None, Location::Here)
    );
    assert_eq!(
        before.activity_at,
        Some(UtcMs(r.b.now().wall.0 - 3_600_000))
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

// --- With the cockpit (5b's L11, L17): the kill is the queue's vacate ---

/// A listing's answers: the cockpit, with a tree panel and, while `old`, S's old tab %5 in
/// its spot; no spare empty slot.
fn cockpit_scene(old: bool) -> Vec<Result<Vec<String>, String>> {
    let lines = |l: &[String]| Ok(l.to_vec());
    let mut panes = vec!["P %2 @1 $0 0 0 0 1  0 tree".to_string()];
    if old {
        panes.push(format!("P %5 @1 $0 1 0 0 1 {S} 0 agent"));
    }
    vec![
        lines(&["S $0 moa".into(), "S $1 _moa".into()]),
        lines(&panes),
        lines(&["C 1 $1 client-9".into()]),
    ]
}

/// tmux answers query `n` on connection `conn`.
fn answer_on(
    r: &mut Rig,
    conn: u64,
    n: u64,
    results: Vec<Result<Vec<String>, String>>,
) -> Vec<Effect> {
    let token = TmuxToken::Query(n);
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token,
            results,
        },
    )
}

/// Whether `fx` sends a window for a launch.
fn opens(fx: &[Effect]) -> bool {
    fx.iter().any(|e| {
        matches!(
            e,
            Effect::Tmux {
                token: Some(TmuxToken::Open(_)),
                ..
            }
        )
    })
}

/// The cockpit listed with S's old tab %5 in its spot, then `restart S` with its pane query
/// answered: the commit and the vacate's listing go out, never the kill or the new window
/// yet (R24). The listing's number.
fn restart_with_a_cockpit(r: &mut Rig) -> u64 {
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: cockpit_scene(true),
        },
    );
    assert!(queried(&r.restart(1)));
    let fx = r.listed();
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(!opens(&fx), "the new tab waits for the old agent: {fx:?}");
    let n = the_listing(&fx);
    let persist = fx.iter().position(|e| matches!(e, Effect::Persist(_)));
    let listing = fx.iter().position(
        |e| matches!(e, Effect::Tmux { token: Some(TmuxToken::Query(q)), .. } if *q == n),
    );
    assert!(persist.is_some() && persist < listing, "{fx:?}");
    n
}

/// S's old agent in the cockpit's spot (L11): restarted, the empty slot takes its place, by
/// the cockpit's queue, from a listing of its own. Only once that chain is answered does the
/// new tab open (R24): the old agent goes first, as with no cockpit.
#[test]
fn restarting_the_spots_agent_leaves_the_empty_slot() {
    let mut r = World::new().reconciled();
    let n = restart_with_a_cockpit(&mut r);
    let fx = answer_on(&mut r, 1, n, cockpit_scene(true));
    let [(n, chain)] = &queries(&fx)[..] else {
        panic!("one chain: {fx:?}");
    };
    let first = &chain[0].parts()[0];
    assert_eq!(
        (first.name(), &first.argv()[..3]),
        (
            "respawn-pane",
            &["-k".to_string(), "-t".into(), "%5".into()][..]
        )
    );
    assert_eq!(chain[0].parts().len(), 1, "the respawn alone: {chain:?}");
    assert!(!opens(&fx), "{fx:?}");

    // The old agent is gone; the empty slot's tags follow, then the new tab opens.
    let fx = answer_on(&mut r, 1, *n, vec![Ok(Vec::new())]);
    assert!(!opens(&fx), "{fx:?}");
    let [(n, tags)] = &queries(&fx)[..] else {
        panic!("the slot's tags: {fx:?}");
    };
    assert!(
        tags[0]
            .parts()
            .iter()
            .any(|c| c.argv().iter().any(|a| a == "empty-slot")),
        "{tags:?}"
    );
    let fx = answer_on(&mut r, 1, *n, vec![Ok(Vec::new())]);
    let (id, cmds) = the_open(&fx);
    assert_eq!((id, cmds[0].name()), (s_id(), "new-window"));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(replies(&r.bound("%7")), [(ReqId(1), restarted())]);
}

/// The respawn took the old agent down, but tmux refused a tag after it: the agent is gone,
/// so the restart goes ahead; the missing slot is logged.
#[test]
fn a_restart_whose_old_agent_went_resumes_though_a_tag_failed() {
    let mut r = World::new().reconciled();
    let n = restart_with_a_cockpit(&mut r);
    let fx = answer_on(&mut r, 1, n, cockpit_scene(true));
    let [(n, _)] = &queries(&fx)[..] else {
        panic!("the respawn: {fx:?}");
    };
    let fx = answer_on(&mut r, 1, *n, vec![Ok(Vec::new())]);
    let [(n, _)] = &queries(&fx)[..] else {
        panic!("the slot's tags: {fx:?}");
    };
    let fx = answer_on(&mut r, 1, *n, vec![Err("invalid option: @moa_view".into())]);
    assert_eq!(the_open(&fx).0, s_id());
    let said = "the agent in %5 was taken down, but the empty slot there couldn't be set up: \
                invalid option: @moa_view";
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Log(l) if l.message == said)),
        "{fx:?}"
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
}

/// The old tab went before the vacate's turn: nothing to take down, so the new tab opens.
#[test]
fn a_restart_whose_old_tab_is_gone_resumes() {
    let mut r = World::new().reconciled();
    let n = restart_with_a_cockpit(&mut r);
    let fx = answer_on(&mut r, 1, n, cockpit_scene(false));
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert_eq!(the_open(&fx).0, s_id());
    assert_eq!(replies(&r.bound("%7")), [(ReqId(1), restarted())]);
}

/// tmux refuses the vacate's chain: the old agent may still run, so nothing is resumed. The
/// restart is answered plainly and logged, and S is asleep, no longer opening.
#[test]
fn a_restart_whose_old_agent_stays_isnt_resumed() {
    let mut r = World::new().reconciled();
    let n = restart_with_a_cockpit(&mut r);
    let fx = answer_on(&mut r, 1, n, cockpit_scene(true));
    let [(n, _)] = &queries(&fx)[..] else {
        panic!("one chain: {fx:?}");
    };
    let fx = answer_on(&mut r, 1, *n, vec![Err("can't find pane: %5".into())]);
    assert!(!opens(&fx), "{fx:?}");
    assert_eq!(
        refusal(&fx),
        "its old tab couldn't be closed, so it wasn't restarted; see moa's log"
    );
    let said = format!("session {S} wasn't restarted: its old agent in %5 couldn't be taken down");
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Log(l) if l.message == said)),
        "{fx:?}"
    );
    assert!(is_asleep(r.s()), "{:?}", r.s());
    assert_ne!(why_not(&r.st, s_id(), r.b.now().wall, 0), Some("opening"));

    // Its old agent still runs in %5: what it says counts again, neither the restart's fence
    // nor a killed tab's.
    let started = HookKind::SessionStarted {
        source: StartSource::Startup,
    };
    hook(&mut r, false, started, 1000, 901);
    assert_eq!(r.s().lifecycle, Lifecycle::Live, "{:?}", r.s());
}

/// The link goes while the restart waits for its vacate: the restart hears so, and its new
/// tab never opens; back on the same server, the vacate still takes the old agent down.
#[test]
fn a_restart_held_for_its_vacate_ends_with_the_link() {
    let mut r = World::new().reconciled();
    restart_with_a_cockpit(&mut r);
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError("moa's terminal went away before it opened".into()))
        )]
    );
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: generation(),
            version: "3.5a".into(),
            ctl: moa_core::tmux::ids::ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    // The vacate's listing, beside the reconcile's look at the tabs.
    let listings: Vec<u64> = queries(&fx)
        .into_iter()
        .filter(|(_, cmds)| *cmds == refresh_cmds())
        .map(|(n, _)| n)
        .collect();
    let [n] = listings[..] else {
        panic!("the vacate's listing: {fx:?}");
    };
    let fx = answer_on(&mut r, 2, n, cockpit_scene(true));
    let chains: Vec<u64> = queries(&fx)
        .into_iter()
        .filter(|(_, cmds)| cmds.len() == 1 && cmds[0].name() == "chain")
        .map(|(n, _)| n)
        .collect();
    let [n] = chains[..] else {
        panic!("the vacate's chain: {fx:?}");
    };
    let fx = answer_on(&mut r, 2, n, vec![Ok(Vec::new())]);
    assert!(!opens(&fx), "{fx:?}");
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
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    let fx = r.bound("%7");
    assert_eq!(replies(&fx), [(ReqId(1), restarted())]);
    assert!(r.st.binding(s_id()).is_some_and(|b| b.tracked));
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// The likely order: the new tab is bound within milliseconds of the kill, and the old
/// agent's end comes after. It carries no `MOA_SESSION`, so it is still history only.
#[test]
fn the_old_agents_end_after_the_bind_is_history_only() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    let fx = old_agent_ended(&mut r, 1000);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    assert!(
        r.st.binding(s_id())
            .is_some_and(|b| b.tracked && b.pane_id == "%7")
    );
}

/// A slow old agent: its end is stamped after the new agent's start. It still changes
/// nothing, and neither does any other late event of its own.
#[test]
fn the_old_agents_end_after_the_new_agents_start_is_history_only() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    let fx = hook(
        &mut r,
        true,
        HookKind::SessionStarted {
            source: StartSource::Resume,
        },
        1000,
        700,
    );
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Session(s) if s.id == s_id())),
        "the new agent's start applies: {fx:?}"
    );
    let seen = r.s().last_seen_at;
    let fx = old_agent_ended(&mut r, 2000);
    history_only(&fx);
    let fx = hook(
        &mut r,
        false,
        HookKind::TurnStarted {
            prompt: "late".into(),
        },
        3000,
        901,
    );
    assert!(
        !changes(&fx).iter().any(|c| matches!(c, Change::Session(_))),
        "{fx:?}"
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    assert_eq!(r.s().activity, Activity::Idle);
    assert_eq!(r.s().last_seen_at, seen);
}

/// The fence is only for agents moa didn't start: the new agent's own unexpected end still
/// puts S to sleep (R14).
#[test]
fn the_new_agents_own_end_still_counts() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    hook(&mut r, true, ended(), 1000, 700);
    assert!(is_asleep(r.s()), "{:?}", r.s());
}

/// While the restart's tab opens, any end of S's is history only, `MOA_SESSION` or not:
/// tmux names the new tab within milliseconds, long before the new agent could end, so
/// only the old agent ends then. Should the new agent's tab exit before tmux names it, the
/// exit still puts S to sleep once it does (R14).
#[test]
fn an_end_before_the_new_tab_is_known_is_history_only() {
    let mut r = World::new().reconciled();
    r.committed(1);
    let fx = hook(&mut r, true, ended(), 1000, 700);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    r.pane_exited("%7");
    r.bound("%7");
    assert!(is_asleep(r.s()), "{:?}", r.s());
    assert!(r.st.binding(s_id()).is_none());
}

/// `adopt` bound S to the agent moa started for it, in a terminal tab (`bind_own`): that
/// agent carries `MOA_SESSION` = S. Restarted, its end while the new tab opens is history
/// only: S stays live, its launch waiting, and the new tab answers the restart.
#[test]
fn the_old_moa_started_agents_end_while_restarting_is_history_only() {
    let mut r = restarted_from_a_moa_started_agent();
    let fx = hook(&mut r, true, ended(), 1000, 900);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());
    let fx = r.bound("%7");
    assert_eq!(replies(&fx), [(ReqId(2), restarted())]);
    assert!(r.st.binding(s_id()).is_some_and(|b| b.tracked));
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
}

/// S bound by `adopt` to the agent moa started for it, in terminal tab %5: S restarted.
fn restarted_from_a_moa_started_agent() -> Rig {
    let mut w = World::new();
    w.binding = None;
    MOA_STARTED.with(|m| m.set(true));
    let mut r = w.reconciled();
    let fx = send(
        &r.b,
        &mut r.st,
        1,
        Op::SessionAdopt {
            pane: "%5".into(),
            title: None,
            group: None,
            id: None,
        },
    );
    assert!(queried(&fx), "{fx:?}");
    let lantern = shown(&r.b.home.join("work/lantern"));
    let line = format!("I\t%5\t505\t0\t1\t\t\t{lantern}");
    let fx = listed(&mut r.st, &r.b, &[&line], &[]);
    assert!(
        matches!(replies(&fx).as_slice(), [(ReqId(1), Ok(d))] if d.text.starts_with("bound ")),
        "{fx:?}"
    );
    assert!(r.st.binding(s_id()).is_some_and(|b| !b.tracked));
    r.committed(2);
    r
}

/// R14 holds for a restarted session: once its new agent's tab exits (and loses its
/// binding), that agent's clean quit, from %7, still ends it.
#[test]
fn the_new_agents_clean_quit_after_its_tab_exits_ends_it() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    r.pane_exited("%7");
    assert!(is_asleep(r.s()), "{:?}", r.s());
    assert!(r.st.binding(s_id()).is_none());
    hook(
        &mut r,
        true,
        HookKind::SessionEnded {
            reason: "prompt_input_exit".into(),
        },
        1000,
        700,
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Ended);
    assert_eq!(r.s().asleep_at, None, "{:?}", r.s());
}

/// The likely order for an old agent moa started (it carries `MOA_SESSION` = S): the new
/// tab is bound first, and the old agent's end comes after. Its tab (%5) isn't S's tab any
/// more, so the end is history only; the new agent's own events, from %7, apply.
#[test]
fn the_old_moa_started_agents_end_after_the_bind_is_history_only() {
    let mut r = restarted_from_a_moa_started_agent();
    r.bound("%7");
    let fx = hook_in(&mut r, true, Some("%5"), ended(), 1000, 900);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_some_and(|b| b.pane_id == "%7"));
    hook_in(
        &mut r,
        true,
        Some("%7"),
        HookKind::TurnStarted {
            prompt: "go on".into(),
        },
        2000,
        700,
    );
    assert_eq!(r.s().activity, Activity::Working);
    hook_in(&mut r, true, Some("%7"), ended(), 3000, 700);
    assert!(is_asleep(r.s()), "{:?}", r.s());
}

/// A start the old agent sent before the restart, arriving after it (a late `/clear`), is
/// that agent's, not a new one's: it doesn't lift the fence, and the old agent's end that
/// follows is still history only.
#[test]
fn a_late_start_from_before_the_restart_keeps_the_fence() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    hook(
        &mut r,
        false,
        HookKind::SessionStarted {
            source: StartSource::Clear,
        },
        -1000,
        905,
    );
    let fx = old_agent_ended(&mut r, 500);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
}

/// A start the old agent sent between the restart's commit and the kill of its tab, so
/// stamped after the restart, is still that agent's: it comes from the killed tab. It
/// doesn't lift the fence, and the old agent's end that follows is still history only.
#[test]
fn a_late_start_from_the_killed_tab_keeps_the_fence() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
    let fx = hook(
        &mut r,
        false,
        HookKind::SessionStarted {
            source: StartSource::Clear,
        },
        1000,
        905,
    );
    assert!(
        matches!(changes(&fx).as_slice(), [Change::Event(e)]
            if e.kind.0 == EventKind::SESSION_STARTED && e.session_id == Some(s_id())),
        "{fx:?}"
    );
    let fx = old_agent_ended(&mut r, 2000);
    history_only(&fx);
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(!is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_some_and(|b| b.pane_id == "%7"));
}

/// After a restart, a start that doesn't name S (C9 resumed by hand in another app) is a
/// new agent of S's: the killed one can't start again. It lifts the fence and brings S
/// back, as it would have before the restart: live, with no tab here, and its own later
/// events apply.
#[test]
fn a_hand_resume_after_a_restart_brings_it_back() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
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
    assert_eq!(kills(&fx), [&kill("%7")]);
    assert!(is_asleep(r.s()));

    hook(
        &mut r,
        false,
        HookKind::SessionStarted {
            source: StartSource::Resume,
        },
        1000,
        903,
    );
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert_eq!(r.s().asleep_at, None);
    assert!(r.st.binding(s_id()).is_none());
    hook(
        &mut r,
        false,
        HookKind::TurnStarted {
            prompt: "go on".into(),
        },
        2000,
        904,
    );
    assert_eq!(r.s().activity, Activity::Working);
}

/// Once moa takes on a hand-started agent for S again (adopt), that agent is S's own: its
/// events apply.
#[test]
fn adopting_again_lifts_the_fence() {
    let mut r = World::new().reconciled();
    r.committed(1);
    r.bound("%7");
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
    assert_eq!(kills(&fx), [&kill("%7")]);
    assert!(is_asleep(r.s()));

    // A new hand-started agent on C9, in tab %8 (pid 505 again, as `inspect` knows it).
    let fx = send(
        &r.b,
        &mut r.st,
        3,
        Op::SessionAdopt {
            pane: "%8".into(),
            title: None,
            group: None,
            id: None,
        },
    );
    assert!(queried(&fx), "{fx:?}");
    let lantern = shown(&r.b.home.join("work/lantern"));
    let line = format!("I\t%8\t505\t0\t\t\t\t{lantern}");
    let fx = listed(&mut r.st, &r.b, &[&line], &[]);
    assert!(
        matches!(replies(&fx).as_slice(), [(ReqId(3), Ok(_))]),
        "{fx:?}"
    );
    assert!(r.st.binding(s_id()).is_some_and(|b| !b.tracked));

    hook(
        &mut r,
        false,
        HookKind::TurnStarted {
            prompt: "go on".into(),
        },
        1000,
        902,
    );
    assert_eq!(r.s().activity, Activity::Working);
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

/// A read of the adopted tab that fails proves nothing (R31): restart, resume and focus
/// refuse, and nothing is saved, killed, untagged, asked or launched.
#[test]
fn restart_resume_and_focus_refuse_when_the_tab_cant_be_read() {
    let ops: [Op; 3] = [
        Op::SessionRestart { q: S.into() },
        Op::SessionResume {
            q: S.into(),
            bg: false,
        },
        Op::SessionFocus { q: S.into() },
    ];
    for op in ops {
        let mut r = World::new().reconciled();
        UNREADABLE.with(|a| a.set(true));
        let before = r.s().clone();
        let fx = send(&r.b, &mut r.st, 1, op.clone());
        assert_eq!(
            refusal(&fx),
            format!("moa couldn't read what runs in {TITLE}'s tab; try again in a moment"),
            "{op:?}"
        );
        assert!(sent(&fx).is_empty(), "{op:?}: {fx:?}");
        assert!(changes(&fx).is_empty(), "{op:?}: {fx:?}");
        assert_eq!(r.s(), &before, "{op:?}");
        assert!(r.st.binding(s_id()).is_some_and(|b| !b.tracked));
    }
}

/// The tab can't be read once the restart's listing answers: it refuses then, as above.
#[test]
fn restart_refuses_when_the_tab_cant_be_read_once_the_tabs_answer() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.restart(1)));
    UNREADABLE.with(|a| a.set(true));
    let fx = r.listed();
    assert_eq!(
        refusal(&fx),
        format!("moa couldn't read what runs in {TITLE}'s tab; try again in a moment")
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(windows(&fx).is_empty(), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.s().lifecycle, Lifecycle::Live);
    assert!(r.st.binding(s_id()).is_some());
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
/// so the running agent is left alone. It is marked so, as a resume would mark it (rule 3):
/// once its agent stops it can't be resumed either, and its next start clears the mark.
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
    assert!(r.st.binding(s_id()).is_some_and(|b| !b.tracked));
    let saved = changes(&fx);
    assert!(
        matches!(saved.as_slice(), [Change::Session(s)]
            if s.resume_failed && s.lifecycle == Lifecycle::Live && s.asleep_at.is_none()),
        "{saved:?}"
    );
    assert!(r.s().resume_failed);
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
    // The second joined the first's launch, and hears where it went, as a joined resume
    // does.
    let fx = r.bound("%7");
    assert_eq!(
        replies(&fx),
        [
            (ReqId(1), restarted()),
            (
                ReqId(2),
                Ok(Done {
                    text: format!("went to {TITLE}"),
                    data: json!({ "id": s_id(), "title": TITLE }),
                })
            )
        ]
    );
}

/// R13a: a restart whose link goes away before its new tab is answered leaves the session
/// asleep and resumable, not failed; the old tab was already killed.
#[test]
fn a_restart_whose_link_goes_away_sleeps() {
    let mut r = World::new().reconciled();
    r.committed(1);
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    assert_eq!(refusal(&fx), "moa's terminal went away before it opened");
    let s = r.s();
    assert!(is_asleep(s), "{s:?}");
    assert_eq!(s.asleep_at, Some(r.b.now().wall));
    assert!(!s.resume_failed);
    assert!(r.st.binding(s_id()).is_none());
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

/// The batch that held the restart's commit failed: the reload brings S back as it was, its
/// adopted tab bound and its agent running (the kill and the window were dropped with the
/// batch). The caller hears nothing was saved, and the launch isn't left waiting: another
/// restart looks at the tabs again rather than joining a launch that never went out, and
/// the old agent's own end counts again.
#[test]
fn a_reload_that_lost_a_restarts_commit_answers_it() {
    let mut r = World::new().reconciled();
    let s = r.s().clone();
    let binding = r.st.binding(s_id()).cloned().unwrap();
    let convs: Vec<Conversation> = r.st.conversations_of(s_id()).cloned().collect();
    let fx = r.committed(1);
    assert_eq!(windows(&fx).len(), 1, "{fx:?}");
    let mut snap = r.b.snapshot();
    snap.sessions.push(s);
    snap.conversations.extend(convs);
    snap.pane_bindings.push(binding.clone());
    let fx = handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        r.b.now(),
    );
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(
                "moa couldn't save that session; nothing was opened, try again".into()
            ))
        )]
    );
    assert_eq!(r.st.binding(s_id()), Some(&binding));
    assert!(!is_asleep(r.s()));

    // A restart now looks at the tabs again (once they are reconciled), not at a dead launch.
    let lantern = shown(&r.b.home.join("work/lantern"));
    let line = format!("I\t%5\t505\t0\t\t1\t{S}\t{lantern}");
    listed(&mut r.st, &r.b, &[&line], &[]);
    let fx = r.restart(2);
    assert!(queried(&fx), "{fx:?}");
    handle_checked(
        &mut r.st,
        Input::ClientGone(moa_core::op::ConnId(1)),
        r.b.now(),
    );

    // The old agent still runs: its end is S's.
    old_agent_ended(&mut r, 1000);
    assert!(is_asleep(r.s()), "{:?}", r.s());
}

/// The batch that held the restart's commit failed, so its kill never went out: the reload
/// brings S back bound to its adopted tab, %5, the tab the restart had queued to kill. %5 is
/// S's tab again, so its agent's events count again: a turn it starts is applied.
#[test]
fn a_tab_the_session_is_bound_to_again_isnt_a_killed_tab() {
    let mut r = World::new().reconciled();
    let s = r.s().clone();
    let binding = r.st.binding(s_id()).cloned().unwrap();
    let convs: Vec<Conversation> = r.st.conversations_of(s_id()).cloned().collect();
    r.committed(1);
    let mut snap = r.b.snapshot();
    snap.sessions.push(s);
    snap.conversations.extend(convs);
    snap.pane_bindings.push(binding.clone());
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        r.b.now(),
    );
    assert_eq!(r.st.binding(s_id()), Some(&binding));

    hook(
        &mut r,
        false,
        HookKind::TurnStarted {
            prompt: "go on".into(),
        },
        1000,
        900,
    );
    assert_eq!(r.s().activity, Activity::Working, "{:?}", r.s());
}

/// A reload that kept the restart's commit (another batch failed) leaves its launch
/// waiting: its kill and window went out, and tmux's answer binds the new tab.
#[test]
fn a_reload_that_kept_a_restarts_commit_keeps_waiting() {
    let mut r = World::new().reconciled();
    r.committed(1);
    assert!(r.st.binding(s_id()).is_none());
    let mut snap = r.b.snapshot();
    snap.sessions.push(r.s().clone());
    snap.conversations
        .extend(r.st.conversations_of(s_id()).cloned());
    let fx = handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        r.b.now(),
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    listed(&mut r.st, &r.b, &[], &[]);
    let fx = r.bound("%7");
    assert_eq!(replies(&fx), [(ReqId(1), restarted())]);
    assert_eq!(r.st.binding(s_id()).map(|b| b.tracked), Some(true));
}
