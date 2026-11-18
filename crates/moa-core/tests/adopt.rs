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
    Activity, ActivityWho, AgentKind, Conversation, ConversationReason, Lifecycle, Location,
    PaneAgent, PaneBinding, Session, SessionId,
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
/// sessions to `work`; `mine` is Claude's own default store.
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
    /// The agent each pid runs.
    static AGENTS: RefCell<BTreeMap<i64, PaneAgent>> = const { RefCell::new(BTreeMap::new()) };
    /// What the newest-transcript lookup finds.
    static NEWEST: RefCell<Option<String>> = const { RefCell::new(None) };
    static NEWEST_ASKED: RefCell<Vec<NewestAsk>> = const { RefCell::new(Vec::new()) };
}

fn inspect(pid: i64, _: &[String]) -> Option<PaneAgent> {
    AGENTS.with(|a| a.borrow().get(&pid).cloned())
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
        AGENTS.with(|a| a.borrow_mut().insert(pid, agent));
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
/// transcript, is still its session's agent the next time the tabs are read: it stays live.
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
    assert!(r.st.binding(new).is_some());
    let s = r.st.session(new).unwrap();
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);

    let fx = send(&r.b, &mut r.st, 2, Op::SessionLs { all: false });
    assert!(changes(&fx).is_empty(), "{fx:?}");
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
    assert_eq!(
        r.st.conversations_of(new).next().unwrap().conversation_id,
        C9
    );
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

#[test]
fn adopt_refuses_a_terminal() {
    let mut r = World::new().reconciled();
    r.tabs[0] = r.tabs[0].replacen("\t50\t0\t\t", "\t50\t0\t1\t", 1);
    r.in_5(Some(&r.home().join(".claude-a")), Some(C9));
    let fx = r.adopt("%5");
    refused(&fx, "that tab is a terminal, not an agent");
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
