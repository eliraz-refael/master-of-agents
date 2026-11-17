//! A TempBrain's terminal server.

use std::time::Duration;

use moa_testkit::{TempBrain, wait_for};

/// tmux runs a one-string pane command through `default-shell -c`, and takes that shell
/// from `SHELL`, else from the user's passwd entry. A build user's entry may be
/// `/sbin/nologin` (nix on macOS), which would kill every pane and with them the server.
/// `TempBrain::tmux` sets `SHELL`, so the server's shell never depends on who runs the test.
#[test]
fn a_temp_brain_server_runs_its_panes_with_sh() {
    let tb = TempBrain::new(None);
    tb.tmux(
        "default",
        &["-f", "/dev/null", "new-session", "-d", "-s", "t", "/bin/sh"],
    )
    .unwrap_or_else(|e| panic!("the server starts: {e}"));
    let shell = tb
        .tmux("default", &["show-options", "-gv", "default-shell"])
        .unwrap();
    assert_eq!(shell, "/bin/sh");
    // The one-string command still runs a while later.
    std::thread::sleep(Duration::from_millis(300));
    let alive = wait_for(Duration::from_secs(1), || {
        let dead = tb
            .tmux(
                "default",
                &["list-panes", "-t", "=t:", "-F", "#{pane_dead}"],
            )
            .ok()?;
        (dead == "0").then_some(())
    });
    assert!(alive.is_some(), "the pane died");
}
