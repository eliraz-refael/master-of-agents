//! A newer event from a session that isn't live and has no tab (the human's option C, M7):
//! the event is history only, and moa asks one listing of the tabs. A tab that runs the
//! session's agent is bound, the session wakes, and the event is applied then; with none,
//! or no answer, the session stays as it was left. The typical brain, its link played by
//! hand; each tab's agent comes from a test function.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind, StartSource};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Inspected, Lifecycle, Location,
    PaneAgent, Session, SessionId,
};
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::panes::list_panes;
use moa_testkit::brain::TypicalBrain;

use common::*;

const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// Z: asleep in `~/private/boards`, with no tab, on conversation CZ.
const Z: &str = "00000000-0000-4000-8000-0000000000c8";
const CZ: &str = "c8000000-0000-4000-8000-0000000000c8";

fn z() -> SessionId {
    Z.parse().unwrap()
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

thread_local! {
    /// What a read of each pid finds; a pid not in it runs no agent.
    static AGENTS: RefCell<BTreeMap<i64, Inspected>> = const { RefCell::new(BTreeMap::new()) };
}

fn inspect(pid: i64, _: &[String]) -> Inspected {
    AGENTS.with(|a| a.borrow().get(&pid).cloned().unwrap_or(Inspected::Gone))
}

/// A Claude on acct-a's store running `conv`, started long ago.
fn claude(b: &TypicalBrain, conv: &str) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::Claude,
        store_dir: Some(shown(&b.home.join(".claude-a"))),
        conversation: Some(conv.into()),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: Some(UtcMs(1)),
    }
}

/// An hour before the typical now: when Z was last active and went to sleep.
fn earlier(b: &TypicalBrain) -> UtcMs {
    UtcMs(b.now().wall.0 - 3_600_000)
}

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    /// The typical brain plus Z, asleep (or, with `closed`, ended by `close`): loaded,
    /// linked, drained and reconciled with no tabs but the cockpit's.
    fn new(closed: bool) -> Rig {
        AGENTS.with(|a| a.borrow_mut().clear());
        let b = TypicalBrain::new();
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            inspect,
            ..Deps::default()
        };
        let cwd = shown(&b.home.join("private/boards"));
        let at = earlier(&b);
        let mut snap = b.snapshot();
        snap.sessions.push(Session {
            id: z(),
            agent_kind: AgentKind::Claude,
            account: Some("acct-a".into()),
            cwd: Some(cwd.clone()),
            start_cwd: Some(cwd),
            project_id: None,
            title: Some("boards".into()),
            title_locked: false,
            group_path: None,
            lifecycle: Lifecycle::Ended,
            activity: Activity::Idle,
            activity_at: Some(at),
            activity_why: Some("Done.".into()),
            last_prompt: None,
            last_turn_end_at: None,
            seen_at: Some(at),
            asleep_at: (!closed).then_some(at),
            archived_at: None,
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(1),
            last_seen_at: Some(at),
        });
        snap.conversations.push(Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: CZ.into(),
            session_id: z(),
            store_dir: Some(shown(&b.home.join(".claude-a"))),
            spike_account: None,
            reason: ConversationReason::Start,
            first_seen_at: UtcMs(1),
            last_seen_at: UtcMs(1),
            transcript_path: None,
            transcript_present: None,
            transcript_checked_at: None,
        });
        let mut st = loaded_with(&b, TOML, deps, snap);
        link_up(&b, &mut st);
        handle_checked(&mut st, Input::StartDrained, b.now());
        listed(&mut st, &b, &[], &[]);
        assert!(st.tmux().reconciled());
        Rig { b, st }
    }

    /// Z's agent sends `kind`, `ms` after the typical now, as hook `seq`.
    fn heard(&mut self, seq: u32, ms: i64, kind: HookKind) -> Vec<Effect> {
        let at = self.b.now().wall.0 + ms;
        let envelope = HookEnvelope {
            agent: AgentKind::Claude,
            slot: None,
            pane: None,
            brain: Some("default".into()),
            store_dir: Some(shown(&self.b.home.join(".claude-a"))),
            conversation_id: CZ.into(),
            kind,
            at: UtcMs(at),
            at_ns: at * 1_000_000,
            pid: 4242,
            seq,
            cwd: None,
            transcript_path: None,
        };
        handle_checked(
            &mut self.st,
            Input::Hook {
                reply: None,
                envelope,
            },
            self.b.now(),
        )
    }

    /// Z's agent runs in untagged shell tab %6 (pid 60).
    fn runs_in_6(&self) {
        let agent = claude(&self.b, CZ);
        AGENTS.with(|a| a.borrow_mut().insert(60, Inspected::Agent(agent)));
    }

    /// The listing shows untagged shell tab %6.
    fn tab_6(&self) -> String {
        format!(
            "I\t%6\t60\t0\t\t\t\t{}",
            shown(&self.b.home.join("private/boards"))
        )
    }

    fn z(&self) -> &Session {
        self.st.session(z()).unwrap()
    }
}

/// The pane queries `fx` asks.
fn queries(fx: &[Effect]) -> Vec<&[Cmd]> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                token: Some(TmuxToken::Query(_)),
                cmds,
                ..
            } => Some(cmds.as_slice()),
            _ => None,
        })
        .collect()
}

fn event_rows(fx: &[Effect]) -> usize {
    changes(fx)
        .iter()
        .filter(|c| matches!(c, Change::Event(_)))
        .count()
}

fn feed_rows(fx: &[Effect]) -> Vec<String> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

fn prompt(text: &str) -> HookKind {
    HookKind::TurnStarted {
        prompt: text.into(),
    }
}

/// `fx` saved one `events` row and nothing else: history only.
#[track_caller]
fn history_only(fx: &[Effect]) {
    let saved = changes(fx);
    assert!(matches!(saved.as_slice(), [Change::Event(_)]), "{saved:?}");
}

#[test]
fn a_session_heard_from_while_asleep_wakes_once_a_tab_runs_its_agent() {
    let mut r = Rig::new(false);
    let before = r.z().clone();
    r.runs_in_6();
    let fx = r.heard(1, 1000, prompt("go on"));
    // History only for now, and one listing asked.
    history_only(&fx);
    assert_eq!(queries(&fx), [list_panes().as_slice()], "{fx:?}");
    assert_eq!(r.z(), &before);

    let tab = r.tab_6();
    let fx = listed(&mut r.st, &r.b, &[&tab], &[]);
    // Bound, woken, then the event applied, once: no second history row.
    let s = r.z().clone();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);
    assert_eq!(s.activity, Activity::Working);
    assert_eq!(s.activity_why.as_deref(), Some("go on"));
    assert_eq!(s.activity_at, Some(UtcMs(r.b.now().wall.0 + 1000)));
    let bound = r.st.binding(z()).unwrap();
    assert_eq!((bound.pane_id.as_str(), bound.tracked), ("%6", false));
    assert_eq!(event_rows(&fx), 0, "{fx:?}");
    assert_eq!(feed_rows(&fx), ["boards › go on"]);
    // The binding comes before the event's session change in the one commit.
    let saved = changes(&fx);
    let bind = saved
        .iter()
        .position(|c| matches!(c, Change::PaneBinding(_)))
        .unwrap();
    let working = saved
        .iter()
        .position(|c| matches!(c, Change::Session(s) if s.activity == Activity::Working))
        .unwrap();
    assert!(bind < working, "{saved:?}");
    // Its tab is marked adopted and tagged, after the commit (R15).
    let mark = Cmd::new("set-option").args(["-p", "-t", "%6", "@moa_adopted", "1"]);
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let marked = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { token: None, cmds, .. } if cmds.contains(&mark)))
        .unwrap();
    assert!(persist < marked, "{fx:?}");
}

/// M7: with no tab running its agent, the session shows as it was left: no activity, why
/// or feed row moves. An ended session (one `close` ended) is looked for the same way.
#[test]
fn with_no_tab_running_its_agent_the_event_stays_history() {
    for closed in [false, true] {
        let mut r = Rig::new(closed);
        let before = r.z().clone();
        let fx = r.heard(1, 1000, prompt("go on"));
        history_only(&fx);
        assert_eq!(queries(&fx).len(), 1, "{fx:?}");
        let tab = r.tab_6();
        let fx = listed(&mut r.st, &r.b, &[&tab], &[]);
        assert!(changes(&fx).is_empty(), "{fx:?}");
        assert_eq!(r.z(), &before);
        assert!(r.st.binding(z()).is_none());
    }
}

/// A listing tmux refuses, or one the link loses before it answers: history only.
#[test]
fn a_failed_or_lost_listing_leaves_it_asleep() {
    for lost in [false, true] {
        let mut r = Rig::new(false);
        let before = r.z().clone();
        r.runs_in_6();
        r.heard(1, 1000, prompt("go on"));
        let n = r.st.tmux().queries().next().unwrap();
        let fx = if lost {
            feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost))
        } else {
            feed(
                &r.b,
                &mut r.st,
                TmuxIn::Replies {
                    conn: 1,
                    token: TmuxToken::Query(n),
                    results: vec![Err("no server".into()), Ok(Vec::new())],
                },
            )
        };
        assert!(changes(&fx).is_empty(), "{fx:?}");
        assert_eq!(r.z(), &before);
        assert!(r.st.binding(z()).is_none());
    }
}

/// With no link, nothing can be looked at: history only, and nothing asked.
#[test]
fn with_the_link_down_nothing_is_asked() {
    let mut r = Rig::new(false);
    let before = r.z().clone();
    r.runs_in_6();
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let fx = r.heard(1, 1000, prompt("go on"));
    history_only(&fx);
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.z(), &before);
}

/// Two events in a row ask one listing; once it shows the agent, both are applied, in
/// order.
#[test]
fn two_events_in_a_row_ask_one_listing() {
    let mut r = Rig::new(false);
    r.runs_in_6();
    let fx = r.heard(1, 1000, prompt("go on"));
    assert_eq!(queries(&fx).len(), 1, "{fx:?}");
    let fx = r.heard(
        2,
        2000,
        HookKind::TurnEnded {
            last_message: "Done again.".into(),
        },
    );
    history_only(&fx);
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.st.tmux().queries().count(), 1);

    let tab = r.tab_6();
    let fx = listed(&mut r.st, &r.b, &[&tab], &[]);
    assert_eq!(feed_rows(&fx), ["boards › go on", "boards ✓ turn done"]);
    let s = r.z();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.activity, Activity::Idle);
    assert_eq!(s.activity_why.as_deref(), Some("Done again."));
    assert_eq!(s.activity_at, Some(UtcMs(r.b.now().wall.0 + 2000)));
    // Answered: the next event from a session that isn't live would ask again.
    assert_eq!(r.st.tmux().queries().count(), 0);
}

/// A clean quit still ends an asleep session, and a start still wakes it, with no look.
#[test]
fn a_clean_quit_ends_and_a_start_wakes_without_a_look() {
    let mut r = Rig::new(false);
    let fx = r.heard(
        1,
        1000,
        HookKind::SessionEnded {
            reason: "prompt_input_exit".into(),
        },
    );
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.z().lifecycle, Lifecycle::Ended);
    assert_eq!(r.z().asleep_at, None);
    assert_eq!(feed_rows(&fx), ["boards ○ ended"]);

    let mut r = Rig::new(false);
    let fx = r.heard(
        1,
        1000,
        HookKind::SessionStarted {
            source: StartSource::Resume,
        },
    );
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.z().lifecycle, Lifecycle::Live);
    assert_eq!(r.z().asleep_at, None);
}

/// A sleep whose kill was lost with the link: moa's own tab for Z still runs, tagged for
/// it. The look binds it by its tag, tracked, as reconcile would, and adds no tags.
#[test]
fn a_tab_still_tagged_for_it_wakes_it_tracked() {
    let mut r = Rig::new(false);
    r.heard(1, 1000, prompt("go on"));
    let tab = format!(
        "I\t%6\t60\t0\t\t\t{Z}\t{}",
        shown(&r.b.home.join("private/boards"))
    );
    let fx = listed(&mut r.st, &r.b, &[&tab], &[]);
    assert_eq!(r.z().lifecycle, Lifecycle::Live);
    let bound = r.st.binding(z()).unwrap();
    assert_eq!((bound.pane_id.as_str(), bound.tracked), ("%6", true));
    assert_eq!(feed_rows(&fx), ["boards › go on"]);
    // Its tab's own options (`set-option -p`) stay as they are.
    let tagged = fx.iter().any(|e| {
        matches!(e, Effect::Tmux { token: None, cmds, .. }
            if cmds.iter().any(|c| c.name() == "set-option" && c.argv().first().is_some_and(|a| a == "-p")))
    });
    assert!(!tagged, "{fx:?}");
}

/// tmux's lists, read again: one window, `@1`, holding `panes`.
fn topology_shows(r: &mut Rig, panes: &[&str]) {
    let lines = panes
        .iter()
        .map(|p| format!("P {p} @1 $1 0 1 0 0 "))
        .collect();
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: vec![
                Ok(vec!["S $1 work".into()]),
                Ok(lines),
                Ok(Vec::new()),
                Ok(Vec::new()),
            ],
        },
    );
}

/// A look that found no tab running Z's agent isn't asked again for each event Z sends:
/// three more events ask nothing, so one listing in all.
#[test]
fn after_an_empty_look_more_events_ask_nothing() {
    let mut r = Rig::new(false);
    let before = r.z().clone();
    let fx = r.heard(1, 1000, prompt("go on"));
    let mut asked = queries(&fx).len();
    let tab = r.tab_6();
    listed(&mut r.st, &r.b, &[&tab], &[]);
    for seq in 2..=4 {
        let fx = r.heard(seq, i64::from(seq) * 1000, prompt("more"));
        history_only(&fx);
        asked += queries(&fx).len();
    }
    assert_eq!(asked, 1);
    assert_eq!(r.z(), &before);
}

/// A tab the look didn't see may run Z's agent: once tmux shows one, the next event asks
/// again.
#[test]
fn a_new_tab_since_the_empty_look_asks_again() {
    let mut r = Rig::new(false);
    r.heard(1, 1000, prompt("go on"));
    let tab = r.tab_6();
    listed(&mut r.st, &r.b, &[&tab], &[]);
    topology_shows(&mut r, &["%6"]);
    let fx = r.heard(2, 2000, prompt("more"));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    topology_shows(&mut r, &["%6", "%7"]);
    let fx = r.heard(3, 3000, prompt("more"));
    assert_eq!(queries(&fx).len(), 1, "{fx:?}");
}

/// A start from Z lifts the mark: once Z is asleep again, its next event asks again.
#[test]
fn a_start_since_the_empty_look_asks_again() {
    let mut r = Rig::new(false);
    r.heard(1, 1000, prompt("go on"));
    let tab = r.tab_6();
    listed(&mut r.st, &r.b, &[&tab], &[]);
    r.heard(
        2,
        2000,
        HookKind::SessionStarted {
            source: StartSource::Resume,
        },
    );
    assert_eq!(r.z().lifecycle, Lifecycle::Live);
    r.heard(
        3,
        3000,
        HookKind::SessionEnded {
            reason: "other".into(),
        },
    );
    assert_eq!(r.z().lifecycle, Lifecycle::Ended);
    let fx = r.heard(4, 4000, prompt("more"));
    assert_eq!(queries(&fx).len(), 1, "{fx:?}");
}

/// A look whose tab couldn't be read proves nothing: Z isn't marked, and its next event
/// asks again.
#[test]
fn a_look_that_couldnt_read_a_tab_asks_again() {
    let mut r = Rig::new(false);
    AGENTS.with(|a| a.borrow_mut().insert(60, Inspected::Unknown));
    r.heard(1, 1000, prompt("go on"));
    let tab = r.tab_6();
    listed(&mut r.st, &r.b, &[&tab], &[]);
    let fx = r.heard(2, 2000, prompt("more"));
    assert_eq!(queries(&fx).len(), 1, "{fx:?}");
}
