//! Reconcile: once per connection, after the start drain, the bindings are checked against
//! one listing of the server's panes. What a `kill-server` or a reboot left comes back
//! asleep, tabs that run a conversation moa knows are bound, and the spike's panes are
//! verified or cleared (spec §6.13, R4–R7).

mod common;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Down, Effect, Input, LogLine, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, PaneAgent,
    PaneBinding, Session, SessionId, SpikePane,
};
use moa_core::op::{Op, OpError};
use moa_core::state::{Deps, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId};
use moa_core::tmux::link::{Link, NOT_READY, ready};
use moa_core::tmux::panes::list_panes;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use proptest::prelude::*;

use common::*;

/// Brain `default` (folder `~/work`), opening with `acct-a`.
const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// The server the bindings were made on, and another one.
const G1: Generation = Generation { pid: 100, start: 1 };
const G2: Generation = Generation { pid: 200, start: 2 };

// Sessions, by number. A, B and C are live and bound to %1, %2 and %3 on G1.
const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 3;
const D: u8 = 4;
const E: u8 = 5;
const F: u8 = 6;
const G: u8 = 7;
const H: u8 = 8;
/// An adopted session (an untracked binding).
const U: u8 = 9;
/// A session somewhere else.
const X: u8 = 10;

fn id(n: u8) -> SessionId {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

fn tag(n: u8) -> String {
    id(n).to_string()
}

fn conv(n: u8) -> String {
    format!("c0000000-0000-4000-8000-0000000000{n:02x}")
}

/// Pane `%n`'s process.
fn pid(pane: u32) -> i64 {
    i64::from(pane) * 101
}

fn cwd(b: &TypicalBrain, n: u8) -> String {
    b.home.join(format!("work/s{n}")).display().to_string()
}

/// An hour before the typical now.
fn earlier(b: &TypicalBrain) -> UtcMs {
    UtcMs(b.now().wall.0 - 3_600_000)
}

fn session_in(n: u8, cwd: String, at: UtcMs) -> Session {
    Session {
        id: id(n),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.clone()),
        start_cwd: Some(cwd),
        project_id: None,
        title: Some(format!("s{n}")),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(at),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(at),
    }
}

fn conversation(n: u8, at: UtcMs) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conv(n),
        session_id: id(n),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: at,
        last_seen_at: at,
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

fn binding(n: u8, generation: Generation, pane: u32, tracked: bool) -> PaneBinding {
    PaneBinding {
        session_id: id(n),
        server_pid: generation.pid,
        server_start: generation.start,
        socket: "moa".into(),
        pane_id: format!("%{pane}"),
        pane_pid: pid(pane),
        bound_at: UtcMs(1),
        tracked,
    }
}

fn spike(n: u8, pane: u32, tmux: Option<&str>) -> SpikePane {
    SpikePane {
        session_id: id(n),
        pane_id: format!("%{pane}"),
        tmux: tmux.map(str::to_string),
        imported_at: UtcMs(1),
    }
}

/// A Claude agent, its store the default one.
fn claude(conversation: Option<String>, moa_session: Option<String>) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation,
        moa_session,
        moa_brain: None,
        title: None,
        group: None,
        started_at: None,
    }
}

fn no_agent(_: i64, _: &[String]) -> Option<PaneAgent> {
    None
}

/// The typical brain with its own sessions ended and its spike panes gone, so that only
/// this test's sessions take part: A, B and C, live and bound on G1.
struct World {
    b: TypicalBrain,
    snap: Snapshot,
}

impl World {
    fn new() -> World {
        let b = TypicalBrain::new();
        let mut snap = b.snapshot();
        for s in &mut snap.sessions {
            s.lifecycle = Lifecycle::Ended;
        }
        snap.pane_bindings.clear();
        snap.spike_panes.clear();
        let mut w = World { b, snap };
        for (n, pane) in [(A, 1), (B, 2), (C, 3)] {
            w.add(n);
            w.snap.pane_bindings.push(binding(n, G1, pane, true));
        }
        w
    }

    /// A live session `n` in its own folder, on its conversation, with no binding.
    fn add(&mut self, n: u8) -> &mut Session {
        let at = earlier(&self.b);
        self.snap.sessions.push(session_in(n, cwd(&self.b, n), at));
        self.snap.conversations.push(conversation(n, at));
        self.snap.sessions.last_mut().unwrap()
    }

    fn conversation(&mut self, n: u8) -> &mut Conversation {
        self.snap
            .conversations
            .iter_mut()
            .find(|c| c.session_id == id(n))
            .unwrap()
    }

    /// The state, with `inspect` as its process reads; the start drain not yet run.
    fn load(self, inspect: fn(i64, &[String]) -> Option<PaneAgent>) -> Rig {
        let deps = Deps {
            home: self.b.home.clone(),
            inspect,
            ..Deps::default()
        };
        let st = loaded_with(&self.b, TOML, deps, self.snap.clone());
        Rig {
            b: self.b,
            st,
            snap: self.snap,
        }
    }

    /// [`World::load`], then the start drain.
    fn drained(self, inspect: fn(i64, &[String]) -> Option<PaneAgent>) -> Rig {
        let mut r = self.load(inspect);
        r.input(Input::StartDrained);
        r
    }
}

struct Rig {
    b: TypicalBrain,
    st: State,
    snap: Snapshot,
}

impl Rig {
    fn input(&mut self, input: Input) -> Vec<Effect> {
        handle_checked(&mut self.st, input, self.b.now())
    }

    fn connect(&mut self, generation: Generation) -> Vec<Effect> {
        self.connect_on(1, generation)
    }

    /// The link comes up on connection `conn`.
    fn connect_on(&mut self, conn: u64, generation: Generation) -> Vec<Effect> {
        self.input(Input::Tmux(TmuxIn::Connected {
            conn,
            generation,
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        }))
    }

    fn listed(&mut self, info: &[String], start: &[String]) -> Vec<Effect> {
        let info: Vec<&str> = info.iter().map(String::as_str).collect();
        let start: Vec<&str> = start.iter().map(String::as_str).collect();
        listed(&mut self.st, &self.b, &info, &start)
    }

    fn session(&self, n: u8) -> &Session {
        self.st.session(id(n)).unwrap()
    }

    fn is_asleep(&self, n: u8) -> bool {
        let s = self.session(n);
        s.lifecycle == Lifecycle::Ended
            && s.asleep_at == Some(self.b.now().wall)
            && display_state(s, self.b.now().wall).0 == DisplayState::Asleep
            && self.st.binding(id(n)).is_none()
    }

    fn is_live(&self, n: u8) -> bool {
        let s = self.session(n);
        s.lifecycle == Lifecycle::Live && s.asleep_at.is_none()
    }

    fn bound_to(&self, n: u8) -> Option<&PaneBinding> {
        self.st.binding(id(n))
    }

    /// A moment `ms` after the typical now.
    fn later(&self, ms: i64) -> Now {
        let now = self.b.now();
        Now::at(UtcMs(now.wall.0 + ms), Duration::from_secs(1), now.tz)
    }
}

/// A `PANE_INFO` line for a live pane, tagged `tag` (empty: none).
fn pane(n: u32, tag: &str, cwd: &str) -> String {
    format!("I\t%{n}\t{}\t0\t\t\t{tag}\t{cwd}", pid(n))
}

fn dead(n: u32, tag: &str, cwd: &str) -> String {
    format!("I\t%{n}\t{}\t1\t\t\t{tag}\t{cwd}", pid(n))
}

fn adopted(n: u32, tag: &str, cwd: &str) -> String {
    format!("I\t%{n}\t{}\t0\t\t1\t{tag}\t{cwd}", pid(n))
}

/// A `PANE_START` line.
fn start(n: u32, cmd: &str) -> String {
    format!("S\t%{n}\t{cmd}")
}

/// A, B and C's panes, alive and tagged.
fn abc(b: &TypicalBrain) -> Vec<String> {
    vec![
        pane(1, &tag(A), &cwd(b, A)),
        pane(2, &tag(B), &cwd(b, B)),
        pane(3, &tag(C), &cwd(b, C)),
    ]
}

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

fn logs(fx: &[Effect]) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(LogLine { message, .. }) => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn has_log(fx: &[Effect], line: &str) -> bool {
    logs(fx).contains(&line)
}

fn set_tag(pane: u32, n: u8) -> Cmd {
    Cmd::new("set-option").args([
        "-p".to_string(),
        "-t".to_string(),
        format!("%{pane}"),
        "@moa_session".to_string(),
        tag(n),
    ])
}

/// Where the untokened command `cmd` is sent among `fx`, and where the first `Persist` is.
fn after_persist(fx: &[Effect], cmd: &Cmd) -> bool {
    let persist = fx.iter().position(|e| matches!(e, Effect::Persist(_)));
    let sent = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { token: None, cmds, .. } if cmds.contains(cmd)));
    matches!((persist, sent), (Some(p), Some(s)) if p < s)
}

/// Where the untokened command `cmd` is sent among `fx`: its effect, then its place in it.
fn sent_at(fx: &[Effect], cmd: &Cmd) -> Option<(usize, usize)> {
    fx.iter().enumerate().find_map(|(i, e)| match e {
        Effect::Tmux {
            token: None, cmds, ..
        } => cmds.iter().position(|c| c == cmd).map(|j| (i, j)),
        _ => None,
    })
}

#[test]
fn a_new_generation_puts_live_sessions_to_sleep() {
    let mut r = World::new().drained(no_agent);
    r.connect(G2);
    let fx = r.listed(&[], &[]);
    for n in [A, B, C] {
        assert!(r.is_asleep(n), "{:?}", r.session(n));
    }
    assert!(
        !changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Activity(_))),
        "{fx:?}"
    );
    assert!(
        has_log(&fx, "reconciled the tabs: 0 kept, 0 bound, 3 put to sleep"),
        "{fx:?}"
    );
    assert!(r.st.tmux().reconciled());
}

#[test]
fn same_generation_keeps_live_panes_and_sleeps_vanished_ones() {
    let mut r = World::new().drained(no_agent);
    r.connect(G1);
    let b = &r.b;
    let lines = [pane(1, &tag(A), &cwd(b, A)), dead(2, &tag(B), &cwd(b, B))];
    let fx = r.listed(&lines, &[]);
    assert!(r.is_live(A));
    assert_eq!(r.bound_to(A), Some(&binding(A, G1, 1, true)));
    assert!(r.is_asleep(B));
    assert!(r.is_asleep(C));
    assert!(
        has_log(&fx, "reconciled the tabs: 1 kept, 0 bound, 2 put to sleep"),
        "{fx:?}"
    );
}

#[test]
fn a_reused_pane_id_with_another_tag_is_not_kept() {
    let mut r = World::new().drained(no_agent);
    r.connect(G1);
    let lines = [pane(1, &tag(C), &cwd(&r.b, C))];
    let fx = r.listed(&lines, &[]);
    assert!(r.is_asleep(A));
    assert!(r.is_asleep(B));
    assert!(r.is_live(C));
    let bound = r.bound_to(C).unwrap();
    assert_eq!(bound.pane_id, "%1");
    assert!(bound.tracked);
    // The pane already carries C's tag.
    let tags = |e: &Effect| matches!(e, Effect::Tmux { cmds, .. } if cmds.iter().any(|c| c.argv().iter().any(|a| a == "@moa_session")));
    assert!(!fx.iter().any(tags), "{fx:?}");
    assert!(
        has_log(&fx, "reconciled the tabs: 0 kept, 1 bound, 2 put to sleep"),
        "{fx:?}"
    );
}

#[test]
fn a_junk_tag_is_another_sessions() {
    let mut r = World::new().drained(no_agent);
    r.connect(G1);
    let b = &r.b;
    let lines = [
        pane(1, "junk", &cwd(b, A)),
        pane(2, &tag(B), &cwd(b, B)),
        pane(3, &tag(C), &cwd(b, C)),
    ];
    r.listed(&lines, &[]);
    assert!(r.is_asleep(A));
    assert!(r.bound_to(B).is_some());
    assert!(r.bound_to(C).is_some());
}

/// D's agent: its conversation, and `MOA_SESSION` naming D.
fn d_with_moa_session(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 909).then(|| claude(Some(conv(D)), Some(tag(D))))
}

/// D's agent, started outside moa: no `MOA_SESSION`.
fn d_without_moa_session(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 909).then(|| claude(Some(conv(D)), None))
}

/// D ended by a lost `open` (no binding, not asleep), its agent running untagged in %9.
fn with_d_in_9(inspect: fn(i64, &[String]) -> Option<PaneAgent>) -> (Rig, Vec<Effect>) {
    let mut w = World::new();
    w.add(D).lifecycle = Lifecycle::Ended;
    let mut r = w.drained(inspect);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(pane(9, "", &cwd(&r.b, D)));
    let fx = r.listed(&lines, &[]);
    (r, fx)
}

#[test]
fn an_untagged_pane_running_a_known_conversation_is_bound() {
    let (r, fx) = with_d_in_9(d_with_moa_session);
    assert!(r.is_live(D));
    assert_eq!(r.session(D).location, Location::Here);
    assert_eq!(
        r.bound_to(D),
        Some(&PaneBinding {
            bound_at: r.b.now().wall,
            ..binding(D, G1, 9, true)
        })
    );
    assert!(after_persist(&fx, &set_tag(9, D)), "{fx:?}");
    assert!(
        has_log(&fx, "reconciled the tabs: 3 kept, 1 bound, 0 put to sleep"),
        "{fx:?}"
    );
}

#[test]
fn an_agent_without_moa_session_rebinds_untracked() {
    let (r, fx) = with_d_in_9(d_without_moa_session);
    assert!(r.is_live(D));
    assert!(!r.bound_to(D).unwrap().tracked);
    assert!(after_persist(&fx, &set_tag(9, D)), "{fx:?}");
}

/// A Claude whose argv was retitled to plain `claude`: only its start command names the
/// conversation.
fn retitled(pid: i64, start: &[String]) -> Option<PaneAgent> {
    if pid != 909 {
        return None;
    }
    let conversation = start
        .windows(2)
        .find(|w| w[0] == "--resume")
        .map(|w| w[1].clone());
    Some(claude(conversation, None))
}

#[test]
fn a_retitled_agent_is_found_by_its_start_command() {
    let mut w = World::new();
    w.add(D).lifecycle = Lifecycle::Ended;
    let mut r = w.drained(retitled);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(pane(9, "", &cwd(&r.b, D)));
    let starts = [start(9, &format!("/opt/claude --resume {}", conv(D)))];
    r.listed(&lines, &starts);
    assert!(r.is_live(D));
    assert_eq!(r.bound_to(D).unwrap().pane_id, "%9");
}

#[test]
fn an_adopted_agent_that_exits_in_its_shell_tab_shows_asleep() {
    let mut w = World::new();
    w.add(U);
    w.snap.pane_bindings.push(binding(U, G1, 4, false));
    let mut r = w.drained(no_agent);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(adopted(4, &tag(U), &cwd(&r.b, U)));
    let fx = r.listed(&lines, &[]);
    assert!(r.is_asleep(U));
    assert!(
        has_log(&fx, "reconciled the tabs: 3 kept, 0 bound, 1 put to sleep"),
        "{fx:?}"
    );
    // The shell tab is the user's again, without moa's tags.
    assert_eq!(
        unsets(&fx),
        untagged("%4").iter().collect::<Vec<_>>(),
        "{fx:?}"
    );
}

/// An agent with no `CLAUDE_CONFIG_DIR` and no conversation on its command line.
fn default_store_agent(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 404).then(|| claude(None, None))
}

#[test]
fn a_default_folder_adopted_agent_still_runs() {
    let mut w = World::new();
    w.add(U);
    let store = w.b.home.join(".claude").display().to_string();
    w.conversation(U).store_dir = Some(store);
    w.snap.pane_bindings.push(binding(U, G1, 4, false));
    let mut r = w.drained(default_store_agent);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(adopted(4, &tag(U), &cwd(&r.b, U)));
    let fx = r.listed(&lines, &[]);
    assert!(r.is_live(U));
    assert_eq!(r.bound_to(U), Some(&binding(U, G1, 4, false)));
    assert!(
        has_log(&fx, "reconciled the tabs: 4 kept, 0 bound, 0 put to sleep"),
        "{fx:?}"
    );
}

/// E's spike pane runs E's conversation, F's another one, and H's an agent naming none.
fn spike_agents(pid: i64, _: &[String]) -> Option<PaneAgent> {
    match pid {
        404 => Some(claude(Some(conv(E)), None)),
        505 => Some(claude(Some("not-a-known-conversation".into()), None)),
        707 => Some(claude(None, None)),
        _ => None,
    }
}

#[test]
fn spike_panes_are_verified_or_cleared() {
    let mut w = World::new();
    for n in [E, F, G, H] {
        w.add(n);
    }
    w.snap.spike_panes = vec![
        spike(E, 4, Some("/tmp/tmux-501/moa")),
        spike(F, 5, Some("/tmp/tmux-501/moa")),
        spike(G, 6, Some("/tmp/tmux-501/moa-other")),
        spike(H, 7, None),
    ];
    let mut r = w.drained(spike_agents);
    r.connect(G1);
    let b = &r.b;
    let mut lines = abc(b);
    lines.extend([
        pane(4, "", &cwd(b, E)),
        pane(5, "", &cwd(b, F)),
        pane(7, "", &format!("{}/sub", cwd(b, H))),
    ]);
    let fx = r.listed(&lines, &[]);

    let e = r.bound_to(E).unwrap();
    assert_eq!((e.pane_id.as_str(), e.tracked), ("%4", true));
    assert!(r.is_live(E));
    assert!(r.is_asleep(F));
    assert!(r.is_live(G));
    assert!(r.bound_to(G).is_none());
    let h = r.bound_to(H).unwrap();
    assert_eq!((h.pane_id.as_str(), h.tracked), ("%7", true));
    assert!(r.is_live(H));
    let left: Vec<SessionId> = r.st.spike_panes().map(|p| p.session_id).collect();
    assert_eq!(left, [id(G)]);
    assert!(
        has_log(&fx, "reconciled the tabs: 3 kept, 2 bound, 1 put to sleep"),
        "{fx:?}"
    );
}

#[test]
fn elsewhere_and_waiting_sessions_are_left_alone() {
    let mut w = World::new();
    w.add(X).location = Location::Elsewhere;
    let mut r = w.drained(no_agent);
    r.connect(G1);
    let path = r.b.home.join("work/lantern");
    let fx = send(
        &r.b,
        &mut r.st,
        1,
        Op::SessionOpen {
            path,
            account: None,
            title: None,
            group: None,
            prompt: None,
            bg: false,
        },
    );
    let (waiting, _) = the_open(&fx);
    let lines = abc(&r.b);
    let fx = r.listed(&lines, &[]);
    assert!(r.is_live(X));
    let s = r.st.session(waiting).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert!(
        has_log(&fx, "reconciled the tabs: 3 kept, 0 bound, 0 put to sleep"),
        "{fx:?}"
    );
}

#[test]
fn reconcile_waits_for_the_drain() {
    let mut r = World::new().load(no_agent);
    let fx = r.connect(G1);
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.st.tmux().queries().count(), 0);
    let fx = r.input(Input::StartDrained);
    assert_eq!(queries(&fx), [list_panes().as_slice()]);
    assert_eq!(r.st.tmux().queries().count(), 1);
}

#[test]
fn a_refused_reconcile_is_asked_again() {
    let mut r = World::new().drained(no_agent);
    r.connect(G1);
    let until = r.later(60_000).wall;
    r.input(Input::Reload {
        snapshot: Box::new(r.snap.clone()),
        refuse_writes: Some(RefuseWrites {
            until,
            reason: "the disk is full".into(),
        }),
    });
    let fx = r.listed(&[], &[]);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    for n in [A, B, C] {
        assert!(r.is_live(n));
        assert!(r.bound_to(n).is_some());
    }
    assert_eq!(ready(&r.st), Err(OpError(NOT_READY.into())));

    // Not while writes are refused.
    let soon = r.later(1_000);
    let fx = handle_checked(&mut r.st, Input::Timer(TimerKey::Clock), soon);
    assert!(queries(&fx).is_empty(), "{fx:?}");
    let then = r.later(60_000);
    let fx = handle_checked(&mut r.st, Input::Timer(TimerKey::Clock), then);
    assert_eq!(queries(&fx), [list_panes().as_slice()]);
}

/// A process read that always panics (a malformed process entry, say).
fn panicking_agent(_: i64, _: &[String]) -> Option<PaneAgent> {
    panic!("a process read that always panics")
}

#[test]
fn a_reconcile_that_always_panics_is_tried_a_bounded_number_of_times() {
    let mut r = World::new().drained(panicking_agent);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(pane(9, "", &cwd(&r.b, D)));
    let reload = |r: &mut Rig| {
        r.input(Input::Reload {
            snapshot: Box::new(r.snap.clone()),
            refuse_writes: None,
        })
    };
    for attempt in 1..=3 {
        let run = catch_unwind(AssertUnwindSafe(|| r.listed(&lines, &[])));
        assert!(run.is_err(), "attempt {attempt} didn't panic");
        // As the daemon does after a panic: the core reloads.
        let fx = reload(&mut r);
        if attempt < 3 {
            assert_eq!(queries(&fx), [list_panes().as_slice()], "{fx:?}");
        } else {
            assert!(queries(&fx).is_empty(), "{fx:?}");
            let warned =
                "reconciling the tabs failed 3 times; moa tries again at the next connection";
            assert!(has_log(&fx, warned), "{fx:?}");
        }
    }
    // Said once; the ops on tabs keep waiting.
    let fx = r.input(Input::Timer(TimerKey::Clock));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert!(
        logs(&fx).iter().all(|l| !l.starts_with("reconciling")),
        "{fx:?}"
    );
    assert_eq!(ready(&r.st), Err(OpError(NOT_READY.into())));

    // The next connection tries again, from a fresh count.
    r.input(Input::Tmux(TmuxIn::Down(Down::Lost)));
    let fx = r.connect_on(2, G1);
    assert_eq!(queries(&fx), [list_panes().as_slice()], "{fx:?}");
}

thread_local! {
    /// Whether D's agent still runs in its shell tab, %9.
    static D_RUNS: Cell<bool> = const { Cell::new(true) };
}

/// D's agent, started by hand in a shell tab: no `MOA_SESSION`, while it runs.
fn d_in_a_shell(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 909 && D_RUNS.with(Cell::get)).then(|| claude(Some(conv(D)), None))
}

#[test]
fn an_untracked_rebind_is_marked_adopted_and_checked_again() {
    let mut w = World::new();
    w.add(D).lifecycle = Lifecycle::Ended;
    let mut r = w.drained(d_in_a_shell);
    r.connect(G1);
    let mut lines = abc(&r.b);
    lines.push(pane(9, "", &cwd(&r.b, D)));
    let fx = r.listed(&lines, &[]);
    assert!(!r.bound_to(D).unwrap().tracked);
    let adopt = Cmd::new("set-option").args(["-p", "-t", "%9", "@moa_adopted", "1"]);
    assert!(after_persist(&fx, &set_tag(9, D)), "{fx:?}");
    assert!(after_persist(&fx, &adopt), "{fx:?}");
    // Marked adopted before it is tagged: a link lost between the two never leaves a tab
    // that looks tracked.
    assert!(
        sent_at(&fx, &adopt) < sent_at(&fx, &set_tag(9, D)),
        "{fx:?}"
    );

    // The agent leaves; its shell tab stays, tagged as moa left it. Two later connections to
    // the same server: the first puts D to sleep, and the second doesn't bind the shell.
    D_RUNS.with(|c| c.set(false));
    let mut lines = abc(&r.b);
    lines.push(adopted(9, &tag(D), &cwd(&r.b, D)));
    for conn in [2, 3] {
        r.input(Input::Tmux(TmuxIn::Down(Down::Lost)));
        r.connect_on(conn, G1);
        r.listed(&lines, &[]);
        assert!(r.is_asleep(D), "connection {conn}: {:?}", r.session(D));
        assert!(r.bound_to(D).is_none(), "connection {conn}");
    }
}

#[test]
fn a_failed_listing_is_asked_again() {
    let mut r = World::new().drained(no_agent);
    r.connect(G1);
    let n = r.st.tmux().queries().next().unwrap();
    let fx = r.input(Input::Tmux(TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results: vec![Err("no server".into()), Ok(Vec::new())],
    }));
    assert!(has_log(&fx, "listing the tabs failed"), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(
        queries(&fx).is_empty(),
        "asked again by the same input: {fx:?}"
    );
    assert!(!r.st.tmux().reconciled());
    let fx = r.input(Input::Timer(TimerKey::Clock));
    assert_eq!(queries(&fx), [list_panes().as_slice()]);
}

#[test]
fn no_server_puts_live_sessions_to_sleep() {
    // P's spike pane is on this server, Q's on another one.
    const P: u8 = 11;
    const Q: u8 = 12;
    let mut w = World::new();
    w.add(P);
    w.add(Q);
    w.add(X).location = Location::Elsewhere;
    w.snap.spike_panes = vec![
        spike(P, 4, Some("/tmp/tmux-501/moa")),
        spike(Q, 5, Some("/tmp/tmux-501/moa-other")),
    ];
    let mut r = w.drained(no_agent);
    r.connect(G1);
    let fx = r.input(Input::Tmux(TmuxIn::Down(Down::NoServer)));
    for n in [A, B, C, P] {
        assert!(r.is_asleep(n), "{:?}", r.session(n));
    }
    assert!(r.is_live(Q));
    assert!(r.is_live(X));
    let left: Vec<SessionId> = r.st.spike_panes().map(|p| p.session_id).collect();
    assert_eq!(left, [id(Q)]);
    assert!(!r.st.tmux().reconciled());
    assert!(
        !changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Activity(_))),
        "{fx:?}"
    );
}

#[test]
fn no_server_before_the_drain_sleeps_sessions_once_drained() {
    let mut r = World::new().load(no_agent);
    let fx = r.input(Input::Tmux(TmuxIn::Down(Down::NoServer)));
    assert!(changes(&fx).is_empty(), "{fx:?}");
    for n in [A, B, C] {
        assert!(r.is_live(n));
    }
    r.input(Input::StartDrained);
    for n in [A, B, C] {
        assert!(r.is_asleep(n), "{:?}", r.session(n));
    }
}

/// A bare brain for the proptest: no typical fixture, so each case is cheap.
fn bare(sessions: Vec<Session>, bindings: Vec<PaneBinding>, now: &Now) -> State {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    let snap = Snapshot {
        sessions,
        pane_bindings: bindings,
        ..Snapshot::default()
    };
    handle_checked(&mut st, Input::Startup(Box::new(snap)), now.clone());
    st
}

proptest! {
    /// Random bindings on two servers, each session live or not; random listings, each pane
    /// alive, dead or missing, adopted or not, untagged, tagged with a session or with junk.
    #[test]
    fn reconcile_never_keeps_a_stale_binding(
        seeds in prop::collection::vec(
            (any::<bool>(), prop::option::of((any::<bool>(), 1u32..7, any::<bool>(), 0i64..3))),
            1..7,
        ),
        listing in prop::collection::vec(prop::option::of((any::<bool>(), any::<bool>(), 0u8..8)), 6),
    ) {
        let now = Now::at(UtcMs(1_800_000_000_000), Duration::from_secs(1), jiff::tz::TimeZone::UTC);
        let mut sessions = Vec::new();
        let mut bindings = Vec::new();
        for (i, (live, bound)) in seeds.iter().enumerate() {
            let n = u8::try_from(i + 1).unwrap();
            let mut s = session_in(n, format!("/w/s{n}"), UtcMs(1));
            if !live {
                s.lifecycle = Lifecycle::Ended;
            }
            sessions.push(s);
            if let Some((current, pane, tracked, at)) = bound {
                let generation = if *current { G2 } else { G1 };
                bindings.push(PaneBinding {
                    bound_at: UtcMs(*at),
                    ..binding(n, generation, *pane, *tracked)
                });
            }
        }
        let mut info = Vec::new();
        for (i, p) in listing.iter().enumerate() {
            let Some((is_dead, is_adopted, t)) = p else { continue };
            let n = u32::try_from(i + 1).unwrap();
            let t = match t {
                0 => String::new(),
                7 => "junk".into(),
                k => tag(*k),
            };
            info.push(format!(
                "I\t%{n}\t{}\t{}\t\t{}\t{t}\t/w",
                pid(n),
                u8::from(*is_dead),
                if *is_adopted { "1" } else { "" },
            ));
        }
        let mut st = bare(sessions, bindings, &now);
        handle_checked(&mut st, Input::StartDrained, now.clone());
        handle_checked(&mut st, Input::Tmux(TmuxIn::Connected {
            conn: 1,
            generation: G2,
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        }), now.clone());
        let q = st.tmux().queries().next().unwrap();
        handle_checked(&mut st, Input::Tmux(TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(q),
            results: vec![Ok(info), Ok(Vec::new())],
        }), now.clone());
        prop_assert!(st.tmux().reconciled());
        let up = matches!(st.tmux().link, Link::Up { .. });
        prop_assert!(up);

        let mut panes = std::collections::BTreeSet::new();
        let ids: Vec<SessionId> = st.sessions().map(|s| s.id).collect();
        for sid in ids {
            let Some(bd) = st.binding(sid) else { continue };
            prop_assert_eq!((bd.server_pid, bd.server_start), (G2.pid, G2.start));
            let p: PaneId = bd.pane_id.parse().unwrap();
            prop_assert!(panes.insert(p), "two bindings on {}", p);
            let listed = listing.get(p.0 as usize - 1).copied().flatten();
            let Some((is_dead, _, t)) = listed else {
                return Err(TestCaseError::fail(format!("{p} isn't listed")));
            };
            prop_assert!(!is_dead);
            prop_assert!(t == 0 || (1..=6).contains(&t) && id(t) == sid, "{} tagged {}", p, t);
        }
    }
}
