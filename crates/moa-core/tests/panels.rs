//! Panels in the core: which connection is which panel (`ui.panel`), where each points
//! (`ui.here`), the `brain` and `panels` topics their views read, and `Ctrl-a $`, which opens
//! a terminal where the key was pressed (V6).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, LogLevel, Publish, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::{Activity, AgentKind, Lifecycle, Location, PaneBinding, Session, SessionId};
use moa_core::op::{ConnId, Done, Op, OpError};
use moa_core::panel::View;
use moa_core::publish::{BrainRow, FOLDED_KEY, Model, Topic, folded};
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, PaneId};
use moa_core::tmux::link::notice;
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

use common::*;

/// Brain `default` owns `~/private` and opens with `acct-a`; `acct-c` has no launcher.
const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"

[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
launcher = "/opt/claude"

[accounts.acct-c]
kind = "claude"
store = "~/.claude-c"
"#;

fn panel(pane: Option<&str>, view: Option<View>) -> Op {
    Op::UiPanel {
        pane: pane.map(str::to_string),
        view,
    }
}

fn here(folder: &str) -> Op {
    Op::UiHere {
        folder: Some(folder.into()),
    }
}

fn models(fx: &[Effect]) -> Vec<&Arc<Model>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => Some(m),
            _ => None,
        })
        .collect()
}

fn the_reply(fx: &[Effect]) -> Result<Done, OpError> {
    let r = replies(fx);
    assert_eq!(r.len(), 1, "{fx:?}");
    r[0].1.clone()
}

fn topic(m: &Model, t: Topic) -> Value {
    serde_json::to_value(&*m.topics[&t]).unwrap()
}

fn published_panels(st: &State) -> Value {
    topic(&st.model(), Topic::Panels)
}

#[test]
fn a_panel_registers_and_is_published() {
    let (b, mut st) = loaded(TOML);
    let fx = send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    let ms = models(&fx);
    assert_eq!(ms.len(), 1, "{fx:?}");
    assert_eq!(
        topic(ms[0], Topic::Panels),
        json!({"c7": {"key": "c7", "pane": "%3", "view": "tree"}})
    );
    let at_publish = fx
        .iter()
        .position(|e| matches!(e, Effect::Publish(_)))
        .unwrap();
    let at_reply = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .unwrap();
    assert!(at_publish < at_reply, "{fx:?}");
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c7", "view": "tree"})
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Effect::Persist(_))));
}

#[test]
fn a_panel_without_a_view_gets_its_panes_last_one_else_picture() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Plan)));
    handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    let fx = send_on(&b, &mut st, 8, 1, panel(Some("%3"), None));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c8", "view": "plan"})
    );
    let fx = send_on(&b, &mut st, 9, 1, panel(Some("%4"), None));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c9", "view": "picture"})
    );
}

#[test]
fn a_second_panel_on_a_pane_replaces_the_first() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    send_on(&b, &mut st, 8, 1, panel(Some("%3"), Some(View::Status)));
    assert_eq!(
        published_panels(&st),
        json!({"c8": {"key": "c8", "pane": "%3", "view": "status"}})
    );
}

#[test]
fn a_closed_connection_drops_its_panel() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    send_on(&b, &mut st, 8, 1, panel(Some("%4"), Some(View::Today)));
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    let ms = models(&fx);
    assert_eq!(ms.len(), 1, "{fx:?}");
    assert_eq!(
        topic(ms[0], Topic::Panels),
        json!({"c8": {"key": "c8", "pane": "%4", "view": "today"}})
    );
    // A connection with no panel publishes nothing when it goes.
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(9)), b.now());
    assert!(fx.is_empty(), "{fx:?}");
}

#[test]
fn ui_here_is_kept_per_panel() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    let fx = send_on(&b, &mut st, 7, 2, here("/r/work/a"));
    assert!(the_reply(&fx).is_ok(), "{fx:?}");
    assert!(models(&fx).is_empty(), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Effect::Persist(_))));
    assert_eq!(st.here_of(PaneId(3)), Some("/r/work/a"));
    assert_eq!(st.here_of(PaneId(4)), None);

    let fx = send_on(&b, &mut st, 8, 1, here("/r/work/b"));
    assert_eq!(
        the_reply(&fx),
        Err(OpError("say which panel this is first (ui.panel)".into()))
    );
    assert!(models(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_bad_pane_is_refused() {
    let (b, mut st) = loaded(TOML);
    let fx = send_on(&b, &mut st, 7, 1, panel(Some("5"), Some(View::Tree)));
    assert_eq!(
        the_reply(&fx),
        Err(OpError("a panel's tab id looks like %5".into()))
    );
    assert!(models(&fx).is_empty(), "{fx:?}");
    assert_eq!(published_panels(&st), json!({}));
}

#[test]
fn brain_topic_lists_folders_and_accounts() {
    let (b, st) = loaded(TOML);
    let row = &st.model().topics[&Topic::Brain]["brain"];
    assert_eq!(row["key"], "brain");
    let brain: BrainRow = serde_json::from_value(row.clone()).unwrap();
    assert_eq!(brain.name, "default");
    assert_eq!(
        brain.folders,
        [b.home.join("private").display().to_string()]
    );
    assert_eq!(brain.accounts, ["acct-a", "acct-b"]);
    assert_eq!(brain.open_account.as_deref(), Some("acct-a"));
    assert!(brain.folded.is_empty());
}

/// Session ids for the adopted test: A has an untracked binding, B a tracked one, C none.
const A: &str = "00000000-0000-4000-8000-0000000000a1";
const B: &str = "00000000-0000-4000-8000-0000000000a2";
const C: &str = "00000000-0000-4000-8000-0000000000a3";

fn sid(s: &str) -> SessionId {
    s.parse().unwrap()
}

fn session(id: &str, lifecycle: Lifecycle) -> Session {
    Session {
        id: sid(id),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: None,
        start_cwd: None,
        project_id: None,
        title: Some("Lantern".into()),
        title_locked: false,
        group_path: None,
        lifecycle,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(1)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: Some(UtcMs(1)),
        asleep_at: (lifecycle == Lifecycle::Ended).then_some(UtcMs(1)),
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(1)),
    }
}

fn binding(id: &str, pane: &str, tracked: bool) -> PaneBinding {
    PaneBinding {
        session_id: sid(id),
        server_pid: generation().pid,
        server_start: generation().start,
        socket: "moa".into(),
        pane_id: pane.into(),
        pane_pid: 50,
        bound_at: UtcMs(1),
        tracked,
    }
}

fn adopted(m: &Model, id: &str) -> Value {
    m.topics[&Topic::Sessions][id]["adopted"].clone()
}

#[test]
fn session_rows_say_adopted() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    // A is asleep, so its tab going changes nothing but its binding.
    snapshot.sessions.extend([
        session(A, Lifecycle::Ended),
        session(B, Lifecycle::Live),
        session(C, Lifecycle::Live),
    ]);
    snapshot
        .pane_bindings
        .extend([binding(A, "%5", false), binding(B, "%6", true)]);
    let deps = Deps {
        home: b.home.clone(),
        session_plugin: PathBuf::from(PLUGIN),
        ..Deps::default()
    };
    let mut st = loaded_with(&b, TOML, deps, snapshot);
    let m = st.model();
    assert_eq!(adopted(&m, A), json!(true));
    assert_eq!(adopted(&m, B), json!(false));
    assert_eq!(adopted(&m, C), json!(false));

    link_up(&b, &mut st);
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::Message("moa:hook pane-exited %5".into())),
    );
    assert!(st.binding(sid(A)).is_none(), "{fx:?}");
    let ms = models(&fx);
    assert!(!ms.is_empty(), "{fx:?}");
    assert_eq!(adopted(ms[ms.len() - 1], A), json!(false));
}

// --- Ctrl-a $ (V6) ---

/// `Ctrl-a $` from client `/dev/ttys1` in tab `pane`, as moa's binding sends it.
fn terminal_key(b: &TypicalBrain, st: &mut State, pane: &str) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Notification(Notification::Message(format!(
            "moa:key terminal {pane} $1 /dev/ttys1"
        ))),
    )
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

/// tmux answers query `n` with one command's result.
fn answer(
    b: &TypicalBrain,
    st: &mut State,
    n: u64,
    result: Result<Vec<String>, String>,
) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![result],
        },
    )
}

fn line(s: &str) -> Result<Vec<String>, String> {
    Ok(vec![s.to_string()])
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

fn where_is(pane: &str) -> Cmd {
    Cmd::new("display-message").args(["-p", "-t", pane, "#{pane_current_path}"])
}

/// Every command sent, in order.
fn all_cmds(fx: &[Effect]) -> Vec<Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// A key is told by notice: nothing but commands to tmux.
fn only_tmux(fx: &[Effect]) {
    assert!(
        fx.iter().all(|e| matches!(e, Effect::Tmux { .. })),
        "{fx:?}"
    );
}

fn log_lines(fx: &[Effect]) -> Vec<(LogLevel, String)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some((l.level, l.message.clone())),
            _ => None,
        })
        .collect()
}

/// `fx` opens a terminal tab `name` in `path`; once tmux answers with its pane, the tab is
/// tagged and brought to the screen, and the key's client gets the notice.
fn opens_in(b: &TypicalBrain, st: &mut State, fx: &[Effect], name: &str, path: &str) {
    only_tmux(fx);
    let (n, cmds) = the_query(fx);
    assert_eq!(cmds, [new_window(name, path)]);
    let fx = answer(b, st, n, line("P %9 777"));
    only_tmux(&fx);
    let shown = if path == b.home.display().to_string() {
        "~"
    } else {
        path
    };
    assert_eq!(
        all_cmds(&fx),
        [
            Cmd::new("set-option").args(["-p", "-t", "%9", "@moa_term", "1"]),
            Cmd::new("set-option").args(["-p", "-t", "%9", "@moa_label", name]),
            Cmd::new("select-window").args(["-t", "%9"]),
            Cmd::new("select-pane").args(["-t", "%9"]),
            notice(
                &ClientName("/dev/ttys1".into()),
                &format!("opened a terminal in {shown} (tab {name})")
            ),
        ]
    );
}

/// The link up, and conn 7 the tree panel on `%3`.
fn with_panel(b: &TypicalBrain, deps: Deps, snapshot: moa_core::state::Snapshot) -> State {
    let mut st = loaded_with(b, TOML, deps, snapshot);
    link_up(b, &mut st);
    send_on(b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    st
}

fn deps(b: &TypicalBrain) -> Deps {
    Deps {
        home: b.home.clone(),
        session_plugin: PathBuf::from(PLUGIN),
        ..Deps::default()
    }
}

fn home(b: &TypicalBrain) -> String {
    b.home.display().to_string()
}

#[test]
fn the_terminal_key_opens_in_the_panels_here() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    send_on(&b, &mut st, 7, 2, here("/r/work/a"));
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ a", "/r/work/a");
}

/// Until 5b gives a panel the agent spot's folder (V6).
#[test]
fn a_panel_with_no_here_opens_at_home() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ ~", &home(&b));
}

/// The menu's item has no pane.
#[test]
fn the_terminal_menu_item_opens_at_home() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let fx = terminal_key(&b, &mut st, "-");
    opens_in(&b, &mut st, &fx, "⌨ ~", &home(&b));
}

#[test]
fn the_terminal_key_on_an_agent_tab_uses_its_folder() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    let mut s = session(A, Lifecycle::Live);
    s.cwd = Some("/r/work/b".into());
    snapshot.sessions.push(s);
    snapshot.pane_bindings.push(binding(A, "%4", true));
    let mut st = with_panel(&b, deps(&b), snapshot);
    let fx = terminal_key(&b, &mut st, "%4");
    opens_in(&b, &mut st, &fx, "⌨ b", "/r/work/b");
}

#[test]
fn the_terminal_key_on_a_shell_asks_tmux_where_it_is() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let home = home(&b);
    let cases = [
        (line("/r/work/c"), "⌨ c", "/r/work/c"),
        (line(""), "⌨ ~", home.as_str()),
        (line("r/work/c"), "⌨ ~", home.as_str()),
        (Err("can't find pane".to_string()), "⌨ ~", home.as_str()),
    ];
    for (result, name, path) in cases {
        let fx = terminal_key(&b, &mut st, "%5");
        only_tmux(&fx);
        let (n, cmds) = the_query(&fx);
        assert_eq!(cmds, [where_is("%5")]);
        let fx = answer(&b, &mut st, n, result);
        opens_in(&b, &mut st, &fx, name, path);
    }
}

/// A `#` in a panel's folder reaches tmux doubled, in the folder and the tab's name.
#[test]
fn a_hash_in_the_panels_here_is_literal() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    send_on(&b, &mut st, 7, 2, here("/r/work/#x"));
    let fx = terminal_key(&b, &mut st, "%3");
    only_tmux(&fx);
    assert_eq!(the_query(&fx).1, [new_window("⌨ ##x", "/r/work/##x")]);
}

#[test]
fn the_terminal_key_avoids_a_missing_folder() {
    let b = TypicalBrain::new();
    let deps = Deps {
        is_dir: |p| p != Path::new("/r/gone"),
        ..deps(&b)
    };
    let mut st = with_panel(&b, deps, b.snapshot());
    send_on(&b, &mut st, 7, 2, here("/r/gone"));
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ ~", &home(&b));
}

#[test]
fn a_terminal_key_whose_answer_never_comes_opens_nothing() {
    let b = TypicalBrain::new();
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let fx = terminal_key(&b, &mut st, "%5");
    the_query(&fx);
    let fx = feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    assert!(
        !all_cmds(&fx).iter().any(|c| c.name() == "new-window"),
        "{fx:?}"
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        log_lines(&fx),
        [(
            LogLevel::Info,
            "moa's terminal went away before it said where to open a terminal".to_string()
        )]
    );
}

fn fold(path: &str, collapsed: bool) -> Op {
    Op::GroupFold {
        path: path.into(),
        collapsed,
    }
}

fn groups(st: &State) -> Vec<String> {
    st.groups().map(|g| g.path.clone()).collect()
}

fn brain_row(st: &State) -> BrainRow {
    serde_json::from_value(st.model().topics[&Topic::Brain]["brain"].clone()).unwrap()
}

#[test]
fn a_fold_is_one_meta_value() {
    let (b, mut st) = loaded(TOML);
    let before = groups(&st);
    let fx = send(&b, &mut st, 1, fold("Private/Lantern", true));
    assert_eq!(
        changes(&fx),
        [&Change::Meta {
            key: "tree.folded".into(),
            value: Some("[\"Private/Lantern\"]".into()),
        }]
    );
    assert_eq!(FOLDED_KEY, "tree.folded");
    let done = the_reply(&fx).unwrap();
    assert_eq!(done.text, "");
    assert_eq!(
        done.data,
        json!({"path": "Private/Lantern", "collapsed": true})
    );
    assert_eq!(brain_row(&st).folded, ["Private/Lantern"]);
    assert_eq!(groups(&st), before);

    let fx = send(&b, &mut st, 2, fold("Private/Lantern", false));
    assert_eq!(
        changes(&fx),
        [&Change::Meta {
            key: "tree.folded".into(),
            value: Some("[]".into()),
        }]
    );
    assert!(brain_row(&st).folded.is_empty());
}

#[test]
fn folding_loose_leaves_groups_unchanged() {
    let (b, mut st) = loaded(TOML);
    let before = groups(&st);
    let fx = send(&b, &mut st, 1, fold("Loose", true));
    assert!(
        !changes(&fx).iter().any(|c| matches!(c, Change::Group(_))),
        "{fx:?}"
    );
    assert_eq!(groups(&st), before);
    assert_eq!(folded(&st), ["Loose"]);
}

#[test]
fn folds_are_sorted_and_kept_once() {
    let (b, mut st) = loaded(TOML);
    send(&b, &mut st, 1, fold("B", true));
    send(&b, &mut st, 2, fold("A", true));
    send(&b, &mut st, 3, fold("A", true));
    assert_eq!(folded(&st), ["A", "B"]);
    assert_eq!(brain_row(&st).folded, ["A", "B"]);
}

#[test]
fn an_empty_fold_path_is_refused() {
    let (b, mut st) = loaded(TOML);
    let fx = send(&b, &mut st, 1, fold("", true));
    assert_eq!(the_reply(&fx), Err(OpError("a group needs a name".into())));
    assert!(changes(&fx).is_empty());
}

#[test]
fn a_bad_folded_value_reads_as_empty() {
    let b = TypicalBrain::new();
    let deps = Deps {
        home: b.home.clone(),
        ..Deps::default()
    };
    let mut snapshot = b.snapshot();
    snapshot.meta.push((FOLDED_KEY.into(), "not json".into()));
    let mut st = loaded_with(&b, TOML, deps, snapshot);
    assert!(folded(&st).is_empty());
    send(&b, &mut st, 1, fold("A", true));
    assert_eq!(folded(&st), ["A"]);
}
