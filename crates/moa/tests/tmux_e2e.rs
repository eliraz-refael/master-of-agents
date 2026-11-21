//! Slice 3 end to end: the Typical brain adopted into a TempBrain, `moa` on a `Terminal`,
//! and keys typed as a user types them. Every key and menu item reaches the daemon without
//! a launch; the status line follows the brain; a killed server comes back as a new
//! generation; keys say so when the daemon is gone; a user's own tmux.conf keeps working.
//! And slice 5a's probe: tmux hands mouse motion and focus reports to a pane that asked.
//! Between keys the tests wait for what tmux shows (a notice, the menu, the key table), never
//! for a fixed time: on tmux 3.4 a key typed the instant a binding ran can be lost.

mod common;

use std::time::Duration;

use moa_core::op::Op;
use moa_core::tmux::mailbox::KeyAction;
use moa_core::tmux::status::counts_text;
use moa_testkit::brain::{TypicalBrain, run as run_op};
use moa_testkit::{Sandbox, TempBrain, Terminal, ids, tmux_path, wait_for};

use common::*;

/// What a key says when the daemon isn't there.
const NOT_RUNNING: &str = "moa isn't running: run moa";
/// How long a notice or a status write may take to show.
const QUICK: Duration = Duration::from_secs(2);

/// A root with the Typical brain adopted as the default brain.
fn typical() -> TempBrain {
    let tb = TempBrain::new(None);
    tb.adopt_typical(B);
    tb
}

fn option(tb: &TempBrain, name: &str) -> String {
    tmux(tb, &["show-options", "-gqv", name])
}

/// Ctrl-a then `key`; `says` shows within 2 s.
fn moa_key(tb: &TempBrain, term: &Terminal, key: &str, says: &str) {
    ctrl_a(tb, term);
    term.press(key.as_bytes());
    assert!(
        term.wait_for_text(says, QUICK),
        "Ctrl-a {key} never said {says:?}"
    );
}

/// The notice a daemon key shows in this build.
fn notice(a: KeyAction) -> String {
    format!("moa: {}", a.not_yet().unwrap())
}

fn launches(tb: &TempBrain) -> u64 {
    let s = status_json(tb);
    s["launches"].as_u64().unwrap_or_else(|| panic!("{s}"))
}

/// How many sessions a status text counts, whatever their state.
fn counted(text: &str) -> usize {
    text.split("  ")
        .filter_map(|part| part.rsplit(' ').next()?.parse::<usize>().ok())
        .sum()
}

/// Waits (10 s) for `@moa_status` to be `want`: it may wait for the connection's reconcile.
fn status_becomes(tb: &TempBrain, want: &str) {
    let seen = wait_for(SHOWS, || (option(tb, "@moa_status") == want).then_some(()));
    assert!(
        seen.is_some(),
        "@moa_status is {:?}, not {want:?}",
        option(tb, "@moa_status")
    );
}

/// Waits (2 s) for `moa status` to say `line`.
fn terminal_says(tb: &TempBrain, line: &str) {
    let want = format!("\n{line}\n");
    let seen = wait_for(QUICK, || {
        out(&run(tb, &["status"], &[]))
            .contains(&want)
            .then_some(())
    });
    assert!(seen.is_some(), "{}", out(&run(tb, &["status"], &[])));
}

/// Session hooks and window hooks, which tmux lists apart.
fn hooks(tb: &TempBrain) -> String {
    tmux(tb, &["show-hooks", "-g"]) + "\n" + &tmux(tb, &["show-hooks", "-gw"])
}

#[test]
fn keys_and_menu_reach_the_daemon_with_zero_launches() {
    let tb = typical();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    let before = launches(&tb);

    // `$` opens a tab now (slice 5a's Task 14 tests it end to end), and the cockpit's keys
    // act (cockpit_run.rs in moa-core): the rest still say they're coming. `/` opens the
    // picker's popup (moa-tmux's conf.rs tests it), and the full tab is `s`, the daemon's
    // `find`.
    for (key, a) in [("s", KeyAction::Find), ("?", KeyAction::Help)] {
        moa_key(&tb, &term, key, &notice(a));
    }
    // `n` asks for a folder in the status line; Ctrl-c puts it away and answers nothing.
    // (Esc would too, but tmux reads an Esc then the next Ctrl-a as one key.)
    moa_key(&tb, &term, KeyAction::New.key(), "new session · folder:");
    term.press(b"\x03");

    // The menu, then an item in it.
    ctrl_a(&tb, &term);
    term.press(b" ");
    assert!(
        term.wait_for_text(KeyAction::Plan.label(), QUICK),
        "the menu never showed"
    );
    term.mark();
    term.press(KeyAction::Help.key().as_bytes());
    assert!(
        term.wait_for_text(&notice(KeyAction::Help), QUICK),
        "the menu's help item never answered"
    );

    // Pure keys: the next and previous tab, and no split in the cockpit.
    tmux(&tb, &["new-window", "-d", "-t", "=moa:"]);
    let index = || client_field(&tb, &term, "#{window_index}");
    assert_eq!(index().as_deref(), Some("1"));
    for (key, want) in [("]", "2"), ("[", "1")] {
        ctrl_a(&tb, &term);
        term.press(key.as_bytes());
        let moved = wait_for(QUICK, || (index().as_deref() == Some(want)).then_some(()));
        assert!(moved.is_some(), "Ctrl-a {key}: the tab is {:?}", index());
    }
    moa_key(&tb, &term, "|", "the cockpit is a layout");

    assert_eq!(launches(&tb), before, "a key launched something");
}

#[test]
fn the_status_line_follows_the_brain() {
    let tb = typical();
    let brain = TypicalBrain::new();
    // As the daemon shows it once it has looked at its tabs: the live sessions no tab runs
    // are asleep.
    let mut state = brain.reconciled_state();
    let want = counts_text(&state);
    assert!(!want.is_empty());

    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    status_becomes(&tb, &want);

    // A live session the status line counts: its spike tab is on the spike's own server, so
    // it stays live. It reads as a question: its last words ask one.
    let now = brain.now();
    let o = run_exe(&tb, &moactl(&tb), &["archive", ids::S2_IDLE], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let op = Op::SessionArchive {
        qs: vec![ids::S2_IDLE.to_string()],
    };
    run_op(&mut state, op, &now).expect("the archive applies");
    let after = counts_text(&state);
    assert_eq!(counted(&after), counted(&want) - 1, "{after:?}");
    status_becomes(&tb, &after);
}

#[test]
fn the_clock_shows_while_attached() {
    let tb = typical();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    let hh_mm = |s: &str| {
        let b = s.as_bytes();
        b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
    };
    let shown = wait_for(QUICK, || hh_mm(&option(&tb, "@moa_clock")).then_some(()));
    assert!(
        shown.is_some(),
        "@moa_clock is {:?}",
        option(&tb, "@moa_clock")
    );
}

#[test]
fn kill_server_then_moa_gives_a_new_generation() {
    let tb = typical();
    let first = open(&tb, &[], &[]);
    came_up(&tb, &first);
    let old = status_json(&tb)["tmux"]["pid"].clone();
    assert_eq!(old.to_string(), server_pid(&tb));
    assert_eq!(status_json(&tb)["tmux_restarts"], 0);

    tmux(&tb, &["kill-server"]);
    terminal_says(&tb, "terminal: not connected");

    let second = open(&tb, &[], &[]);
    came_up(&tb, &second);
    terminal_says(&tb, "terminal: connected");
    let s = status_json(&tb);
    assert_eq!(s["tmux"]["pid"].to_string(), server_pid(&tb), "{s}");
    assert_ne!(s["tmux"]["pid"], old, "{s}");
    assert_eq!(s["tmux_restarts"], 1, "{s}");
}

#[test]
fn keys_say_moa_isnt_running_when_the_daemon_is_gone() {
    let tb = typical();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);

    // A clean stop.
    shutdown(&tb);
    moa_key(&tb, &term, "c", NOT_RUNNING);

    // Back again, from a second terminal; the first one's keys work.
    let again = open(&tb, &[], &[]);
    came_up(&tb, &again);
    moa_key(&tb, &term, "?", &notice(KeyAction::Help));

    // A crash.
    let pid = status_json(&tb)["pid"].to_string();
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap();
    assert!(killed.success());
    // The server clears `@moa_ctl` once it sees the client go, a moment after the kill.
    let cleared = wait_for(SHOWS, || option(&tb, "@moa_ctl").is_empty().then_some(()));
    assert!(
        cleared.is_some(),
        "@moa_ctl is {:?}",
        option(&tb, "@moa_ctl")
    );
    moa_key(&tb, &term, "c", NOT_RUNNING);
}

/// Step 1b of slice 5a's plan, which decides V11 (the hover mark) against V22 (a first click
/// only focuses): tmux forwards focus reports, motion and clicks to a pane that asked for
/// them, while another pane is the active one. The probe pane turns on every mouse event
/// (`?1003h`), SGR (`?1006h`) and focus reports (`?1004h`), then copies its raw input, byte by
/// byte, to a file. No moa here: a plain sandbox server with the mouse on, as moa sets it.
#[test]
fn tmux_forwards_motion_and_focus_to_a_pane_that_asked() {
    let sb = Sandbox::start().unwrap_or_else(|e| panic!("{e}"));
    let tmux_at = |args: &[&str]| sb.tmux(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    // As moa.tmux sets them (V11). Without focus-events, no pane hears a focus report.
    tmux_at(&["set", "-g", "mouse", "on"]);
    tmux_at(&["set", "-s", "focus-events", "on"]);
    let program = tmux_path().unwrap_or_else(|e| panic!("{e}"));
    let mut args: Vec<std::ffi::OsString> = vec!["-u".into()];
    args.extend(sb.socket_args());
    args.extend(["attach".into(), "-t".into(), "=sandbox".into()]);
    let term = Terminal::spawn(&program, &args, &sb.env(), 100, 30).unwrap();
    let tty = term.tty();
    let attached = wait_for(SHOWS, || {
        let list = sb.tmux(&["list-clients", "-F", "#{client_name}"]).ok()?;
        list.lines().any(|l| l == tty).then_some(())
    });
    assert!(attached.is_some(), "the terminal never attached");
    term.mark();

    let seen_at = sb.home().join("seen");
    let seen = || std::fs::read(&seen_at).unwrap_or_default();
    let holds = |want: &[u8]| seen().windows(want.len()).any(|w| w == want);
    let probe_cmd = format!(
        "printf '\\033[?1003h\\033[?1006h\\033[?1004h'; stty raw -echo; exec dd bs=1 of='{}'",
        seen_at.display()
    );
    let probe = tmux_at(&[
        "new-window",
        "-t",
        "=sandbox:",
        "-P",
        "-F",
        "#{pane_id}",
        &probe_cmd,
    ]);
    let field = |pane: &str, format: &str| tmux_at(&["display-message", "-p", "-t", pane, format]);
    // Raw, and asking for the mouse: `dd` makes the file once `stty` is done.
    let ready = wait_for(SHOWS, || {
        (seen_at.exists() && field(&probe, "#{mouse_all_flag}") == "1").then_some(())
    });
    assert!(
        ready.is_some(),
        "the probe never ran: {}",
        field(&probe, "#{mouse_all_flag}")
    );
    let other = tmux_at(&["split-window", "-h", "-t", &probe, "-P", "-F", "#{pane_id}"]);

    // 1. Focus moves off the probe: it hears a focus-out.
    tmux_at(&["select-pane", "-t", &probe]);
    tmux_at(&["select-pane", "-t", &other]);
    let out = wait_for(SHOWS, || holds(b"\x1b[O").then_some(()));
    assert!(
        out.is_some(),
        "no focus-out: {:?}",
        String::from_utf8_lossy(&seen())
    );
    assert_eq!(field(&probe, "#{pane_active}"), "0");

    // 2. With the other half active, tmux still asks the terminal for every mouse event, for
    // the probe that is on screen: the last `?1003` it sent is `h`, not the `l` it clears
    // modes with. The active pane goes back and forth first, ending on the other half; a
    // notice drawn after that shows the terminal has everything tmux sent for it.
    for pane in [&probe, &other, &probe, &other] {
        tmux_at(&["select-pane", "-t", pane]);
    }
    tmux_at(&["display-message", "-c", &tty, "moa-probe-synced"]);
    assert!(
        term.wait_for_text("moa-probe-synced", SHOWS),
        "the notice never showed: {}",
        term.tail(400)
    );
    assert_eq!(field(&probe, "#{pane_active}"), "0");
    // `tail` escapes ESC as `\u{1b}`.
    let output = term.tail(usize::MAX);
    let last = |mode: &str| output.rfind(&format!("\\u{{1b}}[?1003{mode}"));
    assert!(
        last("h").is_some_and(|h| last("l").is_none_or(|l| l < h)),
        "the terminal's last ?1003 isn't h: {}",
        term.tail(400)
    );

    // 3. The pointer moves over the probe, which isn't the active pane: it hears the motion.
    let at = field(&probe, "#{pane_left} #{pane_top}");
    let (left, top) = at.split_once(' ').unwrap();
    let (x, y) = (
        left.parse::<u32>().unwrap() + 3,
        top.parse::<u32>().unwrap() + 2,
    );
    term.press(format!("\x1b[<35;{x};{y}M").as_bytes());
    let moved = wait_for(SHOWS, || holds(b"[<35;").then_some(()));
    assert!(
        moved.is_some(),
        "no motion: {:?}",
        String::from_utf8_lossy(&seen())
    );
    assert_eq!(field(&probe, "#{pane_active}"), "0");

    // 4. A left click there reaches it, and makes it the active pane.
    term.press(format!("\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m").as_bytes());
    let clicked = wait_for(SHOWS, || holds(b"[<0;").then_some(()));
    assert!(
        clicked.is_some(),
        "no click: {:?}",
        String::from_utf8_lossy(&seen())
    );
    let active = wait_for(SHOWS, || {
        (field(&probe, "#{pane_active}") == "1").then_some(())
    });
    assert!(active.is_some(), "the click never made the probe active");
}

/// The hooks moa sets at index 90.
const MOA_HOOKS: [&str; 5] = [
    "pane-exited",
    "pane-died",
    "window-layout-changed",
    "client-attached",
    "client-detached",
];

/// The user's own file, with screen's prefix (Ctrl-a), a waiting command and a hook.
#[test]
fn a_user_tmux_conf_with_run_shell_and_hooks() {
    let tb = typical();
    std::fs::write(
        tb.root().join(".tmux.conf"),
        "set -g prefix C-a\n\
         set -g @user_opt 1\n\
         run-shell 'sleep 1'\n\
         set-hook -g pane-exited[5] 'set -g @user_hook 1'\n",
    )
    .unwrap();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    moa_key(&tb, &term, "?", &notice(KeyAction::Help));

    assert_eq!(option(&tb, "@user_opt"), "1");
    let set = hooks(&tb);
    assert!(set.contains("pane-exited[5]"), "{set}");
    for hook in MOA_HOOKS {
        assert!(set.contains(&format!("{hook}[90]")), "{hook}: {set}");
    }

    shutdown(&tb);
    let gone = wait_for(QUICK, || (!hooks(&tb).contains("[90]")).then_some(()));
    assert!(gone.is_some(), "{}", hooks(&tb));
    assert!(hooks(&tb).contains("pane-exited[5]"), "{}", hooks(&tb));
}

/// A user's `detach-on-destroy off` would drop the terminal into `_moa` when the `moa`
/// session's last shell goes. The `moa` session sets its own `on`, so the terminal leaves.
#[test]
fn a_user_detach_on_destroy_off_never_lands_in_moas_hidden_session() {
    let tb = typical();
    std::fs::write(
        tb.root().join(".tmux.conf"),
        "set -g detach-on-destroy off\n",
    )
    .unwrap();
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    assert_eq!(option(&tb, "detach-on-destroy"), "off");
    assert_eq!(
        tmux(
            &tb,
            &["show-options", "-t", "moa", "-v", "detach-on-destroy"]
        ),
        "on"
    );

    clients_become(&tb, &["moa"]);
    tmux(&tb, &["kill-session", "-t", "=moa"]);
    clients_become(&tb, &[]);
}
