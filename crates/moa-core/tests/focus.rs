//! `focus`: bringing a session's tab to the screen, on the typical brain, once reconcile has
//! looked at the tabs.

mod common;

use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::SessionId;
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::link::NOT_READY;
use moa_testkit::brain::TypicalBrain;
use serde_json::json;

use common::*;

const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

/// The typical brain, linked, its start drain done and its tabs reconciled (none listed).
fn reconciled() -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    handle_checked(&mut st, Input::StartDrained, b.now());
    listed(&mut st, &b, &[], &[]);
    assert!(st.tmux().reconciled());
    (b, st)
}

/// `open` of `<home>/private/lantern` in the background, as request `req`.
fn open_bg(b: &TypicalBrain, st: &mut State, req: u64) -> SessionId {
    let op = Op::SessionOpen {
        path: b.home.join("private/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    the_open(&send(b, st, req, op)).0
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

/// `focus q` as request `req` on connection `conn`.
fn focus_from(b: &TypicalBrain, st: &mut State, conn: u64, req: u64, q: &str) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(conn),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op: Op::SessionFocus { q: q.into() },
        },
        b.now(),
    )
}

fn select(pane: &str) -> [Cmd; 2] {
    [
        Cmd::new("select-window").args(["-t", pane]),
        Cmd::new("select-pane").args(["-t", pane]),
    ]
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

fn went_to(id: SessionId) -> Result<Done, OpError> {
    Ok(Done {
        text: "went to lantern".into(),
        data: json!({ "id": id, "title": "lantern" }),
    })
}

#[test]
fn focus_selects_a_running_sessions_tab() {
    let (b, mut st) = reconciled();
    let id = open_bg(&b, &mut st, 1);
    bound(&b, &mut st, id, "%7");
    let fx = focus_from(&b, &mut st, 1, 2, &id.to_string());
    // Nothing to save: the commands go out on their own, before the reply.
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let sent = fx
        .iter()
        .position(
            |e| matches!(e, Effect::Tmux { conn: 1, cmds, token: None } if cmds == &select("%7")),
        )
        .expect("the tab is selected");
    let replied = fx
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .expect("a reply");
    assert!(sent < replied, "{fx:?}");
    assert!(select("%7").iter().all(|c| c.check().is_ok()));
    assert_eq!(replies(&fx), [(ReqId(2), went_to(id))]);
}

/// A focus that comes while the session's tab is opening waits for it: both answers come
/// when tmux says which pane it is, and the tab is selected, though the open was `--bg`.
#[test]
fn focus_waits_for_a_launch_and_answers_when_bound() {
    let (b, mut st) = reconciled();
    let id = open_bg(&b, &mut st, 1);
    let fx = focus_from(&b, &mut st, 1, 2, &id.to_string());
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(selects(&fx).is_empty(), "{fx:?}");

    let fx = bound(&b, &mut st, id, "%7");
    assert!(
        changes(&fx)
            .iter()
            .any(|c| matches!(c, Change::PaneBinding(p) if p.session_id == id)),
        "{fx:?}"
    );
    assert_eq!(selects(&fx), select("%7").iter().collect::<Vec<_>>());
    let path = b.home.join("private/lantern");
    assert_eq!(
        replies(&fx),
        [
            (
                ReqId(1),
                Ok(Done {
                    text: format!("opened lantern with acct-a in {}", path.display()),
                    data: json!({ "id": id, "title": "lantern" }),
                })
            ),
            (ReqId(2), went_to(id)),
        ]
    );
}

/// The three lists of a listing: sessions `moa` and `_moa`, `panes`, moa's control client
/// and a user's terminal.
fn cockpit_lists(panes: &[String]) -> Vec<Result<Vec<String>, String>> {
    vec![
        Ok(vec!["S $0 moa".into(), "S $1 _moa".into()]),
        Ok(panes.to_vec()),
        Ok(vec!["C 1 $1 client-9".into(), "C 0 $0 /dev/ttys1".into()]),
        Ok(Vec::new()),
    ]
}

/// With a cockpit, a focus waiting for a launch hears what the show did once it ran (L11):
/// that the session was brought into the spot, or that its tab closed first. Never `went
/// to` before the show.
#[test]
fn with_a_cockpit_a_focus_waiting_for_a_launch_hears_where_it_landed() {
    for closed in [false, true] {
        let (b, mut st) = reconciled();
        // The link's own listing: the cockpit, its empty slot `%5` in the spot.
        let mut panes = vec![
            "P %2 @1 $0 1 1 0 1  0 tree".to_string(),
            "P %5 @1 $0 0 1 0 1  0 empty-slot".to_string(),
        ];
        let refresh = TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: cockpit_lists(&panes),
        };
        feed(&b, &mut st, refresh);
        let id = open_bg(&b, &mut st, 1);
        let fx = focus_from(&b, &mut st, 1, 2, &id.to_string());
        assert!(replies(&fx).is_empty(), "{fx:?}");

        let fx = bound(&b, &mut st, id, "%7");
        assert_eq!(replies(&fx).len(), 1, "only the open's: {fx:?}");
        assert_eq!(replies(&fx)[0].0, ReqId(1));
        assert!(selects(&fx).is_empty(), "{fx:?}");

        // The show's listing.
        let asked: Vec<u64> = st.tmux().queries().collect();
        let [n] = asked[..] else {
            panic!("one listing, not {asked:?}");
        };
        if !closed {
            panes.push(format!("P %7 @7 $0 1 0 0 0 {id} 0 "));
        }
        let answer = |n, results| TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results,
        };
        let fx = feed(&b, &mut st, answer(n, cockpit_lists(&panes)));
        if closed {
            assert_eq!(
                replies(&fx),
                [(
                    ReqId(2),
                    Err(OpError("its tab closed before moa could show it".into()))
                )]
            );
            continue;
        }
        assert!(replies(&fx).is_empty(), "{fx:?}");
        let n = st.tmux().queries().next().expect("the swap's chain");
        let fx = feed(&b, &mut st, answer(n, vec![Ok(Vec::new())]));
        assert_eq!(
            replies(&fx),
            [(
                ReqId(2),
                Ok(Done {
                    text: "brought it into the cockpit".into(),
                    data: json!({ "id": id, "title": "lantern" }),
                })
            )]
        );
    }
}

/// R10: the bindings can't be trusted until this connection's tabs are reconciled.
#[test]
fn focus_refuses_before_reconcile() {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    let id = open_bg(&b, &mut st, 1);
    bound(&b, &mut st, id, "%7");
    let fx = focus_from(&b, &mut st, 1, 2, &id.to_string());
    assert!(selects(&fx).is_empty(), "{fx:?}");
    assert_eq!(replies(&fx), [(ReqId(2), Err(OpError(NOT_READY.into())))]);
}

/// R26: a focus whose connection closed is never answered; the open it waited on still is.
#[test]
fn a_closed_focus_waiter_is_dropped() {
    let (b, mut st) = reconciled();
    let id = open_bg(&b, &mut st, 1);
    focus_from(&b, &mut st, 2, 5, &id.to_string());
    let fx = handle_checked(&mut st, Input::ClientGone(ConnId(2)), b.now());
    assert!(fx.is_empty(), "{fx:?}");
    let fx = bound(&b, &mut st, id, "%7");
    let to: Vec<(ConnId, ReqId)> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Reply { conn, req, .. } => Some((*conn, *req)),
            _ => None,
        })
        .collect();
    assert_eq!(to, [(ConnId(1), ReqId(1))]);
}

/// A session that isn't running and has no tab opening is resumed (R12): its checks pass,
/// and the tabs are looked at first (`resume.rs` follows it to its tab).
#[test]
fn focus_of_a_session_with_no_tab_resumes_it() {
    let (b, mut st) = reconciled();
    let id = open_bg(&b, &mut st, 1);
    feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Err("create window failed".into())],
        },
    );
    let fx = focus_from(&b, &mut st, 1, 2, &id.to_string());
    assert!(selects(&fx).is_empty(), "{fx:?}");
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
