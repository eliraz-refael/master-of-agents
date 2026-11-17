//! `open`: a fresh session in a new tab of moa's terminal, on the typical brain. The reply
//! waits for tmux to say which pane the tab has.

use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Down, Effect, Input, LogLine, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::live::{DisplayState, display_state};
use moa_core::model::{
    Activity, AgentKind, ConversationReason, Lifecycle, Location, PaneBinding, ProjectId, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

const PLUGIN: &str = "/opt/moa/plugin/session";

/// Brain `default` (folder `~/work`, opening with `acct-a`) and a `work` brain beside it.
/// `acct-b` sends everything to `work`, `acct-c` has no launcher, `acct-d` is limited to
/// `~/work`, `acct-x` is a Codex account.
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
launcher = "/opt/claude"
brain = "work"

[accounts.acct-c]
kind = "claude"
store = "~/.claude-c"

[accounts.acct-d]
kind = "claude"
store = "~/.claude-d"
launcher = "/opt/claude"
only = ["~/work"]

[accounts.acct-x]
kind = "codex"
store = "~/.codex-x"
launcher = "/opt/codex"
"#;

/// The typical brain under `toml`, its link not yet up.
fn loaded(toml: &str) -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let config = Config::parse(toml, &b.home).unwrap();
    let deps = Deps {
        session_plugin: PathBuf::from(PLUGIN),
        ..Deps::default()
    };
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(b.snapshot())), b.now());
    (b, st)
}

fn generation() -> Generation {
    Generation {
        pid: 10,
        start: 1000,
    }
}

fn link_up(b: &TypicalBrain, st: &mut State) {
    feed(
        b,
        st,
        TmuxIn::Connected {
            conn: 1,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
}

/// The typical brain under [`TOML`], linked.
fn typical() -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    (b, st)
}

fn feed(b: &TypicalBrain, st: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(st, Input::Tmux(t), b.now())
}

fn open_op(path: &Path) -> Op {
    Op::SessionOpen {
        path: path.to_path_buf(),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: false,
    }
}

fn send(b: &TypicalBrain, st: &mut State, req: u64, op: Op) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op,
        },
        b.now(),
    )
}

/// The `n`th id the default `new_uuid` hands out in a thread, at the typical now.
fn nth(b: &TypicalBrain, n: u64) -> uuid::Uuid {
    let mut bytes = [0u8; 10];
    bytes[2..].copy_from_slice(&n.to_be_bytes());
    uuid::Builder::from_unix_timestamp_millis(b.now().wall.0 as u64, &bytes).into_uuid()
}

fn changes(fx: &[Effect]) -> Vec<&Change> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn replies(fx: &[Effect]) -> Vec<(ReqId, Result<Done, OpError>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply { req, result, .. } => Some((*req, result.clone())),
            _ => None,
        })
        .collect()
}

/// The commands of the one tokened `Open`, and its session.
fn the_open(fx: &[Effect]) -> (SessionId, &[Cmd]) {
    let opens: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Open(id)),
            } => Some((*id, cmds.as_slice())),
            _ => None,
        })
        .collect();
    assert_eq!(opens.len(), 1, "{fx:?}");
    opens[0]
}

fn lantern(b: &TypicalBrain) -> PathBuf {
    b.home.join("private/lantern")
}

fn refused(b: &TypicalBrain, st: &mut State, op: Op) -> String {
    let fx = send(b, st, 1, op);
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Tmux { token: Some(_), .. })),
        "{fx:?}"
    );
    match replies(&fx).as_slice() {
        [(_, Err(e))] => e.0.clone(),
        other => panic!("one refusal, got {other:?}"),
    }
}

#[test]
fn open_sends_one_new_window_with_the_env() {
    let (b, mut st) = typical();
    let path = lantern(&b);
    let fx = send(&b, &mut st, 1, open_op(&path));
    let (id, cmds) = the_open(&fx);
    let conv = nth(&b, 2);
    assert_eq!(id, SessionId(nth(&b, 1)));
    let home = b.home.display();
    assert_eq!(cmds.len(), 1);
    assert_eq!(
        cmds[0].line(),
        format!(
            "new-window -d -P -F 'P #{{pane_id}} #{{pane_pid}}' -n lantern -c {home}/private/lantern \
             -t =moa: -e CLAUDE_CONFIG_DIR={home}/.claude-a -e MOA_SESSION={id} -e MOA_BRAIN=default \
             -e CLAUDE_CODE_PLUGIN_DIRS={PLUGIN} /opt/claude --session-id {conv} --name lantern"
        )
    );
    assert_eq!(cmds[0].check(), Ok(()));
    // Nothing is answered yet: the reply waits for the pane.
    assert!(replies(&fx).is_empty(), "{fx:?}");

    // One commit: the session, its conversation, and the `ui` row.
    let saved = changes(&fx);
    let Change::Session(s) = saved[0] else {
        panic!("{saved:?}")
    };
    let now = b.now().wall;
    assert_eq!(s.id, id);
    assert_eq!(s.agent_kind, AgentKind::Claude);
    assert_eq!(
        (s.lifecycle, s.activity, s.activity_at),
        (Lifecycle::Live, Activity::Idle, Some(now))
    );
    let shown = path.display().to_string();
    assert_eq!(s.cwd.as_deref(), Some(shown.as_str()));
    assert_eq!(s.start_cwd.as_deref(), Some(shown.as_str()));
    assert_eq!(s.account.as_deref(), Some("acct-a"));
    // Lantern's project is `<home>/private/lantern`.
    let lantern_id = st
        .projects()
        .find(|p| p.name == "Lantern")
        .map(|p| p.id)
        .unwrap();
    assert_eq!(s.project_id, Some(lantern_id));
    assert_eq!(
        (s.title.as_deref(), s.title_locked, s.group_path.as_deref()),
        (Some("lantern"), false, None)
    );
    assert_eq!(s.location, Location::Here);
    assert_eq!((s.created_at, s.last_seen_at), (now, Some(now)));
    let Change::Conversation(c) = saved[1] else {
        panic!("{saved:?}")
    };
    assert_eq!(c.conversation_id, conv.to_string());
    assert_eq!(c.session_id, id);
    assert_eq!(c.reason, ConversationReason::Start);
    assert_eq!(
        c.store_dir.as_deref(),
        Some(b.home.join(".claude-a").to_str().unwrap())
    );
    assert_eq!((c.first_seen_at, c.last_seen_at), (now, now));
    let rows: Vec<_> = saved
        .iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rows, ["opened lantern with acct-a"]);
    assert_eq!(st.session(id), Some(s));
}

#[test]
fn open_replies_after_the_pane_is_bound() {
    let (b, mut st) = typical();
    let path = lantern(&b).join("src");
    let op = Op::SessionOpen {
        path: path.clone(),
        account: Some("acct-a".into()),
        title: Some("Boards".into()),
        group: Some("Lantern/boards".into()),
        prompt: None,
        bg: true,
    };
    let fx = send(&b, &mut st, 5, op);
    let (id, cmds) = the_open(&fx);
    assert!(replies(&fx).is_empty());
    let s = st.session(id).unwrap();
    assert_eq!(
        (s.title.as_deref(), s.title_locked, s.group_path.as_deref()),
        (Some("Boards"), true, Some("Lantern/boards"))
    );
    assert!(
        cmds[0]
            .argv()
            .ends_with(&["--name".into(), "Boards".into()])
    );

    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %7 4242".into()])],
        },
    );
    let binding = PaneBinding {
        session_id: id,
        server_pid: 10,
        server_start: 1000,
        socket: "moa".into(),
        pane_id: "%7".into(),
        pane_pid: 4242,
        bound_at: b.now().wall,
    };
    assert_eq!(changes(&fx), [&Change::PaneBinding(binding)]);
    let set = Cmd::new("set-option").args(["-p", "-t", "%7", "@moa_session", &id.to_string()]);
    let at_set = fx
        .iter()
        .position(|e| {
            matches!(e, Effect::Tmux { cmds, token: None, .. } if cmds == std::slice::from_ref(&set))
        })
        .expect("the pane is tagged");
    let at_reply = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .expect("a reply");
    assert!(at_set < at_reply);
    assert_eq!(
        replies(&fx),
        [(
            ReqId(5),
            Ok(Done {
                text: format!("opened Boards with acct-a in {}", path.display()),
                data: json!({ "id": id, "title": "Boards" }),
            })
        )]
    );
    // Answered once: a second answer finds nobody waiting.
    let again = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %7 4242".into()])],
        },
    );
    assert!(replies(&again).is_empty());
    assert!(changes(&again).is_empty());
}

/// Nothing has happened in a new session that the user hasn't seen, so its first turn's end
/// shows as done until they look.
#[test]
fn an_opened_sessions_first_turn_ends_as_done() {
    let (b, mut st) = typical();
    let fx = send(&b, &mut st, 1, open_op(&lantern(&b)));
    let (id, _) = the_open(&fx);
    let now = b.now().wall;
    assert_eq!(st.session(id).unwrap().seen_at, Some(now));
    let envelope = HookEnvelope {
        agent: AgentKind::Claude,
        slot: Some(id.to_string()),
        brain: Some("default".into()),
        store_dir: None,
        conversation_id: nth(&b, 2).to_string(),
        kind: HookKind::TurnEnded {
            last_message: "Lit.".into(),
        },
        at: UtcMs(now.0 + 1000),
        at_ns: (now.0 + 1000) * 1_000_000,
        pid: 100,
        seq: 0,
        cwd: None,
        transcript_path: None,
    };
    handle_checked(
        &mut st,
        Input::Hook {
            reply: None,
            envelope,
        },
        b.now(),
    );
    let s = st.session(id).unwrap();
    assert_eq!(s.last_turn_end_at, Some(UtcMs(now.0 + 1000)));
    assert_eq!(
        display_state(s, UtcMs(now.0 + 2000)),
        (DisplayState::Done, "Lit.".to_string())
    );
}

#[test]
fn open_refuses_without_a_terminal() {
    let (b, mut st) = loaded(TOML);
    assert_eq!(
        refused(&b, &mut st, open_op(&lantern(&b))),
        "moa isn't open in a terminal yet; run moa first"
    );
}

#[test]
fn open_refuses_without_an_account() {
    let (b, mut st) = loaded(
        r#"
[brains.default]
folders = ["~/work"]
[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#,
    );
    link_up(&b, &mut st);
    assert_eq!(
        refused(&b, &mut st, open_op(&lantern(&b))),
        "no account to open with: set open_account in moa.toml"
    );
}

fn with_account(path: &Path, account: &str) -> Op {
    let Op::SessionOpen {
        path,
        title,
        group,
        prompt,
        bg,
        ..
    } = open_op(path)
    else {
        unreachable!()
    };
    Op::SessionOpen {
        path,
        account: Some(account.into()),
        title,
        group,
        prompt,
        bg,
    }
}

#[test]
fn open_refuses_an_unknown_account() {
    let (b, mut st) = typical();
    for name in ["acct-z", "not an account!"] {
        assert_eq!(
            refused(&b, &mut st, with_account(&lantern(&b), name)),
            format!("there's no account called {name}")
        );
    }
}

#[test]
fn open_refuses_a_codex_account() {
    let (b, mut st) = typical();
    assert_eq!(
        refused(&b, &mut st, with_account(&lantern(&b), "acct-x")),
        "only Claude accounts can open sessions for now"
    );
}

#[test]
fn open_refuses_without_a_launcher() {
    let (b, mut st) = typical();
    assert_eq!(
        refused(&b, &mut st, with_account(&lantern(&b), "acct-c")),
        "can't find claude for acct-c: set its launcher in moa.toml"
    );
}

#[test]
fn open_refuses_another_brains_folder() {
    let (b, mut st) = typical();
    assert_eq!(
        refused(&b, &mut st, with_account(&lantern(&b), "acct-b")),
        "that folder belongs to the work brain; open it there"
    );
}

#[test]
fn open_refuses_a_folder_the_account_cant_use() {
    let (b, mut st) = typical();
    assert_eq!(
        refused(&b, &mut st, with_account(&lantern(&b), "acct-d")),
        "acct-d can't open sessions in that folder"
    );
    // Inside its folder it opens.
    let fx = send(
        &b,
        &mut st,
        1,
        with_account(&b.home.join("work/x"), "acct-d"),
    );
    the_open(&fx);
}

#[test]
fn open_refuses_a_relative_folder() {
    let (b, mut st) = typical();
    assert_eq!(
        refused(&b, &mut st, open_op(Path::new("private/lantern"))),
        "open needs a full path"
    );
}

fn log_lines(fx: &[Effect]) -> Vec<&LogLine> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l),
            _ => None,
        })
        .collect()
}

#[test]
fn a_failed_new_window_ends_the_session() {
    for (results, logged) in [
        (
            vec![Err("create window failed: index in use\nmore".to_string())],
            "create window failed: index in use",
        ),
        (vec![Ok(vec!["nonsense".to_string()])], "didn't parse"),
        (vec![Ok(vec![])], "didn't parse"),
    ] {
        let (b, mut st) = typical();
        let fx = send(&b, &mut st, 3, open_op(&lantern(&b)));
        let (id, _) = the_open(&fx);
        let fx = feed(
            &b,
            &mut st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Open(id),
                results,
            },
        );
        let s = st.session(id).unwrap();
        assert_eq!((s.lifecycle, s.resume_failed), (Lifecycle::Ended, true));
        assert!(
            changes(&fx)
                .iter()
                .any(|c| matches!(c, Change::Session(s) if s.id == id))
        );
        assert!(
            !changes(&fx)
                .iter()
                .any(|c| matches!(c, Change::PaneBinding(_)))
        );
        assert_eq!(
            replies(&fx),
            [(
                ReqId(3),
                Err(OpError("couldn't open a tab for it; see moa's log".into()))
            )]
        );
        let logs = log_lines(&fx);
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(logs[0].message.contains(logged), "{logs:?}");
        assert!(logs[0].message.contains(&id.to_string()), "{logs:?}");
        // Only tmux's first line, and no record contents.
        assert!(!logs[0].message.contains("more"), "{logs:?}");
        assert!(!logs[0].message.contains("lantern"), "{logs:?}");
    }
}

#[test]
fn the_link_going_down_fails_waiting_opens() {
    for down in [Down::Lost, Down::NoServer, Down::GaveUp] {
        let (b, mut st) = typical();
        let one = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
        let two = the_open(&send(&b, &mut st, 2, open_op(&b.home.join("work/x")))).0;
        let fx = feed(&b, &mut st, TmuxIn::Down(down));
        let gone = Err(OpError("moa's terminal went away before it opened".into()));
        let mut got = replies(&fx);
        got.sort_by_key(|(r, _)| r.0);
        assert_eq!(got, [(ReqId(1), gone.clone()), (ReqId(2), gone)]);
        for id in [one, two] {
            assert_eq!(st.session(id).unwrap().lifecycle, Lifecycle::Ended);
        }
        // A late answer from the old connection changes nothing.
        let late = feed(
            &b,
            &mut st,
            TmuxIn::Replies {
                conn: 1,
                token: TmuxToken::Open(one),
                results: vec![Ok(vec!["P %7 4242".into()])],
            },
        );
        assert!(changes(&late).is_empty());
        assert!(replies(&late).is_empty());
    }
}

/// A connection replaced without a `Down` between can't answer what the old one was asked.
#[test]
fn a_new_connection_fails_waiting_opens() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Connected {
            conn: 2,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-10".into()),
            socket: "moa".into(),
        },
    );
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError("moa's terminal went away before it opened".into()))
        )]
    );
    assert_eq!(st.session(id).unwrap().lifecycle, Lifecycle::Ended);
}

/// A reload after the open's commit failed in the store: the session isn't in the brain, so
/// its tab is not bound.
#[test]
fn a_tab_for_a_session_the_brain_lost_is_not_bound() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 4, open_op(&lantern(&b)))).0;
    handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(b.snapshot()),
            refuse_writes: None,
        },
        b.now(),
    );
    assert!(st.session(id).is_none());
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %7 4242".into()])],
        },
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Effect::Tmux { token: None, cmds, .. }
        if cmds.iter().any(|c| c.name() == "set-option" && c.argv().contains(&"@moa_session".to_string())))));
    assert_eq!(
        replies(&fx),
        [(
            ReqId(4),
            Err(OpError("couldn't open a tab for it; see moa's log".into()))
        )]
    );
    let logs = log_lines(&fx);
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(logs[0].message.contains(&id.to_string()), "{logs:?}");
}

#[test]
fn the_window_name_is_cut_to_24_chars() {
    let (b, mut st) = typical();
    let title = "Ångström läntern rewrite — phase two";
    let op = Op::SessionOpen {
        path: lantern(&b),
        account: None,
        title: Some(title.into()),
        group: None,
        prompt: Some("look at\nthe 'boards'".into()),
        bg: false,
    };
    let fx = send(&b, &mut st, 1, op);
    let (_, cmds) = the_open(&fx);
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "-n").unwrap();
    assert_eq!(argv[at + 1], "Ångström läntern rewrite");
    assert_eq!(argv[at + 1].chars().count(), 24);
    // The whole title goes to Claude, and the prompt after `--`.
    assert!(argv.ends_with(&[
        "--name".into(),
        title.into(),
        "--".into(),
        "look at\nthe 'boards'".into()
    ]));
    assert_eq!(cmds[0].check(), Ok(()));
    assert!(!cmds[0].line().contains('\n'));
}

/// tmux expands `-n` and `-c` as formats, so their `#` is doubled: `#S` would show the
/// session's name and `#(…)` would run a command. The name is cut first, so a doubled `#`
/// at the cut stays whole. What Claude gets and what the brain records stay as typed.
#[test]
fn the_tab_name_and_folder_keep_their_hashes_literal() {
    let (b, mut st) = typical();
    // The 24th character is a `#`.
    let title = "fix #S #(x) aaaaaaaaaaa#b";
    let path = b.home.join("work/d#x#{s}");
    let op = Op::SessionOpen {
        path: path.clone(),
        account: None,
        title: Some(title.into()),
        group: None,
        prompt: None,
        bg: false,
    };
    let fx = send(&b, &mut st, 1, op);
    let (id, cmds) = the_open(&fx);
    let conv = nth(&b, 2);
    let home = b.home.display();
    assert_eq!(
        cmds[0].line(),
        format!(
            "new-window -d -P -F 'P #{{pane_id}} #{{pane_pid}}' -n 'fix ##S ##(x) aaaaaaaaaaa##' \
             -c '{home}/work/d##x##{{s}}' -t =moa: -e CLAUDE_CONFIG_DIR={home}/.claude-a \
             -e MOA_SESSION={id} -e MOA_BRAIN=default -e CLAUDE_CODE_PLUGIN_DIRS={PLUGIN} \
             /opt/claude --session-id {conv} --name 'fix #S #(x) aaaaaaaaaaa#b'"
        )
    );
    assert_eq!(cmds[0].check(), Ok(()));
    let s = st.session(id).unwrap();
    assert_eq!(s.title.as_deref(), Some(title));
    assert_eq!(s.cwd.as_deref(), path.to_str());
    assert_eq!(s.start_cwd.as_deref(), path.to_str());
}

#[test]
fn a_folder_with_no_name_opens_as_agent() {
    let (b, mut st) = typical();
    let fx = send(&b, &mut st, 1, open_op(Path::new("/")));
    let (id, cmds) = the_open(&fx);
    assert_eq!(st.session(id).unwrap().title.as_deref(), Some("agent"));
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "-n").unwrap();
    assert_eq!(argv[at + 1], "agent");
    assert_eq!(st.session(id).unwrap().project_id, None::<ProjectId>);
}
