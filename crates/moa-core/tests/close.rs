//! `close` (end a session and kill its tab, only when moa can prove the tab is the
//! session's: R14, R15, R31) and `term` (a plain shell in a new tab, R16), on the typical
//! brain with its link played by hand.

mod common;

use std::cell::Cell;
use std::path::PathBuf;

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{
    Down, Effect, Input, Publish, RefuseWrites, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::live::is_asleep;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, EventKind, Lifecycle, Location,
    PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
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

/// The running session S, and its conversation.
const S: &str = "00000000-0000-4000-8000-0000000000c1";
const C: &str = "c0000000-0000-4000-8000-0000000000c1";
const TITLE: &str = "boards";

fn s_id() -> SessionId {
    S.parse().unwrap()
}

fn shown(p: &std::path::Path) -> String {
    p.display().to_string()
}

thread_local! {
    /// Whether S's adopted agent still runs in its shell tab, %9.
    static AGENT_RUNS: Cell<bool> = const { Cell::new(true) };
}

/// S's agent in %9 (pid 909), started by hand: no `MOA_SESSION`, while it runs.
fn s_in_a_shell(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 909 && AGENT_RUNS.with(Cell::get)).then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: Some(C.into()),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    })
}

/// The world before loading: the typical brain plus S, live in `<home>/work/lantern` on
/// conversation C, bound to tab %1 of this server, which moa started.
struct World {
    b: TypicalBrain,
    s: Session,
    binding: Option<PaneBinding>,
    deps: Deps,
    /// S's row was loaded with a value this build doesn't know: no write to it is saved.
    unknown: bool,
}

impl World {
    fn new() -> World {
        let b = TypicalBrain::new();
        let lantern = b.home.join("work/lantern");
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
            pane_id: "%1".into(),
            pane_pid: 101,
            bound_at: at,
            tracked: true,
        };
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
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

    /// S adopted in shell tab %9: an untracked binding, its agent checked by `inspect`.
    fn adopted() -> World {
        let mut w = World::new();
        let b = w.binding.as_mut().unwrap();
        b.pane_id = "%9".into();
        b.pane_pid = 909;
        b.tracked = false;
        w.deps.inspect = s_in_a_shell;
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
        let conv = Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: C.into(),
            session_id: s_id(),
            store_dir: Some(shown(&self.b.home.join(".claude-a"))),
            spike_account: None,
            reason: ConversationReason::Start,
            first_seen_at: UtcMs(1),
            last_seen_at: UtcMs(1),
            transcript_path: None,
            transcript_present: None,
            transcript_checked_at: None,
        };
        let cwd = self.s.cwd.clone().unwrap();
        let line = self.binding.as_ref().map(|b| {
            let adopted = if b.tracked { "" } else { "1" };
            format!(
                "I\t{}\t{}\t0\t\t{adopted}\t{S}\t{cwd}",
                b.pane_id, b.pane_pid
            )
        });
        let mut snap = self.b.snapshot();
        snap.sessions.push(self.s);
        snap.conversations.push(conv);
        snap.pane_bindings.extend(self.binding);
        if self.unknown {
            snap.unknown.push(LoadNote {
                table: "sessions",
                key: S.into(),
                reason: "a state this build doesn't know",
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

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    fn close(&mut self, req: u64) -> Vec<Effect> {
        send(&self.b, &mut self.st, req, Op::SessionClose { q: S.into() })
    }

    fn s(&self) -> &Session {
        self.st.session(s_id()).unwrap()
    }
}

fn kill(pane: &str) -> Cmd {
    Cmd::new("kill-pane").args(["-t", pane])
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

fn kills(fx: &[Effect]) -> Vec<&Cmd> {
    sent(fx)
        .into_iter()
        .filter(|c| c.name() == "kill-pane")
        .collect()
}

fn closed() -> Result<Done, OpError> {
    Ok(Done {
        text: "closed".into(),
        data: json!({ "id": S }),
    })
}

fn ui_texts(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Event(e) if e.kind.0 == "ui" => e.text.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn close_kills_the_tab_after_the_commit() {
    let mut r = World::new().reconciled();
    let fx = r.close(1);
    let now = r.b.now().wall;

    // The session ended (not asleep), its binding gone, and "closed boards" said, in one
    // commit.
    let persists: Vec<usize> = fx
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Effect::Persist(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(persists, [0], "{fx:?}");
    let saved = changes(&fx);
    assert!(
        saved.iter().any(|c| matches!(c, Change::Session(s)
            if s.id == s_id()
                && s.lifecycle == Lifecycle::Ended
                && s.asleep_at.is_none()
                && s.activity_at == Some(now))),
        "{fx:?}"
    );
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(id) if *id == s_id())),
        "{fx:?}"
    );
    assert_eq!(ui_texts(&fx), [format!("closed {TITLE}")]);

    // Then the views, then the kill, then the reply (R15).
    let publish = fx
        .iter()
        .position(|e| matches!(e, Effect::Publish(Publish::Model(_))))
        .expect("the views hear of it");
    let killed = fx
        .iter()
        .position(
            |e| matches!(e, Effect::Tmux { conn: 1, cmds, token: None } if cmds == &[kill("%1")]),
        )
        .expect("the tab is killed");
    let replied = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .expect("a reply");
    assert!(
        0 < publish && publish < killed && killed < replied,
        "{fx:?}"
    );
    assert!(kill("%1").check().is_ok());
    moa_core::effect::check_order(&fx).unwrap();
    assert_eq!(replies(&fx), [(ReqId(1), closed())]);
    assert!(r.st.binding(s_id()).is_none());
    assert!(!is_asleep(r.s()));
}

/// A session that isn't running is still marked ended, as the spike did; there's no tab
/// to kill.
#[test]
fn close_of_a_stopped_session_just_ends_it() {
    let mut r = World::asleep().reconciled();
    let fx = r.close(1);
    assert!(kills(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(replies(&fx), [(ReqId(1), closed())]);
    assert_eq!(r.s().lifecycle, Lifecycle::Ended);
    assert!(r.s().asleep_at.is_none(), "closed, not asleep");
    assert_eq!(ui_texts(&fx), [format!("closed {TITLE}")]);
}

/// A session whose tab is opening has no pane to kill yet.
#[test]
fn close_refuses_while_opening() {
    let mut r = World::asleep().reconciled();
    let op = Op::SessionOpen {
        path: r.b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = the_open(&send(&r.b, &mut r.st, 1, op)).0;
    let fx = send(&r.b, &mut r.st, 2, Op::SessionClose { q: id.to_string() });
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(2),
            Err(OpError("it's still opening; try again in a moment".into()))
        )]
    );
}

/// A resume still looking at the tabs launches the session once they answer: a close before
/// then would be undone by that launch, so it is refused the same way.
#[test]
fn close_refuses_while_a_resume_looks_at_the_tabs() {
    let mut r = World::asleep().reconciled();
    let op = Op::SessionResume {
        q: S.into(),
        bg: true,
    };
    let fx = send(&r.b, &mut r.st, 1, op);
    assert!(
        replies(&fx).is_empty(),
        "the resume waits for its pane query: {fx:?}"
    );
    assert_eq!(r.st.tmux().queries().count(), 1, "{fx:?}");
    let fx = r.close(2);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(2),
            Err(OpError("it's still opening; try again in a moment".into()))
        )]
    );
    assert!(is_asleep(r.s()));
}

/// A close the store refuses kills nothing (R15): the pane would die while the brain still
/// says live.
#[test]
fn close_with_writes_refused_kills_nothing() {
    // S's row can't be written.
    let mut w = World::new();
    w.unknown = true;
    let mut r = w.reconciled();
    let fx = r.close(1);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    let [(_, Err(e))] = &replies(&fx)[..] else {
        panic!("a refusal: {fx:?}");
    };
    assert!(
        e.0.ends_with("has a value this build doesn't know; update moa"),
        "{e:?}"
    );
    assert!(r.st.binding(s_id()).is_some());
    assert_eq!(r.s().lifecycle, Lifecycle::Live);

    // Nothing can be written: the tabs are looked at again once writes are accepted, and
    // until then nothing acts on them.
    let mut r = World::new().reconciled();
    let snapshot = r.b.snapshot();
    let mut snap = snapshot;
    snap.sessions.push(r.s().clone());
    snap.pane_bindings.extend(r.st.binding(s_id()).cloned());
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(r.b.now().wall.0 + 60_000),
                reason: "disk full".into(),
            }),
        },
        r.b.now(),
    );
    let fx = r.close(2);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(replies(&fx), [(ReqId(2), Err(OpError(NOT_READY.into())))]);
}

/// R31: an adopted agent that left its shell tab leaves the tab to the user. The session
/// goes to sleep, its binding goes, and nothing is killed. Something was saved, so the
/// answer is success-shaped.
#[test]
fn close_of_an_adopted_session_whose_agent_left_kills_nothing() {
    AGENT_RUNS.with(|c| c.set(true));
    let mut r = World::adopted().reconciled();
    assert!(
        !r.st.binding(s_id()).unwrap().tracked,
        "reconcile kept the adopted tab"
    );
    AGENT_RUNS.with(|c| c.set(false));
    let fx = r.close(1);
    assert!(kills(&fx).is_empty(), "{fx:?}");
    // The tab is the user's again: moa's tags come off it, after the commit (R15).
    let want = untagged("%9");
    assert_eq!(unsets(&fx), want.iter().collect::<Vec<_>>(), "{fx:?}");
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let untag = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { cmds, .. } if cmds.contains(&want[0])))
        .unwrap();
    assert!(persist < untag, "{fx:?}");
    assert_eq!(
        r.st.tmux().topology.moa_session("%9".parse().unwrap()),
        None
    );
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(id) if *id == s_id())),
        "{fx:?}"
    );
    assert!(ui_texts(&fx).is_empty(), "asleep writes no row: {fx:?}");
    assert!(is_asleep(r.s()));
    assert!(r.st.binding(s_id()).is_none());
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Ok(Done {
                text: format!("{TITLE}'s agent isn't running in that tab any more"),
                data: json!({ "id": S }),
            })
        )]
    );
}

/// The adopted agent still runs there: its tab is killed like any other.
#[test]
fn close_of_an_adopted_session_whose_agent_runs_kills_its_tab() {
    AGENT_RUNS.with(|c| c.set(true));
    let mut r = World::adopted().reconciled();
    let fx = r.close(1);
    assert_eq!(kills(&fx), [&kill("%9")]);
    assert_eq!(replies(&fx), [(ReqId(1), closed())]);
}

/// R14: the kill makes Claude send `SessionEnd(other)`, and tmux report the pane's exit.
/// Neither turns the closed session into an asleep one.
#[test]
fn the_agents_end_after_close_is_history_only() {
    let mut r = World::new().reconciled();
    r.close(1);

    let at = r.b.now().wall.0 + 1000;
    let fx = handle_checked(
        &mut r.st,
        Input::Hook {
            reply: None,
            envelope: HookEnvelope {
                agent: AgentKind::Claude,
                slot: Some(S.into()),
                brain: Some("default".into()),
                store_dir: Some(shown(&r.b.home.join(".claude-a"))),
                conversation_id: C.into(),
                kind: HookKind::SessionEnded {
                    reason: "other".into(),
                },
                at: UtcMs(at),
                at_ns: at * 1_000_000,
                pid: 100,
                seq: 1,
                cwd: None,
                transcript_path: None,
            },
        },
        r.b.now(),
    );
    // Saved as history, and nothing more.
    assert!(
        changes(&fx).iter().any(|c| matches!(c, Change::Event(e)
            if e.kind.0 == EventKind::SESSION_ENDED && e.session_id == Some(s_id()))),
        "{fx:?}"
    );
    assert!(
        !changes(&fx).iter().any(|c| matches!(c, Change::Session(_))),
        "{fx:?}"
    );
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Notification(Notification::Message("moa:hook pane-exited %1".into())),
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let after = r.s();
    assert_eq!(after.lifecycle, Lifecycle::Ended);
    assert!(after.asleep_at.is_none());
}

// --- term ---

/// The typical brain, linked; its tabs not looked at (a terminal binds nothing).
fn linked() -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    (b, st)
}

fn term_op(path: PathBuf, title: Option<&str>) -> Op {
    Op::TermOpen {
        path,
        title: title.map(str::to_string),
    }
}

/// The one tokened query's number and commands.
fn the_query(fx: &[Effect]) -> (u64, Vec<Cmd>) {
    let asked: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Query(n)),
            } => Some((*n, cmds.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 1, "{fx:?}");
    asked[0].clone()
}

fn answer(b: &TypicalBrain, st: &mut State, n: u64, line: &str) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec![line.to_string()])],
        },
    )
}

fn new_window(name: &str, path: &str) -> Cmd {
    Cmd::new("new-window").args([
        "-d",
        "-P",
        "-F",
        "P #{pane_id} #{pane_pid}",
        "-n",
        name,
        "-c",
        path,
        "-t",
        "=moa:",
    ])
}

fn tags(pane: &str, label: &str) -> Vec<Cmd> {
    vec![
        Cmd::new("set-option").args(["-p", "-t", pane, "@moa_term", "1"]),
        Cmd::new("set-option").args(["-p", "-t", pane, "@moa_label", label]),
        Cmd::new("select-window").args(["-t", pane]),
        Cmd::new("select-pane").args(["-t", pane]),
    ]
}

#[test]
fn term_opens_a_shell_tab_and_tags_it() {
    let (b, mut st) = linked();
    let lantern = b.home.join("work/lantern");
    let fx = send(&b, &mut st, 1, term_op(lantern.clone(), None));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let (n, cmds) = the_query(&fx);
    // No command: the user's shell, with no moa environment.
    assert_eq!(cmds, [new_window("⌨ lantern", &shown(&lantern))]);
    assert!(cmds[0].check().is_ok());

    let fx = answer(&b, &mut st, n, "P %5 777");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let tagged = fx
        .iter()
        .position(|e| {
            matches!(e, Effect::Tmux { conn: 1, cmds, token: None }
                if *cmds == tags("%5", "⌨ lantern"))
        })
        .unwrap_or_else(|| panic!("tagged and focused: {fx:?}"));
    let replied = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .expect("a reply");
    assert!(tagged < replied, "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Ok(Done {
                text: "opened a terminal in ~/work/lantern (tab ⌨ lantern)".into(),
                data: json!({ "pane": "%5", "name": "⌨ lantern" }),
            })
        )]
    );
}

/// A `#[…]` in a title would style the tab's border text: it is doubled, in the name and
/// the label. The name is cut to 24 characters first.
#[test]
fn a_terms_name_is_literal_and_cut() {
    let (b, mut st) = linked();
    let fx = send(&b, &mut st, 1, term_op(b.home.clone(), Some("a#[fg=red]b")));
    let (n, cmds) = the_query(&fx);
    assert_eq!(cmds, [new_window("⌨ a##[fg=red]b", &shown(&b.home))]);
    let fx = answer(&b, &mut st, n, "P %5 777");
    assert_eq!(
        sent(&fx),
        tags("%5", "⌨ a##[fg=red]b").iter().collect::<Vec<_>>()
    );
    let [(_, Ok(done))] = &replies(&fx)[..] else {
        panic!("{fx:?}");
    };
    assert_eq!(done.text, "opened a terminal in ~ (tab ⌨ a#[fg=red]b)");

    // No title: the folder's name, else `~` for home.
    let fx = send(&b, &mut st, 2, term_op(b.home.clone(), None));
    assert_eq!(the_query(&fx).1, [new_window("⌨ ~", &shown(&b.home))]);
    let long = "a-title-much-longer-than-a-tab-has-room-for";
    let fx = send(&b, &mut st, 3, term_op(b.home.clone(), Some(long)));
    let name: String = format!("⌨ {long}").chars().take(24).collect();
    assert_eq!(the_query(&fx).1, [new_window(&name, &shown(&b.home))]);
}

/// R26: the caller went before tmux answered. The tab is on its way: it still gets its tags.
#[test]
fn a_term_whose_caller_left_is_still_tagged() {
    let (b, mut st) = linked();
    let fx = send(&b, &mut st, 1, term_op(b.home.join("work"), None));
    let (n, _) = the_query(&fx);
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(1)), b.now());
    assert!(fx.is_empty(), "{fx:?}");
    let fx = answer(&b, &mut st, n, "P %5 777");
    assert_eq!(sent(&fx), tags("%5", "⌨ work").iter().collect::<Vec<_>>());
    assert!(replies(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_term_tmux_refuses_says_so() {
    let (b, mut st) = linked();
    let fx = send(&b, &mut st, 1, term_op(b.home.join("work"), None));
    let (n, _) = the_query(&fx);
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Err("create window failed".into())],
        },
    );
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError("couldn't open a terminal; see moa's log".into()))
        )]
    );
}

fn logs(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l.message.clone()),
            _ => None,
        })
        .collect()
}

/// tmux refusing the window is logged as a refusal, not as an answer that didn't parse.
#[test]
fn a_term_tmux_refuses_is_logged_as_refused() {
    let (b, mut st) = linked();
    let fx = send(&b, &mut st, 1, term_op(b.home.join("work"), None));
    let (n, _) = the_query(&fx);
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Err("create window failed".into())],
        },
    );
    let logs = logs(&fx);
    assert!(
        logs.iter()
            .any(|l| l == "tmux couldn't open a terminal's tab"),
        "{logs:?}"
    );
    assert!(!logs.iter().any(|l| l.contains("parse")), "{logs:?}");
}

/// The link going down before tmux answers is said as a launch says it, and logged as what
/// happened.
#[test]
fn a_term_whose_terminal_went_away_says_so() {
    let (b, mut st) = linked();
    let fx = send(&b, &mut st, 1, term_op(b.home.join("work"), None));
    the_query(&fx);
    let fx = feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError("moa's terminal went away before it opened".into()))
        )]
    );
    let logs = logs(&fx);
    assert!(
        logs.iter()
            .any(|l| l == "moa's terminal went away before a terminal's tab opened"),
        "{logs:?}"
    );
    assert!(!logs.iter().any(|l| l.contains("parse")), "{logs:?}");
}

#[test]
fn term_needs_the_terminal() {
    let (b, mut st) = loaded(TOML);
    let fx = send(&b, &mut st, 1, term_op(b.home.join("work"), None));
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(
                "moa isn't open in a terminal yet; run moa first".into()
            ))
        )]
    );
}
