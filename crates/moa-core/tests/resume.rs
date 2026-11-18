//! `resume` (C11): an asleep session's conversation relaunched in a new tab, under the
//! account whose store it was recorded in, never the default one; refused with a reason
//! rather than guessed. Transcripts and folders are real files under the brain's temp home.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use moa_agents::claude::{find_transcript, project_slot};
use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind, StartSource};
use moa_core::live::is_asleep;
use moa_core::model::{
    Activity, AgentKind, Conversation, ConversationReason, Lifecycle, Location, PaneAgent, Session,
    SessionId, SpikePane,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::link::NOT_READY;
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

use common::*;

/// Brain `default` (folder `~/work`, opening with `acct-a`) and a `work` brain beside it.
/// `acct-b` is another account of this brain, `acct-c` has no launcher, `acct-d` is limited
/// to `~/office`, `acct-w` sends everything to `work`, `acct-x` is a Codex account, and
/// `mine` is on Claude's own default store.
const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[brains.work]
folders = ["~/office"]

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"

[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
launcher = "/opt/claude-b"

[accounts.acct-c]
kind = "claude"
store = "~/.claude-c"

[accounts.acct-d]
kind = "claude"
store = "~/.claude-d"
launcher = "/opt/claude"
only = ["~/office"]

[accounts.acct-w]
kind = "claude"
store = "~/.claude-w"
launcher = "/opt/claude"
brain = "work"

[accounts.acct-x]
kind = "codex"
store = "~/.codex-x"
launcher = "/opt/codex"

[accounts.mine]
kind = "claude"
store = "~/.claude"
launcher = "/opt/claude-mine"
"#;

/// The asleep session S, and its conversation.
const S: &str = "00000000-0000-4000-8000-0000000000a1";
const C: &str = "c0000000-0000-4000-8000-0000000000a1";
const TITLE: &str = "boards";

fn s_id() -> SessionId {
    S.parse().unwrap()
}

/// An hour before the typical now.
fn earlier(b: &TypicalBrain) -> UtcMs {
    UtcMs(b.now().wall.0 - 3_600_000)
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

/// `<store>/projects/<slot of folder>/<conv>.jsonl`, written.
fn write_transcript(store: &Path, folder: &Path, conv: &str) -> PathBuf {
    let dir = store.join("projects").join(project_slot(folder));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{conv}.jsonl"));
    fs::write(&file, "{\"type\":\"summary\",\"summary\":\"fake\"}\n").unwrap();
    file
}

/// The world before loading: the typical brain plus S, asleep in `<home>/work/lantern` on
/// conversation C, recorded in acct-a's store, whose transcript is there.
struct World {
    b: TypicalBrain,
    s: Session,
    conv: Option<Conversation>,
    spike: Option<SpikePane>,
    toml: String,
    deps: Deps,
}

impl World {
    fn new() -> World {
        let b = TypicalBrain::new();
        let lantern = b.home.join("work/lantern");
        fs::create_dir_all(&lantern).unwrap();
        let transcript = write_transcript(&b.home.join(".claude-a"), &lantern, C);
        let at = earlier(&b);
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
            lifecycle: Lifecycle::Ended,
            activity: Activity::Working,
            activity_at: Some(at),
            activity_why: Some("a tool".into()),
            last_prompt: None,
            last_turn_end_at: None,
            seen_at: None,
            asleep_at: Some(at),
            archived_at: None,
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(1),
            last_seen_at: Some(at),
        };
        let conv = Conversation {
            agent_kind: AgentKind::Claude,
            conversation_id: C.into(),
            session_id: s_id(),
            store_dir: Some(shown(&b.home.join(".claude-a"))),
            spike_account: None,
            reason: ConversationReason::Start,
            first_seen_at: at,
            last_seen_at: at,
            transcript_path: Some(shown(&transcript)),
            transcript_present: None,
            transcript_checked_at: None,
        };
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            transcript: find_transcript,
            is_dir: Path::is_dir,
            ..Deps::default()
        };
        World {
            b,
            s,
            conv: Some(conv),
            spike: None,
            toml: TOML.to_string(),
            deps,
        }
    }

    fn home(&self) -> &Path {
        &self.b.home
    }

    fn conv(&mut self) -> &mut Conversation {
        self.conv.as_mut().unwrap()
    }

    /// Loaded, linked, drained and reconciled with no tabs listed.
    fn reconciled(self) -> Rig {
        let mut r = self.linked();
        handle_checked(&mut r.st, Input::StartDrained, r.b.now());
        listed(&mut r.st, &r.b, &[], &[]);
        assert!(r.st.tmux().reconciled());
        r
    }

    /// Loaded and linked; not reconciled.
    fn linked(self) -> Rig {
        let mut snap = self.b.snapshot();
        snap.sessions.push(self.s);
        snap.conversations.extend(self.conv);
        snap.spike_panes.extend(self.spike);
        let mut st = loaded_with(&self.b, &self.toml, self.deps, snap);
        link_up(&self.b, &mut st);
        Rig { b: self.b, st }
    }
}

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    fn home(&self) -> PathBuf {
        self.b.home.clone()
    }

    fn lantern(&self) -> String {
        shown(&self.b.home.join("work/lantern"))
    }

    /// `resume S` as request `req`.
    fn resume(&mut self, req: u64, bg: bool) -> Vec<Effect> {
        let op = Op::SessionResume { q: S.into(), bg };
        send(&self.b, &mut self.st, req, op)
    }

    /// tmux answers the resume's pane query with these lines.
    fn listed(&mut self, info: &[&str], start: &[&str]) -> Vec<Effect> {
        listed(&mut self.st, &self.b, info, start)
    }

    /// `resume S`, its pane query answered with no tabs: the launch's effects.
    fn launched(&mut self, req: u64, bg: bool) -> Vec<Effect> {
        let fx = self.resume(req, bg);
        assert!(queried(&fx), "{fx:?}");
        self.listed(&[], &[])
    }

    /// tmux says the launch's tab is `pane`.
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

fn any_tmux(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Tmux { .. }))
}

fn selects(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name().starts_with("select-"))
        .collect()
}

fn select(pane: &str) -> Vec<Cmd> {
    vec![
        Cmd::new("select-window").args(["-t", pane]),
        Cmd::new("select-pane").args(["-t", pane]),
    ]
}

fn done(text: String) -> Result<Done, OpError> {
    Ok(Done {
        text,
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

/// `w` reconciled, then `resume S`, refused at once with no tab asked for: the text.
fn refused(w: World) -> (Rig, Vec<Effect>, String) {
    let mut r = w.reconciled();
    let fx = r.resume(1, false);
    assert!(!any_tmux(&fx), "{fx:?}");
    let text = refusal(&fx);
    (r, fx, text)
}

/// `refused`, asserting nothing was saved.
fn refused_unsaved(w: World) -> String {
    let (_, fx, text) = refused(w);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    text
}

/// `refused`, asserting the one thing saved is S marked `resume_failed`.
fn refused_failed(w: World) -> String {
    let (r, fx, text) = refused(w);
    let saved = changes(&fx);
    assert!(
        matches!(saved.as_slice(), [Change::Session(s)] if s.id == s_id() && s.resume_failed),
        "{saved:?}"
    );
    assert!(r.s().resume_failed);
    assert!(is_asleep(r.s()));
    text
}

/// The argument after `flag` in the launch's `new-window`.
fn after<'a>(cmd: &'a Cmd, flag: &str) -> &'a str {
    let argv = cmd.argv();
    let at = argv.iter().position(|a| a == flag).unwrap();
    &argv[at + 1]
}

#[test]
fn resume_sends_the_resume_window() {
    let mut r = World::new().reconciled();
    let fx = r.resume(1, false);
    // Step 6's one pane query, and nothing else yet.
    assert!(queried(&fx), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");

    let fx = r.listed(&[], &[]);
    let (id, cmds) = the_open(&fx);
    assert_eq!(id, s_id());
    let home = r.home();
    let home = home.display();
    assert_eq!(cmds.len(), 1);
    assert_eq!(
        cmds[0].line(),
        format!(
            "new-window -d -P -F 'P #{{pane_id}} #{{pane_pid}}' -n {TITLE} -c {home}/work/lantern \
             -t =moa: -e CLAUDE_CONFIG_DIR={home}/.claude-a -e MOA_SESSION={S} -e MOA_BRAIN=default \
             -e CLAUDE_CODE_PLUGIN_DIRS={PLUGIN} /opt/claude --resume {C} --name {TITLE}"
        )
    );
    assert_eq!(cmds[0].check(), Ok(()));
    // R13 step 7: nothing that skips Claude's trust prompt.
    assert!(
        !cmds[0]
            .argv()
            .iter()
            .any(|a| a.contains("dangerously") || a.contains("trust")),
        "{:?}",
        cmds[0].argv()
    );
    // The reply waits for the tab.
    assert!(replies(&fx).is_empty(), "{fx:?}");

    // R13a: one commit, before the window.
    let now = r.b.now().wall;
    let saved = changes(&fx);
    let Some(Change::Session(s)) = saved.first() else {
        panic!("{saved:?}")
    };
    assert_eq!(s.lifecycle, Lifecycle::Live);
    assert_eq!(s.asleep_at, None);
    assert_eq!(s.location, Location::Here);
    assert!(!s.resume_failed);
    assert_eq!(
        (s.activity, s.activity_why.as_deref()),
        (Activity::Idle, None)
    );
    assert_eq!((s.activity_at, s.last_seen_at), (Some(now), Some(now)));
    assert_eq!(s.cwd.as_deref(), Some(r.lantern().as_str()));
    let rows: Vec<&str> = saved
        .iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [format!("resumed {TITLE}")]);
    let persist = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    let window = fx
        .iter()
        .position(|e| {
            matches!(
                e,
                Effect::Tmux {
                    token: Some(TmuxToken::Open(_)),
                    ..
                }
            )
        })
        .unwrap();
    assert!(persist < window, "{fx:?}");

    // Bound: the reply names the folder it runs in, and the tab comes to the screen.
    let fx = r.bound("%7");
    assert!(
        changes(&fx).iter().any(
            |c| matches!(c, Change::PaneBinding(p) if p.session_id == s_id() && p.bound_at == now)
        ),
        "{fx:?}"
    );
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            done(format!("resumed {TITLE} in {}", r.lantern()))
        )]
    );
    assert_eq!(selects(&fx), select("%7").iter().collect::<Vec<_>>());
    assert!(!is_asleep(r.s()));
}

/// Rule 1: the store the conversation was recorded in picks the account, whatever the brain
/// opens new sessions with.
#[test]
fn resume_uses_the_recorded_store_not_the_default_account() {
    let mut w = World::new();
    w.toml = TOML.replace(r#"open_account = "acct-a""#, r#"open_account = "acct-b""#);
    let mut r = w.reconciled();
    let fx = r.launched(1, false);
    let (_, cmds) = the_open(&fx);
    let home = r.home();
    assert_eq!(
        after(&cmds[0], "-e"),
        format!("CLAUDE_CONFIG_DIR={}", home.join(".claude-a").display())
    );
    assert!(cmds[0].argv().iter().any(|a| a == "/opt/claude"));
    assert!(!cmds[0].argv().iter().any(|a| a == "/opt/claude-b"));
}

/// R32: a conversation on Claude's own store (recorded so, or with no store at all) resumes
/// with `mine`, the variable unset.
#[test]
fn a_default_store_session_resumes() {
    for recorded in [Some(".claude"), None] {
        let mut w = World::new();
        let home = w.home().to_path_buf();
        let lantern = home.join("work/lantern");
        write_transcript(&home.join(".claude"), &lantern, C);
        fs::remove_dir_all(home.join(".claude-a")).unwrap();
        w.conv().store_dir = recorded.map(|d| shown(&home.join(d)));
        let mut r = w.reconciled();
        let fx = r.launched(1, false);
        let (_, cmds) = the_open(&fx);
        let home = home.display();
        assert_eq!(
            cmds[0].line(),
            format!(
                "new-window -d -P -F 'P #{{pane_id}} #{{pane_pid}}' -n {TITLE} -c {home}/work/lantern \
                 -t =moa: -e MOA_SESSION={S} -e MOA_BRAIN=default \
                 -e CLAUDE_CODE_PLUGIN_DIRS={PLUGIN} /usr/bin/env -u CLAUDE_CONFIG_DIR \
                 /opt/claude-mine --resume {C} --name {TITLE}"
            ),
            "{recorded:?}"
        );
    }
}

/// Carried item b: a store recorded in another spelling of acct-a's folder (as hook
/// rotation records the canonical one) is still acct-a's; the `-e` value is moa.toml's.
#[test]
fn a_store_in_another_spelling_is_the_same_account() {
    fn resolve(p: &Path) -> PathBuf {
        PathBuf::from(p.display().to_string().replace("/./", "/"))
    }
    let mut w = World::new();
    w.deps.canonical = resolve;
    let other = format!("{}/./.claude-a", w.home().display());
    w.conv().store_dir = Some(other);
    let mut r = w.reconciled();
    let fx = r.launched(1, false);
    let (_, cmds) = the_open(&fx);
    assert_eq!(
        after(&cmds[0], "-e"),
        format!("CLAUDE_CONFIG_DIR={}", r.home().join(".claude-a").display())
    );
}

#[test]
fn resume_of_a_running_session_only_focuses() {
    let mut r = World::new().reconciled();
    r.launched(1, true);
    r.bound("%7");

    let fx = r.resume(2, false);
    assert!(!queried(&fx), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(selects(&fx), select("%7").iter().collect::<Vec<_>>());
    assert_eq!(replies(&fx), [(ReqId(2), done(format!("went to {TITLE}")))]);

    let fx = r.resume(3, true);
    assert!(!any_tmux(&fx), "{fx:?}");
    assert_eq!(
        replies(&fx),
        [(ReqId(3), done(format!("{TITLE} is already running")))]
    );
}

/// A resume asked while one is opening its tab joins it, rather than opening a second.
#[test]
fn a_second_resume_waits_for_the_first() {
    let mut r = World::new().reconciled();
    r.launched(1, true);
    let fx = r.resume(2, false);
    assert!(!any_tmux(&fx), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = r.bound("%7");
    let to: Vec<ReqId> = replies(&fx).into_iter().map(|(req, _)| req).collect();
    assert_eq!(to, [ReqId(1), ReqId(2)]);
    assert_eq!(selects(&fx), select("%7").iter().collect::<Vec<_>>());
}

/// Two resumes asked before either's pane query came back open one tab.
#[test]
fn two_resumes_in_flight_open_one_tab() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.resume(1, true)));
    assert!(queried(&r.resume(2, true)));
    let asked: Vec<u64> = r.st.tmux().queries().collect();
    let mut opens = 0;
    for n in asked {
        let fx = feed(
            &r.b,
            &mut r.st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Query(n),
                results: vec![Ok(vec![]), Ok(vec![])],
            },
        );
        opens += fx
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::Tmux {
                        token: Some(TmuxToken::Open(_)),
                        ..
                    }
                )
            })
            .count();
    }
    assert_eq!(opens, 1);
    let fx = r.bound("%7");
    let to: Vec<ReqId> = replies(&fx).into_iter().map(|(req, _)| req).collect();
    assert_eq!(to, [ReqId(1), ReqId(2)]);
}

fn runs_c(pid: i64, _: &[String]) -> Option<PaneAgent> {
    (pid == 808).then(|| PaneAgent {
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

/// Step 6: an agent started by hand on the conversation since the last reconcile.
#[test]
fn resume_refuses_when_an_untracked_tab_runs_it() {
    let mut w = World::new();
    w.deps.inspect = runs_c;
    let mut r = w.reconciled();
    assert!(queried(&r.resume(1, false)));
    let info = format!("I\t%8\t808\t0\t\t\t\t{}", r.lantern());
    let fx = r.listed(&[&info], &["S\t%8\tclaude"]);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(!any_tmux(&fx), "{fx:?}");
    assert_eq!(
        refusal(&fx),
        "it's already running in tab %8; adopt it with moactl adopt %8"
    );
    assert!(is_asleep(r.s()));
}

/// A failed listing doesn't hold the resume back: steps 1–5 still guard it.
#[test]
fn a_failed_listing_still_resumes() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.resume(1, false)));
    let n = r.st.tmux().queries().next().unwrap();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Err("no".into()), Ok(vec![])],
        },
    );
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Log(l) if l.level == moa_core::effect::LogLevel::Warn)),
        "{fx:?}"
    );
    the_open(&fx);
}

#[test]
fn resume_refuses_a_session_running_elsewhere() {
    let mut w = World::new();
    w.s.location = Location::Elsewhere;
    assert_eq!(
        refused_unsaved(w),
        "it's running in another app right now; close it there first"
    );
}

#[test]
fn resume_refuses_a_spike_pane_on_another_server() {
    let mut w = World::new();
    w.spike = Some(SpikePane {
        session_id: s_id(),
        pane_id: "%3".into(),
        tmux: Some("/tmp/tmux-501/other".into()),
        imported_at: UtcMs(1),
    });
    assert_eq!(
        refused_unsaved(w),
        "it's running in another moa terminal; use it there, or close it there first"
    );
}

/// Rule 3: never "the latest conversation in this folder".
#[test]
fn resume_refuses_with_no_conversation() {
    let mut w = World::new();
    w.conv = None;
    assert_eq!(
        refused_unsaved(w),
        "moa doesn't know which conversation it had; start it fresh with moactl open"
    );
}

#[test]
fn resume_refuses_a_store_no_account_has() {
    let mut w = World::new();
    let store = shown(&w.home().join(".claude-z"));
    w.conv().store_dir = Some(store);
    assert_eq!(
        refused_unsaved(w),
        "the account its conversation was recorded in isn't in moa.toml; add it there to resume"
    );
}

#[test]
fn resume_refuses_a_codex_store() {
    let mut w = World::new();
    let store = shown(&w.home().join(".codex-x"));
    w.conv().store_dir = Some(store);
    assert_eq!(
        refused_unsaved(w),
        "only Claude accounts can open sessions for now"
    );
}

#[test]
fn resume_refuses_an_account_with_no_launcher() {
    let mut w = World::new();
    let store = shown(&w.home().join(".claude-c"));
    w.conv().store_dir = Some(store);
    assert_eq!(
        refused_unsaved(w),
        "can't find claude for acct-c: set its launcher in moa.toml"
    );
}

#[test]
fn resume_refuses_a_folder_thats_gone() {
    let mut w = World::new();
    let gone = shown(&w.home().join("work/gone"));
    w.s.cwd = Some(gone.clone());
    w.s.start_cwd = Some(gone.clone());
    assert_eq!(refused_unsaved(w), format!("its folder {gone} is gone"));
}

#[test]
fn resume_refuses_another_brains_folder() {
    let mut w = World::new();
    let store = shown(&w.home().join(".claude-w"));
    w.conv().store_dir = Some(store);
    assert_eq!(
        refused_unsaved(w),
        "that folder belongs to the work brain; resume it there"
    );
}

#[test]
fn resume_refuses_a_folder_the_account_cant_use() {
    let mut w = World::new();
    let store = shown(&w.home().join(".claude-d"));
    w.conv().store_dir = Some(store);
    assert_eq!(
        refused_unsaved(w),
        "acct-d can't run sessions in that folder"
    );
}

/// Rule 2: a transcript in another account's store isn't resumed under this one.
#[test]
fn resume_refuses_a_transcript_in_another_account() {
    let w = World::new();
    let home = w.home().to_path_buf();
    fs::remove_dir_all(home.join(".claude-a/projects")).unwrap();
    write_transcript(&home.join(".claude-b"), &home.join("work/lantern"), C);
    assert_eq!(
        refused_failed(w),
        "its conversation is in acct-b's folder, not acct-a's; moa won't resume it under the wrong account"
    );
}

#[test]
fn resume_refuses_a_missing_transcript() {
    let w = World::new();
    fs::remove_dir_all(w.home().join(".claude-a/projects")).unwrap();
    assert_eq!(
        refused_failed(w),
        "its conversation isn't in acct-a's folder any more, so it can't be resumed"
    );
}

#[test]
fn resume_refuses_an_unsafe_conversation_id() {
    let mut w = World::new();
    w.conv().conversation_id = "../../etc".into();
    assert_eq!(
        refused_unsaved(w),
        "that conversation's id isn't one moa can resume"
    );
}

/// tmux reads `-c` as a format: a `#` in the folder is doubled.
#[test]
fn resume_doubles_hashes_in_the_folder() {
    let mut w = World::new();
    let folder = w.home().join("work/a#(b)");
    fs::create_dir_all(&folder).unwrap();
    w.s.cwd = Some(shown(&folder));
    w.s.start_cwd = Some(shown(&folder));
    w.conv().transcript_path = None;
    let mut r = w.reconciled();
    let fx = r.launched(1, false);
    let (_, cmds) = the_open(&fx);
    assert_eq!(
        after(&cmds[0], "-c"),
        format!("{}/work/a##(b)", r.home().display())
    );
    assert_eq!(cmds[0].check(), Ok(()));
}

/// Step 4: Claude keeps a conversation under the folder it started in. A conversation begun
/// in the folder the agent moved to belongs there, as its transcript's slot says.
#[test]
fn resume_picks_the_folder_the_transcript_belongs_to() {
    let mut w = World::new();
    let other = w.home().join("work/other");
    fs::create_dir_all(&other).unwrap();
    fs::remove_dir_all(w.home().join(".claude-a/projects")).unwrap();
    let transcript = write_transcript(&w.home().join(".claude-a"), &other, C);
    w.s.cwd = Some(shown(&other));
    w.conv().transcript_path = Some(shown(&transcript));
    let mut r = w.reconciled();
    let fx = r.launched(1, false);
    let (_, cmds) = the_open(&fx);
    assert_eq!(after(&cmds[0], "-c"), shown(&other));
    // The reply names the folder it was resumed in.
    let fx = r.bound("%7");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            done(format!("resumed {TITLE} in {}", shown(&other)))
        )]
    );
}

/// With no transcript slot to go by (or one that names neither), the start folder.
#[test]
fn resume_falls_back_to_the_start_folder() {
    let mut w = World::new();
    let other = w.home().join("work/other");
    fs::create_dir_all(&other).unwrap();
    w.s.cwd = Some(shown(&other));
    w.conv().transcript_path = Some("/elsewhere/projects/-x/c.jsonl".into());
    let mut r = w.reconciled();
    let fx = r.launched(1, false);
    let (_, cmds) = the_open(&fx);
    assert_eq!(after(&cmds[0], "-c"), r.lantern());
}

/// R13a: a tab tmux couldn't open leaves the session asleep and still resumable.
#[test]
fn a_resume_whose_tab_fails_stays_asleep() {
    let mut r = World::new().reconciled();
    r.launched(1, false);
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(s_id()),
            results: vec![Err("create window failed".into())],
        },
    );
    assert!(matches!(
        refusal(&fx).as_str(),
        "couldn't open a tab for it; see moa's log"
    ));
    let s = r.s();
    assert!(is_asleep(s), "{s:?}");
    assert_eq!(s.lifecycle, Lifecycle::Ended);
    assert!(!s.resume_failed);
}

/// R12: focusing a session that isn't running resumes it, and answers once its tab is
/// there.
#[test]
fn focus_resumes_a_sleeping_session() {
    let mut r = World::new().reconciled();
    let fx = send(&r.b, &mut r.st, 1, Op::SessionFocus { q: S.into() });
    assert!(queried(&fx), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = r.listed(&[], &[]);
    let (_, cmds) = the_open(&fx);
    assert!(cmds[0].argv().iter().any(|a| a == "--resume"));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let fx = r.bound("%7");
    assert_eq!(replies(&fx), [(ReqId(1), done(format!("went to {TITLE}")))]);
    assert_eq!(selects(&fx), select("%7").iter().collect::<Vec<_>>());
}

/// R10.
#[test]
fn resume_refuses_before_reconcile() {
    let mut r = World::new().linked();
    let fx = r.resume(1, false);
    assert!(!any_tmux(&fx), "{fx:?}");
    assert_eq!(replies(&fx), [(ReqId(1), Err(OpError(NOT_READY.into())))]);
}

/// R26: a resume whose caller left before its pane query came back opens nothing.
#[test]
fn a_resume_whose_caller_left_opens_nothing() {
    let mut r = World::new().reconciled();
    assert!(queried(&r.resume(1, false)));
    handle_checked(&mut r.st, Input::ClientGone(ConnId(1)), r.b.now());
    assert_eq!(r.st.tmux().queries().count(), 0);
    assert!(is_asleep(r.s()));
}

/// The resumed agent's first start names conversation C under S's slot: it is S's own
/// (4a's `is_own`), and applies.
#[test]
fn the_resumed_agents_start_is_its_own() {
    let mut r = World::new().reconciled();
    r.launched(1, true);
    r.bound("%7");
    let at = r.b.now().wall.0 + 1000;
    let store = shown(&r.home().join(".claude-a"));
    let fx = handle_checked(
        &mut r.st,
        Input::Hook {
            reply: None,
            envelope: HookEnvelope {
                agent: AgentKind::Claude,
                slot: Some(S.into()),
                brain: Some("default".into()),
                store_dir: Some(store),
                conversation_id: C.into(),
                kind: HookKind::SessionStarted {
                    source: StartSource::Resume,
                },
                at: UtcMs(at),
                at_ns: at * 1_000_000,
                pid: 100,
                seq: 0,
                cwd: None,
                transcript_path: None,
            },
        },
        r.b.now(),
    );
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Session(s) if s.id == s_id())),
        "{fx:?}"
    );
    assert_eq!(r.s().last_seen_at, Some(UtcMs(at)));
    let current: Vec<&str> =
        r.st.conversations_of(s_id())
            .map(|c| c.conversation_id.as_str())
            .collect();
    assert_eq!(current, [C]);
}
