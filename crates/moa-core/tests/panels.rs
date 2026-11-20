//! Panels in the core: which connection is which panel (`ui.panel`), where each points
//! (`ui.here`), the `brain` and `panels` topics their views read, and `Ctrl-a $`, which opens
//! a terminal where the key was pressed (V6).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moa_core::UtcMs;
use moa_core::change::Change;
use moa_core::cockpit::spawn_tags;
use moa_core::effect::{Down, Effect, Input, LogLevel, Publish, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::{Activity, AgentKind, Lifecycle, Location, PaneBinding, Session, SessionId};
use moa_core::op::{ConnId, Done, Op, OpError};
use moa_core::panel::View;
use moa_core::publish::{BrainRow, FOLDED_KEY, Model, Topic, folded};
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId};
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
        placed: false,
        once: false,
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
        json!({"c7": {"key": "c7", "pane": "%3", "view": "tree", "managed": false}})
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
        json!({"key": "c7", "view": "tree", "managed": false})
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
        json!({"key": "c8", "view": "plan", "managed": false})
    );
    let fx = send_on(&b, &mut st, 9, 1, panel(Some("%4"), None));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c9", "view": "picture", "managed": false})
    );
}

fn connected(conn: u64, generation: Generation) -> TmuxIn {
    TmuxIn::Connected {
        conn,
        generation,
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
        socket: "moa".into(),
    }
}

/// Another tmux server's `%3` is another tab: it doesn't get the old `%3`'s view.
#[test]
fn another_server_forgets_the_tabs_views() {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Plan)));
    handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    // The same server, linked again: `%3` keeps its view.
    feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    feed(&b, &mut st, connected(2, generation()));
    let fx = send_on(&b, &mut st, 8, 1, panel(Some("%3"), None));
    assert_eq!(the_reply(&fx).unwrap().data["view"], "plan");
    handle_checked(&mut st, Input::ClientGone(ConnId(8)), b.now());
    // A new server.
    feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    let other = Generation {
        pid: 11,
        start: 2000,
    };
    feed(&b, &mut st, connected(3, other));
    let fx = send_on(&b, &mut st, 9, 1, panel(Some("%3"), None));
    assert_eq!(the_reply(&fx).unwrap().data["view"], "picture");
}

#[test]
fn a_second_panel_on_a_pane_replaces_the_first() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    send_on(&b, &mut st, 8, 1, panel(Some("%3"), Some(View::Status)));
    assert_eq!(
        published_panels(&st),
        json!({"c8": {"key": "c8", "pane": "%3", "view": "status", "managed": false}})
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
        json!({"c8": {"key": "c8", "pane": "%4", "view": "today", "managed": false}})
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

/// A brain with sessions A, B and C bound as their ids' comment says.
fn adopted_brain() -> (TypicalBrain, State) {
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
    let st = loaded_with(&b, TOML, deps, snapshot);
    (b, st)
}

#[test]
fn session_rows_say_adopted() {
    let (b, mut st) = adopted_brain();
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

/// The `adopted` flag rides on the session rows, and the `R` key and its hint read it: a
/// change to a binding alone, with no change to its session, still republishes `sessions`.
#[test]
fn a_binding_change_alone_republishes_sessions() {
    for change in [
        Change::PaneBinding(binding(A, "%5", false)),
        Change::DeletePaneBinding(sid(A)),
    ] {
        assert!(change.topics().contains(&Topic::Sessions), "{change:?}");
    }

    let (b, mut st) = adopted_brain();
    assert_eq!(adopted(&st.model(), A), json!(true));
    link_up(&b, &mut st);
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::Message("moa:hook pane-exited %5".into())),
    );
    let changed = changes(&fx);
    assert!(
        changed
            .iter()
            .any(|c| matches!(c, Change::DeletePaneBinding(id) if *id == sid(A))),
        "{fx:?}"
    );
    assert!(
        !changed.iter().any(|c| matches!(c, Change::Session(_))),
        "only the binding changes: {fx:?}"
    );
    let published = models(&fx)
        .into_iter()
        .any(|m| adopted(m, A) == json!(false));
    assert!(published, "sessions wasn't republished: {fx:?}");
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
/// tagged and brought to the screen, and the key's client gets the notice. The `terms`
/// topic has it at once (published unless it already had it so).
fn opens_in(b: &TypicalBrain, st: &mut State, fx: &[Effect], name: &str, path: &str) {
    only_tmux(fx);
    let (n, cmds) = the_query(fx);
    assert_eq!(cmds, [new_window(name, path)]);
    let fx = answer(b, st, n, line("P %9 777"));
    let (published, fx): (Vec<_>, Vec<_>) = fx
        .into_iter()
        .partition(|e| matches!(e, Effect::Publish(_)));
    assert!(published.len() <= 1, "{published:?}");
    assert_eq!(
        st.model().topics[&Topic::Terms]["%9"]["name"],
        name.trim_start_matches("⌨ ")
    );
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

/// A panel with no folder of its own opens a terminal in the folder of the agent in the
/// spot, else at home (L11).
#[test]
fn the_terminal_key_falls_back_to_the_spots_folder() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    let mut s = session(A, Lifecycle::Live);
    s.cwd = Some("/r/work/a".into());
    snapshot.sessions.push(s);
    snapshot.pane_bindings.push(binding(A, "%7", true));
    // The tree `%3` in the cockpit, beside Lantern's agent `%7` in the spot.
    let tree = "P %3 @1 $0 1 1 0 1  0 tree".to_string();
    let spot = format!("P %7 @1 $0 1 1 0 1 {A} 0 agent");
    let mut st = with_panel(&b, deps(&b), snapshot);
    link_listed(&b, &mut st, &[tree.clone(), spot]);
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ a", "/r/work/a");

    // The empty slot in the spot: home.
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let slot = "P %8 @1 $0 1 1 0 1  0 empty-slot".to_string();
    link_listed(&b, &mut st, &[tree, slot]);
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ ~", &home(&b));

    // No cockpit: home.
    let mut st = with_panel(&b, deps(&b), b.snapshot());
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ ~", &home(&b));
}

/// A stale `agent` tag on a pane outside the cockpit (left by a chain that stopped half
/// way) never picks the folder: only the cockpit's spot does.
#[test]
fn a_stale_agent_tag_outside_the_cockpit_never_picks_the_folder() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    for (id, cwd) in [(A, "/r/work/a"), (B, "/r/work/b")] {
        let mut s = session(id, Lifecycle::Live);
        s.cwd = Some(cwd.into());
        snapshot.sessions.push(s);
    }
    snapshot
        .pane_bindings
        .extend([binding(A, "%7", true), binding(B, "%6", true)]);
    // B's tab `%6`, in window `@0` (listed first), still tagged `agent`.
    let stale = format!("P %6 @0 $0 1 0 0 0 {B} 0 agent");
    let tree = "P %3 @1 $0 1 1 0 1  0 tree".to_string();

    // Lantern in the spot: its folder.
    let spot = format!("P %7 @1 $0 1 1 0 1 {A} 0 agent");
    let mut st = with_panel(&b, deps(&b), snapshot.clone());
    link_listed(&b, &mut st, &[stale.clone(), tree.clone(), spot]);
    let fx = terminal_key(&b, &mut st, "%3");
    opens_in(&b, &mut st, &fx, "⌨ a", "/r/work/a");

    // The empty slot in the spot: home.
    let slot = "P %8 @1 $0 1 1 0 1  0 empty-slot".to_string();
    let mut st = with_panel(&b, deps(&b), snapshot);
    link_listed(&b, &mut st, &[stale, tree, slot]);
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

/// An agent tab whose session has no folder opens where tmux says the tab is.
#[test]
fn an_agent_tab_with_no_folder_asks_tmux_where_it_is() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    let mut s = session(A, Lifecycle::Live);
    s.cwd = None;
    snapshot.sessions.push(s);
    snapshot.pane_bindings.push(binding(A, "%4", true));
    let mut st = with_panel(&b, deps(&b), snapshot);
    let fx = terminal_key(&b, &mut st, "%4");
    only_tmux(&fx);
    let (n, cmds) = the_query(&fx);
    assert_eq!(cmds, [where_is("%4")]);
    let fx = answer(&b, &mut st, n, line("/r/work/d"));
    opens_in(&b, &mut st, &fx, "⌨ d", "/r/work/d");
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

/// Folding what is folded, or unfolding what isn't, saves nothing.
#[test]
fn a_fold_already_so_saves_nothing() {
    let (b, mut st) = loaded(TOML);
    let fx = send(&b, &mut st, 1, fold("A", false));
    assert!(the_reply(&fx).is_ok());
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Persist(_))),
        "{fx:?}"
    );
    send(&b, &mut st, 2, fold("A", true));
    let fx = send(&b, &mut st, 3, fold("A", true));
    assert!(the_reply(&fx).is_ok());
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Persist(_))),
        "{fx:?}"
    );
}

/// A renamed group keeps its fold, and its subgroups' folds, under the new name, in the
/// rename's own commit; a new group with the old name starts unfolded.
#[test]
fn a_rename_moves_its_folds() {
    let (b, mut st) = loaded(TOML);
    send(&b, &mut st, 1, Op::GroupAdd { path: "A/x".into() });
    send(&b, &mut st, 2, fold("A", true));
    send(&b, &mut st, 3, fold("A/x", true));
    send(&b, &mut st, 4, fold("AB", true));
    let fx = send(
        &b,
        &mut st,
        5,
        Op::GroupRename {
            old: "A".into(),
            new: "B".into(),
        },
    );
    assert!(the_reply(&fx).is_ok(), "{fx:?}");
    assert_eq!(
        fx.iter()
            .filter(|e| matches!(e, Effect::Persist(_)))
            .count(),
        1,
        "{fx:?}"
    );
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::Meta { key, .. } if key == FOLDED_KEY)),
        "{fx:?}"
    );
    assert_eq!(folded(&st), ["AB", "B", "B/x"]);
    assert_eq!(brain_row(&st).folded, ["AB", "B", "B/x"]);
    send(&b, &mut st, 6, Op::GroupAdd { path: "A".into() });
    assert!(!folded(&st).contains(&"A".to_string()));
}

/// A removed group's folds, and its subgroups', go with it.
#[test]
fn a_removed_group_drops_its_folds() {
    let (b, mut st) = loaded(TOML);
    send(&b, &mut st, 1, Op::GroupAdd { path: "A/x".into() });
    send(&b, &mut st, 2, fold("A", true));
    send(&b, &mut st, 3, fold("A/x", true));
    let fx = send(&b, &mut st, 4, Op::GroupRm { path: "A".into() });
    assert!(the_reply(&fx).is_ok(), "{fx:?}");
    assert_eq!(
        fx.iter()
            .filter(|e| matches!(e, Effect::Persist(_)))
            .count(),
        1,
        "{fx:?}"
    );
    assert!(folded(&st).is_empty());
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

// --- Panels follow moa's view (L8) ---

/// `ui.panel` from a panel moa placed itself (`MOA_NO_AUTOSTART`), or not.
fn placed_panel(pane: &str, view: View, placed: bool) -> Op {
    Op::UiPanel {
        pane: Some(pane.to_string()),
        view: Some(view),
        placed,
        once: false,
    }
}

fn select_view(pane: &str, view: View) -> Op {
    Op::UiSelectView {
        pane: pane.to_string(),
        view,
    }
}

/// One `list-panes` line: `%id` in window `@window` of session `$session` (`$0` is `moa`,
/// `$1` is `_moa`), outside the cockpit, alive, tagged with `view` (empty: none).
fn pane_line(id: u32, window: u32, session: u32, view: &str) -> String {
    format!("P %{id} @{window} ${session} 1 1 0 0  0 {view}")
}

/// The three lists: sessions `moa` and `_moa`, `panes`, and moa's control client.
fn lists(panes: &[String]) -> Vec<Result<Vec<String>, String>> {
    vec![
        Ok(vec!["S $0 moa".to_string(), "S $1 _moa".to_string()]),
        Ok(panes.to_vec()),
        Ok(vec!["C 1 $1 client-9".to_string()]),
        Ok(Vec::new()),
    ]
}

/// The link's own listing comes back with `panes`.
fn link_listed(b: &TypicalBrain, st: &mut State, panes: &[String]) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(panes),
        },
    )
}

/// tmux answers query `n` with the three lists, `panes` among them.
fn answer_lists(b: &TypicalBrain, st: &mut State, n: u64, panes: Vec<String>) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: lists(&panes),
        },
    )
}

/// `%3` tagged `status` and `%9` tagged `tree`, parked; `%4`, an untagged shell in `moa`;
/// `%6`, an untagged pane in `_moa`.
fn the_panes() -> Vec<String> {
    vec![
        pane_line(3, 103, 1, "status"),
        pane_line(9, 109, 1, "tree"),
        pane_line(4, 4, 0, ""),
        pane_line(6, 106, 1, ""),
    ]
}

/// The link up, its first listing showing [`the_panes`].
fn listed_up() -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    link_listed(&b, &mut st, &the_panes());
    (b, st)
}

/// The last `panels` published in `fx`.
fn last_panels(fx: &[Effect]) -> Value {
    let ms = models(fx);
    let m = ms
        .last()
        .unwrap_or_else(|| panic!("nothing published: {fx:?}"));
    topic(m, Topic::Panels)
}

/// The one tokened query in `fx`: its number and its commands, each as one line.
fn the_ask(fx: &[Effect]) -> (u64, Vec<String>) {
    let (n, cmds) = the_query(fx);
    let lines = cmds
        .iter()
        .flat_map(|c| c.parts().to_vec())
        .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
        .collect();
    (n, lines)
}

#[test]
fn the_tag_wins_over_the_view_a_panel_asks_for() {
    let (b, mut st) = listed_up();
    let fx = send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c7", "view": "status", "managed": true})
    );
    let fx = send_on(&b, &mut st, 8, 1, panel(Some("%4"), Some(View::Tree)));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c8", "view": "tree", "managed": false})
    );
    assert_eq!(
        published_panels(&st),
        json!({
            "c7": {"key": "c7", "pane": "%3", "view": "status", "managed": true},
            "c8": {"key": "c8", "pane": "%4", "view": "tree", "managed": false},
        })
    );
}

#[test]
fn a_listing_corrects_a_panel_that_registered_too_early() {
    let (b, mut st) = loaded(TOML);
    let fx = send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c7", "view": "tree", "managed": false})
    );
    link_up(&b, &mut st);
    let fx = link_listed(&b, &mut st, &the_panes());
    assert_eq!(
        last_panels(&fx),
        json!({"c7": {"key": "c7", "pane": "%3", "view": "status", "managed": true}})
    );
    // A listing that changes nothing publishes nothing.
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::SessionsChanged),
    );
    let fx = link_listed(&b, &mut st, &the_panes());
    assert!(models(&fx).is_empty(), "{fx:?}");
    // One that shows the tag gone: the panel is its own again, with the view it has.
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::SessionsChanged),
    );
    let fx = link_listed(&b, &mut st, &[pane_line(3, 103, 1, "")]);
    assert_eq!(
        last_panels(&fx),
        json!({"c7": {"key": "c7", "pane": "%3", "view": "status", "managed": false}})
    );
}

#[test]
fn select_view_retags_and_publishes() {
    let (b, mut st) = listed_up();
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    let fx = send_on(&b, &mut st, 7, 2, select_view("%3", View::Plan));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let (n, asked) = the_ask(&fx);
    assert_eq!(asked.len(), 4, "a listing: {asked:?}");
    assert!(asked[1].starts_with("list-panes"), "{asked:?}");

    let fx = answer_lists(&b, &mut st, n, the_panes());
    let (n, chain) = the_ask(&fx);
    assert_eq!(
        chain,
        [
            "set-option -p -t %3 @moa_view plan",
            "set-option -p -t %3 @moa_label ◷ big picture",
            "set-option -p -t %3 @moa_framed 1",
            "set-option -p -t %3 window-style fg=#4c566a",
            "set-option -p -t %3 window-active-style fg=#88c0d0",
        ]
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");

    let fx = answer(&b, &mut st, n, Ok(Vec::new()));
    assert_eq!(
        last_panels(&fx),
        json!({"c7": {"key": "c7", "pane": "%3", "view": "plan", "managed": true}})
    );
    let at_publish = fx
        .iter()
        .rposition(|e| matches!(e, Effect::Publish(_)))
        .unwrap();
    let at_reply = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .unwrap();
    assert!(at_publish < at_reply, "{fx:?}");
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"pane": "%3", "view": "plan"})
    );
    // The tab's view is moa's: a panel there again gets it.
    handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    let fx = send_on(&b, &mut st, 8, 1, panel(Some("%3"), None));
    assert_eq!(the_reply(&fx).unwrap().data["view"], "plan");

    // An untagged pane isn't a panel moa placed.
    let fx = send_on(&b, &mut st, 8, 2, select_view("%4", View::Plan));
    let (n, _) = the_ask(&fx);
    let fx = answer_lists(&b, &mut st, n, the_panes());
    assert_eq!(the_reply(&fx), Err(OpError("%4 isn't a moa panel".into())));
    assert!(st.tmux().queries().next().is_none());
    // Nor is a tab id that isn't one.
    let fx = send_on(&b, &mut st, 8, 3, select_view("4", View::Plan));
    assert_eq!(
        the_reply(&fx),
        Err(OpError("a panel's tab id looks like %5".into()))
    );
}

/// L17: the topology knows a chain's tags as soon as its `Ok` comes, before any listing.
#[test]
fn chain_ok_records_its_tags_at_once() {
    let (b, mut st) = listed_up();
    let fx = send_on(&b, &mut st, 7, 1, select_view("%9", View::Plan));
    let (n, _) = the_ask(&fx);
    let (n, _) = the_ask(&answer_lists(&b, &mut st, n, the_panes()));
    assert_eq!(st.tmux().topology.view_of(PaneId(9)), Some("tree"));
    answer(&b, &mut st, n, Ok(Vec::new()));
    assert_eq!(st.tmux().topology.view_of(PaneId(9)), Some("plan"));
    assert!(st.tmux().queries().next().is_none());
}

/// `ui.select_view` on conn 7 for `pane`, through its listing (which shows `panes`): the
/// reply it ends with, when it ends there.
fn select_from_listing(
    b: &TypicalBrain,
    st: &mut State,
    pane: &str,
    panes: Vec<String>,
) -> Vec<Effect> {
    let fx = send_on(b, st, 7, 1, select_view(pane, View::Plan));
    let (n, _) = the_ask(&fx);
    answer_lists(b, st, n, panes)
}

/// The agent spot keeps its view, whoever is in it; no panel is given the empty slot's
/// view or the picker's.
#[test]
fn select_view_leaves_the_spot_alone() {
    let (b, mut st) = listed_up();
    let mut panes = the_panes();
    // In the cockpit: the empty slot `%8`, and Lantern's agent `%7` (should both be there).
    panes.push("P %8 @1 $0 1 1 0 1  0 empty-slot".to_string());
    panes.push(format!("P %7 @1 $0 1 1 0 1 {A} 0 agent"));
    for pane in ["%8", "%7"] {
        let fx = select_from_listing(&b, &mut st, pane, panes.clone());
        let said = format!("{pane} is the agent spot; it can't show another view");
        assert_eq!(the_reply(&fx), Err(OpError(said)));
        assert!(st.tmux().queries().next().is_none(), "no chain");
    }
    // A spare empty slot parked in `_moa` stays one: a vacate swaps it into the spot.
    panes.push(pane_line(5, 105, 1, "empty-slot"));
    let fx = select_from_listing(&b, &mut st, "%5", panes);
    let said = "%5 is a spare agent spot; it can't show another view".to_string();
    assert_eq!(the_reply(&fx), Err(OpError(said)));
    assert!(st.tmux().queries().next().is_none(), "no chain");
    for view in [View::EmptySlot, View::Picker] {
        let fx = send_on(&b, &mut st, 7, 2, select_view("%3", view));
        let said = format!("{view} can't be chosen for a panel");
        assert_eq!(the_reply(&fx), Err(OpError(said)));
        assert!(st.tmux().queries().next().is_none(), "no listing");
    }
}

/// tmux refusing the retag is said in plain words, and nothing changes.
#[test]
fn a_select_view_tmux_refuses_says_why() {
    let (b, mut st) = listed_up();
    send_on(&b, &mut st, 8, 1, panel(Some("%9"), None));
    let (n, _) = the_ask(&select_from_listing(&b, &mut st, "%9", the_panes()));
    let fx = answer(&b, &mut st, n, Err("can't find pane: %9".into()));
    assert_eq!(
        the_reply(&fx),
        Err(OpError(
            "the panel in %9 couldn't show plan: something moa was moving closed meanwhile".into()
        ))
    );
    assert!(models(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.tmux().topology.view_of(PaneId(9)), Some("tree"));
    assert_eq!(published_panels(&st)["c8"]["view"], "tree");
}

#[test]
fn select_view_with_no_terminal_says_so() {
    let (b, mut st) = loaded(TOML);
    let fx = send_on(&b, &mut st, 7, 1, select_view("%3", View::Plan));
    assert_eq!(
        the_reply(&fx),
        Err(OpError(
            "moa isn't open in a terminal yet; run moa first".into()
        ))
    );
}

/// The connection that asked closed: the retag still happens, with nobody to answer.
#[test]
fn a_select_view_whose_asker_left_still_retags() {
    let (b, mut st) = listed_up();
    let (n, _) = the_ask(&select_from_listing(&b, &mut st, "%9", the_panes()));
    handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    let fx = answer(&b, &mut st, n, Ok(Vec::new()));
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.tmux().topology.view_of(PaneId(9)), Some("plan"));
}

#[test]
fn a_new_tmux_server_clears_the_panes_of_registered_panels() {
    let (b, mut st) = listed_up();
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    // The same server, linked again: the panel keeps its tab.
    feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    let fx = feed(&b, &mut st, connected(2, generation()));
    assert!(models(&fx).is_empty(), "{fx:?}");
    assert_eq!(published_panels(&st)["c7"]["pane"], "%3");
    // Another server's `%3` is another tab.
    feed(&b, &mut st, TmuxIn::Down(Down::Lost));
    let other = Generation {
        pid: 11,
        start: 2000,
    };
    let fx = feed(&b, &mut st, connected(3, other));
    assert_eq!(
        last_panels(&fx),
        json!({"c7": {"key": "c7", "pane": null, "view": "status", "managed": false}})
    );
}

/// The commands sent as a placed panel's tags (`TmuxToken::Tags`).
fn tags_sent(fx: &[Effect]) -> Vec<Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds,
                token: Some(TmuxToken::Tags),
                ..
            } => Some(cmds.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// L18: a panel moa placed that was never tagged (its spawn's answer lost) is tagged as it
/// registers, as its spawn would have tagged it. One the user started, or one outside
/// `_moa`, never is.
#[test]
fn an_untagged_panel_moa_placed_is_tagged_as_it_registers() {
    let (b, mut st) = listed_up();
    let fx = send_on(&b, &mut st, 7, 1, placed_panel("%6", View::Tree, true));
    assert_eq!(tags_sent(&fx), spawn_tags(PaneId(6), View::Tree));
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c7", "view": "tree", "managed": true})
    );
    assert_eq!(st.tmux().topology.view_of(PaneId(6)), Some("tree"));
    // Kept through a listing that doesn't show it yet, and not sent again.
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::SessionsChanged),
    );
    let fx = link_listed(&b, &mut st, &the_panes());
    assert!(tags_sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(published_panels(&st)["c7"]["managed"], true);

    // The user's own panel, and a placed one outside `_moa`.
    let (b, mut st) = listed_up();
    let fx = send_on(&b, &mut st, 7, 1, placed_panel("%6", View::Tree, false));
    assert!(tags_sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(the_reply(&fx).unwrap().data["managed"], false);
    let fx = send_on(&b, &mut st, 8, 1, placed_panel("%4", View::Tree, true));
    assert!(tags_sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(the_reply(&fx).unwrap().data["managed"], false);
}

/// L17: healing waits while a cockpit operation runs, since a listing taken before its chain
/// could retag a pane after the chain ran. The orphan stays untagged, and the first listing
/// once the queue is idle heals it.
#[test]
fn healing_waits_for_the_cockpit_queue() {
    let (b, mut st) = listed_up();
    // A `ui.select_view` of `%3`, its chain in flight.
    let (n, _) = the_ask(&send_on(&b, &mut st, 7, 1, select_view("%3", View::Plan)));
    let (n, _) = the_ask(&answer_lists(&b, &mut st, n, the_panes()));

    let fx = send_on(&b, &mut st, 8, 1, placed_panel("%6", View::Tree, true));
    assert!(tags_sent(&fx).is_empty(), "{fx:?}");
    let changed = TmuxIn::Notification(Notification::SessionsChanged);
    feed(&b, &mut st, changed.clone());
    let fx = link_listed(&b, &mut st, &the_panes());
    assert!(tags_sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.tmux().topology.view_of(PaneId(6)), None);

    // The chain answered, the queue idle: the next listing heals it.
    answer(&b, &mut st, n, Ok(Vec::new()));
    feed(&b, &mut st, changed);
    let fx = link_listed(&b, &mut st, &the_panes());
    assert_eq!(tags_sent(&fx), spawn_tags(PaneId(6), View::Tree));
}

/// A `ui.select_view` refused after its asker left has nobody to hear it: it is logged.
#[test]
fn a_select_view_refused_after_its_asker_left_is_logged() {
    let (b, mut st) = listed_up();
    let (n, _) = the_ask(&send_on(&b, &mut st, 7, 1, select_view("%4", View::Plan)));
    handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    let fx = answer_lists(&b, &mut st, n, the_panes());
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        log_lines(&fx),
        [(LogLevel::Warn, "%4 isn't a moa panel".to_string())]
    );
}

/// A placed panel that registered before moa listed its tab is tagged at that listing.
#[test]
fn an_untagged_placed_panel_is_tagged_at_the_first_listing() {
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, placed_panel("%6", View::Today, true));
    link_up(&b, &mut st);
    let fx = link_listed(&b, &mut st, &the_panes());
    assert_eq!(tags_sent(&fx), spawn_tags(PaneId(6), View::Today));
    assert_eq!(
        last_panels(&fx),
        json!({"c7": {"key": "c7", "pane": "%6", "view": "today", "managed": true}})
    );
}

/// S11: setup has a frame, so choosing it for a panel sets the frame's three options. (Help
/// has one too, but it is a sheet, never a panel's to choose: S9.)
#[test]
fn select_view_setup_sets_the_frame() {
    let (b, mut st) = listed_up();
    send_on(&b, &mut st, 7, 1, panel(Some("%3"), Some(View::Tree)));
    let fx = send_on(&b, &mut st, 7, 2, select_view("%3", View::Setup));
    let (n, _) = the_ask(&fx);
    let fx = answer_lists(&b, &mut st, n, the_panes());
    let (_, chain) = the_ask(&fx);
    assert_eq!(
        chain,
        [
            "set-option -p -t %3 @moa_view setup",
            "set-option -p -t %3 @moa_label setup",
            "set-option -p -t %3 @moa_framed 1",
            "set-option -p -t %3 window-style fg=#4c566a",
            "set-option -p -t %3 window-active-style fg=#88c0d0",
        ]
    );
}

// --- Popups (S8) ---

/// `ui.panel` from a popup (`moa panel picker --once`).
fn popup() -> Op {
    Op::UiPanel {
        pane: None,
        view: Some(View::Picker),
        placed: false,
        once: true,
    }
}

/// The `run-shell true` nudges in `fx`, which make tmux reap what it hasn't.
fn nudges(fx: &[Effect]) -> usize {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds),
            _ => None,
        })
        .flatten()
        .filter(|c| c.line() == "run-shell true")
        .count()
}

/// S8: a popup's connection going means its program ended; tmux closes the popup only once
/// it reaps that program, which 3.4 and 3.5 built with utempter may not do on their own
/// (5b). There, and only there, the core nudges, once. No other panel's going nudges.
#[test]
fn a_once_connection_going_nudges_on_3_4() {
    for (version, want) in [
        ("3.4", 1),
        ("3.5a", 1),
        ("next-3.5", 1),
        ("3.6", 0),
        ("3.8", 0),
    ] {
        let (b, mut st) = loaded(TOML);
        let fx = feed(
            &b,
            &mut st,
            TmuxIn::Connected {
                conn: 1,
                generation: generation(),
                version: version.into(),
                ctl: ClientName("client-9".into()),
                socket: "moa".into(),
            },
        );
        assert_eq!(nudges(&fx), 0, "{version}");
        send_on(&b, &mut st, 7, 1, popup());
        send_on(&b, &mut st, 8, 1, placed_panel("%3", View::Tree, true));
        send_on(&b, &mut st, 9, 1, panel(Some("%4"), Some(View::Tree)));
        for conn in [8, 9] {
            let fx = handle_checked(&mut st, Input::ClientGone(ConnId(conn)), b.now());
            assert_eq!(nudges(&fx), 0, "{version}: c{conn}");
        }
        let fx = handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
        assert_eq!(nudges(&fx), want, "{version}");
        // It has gone: its connection can't go twice.
        let fx = handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
        assert_eq!(nudges(&fx), 0, "{version}");
    }
    // With the link down there is no tmux to nudge.
    let (b, mut st) = loaded(TOML);
    send_on(&b, &mut st, 7, 1, popup());
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(7)), b.now());
    assert_eq!(nudges(&fx), 0);
}

/// A popup claims no tab and is never moa's: its row has no pane, and the reply says
/// `managed: false`.
#[test]
fn a_popup_registers_without_a_tab() {
    let (b, mut st) = loaded(TOML);
    let fx = send_on(&b, &mut st, 7, 1, popup());
    assert_eq!(
        the_reply(&fx).unwrap().data,
        json!({"key": "c7", "view": "picker", "managed": false})
    );
    assert_eq!(
        published_panels(&st),
        json!({"c7": {"key": "c7", "pane": null, "view": "picker", "managed": false}})
    );
}
