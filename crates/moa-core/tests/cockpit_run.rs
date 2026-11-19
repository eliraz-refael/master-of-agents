//! The cockpit queue (L17) through `handle`: layout switching, `goto`, Repair and the lazy
//! cockpit (L6, L9, L12, L13, L18). Every operation takes a listing of its own, sends one
//! chain, and the next starts only once that chain is answered.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Down, Effect, Input, LogLevel, Publish, TmuxIn, TmuxToken, handle_checked};
use moa_core::layout::{CURRENT_KEY, Leaf};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::panel::View;
use moa_core::publish::{CoreEvent, Topic};
use moa_core::state::{CallerInfo, Deps, PanelLaunch, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};

const TTY: &str = "/dev/ttys1";
const LOST: &str = "moa lost its terminal; try again";
const NO_TERMINAL: &str = "moa isn't open in a terminal yet; run moa first";

fn now() -> Now {
    Now::at(
        UtcMs(1_000_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn feed(st: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(st, Input::Tmux(t), now())
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

/// A started core whose panels are `/x/moa` at home `/h`, its link up. The link's own first
/// listing shows no session `moa`, so it builds nothing by itself.
fn up() -> State {
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
    feed(&mut st, refresh(lists(&[], false)));
    st
}

/// One `list-panes` line: `%N` in window `@W` of session `$S`, `in_cockpit`, tagged with
/// `view` (empty: none), dead or not.
fn pane(id: u32, window: u32, session: u32, in_cockpit: bool, view: &str, dead: bool) -> String {
    format!(
        "P %{id} @{window} ${session} 1 1 0 {} {} {} {view}",
        u8::from(in_cockpit),
        "",
        u8::from(dead)
    )
}

/// A panel in the cockpit (`@1` in `moa`).
fn shown(id: u32, view: &str) -> String {
    pane(id, 1, 0, true, view, false)
}

/// A panel parked in `_moa`, in a window of its own (`@id`).
fn parked(id: u32, view: &str) -> String {
    pane(id, 100 + id, 1, false, view, false)
}

/// The listing's three answers: sessions `moa` (`$0`) and `_moa` (`$1`), `panes`, and moa's
/// control client, plus a user's terminal on `moa` when `user`.
fn lists(panes: &[String], user: bool) -> Vec<Result<Vec<String>, String>> {
    let sessions = if panes.iter().any(|p| p.contains(" $0 ")) {
        vec!["S $0 moa".to_string(), "S $1 _moa".to_string()]
    } else {
        vec!["S $1 _moa".to_string()]
    };
    let mut clients = vec!["C 1 $1 client-9".to_string()];
    if user {
        clients.push(format!("C 0 $0 {TTY}"));
    }
    vec![Ok(sessions), Ok(panes.to_vec()), Ok(clients)]
}

fn refresh(results: Vec<Result<Vec<String>, String>>) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results,
    }
}

fn answer(n: u64, results: Vec<Result<Vec<String>, String>>) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results,
    }
}

/// The shell in the cockpit (`%1`); tree, console, plan and an empty slot parked (`%2`–`%5`).
fn shell_and_parked() -> Vec<String> {
    vec![
        shown(1, ""),
        parked(2, "tree"),
        parked(3, "console"),
        parked(4, "plan"),
        parked(5, "empty-slot"),
    ]
}

/// `cockpit` built: tree `%2`, console `%3`, plan `%4`; the empty slot `%5` parked, and the
/// old shell `%1` in a tab of its own.
fn cockpit_built() -> Vec<String> {
    vec![
        pane(1, 6, 0, false, "", false),
        shown(2, "tree"),
        shown(3, "console"),
        shown(4, "plan"),
        parked(5, "empty-slot"),
    ]
}

/// `focus` built: tree `%2`, plan `%4`, the empty slot `%5`, console `%3`.
fn focus_built() -> Vec<String> {
    vec![
        pane(1, 6, 0, false, "", false),
        shown(2, "tree"),
        shown(4, "plan"),
        shown(5, "empty-slot"),
        shown(3, "console"),
    ]
}

/// Each tokened query in `fx`: its number and its commands.
fn queries(fx: &[Effect]) -> Vec<(u64, Vec<Cmd>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Query(n)),
            } => Some((*n, cmds.clone())),
            _ => None,
        })
        .collect()
}

/// The one listing an operation asked in `fx`: its query number.
fn the_listing(fx: &[Effect]) -> u64 {
    match &queries(fx)[..] {
        [(n, cmds)] if *cmds == refresh_cmds() => *n,
        other => panic!("not one listing: {other:?}"),
    }
}

/// Each command of a chain (or of a plain command), as one line of its words.
fn lines(cmd: &Cmd) -> Vec<String> {
    cmd.parts()
        .iter()
        .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
        .collect()
}

/// The one chain asked in `fx`: its query number and its lines.
fn the_chain(fx: &[Effect]) -> (u64, Vec<String>) {
    match &queries(fx)[..] {
        [(n, cmds)] if cmds.len() == 1 && cmds[0] != refresh_cmds()[0] => {
            assert_eq!(cmds[0].check(), Ok(()));
            (*n, lines(&cmds[0]))
        }
        other => panic!("not one chain: {other:?}"),
    }
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
        .map(|c| {
            assert_eq!(c.argv()[..2], ["-c", TTY]);
            c.argv().last().unwrap().clone()
        })
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

fn reply_text(fx: &[Effect]) -> Result<String, String> {
    match &replies(fx)[..] {
        [(_, Ok(done))] => Ok(done.text.clone()),
        [(_, Err(OpError(e)))] => Err(e.clone()),
        other => panic!("not one reply: {other:?}"),
    }
}

fn logs(fx: &[Effect]) -> Vec<(LogLevel, String)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some((l.level, l.message.clone())),
            _ => None,
        })
        .collect()
}

fn send(st: &mut State, req: u64, op: Op) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(req),
            caller: CallerInfo::default(),
            op,
        },
        now(),
    )
}

fn apply(st: &mut State, req: u64, name: &str) -> Vec<Effect> {
    send(
        st,
        req,
        Op::LayoutApply {
            name: Some(name.into()),
        },
    )
}

fn message(st: &mut State, text: &str) -> Vec<Effect> {
    feed(st, TmuxIn::Notification(Notification::Message(text.into())))
}

fn key(st: &mut State, word: &str) -> Vec<Effect> {
    message(st, &format!("moa:key {word} %3 $0 {TTY}"))
}

fn panel_starts(st: &State) -> u32 {
    st.status(&CallerInfo::default(), UtcMs(1_000_000))
        .panel_starts
}

/// The selects of `pane` (L6 step 7).
fn selects(pane: &str) -> [String; 2] {
    [
        format!("select-window -t {pane}"),
        format!("select-pane -t {pane}"),
    ]
}

fn selected(chain: &[String]) -> bool {
    chain.iter().any(|l| l.starts_with("select-"))
}

#[test]
fn layout_apply_lists_then_builds_then_remembers() {
    let mut st = up();
    let fx = apply(&mut st, 1, "focus");
    let n = the_listing(&fx);
    assert!(replies(&fx).is_empty());

    // The panels are parked: one chain, and nothing started.
    let fx = feed(&mut st, answer(n, lists(&shell_and_parked(), true)));
    let (n, chain) = the_chain(&fx);
    assert!(
        !chain.iter().any(|l| l.starts_with("new-window")),
        "{chain:?}"
    );
    assert_eq!(chain[0], "swap-pane -d -s %2 -t %1");
    assert_eq!(chain[chain.len() - 2..], selects("%3"), "the console's");
    assert_eq!(
        st.meta(CURRENT_KEY),
        None,
        "nothing saved before the chain's answer"
    );

    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    let saved: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .flatten()
        .filter_map(|c| match c {
            moa_core::change::Change::Meta { key, value } => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        saved,
        [(CURRENT_KEY.to_string(), Some("focus".to_string()))]
    );
    assert_eq!(st.meta(CURRENT_KEY), Some("focus"));
    let models: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Model(m)) => Some(m.clone()),
            _ => None,
        })
        .collect();
    assert!(models.len() >= 2, "layouts, then panels: {fx:?}");
    assert_eq!(
        models[0].topics[&Topic::Layouts]["focus"]["current"],
        serde_json::json!(true)
    );
    assert!(fx.contains(&Effect::Publish(Publish::Event(CoreEvent::Ui {
        text: "layout → focus".into()
    }))));
    assert_eq!(reply_text(&fx), Ok("layout focus applied".into()));
    assert!(queries(&fx).is_empty(), "nothing waits");
    assert_eq!(st.tmux().queries().count(), 0);
}

#[test]
fn two_layout_keys_run_one_after_the_other() {
    let mut st = up();
    let first = the_listing(&key(&mut st, "layout-2"));
    // The second waits: nothing asked, one operation in flight.
    let fx = key(&mut st, "layout-4");
    assert!(queries(&fx).is_empty() && notices(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.tmux().queries().collect::<Vec<_>>(), [first]);

    let (chain, _) = the_chain(&feed(
        &mut st,
        answer(first, lists(&shell_and_parked(), true)),
    ));
    // A third key while one runs and one waits takes the waiting one's place.
    let fx = key(&mut st, "layout-1");
    assert!(queries(&fx).is_empty());
    assert_eq!(notices(&fx), ["moa: replaced by a later layout request"]);

    // Only the chain's answer starts the next, which lists again.
    let fx = feed(&mut st, answer(chain, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: layout focus applied"]);
    let next = the_listing(&fx);

    // Planned from that listing: `focus` is built, so the empty slot goes home.
    let fx = feed(&mut st, answer(next, lists(&focus_built(), true)));
    let (chain, lines) = the_chain(&fx);
    assert!(
        lines.contains(&"break-pane -d -s %5 -t =_moa:".to_string()),
        "{lines:?}"
    );
    assert_eq!(lines[lines.len() - 2..], selects("%3"));

    // A waiting Repair is never replaced: a build after it waits behind it, and the next
    // build replaces only that one.
    for k in ["repair", "layout-3"] {
        let fx = key(&mut st, k);
        assert!(
            queries(&fx).is_empty() && notices(&fx).is_empty(),
            "{k}: {fx:?}"
        );
    }
    let fx = key(&mut st, "layout-4");
    assert_eq!(notices(&fx), ["moa: replaced by a later layout request"]);
    let fx = feed(&mut st, answer(chain, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: layout cockpit applied"]);
    // Repair's turn: nothing dead, so it applies the current layout, selecting nothing.
    let repair = the_listing(&fx);
    let fx = feed(&mut st, answer(repair, lists(&cockpit_built(), true)));
    let (chain, lines) = the_chain(&fx);
    assert!(!selected(&lines), "{lines:?}");
    let fx = feed(&mut st, answer(chain, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: layout cockpit applied"]);
    // Then the last build, `triage`.
    let triage = the_listing(&fx);
    let (_, lines) = the_chain(&feed(
        &mut st,
        answer(triage, lists(&cockpit_built(), true)),
    ));
    assert!(
        lines.contains(&"set-option -p -t %2 @moa_view status".to_string()),
        "{lines:?}"
    );
}

#[test]
fn a_failed_build_says_so_and_the_next_apply_mends_it() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "focus"));
    let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    let fx = feed(&mut st, answer(n, vec![Err("can't find pane: %4".into())]));
    assert_eq!(
        reply_text(&fx),
        Err("layout focus couldn't be applied: can't find pane: %4; Ctrl-a r tries again".into())
    );
    assert!(!fx.iter().any(|e| matches!(e, Effect::Persist(_))));
    assert_eq!(st.meta(CURRENT_KEY), None);
    assert_eq!(st.tmux().queries().count(), 0, "nothing in flight");

    let n = the_listing(&apply(&mut st, 2, "focus"));
    let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(reply_text(&fx), Ok("layout focus applied".into()));
    assert_eq!(st.meta(CURRENT_KEY), Some("focus"));
}

#[test]
fn a_layout_too_big_for_the_window_is_said_plainly() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "focus"));
    let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    let fx = feed(
        &mut st,
        answer(
            n,
            vec![Err("size or position no space for a new pane".into())],
        ),
    );
    assert_eq!(
        reply_text(&fx),
        Err("the focus layout doesn't fit this window; make the terminal bigger".into())
    );
}

#[test]
fn the_layout_keys_apply_the_presets() {
    for (k, name) in [
        ("layout-1", "cockpit"),
        ("layout-2", "focus"),
        ("layout-3", "classic"),
        ("layout-4", "triage"),
    ] {
        let mut st = up();
        let fx = message(&mut st, &format!("moa:key {k} %3 $1 {TTY}"));
        let n = the_listing(&fx);
        let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
        let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
        assert_eq!(notices(&fx), [format!("moa: layout {name} applied")], "{k}");
        assert_eq!(st.meta(CURRENT_KEY), Some(name));
    }
}

#[test]
fn goto_selects_or_reapplies() {
    let goto = |st: &mut State, leaf: Leaf| send(st, 1, Op::Goto { panel: leaf });
    let tree = Leaf::Panel(View::Tree);

    // In the cockpit: only the select.
    let mut st = up();
    let n = the_listing(&goto(&mut st, tree));
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(&cockpit_built(), true))));
    assert_eq!(lines, selects("%2"));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(reply_text(&fx), Ok("tree".into()));
    assert_eq!(st.meta(CURRENT_KEY), None, "goto doesn't change the layout");

    // Missing: the current layout is built, then the tree is selected.
    let mut missing = cockpit_built();
    missing[1] = parked(2, "tree");
    let mut st = up();
    let n = the_listing(&goto(&mut st, tree));
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(&missing, true))));
    assert!(
        lines.iter().any(|l| l.starts_with("join-pane")),
        "{lines:?}"
    );
    assert_eq!(lines[lines.len() - 2..], selects("%2"));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(reply_text(&fx), Ok("tree".into()));

    // A panel the layout doesn't have.
    let mut st = up();
    let n = the_listing(&goto(&mut st, Leaf::Panel(View::Status)));
    let fx = feed(&mut st, answer(n, lists(&cockpit_built(), true)));
    assert!(queries(&fx).is_empty());
    assert_eq!(
        reply_text(&fx),
        Err("the cockpit layout has no status panel (moa layout list)".into())
    );

    // `agent` finds the empty slot.
    let mut st = up();
    let n = the_listing(&goto(&mut st, Leaf::Agent));
    let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(&focus_built(), true))));
    assert_eq!(lines, selects("%5"));
}

#[test]
fn repair_respawns_dead_panels_then_applies_from_a_new_listing() {
    let mut st = up();
    let before = panel_starts(&st);
    let mut dead = cockpit_built();
    dead[3] = pane(7, 1, 0, true, "plan", true);
    // An agent's tag on a dead pane in a tab of its own: no panel, so never restarted here.
    dead.push(pane(8, 7, 0, false, "agent", true));
    let n = the_listing(&key(&mut st, "repair"));
    let fx = feed(&mut st, answer(n, lists(&dead, true)));
    let [(n, cmds)] = &queries(&fx)[..] else {
        panic!("{fx:?}")
    };
    for c in cmds {
        assert_eq!(c.check(), Ok(()), "{}", c.line());
    }
    let respawns: Vec<_> = cmds.iter().flat_map(lines).collect();
    assert_eq!(
        respawns,
        [
            "respawn-pane -k -t %7 -c /h -e MOA_BRAIN=default -e MOA_NO_AUTOSTART=1 /x/moa panel plan --brain default"
        ]
    );
    let fx = feed(&mut st, answer(*n, vec![Ok(Vec::new())]));
    assert_eq!(panel_starts(&st), before + 1);
    // A listing taken after the respawn, in which %7 runs: the build, starting nothing.
    let n = the_listing(&fx);
    let mut alive = dead.clone();
    alive[3] = shown(7, "plan");
    let fx = feed(&mut st, answer(n, lists(&alive, true)));
    let (n, lines) = the_chain(&fx);
    assert!(
        !lines.iter().any(|l| l.starts_with("new-window")),
        "{lines:?}"
    );
    assert!(!selected(&lines), "{lines:?}");
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: repaired: 1 panel restarted"]);
    assert_eq!(panel_starts(&st), before + 1);
}

#[test]
fn a_dead_panel_is_logged_once() {
    let mut st = up();
    let mut dead = cockpit_built();
    dead[3] = pane(7, 1, 0, true, "plan", true);
    // The link knows the panel: no user is attached, so nothing is built.
    feed(&mut st, TmuxIn::Notification(Notification::SessionsChanged));
    feed(&mut st, refresh(lists(&dead, false)));
    let mut said = Vec::new();
    for _ in 0..2 {
        said.extend(logs(&message(&mut st, "moa:hook pane-died %7")));
    }
    assert_eq!(
        said,
        [(LogLevel::Info, "panel plan in %7 stopped".to_string())]
    );
}

/// The listing after a client attached: a user's terminal, and only a shell in the cockpit.
#[test]
fn the_first_attach_builds_the_cockpit() {
    let mut st = up();
    let n = the_listing(&message(
        &mut st,
        &format!("moa:hook client-attached {TTY}"),
    ));
    let fx = feed(&mut st, answer(n, lists(&[shown(1, "")], true)));
    let [(n, spawns)] = &queries(&fx)[..] else {
        panic!("{fx:?}")
    };
    let views: Vec<_> = spawns
        .iter()
        .map(|c| {
            let at = c.argv().iter().position(|a| a == "-n").unwrap();
            c.argv()[at + 1].clone()
        })
        .collect();
    assert_eq!(views, ["tree", "console", "plan"]);
    let fx = feed(
        &mut st,
        answer(
            *n,
            vec![
                Ok(vec!["%2".into()]),
                Ok(vec!["%3".into()]),
                Ok(vec!["%4".into()]),
            ],
        ),
    );
    assert_eq!(panel_starts(&st), 3);
    let n = the_listing(&fx);
    let now = vec![
        shown(1, ""),
        parked(2, "tree"),
        parked(3, "console"),
        parked(4, "plan"),
    ];
    let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(&now, true))));
    assert!(!selected(&lines), "nothing is selected: {lines:?}");
    assert!(lines.contains(&"break-pane -d -s %1 -t =moa: -n ⌨ shell".to_string()));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert!(notices(&fx).is_empty() && replies(&fx).is_empty() && queries(&fx).is_empty());
    assert_eq!(st.meta(CURRENT_KEY), None);
}

#[test]
fn two_attaches_build_the_cockpit_once() {
    let mut st = up();
    let attach = format!("moa:hook client-attached {TTY}");
    let n = the_listing(&message(&mut st, &attach));
    assert!(
        queries(&message(&mut st, &attach)).is_empty(),
        "one at a time"
    );
    let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    let next = the_listing(&feed(&mut st, answer(n, vec![Ok(Vec::new())])));
    // The second finds the cockpit built and does nothing.
    let fx = feed(&mut st, answer(next, lists(&cockpit_built(), true)));
    assert!(queries(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.tmux().queries().count(), 0);
}

#[test]
fn a_control_client_attaching_builds_nothing() {
    let mut st = up();
    let n = the_listing(&message(&mut st, "moa:hook client-attached client-9"));
    let fx = feed(&mut st, answer(n, lists(&shell_and_parked(), false)));
    assert!(queries(&fx).is_empty() && logs(&fx).is_empty(), "{fx:?}");
}

#[test]
fn an_attach_before_moa_knows_session_moa_still_builds() {
    let mut st = up();
    assert_eq!(st.tmux().topology.session_named("moa"), None);
    let n = the_listing(&message(
        &mut st,
        &format!("moa:hook client-attached {TTY}"),
    ));
    let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    assert!(
        lines.iter().any(|l| l.starts_with("join-pane")),
        "{lines:?}"
    );
}

#[test]
fn a_missing_cockpit_window_is_made_again() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "cockpit"));
    // Session `moa` holds only the shell's tab, not named `moa`.
    let mut panes = shell_and_parked();
    panes[0] = pane(1, 6, 0, false, "", false);
    let (_, lines) = the_chain(&feed(&mut st, answer(n, lists(&panes, true))));
    assert_eq!(
        lines[..3],
        [
            "break-pane -d -s %2 -t =moa: -n moa",
            "resize-window -A -t %2",
            "set-option -w -u -t %2 window-size",
        ]
    );

    // With no session `moa` at all, nothing is done.
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "cockpit"));
    let fx = feed(&mut st, answer(n, lists(&[parked(2, "tree")], true)));
    assert!(queries(&fx).is_empty());
    assert_eq!(reply_text(&fx), Err(NO_TERMINAL.into()));
}

#[test]
fn link_up_with_a_user_attached_builds_it() {
    let mut st = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut st, Input::Startup(Box::default()), now());
    feed(&mut st, connected());
    let fx = feed(&mut st, refresh(lists(&[shown(1, "")], true)));
    let n = the_listing(&fx);
    let fx = feed(&mut st, answer(n, lists(&shell_and_parked(), true)));
    let (_, lines) = the_chain(&fx);
    assert!(!selected(&lines));

    // With the cockpit built, a new link builds nothing.
    let mut st = up();
    feed(&mut st, TmuxIn::Down(Down::Lost));
    feed(&mut st, connected());
    let fx = feed(&mut st, refresh(lists(&cockpit_built(), true)));
    assert!(queries(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_lost_link_drops_all_but_the_vacates() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "focus"));
    assert!(queries(&apply(&mut st, 2, "triage")).is_empty());
    key(&mut st, "repair");
    let fx = feed(&mut st, TmuxIn::Down(Down::Lost));
    let mut got = replies(&fx);
    got.sort_by_key(|(r, _)| r.0);
    assert_eq!(
        got,
        [
            (ReqId(1), Err(OpError(LOST.into()))),
            (ReqId(2), Err(OpError(LOST.into()))),
        ]
    );
    assert!(
        notices(&fx).is_empty(),
        "a key's request is dropped with nothing said"
    );
    assert_eq!(
        logs(&fx),
        [(
            LogLevel::Info,
            "a cockpit change was dropped: moa lost its terminal".to_string()
        )]
    );
    // Its listing's late answer, on a connection gone, starts nothing.
    feed(&mut st, connected());
    let fx = feed(&mut st, answer(n, lists(&shell_and_parked(), true)));
    assert!(queries(&fx).is_empty(), "{fx:?}");
}

/// R2: a spawn the link lost under it: the operation is dropped, with one line said.
#[test]
fn a_spawn_whose_answer_never_comes_starts_nothing_else() {
    let mut st = up();
    let n = the_listing(&message(
        &mut st,
        &format!("moa:hook client-attached {TTY}"),
    ));
    let fx = feed(&mut st, answer(n, lists(&[shown(1, "")], true)));
    assert_eq!(queries(&fx).len(), 1, "the spawns");
    let fx = feed(&mut st, TmuxIn::Down(Down::Lost));
    assert!(queries(&fx).is_empty() && notices(&fx).is_empty(), "{fx:?}");
    assert_eq!(logs(&fx).len(), 1, "{fx:?}");
    assert_eq!(st.tmux().queries().count(), 0);
    assert_eq!(panel_starts(&st), 0);
}

/// R2: three spawns, the second refused: the operation plans again once, from a listing
/// that has the other two, and says which panel didn't start.
#[test]
fn one_failed_spawn_is_said_after_one_new_plan() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "cockpit"));
    let fx = feed(&mut st, answer(n, lists(&[shown(1, "")], true)));
    let [(n, spawns)] = &queries(&fx)[..] else {
        panic!("{fx:?}")
    };
    assert_eq!(spawns.len(), 3);
    let fx = feed(
        &mut st,
        answer(
            *n,
            vec![
                Ok(vec!["%2".into()]),
                Err("create window failed".into()),
                Ok(vec!["%4".into()]),
            ],
        ),
    );
    assert_eq!(panel_starts(&st), 2);
    let n = the_listing(&fx);
    let now = vec![shown(1, ""), parked(2, "tree"), parked(4, "plan")];
    let fx = feed(&mut st, answer(n, lists(&now, true)));
    assert!(queries(&fx).is_empty(), "no second spawn: {fx:?}");
    assert_eq!(
        reply_text(&fx),
        Err("couldn't start the console panel; see panel.log".into())
    );
}

/// A name `layouts.json` doesn't have is said at once, with no listing.
#[test]
fn an_unknown_layout_is_refused_at_once() {
    let mut st = up();
    let fx = apply(&mut st, 1, "nope");
    assert!(queries(&fx).is_empty());
    assert_eq!(
        reply_text(&fx),
        Err("no layout 'nope' (have: cockpit, focus, classic, triage)".into())
    );
    // The queue goes on.
    the_listing(&apply(&mut st, 2, "focus"));
}

/// A goto never takes a waiting layout switch's place, nor a switch a goto's: only a build
/// of the same kind replaces another.
#[test]
fn a_goto_and_a_layout_key_never_replace_each_other() {
    let mut st = up();
    let first = the_listing(&key(&mut st, "layout-1"));
    for k in ["layout-2", "tree"] {
        let fx = key(&mut st, k);
        assert!(
            queries(&fx).is_empty() && notices(&fx).is_empty(),
            "{k}: {fx:?}"
        );
    }
    // Each kind replaces its own.
    for k in ["layout-3", "console"] {
        let fx = key(&mut st, k);
        assert_eq!(
            notices(&fx),
            ["moa: replaced by a later layout request"],
            "{k}"
        );
    }
    // In order: `classic`, then the goto to the console.
    let (n, _) = the_chain(&feed(
        &mut st,
        answer(first, lists(&shell_and_parked(), true)),
    ));
    let next = the_listing(&feed(&mut st, answer(n, vec![Ok(Vec::new())])));
    let (n, _) = the_chain(&feed(
        &mut st,
        answer(next, lists(&shell_and_parked(), true)),
    ));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: layout classic applied"]);
    let next = the_listing(&fx);
    let (n, lines) = the_chain(&feed(&mut st, answer(next, lists(&cockpit_built(), true))));
    assert_eq!(lines, selects("%3"));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: console"]);
}

/// L2: a `cockpit` entry that `layouts.json` breaks falls back to the preset, for
/// `layout apply` with no name and for `goto`, as it does for the lazy build.
#[test]
fn a_broken_current_layout_falls_back_to_the_preset() {
    let deps = Deps {
        home: PathBuf::from("/h"),
        panel: PanelLaunch {
            exe: PathBuf::from("/x/moa"),
            env: Vec::new(),
        },
        read_text: |_| {
            Ok(Some(
                r#"{"layouts": {"cockpit": {"panel": "nope"}}}"#.into(),
            ))
        },
        ..Deps::default()
    };
    for op in [
        Op::LayoutApply { name: None },
        Op::Goto {
            panel: Leaf::Panel(View::Tree),
        },
    ] {
        let mut st =
            State::new(BrainName::default_brain(), Config::default()).with_deps(deps.clone());
        handle_checked(&mut st, Input::Startup(Box::default()), now());
        feed(&mut st, connected());
        feed(&mut st, refresh(lists(&[], false)));
        let n = the_listing(&send(&mut st, 1, op.clone()));
        let (n, lines) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
        assert!(
            lines.iter().any(|l| l.starts_with("join-pane")),
            "{op:?}: {lines:?}"
        );
        let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
        assert!(reply_text(&fx).is_ok(), "{op:?}: {fx:?}");
    }
}

/// A chain's answer for an operation a reload dropped is ignored: nothing is saved or said,
/// and the operation in flight goes on.
#[test]
fn a_stale_chain_answer_is_ignored() {
    let mut st = up();
    let n = the_listing(&apply(&mut st, 1, "focus"));
    let (old, _) = the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
    let fx = handle_checked(
        &mut st,
        Input::Reload {
            snapshot: Box::default(),
            refuse_writes: None,
        },
        now(),
    );
    assert_eq!(reply_text(&fx), Err(LOST.into()));
    let n = the_listing(&apply(&mut st, 2, "triage"));
    let fx = feed(&mut st, answer(old, vec![Ok(Vec::new())]));
    assert!(replies(&fx).is_empty() && queries(&fx).is_empty(), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Effect::Persist(_))));
    assert_eq!(st.meta(CURRENT_KEY), None);
    // The one in flight still plans from its own listing.
    the_chain(&feed(&mut st, answer(n, lists(&shell_and_parked(), true))));
}

/// L13: `c t p g` say where they went, once there.
#[test]
fn the_goto_keys_say_where_they_went() {
    for (k, panes) in [
        ("console", cockpit_built()),
        ("tree", cockpit_built()),
        ("plan", cockpit_built()),
        ("agent", focus_built()),
    ] {
        let mut st = up();
        let n = the_listing(&key(&mut st, k));
        let (n, _) = the_chain(&feed(&mut st, answer(n, lists(&panes, true))));
        let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
        assert_eq!(notices(&fx), [format!("moa: {k}")], "{k}");
    }
}

/// A replaced build gives up its place: the newer one waits at the end, so keys of the two
/// kinds still run in the order they were pressed.
#[test]
fn keys_keep_their_order() {
    let mut st = up();
    let first = the_listing(&key(&mut st, "layout-1"));
    for k in ["tree", "layout-2"] {
        assert!(queries(&key(&mut st, k)).is_empty(), "{k}");
    }
    let fx = key(&mut st, "console");
    assert_eq!(notices(&fx), ["moa: replaced by a later layout request"]);
    // `focus` first, as it was pressed after the goto `console` replaced.
    let (n, _) = the_chain(&feed(
        &mut st,
        answer(first, lists(&shell_and_parked(), true)),
    ));
    let next = the_listing(&feed(&mut st, answer(n, vec![Ok(Vec::new())])));
    let (n, _) = the_chain(&feed(
        &mut st,
        answer(next, lists(&shell_and_parked(), true)),
    ));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: layout focus applied"]);
    let next = the_listing(&fx);
    let (n, _) = the_chain(&feed(&mut st, answer(next, lists(&focus_built(), true))));
    let fx = feed(&mut st, answer(n, vec![Ok(Vec::new())]));
    assert_eq!(notices(&fx), ["moa: console"]);
}
