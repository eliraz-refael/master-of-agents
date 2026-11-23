//! The typical brain under a `moa.toml`, its link played by hand: what `open`, reconcile and
//! the ops on tabs are tested against.
#![allow(dead_code)]

pub mod central;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, Publish, TimerKey, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::SessionId;
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, Deps, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::link::Link;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use serde_json::Value;

/// The plugin sessions are started with.
pub const PLUGIN: &str = "/opt/moa/plugin/session";

/// The typical brain under `toml`, its link not yet up.
pub fn loaded(toml: &str) -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let deps = Deps {
        session_plugin: PathBuf::from(PLUGIN),
        home: b.home.clone(),
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

/// The typical brain's time zone.
pub fn tz() -> TimeZone {
    TimeZone::posix(TYPICAL_TZ).unwrap()
}

/// The moment `wall` in the typical brain's time zone.
pub fn at(wall: UtcMs) -> Now {
    Now::at(wall, Duration::ZERO, tz())
}

/// The effects in `fx` that arm or cancel timer `key`.
pub fn timers(fx: &[Effect], key: TimerKey) -> Vec<&Effect> {
    fx.iter()
        .filter(
            |e| matches!(e, Effect::SetTimer { key: k, .. } | Effect::CancelTimer(k) if *k == key),
        )
        .collect()
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
    send_on(b, st, 1, req, op)
}

/// Request `req` on connection `conn`.
pub fn send_on(b: &TypicalBrain, st: &mut State, conn: u64, req: u64, op: Op) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(conn),
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

/// The pane queries asked in `fx` (listings, chains, …): their numbers and commands.
pub fn queries(fx: &[Effect]) -> Vec<(u64, Vec<Cmd>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds,
                token: Some(TmuxToken::Query(n)),
                ..
            } => Some((*n, cmds.clone())),
            _ => None,
        })
        .collect()
}

/// The one query asked in `fx`, which is a listing (the cockpit queue's): its number.
pub fn the_listing(fx: &[Effect]) -> u64 {
    match &queries(fx)[..] {
        [(n, cmds)] if *cmds == refresh_cmds() => *n,
        other => panic!("not one listing: {other:?}"),
    }
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

/// `pane`'s line in the refresh's fourth list (`LIST_TERMS`): its process (pid `1000 + N`),
/// its terminal mark `mark` (`""` for none), `command` in front in folder `cwd`, and its
/// label as tmux prints it.
pub fn pane_line(pane: &str, mark: &str, command: &str, cwd: &str, label: &str) -> String {
    let pid = 1000 + pane.trim_start_matches('%').parse::<u32>().unwrap_or(0);
    format!("T\t{pane}\t{pid}\t{mark}\t{command}\t{cwd}\t{label}")
}

/// A terminal's line in the refresh's fourth list (`LIST_TERMS`): `pane` (`%N`), marked
/// `@moa_term` and labelled `label` as tmux prints it, its shell `sh` (pid `1000 + N`) at
/// its prompt in `/r`.
pub fn term_line(pane: &str, label: &str) -> String {
    pane_line(pane, "1", "sh", "/r", label)
}

/// The link's lists: the sessions `moa` (`$0`) and `_moa` (`$1`), these `panes`
/// (`LIST_PANES` lines), a user attached to `moa`, and `terms` as the fourth query
/// (`LIST_TERMS`) answers.
pub fn lists(panes: &[&str], terms: &[String]) -> Vec<Result<Vec<String>, String>> {
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    vec![
        lines(&["S $0 moa", "S $1 _moa"]),
        lines(panes),
        lines(&["C 0 $0 /dev/ttys1"]),
        Ok(terms.to_vec()),
    ]
}

/// tmux answers a refresh on the link's connection with [`lists`].
pub fn refreshed(
    b: &TypicalBrain,
    st: &mut State,
    panes: &[&str],
    terms: &[String],
) -> Vec<Effect> {
    refresh_answered(st, b.now(), lists(panes, terms))
}

/// tmux answers a refresh on the link's connection, at `now`, with `results`.
pub fn refresh_answered(
    st: &mut State,
    now: Now,
    results: Vec<Result<Vec<String>, String>>,
) -> Vec<Effect> {
    let Link::Up { conn, .. } = st.tmux().link else {
        panic!("the link isn't up");
    };
    let input = TmuxIn::Replies {
        conn,
        token: TmuxToken::Refresh,
        results,
    };
    handle_checked(st, Input::Tmux(input), now)
}

/// The `terms` topic's rows, as published.
pub fn term_rows(st: &State) -> BTreeMap<String, Value> {
    (*st.model().topics[&Topic::Terms]).clone()
}

/// The `terms` rows each publish in `fx` carried.
pub fn published_term_rows(fx: &[Effect]) -> Vec<BTreeMap<String, Value>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => m.topics.get(&Topic::Terms),
            _ => None,
        })
        .map(|t| (**t).clone())
        .collect()
}

/// `set-option -p -u -t <pane> <name>`: one of moa's options taken off a tab.
pub fn unset(pane: &str, name: &str) -> Cmd {
    Cmd::new("set-option").args(["-p", "-u", "-t", pane, name])
}

/// The options taken off tabs (`set-option -p -u`), in the order they go out.
pub fn unsets(fx: &[Effect]) -> Vec<&Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| {
            c.name() == "set-option"
                && c.argv()
                    .get(..2)
                    .is_some_and(|a| a[0] == "-p" && a[1] == "-u")
        })
        .collect()
}

/// What tab `pane` loses when its adopted agent left: the session tag, then the mark.
pub fn untagged(pane: &str) -> Vec<Cmd> {
    vec![unset(pane, "@moa_session"), unset(pane, "@moa_adopted")]
}
