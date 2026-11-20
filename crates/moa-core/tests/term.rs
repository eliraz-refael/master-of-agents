//! Terminals as a topic (`terms`, S6b): the plain shell tabs moa opened, listed by the
//! link's fourth refresh query and by `term`'s own tag, and `term.show`, which brings one to
//! the screen.

mod common;

use std::collections::BTreeMap;

use moa_core::effect::{Effect, LogLevel, Publish, TmuxIn, TmuxToken};
use moa_core::op::{Done, Op, OpError, ReqId};
use moa_core::publish::Topic;
use moa_core::state::State;
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{PaneId, WindowId};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

use common::*;

const TOML: &str = r#"
[brains.default]
folders = ["~/work"]
"#;

/// The link's lists: `%5` and `%6` in the session `moa`, a user attached, and `terms` as
/// the terminals' query answers.
fn lists(panes: &[&str], terms: &[&str]) -> Vec<Result<Vec<String>, String>> {
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    vec![
        lines(&["S $0 moa", "S $1 _moa"]),
        lines(panes),
        lines(&["C 0 $0 /dev/ttys1"]),
        lines(terms),
    ]
}

const BOTH: &[&str] = &["P %5 @2 $0 1 1 0 0  0 ", "P %6 @3 $0 1 0 0 0  0 "];

/// tmux answers the refresh in flight.
fn refreshed(b: &TypicalBrain, st: &mut State, panes: &[&str], terms: &[&str]) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(panes, terms),
        },
    )
}

/// The typical brain, linked, and its first listing answered with `terms`.
fn listed_with(terms: &[&str]) -> (TypicalBrain, State) {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    refreshed(&b, &mut st, BOTH, terms);
    (b, st)
}

/// The `terms` topic's rows, as published.
fn terms(st: &State) -> BTreeMap<String, Value> {
    (*st.model().topics[&Topic::Terms]).clone()
}

/// The `terms` rows each publish in `fx` carried.
fn published_terms(fx: &[Effect]) -> Vec<BTreeMap<String, Value>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => m.topics.get(&Topic::Terms),
            _ => None,
        })
        .map(|t| (**t).clone())
        .collect()
}

fn row(key: &str, name: &str) -> (String, Value) {
    (key.to_string(), json!({ "key": key, "name": name }))
}

fn show(b: &TypicalBrain, st: &mut State, pane: &str) -> Vec<Effect> {
    send(b, st, 1, Op::TermShow { pane: pane.into() })
}

#[test]
fn refresh_cmds_lists_terminals() {
    assert_eq!(
        refresh_cmds()[3],
        Cmd::new("list-panes").args([
            "-a",
            "-f",
            "#{@moa_term}",
            "-F",
            "T #{pane_id} #{@moa_label}"
        ])
    );
    assert_eq!(refresh_cmds().len(), 4);
}

/// Only the panes the terminals' query lists are terminals: `%6` is in the panes' list
/// with no `@moa_term`, and isn't one. The name is the label without its `⌨ `.
#[test]
fn terminals_are_published_by_name() {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    let fx = refreshed(&b, &mut st, BOTH, &["T %5 ⌨ kite work"]);
    let want = BTreeMap::from([row("%5", "kite work")]);
    assert_eq!(published_terms(&fx), std::slice::from_ref(&want));
    assert_eq!(terms(&st), want);
}

/// tmux keeps a label's `#`s doubled, as moa set it: the name has them single again. A
/// label without the `⌨ ` (a hand-set one) is the name as it is.
#[test]
fn a_terminals_label_is_read_as_moa_set_it() {
    let (_b, st) = listed_with(&["T %5 ⌨ a##[fg=red]b", "T %6 plain"]);
    assert_eq!(
        terms(&st),
        BTreeMap::from([row("%5", "a#[fg=red]b"), row("%6", "plain")])
    );
}

/// A label holding a newline prints a line of its own: a `T` line for a pane the panes'
/// list doesn't hold counts for nothing, nor does a pane listed twice.
#[test]
fn a_forged_terminal_line_counts_for_nothing() {
    let (_b, st) = listed_with(&["T %5 ⌨ kite", "T %9 forged", "T %6 a", "T %6 b"]);
    assert_eq!(terms(&st), BTreeMap::from([row("%5", "kite")]));
}

/// A title holding a newline names its tab on one line.
#[test]
fn a_terminals_name_is_one_line() {
    let (b, mut st) = listed_with(&[]);
    let fx = send(
        &b,
        &mut st,
        1,
        Op::TermOpen {
            path: b.home.join("work"),
            title: Some("a\nT %6 b".into()),
        },
    );
    let [(_, cmds)] = &queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let argv = cmds[0].argv();
    let at = argv.iter().position(|a| a == "-n").expect("a name");
    assert_eq!(argv[at + 1], "⌨ a T %6 b");
}

const SESSION: &str = "00000000-0000-4000-8000-0000000000c1";

/// An agent typed into a terminal moa opened: its tab keeps the terminal mark, but it runs
/// a session's agent (it is tagged with the session), so it is that session and no
/// terminal. In the spot its label is the agent's (`● fix`); back in a tab of its own it
/// has none. Once the agent is gone and the tag with it, it is a terminal again, still with
/// no label: it is called `shell`, as the cockpit calls a shell it sends home.
#[test]
fn a_terminal_running_an_agent_is_its_session() {
    let (b, mut st) = loaded(TOML);
    link_up(&b, &mut st);
    let in_spot = format!("P %5 @2 $0 1 1 0 1 {SESSION} 0 agent");
    refreshed(&b, &mut st, &[&in_spot, BOTH[1]], &["T %5 ● fix"]);
    assert!(terms(&st).is_empty(), "{:?}", terms(&st));
    assert_eq!(
        replies(&show(&b, &mut st, "%5")),
        [(ReqId(1), Err(OpError("that terminal is gone".into())))]
    );

    let in_a_tab = format!("P %5 @4 $0 1 1 0 0 {SESSION} 0 ");
    let notified = TmuxIn::Notification(Notification::WindowClose {
        window: WindowId(2),
    });
    feed(&b, &mut st, notified.clone());
    refreshed(&b, &mut st, &[&in_a_tab, BOTH[1]], &["T %5 "]);
    assert!(terms(&st).is_empty(), "{:?}", terms(&st));
    assert_eq!(
        replies(&show(&b, &mut st, "%5")),
        [(ReqId(1), Err(OpError("that terminal is gone".into())))]
    );

    let home = "P %5 @4 $0 1 1 0 0  0 ";
    feed(&b, &mut st, notified.clone());
    let fx = refreshed(&b, &mut st, &[home, BOTH[1]], &["T %5 "]);
    let want = BTreeMap::from([row("%5", "shell")]);
    assert_eq!(published_terms(&fx), std::slice::from_ref(&want));
    let fx = show(&b, &mut st, "%5");
    assert_eq!(reply_text(&fx), Ok("went to shell".to_string()));

    // Labelled again by hand, it goes by that.
    feed(&b, &mut st, notified);
    let fx = refreshed(&b, &mut st, &[home, BOTH[1]], &["T %5 ⌨ kite"]);
    let want = BTreeMap::from([row("%5", "kite")]);
    assert_eq!(published_terms(&fx), std::slice::from_ref(&want));
}

/// The text of the one reply in `fx`, or its refusal.
fn reply_text(fx: &[Effect]) -> Result<String, String> {
    match &replies(fx)[..] {
        [(_, Ok(done))] => Ok(done.text.clone()),
        [(_, Err(OpError(why)))] => Err(why.clone()),
        other => panic!("one reply: {other:?}"),
    }
}

/// Only the terminals' list failed: the other three still rebuild the topology, which has
/// no terminals, and the log says why in one line.
#[test]
fn a_failed_terminals_list_keeps_the_rest_of_the_listing() {
    let (b, mut st) = listed_with(&["T %5 ⌨ kite"]);
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::WindowClose {
            window: WindowId(3),
        }),
    );
    let mut results = lists(&[BOTH[0], "P %7 @7 $0 1 0 0 0  0 "], &[]);
    results[3] = Err("unknown filter\nmore".into());
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results,
        },
    );
    let topology = &st.tmux().topology;
    assert!(topology.has_pane(PaneId(7)) && !topology.has_pane(PaneId(6)));
    assert!(terms(&st).is_empty());
    let logged: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some((l.level, l.message.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        logged,
        [(
            LogLevel::Warn,
            "tmux couldn't list moa's terminals: unknown filter"
        )]
    );
}

/// A listing that changes no terminal publishes none.
#[test]
fn the_same_terminals_again_publish_nothing() {
    let (b, mut st) = listed_with(&["T %5 ⌨ kite"]);
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::WindowClose {
            window: WindowId(9),
        }),
    );
    let fx = refreshed(&b, &mut st, BOTH, &["T %5 ⌨ kite"]);
    assert!(published_terms(&fx).is_empty(), "{fx:?}");
}

/// `term` tags its new tab: the topic has it at once, before any listing shows it.
#[test]
fn opening_a_terminal_publishes_it_at_once() {
    let (b, mut st) = listed_with(&[]);
    let fx = send(
        &b,
        &mut st,
        1,
        Op::TermOpen {
            path: b.home.join("work"),
            title: Some("beacon".into()),
        },
    );
    let [(n, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec!["P %7 777".into()])],
        },
    );
    let want = BTreeMap::from([row("%7", "beacon")]);
    assert_eq!(published_terms(&fx), std::slice::from_ref(&want));
    assert_eq!(terms(&st), want);
}

/// A listing asked before the tag went out doesn't show it: the terminal stays while the
/// listing shows its pane, and goes with the pane.
#[test]
fn a_listing_older_than_the_tag_keeps_the_terminal() {
    let (b, mut st) = loaded(TOML);
    // The link's first listing is in flight.
    link_up(&b, &mut st);
    let fx = send(
        &b,
        &mut st,
        1,
        Op::TermOpen {
            path: b.home.join("work"),
            title: Some("beacon".into()),
        },
    );
    let [(n, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec!["P %7 777".into()])],
        },
    );
    let with_7 = ["P %7 @4 $0 1 1 0 0  0 "];
    let fx = refreshed(&b, &mut st, &with_7, &[]);
    assert!(published_terms(&fx).is_empty(), "{fx:?}");
    assert_eq!(terms(&st), BTreeMap::from([row("%7", "beacon")]));

    // Closed: a listing without its pane drops it.
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::WindowClose {
            window: WindowId(4),
        }),
    );
    refreshed(&b, &mut st, BOTH, &[]);
    assert!(terms(&st).is_empty());
}

/// A terminal closed is gone from the next listing, so from the topic.
#[test]
fn a_terminal_closing_leaves_the_topic() {
    let (b, mut st) = listed_with(&["T %5 ⌨ kite work"]);
    feed(
        &b,
        &mut st,
        TmuxIn::Notification(Notification::WindowClose {
            window: WindowId(2),
        }),
    );
    let fx = refreshed(&b, &mut st, &["P %6 @3 $0 1 1 0 0  0 "], &[]);
    assert_eq!(published_terms(&fx), [BTreeMap::new()]);
    assert!(terms(&st).is_empty());
}

/// `term.show` selects the terminal's tab and pane, on its own: nothing is saved.
#[test]
fn term_show_selects_the_terminal() {
    let (b, mut st) = listed_with(&["T %5 ⌨ kite"]);
    let fx = show(&b, &mut st, "%5");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        fx,
        [
            Effect::Tmux {
                conn: 1,
                cmds: vec![
                    Cmd::new("select-window").args(["-t", "%5"]),
                    Cmd::new("select-pane").args(["-t", "%5"]),
                ],
                token: None,
            },
            Effect::Reply {
                conn: moa_core::op::ConnId(1),
                req: ReqId(1),
                result: Ok(Done {
                    text: "went to kite".into(),
                    data: json!({ "pane": "%5", "name": "kite" }),
                }),
            },
        ]
    );
}

/// A pane that isn't a terminal moa knows of, or isn't a pane id, is gone: nothing is sent.
#[test]
fn term_show_of_a_non_terminal_says_gone() {
    let (b, mut st) = listed_with(&["T %5 ⌨ kite"]);
    for pane in ["%6", "%99", "kite"] {
        let fx = show(&b, &mut st, pane);
        assert_eq!(
            replies(&fx),
            [(ReqId(1), Err(OpError("that terminal is gone".into())))],
            "{pane}"
        );
        assert!(
            !fx.iter().any(|e| matches!(e, Effect::Tmux { .. })),
            "{fx:?}"
        );
    }
}

/// With no link, no tab can be shown.
#[test]
fn term_show_without_the_link_says_so() {
    let (b, mut st) = loaded(TOML);
    let fx = show(&b, &mut st, "%5");
    assert_eq!(
        replies(&fx),
        [(
            ReqId(1),
            Err(OpError(
                "moa isn't open in a terminal yet; run moa first".into()
            ))
        )]
    );
}
