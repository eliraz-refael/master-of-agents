//! The typical brain under a `moa.toml`, its link played by hand: what `open`, reconcile and
//! the ops on tabs are tested against.
#![allow(dead_code)]

use std::path::PathBuf;

use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::SessionId;
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Deps, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::link::Link;
use moa_testkit::brain::TypicalBrain;

/// The plugin sessions are started with.
pub const PLUGIN: &str = "/opt/moa/plugin/session";

/// The typical brain under `toml`, its link not yet up.
pub fn loaded(toml: &str) -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let deps = Deps {
        session_plugin: PathBuf::from(PLUGIN),
        ..Deps::default()
    };
    let snapshot = b.snapshot();
    let st = loaded_with(&b, toml, deps, snapshot);
    (b, st)
}

/// `b` under `toml`, with `deps`, started from `snapshot`; its link not yet up.
pub fn loaded_with(b: &TypicalBrain, toml: &str, deps: Deps, snapshot: Snapshot) -> State {
    let config = Config::parse(toml, &b.home).unwrap();
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), b.now());
    st
}

pub fn generation() -> Generation {
    Generation {
        pid: 10,
        start: 1000,
    }
}

/// The link comes up, on connection 1.
pub fn link_up(b: &TypicalBrain, st: &mut State) {
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

pub fn feed(b: &TypicalBrain, st: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(st, Input::Tmux(t), b.now())
}

/// Request `req` on connection 1.
pub fn send(b: &TypicalBrain, st: &mut State, req: u64, op: Op) -> Vec<Effect> {
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

pub fn changes(fx: &[Effect]) -> Vec<&Change> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn replies(fx: &[Effect]) -> Vec<(ReqId, Result<Done, OpError>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply { req, result, .. } => Some((*req, result.clone())),
            _ => None,
        })
        .collect()
}

/// The commands of the one tokened `Open`, and its session.
pub fn the_open(fx: &[Effect]) -> (SessionId, &[Cmd]) {
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

/// tmux answers the one pane query waiting, on the link's connection, with these
/// `PANE_INFO` and `PANE_START` lines.
pub fn listed(st: &mut State, b: &TypicalBrain, info: &[&str], start: &[&str]) -> Vec<Effect> {
    let asked: Vec<u64> = st.tmux().queries().collect();
    let [n] = asked[..] else {
        panic!("one pane query waiting, not {asked:?}");
    };
    let Link::Up { conn, .. } = st.tmux().link else {
        panic!("the link isn't up");
    };
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(n),
            results: vec![lines(info), lines(start)],
        },
    )
}
