//! Pane queries, the start drain and the reconcile flag: the link's bookkeeping that ops and
//! reconcile wait on. Here rather than in `tests/`, since all of it is the crate's own.

use std::time::Duration;

use jiff::tz::TimeZone;

use super::*;
use crate::config::{BrainName, Config};
use crate::core::handle;
use crate::effect::Input;
use crate::state::{Snapshot, State};
use crate::tmux::panes::list_panes;
use crate::{Now, UtcMs};

fn now() -> Now {
    Now::at(
        UtcMs(1_000_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn connected(conn: u64) -> TmuxIn {
    TmuxIn::Connected {
        conn,
        generation: Generation {
            pid: 10,
            start: 1000,
        },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
        socket: "moa".into(),
    }
}

fn up(conn: u64) -> TmuxState {
    let mut t = TmuxState::default();
    t.input(connected(conn));
    t
}

/// The query number of a tokened `Query` effect, after checking what it sends.
fn query_of(fx: &Effect, conn: u64) -> u64 {
    match fx {
        Effect::Tmux {
            conn: c,
            cmds,
            token: Some(TmuxToken::Query(n)),
        } if *c == conn && *cmds == list_panes() => *n,
        other => panic!("not a pane query on {conn}: {other:?}"),
    }
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

fn replies(conn: u64, n: u64, results: Vec<Result<Vec<String>, String>>) -> TmuxIn {
    TmuxIn::Replies {
        conn,
        token: TmuxToken::Query(n),
        results,
    }
}

#[test]
fn a_query_is_answered_once() {
    let mut t = TmuxState::default();
    assert_eq!(
        t.ask(Asked::Reconcile, list_panes()),
        None,
        "down: nothing to ask"
    );

    let mut t = up(1);
    let fx = t.ask(Asked::Reconcile, list_panes()).unwrap();
    // The link's own plumbing: it reveals no saved state, so it goes out at once.
    assert!(fx.runs_at_once() && !fx.is_heard());
    let n = query_of(&fx, 1);
    let other = query_of(&t.ask(Asked::Reconcile, list_panes()).unwrap(), 1);
    assert_ne!(n, other);
    assert_eq!(t.queries().collect::<Vec<_>>(), [n, other]);

    let (info, start) = (lines(&["I a"]), lines(&["S b"]));
    let answer = replies(1, n, vec![Ok(info.clone()), Ok(start.clone())]);
    // Another connection's answer, or another query's, answers nothing.
    assert!(
        t.answered(&replies(2, n, vec![Ok(info.clone())]))
            .is_empty()
    );
    assert!(
        t.answered(&replies(1, 99, vec![Ok(info.clone())]))
            .is_empty()
    );
    assert_eq!(
        t.answered(&answer),
        [(Asked::Reconcile, Ok(vec![info.clone(), start]))]
    );
    assert!(t.answered(&answer).is_empty(), "answered once");
    assert!(t.input(answer).is_empty());

    // Any command that failed fails the query.
    let failed = replies(1, other, vec![Ok(info), Err("no server".into())]);
    assert_eq!(t.answered(&failed), [(Asked::Reconcile, Err(()))]);
    assert_eq!(t.queries().count(), 0);
}

#[test]
fn a_query_is_failed_when_the_link_goes_down_or_reconnects() {
    for d in [Down::Lost, Down::NoServer, Down::GaveUp] {
        let mut t = up(1);
        t.ask(Asked::Reconcile, list_panes()).unwrap();
        let down = TmuxIn::Down(d.clone());
        assert_eq!(t.answered(&down), [(Asked::Reconcile, Err(()))], "{d:?}");
        t.input(down);
        assert_eq!(t.queries().count(), 0);
        assert_eq!(t.last_down, Some(d));
    }

    let mut t = up(1);
    let n = query_of(&t.ask(Asked::Reconcile, list_panes()).unwrap(), 1);
    // The same connection again asks nothing new of it; another one does.
    assert!(t.answered(&connected(1)).is_empty());
    assert_eq!(t.answered(&connected(2)), [(Asked::Reconcile, Err(()))]);
    t.input(connected(2));
    // An answer to the old connection's query that arrives late is dropped.
    assert!(t.answered(&replies(1, n, vec![Ok(Vec::new())])).is_empty());
    assert!(t.answered(&replies(2, n, vec![Ok(Vec::new())])).is_empty());
}

fn started() -> State {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    handle(&mut st, Input::Startup(Box::default()), now());
    st
}

fn reload(st: &mut State) {
    handle(
        st,
        Input::Reload {
            snapshot: Box::new(Snapshot::default()),
            refuse_writes: None,
        },
        now(),
    );
}

#[test]
fn ready_waits_for_reconcile() {
    let mut st = started();
    assert_eq!(ready(&st), Err(OpError(NO_TERMINAL.into())));
    handle(&mut st, Input::Tmux(connected(1)), now());
    assert_eq!(ready(&st), Err(OpError(NOT_READY.into())));
    // Reconcile sets it, for this connection only.
    st.tmux.reconciled = true;
    assert!(st.tmux.reconciled());
    assert_eq!(ready(&st), Ok(1));
    reload(&mut st);
    assert_eq!(ready(&st), Err(OpError(NOT_READY.into())));

    st.tmux.reconciled = true;
    handle(&mut st, Input::Tmux(connected(2)), now());
    assert_eq!(ready(&st), Err(OpError(NOT_READY.into())));
    st.tmux.reconciled = true;
    handle(&mut st, Input::Tmux(TmuxIn::Down(Down::Lost)), now());
    assert_eq!(ready(&st), Err(OpError(NO_TERMINAL.into())));
    assert!(!st.tmux.reconciled());
}

#[test]
fn the_start_drain_is_remembered_for_the_daemons_life() {
    let mut st = started();
    assert!(!st.tmux.drained);
    let fx = handle(&mut st, Input::StartDrained, now());
    assert!(st.tmux.drained);
    assert!(fx.iter().all(|e| !e.is_heard()), "{fx:?}");
    reload(&mut st);
    handle(&mut st, Input::Tmux(TmuxIn::Down(Down::NoServer)), now());
    handle(&mut st, Input::Tmux(connected(1)), now());
    assert!(st.tmux.drained);
}
