//! `adopt %N` (R21): an agent the user started by hand in a tab, taken on as an untracked
//! session after one listing of the tabs, or refused with a reason. The typical brain, its
//! link played by hand; the tab's agent and the newest transcript come from test functions.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Conversation, ConversationReason, Inspected, Lifecycle,
    Location, PaneAgent, PaneBinding, Session, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError};
use moa_core::sleep::why_not;
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::link::NOT_READY;
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

use common::*;

/// Brain `default` owns `~/private`, the `work` brain `~/office`. `acct-w` sends its
/// sessions to `work`, `acct-d` runs only in `~/private/boards`, and `mine` is Claude's own
/// default store.
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

[accounts.acct-w]
kind = "claude"
store = "~/.claude-w"
launcher = "/opt/claude"
brain = "work"

[accounts.acct-d]
kind = "claude"
store = "~/.claude-d"
launcher = "/opt/claude"
only = ["~/private/boards"]

[accounts.mine]
kind = "claude"
store = "~/.claude"
launcher = "/opt/claude-mine"
"#;

/// K: live in `~/private/web`, its agent moa's own, in tab %7 (pid 70).
const K: &str = "00000000-0000-4000-8000-0000000000b7";
const CK: &str = "c0000000-0000-4000-8000-0000000000b7";
/// Z: asleep, on conversation CZ in acct-a's store.
const Z: &str = "00000000-0000-4000-8000-0000000000b8";
const CZ: &str = "c0000000-0000-4000-8000-0000000000b8";
/// X: live, with a binding of this server naming tab %5.
const X: &str = "00000000-0000-4000-8000-0000000000b9";
const CX: &str = "c0000000-0000-4000-8000-0000000000b9";
/// A session id the brain doesn't have.
const Y: &str = "00000000-0000-4000-8000-0000000000ba";
/// The conversation the hand-started agent in %5 runs.
const C9: &str = "C9";

fn id(s: &str) -> SessionId {
    s.parse().unwrap()
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

/// A minute before the typical now: when the agent in %5 started.
fn started(b: &TypicalBrain) -> UtcMs {
    UtcMs(b.now().wall.0 - 60_000)
}

/// What the newest-transcript lookup was asked: store, folder, held, since.
type NewestAsk = (PathBuf, PathBuf, Vec<String>, Option<UtcMs>);

thread_local! {
    /// What a read of each pid finds; a pid not in it runs no agent.
    static AGENTS: RefCell<BTreeMap<i64, Inspected>> = const { RefCell::new(BTreeMap::new()) };
    /// What the newest-transcript lookup finds.
    static NEWEST: RefCell<Option<String>> = const { RefCell::new(None) };
    static NEWEST_ASKED: RefCell<Vec<NewestAsk>> = const { RefCell::new(Vec::new()) };
}

fn inspect(pid: i64, _: &[String]) -> Inspected {
    AGENTS.with(|a| a.borrow().get(&pid).cloned().unwrap_or(Inspected::Gone))
}

fn newest(store: &Path, cwd: &Path, held: &[String], since: Option<UtcMs>) -> Option<String> {
    let mut held = held.to_vec();
    held.sort();
    NEWEST_ASKED.with(|a| {
        a.borrow_mut()
            .push((store.to_path_buf(), cwd.to_path_buf(), held, since))
    });
    NEWEST.with(|n| n.borrow().clone())
}

/// A Claude started by hand: `CLAUDE_CONFIG_DIR=<store>` (none when `None`), running `conv`.
fn claude(store: Option<&Path>, conv: Option<&str>, started_at: UtcMs) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::Claude,
        store_dir: store.map(shown),
        conversation: conv.map(str::to_string),
        moa_session: None,
        moa_brain: None,
        title: None,
        group: None,
        started_at: Some(started_at),
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

/// A `PANE_INFO` line: a live tab, not a terminal, with this tag, in `cwd`.
fn line(pane: &str, pid: i64, tag: &str, cwd: &Path) -> String {
    format!("I\t{pane}\t{pid}\t0\t\t\t{tag}\t{}", cwd.display())
}

/// The world before loading: the typical brain plus K (live in %7), Z (asleep) and, when
/// asked, X (live, bound to %5). Shell tab %5 (pid 50) is in `~/private/lantern`.
struct World {
    b: TypicalBrain,
    sessions: Vec<Session>,
    conversations: Vec<Conversation>,
    bindings: Vec<PaneBinding>,
    /// The tabs reconcile lists.
    tabs: Vec<String>,
}

impl World {
    fn new() -> World {
        AGENTS.with(|a| a.borrow_mut().clear());
        NEWEST.with(|n| *n.borrow_mut() = None);
        NEWEST_ASKED.with(|a| a.borrow_mut().clear());
        let b = TypicalBrain::new();
        let sessions = vec![
            session(&b, K, "private/web", Lifecycle::Live),
            session(&b, Z, "private/lantern", Lifecycle::Ended),
        ];
        let conversations = vec![conversation(&b, K, CK), conversation(&b, Z, CZ)];
        let tabs = vec![
            line("%5", 50, "", &b.home.join("private/lantern")),
            line("%7", 70, K, &b.home.join("private/web")),
        ];
        World {
            sessions,
            conversations,
            bindings: vec![binding(K, "%7", 70, true)],
            tabs,
            b,
        }
    }

    /// X too, live, its binding of this server naming %5: reconcile keeps it, since %5
    /// isn't tagged yet.
    fn with_x() -> World {
        let mut w = World::new();
        w.sessions
            .push(session(&w.b, X, "private/lantern", Lifecycle::Live));
        w.conversations.push(conversation(&w.b, X, CX));
        w.bindings.push(binding(X, "%5", 50, true));
        w
    }

    /// Loaded, linked, drained and reconciled. No tab runs an agent yet as far as `inspect`
    /// knows, so reconcile binds none by its agent.
    fn reconciled(self) -> Rig {
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: self.b.home.clone(),
            inspect,
            newest_conversation: newest,
            ..Deps::default()
        };
        let mut snap = self.b.snapshot();
        snap.sessions.extend(self.sessions);
        snap.conversations.extend(self.conversations);
        snap.pane_bindings.extend(self.bindings);
        let mut st = loaded_with(&self.b, TOML, deps, snap);
        link_up(&self.b, &mut st);
        handle_checked(&mut st, Input::StartDrained, self.b.now());
        let tabs: Vec<&str> = self.tabs.iter().map(String::as_str).collect();
        listed(&mut st, &self.b, &tabs, &[]);
        assert!(st.tmux().reconciled());
        let tabs = self.tabs.clone();
        Rig {
            b: self.b,
            st,
            tabs,
        }
    }
}

struct Rig {
    b: TypicalBrain,
    st: State,
    /// The tabs the adopt's listing shows.
    tabs: Vec<String>,
}

impl Rig {
    fn home(&self) -> PathBuf {
        self.b.home.clone()
    }

    fn lantern(&self) -> PathBuf {
        self.b.home.join("private/lantern")
    }

    /// The agent `pid` runs, from now on.
    fn runs(&self, pid: i64, agent: PaneAgent) {
        AGENTS.with(|a| a.borrow_mut().insert(pid, Inspected::Agent(agent)));
    }

    /// `pid`'s process can't be read, from now on.
    fn unreadable(&self, pid: i64) {
        AGENTS.with(|a| a.borrow_mut().insert(pid, Inspected::Unknown));
    }

    /// %5's agent: started by hand a minute ago, on `store`, running `conv`.
    fn in_5(&self, store: Option<&Path>, conv: Option<&str>) {
        self.runs(50, claude(store, conv, started(&self.b)));
    }

    /// `adopt` sent, and only asks for the tabs.
    fn ask(&mut self, op: Op) -> Vec<Effect> {
        let fx = send(&self.b, &mut self.st, 1, op);
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
        fx
    }

    /// `adopt <pane> [flags]`, answered with [`Rig::tabs`].
    fn adopt_with(
        &mut self,
        pane: &str,
        title: Option<&str>,
        group: Option<&str>,
        conv: Option<&str>,
    ) -> Vec<Effect> {
        self.ask(adopt_op(pane, title, group, conv));
        let tabs: Vec<&str> = self.tabs.iter().map(String::as_str).collect();
        listed(&mut self.st, &self.b, &tabs, &[])
    }

    fn adopt(&mut self, pane: &str) -> Vec<Effect> {
        self.adopt_with(pane, None, None, None)
    }

    /// The tabs show %5 in `folder` instead.
    fn five_in(&mut self, folder: &Path) {
        self.tabs[0] = line("%5", 50, "", folder);
    }

    fn handle(&self, sid: &str) -> String {
        self.st.handle_of(id(sid))
    }
}

fn adopt_op(pane: &str, title: Option<&str>, group: Option<&str>, conv: Option<&str>) -> Op {
    Op::SessionAdopt {
        pane: pane.into(),
        title: title.map(str::to_string),
        group: group.map(str::to_string),
        id: conv.map(str::to_string),
    }
}

fn one_reply(fx: &[Effect]) -> Result<Done, OpError> {
    let r = replies(fx);
    assert_eq!(r.len(), 1, "{fx:?}");
    r[0].1.clone()
}

/// The new session's id, from an adopt's reply.
fn adopted_id(fx: &[Effect]) -> SessionId {
    let done = one_reply(fx).unwrap();
    done.data["id"].as_str().unwrap().parse().unwrap()
}

/// The options set on tabs (`set-option -p`): moa's tags.
fn set_options(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name() == "set-option" && c.argv().first().is_some_and(|a| a == "-p"))
        .collect()
}

fn tag(pane: &str, name: &str, value: &str) -> Cmd {
    Cmd::new("set-option").args(["-p", "-t", pane, name, value])
}

/// A refusal: this text, and nothing saved or sent.
fn refused(fx: &[Effect], text: &str) {
    assert_eq!(one_reply(fx), Err(OpError(text.into())), "{fx:?}");
    assert!(changes(fx).is_empty(), "{fx:?}");
    assert!(set_options(fx).is_empty(), "{fx:?}");
}

/// Where in `fx` the first effect matching `f` is.
fn at(fx: &[Effect], f: impl Fn(&Effect) -> bool) -> usize {
    fx.iter().position(f).unwrap_or_else(|| panic!("{fx:?}"))
}

#[test]
fn adopt_binds_an_agent_typed_in_a_shell() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some(C9));
    let fx = r.adopt("%5");
    let now = r.b.now().wall;
    let lantern = shown(&r.lantern());

    let new = adopted_id(&fx);
    assert_eq!(
        one_reply(&fx),
        Ok(Done {
            text: format!("adopted C9 (acct-a) in {lantern} as lantern [-]"),
            data: json!({ "id": new, "title": "lantern" }),
        })
    );
    let s = r.st.session(new).unwrap().clone();
    assert_eq!(s.title.as_deref(), Some("lantern"));
    assert_eq!(s.account.as_deref(), Some("acct-a"));
    assert_eq!(s.cwd.as_deref(), Some(lantern.as_str()));
    assert_eq!(s.start_cwd.as_deref(), Some(lantern.as_str()));
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.location, Location::Here);
    assert_eq!(s.group_path, None);
    assert_eq!(s.created_at, now);
    let convs: Vec<&Conversation> = r.st.conversations_of(new).collect();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].conversation_id, C9);
    assert_eq!(convs[0].reason, ConversationReason::Adopt);
    assert_eq!(convs[0].store_dir.as_deref(), Some(shown(&store).as_str()));
    let b = r.st.binding(new).unwrap();
    assert_eq!(
        *b,
        PaneBinding {
            session_id: new,
            server_pid: generation().pid,
            server_start: generation().start,
            socket: "moa".into(),
            pane_id: "%5".into(),
            pane_pid: 50,
            bound_at: now,
            tracked: false,
        }
    );

    // One commit, with the activity row; then the tags, adopted first; then the reply.
    let saved = changes(&fx);
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::Session(s) if s.id == new))
    );
    assert!(
        saved
            .iter()
            .any(|c| matches!(c, Change::PaneBinding(b) if b.session_id == new))
    );
    let rows: Vec<_> = saved
        .iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some((a.who, a.text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        vec![(
            ActivityWho::You,
            format!("adopted lantern (acct-a, {lantern})")
        )]
    );
    assert_eq!(
        set_options(&fx),
        vec![
            &tag("%5", "@moa_adopted", "1"),
            &tag("%5", "@moa_session", &new.to_string())
        ]
    );
    let persist = at(&fx, |e| matches!(e, Effect::Persist(_)));
    let tags = at(
        &fx,
        |e| matches!(e, Effect::Tmux { cmds, .. } if cmds.iter().any(|c| c.argv().contains(&"@moa_session".to_string()))),
    );
    let reply = at(&fx, |e| matches!(e, Effect::Reply { .. }));
    assert!(persist < tags && tags < reply, "{fx:?}");
    // The tag is known at once, before any listing shows it (R18 reads it).
    let five = "%5".parse().unwrap();
    assert_eq!(r.st.tmux().topology.moa_session(five), Some(new));
}

#[test]
fn adopt_reads_no_variable_as_the_default_store() {
    let mut r = World::new().reconciled();
    r.in_5(None, Some(C9));
    let fx = r.adopt("%5");
    let lantern = shown(&r.lantern());
    let new = adopted_id(&fx);
    assert_eq!(
        one_reply(&fx).unwrap().text,
        format!("adopted C9 (mine) in {lantern} as lantern [-]")
    );
    assert_eq!(r.st.session(new).unwrap().account.as_deref(), Some("mine"));
    let conv = r.st.conversations_of(new).next().unwrap();
    assert_eq!(
        conv.store_dir.as_deref(),
        Some(shown(&r.home().join(".claude")).as_str())
    );
}

/// N1: an agent on the default store whose argv names no conversation, adopted by its newest
/// transcript, is still its session's agent the next time the tabs are read, by `ls` (R23)
/// and by a new connection's reconcile: it stays live, and nothing is saved.
#[test]
fn a_default_folder_adoption_stays_live_in_ls() {
    let mut r = World::new().reconciled();
    r.in_5(None, None);
    NEWEST.with(|n| *n.borrow_mut() = Some(C9.into()));
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    assert_eq!(r.st.binding(new).map(|b| b.tracked), Some(false));

    // The next listing shows %5 as adopt tagged it, with the same agent in it.
    let lantern = r.lantern();
    let tagged = format!("I\t%5\t50\t0\t\t1\t{new}\t{}", lantern.display());
    let k = line("%7", 70, K, &r.home().join("private/web"));
    let still_live = |r: &mut Rig, req: u64| {
        let fx = send(&r.b, &mut r.st, req, Op::SessionLs { all: false });
        assert!(replies(&fx).is_empty(), "{fx:?}");
        let fx = listed(&mut r.st, &r.b, &[&tagged, &k], &[]);
        assert!(changes(&fx).is_empty(), "{fx:?}");
        assert!(r.st.binding(new).is_some());
        let s = r.st.session(new).unwrap();
        assert_eq!(s.lifecycle, Lifecycle::Live);
        assert_eq!(s.asleep_at, None);
        let done = one_reply(&fx).unwrap();
        let row = done
            .data
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == json!(new))
            .unwrap()
            .clone();
        assert_eq!(row["alive"], "idle", "{row}");
        assert_eq!(row["display"], "idle", "{row}");
        assert_eq!(row["tracked"], false, "{row}");
        // Its tab is the session's, not an agent moa doesn't track.
        assert!(!done.text.contains("UNTRACKED"), "{}", done.text);
    };
    still_live(&mut r, 2);

    feed(
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
    assert!(!r.st.tmux().reconciled());
    let fx = listed(&mut r.st, &r.b, &[&tagged, &k], &[]);
    assert!(r.st.tmux().reconciled());
    assert!(changes(&fx).is_empty(), "{fx:?}");
    still_live(&mut r, 3);
}

/// The adopted agent leaves its shell tab and the user types another Claude there: `ls`
/// puts the session to sleep and takes moa's tags off the tab, so the next `ls` lists the
/// new agent as one moa doesn't track (R23), ready to be adopted.
#[test]
fn a_tab_an_adopted_agent_left_is_listed_again_once_another_runs_there() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some(C9));
    let new = adopted_id(&r.adopt("%5"));
    let five = "%5".parse().unwrap();
    assert_eq!(r.st.tmux().topology.moa_session(five), Some(new));

    // The agent left: the listing shows %5 as adopt tagged it, running no agent.
    AGENTS.with(|a| a.borrow_mut().remove(&50));
    let lantern = r.lantern();
    let tagged = format!("I\t%5\t50\t0\t\t1\t{new}\t{}", lantern.display());
    let k = line("%7", 70, K, &r.home().join("private/web"));
    send(&r.b, &mut r.st, 2, Op::SessionLs { all: false });
    let fx = listed(&mut r.st, &r.b, &[&tagged, &k], &[]);
    assert_eq!(r.st.session(new).unwrap().lifecycle, Lifecycle::Ended);
    assert!(r.st.binding(new).is_none());
    assert_eq!(
        unsets(&fx),
        untagged("%5").iter().collect::<Vec<_>>(),
        "{fx:?}"
    );
    assert_eq!(r.st.tmux().topology.moa_session(five), None);

    // Another Claude, typed in that shell.
    r.in_5(Some(&store), Some("C10"));
    send(&r.b, &mut r.st, 3, Op::SessionLs { all: false });
    let untagged = line("%5", 50, "", &lantern);
    let fx = listed(&mut r.st, &r.b, &[&untagged, &k], &[]);
    let done = one_reply(&fx).unwrap();
    let shown_lantern = shown(&lantern);
    assert!(
        done.text.ends_with(&format!(
            "UNTRACKED tab %5: acct-a in {shown_lantern}  → moactl adopt %5"
        )),
        "{}",
        done.text
    );
}

#[test]
fn adopt_takes_the_newest_free_transcript_since_the_agent_started() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), None);
    NEWEST.with(|n| *n.borrow_mut() = Some(C9.into()));
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    assert_eq!(
        r.st.conversations_of(new).next().unwrap().conversation_id,
        C9
    );
    // K's tab is kept, so its conversation is held; Z has no tab, so CZ is free.
    let asked = NEWEST_ASKED.with(|a| a.borrow().clone());
    assert_eq!(
        asked,
        vec![(
            store,
            r.lantern(),
            vec![CK.to_string()],
            Some(started(&r.b))
        )]
    );
}

#[test]
fn adopt_takes_the_given_id_over_the_agents_own() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some("C8"));
    let fx = r.adopt_with("%5", Some("boards"), Some("/Work//Lantern/"), Some(C9));
    let new = adopted_id(&fx);
    let lantern = shown(&r.lantern());
    assert_eq!(
        one_reply(&fx).unwrap().text,
        format!("adopted C9 (acct-a) in {lantern} as boards [Work/Lantern]")
    );
    let s = r.st.session(new).unwrap();
    assert_eq!(s.title.as_deref(), Some("boards"));
    assert!(s.title_locked);
    assert_eq!(s.group_path.as_deref(), Some("Work/Lantern"));
    assert!(r.st.groups().any(|g| g.path == "Work/Lantern"));
    assert_eq!(r.st.handle_of(new), C9);
    assert!(NEWEST_ASKED.with(|a| a.borrow().is_empty()));
}

/// Without flags, the title and group come from `MOA_TITLE` and `MOA_GROUP`.
#[test]
fn adopt_names_it_from_the_agents_environment() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    let mut agent = claude(Some(&store), Some(C9), started(&r.b));
    agent.title = Some("boards".into());
    agent.group = Some("Work".into());
    r.runs(50, agent);
    let fx = r.adopt("%5");
    let s = r.st.session(adopted_id(&fx)).unwrap();
    assert_eq!(s.title.as_deref(), Some("boards"));
    assert_eq!(s.group_path.as_deref(), Some("Work"));
}

/// A conversation moa knows keeps its session: Z wakes in %5, untracked.
#[test]
fn adopt_reuses_the_session_of_a_known_conversation() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some(CZ));
    let sessions = r.st.sessions().count();
    let fx = r.adopt("%5");
    assert_eq!(adopted_id(&fx), id(Z));
    assert_eq!(r.st.sessions().count(), sessions);
    let s = r.st.session(id(Z)).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);
    assert_eq!(r.st.conversations_of(id(Z)).count(), 1);
    assert_eq!(r.st.binding(id(Z)).map(|b| b.tracked), Some(false));
    assert_eq!(
        set_options(&fx),
        vec![
            &tag("%5", "@moa_adopted", "1"),
            &tag("%5", "@moa_session", Z)
        ]
    );
}

/// Z had CZ, then CB (a `/clear`). An agent resumed on CZ by hand and adopted makes CZ Z's
/// current conversation again: `ls` shows it, and `restart` resumes it.
#[test]
fn adopt_makes_an_older_conversation_current() {
    const CB: &str = "cb000000-0000-4000-8000-0000000000b8";
    let mut w = World::new();
    let mut later = conversation(&w.b, Z, CB);
    later.first_seen_at = UtcMs(5);
    later.last_seen_at = UtcMs(5);
    w.conversations.push(later);
    let mut r = w.reconciled();
    assert_eq!(r.handle(Z), "cb000000");

    r.in_5(Some(&r.home().join(".claude-a")), Some(CZ));
    let fx = r.adopt("%5");
    assert_eq!(adopted_id(&fx), id(Z));
    assert_eq!(r.handle(Z), "c0000000");
    assert_eq!(r.st.conversations_of(id(Z)).count(), 2);

    let fx = send(&r.b, &mut r.st, 2, Op::SessionLs { all: true });
    let lantern = r.lantern();
    let tagged = format!("I\t%5\t50\t0\t\t1\t{Z}\t{}", lantern.display());
    let k = line("%7", 70, K, &r.home().join("private/web"));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = listed(&mut r.st, &r.b, &[&tagged, &k], &[]);
    let done = one_reply(&fx).unwrap();
    let row = done
        .data
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(id(Z)))
        .unwrap()
        .clone();
    assert_eq!(row["conversation"], CZ, "{row}");

    let fx = send(&r.b, &mut r.st, 3, Op::SessionRestart { q: Z.into() });
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = listed(&mut r.st, &r.b, &[&tagged, &k], &[]);
    let (sid, cmds) = the_open(&fx);
    assert_eq!(sid, id(Z));
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(argv[at + 1], CZ, "{argv:?}");
}

/// An agent moa started for a session it knows carries `MOA_SESSION`: it is that session's,
/// not one to adopt. Its tab is bound already, or a launch of it is still opening.
#[test]
fn adopt_refuses_moas_own_agent() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    let mut agent = claude(Some(&store), Some(C9), started(&r.b));
    agent.moa_session = Some(K.into());
    r.runs(50, agent);
    let fx = r.adopt("%5");
    refused(&fx, &format!("that tab is already {}", r.handle(K)));
}

#[test]
fn adopt_refuses_moas_own_agent_while_its_session_opens() {
    let mut r = World::new().reconciled();
    let fx = send(
        &r.b,
        &mut r.st,
        7,
        Op::SessionResume {
            q: Z.into(),
            bg: true,
        },
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let store = r.home().join(".claude-a");
    let mut agent = claude(Some(&store), Some(C9), started(&r.b));
    agent.moa_session = Some(Z.into());
    r.runs(50, agent);
    r.ask(adopt_op("%5", None, None, None));
    let n = r.st.tmux().queries().max().unwrap();
    let tabs: Vec<String> = r.tabs.clone();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(tabs), Ok(Vec::new())],
        },
    );
    refused(&fx, "it's still opening; try again in a moment");
}

/// A `MOA_SESSION` naming no session this brain knows says nothing: the adopt goes on.
#[test]
fn adopt_takes_an_agent_whose_moa_session_is_unknown() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    let mut agent = claude(Some(&store), Some(C9), started(&r.b));
    agent.moa_session = Some(Y.into());
    r.runs(50, agent);
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    assert_eq!(r.st.binding(new).map(|b| b.tracked), Some(false));
}

#[test]
fn adopt_refuses_a_tab_that_isnt_there() {
    let mut r = World::new().reconciled();
    let fx = r.adopt("%6");
    refused(&fx, "there's no tab %6");
    // A dead one too.
    r.tabs[0] = r.tabs[0].replacen("\t50\t0\t", "\t50\t1\t", 1);
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt("%5");
    refused(&fx, "there's no tab %5");
}

/// A terminal moa opened that runs only its shell is refused as one.
#[test]
fn adopt_refuses_a_terminal_with_no_agent() {
    let mut r = World::new().reconciled();
    r.tabs[0] = r.tabs[0].replacen("\t50\t0\t\t", "\t50\t0\t1\t", 1);
    let fx = r.adopt("%5");
    refused(&fx, "that tab is a terminal, not an agent");
}

/// A Claude typed in a terminal moa opened is adopted as one typed in any shell. Its tab
/// gets the adoption's tags only: it stays a terminal, whose agent is checked again before
/// any kill (R31).
#[test]
fn adopt_takes_an_agent_typed_in_a_terminal() {
    let mut r = World::new().reconciled();
    r.tabs[0] = r.tabs[0].replacen("\t50\t0\t\t", "\t50\t0\t1\t", 1);
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    let b = r.st.binding(new).unwrap();
    assert_eq!(b.pane_id, "%5");
    assert!(!b.tracked);
    assert_eq!(
        set_options(&fx),
        [
            &tag("%5", "@moa_adopted", "1"),
            &tag("%5", "@moa_session", &new.to_string())
        ]
    );
}

#[test]
fn adopt_refuses_a_tab_a_binding_holds() {
    let mut r = World::new().reconciled();
    r.runs(
        70,
        claude(Some(&r.home().join(".claude-a")), Some(CK), started(&r.b)),
    );
    let fx = r.adopt("%7");
    refused(&fx, &format!("that tab is already {}", r.handle(K)));
}

#[test]
fn adopt_refuses_a_tab_without_claude() {
    let mut r = World::new().reconciled();
    let fx = r.adopt("%5");
    refused(&fx, "that tab isn't running an agent moa recognizes");
    let mut codex = claude(None, Some(C9), started(&r.b));
    codex.kind = AgentKind::Codex;
    r.runs(50, codex);
    let fx = r.adopt("%5");
    refused(&fx, "that tab isn't running an agent moa recognizes");
}

#[test]
fn adopt_refuses_a_folder_outside_the_brain() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    for folder in [r.home().join("office/site"), r.home().join("elsewhere")] {
        r.five_in(&folder);
        let fx = r.adopt("%5");
        refused(
            &fx,
            &format!("{} isn't in this brain's folders", folder.display()),
        );
    }
}

#[test]
fn adopt_refuses_an_unknown_account() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-z")), Some(C9));
    let fx = r.adopt("%5");
    refused(
        &fx,
        "moa doesn't know the account that agent runs under; add it to moa.toml",
    );
}

#[test]
fn adopt_refuses_another_brains_account() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-w")), Some(C9));
    let fx = r.adopt("%5");
    refused(&fx, "that folder belongs to the work brain; adopt it there");
}

#[test]
fn adopt_refuses_an_agent_with_no_conversation_yet() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), None);
    let fx = r.adopt("%5");
    refused(
        &fx,
        "no conversation it wrote yet; ask it one thing first, or pass --id",
    );
    assert_eq!(NEWEST_ASKED.with(|a| a.borrow().len()), 1);
}

/// With no start time, which transcripts were written since the agent started can't be
/// told: no transcript is looked for, so an older free one is never taken for its own.
#[test]
fn adopt_refuses_an_agent_whose_start_time_isnt_known() {
    let mut r = World::new().reconciled();
    let mut agent = claude(Some(&r.home().join(".claude-a")), None, started(&r.b));
    agent.started_at = None;
    r.runs(50, agent);
    NEWEST.with(|n| *n.borrow_mut() = Some(C9.into()));
    let fx = r.adopt("%5");
    refused(
        &fx,
        "no conversation it wrote yet; ask it one thing first, or pass --id",
    );
    assert!(NEWEST_ASKED.with(|a| a.borrow().is_empty()));
    // `--id` doesn't either: naming no conversation, with no start time, the agent could
    // never be told from a later one in the tab, and the binding would be let go at the next
    // look (R31).
    let fx = r.adopt_with("%5", None, None, Some(C9));
    refused(&fx, NO_START);
}

const NO_START: &str =
    "moa can't read when that agent started, so it couldn't tell it from a later one in that tab";

/// moa's own agent for Z, typed in a terminal (so its binding would be untracked), naming
/// no conversation and with no start time: no later look could prove it, so it isn't bound.
#[test]
fn adopt_refuses_moas_own_agent_it_could_never_prove_again() {
    let mut r = World::new().reconciled();
    r.tabs[0] = r.tabs[0].replacen("\t50\t0\t\t", "\t50\t0\t1\t", 1);
    let mut agent = claude(Some(&r.home().join(".claude-a")), None, started(&r.b));
    agent.moa_session = Some(Z.into());
    agent.started_at = None;
    r.runs(50, agent);
    let fx = r.adopt("%5");
    refused(&fx, NO_START);
    assert!(r.st.binding(id(Z)).is_none());
}

/// A tab whose process can't be read: adopt needs an agent, so it refuses, and says why.
#[test]
fn adopt_refuses_a_tab_it_cant_read() {
    let mut r = World::new().reconciled();
    r.unreadable(50);
    let fx = r.adopt("%5");
    refused(
        &fx,
        "moa couldn't read what runs in that tab; try again in a moment",
    );
}

/// An adopted tab whose process can't be read is still the adopted session's: adopt
/// doesn't let it go, and refuses as for any held tab.
#[test]
fn adopt_keeps_an_adopted_tab_it_cant_read() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let old = adopted_id(&r.adopt("%5"));
    r.unreadable(50);
    r.tabs = tagged_tabs(&r, old);
    let fx = r.adopt("%5");
    refused(&fx, &format!("that tab is already {}", r.st.handle_of(old)));
    assert!(unsets(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.st.session(old).unwrap().lifecycle, Lifecycle::Live);
    assert!(r.st.binding(old).is_some());
}

/// An id moa couldn't resume later is refused now, from `--id` or from the agent's argv.
#[test]
fn adopt_refuses_an_id_moa_cant_resume() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    let text = "that conversation's id isn't one moa can resume";
    r.in_5(Some(&store), Some("../x"));
    let fx = r.adopt("%5");
    refused(&fx, text);
    r.in_5(Some(&store), Some(C9));
    // A leading `-` would read as a flag on the resume's command line.
    for bad in [
        "bad id",
        "a/b",
        &"a".repeat(65),
        "--dangerously-skip-permissions",
        "-x",
    ] {
        let fx = r.adopt_with("%5", None, None, Some(bad));
        refused(&fx, text);
    }
    assert!(NEWEST_ASKED.with(|a| a.borrow().is_empty()));
}

/// An account limited to other folders can't take a session here.
#[test]
fn adopt_refuses_an_account_limited_to_other_folders() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-d")), Some(C9));
    let fx = r.adopt("%5");
    refused(&fx, "acct-d can't run sessions in that folder");
}

/// Z's resume is looking at the tabs, about to open it in a tab of its own: adopting the
/// agent that runs Z's conversation waits for it.
#[test]
fn adopt_refuses_a_session_that_is_opening() {
    let mut r = World::new().reconciled();
    let fx = send(
        &r.b,
        &mut r.st,
        7,
        Op::SessionResume {
            q: Z.into(),
            bg: true,
        },
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    r.in_5(Some(&r.home().join(".claude-a")), Some(CZ));
    r.ask(adopt_op("%5", None, None, None));
    // The adopt's listing is the newest query; the resume's is still out.
    let n = r.st.tmux().queries().max().unwrap();
    let tabs: Vec<String> = r.tabs.clone();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(tabs), Ok(Vec::new())],
        },
    );
    refused(&fx, "it's still opening; try again in a moment");
    assert!(r.st.binding(id(Z)).is_none());
}

#[test]
fn adopt_refuses_a_conversation_another_tab_runs() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(CK));
    let fx = r.adopt("%5");
    refused(
        &fx,
        &format!(
            "that conversation is already running in another tab ({})",
            r.handle(K)
        ),
    );
    // Given by `--id` too.
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt_with("%5", None, None, Some(CK));
    refused(
        &fx,
        &format!(
            "that conversation is already running in another tab ({})",
            r.handle(K)
        ),
    );
}

/// X's binding names %5, but %5 is tagged with another session by the time adopt reads the
/// tabs: the binding no longer holds it (R4). X goes to sleep and loses it, in the adopt's
/// commit, and the adopt goes on.
#[test]
fn adopt_frees_a_stale_binding_of_the_pane() {
    let mut r = World::with_x().reconciled();
    assert!(r.st.binding(id(X)).is_some());
    r.tabs[0] = line("%5", 50, Y, &r.lantern());
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    assert_ne!(new, id(X));
    let x = r.st.session(id(X)).unwrap();
    assert_eq!(x.lifecycle, Lifecycle::Ended);
    assert_eq!(x.asleep_at, Some(r.b.now().wall));
    assert!(r.st.binding(id(X)).is_none());
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(s) if *s == id(X)))
    );
    assert_eq!(r.st.binding(new).map(|b| b.pane_id.as_str()), Some("%5"));
}

#[test]
fn an_adopted_session_shows_idle_and_never_sleeps_on_its_own() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    let now = r.b.now().wall;
    let s = r.st.session(new).unwrap();
    assert_eq!(display_state(s, now).0, DisplayState::Idle);
    let hour = 3_600_000;
    let later = UtcMs(now.0 + 2 * hour);
    assert_eq!(display_state(s, later).0, DisplayState::Idle);
    assert_eq!(why_not(&r.st, new, later, hour), Some("not tracked"));
}

#[test]
fn adopt_waits_for_the_tabs_to_be_reconciled() {
    let b = TypicalBrain::new();
    let deps = Deps {
        home: b.home.clone(),
        inspect,
        ..Deps::default()
    };
    let snap = b.snapshot();
    let mut st = loaded_with(&b, TOML, deps, snap);
    link_up(&b, &mut st);
    let fx = send(&b, &mut st, 1, adopt_op("%5", None, None, None));
    assert_eq!(one_reply(&fx), Err(OpError(NOT_READY.into())));
    assert!(st.tmux().queries().next().is_none());
}

#[test]
fn adopt_refuses_what_isnt_a_tab_id() {
    let mut r = World::new().reconciled();
    for bad in ["5", "%", "%x", "@5"] {
        let fx = send(&r.b, &mut r.st, 1, adopt_op(bad, None, None, None));
        refused(&fx, "adopt needs a tab id like %5");
        assert!(r.st.tmux().queries().next().is_none());
    }
}

#[test]
fn adopt_says_so_when_the_terminal_goes_before_it_lists_the_tabs() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    r.ask(adopt_op("%5", None, None, None));
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    refused(&fx, "moa's terminal went away; try again");
}

/// The caller went before the tabs were listed: the adopt is dropped, as a resume is.
#[test]
fn adopt_is_forgotten_when_its_caller_goes() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    r.ask(adopt_op("%5", None, None, None));
    handle_checked(&mut r.st, Input::ClientGone(ConnId(1)), r.b.now());
    assert!(r.st.tmux().queries().next().is_none());
}

/// The tabs as a listing shows them once session `sid` adopted %5: %5 marked and tagged,
/// and K's %7.
fn tagged_tabs(r: &Rig, sid: SessionId) -> Vec<String> {
    vec![
        format!("I\t%5\t50\t0\t\t1\t{sid}\t{}", r.lantern().display()),
        line("%7", 70, K, &r.home().join("private/web")),
    ]
}

impl Rig {
    /// Request `req`, `op`, whose one pane query is answered with `tabs`.
    fn with_tabs(&mut self, req: u64, op: Op, tabs: &[String]) -> Vec<Effect> {
        let fx = send(&self.b, &mut self.st, req, op);
        assert!(replies(&fx).is_empty(), "{fx:?}");
        let tabs: Vec<&str> = tabs.iter().map(String::as_str).collect();
        listed(&mut self.st, &self.b, &tabs, &[])
    }
}

/// The `kill-pane`s in `fx`.
fn kills(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name() == "kill-pane")
        .collect()
}

/// The argument after `--resume` in the one launch in `fx`.
fn resumed_conversation(fx: &[Effect]) -> String {
    let (_, cmds) = the_open(fx);
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "--resume").unwrap();
    argv[at + 1].clone()
}

/// The agent was started on C8 and runs C9 now (a `/resume` or `/clear` inside it), and is
/// adopted with `--id C9`. Its command line still says C8, so C8 is kept as one of the
/// session's conversations too: the next look at the tab still finds the session's agent,
/// and a restart resumes C9, the current one.
#[test]
fn adopt_with_another_id_keeps_its_agent() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some("C8"));
    let new = adopted_id(&r.adopt_with("%5", None, None, Some(C9)));
    assert_eq!(r.st.handle_of(new), C9);
    let mut convs: Vec<&str> =
        r.st.conversations_of(new)
            .map(|c| c.conversation_id.as_str())
            .collect();
    convs.sort_unstable();
    assert_eq!(convs, ["C8", C9]);

    let tabs = tagged_tabs(&r, new);
    let fx = r.with_tabs(2, Op::SessionLs { all: false }, &tabs);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.st.session(new).unwrap().lifecycle, Lifecycle::Live);
    assert!(r.st.binding(new).is_some());

    let restart = Op::SessionRestart { q: new.to_string() };
    let fx = r.with_tabs(3, restart, &tabs);
    assert_eq!(kills(&fx).len(), 1, "{fx:?}");
    assert_eq!(resumed_conversation(&fx), C9);
}

/// An agent started on another session's conversation is that session's: `--id` can't move
/// it to a new one.
#[test]
fn adopt_refuses_an_id_for_an_agent_started_on_another_sessions_conversation() {
    let mut r = World::new().reconciled();
    r.in_5(Some(&r.home().join(".claude-a")), Some(CZ));
    let fx = r.adopt_with("%5", None, None, Some(C9));
    refused(
        &fx,
        &format!(
            "that agent was started on {}; adopt it without --id",
            r.handle(Z)
        ),
    );
}

/// The adopted agent named no conversation (a plain `claude`) and quit, and the user typed
/// another plain `claude` in the same shell: the same store, the same folder, but started
/// after the tab was bound, so it isn't the session's agent (R31). `ls` puts the session
/// to sleep and frees the tab; `close`, `sleep` and `restart` kill nothing.
#[test]
fn a_later_plain_claude_in_the_adopted_tab_isnt_the_adopted_agent() {
    // An agent whose start time can't be read proves nothing either.
    for later in [true, false] {
        let ops: [fn(SessionId) -> Op; 4] = [
            |_| Op::SessionLs { all: false },
            |s| Op::SessionClose { q: s.to_string() },
            |s| Op::SessionSleep {
                qs: vec![s.to_string()],
                idle_ms: None,
                dry_run: false,
            },
            |s| Op::SessionRestart { q: s.to_string() },
        ];
        for (n, op) in ops.into_iter().enumerate() {
            let mut r = World::new().reconciled();
            let store = r.home().join(".claude-a");
            r.in_5(Some(&store), None);
            NEWEST.with(|n| *n.borrow_mut() = Some(C9.into()));
            let new = adopted_id(&r.adopt("%5"));
            let mut agent = claude(Some(&store), None, UtcMs(r.b.now().wall.0 + 60_000));
            if !later {
                agent.started_at = None;
            }
            r.runs(50, agent);
            let tabs = tagged_tabs(&r, new);
            let fx = if n == 0 {
                r.with_tabs(2, op(new), &tabs)
            } else {
                send(&r.b, &mut r.st, 2, op(new))
            };
            let case = format!("op {n}, later {later}");
            assert!(kills(&fx).is_empty(), "{case}: {fx:?}");
            assert!(r.st.binding(new).is_none(), "{case}");
            let s = r.st.session(new).unwrap();
            assert_eq!(s.lifecycle, Lifecycle::Ended, "{case}");
            assert_eq!(s.asleep_at, Some(r.b.now().wall), "{case}");
            assert_eq!(
                unsets(&fx),
                untagged("%5").iter().collect::<Vec<_>>(),
                "{case}: {fx:?}"
            );
        }
    }
}

/// The adopted agent quit and its shell stays open (U14's usual case): `resume` and `focus`
/// find it gone, put the session to sleep and free the tab, then resume it in a tab of its
/// own.
#[test]
fn resume_and_focus_after_the_adopted_agent_left_resume_it() {
    let ops: [fn(SessionId) -> Op; 2] = [
        |s| Op::SessionResume {
            q: s.to_string(),
            bg: false,
        },
        |s| Op::SessionFocus { q: s.to_string() },
    ];
    for op in ops {
        let mut r = World::new().reconciled();
        r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
        let new = adopted_id(&r.adopt("%5"));
        AGENTS.with(|a| a.borrow_mut().remove(&50));
        let fx = send(&r.b, &mut r.st, 2, op(new));
        assert!(replies(&fx).is_empty(), "{fx:?}");
        assert!(kills(&fx).is_empty(), "{fx:?}");
        assert_eq!(
            unsets(&fx),
            untagged("%5").iter().collect::<Vec<_>>(),
            "{fx:?}"
        );
        assert!(r.st.binding(new).is_none());
        let shell = line("%5", 50, "", &r.lantern());
        let k = line("%7", 70, K, &r.home().join("private/web"));
        let fx = listed(&mut r.st, &r.b, &[&shell, &k], &[]);
        assert_eq!(resumed_conversation(&fx), C9);
    }
}

/// The adopted agent quit and another Claude was typed in its shell: adopting the tab again
/// finds the old agent gone, puts its session to sleep, and takes the new one.
#[test]
fn adopt_of_a_tab_whose_adopted_agent_left_takes_the_new_agent() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some(C9));
    let old = adopted_id(&r.adopt("%5"));
    r.runs(50, claude(Some(&store), Some("C10"), r.b.now().wall));
    r.tabs = tagged_tabs(&r, old);
    let fx = r.adopt("%5");
    let new = adopted_id(&fx);
    assert_ne!(new, old);
    assert_eq!(r.st.handle_of(new), "C10");
    let s = r.st.session(old).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Ended);
    assert_eq!(s.asleep_at, Some(r.b.now().wall));
    assert!(r.st.binding(old).is_none());
    assert_eq!(r.st.binding(new).map(|b| b.pane_id.as_str()), Some("%5"));
}

/// The adopted agent quit, and the user resumed Z's conversation by hand in its shell. The
/// tab is still bound to the adopted session, but its agent runs Z's: `resume Z` (step 6)
/// refuses to start a second agent on it, and points at `adopt`, which takes the tab for Z.
#[test]
fn resume_refuses_a_conversation_typed_in_an_adopted_tab_and_adopt_takes_it() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    r.in_5(Some(&store), Some(C9));
    let old = adopted_id(&r.adopt("%5"));
    r.runs(50, claude(Some(&store), Some(CZ), r.b.now().wall));
    r.tabs = tagged_tabs(&r, old);
    let resume = Op::SessionResume {
        q: Z.into(),
        bg: true,
    };
    let tabs = r.tabs.clone();
    let fx = r.with_tabs(2, resume, &tabs);
    assert_eq!(
        one_reply(&fx),
        Err(OpError(
            "it's already running in tab %5; adopt it with moactl adopt %5".into()
        ))
    );
    assert!(kills(&fx).is_empty(), "{fx:?}");
    let fx = r.adopt("%5");
    assert_eq!(adopted_id(&fx), id(Z));
    assert_eq!(r.st.binding(id(Z)).map(|b| b.pane_id.as_str()), Some("%5"));
    assert!(r.st.binding(old).is_none());
}

/// moa's own agent for Z, whose tab's binding was never saved (its commit was refused): Z
/// has no binding and isn't opening, so adopt binds the tab to Z, tracked, as the next
/// connection's reconcile would.
#[test]
fn adopt_binds_moas_own_agent_whose_tab_wasnt_saved() {
    let mut r = World::new().reconciled();
    let store = r.home().join(".claude-a");
    let mut agent = claude(Some(&store), Some(CZ), started(&r.b));
    agent.moa_session = Some(Z.into());
    r.runs(50, agent);
    let fx = r.adopt("%5");
    assert_eq!(adopted_id(&fx), id(Z));
    assert_eq!(
        one_reply(&fx).unwrap().text,
        "bound lantern to tab %5; moa tracks it"
    );
    let s = r.st.session(id(Z)).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);
    let b = r.st.binding(id(Z)).unwrap();
    assert_eq!((b.pane_id.as_str(), b.tracked), ("%5", true));
    assert_eq!(b.bound_at, r.b.now().wall);
    // Tracked: tagged, not marked adopted.
    assert_eq!(set_options(&fx), vec![&tag("%5", "@moa_session", Z)]);
    // `--id` naming another of Z's conversations makes that one current, as adopt does.
    let mut w = World::new();
    let mut older = conversation(&w.b, Z, "CZ-older");
    older.first_seen_at = UtcMs(0);
    older.last_seen_at = UtcMs(0);
    w.conversations.push(older);
    let mut r = w.reconciled();
    let mut agent = claude(Some(&store), Some(CZ), started(&r.b));
    agent.moa_session = Some(Z.into());
    r.runs(50, agent);
    assert_eq!(r.handle(Z), CZ[..8]);
    let fx = r.adopt_with("%5", None, None, Some("CZ-older"));
    assert_eq!(adopted_id(&fx), id(Z));
    assert_eq!(r.handle(Z), "CZ-older");
    assert!(
        changes(&fx).iter().any(|c| matches!(c,
            Change::Conversation(c) if c.conversation_id == "CZ-older"
                && c.last_seen_at == r.b.now().wall)),
        "{fx:?}"
    );
    // `--id` naming a conversation that isn't Z's is refused.
    let mut r = World::new().reconciled();
    let mut agent = claude(Some(&store), Some(CZ), started(&r.b));
    agent.moa_session = Some(Z.into());
    r.runs(50, agent);
    let fx = r.adopt_with("%5", None, None, Some(C9));
    refused(
        &fx,
        &format!(
            "that agent was started for {}; adopt it without --id",
            r.handle(Z)
        ),
    );
}
