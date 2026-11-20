//! S9's sheets through `handle`, the link played by hand: the picker's panel, parked in
//! `_moa`, has its tab linked into your session by `find` and unlinked by its close, by
//! leaving its tab, by a detach, or by a listing after a restart. Each open and close runs
//! in the cockpit's queue (L17), from a listing of its own.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Down, Effect, Input, LogLevel, Publish, TmuxIn, TmuxToken, handle_checked};
use moa_core::op::{ConnId, NoParams, Op, OpError, ReqId};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_core::state::{CallerInfo, Deps, PanelLaunch, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};
use serde_json::Value;

use common::{queries, replies, the_listing};

const TTY: &str = "/dev/ttys1";
/// The wall clock every input here comes at, in milliseconds: the first open's number.
const WALL: i64 = 1_000_000;
/// The picker's panel, its tab and its window, parked in `_moa` (`$1`).
const PICKER: u32 = 7;
const PICKER_TAB: u32 = 107;
/// The picker panel's connection.
const PANEL: u64 = 5;

fn now() -> Now {
    Now::at(
        UtcMs(WALL),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn feed(st: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(st, Input::Tmux(t), now())
}

fn send_on(st: &mut State, conn: u64, req: u64, op: Op) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(conn),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op,
        },
        now(),
    )
}

fn connected() -> TmuxIn {
    TmuxIn::Connected {
        conn: 1,
        generation: Generation {
            pid: 10,
            start: 1000,
        },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
        socket: "moa".into(),
    }
}

/// One `list-panes` line: `%id` in window `@window` of session `$session`, in the cockpit
/// or not, tagged with `view` (empty: none), dead or not.
fn pane(id: u32, window: u32, session: u32, in_cockpit: bool, view: &str, dead: bool) -> String {
    format!(
        "P %{id} @{window} ${session} 1 1 0 {}  {} {view}",
        u8::from(in_cockpit),
        u8::from(dead)
    )
}

/// `cockpit` built in `@1`: tree `%2`, console `%3`, plan `%4`.
fn cockpit() -> Vec<String> {
    vec![
        pane(2, 1, 0, true, "tree", false),
        pane(3, 1, 0, true, "console", false),
        pane(4, 1, 0, true, "plan", false),
    ]
}

/// The cockpit, and the picker parked in `_moa` (alive or not), linked into each of
/// `linked` too: `list-panes -a` lists `_moa` first.
fn with_picker(dead: bool, linked: &[u32]) -> Vec<String> {
    let mut panes = cockpit();
    panes.push(pane(PICKER, PICKER_TAB, 1, false, "picker", dead));
    for s in linked {
        panes.push(pane(PICKER, PICKER_TAB, *s, false, "picker", dead));
    }
    panes
}

/// The listing's four answers: sessions `moa` (`$0`), `_moa` (`$1`) and `work` (`$2`),
/// `panes`, moa's control client and a user's terminal on each of `users`' sessions.
fn lists(panes: &[String], users: &[(&str, u32)]) -> Vec<Result<Vec<String>, String>> {
    let sessions = vec!["S $0 moa".into(), "S $1 _moa".into(), "S $2 work".into()];
    let mut clients = vec!["C 1 $1 client-9".to_string()];
    clients.extend(users.iter().map(|(tty, s)| format!("C 0 ${s} {tty}")));
    vec![
        Ok(sessions),
        Ok(panes.to_vec()),
        Ok(clients),
        Ok(Vec::new()),
    ]
}

/// A user's terminal on `moa`.
fn user() -> &'static [(&'static str, u32)] {
    &[(TTY, 0)]
}

fn answer(n: u64, results: Vec<Result<Vec<String>, String>>) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results,
    }
}

fn ok() -> Vec<Result<Vec<String>, String>> {
    vec![Ok(Vec::new())]
}

/// A started core whose panels are `/x/moa` at home `/h`, its link up, whose first listing
/// shows `panes`.
fn up(panes: &[String]) -> State {
    let deps = Deps {
        home: PathBuf::from("/h"),
        panel: PanelLaunch {
            exe: PathBuf::from("/x/moa"),
            env: Vec::new(),
        },
        ..Deps::default()
    };
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::default()), now());
    feed(&mut st, connected());
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(panes, user()),
        },
    );
    assert!(
        queries(&fx).is_empty(),
        "the first listing asks nothing: {fx:?}"
    );
    st
}

/// `word` pressed by the user in session `$session`.
fn key_in(st: &mut State, word: &str, session: u32) -> Vec<Effect> {
    let text = format!("moa:key {word} %3 ${session} {TTY}");
    feed(st, TmuxIn::Notification(Notification::Message(text)))
}

fn find(st: &mut State) -> Vec<Effect> {
    key_in(st, "find", 0)
}

fn notify(st: &mut State, n: Notification) -> Vec<Effect> {
    feed(st, TmuxIn::Notification(n))
}

/// Session `$session` shows window `@window` now.
fn window_changed(session: u32, window: u32) -> Notification {
    Notification::SessionWindowChanged {
        session: TmuxSessionId(session),
        window: WindowId(window),
    }
}

/// Each command of a chain (or of a plain command), as one line of its words.
fn lines(cmd: &Cmd) -> Vec<String> {
    cmd.parts()
        .iter()
        .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
        .collect()
}

/// The one query asked in `fx` that isn't a listing: its number and its lines, each
/// command checked against the allow-list.
fn the_chain(fx: &[Effect]) -> (u64, Vec<String>) {
    match &queries(fx)[..] {
        [(n, cmds)] if *cmds != refresh_cmds() => {
            for c in cmds {
                assert_eq!(c.check(), Ok(()), "{}", c.line());
            }
            (*n, cmds.iter().flat_map(lines).collect())
        }
        other => panic!("not one chain: {other:?}"),
    }
}

/// An open's chain: `first` (the link, or the select), then the picker's tab sized to the
/// terminal and handed back to tmux's sizing, since no terminal was ever in its parked tab.
fn opens(first: String) -> [String; 3] {
    [
        first,
        format!("resize-window -A -t %{PICKER}"),
        format!("set-option -w -u -t %{PICKER} window-size"),
    ]
}

/// Every command `fx` sends, tokened or not, as lines.
fn sent(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds),
            _ => None,
        })
        .flatten()
        .flat_map(lines)
        .collect()
}

/// The notices in `fx`, as their texts.
fn notices(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name() == "display-message")
        .map(|c| c.argv().last().unwrap().clone())
        .collect()
}

fn panel_starts(st: &State) -> u32 {
    st.status(&CallerInfo::default(), UtcMs(WALL)).panel_starts
}

/// The picker's `panels` row, as last published.
fn picker_row(st: &State) -> Value {
    st.model().topics[&Topic::Panels][&format!("c{PANEL}")].clone()
}

/// Whether `fx` publishes `panels`.
fn publishes_panels(fx: &[Effect]) -> bool {
    fx.iter().any(|e| {
        matches!(e, Effect::Publish(Publish::Model(m))
            if m.topics.contains_key(&Topic::Panels))
    })
}

/// The picker's panel registers on its connection, as one moa placed does.
fn picker_registers(st: &mut State) -> Vec<Effect> {
    send_on(
        st,
        PANEL,
        1,
        Op::UiPanel {
            pane: Some(format!("%{PICKER}")),
            view: Some(View::Picker),
            placed: true,
            once: false,
        },
    )
}

fn ui_close(st: &mut State, conn: u64) -> Vec<Effect> {
    send_on(st, conn, 2, Op::UiClose(NoParams))
}

/// `find` from `$session`, with the picker parked and linked into `linked`, ends in one
/// chain: its lines, once tmux answered it.
fn opened_in(st: &mut State, session: u32, linked: &[u32]) -> Vec<String> {
    let n = the_listing(&key_in(st, "find", session));
    let users: &[(&str, u32)] = &[(TTY, 0), ("/dev/ttys2", 2)];
    let fx = feed(st, answer(n, lists(&with_picker(false, linked), users)));
    let (n, lines) = the_chain(&fx);
    let fx = feed(st, answer(n, ok()));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert!(notices(&fx).is_empty(), "{fx:?}");
    lines
}

/// The picker opened in `moa`, with its panel registered.
fn open_picker() -> State {
    let mut st = up(&with_picker(false, &[]));
    picker_registers(&mut st);
    assert_eq!(
        opened_in(&mut st, 0, &[]),
        opens(format!("link-window -s %{PICKER} -t $0:"))
    );
    st
}

#[test]
fn the_first_find_starts_the_picker_then_links_it() {
    let mut st = up(&cockpit());
    let starts = panel_starts(&st);
    let n = the_listing(&find(&mut st));

    // No picker: it starts parked, tagged as a sheet: framed, so tmux draws no label.
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    let [(n, cmds)] = &queries(&fx)[..] else {
        panic!("not one spawn: {fx:?}")
    };
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].check(), Ok(()));
    let spawn = lines(&cmds[0]).join(" ");
    assert!(spawn.starts_with("new-window -d -P"), "{spawn}");
    // Named as its frame is: the status line shows the name while it is open.
    assert_eq!(
        cmds[0]
            .argv()
            .windows(2)
            .find(|w| w[0] == "-n")
            .map(|w| &w[1][..]),
        Some("find a session")
    );
    assert!(
        spawn.ends_with("/x/moa panel picker --brain default"),
        "{spawn}"
    );

    let fx = feed(&mut st, answer(*n, vec![Ok(vec![format!("%{PICKER}")])]));
    let tags = sent(&fx);
    let p = format!("%{PICKER}");
    for tag in [
        format!("set-option -p -t {p} @moa_view picker"),
        format!("set-option -p -t {p} @moa_framed 1"),
        format!("set-option -p -t {p} remain-on-exit on"),
    ] {
        assert!(tags.contains(&tag), "{tag}: {tags:?}");
    }
    assert_eq!(panel_starts(&st), starts + 1);

    // A fresh listing, then the link into the session the key came from.
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("link-window -s {p} -t $0:")));
    assert_eq!(st.sheet(PaneId(PICKER)), None, "recorded once tmux answers");

    let fx = feed(&mut st, answer(n, ok()));
    assert_eq!(st.sheet(PaneId(PICKER)), Some(WALL));
    assert!(notices(&fx).is_empty(), "an open says nothing: {fx:?}");
    assert!(queries(&fx).is_empty());
}

/// The first open starts the panel, which registers only after the link: its row still
/// reads its open.
#[test]
fn a_panel_registering_after_the_link_reads_its_open() {
    let mut st = up(&with_picker(false, &[]));
    opened_in(&mut st, 0, &[]);
    let fx = picker_registers(&mut st);
    assert!(publishes_panels(&fx));
    let row = picker_row(&st);
    assert_eq!(row["sheet"], serde_json::json!(WALL), "{row}");
    assert_eq!(row["managed"], serde_json::json!(true), "{row}");
}

/// With the picker parked, an open is one listing and one link: nothing starts, and each
/// open has a number of its own.
#[test]
fn a_second_find_starts_nothing() {
    let mut st = open_picker();
    let starts = panel_starts(&st);
    let first = st.sheet(PaneId(PICKER)).unwrap();
    // Closed: its tab is unlinked.
    let n = the_listing(&ui_close(&mut st, PANEL));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, _) = the_chain(&fx);
    feed(&mut st, answer(n, ok()));
    assert_eq!(picker_row(&st)["sheet"], Value::Null);

    let n = the_listing(&find(&mut st));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("link-window -s %{PICKER} -t $0:")));
    feed(&mut st, answer(n, ok()));
    let second = st.sheet(PaneId(PICKER)).unwrap();
    assert!(second > first, "{second} after {first}");
    assert_eq!(picker_row(&st)["sheet"], serde_json::json!(second));
    assert_eq!(panel_starts(&st), starts, "no panel started");
}

/// A picker whose program stopped is restarted in place, never left parked beside a new one.
#[test]
fn a_dead_picker_is_restarted_in_place() {
    let mut st = up(&with_picker(true, &[]));
    let starts = panel_starts(&st);
    let n = the_listing(&find(&mut st));
    let fx = feed(&mut st, answer(n, lists(&with_picker(true, &[]), user())));
    let [(n, cmds)] = &queries(&fx)[..] else {
        panic!("not one respawn: {fx:?}")
    };
    let respawn = cmds.iter().flat_map(lines).collect::<Vec<_>>().join(" ; ");
    assert!(
        respawn.starts_with(&format!("respawn-pane -k -t %{PICKER} ")),
        "{respawn}"
    );
    assert!(
        respawn.ends_with("/x/moa panel picker --brain default"),
        "{respawn}"
    );
    assert!(!respawn.contains("new-window"), "{respawn}");

    let fx = feed(&mut st, answer(*n, ok()));
    assert_eq!(panel_starts(&st), starts + 1);
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("link-window -s %{PICKER} -t $0:")));
}

/// The picker's tab is in the session already (after a restart, or from another key): it is
/// selected there, never linked twice.
#[test]
fn find_while_linked_selects() {
    let mut st = up(&cockpit());
    let n = the_listing(&find(&mut st));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("select-window -t $0:@{PICKER_TAB}")));
    feed(&mut st, answer(n, ok()));
    assert_eq!(st.sheet(PaneId(PICKER)), Some(WALL));
    assert_eq!(st.tmux().queries().count(), 0, "no close of it waits");
}

/// Its close unlinks the tab from every user's session it is in, and never from `_moa`,
/// where it waits for the next open.
#[test]
fn ui_close_unlinks_from_every_user_session() {
    let mut st = open_picker();
    assert_eq!(
        opened_in(&mut st, 2, &[0]),
        opens(format!("link-window -s %{PICKER} -t $2:"))
    );
    let fx = ui_close(&mut st, PANEL);
    assert!(matches!(&replies(&fx)[..], [(ReqId(2), Ok(_))]), "{fx:?}");
    let n = the_listing(&fx);
    let users: &[(&str, u32)] = &[(TTY, 0), ("/dev/ttys2", 2)];
    let fx = feed(
        &mut st,
        answer(n, lists(&with_picker(false, &[0, 2]), users)),
    );
    let (n, chain) = the_chain(&fx);
    assert_eq!(
        chain,
        [
            format!("unlink-window -t $0:@{PICKER_TAB}"),
            format!("unlink-window -t $2:@{PICKER_TAB}"),
        ]
    );
    assert_eq!(picker_row(&st)["sheet"], Value::Null, "closed as it plans");
    assert!(publishes_panels(&fx));
    let fx = feed(&mut st, answer(n, ok()));
    assert!(queries(&fx).is_empty() && notices(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.sheet(PaneId(PICKER)), None);
}

/// A pick takes you to another tab, then closes the picker: two closes, of which the second
/// finds nothing left to unlink (a second unlink would fail).
#[test]
fn a_pick_unsheets_once() {
    let mut st = open_picker();
    let mut unlinks = 0;
    let n = the_listing(&notify(&mut st, window_changed(0, 1)));
    // The close the panel asks waits its turn.
    let fx = ui_close(&mut st, PANEL);
    assert!(queries(&fx).is_empty(), "{fx:?}");

    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, chain) = the_chain(&fx);
    unlinks += chain
        .iter()
        .filter(|l| l.starts_with("unlink-window"))
        .count();
    let n = the_listing(&feed(&mut st, answer(n, ok())));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    unlinks += sent(&fx)
        .iter()
        .filter(|l| l.starts_with("unlink-window"))
        .count();
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(unlinks, 1);
    assert_eq!(st.tmux().queries().count(), 0);
}

/// Finds pressed while a build runs leave one open waiting: each newer one takes the place
/// of the one before, and nobody hears of it.
#[test]
fn repeated_finds_queue_one() {
    let mut st = up(&with_picker(false, &[]));
    let build = the_listing(&key_in(&mut st, "layout-1", 0));
    for _ in 0..3 {
        let fx = find(&mut st);
        assert!(queries(&fx).is_empty() && notices(&fx).is_empty(), "{fx:?}");
    }
    let fx = feed(
        &mut st,
        answer(build, lists(&with_picker(false, &[]), user())),
    );
    let (n, _) = the_chain(&fx);
    let fx = feed(&mut st, answer(n, ok()));
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("link-window -s %{PICKER} -t $0:")));
    let fx = feed(&mut st, answer(n, ok()));
    assert!(queries(&fx).is_empty(), "one open only: {fx:?}");
    assert_eq!(st.tmux().queries().count(), 0);
}

#[test]
fn ui_close_from_another_panel_is_refused() {
    let mut st = open_picker();
    let only = Err(OpError("only the picker and help close this way".into()));
    // A tree panel, and a connection that never said which panel it is.
    send_on(
        &mut st,
        8,
        1,
        Op::UiPanel {
            pane: Some("%2".into()),
            view: Some(View::Tree),
            placed: true,
            once: false,
        },
    );
    for conn in [8, 9] {
        let fx = ui_close(&mut st, conn);
        assert_eq!(replies(&fx), [(ReqId(2), only.clone())], "{conn}");
        assert!(queries(&fx).is_empty(), "{conn}");
    }
    assert_eq!(st.sheet(PaneId(PICKER)), Some(WALL));
}

/// A tab you leave the picker's for (`Ctrl-a ]`, say) closes it, so it never stays behind
/// as a "picker" tab. Its own link selecting it, or `_moa` changing, doesn't.
#[test]
fn leaving_the_sheet_tab_unlinks_it() {
    let mut st = open_picker();
    for n in [window_changed(0, PICKER_TAB), window_changed(1, 2)] {
        let fx = notify(&mut st, n.clone());
        assert!(queries(&fx).is_empty(), "{n:?}: {fx:?}");
    }
    let n = the_listing(&notify(&mut st, window_changed(0, 1)));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}

/// The last terminal on the picker's session detaching closes it: you never come back to a
/// stale picker. Another terminal still there keeps it open.
#[test]
fn detaching_unlinks_the_sheet() {
    let mut st = up(&with_picker(false, &[]));
    let n = the_listing(&find(&mut st));
    let terminals: &[(&str, u32)] = &[(TTY, 0), ("/dev/ttys2", 0), ("/dev/ttys3", 2)];
    let fx = feed(
        &mut st,
        answer(n, lists(&with_picker(false, &[]), terminals)),
    );
    let (n, _) = the_chain(&fx);
    feed(&mut st, answer(n, ok()));

    let detached = |tty: &str| Notification::ClientDetached {
        client: ClientName(tty.into()),
    };
    // One of two on `moa`, then the last on `work`, which the picker isn't in.
    for tty in ["/dev/ttys2", "/dev/ttys3"] {
        let fx = notify(&mut st, detached(tty));
        assert!(queries(&fx).is_empty(), "{tty}: {fx:?}");
    }
    let n = the_listing(&notify(&mut st, detached(TTY)));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), &[])));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}

/// moa forgets its opens when it restarts: the first listing of a new daemon that finds the
/// picker still linked into a user's session unlinks it.
#[test]
fn a_listing_unlinks_a_stale_sheet() {
    let deps = Deps {
        home: PathBuf::from("/h"),
        ..Deps::default()
    };
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::default()), now());
    feed(&mut st, connected());
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker(false, &[0]), user()),
        },
    );
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
    let fx = feed(&mut st, answer(n, ok()));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    // The next listing shows it parked: nothing more.
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker(false, &[]), user()),
        },
    );
    assert!(queries(&fx).is_empty(), "{fx:?}");
}

/// tmux says `%window-close` for every unlink, though the window lives on in `_moa`: it is
/// only a reason to list again. The panel stays registered and tagged, no pane is gone, and
/// the next open works as before.
#[test]
fn an_unlink_kills_nothing() {
    let mut st = open_picker();
    let n = the_listing(&ui_close(&mut st, PANEL));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, _) = the_chain(&fx);
    feed(&mut st, answer(n, ok()));

    let fx = notify(
        &mut st,
        Notification::WindowClose {
            window: WindowId(PICKER_TAB),
        },
    );
    let relists = fx.iter().any(|e| {
        matches!(
            e,
            Effect::Tmux {
                token: Some(TmuxToken::Refresh),
                ..
            }
        )
    });
    assert!(relists, "{fx:?}");
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Persist(_) | Effect::Log(_))),
        "{fx:?}"
    );
    assert!(queries(&fx).is_empty() && notices(&fx).is_empty(), "{fx:?}");
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker(false, &[]), user()),
        },
    );
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Persist(_) | Effect::Log(_))),
        "{fx:?}"
    );
    assert!(queries(&fx).is_empty(), "{fx:?}");
    let row = picker_row(&st);
    assert_eq!(row["view"], "picker", "{row}");
    assert_eq!(row["pane"], format!("%{PICKER}"), "{row}");
    assert_eq!(row["managed"], true, "{row}");
    assert_eq!(row["sheet"], Value::Null, "{row}");
    assert_eq!(st.tmux().topology.view_of(PaneId(PICKER)), Some("picker"));
    assert_eq!(st.sheet(PaneId(PICKER)), None);

    assert_eq!(
        opened_in(&mut st, 0, &[]),
        opens(format!("link-window -s %{PICKER} -t $0:"))
    );
    assert_eq!(picker_row(&st)["sheet"], serde_json::json!(WALL + 1));
}

/// `_moa` holds every parked panel: the picker can't open there.
#[test]
fn find_from_moas_hidden_session_says_so() {
    let mut st = up(&with_picker(false, &[]));
    let n = the_listing(&key_in(&mut st, "find", 1));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(notices(&fx), ["moa: open moa's tab first"]);
}

/// A picker that couldn't start is said once its spawn's listing shows none. tmux refused
/// the start, so its words are in moa's log.
#[test]
fn a_picker_that_cant_start_says_so() {
    let mut st = up(&cockpit());
    let n = the_listing(&find(&mut st));
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    let [(n, _)] = &queries(&fx)[..] else {
        panic!("not one spawn: {fx:?}")
    };
    let fx = feed(
        &mut st,
        answer(*n, vec![Err("no space for new window".into())]),
    );
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(
        notices(&fx),
        ["moa: the picker couldn't start; see moa's log"]
    );
}

/// `help` opens its own sheet, as `find` opens the picker's.
#[test]
fn help_opens_its_own_sheet() {
    let mut st = up(&cockpit());
    let n = the_listing(&key_in(&mut st, "help", 0));
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    let [(_, cmds)] = &queries(&fx)[..] else {
        panic!("not one spawn: {fx:?}")
    };
    let spawn = lines(&cmds[0]).join(" ");
    assert!(
        spawn.ends_with("/x/moa panel help --brain default"),
        "{spawn}"
    );
}

/// Task 9 review M1: a close the queue dropped with the link (it waited behind a build)
/// forgets the open, so the first listing on the new connection finds the tab linked with no
/// open and closes it: you never come back to a stale picker.
#[test]
fn a_close_a_lost_link_dropped_still_happens() {
    let mut st = open_picker();
    the_listing(&key_in(&mut st, "layout-1", 0));
    let fx = notify(&mut st, window_changed(0, 1));
    assert!(queries(&fx).is_empty(), "the close waits: {fx:?}");

    feed(&mut st, TmuxIn::Down(Down::Lost));
    assert_eq!(st.sheet(PaneId(PICKER)), None);
    assert_eq!(picker_row(&st)["sheet"], Value::Null);

    feed(&mut st, connected());
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker(false, &[0]), user()),
        },
    );
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}

/// Task 9 review M2: sessions in a group share their tabs, so the first unlink takes the
/// tab from both, and the second finds none. The close is done, and nothing is logged.
/// Another refusal is still logged (nobody asked the close).
#[test]
fn a_grouped_session_closes_quietly() {
    let mut st = open_picker();
    opened_in(&mut st, 2, &[0]);
    let n = the_listing(&ui_close(&mut st, PANEL));
    let users: &[(&str, u32)] = &[(TTY, 0), ("/dev/ttys2", 2)];
    let fx = feed(
        &mut st,
        answer(n, lists(&with_picker(false, &[0, 2]), users)),
    );
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain.len(), 2, "{chain:?}");
    let gone = format!("can't find window: @{PICKER_TAB}");
    let fx = feed(&mut st, answer(n, vec![Err(gone)]));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Log(_))), "{fx:?}");
    assert!(queries(&fx).is_empty(), "{fx:?}");

    opened_in(&mut st, 0, &[]);
    let n = the_listing(&ui_close(&mut st, PANEL));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, _) = the_chain(&fx);
    let fx = feed(&mut st, answer(n, vec![Err("no such thing".into())]));
    let warned = fx
        .iter()
        .filter(|e| matches!(e, Effect::Log(l) if l.level == LogLevel::Warn))
        .count();
    assert_eq!(warned, 2, "tmux's words, and the plain line: {fx:?}");
}

/// Task 9 review M3: a sheet is never a panel's to pick, and a pane tagged with a sheet's
/// view outside `_moa` is no sheet of moa's: its tab, linked out, could never be unlinked.
#[test]
fn only_a_parked_panel_is_a_sheet() {
    let mut st = up(&cockpit());
    for view in [View::Picker, View::Help] {
        let fx = send_on(
            &mut st,
            1,
            1,
            Op::UiSelectView {
                pane: "%2".into(),
                view,
            },
        );
        let refused = Err(OpError(format!("{view} can't be chosen for a panel")));
        assert_eq!(replies(&fx), [(ReqId(1), refused)], "{view}");
        assert!(queries(&fx).is_empty(), "{view}");
    }

    // A cockpit pane tagged `help` (by hand, say): help starts a panel of its own.
    let mut panes = cockpit();
    panes[1] = pane(3, 1, 0, true, "help", false);
    let mut st = up(&panes);
    let n = the_listing(&key_in(&mut st, "help", 0));
    let fx = feed(&mut st, answer(n, lists(&panes, user())));
    let [(_, cmds)] = &queries(&fx)[..] else {
        panic!("not one spawn: {fx:?}")
    };
    let spawn = lines(&cmds[0]).join(" ");
    assert!(spawn.starts_with("new-window"), "{spawn}");
    assert!(spawn.ends_with("panel help --brain default"), "{spawn}");
}

/// Task 9 review M4: a picker whose restart failed is never linked dead: the key is told it
/// couldn't start, and where to look. tmux refused the restart: moa's log has its words. It
/// restarted and stopped again: the panel said why in `panel.log`.
#[test]
fn a_picker_whose_restart_failed_isnt_linked() {
    for (answered, log) in [
        (Err("no such tab".to_string()), "moa's log"),
        (Ok(Vec::new()), "panel.log"),
    ] {
        let mut st = up(&with_picker(true, &[]));
        let n = the_listing(&find(&mut st));
        let fx = feed(&mut st, answer(n, lists(&with_picker(true, &[]), user())));
        let [(n, _)] = &queries(&fx)[..] else {
            panic!("not one respawn: {fx:?}")
        };
        let fx = feed(&mut st, answer(*n, vec![answered]));
        let n = the_listing(&fx);
        let fx = feed(&mut st, answer(n, lists(&with_picker(true, &[]), user())));
        assert!(queries(&fx).is_empty(), "{fx:?}");
        assert_eq!(
            notices(&fx),
            [format!("moa: the picker couldn't start; see {log}")]
        );
        assert_eq!(st.sheet(PaneId(PICKER)), None);
    }
}

/// Final review M5: help is named plainly when it can't start.
#[test]
fn help_that_cant_start_says_so_plainly() {
    let mut st = up(&cockpit());
    let n = the_listing(&key_in(&mut st, "help", 0));
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    let [(n, _)] = &queries(&fx)[..] else {
        panic!("not one spawn: {fx:?}")
    };
    let n = the_listing(&feed(&mut st, answer(*n, vec![Err("no space".into())])));
    let fx = feed(&mut st, answer(n, lists(&cockpit(), user())));
    assert_eq!(notices(&fx), ["moa: help couldn't start; see moa's log"]);
}

/// The cockpit, and the picker parked in `_moa` and linked into each of `linked`, where it
/// isn't the tab shown (`#{window_active}` is 0): `moa` shows the cockpit.
fn with_picker_behind(linked: &[u32]) -> Vec<String> {
    let mut panes = with_picker(false, &[]);
    for s in linked {
        panes.push(format!("P %{PICKER} @{PICKER_TAB} ${s} 1 0 0 0  0 picker"));
    }
    panes
}

/// Final review M2: a tab left while its own open was still in flight (that news is
/// dropped then) is closed by the next listing, which shows its session on another tab.
#[test]
fn a_sheet_left_during_its_open_is_closed_by_the_next_listing() {
    let mut st = up(&with_picker(false, &[]));
    let n = the_listing(&find(&mut st));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[]), user())));
    let (n, _) = the_chain(&fx);
    let fx = notify(&mut st, window_changed(0, 1));
    assert!(queries(&fx).is_empty(), "the open is under way: {fx:?}");
    let fx = feed(&mut st, answer(n, ok()));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.sheet(PaneId(PICKER)), Some(WALL));

    // The link's own listing (its `%window-add` asked one) shows `$0` on the cockpit.
    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker_behind(&[0]), user()),
        },
    );
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker_behind(&[0]), user())));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}

/// Final review M2: an open sheet whose leave came while the link was lost is closed by the
/// first listing on the new connection. One still shown stays.
#[test]
fn a_sheet_left_while_the_link_was_lost_is_closed() {
    let mut st = open_picker();
    feed(&mut st, TmuxIn::Down(Down::Lost));
    feed(&mut st, connected());
    let shown = TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: lists(&with_picker(false, &[0]), user()),
    };
    let fx = feed(&mut st, shown);
    assert!(queries(&fx).is_empty(), "still shown: {fx:?}");

    let fx = feed(
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: lists(&with_picker_behind(&[0]), user()),
        },
    );
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&with_picker_behind(&[0]), user())));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}

/// Task 9 review M5: with no link, a close can't run, and `ui.close` says so.
#[test]
fn ui_close_with_no_link_says_so() {
    let mut st = open_picker();
    feed(&mut st, TmuxIn::Down(Down::Lost));
    let fx = ui_close(&mut st, PANEL);
    let no = Err(OpError(
        "moa isn't open in a terminal yet; run moa first".into(),
    ));
    assert_eq!(replies(&fx), [(ReqId(2), no)]);
}

/// PR review: with the picker open, `Ctrl-a c` (a build) then `Ctrl-a s` (an open, waiting
/// its turn). The build's select takes the terminal off the picker's tab, but that close
/// would replace the open you asked for since: the open runs, and nothing closes it.
#[test]
fn an_automatic_close_never_replaces_a_newer_open() {
    let mut st = open_picker();
    let build = the_listing(&key_in(&mut st, "console", 0));
    let fx = find(&mut st);
    assert!(queries(&fx).is_empty(), "the open waits: {fx:?}");
    let fx = feed(
        &mut st,
        answer(build, lists(&with_picker(false, &[0]), user())),
    );
    let (n, _) = the_chain(&fx);
    let fx = notify(&mut st, window_changed(0, 1));
    assert!(queries(&fx).is_empty(), "{fx:?}");

    let n = the_listing(&feed(&mut st, answer(n, ok())));
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), user())));
    let (n, chain) = the_chain(&fx);
    assert_eq!(chain, opens(format!("select-window -t $0:@{PICKER_TAB}")));
    let fx = feed(&mut st, answer(n, ok()));
    assert!(queries(&fx).is_empty(), "nothing closes it: {fx:?}");
    assert_eq!(st.tmux().queries().count(), 0);
    assert_eq!(st.sheet(PaneId(PICKER)), Some(WALL + 1));
}

/// PR review: a terminal that switches to another session leaves the picker's session as a
/// detach does. The last one to leave closes the picker; one leaving while another still
/// shows the session doesn't.
#[test]
fn switching_sessions_unlinks_the_sheet() {
    let mut st = up(&with_picker(false, &[]));
    let n = the_listing(&find(&mut st));
    let terminals: &[(&str, u32)] = &[(TTY, 0), ("/dev/ttys2", 0)];
    let fx = feed(
        &mut st,
        answer(n, lists(&with_picker(false, &[]), terminals)),
    );
    let (n, _) = the_chain(&fx);
    feed(&mut st, answer(n, ok()));

    let switched = |tty: &str| Notification::ClientSessionChanged {
        client: ClientName(tty.into()),
        session: TmuxSessionId(2),
    };
    let fx = notify(&mut st, switched("/dev/ttys2"));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    let n = the_listing(&notify(&mut st, switched(TTY)));
    let gone: &[(&str, u32)] = &[(TTY, 2), ("/dev/ttys2", 2)];
    let fx = feed(&mut st, answer(n, lists(&with_picker(false, &[0]), gone)));
    let (_, chain) = the_chain(&fx);
    assert_eq!(chain, [format!("unlink-window -t $0:@{PICKER_TAB}")]);
}
