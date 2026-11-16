//! The tmux link in the core: connect, `tmux.ensure`, refresh, the `moa` session and
//! server generations, every input through `handle_checked`.

use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Down, Effect, Input, LogLevel, LogLine, TmuxIn, TmuxToken, check_order, handle_checked,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, State, TmuxLinkData};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId, WindowId};
use moa_core::tmux::link::Link;
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};
use serde_json::json;

fn now() -> Now {
    let wall = UtcMs(
        "2026-11-15T17:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    Now::at(
        wall,
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn started() -> State {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut s, Input::Startup(Box::default()), now());
    s
}

fn feed(s: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(s, Input::Tmux(t), now())
}

fn ensure_as(s: &mut State, req: u64, tmux: Option<&str>) -> Vec<Effect> {
    handle_checked(
        s,
        Input::Client {
            conn: ConnId(7),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op: Op::TmuxEnsure {
                tmux: tmux.map(PathBuf::from),
            },
        },
        now(),
    )
}

fn ensure(s: &mut State) -> Vec<Effect> {
    ensure_as(s, 1, None)
}

fn generation(pid: i64) -> Generation {
    Generation { pid, start: 1000 }
}

fn connected(s: &mut State, conn: u64, g: Generation) -> Vec<Effect> {
    feed(
        s,
        TmuxIn::Connected {
            conn,
            generation: g,
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
        },
    )
}

fn lines(l: &[&str]) -> Result<Vec<String>, String> {
    Ok(l.iter().map(|s| s.to_string()).collect())
}

/// The three lists with these sessions, one pane in the first one, and one user client.
fn lists(sessions: &[&str]) -> Vec<Result<Vec<String>, String>> {
    vec![
        lines(sessions),
        lines(&["P %1 @1 $0 1 1 0 "]),
        lines(&["C 0 $0 /dev/ttys1"]),
    ]
}

fn reply_lists(s: &mut State, conn: u64, sessions: &[&str]) -> Vec<Effect> {
    feed(
        s,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Refresh,
            results: lists(sessions),
        },
    )
}

const WITH_MOA: &[&str] = &["S $0 moa", "S $1 _moa"];
const WITHOUT_MOA: &[&str] = &["S $1 _moa"];

fn refresh_effect(conn: u64) -> Effect {
    Effect::Tmux {
        conn,
        cmds: refresh_cmds(),
        token: Some(TmuxToken::Refresh),
    }
}

fn cockpit_effect(conn: u64) -> Effect {
    Effect::Tmux {
        conn,
        cmds: vec![Cmd::new("new-session").args(["-d", "-s", "moa", "-n", "moa"])],
        token: Some(TmuxToken::Cockpit),
    }
}

/// The link's own plumbing; the status line's writes are tested in `tmux_status`.
fn tmux_effects(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| matches!(e, Effect::Tmux { token: Some(_), .. }))
        .collect()
}

fn connects(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| matches!(e, Effect::TmuxConnect { .. }))
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

fn running(yes: bool) -> Result<Done, OpError> {
    Ok(Done {
        text: String::new(),
        data: json!({ "running": yes }),
    })
}

fn refused(line: &str) -> Result<Done, OpError> {
    Err(OpError(line.to_string()))
}

fn logs(fx: &[Effect], level: LogLevel) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(LogLine { level: l, message }) if *l == level => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn startup_asks_to_connect() {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    let fx = handle_checked(&mut s, Input::Startup(Box::default()), now());
    assert_eq!(connects(&fx), vec![&Effect::TmuxConnect { tmux: None }]);
    // It comes after the state is published: Startup has nothing to save here.
    let connect = fx
        .iter()
        .position(|e| matches!(e, Effect::TmuxConnect { .. }))
        .unwrap();
    let publish = fx
        .iter()
        .position(|e| matches!(e, Effect::Publish(_)))
        .unwrap();
    assert!(publish < connect);
    assert_eq!(s.tmux().link, Link::Connecting);
}

#[test]
fn ensure_waits_for_the_link_and_the_moa_session() {
    let mut s = started();
    let fx = ensure(&mut s);
    assert!(replies(&fx).is_empty());
    assert!(connects(&fx).is_empty(), "already connecting");
    let fx = connected(&mut s, 1, generation(10));
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(1)]);
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert_eq!(replies(&fx), vec![(ReqId(1), running(true))]);
    assert!(tmux_effects(&fx).is_empty());
    // Now it answers at once.
    let fx = ensure_as(&mut s, 2, None);
    assert_eq!(replies(&fx), vec![(ReqId(2), running(true))]);
}

#[test]
fn ensure_without_a_server_says_not_running() {
    let mut s = started();
    ensure(&mut s);
    let fx = feed(&mut s, TmuxIn::Down(Down::NoServer));
    assert_eq!(replies(&fx), vec![(ReqId(1), running(false))]);
    assert_eq!(s.tmux().link, Link::Down);
    let fx = ensure_as(&mut s, 2, Some("/opt/tools/bin/tmux"));
    assert!(replies(&fx).is_empty());
    assert_eq!(
        connects(&fx),
        vec![&Effect::TmuxConnect {
            tmux: Some("/opt/tools/bin/tmux".into())
        }]
    );
    assert_eq!(s.tmux().link, Link::Connecting);
}

#[test]
fn a_missing_moa_session_is_created_once() {
    let mut s = started();
    ensure(&mut s);
    connected(&mut s, 1, generation(10));
    let fx = reply_lists(&mut s, 1, WITHOUT_MOA);
    assert_eq!(tmux_effects(&fx), vec![&cockpit_effect(1)]);
    assert!(replies(&fx).is_empty());
    let fx = reply_lists(&mut s, 1, WITHOUT_MOA);
    assert!(tmux_effects(&fx).is_empty());
    let fx = feed(&mut s, TmuxIn::Notification(Notification::SessionsChanged));
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(1)]);
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert_eq!(replies(&fx), vec![(ReqId(1), running(true))]);
    assert!(tmux_effects(&fx).is_empty());
}

#[test]
fn a_failed_cockpit_create_answers_the_waiters() {
    let mut s = started();
    ensure(&mut s);
    connected(&mut s, 1, generation(10));
    reply_lists(&mut s, 1, WITHOUT_MOA);
    let fx = feed(
        &mut s,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Cockpit,
            results: vec![Err("duplicate session: moa\nsecond line".into())],
        },
    );
    assert_eq!(
        replies(&fx),
        vec![(
            ReqId(1),
            refused("moa couldn't open its window; see the log")
        )]
    );
    assert_eq!(
        logs(&fx, LogLevel::Warn),
        vec!["creating the moa session failed: duplicate session: moa".to_string()]
    );
    // The waiters were answered once.
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert!(replies(&fx).is_empty());
}

#[test]
fn ensure_after_a_failed_cockpit_create_tries_again() {
    let mut s = started();
    ensure(&mut s);
    connected(&mut s, 1, generation(10));
    reply_lists(&mut s, 1, WITHOUT_MOA);
    feed(
        &mut s,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Cockpit,
            results: vec![Err("no".into())],
        },
    );
    let fx = ensure_as(&mut s, 2, None);
    assert!(replies(&fx).is_empty());
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(1)]);
    let fx = reply_lists(&mut s, 1, WITHOUT_MOA);
    assert_eq!(tmux_effects(&fx), vec![&cockpit_effect(1)]);
}

#[test]
fn refreshes_coalesce() {
    let mut s = started();
    connected(&mut s, 1, generation(10));
    for w in 1..=3 {
        let fx = feed(
            &mut s,
            TmuxIn::Notification(Notification::WindowAdd {
                window: WindowId(w),
            }),
        );
        assert!(tmux_effects(&fx).is_empty(), "one refresh in flight");
    }
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(1)]);
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert!(tmux_effects(&fx).is_empty());
}

#[test]
fn too_old_and_missing_tmux_answer_in_one_line() {
    let mut s = started();
    ensure(&mut s);
    let fx = feed(&mut s, TmuxIn::Down(Down::TooOld("3.3a".into())));
    assert_eq!(
        replies(&fx),
        vec![(
            ReqId(1),
            refused("moa needs tmux 3.4 or newer; this one is 3.3a")
        )]
    );
    let fx = ensure_as(&mut s, 2, Some("/usr/bin/tmux"));
    assert_eq!(connects(&fx).len(), 1);
    let fx = feed(&mut s, TmuxIn::Down(Down::NoTmux));
    assert_eq!(
        replies(&fx),
        vec![(
            ReqId(2),
            refused("moa can't find tmux; install tmux 3.4 or newer")
        )]
    );
    // A tmux that couldn't be started is forgotten.
    let fx = ensure_as(&mut s, 3, None);
    assert_eq!(connects(&fx), vec![&Effect::TmuxConnect { tmux: None }]);
    let fx = feed(&mut s, TmuxIn::Down(Down::GaveUp));
    assert_eq!(
        replies(&fx),
        vec![(
            ReqId(3),
            refused("moa lost its terminal server and couldn't reconnect; see the log")
        )]
    );
}

#[test]
fn lost_keeps_the_waiters_and_connects_once() {
    let mut s = started();
    ensure(&mut s);
    connected(&mut s, 1, generation(10));
    let fx = feed(&mut s, TmuxIn::Down(Down::Lost));
    assert!(replies(&fx).is_empty());
    assert_eq!(s.tmux().link, Link::Connecting);
    let fx = ensure_as(&mut s, 2, None);
    assert!(replies(&fx).is_empty());
    assert!(connects(&fx).is_empty());
    let fx = connected(&mut s, 2, generation(10));
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(2)]);
    let fx = reply_lists(&mut s, 2, WITH_MOA);
    assert_eq!(
        replies(&fx),
        vec![(ReqId(1), running(true)), (ReqId(2), running(true))]
    );
}

#[test]
fn a_new_generation_is_counted_once() {
    let mut s = started();
    let fx = connected(&mut s, 1, generation(10));
    assert!(logs(&fx, LogLevel::Info).is_empty());
    feed(&mut s, TmuxIn::Down(Down::NoServer));
    let fx = connected(&mut s, 2, generation(11));
    assert_eq!(s.tmux().restarts, 1);
    assert_eq!(
        logs(&fx, LogLevel::Info),
        vec!["the terminal server restarted".to_string()]
    );
    feed(&mut s, TmuxIn::Down(Down::Lost));
    let fx = connected(&mut s, 3, generation(11));
    assert_eq!(s.tmux().restarts, 1);
    assert!(logs(&fx, LogLevel::Info).is_empty());
}

#[test]
fn replies_from_an_old_connection_are_dropped() {
    let mut s = started();
    connected(&mut s, 1, generation(10));
    feed(&mut s, TmuxIn::Down(Down::Lost));
    connected(&mut s, 2, generation(10));
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(s.tmux().topology.session_named("moa"), None);
    // While the link is down, nothing is taken either.
    feed(&mut s, TmuxIn::Down(Down::NoServer));
    let fx = reply_lists(&mut s, 2, WITH_MOA);
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(s.tmux().topology.session_named("moa"), None);
}

#[test]
fn a_failed_list_keeps_the_topology() {
    let mut s = started();
    connected(&mut s, 1, generation(10));
    // No moa yet: one create is sent, and it worked.
    reply_lists(&mut s, 1, WITHOUT_MOA);
    feed(
        &mut s,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Cockpit,
            results: vec![Ok(vec![])],
        },
    );
    // An ensure clears the attempt and lists again; the panes list fails.
    ensure(&mut s);
    let mut results = lists(WITHOUT_MOA);
    results[1] = Err("server exited unexpectedly".into());
    let fx = feed(
        &mut s,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results,
        },
    );
    assert!(tmux_effects(&fx).is_empty(), "no create on no information");
    assert!(replies(&fx).is_empty());
    assert_eq!(
        logs(&fx, LogLevel::Warn),
        vec!["listing the terminal server failed".to_string()]
    );
    let t = &s.tmux().topology;
    assert!(t.has_pane(PaneId(1)));
    assert!(t.session_named("_moa").is_some());
}

#[test]
fn a_disabled_link_answers_ensure() {
    let mut s = started();
    ensure(&mut s);
    let fx = feed(&mut s, TmuxIn::Down(Down::Disabled));
    assert_eq!(
        replies(&fx),
        vec![(
            ReqId(1),
            refused("moa v1 isn't switched over yet; try moa --sandbox")
        )]
    );
    assert_eq!(s.tmux().link, Link::Down);
}

#[test]
fn a_bad_tmux_path_is_refused() {
    let mut s = started();
    for bad in ["rm", "/bin/sh", "tmux", "/usr/bin/tmux/"] {
        let fx = ensure_as(&mut s, 1, Some(bad));
        assert_eq!(
            replies(&fx),
            vec![(ReqId(1), refused("that isn't a tmux program"))],
            "{bad}"
        );
        assert!(connects(&fx).is_empty());
    }
    let fx = ensure_as(&mut s, 2, Some("/usr/bin/tmux"));
    assert!(replies(&fx).is_empty(), "a waiter");
}

#[test]
fn reload_keeps_the_link_and_restarts_its_bookkeeping() {
    let mut s = started();
    ensure(&mut s);
    connected(&mut s, 1, generation(10));
    // The refresh is in flight; a reload forgets it and asks again.
    let fx = handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::default(),
            refuse_writes: None,
        },
        now(),
    );
    assert_eq!(tmux_effects(&fx), vec![&refresh_effect(1)]);
    assert!(matches!(s.tmux().link, Link::Up { conn: 1, .. }));
    let fx = reply_lists(&mut s, 1, WITH_MOA);
    assert_eq!(replies(&fx), vec![(ReqId(1), running(true))]);

    // While connecting, a reload asks to connect again.
    feed(&mut s, TmuxIn::Down(Down::Lost));
    let fx = handle_checked(
        &mut s,
        Input::Reload {
            snapshot: Box::default(),
            refuse_writes: None,
        },
        now(),
    );
    assert_eq!(connects(&fx).len(), 1);
    assert!(tmux_effects(&fx).is_empty());
}

#[test]
fn effect_ordering() {
    let view = Effect::Tmux {
        conn: 1,
        cmds: vec![],
        token: None,
    };
    assert!(check_order(&[view.clone(), Effect::Persist(vec![])]).is_err());
    assert!(!view.runs_at_once());
    assert!(refresh_effect(1).runs_at_once());
    assert!(cockpit_effect(1).runs_at_once());
    assert!(Effect::TmuxConnect { tmux: None }.runs_at_once());
    // Plumbing isn't heard: a Persist may follow it.
    assert!(check_order(&[refresh_effect(1), Effect::Persist(vec![])]).is_ok());
}

#[test]
fn status_shows_the_link() {
    let mut s = started();
    let st = s.status(&CallerInfo::default(), UtcMs(0));
    assert_eq!(st.tmux, None);
    assert_eq!(st.tmux_restarts, 0);
    connected(&mut s, 1, Generation { pid: 42, start: 7 });
    feed(&mut s, TmuxIn::Down(Down::NoServer));
    connected(&mut s, 2, Generation { pid: 43, start: 9 });
    let st = s.status(&CallerInfo::default(), UtcMs(0));
    assert_eq!(
        st.tmux,
        Some(TmuxLinkData {
            version: "3.5a".into(),
            pid: 43,
            start: 9
        })
    );
    assert_eq!(st.tmux_restarts, 1);
}
