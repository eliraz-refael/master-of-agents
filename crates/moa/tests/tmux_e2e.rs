//! Slice 3 end to end: the Typical brain adopted into a TempBrain, `moa` on a `Terminal`,
//! and keys typed as a user types them. Every key and menu item reaches the daemon without
//! a launch; the status line follows the brain; a killed server comes back as a new
//! generation; keys say so when the daemon is gone; a user's own tmux.conf keeps working.
//! Between keys the tests wait for what tmux shows (a notice, the menu, the key table), never
//! for a fixed time: on tmux 3.4 a key typed the instant a binding ran can be lost.

mod common;

use std::time::Duration;

use moa_core::live::{DisplayState, display_state};
use moa_core::op::Op;
use moa_core::tmux::mailbox::KeyAction;
use moa_core::tmux::status::counts_text;
use moa_testkit::brain::{TypicalBrain, run as run_op};
use moa_testkit::{TempBrain, Terminal, wait_for};

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

/// `format` for the client on `term`'s pty, or `None` while it isn't attached.
fn client_field(tb: &TempBrain, term: &Terminal, format: &str) -> Option<String> {
    let list = tmux(
        tb,
        &["list-clients", "-F", &format!("#{{client_name}} {format}")],
    );
    let prefix = format!("{} ", term.tty());
    list.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(str::to_string)
}

/// Waits until `term`'s client is in key table `table`.
fn key_table_becomes(tb: &TempBrain, term: &Terminal, table: &str) {
    let seen = wait_for(SHOWS, || {
        (client_field(tb, term, "#{client_key_table}").as_deref() == Some(table)).then_some(())
    });
    assert!(
        seen.is_some(),
        "the keys never became {table}: {:?}",
        client_field(tb, term, "#{client_key_table}")
    );
}

/// Ctrl-a, once the last key is done, then waits for moa's keys. Ctrl-a also clears the last
/// notice, so the mark that follows sees only what the next key draws.
fn ctrl_a(tb: &TempBrain, term: &Terminal) {
    key_table_becomes(tb, term, "root");
    term.press(b"\x01");
    key_table_becomes(tb, term, "moa");
    term.mark();
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
    format!("moa: {}", a.not_yet())
}

fn launches(tb: &TempBrain) -> u64 {
    let s = status_json(tb);
    s["launches"].as_u64().unwrap_or_else(|| panic!("{s}"))
}

/// The idle count in a status text: none shown is 0.
fn idle_count(text: &str) -> usize {
    let mark = format!("{} ", DisplayState::Idle.glyph());
    let Some(at) = text.find(&mark).map(|at| at + mark.len()) else {
        return 0;
    };
    let digits: String = text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

/// Waits (2 s) for `@moa_status` to be `want`.
fn status_becomes(tb: &TempBrain, want: &str) {
    let seen = wait_for(QUICK, || (option(tb, "@moa_status") == want).then_some(()));
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

    for a in KeyAction::ALL {
        moa_key(&tb, &term, a.key(), &notice(a));
    }

    // The menu, then an item in it.
    ctrl_a(&tb, &term);
    term.press(b" ");
    assert!(
        term.wait_for_text(KeyAction::Plan.label(), QUICK),
        "the menu never showed"
    );
    term.mark();
    term.press(KeyAction::Tree.key().as_bytes());
    assert!(
        term.wait_for_text(&notice(KeyAction::Tree), QUICK),
        "the menu's tree item never answered"
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
    let mut state = brain.state();
    let want = counts_text(&state);
    assert!(!want.is_empty());

    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    status_becomes(&tb, &want);

    // The live session the status line counts as idle. (The fixture's `S2_IDLE` reads as a
    // question: its last words ask one.)
    let now = brain.now();
    let idle: Vec<String> = state
        .sessions()
        .filter(|s| display_state(s, now.wall).0 == DisplayState::Idle)
        .map(|s| s.id.to_string())
        .collect();
    let [idle] = idle.as_slice() else {
        panic!("one idle session, not {idle:?}");
    };
    let o = run_exe(&tb, &moactl(&tb), &["archive", idle], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let op = Op::SessionArchive {
        qs: vec![idle.clone()],
    };
    run_op(&mut state, op, &now).expect("the archive applies");
    let after = counts_text(&state);
    assert_eq!(idle_count(&after), idle_count(&want) - 1, "{after:?}");
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
    moa_key(&tb, &term, "c", &notice(KeyAction::Console));

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
    moa_key(&tb, &term, "c", &notice(KeyAction::Console));

    assert_eq!(option(&tb, "@user_opt"), "1");
    let set = hooks(&tb);
    assert!(set.contains("pane-exited[5]"), "{set}");
    assert!(set.contains("pane-exited[90]"), "{set}");

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
