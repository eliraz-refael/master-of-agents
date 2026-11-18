//! Panels in the core: which connection is which panel (`ui.panel`), where each points
//! (`ui.here`), and the `brain` and `panels` topics their views read.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use moa_core::UtcMs;
use moa_core::effect::{Effect, Input, Publish, TmuxIn, handle_checked};
use moa_core::model::{Activity, AgentKind, Lifecycle, Location, PaneBinding, Session, SessionId};
use moa_core::op::{ConnId, Done, Op, OpError};
use moa_core::panel::View;
use moa_core::publish::{BrainRow, Model, Topic};
use moa_core::state::{Deps, State};
use moa_core::tmux::ids::PaneId;
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
