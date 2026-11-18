//! `open`: a fresh session in a new tab of moa's terminal, on the typical brain. The reply
//! waits for tmux to say which pane the tab has.

mod common;

use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, LogLine, TmuxIn, TmuxToken, handle_checked};
use moa_core::hook::{HookEnvelope, HookKind};
use moa_core::live::{DisplayState, display_state, is_asleep};
use moa_core::model::{
    Activity, AgentKind, ConversationReason, Lifecycle, Location, PaneBinding, ProjectId, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::State;
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

use common::*;

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

/// The typical brain under [`TOML`], linked.
fn typical() -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    (b, st)
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

/// The `n`th id the default `new_uuid` hands out in a thread, at the typical now.
fn nth(b: &TypicalBrain, n: u64) -> uuid::Uuid {
    let mut bytes = [0u8; 10];
    bytes[2..].copy_from_slice(&n.to_be_bytes());
    uuid::Builder::from_unix_timestamp_millis(b.now().wall.0 as u64, &bytes).into_uuid()
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
        tracked: true,
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

/// A hook event for an opened session, `plus_ms` after now.
fn opened_hook(
    b: &TypicalBrain,
    id: SessionId,
    conv: &str,
    seq: u32,
    plus_ms: i64,
    kind: HookKind,
) -> Input {
    let at = b.now().wall.0 + plus_ms;
    Input::Hook {
        reply: None,
        envelope: HookEnvelope {
            agent: AgentKind::Claude,
            slot: Some(id.to_string()),
            brain: Some("default".into()),
            store_dir: None,
            conversation_id: conv.into(),
            kind,
            at: UtcMs(at),
            at_ns: at * 1_000_000,
            pid: 100,
            seq,
            cwd: None,
            transcript_path: None,
        },
    }
}

/// A launcher that ignores `--session-id` starts under an id of its own: the session's first
/// start is still its agent's. Once one has started, another start under a new id is a
/// second agent inside the tab, and is dropped.
#[test]
fn a_first_start_under_another_id_is_the_tabs_own_agent() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
    bound(&b, &mut st, id, "%7");
    let own = "d1e2f3a4-0000-4000-8000-000000000001";
    let start = HookKind::SessionStarted {
        source: moa_core::hook::StartSource::Startup,
    };
    handle_checked(
        &mut st,
        opened_hook(&b, id, own, 1, 1000, start.clone()),
        b.now(),
    );
    assert_eq!(st.conversations_of(id).count(), 2);
    let prompt = HookKind::TurnStarted {
        prompt: "light it".into(),
    };
    handle_checked(&mut st, opened_hook(&b, id, own, 2, 2000, prompt), b.now());
    assert_eq!(st.session(id).unwrap().activity, Activity::Working);

    let nested = "d1e2f3a4-0000-4000-8000-000000000002";
    let fx = handle_checked(
        &mut st,
        opened_hook(&b, id, nested, 3, 3000, start),
        b.now(),
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.conversations_of(id).count(), 2);
    assert_eq!(st.session(id).unwrap().activity, Activity::Working);
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

/// A launch keeps running when its caller goes: the tab is still bound and tagged, and only
/// the reply is dropped.
#[test]
fn a_closed_connections_open_still_binds() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(1)), b.now());
    assert!(fx.is_empty(), "{fx:?}");
    let fx = bound(&b, &mut st, id, "%7");
    let binding = PaneBinding {
        session_id: id,
        server_pid: 10,
        server_start: 1000,
        socket: "moa".into(),
        pane_id: "%7".into(),
        pane_pid: 4242,
        bound_at: b.now().wall,
        tracked: true,
    };
    assert_eq!(changes(&fx), [&Change::PaneBinding(binding)]);
    let set = Cmd::new("set-option").args(["-p", "-t", "%7", "@moa_session", &id.to_string()]);
    assert!(
        fx.iter().any(
            |e| matches!(e, Effect::Tmux { cmds, token: None, .. } if cmds == std::slice::from_ref(&set))
        ),
        "the pane is tagged: {fx:?}"
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.session(id).unwrap().lifecycle, Lifecycle::Live);
}

/// A reload after the open's commit failed in the store: the session isn't in the brain, and
/// its new window was never sent (it waits for the commit). The open is answered then, and a
/// late answer about a tab finds nobody waiting. An open whose session was saved still waits.
#[test]
fn a_reload_that_lost_an_opens_session_answers_it() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 4, open_op(&lantern(&b)))).0;
    let kept = the_open(&send(&b, &mut st, 5, open_op(&lantern(&b)))).0;
    let mut snapshot = b.snapshot();
    snapshot.sessions.push(st.session(kept).unwrap().clone());
    let fx = handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: None,
        },
        b.now(),
    );
    assert!(st.session(id).is_none());
    assert_eq!(
        replies(&fx),
        [(
            ReqId(4),
            Err(OpError(
                "moa couldn't save that session; nothing was opened, try again".into()
            ))
        )]
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
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Effect::Tmux { token: None, cmds, .. }
        if cmds.iter().any(|c| c.name() == "set-option" && c.argv().contains(&"@moa_session".to_string())))));

    let fx = bound(&b, &mut st, kept, "%8");
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::PaneBinding(p) if p.session_id == kept)),
        "{fx:?}"
    );
    assert!(
        matches!(replies(&fx).as_slice(), [(ReqId(5), Ok(_))]),
        "{fx:?}"
    );
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

fn pane_exited(b: &TypicalBrain, st: &mut State, pane: &str) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Notification(Notification::Message(format!(
            "moa:hook pane-exited {pane}"
        ))),
    )
}

fn bound(b: &TypicalBrain, st: &mut State, id: SessionId, pane: &str) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec![format!("P {pane} 4242")])],
        },
    )
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

/// An agent that exits at once (a bad launcher, a refused flag) dies before the link's
/// topology has its pane: the exit, with no `SessionEnd` before it, still puts the session
/// to sleep, trusted on the binding's generation alone.
#[test]
fn a_pane_that_exits_before_the_topology_has_it_puts_its_session_to_sleep() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
    bound(&b, &mut st, id, "%7");
    // No refresh in between: the topology has never listed %7.
    let fx = pane_exited(&b, &mut st, "%7");
    assert!(is_asleep(st.session(id).unwrap()));
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(s) if *s == id)),
        "{fx:?}"
    );
    assert!(activity_rows(&fx).is_empty(), "{fx:?}");
}

/// The other order: the exit is seen before the open's answer binds the pane. The answer
/// puts the session to sleep instead of binding it, and the reply is the same as when the
/// exit comes after it.
#[test]
fn a_pane_that_exits_before_the_open_is_answered_puts_its_session_to_sleep() {
    let (b, mut st) = typical();
    let path = lantern(&b);
    let id = the_open(&send(&b, &mut st, 6, open_op(&path))).0;
    let early = pane_exited(&b, &mut st, "%7");
    assert!(changes(&early).is_empty(), "{early:?}");
    assert_eq!(st.session(id).unwrap().lifecycle, Lifecycle::Live);

    let fx = bound(&b, &mut st, id, "%7");
    assert!(is_asleep(st.session(id).unwrap()));
    let saved = changes(&fx);
    assert!(
        !saved.iter().any(|c| matches!(c, Change::PaneBinding(_))),
        "{fx:?}"
    );
    assert!(activity_rows(&fx).is_empty(), "{fx:?}");
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Tmux { token: None, cmds, .. }
            if cmds.iter().any(|c| c.argv().iter().any(|a| a == "@moa_session")))),
        "{fx:?}"
    );
    assert_eq!(
        replies(&fx),
        [(
            ReqId(6),
            Ok(Done {
                text: format!("opened lantern with acct-a in {}", path.display()),
                data: json!({ "id": id, "title": "lantern" }),
            })
        )]
    );
}

/// A binding from an earlier server run names a pane id the new server may reuse: its
/// exit is not this session's.
#[test]
fn a_pane_exit_for_an_older_generations_binding_is_ignored() {
    let (b, mut st) = typical();
    let id = the_open(&send(&b, &mut st, 1, open_op(&lantern(&b)))).0;
    bound(&b, &mut st, id, "%7");
    feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    feed(
        &b,
        &mut st,
        TmuxIn::Connected {
            conn: 2,
            generation: Generation {
                pid: 11,
                start: 2000,
            },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    // The new server has a %7 of its own, in a window: only the generation tells them apart.
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 2,
            token: TmuxToken::Refresh,
            results: vec![
                lines(&["S $0 moa"]),
                lines(&["P %7 @1 $0 1 1 0 0 "]),
                lines(&["C 0 $0 /dev/ttys1"]),
            ],
        },
    );
    let fx = pane_exited(&b, &mut st, "%7");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.session(id).unwrap().lifecycle, Lifecycle::Live);
}
