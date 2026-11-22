//! Every terminal is recorded when moa sees it (5d, T3): each listing, once the link's tabs
//! are reconciled, is compared with the open records. A new shell gets a record (`found` on
//! the connection's first look, `you` after), a gone one's record closes, and a changed
//! program or folder is updated. The `terms` topic stays the topology's, each row filled in
//! from its record.

mod common;

use std::cell::Cell;
use std::path::PathBuf;

use moa_core::change::Change;
use moa_core::effect::{
    Down, Effect, Input, RefuseWrites, TimerKey, TmuxIn, TmuxToken, handle_checked,
};
use moa_core::model::{
    Activity, ActivityWho, AgentKind, Inspected, Lifecycle, Location, OpenedBy, PaneAgent,
    PaneBinding, Session, SessionId, TermId, Terminal,
};
use moa_core::op::{NoParams, Op, OpError};
use moa_core::panel::View;
use moa_core::state::{Deps, LoadNote, Snapshot, State};
use moa_core::terminals::{find, is_idle, open_for, parse_after};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::link::Link;
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
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

/// `snap` with `open` its open records.
fn hold(snap: &mut Snapshot, open: Vec<Terminal>) {
    snap.next_ids.terminal = open.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
    snap.terminals = open;
}

struct Rig {
    b: TypicalBrain,
    st: State,
}

impl Rig {
    /// The typical brain with S live and `open` the open records, linked on connection 1;
    /// its first listing asked, the start drain not yet run.
    fn linked(open: Vec<Terminal>) -> Rig {
        Rig::linked_with(|snap| hold(snap, open))
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

/// A first look held back by a launch in flight isn't the last: the shells it skipped are
/// still `found`, not `you`, when the next listing's run goes through.
#[test]
fn a_held_back_first_run_still_finds_open_terminals() {
    let mut r = Rig::linked(Vec::new());
    let open = Op::SessionOpen {
        path: r.b.home.join("private/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let (id, _) = the_open(&send(&r.b, &mut r.st, 1, open));
    r.list(
        &[P5, P6],
        &[
            pane_line("%5", "", "bash", "/r/a", ""),
            pane_line("%6", "", "bash", "/r/b", ""),
        ],
    );
    let fx = r.reconcile(&[&info("%5"), &info("%6")]);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert!(r.open(5).is_none());

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
    let fx = r.list(
        &[P5, P6, "P %9 @6 $0 1 0 0 0  0 "],
        &[
            plain("%5", "/r/a"),
            plain("%6", "/r/b"),
            pane_line("%9", "", "claude", "/r", ""),
        ],
    );
    let made = records(&fx);
    assert_eq!(made.len(), 2, "{made:?}");
    assert!(
        made.iter().all(|t| t.opened_by == OpenedBy::Found),
        "{made:?}"
    );
    assert!(r.open(9).is_none());
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

// `term ls`, `name` and `note` (T5).

/// 5d p2 rulings: a long name's `@moa_label` is cut as the tab's is, one rule for both.
#[test]
fn a_long_names_label_is_cut_like_the_tab() {
    let mut r = Rig::found();
    let long = "y".repeat(40);
    let fx = send(
        &r.b,
        &mut r.st,
        5,
        Op::TermName {
            target: "%5".into(),
            name: long.clone(),
        },
    );
    let sent = sent(&fx);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0].argv()[3], "@moa_label");
    assert_eq!(sent[0].argv()[4], sent[1].argv()[3], "{sent:?}");
    assert_ne!(sent[0].argv()[4], format!("⌨ {long}"));
}

/// Renaming waits for tmux: with the link down nothing changes.
#[test]
fn naming_with_the_link_down_is_refused() {
    let mut r = Rig::found();
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let before = r.open(5).unwrap().name.clone();
    let fx = send(
        &r.b,
        &mut r.st,
        5,
        Op::TermName {
            target: "%5".into(),
            name: "Kite".into(),
        },
    );
    assert_eq!(
        refusal(&fx),
        "moa isn't open in a terminal yet; run moa first"
    );
    assert!(records(&fx).is_empty() && sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.open(5).unwrap().name, before);
}

/// Losing tmux isn't leaving a terminal: a dropped link stamps nothing, and neither does
/// the reconnect that finds the same terminal looked at.
#[test]
fn a_dropped_link_is_not_leaving_a_terminal() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    look(&mut r, at.clone());
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    assert!(records(&fx).is_empty(), "{fx:?}");
    link_up(&r.b, &mut r.st);
    let at2 = later(&r.b, 5_000);
    let fx = r.list_as(
        at2,
        &["S $0 moa", "S $1 _moa"],
        &[P5, P6],
        &terms56(),
        &["C 0 $0 /dev/ttys1"],
    );
    assert!(records(&fx).is_empty(), "{fx:?}");
    let seen = reconciled_again(&mut r);
    assert!(r.st.tmux().reconciled(), "{seen:?}");
    assert!(records(&seen).is_empty(), "{seen:?}");
    assert_eq!(r.open(5).unwrap().touched_at, Some(at.wall));
}

/// Answers the listing a connection waits on after the start drain already ran, with `%5`
/// and `%6` as they are; the effects.
fn reconciled_again(r: &mut Rig) -> Vec<Effect> {
    let conn = r.conn();
    let asked: Vec<u64> = r.st.tmux().queries().collect();
    let mut seen = Vec::new();
    for n in asked {
        let lines = |l: &[String]| Ok(l.to_vec());
        seen.extend(feed(
            &r.b,
            &mut r.st,
            TmuxIn::Replies {
                conn,
                token: TmuxToken::Query(n),
                results: vec![lines(&[info("%5"), info("%6")]), lines(&[])],
            },
        ));
    }
    seen
}

/// A stamp the store refused is tried again by the next input, not lost.
#[test]
fn a_refused_stamp_is_tried_again() {
    // `%5`'s row is one this build doesn't know, so a write to it is refused.
    let known = Rig::found();
    let open = vec![
        known.open(5).unwrap().clone(),
        known.open(6).unwrap().clone(),
    ];
    let snapshot = |r: &Rig, unknown: bool| {
        let mut snap = r.b.snapshot();
        snap.sessions.push(session_s(&r.b));
        snap.next_ids.terminal = 3;
        snap.terminals = open.clone();
        if unknown {
            snap.unknown.push(LoadNote {
                table: "terminals",
                key: "#1".into(),
                reason: "a state this build doesn't know",
            });
        }
        snap
    };
    let mut r = Rig::linked(Vec::new());
    let snap = snapshot(&r, true);
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        r.b.now(),
    );
    r.list(&[P5, P6], &terms56());
    r.reconcile(&[&info("%5"), &info("%6")]);
    let at = later(&r.b, 1_000);
    let fx = look(&mut r, at);
    assert!(records(&fx).is_empty(), "{fx:?}");
    assert_eq!(r.open(5).unwrap().touched_at, None);

    // The row is known again; the next input stamps what was refused.
    let at = later(&r.b, 2_000);
    let snap = snapshot(&r, false);
    handle_checked(
        &mut r.st,
        Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: None,
        },
        at.clone(),
    );
    let fx = r.reconcile(&[&info("%5"), &info("%6")]);
    assert_eq!(records(&fx).len(), 1, "{fx:?}");
    assert!(r.open(5).unwrap().touched_at.is_some());
}

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const LS_DOWN: &str = "moa isn't connected to tmux right now; this list may be out of date";

/// The typical brain's moment, which a rig's first `now` is.
fn t0() -> UtcMs {
    TypicalBrain::new().now().wall
}

/// `t` as `ls` lists it: `zsh` the shell, `command` in front, in `cwd`.
fn shown(mut t: Terminal, cwd: &str, command: &str) -> Terminal {
    t.cwd = cwd.into();
    t.shell = "zsh".into();
    t.command = command.into();
    t
}

/// `%5` is Kite and `%6` is Beacon, whose records are as they list. Both are reconciled.
fn kite_and_beacon(a: Terminal, b: Terminal) -> Rig {
    let mut r = Rig::linked(vec![a, b]);
    let tabs = [
        pane_line("%5", "1", "zsh", "/r/a", "⌨ Kite"),
        pane_line("%6", "1", "node", "/r/b", "⌨ Beacon"),
    ];
    r.list(&[P5, P6], &tabs);
    r.reconcile(&[&info("%5"), &info("%6")]);
    r
}

/// The listing of `%5` and `%6` as [`kite_and_beacon`] has them.
fn tabs56() -> [String; 2] {
    [
        pane_line("%5", "1", "zsh", "/r/a", "⌨ Kite"),
        pane_line("%6", "1", "node", "/r/b", "⌨ Beacon"),
    ]
}

fn ls(r: &mut Rig, req: u64) -> Vec<Effect> {
    send(&r.b, &mut r.st, req, Op::TermLs(NoParams))
}

/// The one reply in `fx`: its text and data.
fn the_reply(fx: &[Effect]) -> (String, serde_json::Value) {
    match &replies(fx)[..] {
        [(_, Ok(done))] => (done.text.clone(), done.data.clone()),
        other => panic!("one reply: {other:?}"),
    }
}

fn refusal(fx: &[Effect]) -> String {
    match &replies(fx)[..] {
        [(_, Err(OpError(why)))] => why.clone(),
        other => panic!("one refusal: {other:?}"),
    }
}

/// The commands `fx` sends tmux.
fn sent(fx: &[Effect]) -> Vec<Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux { cmds, .. } => Some(cmds.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn find_takes_a_number_a_pane_or_a_name() {
    let r = Rig::linked(vec![record(7, "%5", "Kite"), record(8, "%12", "Beacon")]);
    let id = |t: &str| find(&r.st, t).map(|t| t.id.0);
    assert_eq!(id("7"), Ok(7));
    assert_eq!(id("#7"), Ok(7));
    assert_eq!(id("%12"), Ok(8));
    assert_eq!(id("Beacon"), Ok(8));
    assert_eq!(id("beacon"), Ok(8));
    assert_eq!(id(" KITE "), Ok(7));
}

#[test]
fn find_refuses_unknown_and_ambiguous() {
    let mut gone_server = record(10, "%10", "Lantern");
    gone_server.server_pid += 1;
    let r = Rig::linked(vec![
        record(1, "%5", "Kite"),
        record(2, "%6", "kite"),
        record(3, "%7", "Beacon"),
        gone_server,
    ]);
    let why = |t: &str| find(&r.st, t).map(|t| t.id.0).unwrap_err().0;
    assert_eq!(why("KITE"), "2 terminals are named KITE; use its number");
    assert_eq!(why("9"), "no terminal 9");
    assert_eq!(why("#10"), "no terminal #10");
    assert_eq!(why("%11"), "no terminal %11");
    assert_eq!(why("%x"), "no terminal %x");
    assert_eq!(why("Lantern"), "no terminal Lantern");
    assert_eq!(why("nobody"), "no terminal nobody");
    // A number names a record whatever its name; a pane or a number picks one of two.
    assert_eq!(find(&r.st, "1").map(|t| t.id.0), Ok(1));
    assert_eq!(find(&r.st, "%6").map(|t| t.id.0), Ok(2));
}

#[test]
fn ls_asks_a_fresh_listing_first() {
    let mut r = Rig::found();
    let fx = ls(&mut r, 9);
    assert!(replies(&fx).is_empty(), "{fx:?}");
    let listing: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds,
                token: Some(TmuxToken::Refresh),
                ..
            } => Some(cmds.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(listing, vec![refresh_cmds()], "{fx:?}");
    // A second request meanwhile asks for no listing of its own; the one that lands, and
    // the one asked behind it for a request that came during it, answer them in turn.
    let fx = ls(&mut r, 10);
    assert!(sent(&fx).is_empty() && replies(&fx).is_empty(), "{fx:?}");
    let fx = r.list(&[P5, P6], &terms56());
    assert!(
        replies(&fx).is_empty(),
        "the listing asked behind it is out: {fx:?}"
    );
    let fx = r.list(&[P5, P6], &terms56());
    let got: Vec<u64> = replies(&fx).iter().map(|(req, _)| req.0).collect();
    assert_eq!(got, vec![9, 10], "{fx:?}");
    // Nothing waits any more: a listing nobody asked for answers nobody.
    assert!(replies(&r.list(&[P5, P6], &terms56())).is_empty());
}

#[test]
fn ls_lists_open_terminals_in_text_and_data() {
    let now = t0();
    let mut kite = shown(record(1, "%5", "Kite"), "/r/a", "zsh");
    kite.opened_at = UtcMs(now.0 - 5 * HOUR);
    kite.touched_at = Some(UtcMs(now.0 - 3 * HOUR));
    let mut beacon = shown(record(2, "%6", "Beacon"), "/r/b", "node");
    beacon.opened_at = UtcMs(now.0 - 4 * HOUR);
    beacon.touched_at = Some(UtcMs(now.0 - 5 * MIN));
    beacon.command_since = UtcMs(now.0 - 2 * HOUR);
    beacon.timeout_at = Some(UtcMs(now.0 + 3 * HOUR));
    beacon.note = Some("dev server".into());
    let mut r = kite_and_beacon(kite, beacon);
    ls(&mut r, 9);
    let (text, data) = the_reply(&r.list(&[P5, P6], &tabs56()));
    assert_eq!(
        text,
        "#1  Kite  /r/a  shell  touched 3h  open 5h  idle\n\
         #2  Beacon  /r/b  node (2h)  touched 5m  open 4h  closes in 3h  — dev server"
    );
    let rows = data.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0]["id"].clone(), rows[0]["pane"].clone()),
        (json!(1), json!("%5"))
    );
    assert_eq!(rows[0]["idle"], true);
    assert_eq!(rows[0]["name"], "Kite");
    assert_eq!(rows[1]["idle"], false);
    assert_eq!(rows[1]["note"], "dev server");
    assert_eq!(rows[1]["running"], true);
    assert_eq!(rows[1]["timeout_at"], now.0 + 3 * HOUR);
    assert_eq!(rows[1]["active"], false);
}

#[test]
fn ls_writes_the_home_as_a_tilde_and_a_never_touched_terminal_so() {
    let mut r = Rig::linked(Vec::new());
    let cwd = format!("{}/work/lantern", r.b.home.display());
    let tabs = [plain("%5", &cwd)];
    r.list(&[P5], &tabs);
    r.reconcile(&[&info("%5")]);
    ls(&mut r, 9);
    let (text, _) = the_reply(&r.list(&[P5], &tabs));
    assert_eq!(
        text,
        "#1  shell  ~/work/lantern  shell  touched never  open now"
    );
}

#[test]
fn ls_with_none_says_so() {
    let mut r = Rig::linked(Vec::new());
    // The connection's first listing is still out: the request waits for a later one.
    ls(&mut r, 9);
    assert!(replies(&r.list(&[], &[])).is_empty());
    let (text, data) = the_reply(&r.list(&[], &[]));
    assert_eq!(text, "no terminals open");
    assert_eq!(data, json!([]));
}

#[test]
fn ls_with_the_link_down_says_so() {
    let mut r = Rig::found();
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let fx = ls(&mut r, 9);
    assert!(sent(&fx).is_empty(), "nothing can be asked: {fx:?}");
    let (text, data) = the_reply(&fx);
    assert!(text.starts_with(&format!("{LS_DOWN}\n#1  ")), "{text}");
    assert_eq!(data.as_array().unwrap().len(), 2);

    // A request waiting when the link goes is answered then, as the records say.
    let mut r = Rig::found();
    assert!(replies(&ls(&mut r, 9)).is_empty());
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let (text, _) = the_reply(&fx);
    assert!(text.starts_with(LS_DOWN), "{text}");
}

#[test]
fn ls_shows_how_long_a_program_has_run() {
    let now = t0();
    let mut kite = shown(record(1, "%5", "Kite"), "/r/a", "zsh");
    kite.command_since = UtcMs(now.0 - 3 * HOUR);
    let mut beacon = shown(record(2, "%6", "Beacon"), "/r/b", "node");
    beacon.command_since = UtcMs(now.0 - 2 * HOUR - 30 * MIN);
    let mut r = kite_and_beacon(kite, beacon);
    ls(&mut r, 9);
    let (text, _) = the_reply(&r.list(&[P5, P6], &tabs56()));
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("  shell  "), "{text}");
    assert!(lines[1].contains("  node (2h)  "), "{text}");
}

#[test]
fn is_idle_counts_from_last_touch_or_open_and_never_while_looked_at() {
    let now = UtcMs(10 * HOUR);
    let two = 2 * HOUR;
    assert!(is_idle(UtcMs(now.0 - two), false, two, now));
    assert!(!is_idle(UtcMs(now.0 - two + 1), false, two, now));
    // Looked at: never idle, however long ago it was touched.
    assert!(!is_idle(UtcMs(0), true, two, now));
    // 0 turns the mark off.
    assert!(!is_idle(UtcMs(0), false, 0, now));
}

#[test]
fn an_active_terminal_is_never_idle_in_ls() {
    let mut r = Rig::found();
    let at = later(&r.b, 1_000);
    look(&mut r, at);
    ls(&mut r, 9);
    // 5 hours on: %6 went untouched; %5, looked at since, did not.
    let at = later(&r.b, 5 * HOUR);
    let (text, data) = the_reply(&r.list_as(
        at,
        &["S $0 moa", "S $1 _moa"],
        &[P5, P6],
        &terms56(),
        &["C 0 $0 /dev/ttys1"],
    ));
    let rows = data.as_array().unwrap();
    assert_eq!(
        (rows[0]["active"].clone(), rows[0]["idle"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(
        (rows[1]["active"].clone(), rows[1]["idle"].clone()),
        (json!(false), json!(true))
    );
    assert!(
        text.lines().next().unwrap().contains("touched now"),
        "{text}"
    );
}

#[test]
fn naming_sets_the_record_label_and_tab() {
    let mut r = Rig::found();
    let fx = send(
        &r.b,
        &mut r.st,
        5,
        Op::TermName {
            target: "%5".into(),
            name: "  Kite server  ".into(),
        },
    );
    assert_eq!(the_reply(&fx).0, "named it Kite server");
    assert_eq!(records(&fx).len(), 1, "{fx:?}");
    assert_eq!(r.open(5).unwrap().name, "Kite server");
    assert_eq!(
        sent(&fx),
        vec![
            Cmd::new("set-option").args(["-p", "-t", "%5", "@moa_label", "⌨ Kite server"]),
            Cmd::new("rename-window").args(["-t", "%5", "--", "⌨ Kite server"]),
        ]
    );
    assert_eq!(term_rows(&r.st)["%5"]["name"], "Kite server");
    assert_eq!(
        r.st.tmux().topology.term_name(PaneId(5)),
        Some("Kite server")
    );
    // By number, by name; a `#` stays literal; the tab's name is cut, the label's is not.
    let long = "Harbor #1 build server with a long name";
    let fx = send(
        &r.b,
        &mut r.st,
        6,
        Op::TermName {
            target: "kite server".into(),
            name: long.into(),
        },
    );
    assert_eq!(the_reply(&fx).0, format!("named it {long}"));
    assert_eq!(
        sent(&fx),
        vec![
            Cmd::new("set-option").args([
                "-p",
                "-t",
                "%5",
                "@moa_label",
                "⌨ Harbor ##1 build server"
            ]),
            Cmd::new("rename-window").args(["-t", "%5", "--", "⌨ Harbor ##1 build server"]),
        ]
    );
}

#[test]
fn naming_a_split_leaves_the_agents_tab() {
    let mut r = Rig::linked(Vec::new());
    let split = "P %8 @2 $0 1 0 0 0  0 ";
    let tabs = [plain("%5", "/r/a"), plain("%8", "/r/b")];
    r.list(&[P5, split], &tabs);
    r.reconcile(&[&info("%5"), &info("%8")]);
    assert!(r.open(8).is_some());
    for target in ["%5", "%8"] {
        let fx = send(
            &r.b,
            &mut r.st,
            5,
            Op::TermName {
                target: target.into(),
                name: "Beacon".into(),
            },
        );
        let sent = sent(&fx);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].name(), "set-option");
    }
}

#[test]
fn a_name_is_1_to_40_characters() {
    let mut r = Rig::found();
    let name = |r: &mut Rig, n: &str| {
        send(
            &r.b,
            &mut r.st,
            5,
            Op::TermName {
                target: "%5".into(),
                name: n.into(),
            },
        )
    };
    let refused = "a terminal's name is 1 to 40 characters";
    for bad in ["", "   ", "\n", &"x".repeat(41), &"é".repeat(41)] {
        let fx = name(&mut r, bad);
        assert_eq!(refusal(&fx), refused, "{bad:?}");
        assert!(records(&fx).is_empty() && sent(&fx).is_empty(), "{fx:?}");
    }
    for ok in ["x", &"x".repeat(40), &"é".repeat(40)] {
        assert_eq!(the_reply(&name(&mut r, ok)).0, format!("named it {ok}"));
    }
    // A name `find` would read as a number or a tab could never be used.
    for bad in ["42", "#7", "%3", "-1"] {
        let fx = name(&mut r, bad);
        assert_eq!(
            refusal(&fx),
            "a terminal's name can't be a number or start with %",
            "{bad:?}"
        );
        assert!(records(&fx).is_empty() && sent(&fx).is_empty(), "{fx:?}");
    }
    // A newline in a name is a space, so the list stays a line a terminal.
    assert_eq!(the_reply(&name(&mut r, "a\nb")).0, "named it a b");
    assert_eq!(r.open(5).unwrap().name, "a b");
    // An unknown terminal is the lookup's refusal.
    let fx = send(
        &r.b,
        &mut r.st,
        5,
        Op::TermName {
            target: "9".into(),
            name: "x".into(),
        },
    );
    assert_eq!(refusal(&fx), "no terminal 9");
}

#[test]
fn a_note_is_cut_at_200_and_empty_clears() {
    let mut r = Rig::found();
    let note = |r: &mut Rig, n: &str| {
        send(
            &r.b,
            &mut r.st,
            5,
            Op::TermNote {
                target: "1".into(),
                note: n.into(),
            },
        )
    };
    let fx = note(&mut r, "  waiting on Harbor  ");
    assert_eq!(the_reply(&fx).0, "noted");
    assert_eq!(
        r.open(5).unwrap().note.as_deref(),
        Some("waiting on Harbor")
    );
    assert!(sent(&fx).is_empty(), "a note is the record's alone: {fx:?}");
    assert_eq!(term_rows(&r.st)["%5"]["note"], "waiting on Harbor");

    let fx = note(&mut r, &"n".repeat(201));
    assert_eq!(the_reply(&fx).0, "noted");
    let kept = r.open(5).unwrap().note.clone().unwrap();
    assert_eq!(kept.chars().count(), 200);
    assert_eq!(kept, format!("{}…", "n".repeat(199)));
    let exact = "é".repeat(200);
    note(&mut r, &exact);
    assert_eq!(r.open(5).unwrap().note.as_deref(), Some(exact.as_str()));

    for empty in ["", "  \n "] {
        note(&mut r, "again");
        assert_eq!(the_reply(&note(&mut r, empty)).0, "note cleared");
        assert_eq!(r.open(5).unwrap().note, None);
    }
    // A note may start with a dash, and holds one line.
    note(&mut r, "- next\nstep");
    assert_eq!(r.open(5).unwrap().note.as_deref(), Some("- next step"));
}

// Closing, and timeouts (T8).

const DAY: i64 = 24 * HOUR;
const TIMEOUT_FORM: &str = "a timeout is like 3h or 90m, up to 7 days";

/// `%5`'s record: Kite, its shell at its prompt in `/r/a`.
fn kite() -> Terminal {
    shown(record(1, "%5", "Kite"), "/r/a", "zsh")
}

/// `%6`'s record: Beacon, running `node` in `/r/b`.
fn beacon() -> Terminal {
    shown(record(2, "%6", "Beacon"), "/r/b", "node")
}

/// Kite and Beacon, listed and reconciled at [`t0`].
fn closable() -> Rig {
    kite_and_beacon(kite(), beacon())
}

/// The one question a close asks tmux: what runs in `pane` now.
fn what_runs(pane: &str) -> Cmd {
    Cmd::new("display-message").args(["-p", "-t", pane, "#{pane_current_command}"])
}

fn kill(pane: &str) -> Cmd {
    Cmd::new("kill-pane").args(["-t", pane])
}

fn close(r: &mut Rig, req: u64, target: &str) -> Vec<Effect> {
    let op = Op::TermClose {
        target: target.into(),
    };
    send(&r.b, &mut r.st, req, op)
}

fn timeout(r: &mut Rig, req: u64, target: &str, after_ms: Option<i64>) -> Vec<Effect> {
    let op = Op::TermTimeout {
        target: target.into(),
        after_ms,
    };
    send(&r.b, &mut r.st, req, op)
}

/// The number of the query `fx` asks about what runs in `pane`: there is one.
fn close_query(fx: &[Effect], pane: &str) -> u64 {
    let asked: Vec<u64> = queries(fx)
        .into_iter()
        .filter(|(_, cmds)| *cmds == [what_runs(pane)])
        .map(|(n, _)| n)
        .collect();
    let [n] = asked[..] else {
        panic!("one question about {pane}: {fx:?}");
    };
    n
}

/// tmux answers close query `n`: `Ok` with the program in front, or `Err` with its refusal.
fn runs(r: &mut Rig, n: u64, answer: Result<&str, &str>) -> Vec<Effect> {
    let conn = r.conn();
    let result = answer
        .map(|line| vec![line.to_string()])
        .map_err(str::to_string);
    feed(
        &r.b,
        &mut r.st,
        TmuxIn::Replies {
            conn,
            token: TmuxToken::Query(n),
            results: vec![result],
        },
    )
}

/// The terminals' timer fires at `wall`.
fn fire(r: &mut Rig, wall: i64) -> Vec<Effect> {
    handle_checked(
        &mut r.st,
        Input::Timer(TimerKey::Terminals),
        at(UtcMs(wall)),
    )
}

/// The activity lines `fx` saves, and who each is by.
fn lines(fx: &[Effect]) -> Vec<(ActivityWho, String)> {
    changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Activity(a) => Some((a.who, a.text.clone())),
            _ => None,
        })
        .collect()
}

/// Whether `fx` sends anything a close sends: its question, Ctrl-c, or a kill.
fn closes_any(fx: &[Effect]) -> bool {
    sent(fx).iter().any(|c| {
        matches!(c.name(), "send-keys" | "kill-pane")
            || c.argv().iter().any(|a| a == "#{pane_current_command}")
    })
}

fn set_at(wall: i64) -> Effect {
    Effect::SetTimer {
        key: TimerKey::Terminals,
        at: UtcMs(wall),
    }
}

impl Rig {
    /// The core reloads from S and `open`, at [`t0`]; writes are refused until `refused`
    /// when there is one.
    fn reload(&mut self, open: Vec<Terminal>, refused: Option<i64>) -> Vec<Effect> {
        let mut snap = self.b.snapshot();
        snap.sessions.push(session_s(&self.b));
        hold(&mut snap, open);
        let refuse_writes = refused.map(|until| RefuseWrites {
            until: UtcMs(until),
            reason: "disk full".into(),
        });
        handle_checked(
            &mut self.st,
            Input::Reload {
                snapshot: Box::new(snap),
                refuse_writes,
            },
            self.b.now(),
        )
    }
}

/// tmux doesn't say when a program starts, so a close decides on a fresh answer: the first
/// thing it does is ask what runs there, and nothing else.
#[test]
fn a_close_asks_tmux_first() {
    let mut r = closable();
    let fx = close(&mut r, 5, "Kite");
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    let n = close_query(&fx, "%5");
    assert_eq!(queries(&fx), [(n, vec![what_runs("%5")])]);
    assert_eq!(sent(&fx), [what_runs("%5")]);
}

/// A shell at its prompt (the answer is the record's shell) closes at once.
#[test]
fn a_shell_terminal_closes_at_once() {
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Kite"), "%5");
    let fx = runs(&mut r, n, Ok("zsh"));
    assert_eq!(sent(&fx), [kill("%5")]);
    assert_eq!(the_reply(&fx).0, "closed Kite");
    assert_eq!(
        lines(&fx),
        [(ActivityWho::You, "closed terminal Kite".to_string())]
    );
    assert!(timers(&fx, TimerKey::Terminals).is_empty(), "{fx:?}");
}

/// The listing said Kite was at its prompt; a program started since: it gets Ctrl-c, not a
/// kill.
#[test]
fn a_program_started_after_the_listing_is_still_interrupted() {
    let mut r = closable();
    assert_eq!(r.open(5).unwrap().command, "zsh");
    let n = close_query(&close(&mut r, 5, "Kite"), "%5");
    let fx = runs(&mut r, n, Ok("vim"));
    assert_eq!(sent(&fx), [Cmd::interrupt(PaneId(5))]);
    assert_eq!(the_reply(&fx).0, "stopping vim in Kite, then closing it");
}

/// A running program gets Ctrl-c, then its terminal is killed 3 seconds later, whether it
/// stopped or not; the listing that no longer shows it closes its record.
#[test]
fn a_running_terminal_is_interrupted_then_killed() {
    let t = t0().0;
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Beacon"), "%6");
    let fx = runs(&mut r, n, Ok("node"));
    assert_eq!(sent(&fx), [Cmd::interrupt(PaneId(6))]);
    assert_eq!(the_reply(&fx).0, "stopping node in Beacon, then closing it");
    assert_eq!(
        lines(&fx),
        [(ActivityWho::You, "closed terminal Beacon".to_string())]
    );
    assert_eq!(timers(&fx, TimerKey::Terminals), [&set_at(t + 3_000)]);

    // A timer that fires early kills nothing, and is armed again.
    let fx = fire(&mut r, t + 2_999);
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(timers(&fx, TimerKey::Terminals), [&set_at(t + 3_000)]);

    let fx = fire(&mut r, t + 3_000);
    assert_eq!(sent(&fx), [kill("%6")]);
    assert_eq!(
        timers(&fx, TimerKey::Terminals),
        [&Effect::CancelTimer(TimerKey::Terminals)]
    );
    let fx = r.list_at(at(UtcMs(t + 4_000)), &[P5], &tabs56()[..1]);
    assert_eq!(records(&fx).len(), 1, "{fx:?}");
    assert!(r.open(6).is_none());
    assert_eq!(refusal(&close(&mut r, 6, "Beacon")), "no terminal Beacon");
}

/// A close already under way, at its prompt or running, is said so: no second question, no
/// second Ctrl-c, no second kill.
#[test]
fn closing_twice_kills_once() {
    let t = t0().0;
    // At its prompt: a second close while the question is out, and after the kill.
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Kite"), "%5");
    let fx = close(&mut r, 6, "Kite");
    assert_eq!(the_reply(&fx).0, "already closing Kite");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(sent(&runs(&mut r, n, Ok("zsh"))), [kill("%5")]);
    let fx = close(&mut r, 7, "%5");
    assert_eq!(the_reply(&fx).0, "already closing Kite");
    assert!(sent(&fx).is_empty() && changes(&fx).is_empty(), "{fx:?}");

    // Running: a second close after the Ctrl-c, and after the kill.
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Beacon"), "%6");
    runs(&mut r, n, Ok("node"));
    let fx = close(&mut r, 6, "Beacon");
    assert_eq!(the_reply(&fx).0, "already closing Beacon");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert_eq!(sent(&fire(&mut r, t + 3_000)), [kill("%6")]);
    let fx = close(&mut r, 7, "2");
    assert_eq!(the_reply(&fx).0, "already closing Beacon");
    assert!(sent(&fx).is_empty(), "{fx:?}");
    assert!(sent(&fire(&mut r, t + 6_000)).is_empty());
}

/// A terminal that went before tmux answered (tmux can't find it, or the listing between
/// no longer shows it) is said to be gone, and nothing is sent.
#[test]
fn closing_a_gone_terminal_says_so() {
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Kite"), "%5");
    let fx = runs(&mut r, n, Err("can't find pane: %5"));
    assert_eq!(refusal(&fx), "that terminal is gone");
    assert!(sent(&fx).is_empty() && lines(&fx).is_empty(), "{fx:?}");

    // That close is over: another asks again; the tab closes before tmux answers.
    let n = close_query(&close(&mut r, 6, "Kite"), "%5");
    r.list(&[P6], &tabs56()[1..]);
    let fx = runs(&mut r, n, Ok("zsh"));
    assert_eq!(refusal(&fx), "that terminal is gone");
    assert!(sent(&fx).is_empty() && lines(&fx).is_empty(), "{fx:?}");
    assert_eq!(refusal(&close(&mut r, 7, "Kite")), "no terminal Kite");
}

/// With the link down, or its tabs not yet reconciled, a close is refused in plain words;
/// one whose question is out when the link goes is told so.
#[test]
fn closing_with_the_link_down_is_refused() {
    let mut r = closable();
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    let fx = close(&mut r, 5, "Kite");
    assert_eq!(
        refusal(&fx),
        "moa isn't open in a terminal yet; run moa first"
    );
    assert!(sent(&fx).is_empty(), "{fx:?}");

    let mut r = Rig::linked(vec![kite(), beacon()]);
    r.list(&[P5, P6], &tabs56());
    let fx = close(&mut r, 5, "Kite");
    assert_eq!(
        refusal(&fx),
        "moa is still looking at its tabs; try again in a moment"
    );
    assert!(sent(&fx).is_empty(), "{fx:?}");

    let mut r = closable();
    close_query(&close(&mut r, 5, "Kite"), "%5");
    let fx = feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    assert_eq!(refusal(&fx), "moa's terminal went away; try again");
    assert!(!closes_any(&fx), "{fx:?}");
}

/// You start an agent in a terminal moa is closing, and moa takes it on: the kill 3 seconds
/// later never goes out.
#[test]
fn a_close_skips_a_pane_that_became_an_agent() {
    let t = t0().0;
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Beacon"), "%6");
    runs(&mut r, n, Ok("node"));
    let agent = format!("P %6 @3 $0 1 0 0 0 {S} 0 ");
    let tabs = [
        tabs56()[0].clone(),
        pane_line("%6", "1", "claude", "/r/b", "⌨ Beacon"),
    ];
    r.list_at(at(UtcMs(t + 1_000)), &[P5, &agent], &tabs);
    assert_eq!(r.st.tmux().topology.moa_session(PaneId(6)), Some(s_id()));
    let fx = fire(&mut r, t + 3_000);
    assert!(!closes_any(&fx), "{fx:?}");
}

/// A close under way is forgotten when the link goes, when another server comes, and on a
/// reload: its kill never goes out, and a new close asks again.
#[test]
fn closing_is_cleared_by_a_new_server_a_lost_link_and_reload() {
    let t = t0().0;
    let interrupted = || {
        let mut r = closable();
        let n = close_query(&close(&mut r, 5, "Beacon"), "%6");
        assert_eq!(
            sent(&runs(&mut r, n, Ok("node"))),
            [Cmd::interrupt(PaneId(6))]
        );
        r
    };
    let connected = |r: &mut Rig, generation| {
        let input = TmuxIn::Connected {
            conn: 2,
            generation,
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        };
        feed(&r.b, &mut r.st, input);
    };
    let relisted = |r: &mut Rig| {
        r.list(&[P5, P6], &tabs56());
        reconciled_again(r);
        assert!(r.st.tmux().reconciled());
    };

    // The link is lost, and comes back to the same server.
    let mut r = interrupted();
    feed(&r.b, &mut r.st, TmuxIn::Down(Down::Lost));
    connected(&mut r, generation());
    relisted(&mut r);
    assert!(!closes_any(&fire(&mut r, t + 3_000)));
    close_query(&close(&mut r, 6, "Beacon"), "%6");

    // Another server, whose `%6` is another shell.
    let mut r = interrupted();
    let mut other = generation();
    other.pid += 1;
    connected(&mut r, other);
    relisted(&mut r);
    assert!(!closes_any(&fire(&mut r, t + 3_000)));

    // A reload.
    let mut r = interrupted();
    r.reload(vec![kite(), beacon()], None);
    relisted(&mut r);
    assert!(!closes_any(&fire(&mut r, t + 3_000)));
    close_query(&close(&mut r, 6, "Beacon"), "%6");
}

/// A timeout that runs out clears itself and says, as moa, that it closed the terminal, in
/// one commit; then it closes it as a close does.
#[test]
fn a_timeout_closes_and_says_so() {
    let t = t0().0;
    let mut r = closable();
    let fx = timeout(&mut r, 5, "Kite", Some(HOUR));
    assert_eq!(the_reply(&fx).0, "closes in 1h");
    assert_eq!(timers(&fx, TimerKey::Terminals), [&set_at(t + HOUR)]);

    let fx = fire(&mut r, t + HOUR);
    let saved: Vec<&Vec<Change>> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(saved.len(), 1, "{fx:?}");
    let cleared = Terminal {
        timeout_at: None,
        ..r.open(5).unwrap().clone()
    };
    assert!(saved[0].contains(&Change::Terminal(cleared)), "{fx:?}");
    assert_eq!(
        lines(&fx),
        [(
            ActivityWho::Brain,
            "moa closed Kite: its timeout ran out".to_string()
        )]
    );
    assert!(r.open(5).unwrap().timeout_at.is_none());
    let n = close_query(&fx, "%5");
    assert!(replies(&fx).is_empty(), "{fx:?}");

    let fx = runs(&mut r, n, Ok("zsh"));
    assert_eq!(sent(&fx), [kill("%5")]);
    assert!(replies(&fx).is_empty() && lines(&fx).is_empty(), "{fx:?}");
}

/// A timeout is set and cleared by `term.timeout`; one out of range is refused.
#[test]
fn a_timeout_can_be_cleared() {
    let t = t0().0;
    let mut r = closable();
    let fx = timeout(&mut r, 5, "Kite", Some(3 * HOUR));
    assert_eq!(the_reply(&fx).0, "closes in 3h");
    assert_eq!(r.open(5).unwrap().timeout_at, Some(UtcMs(t + 3 * HOUR)));
    assert_eq!(timers(&fx, TimerKey::Terminals), [&set_at(t + 3 * HOUR)]);

    let fx = timeout(&mut r, 6, "%5", None);
    assert_eq!(the_reply(&fx).0, "no timeout");
    assert_eq!(r.open(5).unwrap().timeout_at, None);
    assert_eq!(
        timers(&fx, TimerKey::Terminals),
        [&Effect::CancelTimer(TimerKey::Terminals)]
    );
    let fx = fire(&mut r, t + 3 * HOUR);
    assert!(!closes_any(&fx) && changes(&fx).is_empty(), "{fx:?}");

    for bad in [0, 999, 7 * DAY + 1, -HOUR] {
        let fx = timeout(&mut r, 7, "Kite", Some(bad));
        assert_eq!(refusal(&fx), TIMEOUT_FORM, "{bad}");
        assert!(changes(&fx).is_empty(), "{fx:?}");
    }
    assert_eq!(refusal(&timeout(&mut r, 8, "9", None)), "no terminal 9");
}

/// A timeout that ran out while moa was down fires once the link is up and its tabs are
/// reconciled, never before, and never arms a timer in the past.
#[test]
fn a_timeout_that_passed_while_down_fires_once_reconciled() {
    let t = t0().0;
    let mut late = kite();
    late.timeout_at = Some(UtcMs(t - HOUR));
    let mut r = Rig::linked(vec![late, beacon()]);
    let fx = r.list(&[P5, P6], &tabs56());
    assert!(!closes_any(&fx) && lines(&fx).is_empty(), "{fx:?}");
    assert!(timers(&fx, TimerKey::Terminals).is_empty(), "{fx:?}");

    let fx = r.reconcile(&[&info("%5"), &info("%6")]);
    assert_eq!(
        lines(&fx),
        [(
            ActivityWho::Brain,
            "moa closed Kite: its timeout ran out".to_string()
        )]
    );
    close_query(&fx, "%5");
    assert!(r.open(5).unwrap().timeout_at.is_none());
    // Once.
    let fx = r.list(&[P5, P6], &tabs56());
    assert!(!closes_any(&fx) && lines(&fx).is_empty(), "{fx:?}");
}

/// The old server's records with timeouts that ran out are closed by the new server's
/// first listing; nothing is sent to the new server's tabs that reuse their ids.
#[test]
fn an_old_servers_timeout_never_touches_the_new_server() {
    let t = t0().0;
    let mut old = kite();
    old.server_pid = 9;
    old.server_start = 1;
    old.timeout_at = Some(UtcMs(t - HOUR));
    let mut r = Rig::linked(vec![old.clone()]);
    let mut fxs = vec![r.list(&[P5], &tabs56()[..1])];
    fxs.push(r.reconcile(&[&info("%5")]));
    fxs.push(fire(&mut r, t + 1_000));
    for fx in &fxs {
        assert!(!closes_any(fx) && lines(fx).is_empty(), "{fx:?}");
    }
    assert_eq!(
        records(&fxs[1])[0],
        Terminal {
            closed_at: Some(r.b.now().wall),
            timeout_at: None,
            ..old
        }
    );
    assert_eq!(r.open(5).map(|t| t.id), Some(TermId(2)));
}

/// While writes are refused, no timeout fires and no close goes on: nothing is said, so
/// nothing is done.
#[test]
fn writes_refused_close_nothing() {
    let t = t0().0;
    let mut due = kite();
    due.timeout_at = Some(UtcMs(t - HOUR));

    // A timeout that ran out.
    let mut r = closable();
    r.reload(vec![due.clone(), beacon()], Some(t + HOUR));
    let mut fxs = vec![r.list(&[P5, P6], &tabs56())];
    fxs.push(fire(&mut r, t + 1_000));
    for fx in &fxs {
        assert!(!closes_any(fx) && changes(fx).is_empty(), "{fx:?}");
    }

    // A close whose question was out when the store started refusing.
    let mut r = closable();
    let n = close_query(&close(&mut r, 5, "Beacon"), "%6");
    r.reload(vec![kite(), beacon()], Some(t + HOUR));
    let fx = runs(&mut r, n, Ok("node"));
    assert!(replies(&fx).iter().all(|(_, r)| r.is_err()), "{fx:?}");
    assert!(!closes_any(&fx) && changes(&fx).is_empty(), "{fx:?}");

    // A record this build can't write: its timeout waits; the other's goes.
    let mut other = beacon();
    other.timeout_at = Some(UtcMs(t - HOUR));
    let mut r = Rig::linked_with(|snap| {
        hold(snap, vec![due.clone(), other]);
        snap.unknown.push(LoadNote {
            table: "terminals",
            key: "#1".into(),
            reason: "a state this build doesn't know",
        });
    });
    r.list(&[P5, P6], &tabs56());
    let fx = r.reconcile(&[&info("%5"), &info("%6")]);
    close_query(&fx, "%6");
    assert!(!sent(&fx).contains(&what_runs("%5")), "{fx:?}");
    assert_eq!(r.open(5).unwrap().timeout_at, Some(UtcMs(t - HOUR)));
}

/// A timeout that can't run yet is retried by the inputs that follow, never by a timer
/// armed at a time already past: each input arms at most one, always ahead.
#[test]
fn a_blocked_timeout_never_arms_a_past_timer() {
    let t = t0().0;
    let mut due = kite();
    due.timeout_at = Some(UtcMs(t - HOUR));
    let mut ahead = beacon();
    ahead.timeout_at = Some(UtcMs(t + DAY));
    // Writes refused, then a row this build can't write.
    let mut r = closable();
    let mut fxs = vec![r.reload(vec![due.clone(), ahead.clone()], Some(t + HOUR))];
    let mut blocked = Rig::linked_with(|snap| {
        hold(snap, vec![due.clone(), ahead.clone()]);
        snap.unknown.push(LoadNote {
            table: "terminals",
            key: "#1".into(),
            reason: "a state this build doesn't know",
        });
    });
    for (i, rig) in [&mut r, &mut blocked].into_iter().enumerate() {
        fxs.push(rig.list(&[P5, P6], &tabs56()));
        if i == 1 {
            fxs.push(rig.reconcile(&[&info("%5"), &info("%6")]));
        }
        for k in 1..=5 {
            let wall = t + k * MIN;
            let fx = fire(rig, wall);
            for e in timers(&fx, TimerKey::Terminals) {
                if let Effect::SetTimer { at, .. } = e {
                    assert!(at.0 > wall, "{fx:?}");
                }
            }
            fxs.push(fx);
        }
    }
    for fx in &fxs {
        let set = timers(fx, TimerKey::Terminals)
            .into_iter()
            .filter(|e| matches!(e, Effect::SetTimer { .. }))
            .count();
        assert!(set <= 1, "{fx:?}");
        if let Some(Effect::SetTimer { at, .. }) = timers(fx, TimerKey::Terminals).first() {
            assert!(at.0 > t, "{fx:?}");
        }
        assert!(!closes_any(fx), "{fx:?}");
    }
}

/// Thirty terminals open, and ordinary activity: an hour of the status clock, the agent's
/// tab changing its active pane, an unchanged listing. moa sends nothing for them, arms no
/// timer, and saves nothing about them.
#[test]
fn thirty_idle_terminals_cost_nothing() {
    let t = t0().0;
    let mut r = Rig::linked(Vec::new());
    let mut panes: Vec<String> = (10..40)
        .map(|n| format!("P %{n} @{n} $0 1 0 0 0  0 "))
        .collect();
    panes.push("P %7 @4 $0 1 1 0 0  0 ".into());
    let panes: Vec<&str> = panes.iter().map(String::as_str).collect();
    let mut tabs: Vec<String> = (10..40).map(|n| plain(&format!("%{n}"), "/r")).collect();
    tabs.push(pane_line("%7", "", "claude", "/r", ""));
    r.list_at(at(UtcMs(t)), &panes, &tabs);
    let mut infos: Vec<String> = (10..40).map(|n| info(&format!("%{n}"))).collect();
    infos.push(info("%7"));
    let infos: Vec<&str> = infos.iter().map(String::as_str).collect();
    let fx = r.reconcile(&infos);
    assert_eq!(records(&fx).len(), 30, "{fx:?}");
    let terminals: Vec<String> = (10..40).map(|n| format!("%{n}")).collect();

    let mut fxs = Vec::new();
    for k in 1..=60 {
        let now = at(UtcMs(t + k * MIN));
        fxs.push(handle_checked(
            &mut r.st,
            Input::Timer(TimerKey::Clock),
            now.clone(),
        ));
        if k % 10 == 0 {
            let moved = Notification::WindowPaneChanged {
                window: WindowId(4),
                pane: PaneId(7),
            };
            fxs.push(r.notify(now, moved));
        }
    }
    fxs.push(r.list_at(at(UtcMs(t + HOUR)), &panes, &tabs));
    for fx in &fxs {
        assert!(timers(fx, TimerKey::Terminals).is_empty(), "{fx:?}");
        assert!(records(fx).is_empty(), "{fx:?}");
        let named = sent(fx)
            .into_iter()
            .any(|c| c.argv().iter().any(|a| terminals.contains(a)));
        assert!(!named, "{fx:?}");
    }
}

#[test]
fn parse_after_reads_3h_90m_2h30m_45s_and_off() {
    assert_eq!(parse_after("3h"), Ok(Some(3 * HOUR)));
    assert_eq!(parse_after("90m"), Ok(Some(90 * MIN)));
    assert_eq!(parse_after("2h30m"), Ok(Some(150 * MIN)));
    assert_eq!(parse_after("45s"), Ok(Some(45_000)));
    assert_eq!(parse_after("1s"), Ok(Some(1_000)));
    assert_eq!(parse_after("7d"), Ok(Some(7 * DAY)));
    assert_eq!(parse_after(" 3h "), Ok(Some(3 * HOUR)));
    assert_eq!(parse_after("off"), Ok(None));
    for bad in ["0s", "8d", "soon", "-1h", "", "1y"] {
        assert_eq!(parse_after(bad), Err(TIMEOUT_FORM.to_string()), "{bad:?}");
    }
}
