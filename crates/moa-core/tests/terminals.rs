//! Every terminal is recorded when moa sees it (5d, T3): each listing, once the link's tabs
//! are reconciled, is compared with the open records. A new shell gets a record (`found` on
//! the connection's first look, `you` after), a gone one's record closes, and a changed
//! program or folder is updated. The `terms` topic stays the topology's, each row filled in
//! from its record.

mod common;

use std::cell::Cell;
use std::path::PathBuf;

use moa_core::change::Change;
use moa_core::effect::{Down, Effect, Input, RefuseWrites, TmuxIn, TmuxToken, handle_checked};
use moa_core::model::{
    Activity, AgentKind, Inspected, Lifecycle, Location, OpenedBy, PaneAgent, PaneBinding, Session,
    SessionId, TermId, Terminal,
};
use moa_core::op::Op;
use moa_core::panel::View;
use moa_core::state::{Deps, Snapshot, State};
use moa_core::terminals::open_for;
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::link::Link;
use moa_core::tmux::notify::Notification;
use moa_core::{Now, UtcMs};
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

/// A session moa launched, live: its agent runs in `%7` (pid 1007) whenever [`AGENT_IN_7`]
/// says so, and names it by `MOA_SESSION`.
const S: &str = "00000000-0000-4000-8000-0000000000c1";

thread_local! {
    static AGENT_IN_7: Cell<bool> = const { Cell::new(true) };
}

/// The tabs `%5`, `%6` and `%7`, each alone in its own tab of the session `moa`.
const P5: &str = "P %5 @2 $0 1 1 0 0  0 ";
const P6: &str = "P %6 @3 $0 1 0 0 0  0 ";
const P7: &str = "P %7 @4 $0 1 0 0 0  0 ";

fn s_id() -> SessionId {
    S.parse().unwrap()
}

fn session_s(b: &TypicalBrain) -> Session {
    let cwd = b.home.join("work/lantern").display().to_string();
    Session {
        id: s_id(),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.clone()),
        start_cwd: Some(cwd),
        project_id: None,
        title: Some("lantern".into()),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity: Activity::Idle,
        activity_at: Some(UtcMs(1)),
        activity_why: None,
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(1)),
    }
}

/// S's agent in `%7`, while it runs there; nothing in any other tab.
fn inspect(pid: i64, _: &[String]) -> Inspected {
    let runs = pid == 1007 && AGENT_IN_7.with(Cell::get);
    Inspected::from(runs.then(|| PaneAgent {
        kind: AgentKind::Claude,
        store_dir: None,
        conversation: None,
        moa_session: Some(S.into()),
        moa_brain: None,
        title: None,
        group: None,
        started_at: Some(UtcMs(1)),
    }))
}

/// Every tab's process is a `zsh`, but `%6`'s and `%11`'s, which can't be read.
fn shell_name(pid: u32) -> Option<String> {
    (pid != 1006 && pid != 1011).then(|| "zsh".to_string())
}

/// `pane`'s shell, unmarked, at its prompt in `cwd`.
fn plain(pane: &str, cwd: &str) -> String {
    pane_line(pane, "", "zsh", cwd, "")
}

/// `pane`'s line in reconcile's listing (`PANE_INFO`): untagged, in `/r`.
fn info(pane: &str) -> String {
    let pid = 1000 + pane.trim_start_matches('%').parse::<u32>().unwrap();
    format!("I\t{pane}\t{pid}\t0\t\t\t\t/r")
}

/// An open record of terminal `pane` on the link's server, made an hour ago: `sh` at its
/// prompt in `/r`, as [`term_line`] lists it.
fn record(id: i64, pane: &str, name: &str) -> Terminal {
    let g = generation();
    Terminal {
        id: TermId(id),
        server_pid: g.pid,
        server_start: g.start,
        pane_id: pane.into(),
        opened_by: OpenedBy::Found,
        cwd: "/r".into(),
        session_id: None,
        opened_at: UtcMs(1),
        touched_at: None,
        name: name.into(),
        note: None,
        timeout_at: None,
        shell: "sh".into(),
        command: "sh".into(),
        command_since: UtcMs(1),
        closed_at: None,
    }
}

/// The records `fx` saved.
fn records(fx: &[Effect]) -> Vec<Terminal> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Terminal(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// The terminal marks (`@moa_term`, `@moa_label`) `fx` sets.
fn marks(fx: &[Effect]) -> Vec<Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| {
            let argv = c.argv();
            c.name() == "set-option"
                && argv.first().is_some_and(|a| a == "-p")
                && argv.get(1).is_some_and(|a| a == "-t")
                && argv
                    .get(3)
                    .is_some_and(|a| a == "@moa_term" || a == "@moa_label")
        })
        .cloned()
        .collect()
}

/// What marks `pane` a terminal named `shell`.
fn marked(pane: &str) -> Vec<Cmd> {
    vec![
        Cmd::new("set-option").args(["-p", "-t", pane, "@moa_term", "1"]),
        Cmd::new("set-option").args(["-p", "-t", pane, "@moa_label", "⌨ shell"]),
    ]
}

/// The typical brain's time, `ms` later.
fn later(b: &TypicalBrain, ms: i64) -> Now {
    let mut now = b.now();
    now.wall = UtcMs(now.wall.0 + ms);
    now
}

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    /// The typical brain with S live and `open` the open records, linked on connection 1;
    /// its first listing asked, the start drain not yet run.
    fn linked(open: Vec<Terminal>) -> Rig {
        Rig::linked_with(|snap| {
            snap.next_ids.terminal = open.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
            snap.terminals = open;
        })
    }

    /// [`Rig::linked`] with no records, its snapshot changed by `f`.
    fn linked_with(f: impl FnOnce(&mut Snapshot)) -> Rig {
        AGENT_IN_7.with(|a| a.set(true));
        let b = TypicalBrain::new();
        let deps = Deps {
            session_plugin: PathBuf::from(PLUGIN),
            home: b.home.clone(),
            inspect,
            process_name: shell_name,
            ..Deps::default()
        };
        let mut snap = b.snapshot();
        snap.sessions.push(session_s(&b));
        f(&mut snap);
        let mut st = loaded_with(&b, TOML, deps, snap);
        link_up(&b, &mut st);
        Rig { b, st }
    }

    /// Linked with no records, `%5` (in `/r/a`) and `%6` (in `/r/b`) listed as plain shells,
    /// and reconciled: the first look found both.
    fn found() -> Rig {
        let mut r = Rig::linked(Vec::new());
        r.list(&[P5, P6], &[plain("%5", "/r/a"), plain("%6", "/r/b")]);
        let fx = r.reconcile(&[&info("%5"), &info("%6")]);
        assert_eq!(records(&fx).len(), 2, "{fx:?}");
        r
    }

    /// tmux answers a refresh with these lists. No user is attached, so moa builds no
    /// cockpit meanwhile.
    fn list(&mut self, panes: &[&str], terms: &[String]) -> Vec<Effect> {
        let now = self.b.now();
        self.list_at(now, panes, terms)
    }

    /// [`Rig::list`] at `now`.
    fn list_at(&mut self, now: Now, panes: &[&str], terms: &[String]) -> Vec<Effect> {
        self.list_as(now, &["S $0 moa", "S $1 _moa"], panes, terms, &[])
    }

    /// [`Rig::list_at`] with these `sessions` and user `clients` (`LIST_CLIENTS` lines).
    fn list_as(
        &mut self,
        now: Now,
        sessions: &[&str],
        panes: &[&str],
        terms: &[String],
        clients: &[&str],
    ) -> Vec<Effect> {
        let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
        let mut results = lists(panes, terms);
        results[0] = lines(sessions);
        results[2] = lines(clients);
        refresh_answered(&mut self.st, now, results)
    }

    /// tmux tells of `n` at `now`.
    fn notify(&mut self, now: Now, n: Notification) -> Vec<Effect> {
        handle_checked(&mut self.st, Input::Tmux(TmuxIn::Notification(n)), now)
    }

    /// The start drain, then reconcile's listing answered with `info` (`PANE_INFO` lines).
    fn reconcile(&mut self, info: &[&str]) -> Vec<Effect> {
        handle_checked(&mut self.st, Input::StartDrained, self.b.now());
        let fx = listed(&mut self.st, &self.b, info, &[]);
        assert!(self.st.tmux().reconciled(), "{fx:?}");
        fx
    }

    fn conn(&self) -> u64 {
        let Link::Up { conn, .. } = self.st.tmux().link else {
            panic!("the link isn't up");
        };
        conn
    }

    fn open(&self, pane: u32) -> Option<&Terminal> {
        open_for(&self.st, &PaneId(pane))
    }
}

/// A `@moa_term` tag changes how reconcile binds an agent, so nothing is recorded or tagged
/// before the tabs are reconciled: an agent moa launched whose tab lost its tag is still
/// bound tracked. Reconcile's end looks once, as the connection's first look: the other
/// shell is found.
#[test]
fn registration_waits_for_reconcile() {
    let mut r = Rig::linked(Vec::new());
    let tabs = [plain("%5", "/r/a"), pane_line("%7", "", "claude", "/r", "")];
    for _ in 0..2 {
        let fx = r.list(&[P5, P7], &tabs);
        assert!(records(&fx).is_empty(), "{fx:?}");
        assert!(marks(&fx).is_empty(), "{fx:?}");
    }
    let fx = r.reconcile(&[&info("%5"), &info("%7")]);
    let binding = r.st.binding(s_id()).expect("S is bound");
    assert_eq!(binding.pane_id, "%7");
    assert!(binding.tracked, "{binding:?}");
    let found = records(&fx);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        (found[0].pane_id.as_str(), found[0].opened_by),
        ("%5", OpenedBy::Found)
    );
    assert_eq!(marks(&fx), marked("%5"));
    assert!(r.open(7).is_none());

    // Once: the next listing records nothing more.
    let fx = r.list(&[P5, P7], &tabs);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(marks(&fx).is_empty(), "{fx:?}");
}

/// The first look finds every open shell: its record says `found`, its shell is its
/// process's name (or, when that can't be read, the program listed), and its program and
/// folder are as listed. The agent's tab has none.
#[test]
fn the_first_run_finds_open_terminals() {
    let mut r = Rig::linked(Vec::new());
    r.list(
        &[P5, P6, P7],
        &[
            pane_line("%5", "", "node", "/r/a", ""),
            pane_line("%6", "", "bash", "/r/b", ""),
            pane_line("%7", "", "claude", "/r", ""),
        ],
    );
    let fx = r.reconcile(&[&info("%5"), &info("%6"), &info("%7")]);
    let now = r.b.now().wall;
    let five = Terminal {
        id: TermId(1),
        opened_by: OpenedBy::Found,
        cwd: "/r/a".into(),
        opened_at: now,
        name: "shell".into(),
        shell: "zsh".into(),
        command: "node".into(),
        command_since: now,
        ..record(1, "%5", "shell")
    };
    let six = Terminal {
        id: TermId(2),
        pane_id: "%6".into(),
        cwd: "/r/b".into(),
        shell: "bash".into(),
        command: "bash".into(),
        ..five.clone()
    };
    assert_eq!(records(&fx), [five.clone(), six.clone()]);
    assert_eq!(r.open(5), Some(&five));
    assert_eq!(r.open(6), Some(&six));
    assert!(r.open(7).is_none());
    let mut want = marked("%5");
    want.extend(marked("%6"));
    assert_eq!(marks(&fx), want);
}

/// After the first look, a split (marked `@moa_term` by its key) is yours, and needs no
/// mark.
#[test]
fn a_split_made_after_the_first_run_is_yours() {
    let mut r = Rig::found();
    let split = "P %8 @2 $0 0 1 0 0  0 ";
    let fx = r.list(
        &[P5, P6, split],
        &[
            plain("%5", "/r/a"),
            plain("%6", "/r/b"),
            pane_line("%8", "1", "zsh", "/r/a", "⌨ shell"),
        ],
    );
    let made = records(&fx);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(
        (
            made[0].pane_id.as_str(),
            made[0].opened_by,
            made[0].name.as_str()
        ),
        ("%8", OpenedBy::You, "shell")
    );
    assert_eq!(made[0].session_id, None);
    assert!(marks(&fx).is_empty(), "{fx:?}");
}

/// A shell opened with plain tmux keys is yours: it is recorded, then marked, so the picker
/// lists it, once the record is saved.
#[test]
fn a_new_shell_is_registered_as_yours() {
    let mut r = Rig::found();
    let fx = r.list(
        &[P5, P6, "P %8 @5 $0 1 0 0 0  0 "],
        &[
            plain("%5", "/r/a"),
            plain("%6", "/r/b"),
            plain("%8", "/r/c"),
        ],
    );
    let made = records(&fx);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(
        (made[0].pane_id.as_str(), made[0].opened_by, made[0].id),
        ("%8", OpenedBy::You, TermId(3))
    );
    assert_eq!(marks(&fx), marked("%8"));
    let persisted = fx.iter().position(|e| matches!(e, Effect::Persist(_)));
    let tagged = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { cmds, .. } if cmds[0] == marked("%8")[0]));
    assert!(persisted < tagged, "{fx:?}");
    assert_eq!(term_rows(&r.st)["%8"]["name"], "shell");
}

/// A tab moa is launching may be listed before tmux says which it is: while a launch, or a
/// terminal's new tab, waits, a new unmarked tab isn't recorded or marked. The next listing
/// records it; the launch's own tab is its session's.
#[test]
fn a_launch_in_flight_holds_back_new_panes() {
    let mut r = Rig::found();
    let open = Op::SessionOpen {
        path: r.b.home.join("private/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let (id, _) = the_open(&send(&r.b, &mut r.st, 1, open));
    let new = "P %8 @5 $0 1 0 0 0  0 ";
    let shells = [
        plain("%5", "/r/a"),
        plain("%6", "/r/b"),
        plain("%8", "/r/c"),
    ];
    let fx = r.list(&[P5, P6, new], &shells);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(marks(&fx).is_empty(), "{fx:?}");

    let conn = r.conn();
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %9 1009".into()])],
        },
    );
    let mut shells = shells.to_vec();
    shells.push(pane_line("%9", "", "claude", "/r", ""));
    let fx = r.list(&[P5, P6, new, "P %9 @6 $0 1 0 0 0  0 "], &shells);
    let made = records(&fx);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(made[0].pane_id, "%8");
    assert!(r.open(9).is_none());

    // A terminal's new tab, asked and not yet answered, holds them back too.
    let term = Op::TermOpen {
        path: r.b.home.join("work"),
        title: None,
    };
    send(&r.b, &mut r.st, 2, term);
    shells.push(plain("%10", "/r/d"));
    let fx = r.list(
        &[
            P5,
            P6,
            new,
            "P %9 @6 $0 1 0 0 0  0 ",
            "P %10 @7 $0 1 0 0 0  0 ",
        ],
        &shells,
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(r.open(10).is_none());
}

/// A listing whose pane list failed says nothing of what runs where: no record closes,
/// none is made, none changes.
#[test]
fn a_failed_pane_list_changes_nothing() {
    let mut r = Rig::found();
    let mut results = lists(&[P6, "P %8 @5 $0 1 0 0 0  0 "], &[]);
    results[2] = Ok(Vec::new());
    results[3] = Err("unknown format".into());
    let fx = refresh_answered(&mut r.st, r.b.now(), results);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(marks(&fx).is_empty(), "{fx:?}");
    assert!(r.open(5).is_some());
}

/// `term` records the terminal it opened as yours, with the name it was given (whole: only
/// the tab's name is cut), once tmux says which tab it has. The next listing makes no
/// second record.
#[test]
fn a_terminal_moa_opened_is_registered_with_its_name() {
    let mut r = Rig::found();
    let title = "beacon dev server for checkout, keep";
    let path = r.b.home.join("work");
    let op = Op::TermOpen {
        path: path.clone(),
        title: Some(title.into()),
    };
    let fx = send(&r.b, &mut r.st, 1, op);
    let [(n, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let conn = r.conn();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec!["P %8 1008".into()])],
        },
    );
    let now = r.b.now().wall;
    let want = Terminal {
        id: TermId(3),
        pane_id: "%8".into(),
        opened_by: OpenedBy::You,
        cwd: path.display().to_string(),
        opened_at: now,
        name: title.into(),
        shell: "zsh".into(),
        command: "zsh".into(),
        command_since: now,
        ..record(3, "%8", title)
    };
    assert_eq!(records(&fx), std::slice::from_ref(&want));
    assert_eq!(r.open(8), Some(&want));
    // The tab is marked with the cut name, after the record is saved.
    let persisted = fx.iter().position(|e| matches!(e, Effect::Persist(_)));
    let tagged = fx
        .iter()
        .position(|e| matches!(e, Effect::Tmux { cmds, .. } if cmds[0] == marked("%8")[0]));
    assert!(persisted < tagged, "{fx:?}");

    let fx = r.list(
        &[P5, P6, "P %8 @5 $0 1 0 0 0  0 "],
        &[
            plain("%5", "/r/a"),
            plain("%6", "/r/b"),
            pane_line(
                "%8",
                "1",
                "zsh",
                &path.display().to_string(),
                "⌨ beacon dev server f…",
            ),
        ],
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert_eq!(term_rows(&r.st)["%8"]["name"], title);
}

/// `moa term --title api` runs as the daemon connects: its tab is listed before tmux says
/// which tab it is, the reconcile answer comes, then the new tab's. Reconcile's end is the
/// first look, and it comes after the answers it can share an input with, so the new tab,
/// still waiting for its answer, isn't recorded as a stranger tagged `⌨ shell`: it is
/// recorded once, as yours, named `api`.
#[test]
fn a_terminal_opened_as_the_link_connects_keeps_its_name() {
    let mut r = Rig::linked(Vec::new());
    r.list(&[P5], &[plain("%5", "/r/a")]);
    handle_checked(&mut r.st, Input::StartDrained, r.b.now());
    let [reconcile] = r.st.tmux().queries().collect::<Vec<_>>()[..] else {
        panic!("one query waiting");
    };
    let op = Op::TermOpen {
        path: r.b.home.join("work"),
        title: Some("api".into()),
    };
    let fx = send(&r.b, &mut r.st, 1, op);
    let [(term, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let fx = r.list(
        &[P5, "P %8 @5 $0 1 0 0 0  0 "],
        &[plain("%5", "/r/a"), plain("%8", "/r/work")],
    );
    assert!(records(&fx).is_empty(), "{fx:?}");

    let conn = r.conn();
    let lines = |l: &[&str]| Ok(l.iter().map(|s| s.to_string()).collect());
    let mut all = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(reconcile),
            results: vec![lines(&[&info("%5"), &info("%8")]), lines(&[])],
        },
    );
    all.extend(feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(term),
            results: vec![lines(&["P %8 1008"])],
        },
    ));
    let recorded = records(&all);
    let eight: Vec<_> = recorded.iter().filter(|t| t.pane_id == "%8").collect();
    assert_eq!(eight.len(), 1, "{recorded:?}");
    assert_eq!(
        (eight[0].opened_by, eight[0].name.as_str()),
        (OpenedBy::You, "api")
    );
    assert!(
        marks(&all)
            .iter()
            .all(|c| !c.argv().iter().any(|a| a == "⌨ shell")),
        "{all:?}"
    );
    assert_eq!(r.open(8).map(|t| t.name.as_str()), Some("api"));
}

/// A terminal moa opened whose shell's name can't be read takes the program the next
/// listing shows as its shell, as a shell found by a listing does: it isn't running anything.
#[test]
fn a_shell_moa_couldnt_read_is_the_first_program_listed() {
    let mut r = Rig::found();
    let op = Op::TermOpen {
        path: r.b.home.join("work"),
        title: Some("kite".into()),
    };
    let fx = send(&r.b, &mut r.st, 1, op);
    let [(n, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let conn = r.conn();
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec!["P %11 1011".into()])],
        },
    );
    assert_eq!(r.open(11).map(|t| t.shell.as_str()), Some(""));
    let fx = r.list(
        &[P5, P6, "P %11 @5 $0 1 0 0 0  0 "],
        &[
            plain("%5", "/r/a"),
            plain("%6", "/r/b"),
            pane_line("%11", "1", "bash", "/r/c", "⌨ kite"),
        ],
    );
    let eleven = r.open(11).cloned().unwrap();
    assert_eq!(records(&fx), std::slice::from_ref(&eleven));
    assert_eq!(
        (eleven.shell.as_str(), eleven.command.as_str()),
        ("bash", "bash")
    );
    assert_eq!(term_rows(&r.st)["%11"]["running"], false);
}

/// A shell split from an agent's tab belongs to that agent's session.
#[test]
fn a_split_belongs_to_its_agents_session() {
    let mut r = Rig::linked(Vec::new());
    let tabs = [plain("%5", "/r/a"), pane_line("%7", "", "claude", "/r", "")];
    r.list(&[P5, P7], &tabs);
    r.reconcile(&[&info("%5"), &info("%7")]);
    assert!(r.st.binding(s_id()).is_some());
    let split = "P %8 @4 $0 0 1 0 0  0 ";
    let mut tabs = tabs.to_vec();
    tabs.push(pane_line("%8", "1", "zsh", "/r", "⌨ shell"));
    let fx = r.list(&[P5, P7, split], &tabs);
    let made = records(&fx);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(made[0].pane_id, "%8");
    assert_eq!(made[0].session_id, Some(s_id()));
}

/// A program that starts sets the program and since when; a folder that changes sets the
/// folder, and leaves since when as it was.
#[test]
fn a_changed_program_updates_since() {
    let mut r = Rig::found();
    let at = later(&r.b, 60_000);
    let fx = r.list_at(
        at.clone(),
        &[P5, P6],
        &[pane_line("%5", "", "node", "/r/a", ""), plain("%6", "/r/b")],
    );
    let five = r.open(5).cloned().unwrap();
    assert_eq!(records(&fx), std::slice::from_ref(&five));
    assert_eq!(
        (five.command.as_str(), five.command_since, five.cwd.as_str()),
        ("node", at.wall, "/r/a")
    );

    let fx = r.list_at(
        later(&r.b, 120_000),
        &[P5, P6],
        &[pane_line("%5", "", "node", "/r/c", ""), plain("%6", "/r/b")],
    );
    let moved = r.open(5).cloned().unwrap();
    assert_eq!(records(&fx), std::slice::from_ref(&moved));
    assert_eq!(
        (
            moved.command.as_str(),
            moved.command_since,
            moved.cwd.as_str()
        ),
        ("node", at.wall, "/r/c")
    );

    // The same again changes nothing.
    let fx = r.list(
        &[P5, P6],
        &[pane_line("%5", "", "node", "/r/c", ""), plain("%6", "/r/b")],
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
}

/// A terminal whose tab is gone from the listing closes, and its timeout goes with it.
#[test]
fn a_gone_terminal_is_closed() {
    let mut kite = record(1, "%5", "kite");
    kite.timeout_at = Some(UtcMs(i64::MAX / 2));
    let mut r = Rig::linked(vec![kite.clone()]);
    r.list(&[P5, P6], &[term_line("%5", "⌨ kite"), plain("%6", "/r")]);
    r.reconcile(&[&info("%5"), &info("%6")]);
    assert!(r.open(5).is_some());

    let fx = r.list(&[P6], &[plain("%6", "/r")]);
    let closed = Terminal {
        closed_at: Some(r.b.now().wall),
        timeout_at: None,
        ..kite
    };
    assert_eq!(records(&fx), [closed]);
    assert!(r.open(5).is_none());
    assert!(!term_rows(&r.st).contains_key("%5"));
}

/// A tab listed but with no process (a folder with a newline printed another tab's line, so
/// it reads as listed twice) is unknown this round: its record neither closes nor changes,
/// and a new one gets none.
#[test]
fn a_pane_with_no_pid_is_unknown_this_round() {
    let mut r = Rig::found();
    let twice = pane_line("%5", "", "node", "/elsewhere", "");
    let fx = r.list(
        &[P5, P6, "P %8 @5 $0 1 0 0 0  0 "],
        &[
            plain("%5", "/r/a"),
            twice,
            plain("%6", "/r/b"),
            plain("%8", "/r/c"),
            plain("%8", "/r/c"),
        ],
    );
    assert!(r.st.tmux().topology.has_pane(PaneId(5)));
    assert_eq!(r.st.tmux().topology.pid_of(&PaneId(5)), None);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(marks(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.open(5).map(|t| t.command.as_str()), Some("zsh"));
    assert!(r.open(8).is_none());
}

/// You run an agent in a terminal and moa takes it on: the tab is the session's now, so the
/// record closes. Its `@moa_term` stays, which keeps sleep from killing your shell.
#[test]
fn an_adopted_terminal_stops_being_one() {
    let mut r = Rig::linked(Vec::new());
    AGENT_IN_7.with(|a| a.set(false));
    let tabs = [plain("%5", "/r/a"), plain("%7", "/r")];
    r.list(&[P5, P7], &tabs);
    let fx = r.reconcile(&[&info("%5"), &info("%7")]);
    assert_eq!(records(&fx).len(), 2, "{fx:?}");
    let seven = r.open(7).cloned().unwrap();

    // `claude` typed in `%7`; the link comes back, and reconcile takes the agent on.
    AGENT_IN_7.with(|a| a.set(true));
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Connected {
            conn: 2,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    let tabs = [
        plain("%5", "/r/a"),
        pane_line("%7", "1", "claude", "/r", "⌨ shell"),
    ];
    r.list(&[P5, P7], &tabs);
    let term_7 = "I\t%7\t1007\t0\t1\t\t\t/r";
    let fx = listed(&mut r.st, &r.b, &[&info("%5"), term_7], &[]);
    assert!(r.st.tmux().reconciled());
    let binding = r.st.binding(s_id()).expect("S is bound");
    assert_eq!(binding.pane_id, "%7");
    assert!(!binding.tracked, "{binding:?}");
    let closed = Terminal {
        closed_at: Some(r.b.now().wall),
        ..seven
    };
    assert_eq!(records(&fx), [closed]);
    assert!(r.open(7).is_none());
    assert!(r.open(5).is_some());
    let unset_term = |c: &&Cmd| c.argv().iter().any(|a| a == "@moa_term");
    assert!(!unsets(&fx).iter().any(unset_term), "{fx:?}");
}

/// A tab bound to a session is that session's, even while the listing shows it without its
/// tag (the tag came back in reconcile's own listing, so it isn't put on again).
#[test]
fn a_bound_tab_is_never_a_terminal() {
    let g = generation();
    let mut r = Rig::linked_with(|snap| {
        snap.pane_bindings.push(PaneBinding {
            session_id: s_id(),
            server_pid: g.pid,
            server_start: g.start,
            socket: "moa".into(),
            pane_id: "%7".into(),
            pane_pid: 1007,
            bound_at: UtcMs(1),
            tracked: true,
        });
    });
    r.list(&[P5, P7], &[plain("%5", "/r/a"), plain("%7", "/r")]);
    let tagged = format!("I\t%7\t1007\t0\t\t\t{S}\t/r");
    let fx = r.reconcile(&[&info("%5"), &tagged]);
    assert_eq!(r.st.binding(s_id()).map(|b| b.pane_id.as_str()), Some("%7"));
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(7)), None);
    let made: Vec<String> = records(&fx).into_iter().map(|t| t.pane_id).collect();
    assert_eq!(made, ["%5"]);
}

/// Panels, the agent spot, an empty slot, the hidden session's tabs, a tab whose program
/// exited, and a hand-run panel's tab are never terminals.
#[test]
fn panels_spot_and_hidden_panes_are_never_terminals() {
    let mut r = Rig::linked(Vec::new());
    let panes = [
        P5,
        "P %10 @5 $0 1 0 0 1  0 tree",
        "P %11 @5 $0 0 0 0 1  0 agent",
        "P %12 @5 $0 0 0 0 1  0 empty",
        "P %13 @6 $1 1 1 0 0  0 ",
        "P %14 @7 $0 1 0 0 0  1 ",
        "P %15 @8 $0 1 0 0 0  0 ",
    ];
    let shells: Vec<String> = ["%5", "%10", "%11", "%12", "%13", "%14", "%15"]
        .iter()
        .map(|p| plain(p, "/r"))
        .collect();
    let panel = Op::UiPanel {
        pane: Some("%15".into()),
        view: Some(View::Tree),
        placed: false,
        once: false,
    };
    send_on(&r.b, &mut r.st, 2, 1, panel);
    r.list(&panes, &shells);
    let infos: Vec<String> = ["%5", "%10", "%11", "%12", "%13", "%14", "%15"]
        .iter()
        .map(|p| info(p))
        .collect();
    let infos: Vec<&str> = infos.iter().map(String::as_str).collect();
    let fx = r.reconcile(&infos);
    let made: Vec<String> = records(&fx).into_iter().map(|t| t.pane_id).collect();
    assert_eq!(made, ["%5"]);
    assert_eq!(marks(&fx), marked("%5"));
}

/// A record that can't be saved doesn't hide a terminal: the picker's row comes from the
/// tab's mark.
#[test]
fn the_picker_keeps_a_terminal_whose_record_failed() {
    let mut r = Rig::found();
    let mut snap = r.b.snapshot();
    snap.sessions.push(session_s(&r.b));
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(r.b.now().wall.0 + 60_000),
                reason: "disk full".into(),
            }),
        },
        r.b.now(),
    );
    let op = Op::TermOpen {
        path: r.b.home.join("work"),
        title: Some("kite".into()),
    };
    let fx = send(&r.b, &mut r.st, 1, op);
    let [(n, _)] = queries(&fx)[..] else {
        panic!("one query: {fx:?}");
    };
    let conn = r.conn();
    let fx = feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(n),
            results: vec![Ok(vec!["P %8 1008".into()])],
        },
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(r.open(8).is_none());
    assert_eq!(
        marks(&fx)[0],
        Cmd::new("set-option").args(["-p", "-t", "%8", "@moa_term", "1"])
    );
    assert_eq!(term_rows(&r.st)["%8"]["name"], "kite");
}

/// The daemon restarts with terminals open on the same server: the first look of the new
/// connection makes no second record, and each keeps its name.
#[test]
fn a_daemon_restart_keeps_each_terminal_once() {
    let mut r = Rig::linked(vec![record(1, "%5", "kite"), record(2, "%6", "web")]);
    r.list(
        &[P5, P6],
        &[term_line("%5", "⌨ kite"), term_line("%6", "⌨ renamed")],
    );
    let fx = r.reconcile(&[&info("%5"), &info("%6")]);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(marks(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.open(5).map(|t| t.name.as_str()), Some("kite"));
    assert_eq!(r.open(6).map(|t| t.name.as_str()), Some("web"));
}

/// A new server (a reboot) closes every record of the old one; its tabs, whatever their ids,
/// are found anew.
#[test]
fn a_new_server_closes_the_old_records() {
    let mut old = record(1, "%5", "kite");
    old.server_pid = 9;
    old.server_start = 1;
    let mut r = Rig::linked(vec![old.clone()]);
    r.list(&[P5], &[plain("%5", "/r/a")]);
    let fx = r.reconcile(&[&info("%5")]);
    let now = r.b.now().wall;
    let made = records(&fx);
    assert_eq!(made.len(), 2, "{made:?}");
    assert_eq!(
        made[0],
        Terminal {
            closed_at: Some(now),
            ..old
        }
    );
    assert_eq!(
        (made[1].id, made[1].pane_id.as_str(), made[1].opened_by),
        (TermId(2), "%5", OpenedBy::Found)
    );
    assert_eq!(r.open(5).map(|t| t.id), Some(TermId(2)));
}

/// The `terms` rows are the topology's terminals, each filled in from its open record: the
/// record's name wins over the tab's label. One with no record has only its name.
#[test]
fn the_terms_topic_is_built_from_records() {
    let kite = Terminal {
        opened_by: OpenedBy::You,
        cwd: "/r/a".into(),
        session_id: Some(s_id()),
        opened_at: UtcMs(1_000),
        note: Some("dev server for checkout, keep".into()),
        timeout_at: Some(UtcMs(3_000)),
        shell: "zsh".into(),
        command: "node".into(),
        command_since: UtcMs(2_000),
        ..record(1, "%5", "kite")
    };
    let mut r = Rig::linked(vec![kite.clone()]);
    r.list(
        &[P5, P6],
        &[
            pane_line("%5", "1", "node", "/r/a", "⌨ other"),
            term_line("%6", "⌨ web"),
        ],
    );
    let rows = term_rows(&r.st);
    assert_eq!(
        rows["%5"],
        json!({
            "key": "%5",
            "name": "kite",
            "id": 1,
            "folder": "/r/a",
            "command": "node",
            "running": true,
            "since": kite.command_since.0,
            "opened_at": kite.opened_at.0,
            "touched_at": null,
            "active": false,
            "opened_by": "you",
            "session": "lantern",
            "note": "dev server for checkout, keep",
            "timeout_at": kite.timeout_at.unwrap().0,
        })
    );
    assert_eq!(
        rows["%6"],
        json!({
            "key": "%6",
            "name": "web",
            "id": 0,
            "folder": "",
            "command": "",
            "running": false,
            "since": 0,
            "opened_at": 0,
            "touched_at": null,
            "active": false,
            "opened_by": "",
            "session": null,
            "note": null,
            "timeout_at": null,
        })
    );
}

/// A terminal's row names its session by title: renaming the session publishes the row
/// again, with the new title, in the same input (`handle` rebuilds `terms` after every
/// input, so a session's change reaches it without `Topic::Terms` among its topics).
#[test]
fn a_renamed_session_renames_its_terminals_row() {
    let kite = Terminal {
        session_id: Some(s_id()),
        ..record(1, "%5", "kite")
    };
    let mut r = Rig::linked(vec![kite]);
    r.list(&[P5], &[term_line("%5", "⌨ kite")]);
    assert_eq!(term_rows(&r.st)["%5"]["session"], "lantern");
    let rename = Op::SessionSet {
        q: S.into(),
        title: Some("harbor".into()),
        group: None,
    };
    let fx = send(&r.b, &mut r.st, 1, rename);
    let published = published_term_rows(&fx);
    assert_eq!(
        published.last().map(|rows| rows["%5"]["session"].clone()),
        Some(json!("harbor")),
        "{fx:?}"
    );
    assert_eq!(term_rows(&r.st)["%5"]["session"], "harbor");
}

/// `%5` (in the tab `@2`) and `%6` (in `@3`) are shells in the session `moa`, whose current
/// tab is `@2`.
fn terms56() -> [String; 2] {
    [plain("%5", "/r/a"), plain("%6", "/r/b")]
}

/// A user client `/dev/ttys1` attached to the session `moa`, at `now`.
fn look(r: &mut Rig, now: Now) -> Vec<Effect> {
    r.list_as(
        now,
        &["S $0 moa", "S $1 _moa"],
        &[P5, P6],
        &terms56(),
        &["C 0 $0 /dev/ttys1"],
    )
}

/// The user switches the session `moa` to the tab `@3`, where `%6` is.
fn switch_to_6(r: &mut Rig, now: Now) -> Vec<Effect> {
    r.notify(
        now,
        Notification::SessionWindowChanged {
            session: TmuxSessionId(0),
            window: WindowId(3),
        },
    )
}

/// Looking at a terminal stamps it when you arrive, and the one you leave when you go.
#[test]
fn switching_to_a_terminal_stamps_it() {
    let mut r = Rig::found();
    assert_eq!(r.open(5).unwrap().touched_at, None);
    let at = later(&r.b, 1_000);
    look(&mut r, at.clone());
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
    assert_eq!(r.open(6).unwrap().touched_at, None);

    let at = later(&r.b, 2_000);
    let fx = switch_to_6(&mut r, at.clone());
    assert_eq!(r.open(6).unwrap().touched_at, Some(at.wall));
    assert!(records(&fx).iter().any(|t| t.pane_id == "%6"), "{fx:?}");
}

#[test]
fn leaving_a_terminal_stamps_it_again() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    look(&mut r, at);
    let at = later(&r.b, 2_000);
    let fx = switch_to_6(&mut r, at.clone());
    let left: Vec<_> = records(&fx)
        .into_iter()
        .filter(|t| t.pane_id == "%5")
        .collect();
    assert_eq!(left.len(), 1, "{fx:?}");
    assert_eq!(left[0].touched_at, Some(at.wall));
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
}

/// Events that move nothing a user looks at write nothing.
#[test]
fn staying_on_a_terminal_writes_nothing() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    look(&mut r, at);
    for i in 0..10 {
        let at = later(&r.b, 2_000 + i);
        let fx = r.notify(
            at,
            Notification::WindowPaneChanged {
                window: WindowId(3),
                pane: PaneId(6),
            },
        );
        assert!(records(&fx).is_empty(), "{fx:?}");
    }
}

#[test]
fn a_detach_stamps_the_terminal_you_were_on() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    look(&mut r, at);
    let at = later(&r.b, 2_000);
    let fx = r.notify(
        at.clone(),
        Notification::ClientDetached {
            client: ClientName("/dev/ttys1".into()),
        },
    );
    assert_eq!(records(&fx).len(), 1, "{fx:?}");
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
}

/// Two users, each on a terminal of their own session, are both looking.
#[test]
fn two_clients_on_two_terminals_stamp_both() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    let p8 = "P %8 @6 $2 1 1 0 0  0 ";
    let mut terms = terms56().to_vec();
    terms.push(plain("%8", "/r/c"));
    r.list_as(
        at.clone(),
        &["S $0 moa", "S $1 _moa", "S $2 kite"],
        &[P5, P6, p8],
        &terms,
        &["C 0 $0 /dev/ttys1", "C 0 $2 /dev/ttys2"],
    );
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
    assert_eq!(r.open(8).unwrap().touched_at, Some(at.wall));
    assert_eq!(r.open(6).unwrap().touched_at, None);
    assert_eq!(term_rows(&r.st)["%5"]["active"], true);
    assert_eq!(term_rows(&r.st)["%8"]["active"], true);
    assert_eq!(term_rows(&r.st)["%6"]["active"], false);
}

/// The link's own control client looks at nothing.
#[test]
fn a_control_client_touches_nothing() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    let fx = r.list_as(
        at,
        &["S $0 moa", "S $1 _moa"],
        &[P5, P6],
        &terms56(),
        &["C 1 $0 client-9"],
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.open(5).unwrap().touched_at, None);
    assert_eq!(term_rows(&r.st)["%5"]["active"], false);
}
