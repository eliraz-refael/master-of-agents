//! `ls` and the tabs (R23): once the tabs are reconciled, `ls` reads one listing of them. It
//! lists the Claude agents running in tabs moa doesn't track, with the line that adopts each,
//! marks adopted sessions, and puts to sleep an adopted session whose agent left its tab.
//! The typical brain, its link played by hand; each tab's agent comes from a test function.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, LogLevel, LogLine, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Inspected, Lifecycle, Location,
    PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError};
use moa_core::state::{Deps, LoadNote, Snapshot, State};
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

use common::*;

/// Brain `default` owns `~/private`, the `work` brain `~/office`.
const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
open_account = "acct-a"

[brains.work]
folders = ["~/office"]

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// K: live in `~/private/web`, moa's own, in tab %7 (pid 70).
const K: &str = "00000000-0000-4000-8000-0000000000c7";
const CK: &str = "c7000000-0000-4000-8000-0000000000c7";
/// A: live in `~/private/lantern`, adopted, in tab %5 (pid 50).
const A: &str = "00000000-0000-4000-8000-0000000000c5";
const CA: &str = "c5000000-0000-4000-8000-0000000000c5";
/// Z: asleep, with no tab.
const Z: &str = "00000000-0000-4000-8000-0000000000c8";
const CZ: &str = "c8000000-0000-4000-8000-0000000000c8";
/// A session id the brain doesn't have.
const Y: &str = "00000000-0000-4000-8000-0000000000c9";

fn id(s: &str) -> SessionId {
    s.parse().unwrap()
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

/// The agent `pid` runs, from now on; `None` when it runs none.
fn runs(pid: i64, agent: Option<PaneAgent>) {
    AGENTS.with(|a| match agent {
        Some(agent) => a.borrow_mut().insert(pid, Inspected::Agent(agent)),
        None => a.borrow_mut().remove(&pid),
    });
}

/// `pid`'s process can't be read, from now on.
fn unreadable(pid: i64) {
    AGENTS.with(|a| a.borrow_mut().insert(pid, Inspected::Unknown));
}

/// A Claude on `store`, running `conv`.
fn claude(store: &Path, conv: Option<&str>) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::Claude,
        store_dir: Some(shown(store)),
        conversation: conv.map(str::to_string),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: Some(UtcMs(1)),
    }
}

fn session(b: &TypicalBrain, sid: &str, folder: &str, lifecycle: Lifecycle) -> Session {
    let cwd = shown(&b.home.join(folder));
    let at = UtcMs(b.now().wall.0 - 3_600_000);
    Session {
        id: id(sid),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.clone()),
        start_cwd: Some(cwd),
        project_id: None,
        title: Some(folder.rsplit('/').next().unwrap().into()),
        title_locked: false,
        group_path: None,
        lifecycle,
        activity: Activity::Idle,
        activity_at: Some(at),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(at),
        asleep_at: (lifecycle == Lifecycle::Ended).then_some(at),
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(at),
    }
}

fn conversation(b: &TypicalBrain, sid: &str, conv: &str) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conv.into(),
        session_id: id(sid),
        store_dir: Some(shown(&b.home.join(".claude-a"))),
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

fn binding(sid: &str, pane: &str, pid: i64, tracked: bool) -> PaneBinding {
    PaneBinding {
        session_id: id(sid),
        server_pid: generation().pid,
        server_start: generation().start,
        socket: "moa".into(),
        pane_id: pane.into(),
        pane_pid: pid,
        bound_at: UtcMs(2),
        tracked,
    }
}

/// A `PANE_INFO` line: a live tab with this tag, marked adopted or a terminal, in `cwd`.
fn tab(pane: &str, pid: i64, tag: &str, adopted: bool, term: bool, cwd: &Path) -> String {
    let term = if term { "1" } else { "" };
    let adopted = if adopted { "1" } else { "" };
    format!(
        "I\t{pane}\t{pid}\t0\t{term}\t{adopted}\t{tag}\t{}",
        cwd.display()
    )
}

/// The typical brain plus K (live in %7), A (live, adopted, in %5) and Z (asleep): loaded,
/// linked, drained and reconciled, with A's agent running in %5.
struct Rig {
    b: TypicalBrain,
    st: State,
    /// The tabs every listing shows.
    tabs: Vec<String>,
    /// What the rig was loaded from, for a reload.
    snap: Snapshot,
}

impl Rig {
    fn new() -> Rig {
        Rig::build(false)
    }

    /// With `a_unknown`, A's row holds a value this build doesn't know: no commit may
    /// change it.
    fn build(a_unknown: bool) -> Rig {
        AGENTS.with(|a| a.borrow_mut().clear());
        let b = TypicalBrain::new();
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            inspect,
            ..Deps::default()
        };
        let mut snap = b.snapshot();
        snap.sessions.extend([
            session(&b, K, "private/web", Lifecycle::Live),
            session(&b, A, "private/lantern", Lifecycle::Live),
            session(&b, Z, "private/boards", Lifecycle::Ended),
        ]);
        snap.conversations.extend([
            conversation(&b, K, CK),
            conversation(&b, A, CA),
            conversation(&b, Z, CZ),
        ]);
        snap.pane_bindings
            .extend([binding(K, "%7", 70, true), binding(A, "%5", 50, false)]);
        if a_unknown {
            snap.unknown.push(LoadNote {
                table: "sessions",
                key: A.into(),
                reason: "a state this build doesn't know",
            });
        }
        let tabs = vec![
            tab("%5", 50, A, true, false, &b.home.join("private/lantern")),
            tab("%7", 70, K, false, false, &b.home.join("private/web")),
        ];
        // A's agent still runs, so reconcile keeps its binding.
        runs(50, Some(claude(&b.home.join(".claude-a"), Some(CA))));
        let mut st = loaded_with(&b, TOML, deps, snap.clone());
        link_up(&b, &mut st);
        handle_checked(&mut st, Input::StartDrained, b.now());
        let lines: Vec<&str> = tabs.iter().map(String::as_str).collect();
        listed(&mut st, &b, &lines, &[]);
        assert!(st.tmux().reconciled());
        assert!(st.binding(id(A)).is_some_and(|b| !b.tracked));
        Rig { b, st, tabs, snap }
    }

    /// tmux answers pane query `n` with [`Rig::tabs`].
    fn answer(&mut self, n: u64) -> Vec<Effect> {
        let tabs = self.tabs.clone();
        feed(
            &self.b,
            &mut self.st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Query(n),
                results: vec![Ok(tabs), Ok(Vec::new())],
            },
        )
    }

    /// Where tab `pane` is in [`Rig::tabs`].
    fn at(&self, pane: &str) -> usize {
        let prefix = format!("I\t{pane}\t");
        self.tabs
            .iter()
            .position(|t| t.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no tab {pane}"))
    }

    fn home(&self) -> PathBuf {
        self.b.home.clone()
    }

    /// `ls` sent: it only asks for the tabs.
    fn ask_ls(&mut self) {
        let fx = send(&self.b, &mut self.st, 1, Op::SessionLs { all: false });
        assert!(replies(&fx).is_empty(), "{fx:?}");
        assert!(
            fx.iter().any(|e| matches!(
                e,
                Effect::Tmux {
                    token: Some(TmuxToken::Query(_)),
                    ..
                }
            )),
            "{fx:?}"
        );
    }

    /// `ls`, answered with [`Rig::tabs`].
    fn ls(&mut self) -> Vec<Effect> {
        self.ask_ls();
        let lines: Vec<&str> = self.tabs.iter().map(String::as_str).collect();
        listed(&mut self.st, &self.b, &lines, &[])
    }
}

fn one_reply(fx: &[Effect]) -> Result<Done, OpError> {
    let r = replies(fx);
    assert_eq!(r.len(), 1, "{fx:?}");
    r[0].1.clone()
}

/// The JSON row of session `sid`.
fn row(done: &Done, sid: &str) -> Value {
    done.data
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(sid))
        .unwrap_or_else(|| panic!("no row for {sid}: {}", done.data))
        .clone()
}

/// The text line of session `sid`.
fn line_of(st: &State, done: &Done, sid: &str) -> String {
    let handle = st.handle_of(id(sid));
    done.text
        .lines()
        .find(|l| l.starts_with(&handle))
        .unwrap_or_else(|| panic!("no line for {sid}: {}", done.text))
        .to_string()
}

fn untracked_rows(done: &Done) -> Vec<Value> {
    done.data
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r.get("untracked").is_some())
        .cloned()
        .collect()
}

#[test]
fn ls_lists_untracked_tabs() {
    let mut r = Rig::new();
    let home = r.home();
    let store_a = home.join(".claude-a");
    let x = home.join("private/x");
    // %6: a Claude typed in a shell in `~/private/x`.
    r.tabs.push(tab("%6", 60, "", false, false, &x));
    runs(60, Some(claude(&store_a, Some("C6"))));
    // %13: one on a store no account lists, shown by its folder's name.
    let q = home.join("private/q");
    r.tabs.push(tab("%13", 130, "", false, false, &q));
    runs(130, Some(claude(&home.join(".claude-q"), None)));
    // %9: one typed in a terminal moa opened, the natural place to start one by hand.
    r.tabs.push(tab("%9", 90, "", false, true, &x));
    runs(90, Some(claude(&store_a, Some("C9"))));
    // %8: one carrying a `MOA_SESSION` this brain doesn't know (an `open` whose session was
    // lost), and %16 one moa started for Z, which has no tab: adopt takes either.
    r.tabs.push(tab("%8", 80, "", false, false, &x));
    runs(
        80,
        Some(PaneAgent {
            moa_session: Some(Y.into()),
            ..claude(&store_a, Some("C8"))
        }),
    );
    r.tabs.push(tab("%16", 160, "", false, false, &x));
    runs(
        160,
        Some(PaneAgent {
            moa_session: Some(Z.into()),
            ..claude(&store_a, Some(CZ))
        }),
    );
    // Not listed: a tab moa started for K, which runs in its own tab, a terminal running
    // only its shell, a tab a binding holds though it isn't tagged, a tagged tab, one in
    // another brain's folder, and a shell.
    r.tabs.push(tab("%15", 150, "", false, false, &x));
    runs(
        150,
        Some(PaneAgent {
            moa_session: Some(K.into()),
            ..claude(&store_a, Some("C15"))
        }),
    );
    r.tabs.push(tab("%14", 140, "", false, true, &x));
    let seven = r.at("%7");
    r.tabs[seven] = tab("%7", 70, "", false, false, &home.join("private/web"));
    runs(70, Some(claude(&store_a, Some("C7"))));
    r.tabs.push(tab("%11", 110, Y, false, false, &x));
    runs(110, Some(claude(&store_a, Some("C11"))));
    r.tabs
        .push(tab("%10", 100, "", false, false, &home.join("office/y")));
    runs(100, Some(claude(&store_a, Some("C10"))));
    r.tabs.push(tab("%12", 120, "", false, false, &x));

    let fx = r.ls();
    let done = one_reply(&fx).unwrap();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let shown_x = shown(&x);
    let shown_q = shown(&q);
    assert!(
        done.text.ends_with(&format!(
            "UNTRACKED tab %6: acct-a in {shown_x}  → moactl adopt %6\n\
             UNTRACKED tab %13: .claude-q in {shown_q}  → moactl adopt %13\n\
             UNTRACKED tab %9: acct-a in {shown_x}  → moactl adopt %9\n\
             UNTRACKED tab %8: acct-a in {shown_x}  → moactl adopt %8\n\
             UNTRACKED tab %16: acct-a in {shown_x}  → moactl adopt %16"
        )),
        "{}",
        done.text
    );
    assert_eq!(
        untracked_rows(&done),
        vec![
            json!({"untracked": "%6", "account": "acct-a", "cwd": shown_x}),
            json!({"untracked": "%13", "account": ".claude-q", "cwd": shown_q}),
            json!({"untracked": "%9", "account": "acct-a", "cwd": shown_x}),
            json!({"untracked": "%8", "account": "acct-a", "cwd": shown_x}),
            json!({"untracked": "%16", "account": "acct-a", "cwd": shown_x}),
        ]
    );
    // They go after the sessions.
    let data = done.data.as_array().unwrap();
    let first = data
        .iter()
        .position(|r| r.get("untracked").is_some())
        .unwrap();
    assert!(data[first..].iter().all(|r| r.get("untracked").is_some()));
    assert!(first > 0);
}

#[test]
fn adopted_rows_say_so() {
    let mut r = Rig::new();
    let fx = r.ls();
    let done = one_reply(&fx).unwrap();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(row(&done, A)["tracked"], json!(false));
    assert_eq!(row(&done, A)["alive"], json!("idle"));
    assert_eq!(row(&done, K)["tracked"], json!(true));
    assert_eq!(row(&done, Z)["tracked"], json!(true));
    let handle = r.st.handle_of(id(A));
    assert!(
        line_of(&r.st, &done, A)
            .ends_with(&format!("  adopted: moactl restart {handle} tracks it")),
        "{}",
        done.text
    );
    assert!(!line_of(&r.st, &done, K).contains("adopted:"));
    assert!(!line_of(&r.st, &done, Z).contains("adopted:"));
    assert!(untracked_rows(&done).is_empty());
}

/// U14: an adopted agent that exits leaves its shell tab open. The listing `ls` reads shows
/// the tab, but its process runs no agent any more: the session goes to sleep, saved before
/// the reply, which shows it asleep.
#[test]
fn an_adopted_agent_that_left_its_tab_shows_asleep_in_ls() {
    let mut r = Rig::new();
    runs(50, None);
    let fx = r.ls();
    let a = r.st.session(id(A)).unwrap();
    assert_eq!(a.lifecycle, Lifecycle::Ended);
    assert_eq!(a.asleep_at, Some(r.b.now().wall));
    assert!(r.st.binding(id(A)).is_none());
    let saved = changes(&fx);
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(s) if *s == id(A)))
    );
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::Session(s) if s.id == id(A) && s.asleep_at.is_some()))
    );
    // K's tab is tracked: it isn't looked at.
    assert!(r.st.binding(id(K)).is_some());
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let reply = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .unwrap();
    assert!(persist < reply, "{fx:?}");
    let done = one_reply(&fx).unwrap();
    assert_eq!(row(&done, A)["alive"], json!("asleep"));
    assert_eq!(row(&done, A)["tracked"], json!(true));
    assert!(!line_of(&r.st, &done, A).contains("adopted:"));
    // The listing still shows the tab tagged with A: it isn't listed as untracked yet.
    assert!(untracked_rows(&done).is_empty());

    // The tab is the user's again: moa's tags come off it once the sleep is saved (R15).
    let want = untagged("%5");
    assert_eq!(unsets(&fx), want.iter().collect::<Vec<_>>(), "{fx:?}");
    let untag = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { cmds, .. } if cmds.contains(&want[0])))
        .unwrap();
    assert!(persist < untag, "{fx:?}");
    assert_eq!(
        r.st.tmux().topology.moa_session("%5".parse().unwrap()),
        None
    );

    // Another Claude typed in that shell shows in the next `ls`, as one moa doesn't track.
    let lantern = r.home().join("private/lantern");
    let five = r.at("%5");
    r.tabs[five] = tab("%5", 50, "", false, false, &lantern);
    runs(
        50,
        Some(claude(&r.home().join(".claude-a"), Some("C-next"))),
    );
    let fx = r.ls();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let done = one_reply(&fx).unwrap();
    assert_eq!(
        untracked_rows(&done),
        vec![json!({"untracked": "%5", "account": "acct-a", "cwd": shown(&lantern)})]
    );
}

/// A read of the adopted tab that fails proves nothing: `ls` leaves A live and bound, its
/// tab tagged, and the next `ls` decides.
#[test]
fn an_adopted_tab_moa_cant_read_is_left_as_it_is_by_ls() {
    let mut r = Rig::new();
    unreadable(50);
    let fx = r.ls();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(unsets(&fx).is_empty(), "{fx:?}");
    let a = r.st.session(id(A)).unwrap();
    assert_eq!(a.lifecycle, Lifecycle::Live);
    assert_eq!(a.asleep_at, None);
    assert!(r.st.binding(id(A)).is_some_and(|b| !b.tracked));
    let done = one_reply(&fx).unwrap();
    assert_eq!(row(&done, A)["tracked"], json!(false));
    assert!(untracked_rows(&done).is_empty());
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::Log(LogLine { level: LogLevel::Debug, message })
                if message.contains("couldn't be read")
        )),
        "{fx:?}"
    );

    // Once it reads as gone, the next `ls` puts A to sleep.
    runs(50, None);
    let fx = r.ls();
    assert!(!changes(&fx).is_empty(), "{fx:?}");
    assert!(r.st.binding(id(A)).is_none());
}

/// A tab no binding holds whose process can't be read isn't listed: adopt needs an agent.
#[test]
fn a_tab_moa_cant_read_isnt_listed_as_untracked() {
    let mut r = Rig::new();
    r.tabs
        .push(tab("%6", 60, "", false, false, &r.home().join("private/x")));
    unreadable(60);
    let fx = r.ls();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(untracked_rows(&one_reply(&fx).unwrap()).is_empty());
}

/// The tabs were reloaded between the ask and the answer: the bindings can't be trusted
/// until the tabs are reconciled again (R10), so `ls` acts on nothing and lists none.
#[test]
fn ls_whose_tabs_were_reloaded_meanwhile_answers_without_the_list() {
    let mut r = Rig::new();
    r.tabs
        .push(tab("%6", 60, "", false, false, &r.home().join("private/x")));
    runs(60, Some(claude(&r.home().join(".claude-a"), Some("C6"))));
    runs(50, None);
    r.ask_ls();
    let n = r.st.tmux().queries().max().unwrap();
    let snap = r.snap.clone();
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        r.b.now(),
    );
    assert!(!r.st.tmux().reconciled());
    let fx = r.answer(n);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(unsets(&fx).is_empty(), "{fx:?}");
    let done = one_reply(&fx).unwrap();
    assert!(untracked_rows(&done).is_empty());
    assert_eq!(row(&done, A)["alive"], json!("idle"));
    assert!(r.st.binding(id(A)).is_some());
}

/// The sleep of an adopted session whose agent left can't be saved: one warn with a count,
/// nothing taken off its tab, and the reply shows it as it is.
#[test]
fn ls_whose_sleep_isnt_saved_says_so_in_the_log() {
    let mut r = Rig::build(true);
    runs(50, None);
    let fx = r.ls();
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(unsets(&fx).is_empty(), "{fx:?}");
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::Log(LogLine { level: LogLevel::Warn, message })
                if message == "1 adopted sessions whose agent left weren't put to sleep: nothing was saved"
        )),
        "{fx:?}"
    );
    let done = one_reply(&fx).unwrap();
    assert_eq!(row(&done, A)["alive"], json!("idle"));
    assert_eq!(row(&done, A)["tracked"], json!(false));
    assert!(r.st.binding(id(A)).is_some());
}

/// An adopted agent that left, and another Claude in the tab running a conversation of its
/// own: the same.
#[test]
fn an_adopted_tab_running_another_conversation_shows_asleep_in_ls() {
    let mut r = Rig::new();
    runs(
        50,
        Some(claude(&r.home().join(".claude-a"), Some("C-other"))),
    );
    let fx = r.ls();
    let done = one_reply(&fx).unwrap();
    assert_eq!(row(&done, A)["alive"], json!("asleep"));
    assert!(r.st.binding(id(A)).is_none());
}

#[test]
fn ls_without_a_link_answers_at_once() {
    let b = TypicalBrain::new();
    let deps = Deps {
        home: b.home.clone(),
        inspect,
        ..Deps::default()
    };
    let snap = b.snapshot();
    let mut st = loaded_with(&b, TOML, deps, snap);
    let fx = send(&b, &mut st, 1, Op::SessionLs { all: true });
    let done = one_reply(&fx).unwrap();
    assert!(untracked_rows(&done).is_empty());
    assert!(st.tmux().queries().next().is_none());

    // Linked, but not reconciled yet (R10): at once too, without the list.
    link_up(&b, &mut st);
    let fx = send(&b, &mut st, 2, Op::SessionLs { all: true });
    one_reply(&fx).unwrap();
    assert!(st.tmux().queries().next().is_none());
}

#[test]
fn ls_when_the_listing_fails_answers_without_the_list() {
    let mut r = Rig::new();
    r.tabs
        .push(tab("%6", 60, "", false, false, &r.home().join("private/x")));
    runs(60, Some(claude(&r.home().join(".claude-a"), Some("C6"))));
    // tmux refuses the listing.
    r.ask_ls();
    let n = r.st.tmux().queries().max().unwrap();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Err("no".into()), Ok(Vec::new())],
        },
    );
    let done = one_reply(&fx).unwrap();
    assert!(!done.text.contains("UNTRACKED"), "{}", done.text);
    assert!(untracked_rows(&done).is_empty());
    assert_eq!(row(&done, A)["tracked"], json!(false));

    // The link goes before tmux answers.
    r.ask_ls();
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let done = one_reply(&fx).unwrap();
    assert!(untracked_rows(&done).is_empty());
    assert!(changes(&fx).is_empty(), "{fx:?}");
}

/// The caller went before the tabs were listed: the `ls` is dropped.
#[test]
fn ls_is_forgotten_when_its_caller_goes() {
    let mut r = Rig::new();
    r.ask_ls();
    handle_checked(&mut r.st, Input::ClientGone(ConnId(1)), r.b.now());
    assert!(r.st.tmux().queries().next().is_none());
}
