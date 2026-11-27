//! Slice 9 end to end: a new build of moa takes over the cockpit in place (§6.15). The
//! cockpit runs from an install of its own in a TempBrain's terminal server, with real `moa
//! panel` processes in its panes, `stub-acp` as moa's agent and fake-claude sessions
//! `moactl open --bg` started; the clock is the real one. The install is rebuilt as a user's
//! is: a new program written beside it and renamed over it ([`rebuild`]). What changed is
//! read back from `moa status`, the panels, the console and the store. Every wait is for what
//! moa shows or says, never for a fixed time.

mod common;

use std::process::Output;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use moa_platform::ensure_private_dir;
use moa_testkit::{FakeStep, StubStep, TempBrain, Terminal, wait_for, write_stub_script};
use serde_json::{Map, Value};

use common::*;

/// The terminal: tall enough that the console holds the hello, its answer and the notice.
const SIZE: (u16, u16) = (120, 50);
/// moa's answer to the user's hello, which gives moa its thread.
const HELLO: &str = "hello there";
/// What `status` says while an upgrade waits for moa's turn (L4).
const WAITING: &str = "waiting for moa's turn";
/// The gate in moa's agent's folder that ends its held turn.
const GATE: &str = "gate";
/// How many hooks a ticking session fires, 100 ms apart: far more than a test runs for, so
/// it is still firing when each test is done.
const TICKS: usize = 600;
/// The ticking sessions. A hook is lost only if it lands in the handoff, which lasts some
/// tens of milliseconds: one session's hooks land there in about three runs of four, three
/// sessions' in nearly every run.
const TICKERS: [&str; 3] = ["kite", "harbor", "lark"];
/// The `moa status` loops run side by side: a call in flight as the old image winds down
/// asks the new one again, in the handoff.
const STATUS_LOOPS: usize = 2;

/// The cockpit run from its install, and its console's pane.
struct Up {
    i: Installed,
    console: String,
}

/// A root whose moa runs the stub with `moa` as its script (its first turn answers
/// [`HELLO`]), opening sessions with fake-claude, and the cockpit run from an install of its
/// own. The user has said hello, so moa has a thread for the notice to go in.
fn up(moa: &[StubStep]) -> Up {
    let tb = TempBrain::new(Some(&stub_moa_toml_opening_sessions()));
    // moa opened the day already: attaching opens nothing here (D2).
    tb.opened_today();
    let agent = tb.paths().agent_dir(B);
    ensure_private_dir(&agent).expect("the agent folder");
    write_stub_script(&agent, moa);
    let i = installed_in(tb, SIZE);
    let console = cockpit_panes(&i.tb)
        .into_iter()
        .find(|p| p.tag == "console")
        .expect("the console's pane")
        .id;
    let r = Up { i, console };
    // Keys sent before the panel draws its input line could be lost.
    r.shows(&["ask moa, or / for commands"]);
    r.type_in("hello");
    r.shows(&[HELLO, "● ready"]);
    r
}

/// [`up`], with moa then thinking: its second turn waits for [`GATE`] ([`Up::open_gate`]).
fn thinking() -> Up {
    let r = up(&[
        say(HELLO, "m0"),
        end(),
        StubStep::Wait { file: GATE.into() },
        say("thought it over", "m1"),
        end(),
    ]);
    r.type_in("think it over");
    r.shows(&["thinking… Esc cancels"]);
    r
}

impl Up {
    fn tb(&self) -> &TempBrain {
        &self.i.tb
    }

    /// Waits until the console shows every one of `texts`.
    #[track_caller]
    fn shows(&self, texts: &[&str]) {
        shows(self.tb(), &self.console, texts);
    }

    /// `text` typed in the console, then Enter.
    fn type_in(&self, text: &str) {
        tmux(self.tb(), &["send-keys", "-t", &self.console, "-l", text]);
        tmux(self.tb(), &["send-keys", "-t", &self.console, "Enter"]);
    }

    /// Ends moa's held turn ([`thinking`]).
    fn open_gate(&self) {
        let gate = self.tb().paths().agent_dir(B).join(GATE);
        std::fs::write(gate, "").expect("open the gate");
    }

    /// `moa status --json`'s `pid`.
    fn pid(&self) -> Value {
        status_json(self.tb())["pid"].clone()
    }

    /// Waits until `status` says the upgrade waits for moa's turn.
    #[track_caller]
    fn upgrade_waits(&self) {
        let seen = wait_for(SHOWS, || {
            (status_json(self.tb())["upgrade"] == WAITING).then_some(())
        });
        assert!(
            seen.is_some(),
            "the upgrade never waited for moa: {}\n{}",
            status_json(self.tb()),
            what_happened(self.tb(), &self.i.term)
        );
    }

    /// `<install>/moa upgrade`, started; it is running once `status` says the upgrade waits
    /// for moa's turn, which nothing else asked for.
    #[track_caller]
    fn start_upgrade(&self) -> Reaped {
        assert!(
            status_json(self.tb())["upgrade"].is_null(),
            "an upgrade is wanted already"
        );
        let child = spawn_exe(self.tb(), &self.i.exe, &["upgrade"], &[]);
        self.upgrade_waits();
        child
    }
}

/// fake-claude's script for a session that starts, then fires a `UserPromptSubmit` every
/// 100 ms, prompted `tick 1`, `tick 2` and on: each hook its own `turn.started` event.
fn ticking() -> Vec<FakeStep> {
    let mut steps = vec![start_hook()];
    for n in 1..=TICKS {
        steps.push(FakeStep::Hook {
            event: "UserPromptSubmit".into(),
            fields: Map::from_iter([("prompt".to_string(), Value::String(format!("tick {n}")))]),
        });
        steps.push(FakeStep::Sleep { ms: 100 });
    }
    steps.push(FakeStep::Hang);
    steps
}

/// The ticks in session `id`'s events, in order.
fn ticks(tb: &TempBrain, id: &str) -> Vec<usize> {
    let query = format!(
        "select text from events where session_id = '{id}' and kind = 'turn.started' \
         and text like 'tick %'"
    );
    let o = run(tb, &["sql", &query], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let mut ticks: Vec<usize> = out(&o)
        .lines()
        .skip(1)
        .map(|l| l["tick ".len()..].parse().unwrap_or_else(|_| panic!("{l}")))
        .collect();
    ticks.sort_unstable();
    ticks
}

/// Waits until session `id`'s events hold ticks 1 to at least `n`, none missing.
#[track_caller]
fn ticked_through(tb: &TempBrain, term: &Terminal, id: &str, n: usize) {
    let whole = |t: &[usize]| t.iter().copied().eq(1..=t.len());
    let seen = wait_for(SHOWS, || {
        let t = ticks(tb, id);
        (t.len() >= n && whole(&t)).then_some(())
    });
    let t = ticks(tb, id);
    let missing: Vec<usize> = (1..=t.last().copied().unwrap_or(0))
        .filter(|k| t.binary_search(k).is_err())
        .collect();
    assert!(
        seen.is_some(),
        "the ticks never reached {n} whole: {} in, up to {:?}, missing {missing:?}\n{}",
        t.len(),
        t.last(),
        what_happened(tb, term)
    );
}

/// Sets its flag when dropped: a loop that reads it ends even when the test fails.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The roadmap's "done when": a rebuild while the cockpit runs updates everything in place,
/// and nothing is lost. Sessions each fire a hook every 100 ms ([`TICKERS`]), and `moa
/// status` (from the install, as a user runs it) runs back to back throughout
/// ([`STATUS_LOOPS`]). After the rebuild:
/// - `moa status` shows the same pid, on the new build;
/// - every `moa status` succeeded, each answered by that pid, and none on the old build
///   after one on the new;
/// - every hook each session fired is in its events, none missing, and each fired on after
///   the upgrade (a hook lost in the handoff is a gap below the later ones);
/// - each panel is linked on the new build, in the same pane, as the same process;
/// - the console says `moa upgraded: nothing to do`.
#[test]
fn a_rebuild_updates_everything_in_place() {
    let r = up(&[say(HELLO, "m0"), end()]);
    let (tb, term) = (r.tb(), &r.i.term);
    let tickers: Vec<String> = TICKERS
        .iter()
        .map(|name| open_session_with(tb, term, name, name, None, &ticking()).id)
        .collect();
    for id in &tickers {
        ticked_through(tb, term, id, 3);
    }
    let old = build_of(tb, &r.i.exe);
    let pid = r.pid();
    let pids = r.i.pids();

    let replies: Mutex<Vec<Output>> = Mutex::new(Vec::new());
    let stop = AtomicBool::new(false);
    let new = std::thread::scope(|s| {
        for _ in 0..STATUS_LOOPS {
            s.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let o = run_exe(tb, &r.i.exe, &["status", "--json"], &[]);
                    replies.lock().unwrap().push(o);
                }
            });
        }
        let _stop = StopOnDrop(&stop);
        rebuild(&r.i.exe);
        let new = build_of(tb, &r.i.exe);
        assert_ne!(new, old, "the rebuild is a new build");
        // Until the loop is answered by the new build, nothing else calls moa: a call lost
        // in the handoff is the loop's.
        let on_new = |o: &Output| {
            serde_json::from_str::<Value>(&out(o)).is_ok_and(|s| s["build"] == new.as_str())
        };
        let seen = wait_for(SHOWS, || {
            replies.lock().unwrap().iter().any(on_new).then_some(())
        });
        assert!(seen.is_some(), "no status call was answered on {new}");
        new
    });
    let replies = replies.into_inner().unwrap();

    // Every status call was answered, by the one daemon, which never went back.
    let failed: Vec<String> = replies
        .iter()
        .filter(|o| !o.status.success())
        .map(|o| out(o) + &err(o))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} status calls failed: {failed:?}",
        failed.len(),
        replies.len()
    );
    let mut on_new = false;
    for o in &replies {
        let s: Value = serde_json::from_str(&out(o)).unwrap();
        assert_eq!(s["pid"], pid, "{s}");
        on_new |= s["build"] == new.as_str();
        let want = if on_new { &new } else { &old };
        assert_eq!(s["build"], want.as_str(), "{s}");
    }
    // The hooks: the session kept firing past the upgrade, and every one is in.
    for id in &tickers {
        let upgraded_at = ticks(tb, id).len();
        ticked_through(tb, term, id, upgraded_at + 3);
    }
    r.i.all_linked_on(&new);
    r.shows(&["moa upgraded: nothing to do"]);

    let after = status_json(tb);
    assert_eq!(after["pid"], pid, "the daemon upgraded in place");
    assert_eq!(after["build"], new.as_str(), "{after}");
    assert_eq!(
        r.i.pids(),
        pids,
        "a panel's process changed: {:?}",
        r.i.panes
    );
    assert!(
        cockpit_panes(tb).iter().all(|p| !p.dead),
        "{:#?}",
        cockpit_panes(tb)
    );
}

/// `moa upgrade`, asked while moa thinks, waits for the turn, then says from which build to
/// which: here the build put in place meanwhile.
#[test]
fn moa_upgrade_says_so() {
    let r = thinking();
    let old = build_of(r.tb(), &r.i.exe);
    let pid = r.pid();
    let upgrade = r.start_upgrade();
    rebuild(&r.i.exe);
    let new = build_of(r.tb(), &r.i.exe);
    assert_ne!(new, old, "the rebuild is a new build");

    r.open_gate();

    let o = upgrade.ended();
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert_eq!(out(&o), format!("moa upgraded ({old} → {new})\n"));
    r.shows(&["thought it over", "moa upgraded: nothing to do"]);
    let after = status_json(r.tb());
    assert_eq!(after["pid"], pid, "{after}");
    assert_eq!(after["build"], new.as_str(), "{after}");
}

/// `moa upgrade` says so when the daemon stopped instead of coming back: its program was
/// replaced by something that isn't a program while the upgrade waited for moa's turn.
#[test]
fn moa_upgrade_says_when_moa_stopped_instead() {
    let r = thinking();
    let upgrade = r.start_upgrade();
    let broken = r.i.exe.with_extension("next");
    std::fs::write(&broken, "not a program\n").unwrap();
    std::fs::rename(&broken, &r.i.exe).unwrap();

    r.open_gate();

    let o = upgrade.ended();
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert_eq!(
        err(&o),
        "moa default stopped while upgrading; run moa to start it again\n"
    );
    assert_eq!(out(&o), "");
    let o = run(r.tb(), &["status"], &[]);
    assert!(!o.status.success(), "{}{}", out(&o), err(&o));
}

/// A panel started while the upgrade waits for moa's turn is the new build, and moa still
/// the old: it says it is from another build, then links once the upgrade lands, in its own
/// pane, as the process it is.
#[test]
fn a_panel_started_while_moa_thinks_links_after_the_upgrade() {
    let mut r = thinking();
    rebuild(&r.i.exe);
    let new = build_of(r.tb(), &r.i.exe);
    r.upgrade_waits();
    let late = panel_tab_from(r.tb(), &r.i.exe, "projects");
    shows(r.tb(), &late, &[MISMATCH]);
    r.i.panes.push(late);
    let pids = r.i.pids();

    r.open_gate();

    r.i.all_linked_on(&new);
    assert_eq!(
        r.i.pids(),
        pids,
        "a panel's process changed: {:?}",
        r.i.panes
    );
    shows(r.tb(), r.i.panes.last().unwrap(), &["▦ projects"]);
}

/// An older build's session plugin: today's, less its `CwdChanged` hook.
fn older_plugin(tb: &TempBrain) {
    let file = tb.paths().session_plugin().join("hooks/hooks.json");
    let mut plugin: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let hooks = plugin["hooks"].as_object_mut().unwrap();
    assert!(hooks.remove("CwdChanged").is_some(), "{plugin}");
    std::fs::write(&file, serde_json::to_string_pretty(&plugin).unwrap() + "\n").unwrap();
}

/// Whether the session plugin on disk has a `CwdChanged` hook.
fn plugin_has_cwd_hook(tb: &TempBrain) -> bool {
    let file = tb.paths().session_plugin().join("hooks/hooks.json");
    let plugin: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    plugin["hooks"].get("CwdChanged").is_some()
}

/// L7: a rebuild that changes the session plugin says that the session running keeps the
/// hooks it started with. The plugin on disk is an older build's, which the session starts
/// with; the new build writes its own, and the console says so, in the singular.
#[test]
fn a_running_session_hears_about_new_hooks() {
    let r = up(&[say(HELLO, "m0"), end()]);
    older_plugin(r.tb());
    open_session(r.tb(), &r.i.term, "kite", "Kite", None);
    let pid = r.pid();

    rebuild(&r.i.exe);
    let new = build_of(r.tb(), &r.i.exe);

    let line =
        "moa upgraded: 1 running session uses the old hooks; it gets the new ones when resumed";
    shows_wrapped(r.tb(), &r.console, line);
    assert!(plugin_has_cwd_hook(r.tb()), "the new plugin wasn't written");
    assert!(activity(r.tb()).contains(line), "{}", activity(r.tb()));
    let after = status_json(r.tb());
    assert_eq!(after["pid"], pid, "{after}");
    assert_eq!(after["build"], new.as_str(), "{after}");
}
