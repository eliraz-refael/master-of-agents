//! The front door against real tmux: `moa` creates a sandbox's terminal server, links its
//! daemon and attaches; a second `moa` joins it; inside its own server `moa` switches
//! instead of nesting. Every tmux query goes through `TempBrain::tmux` (T23), and `moa`
//! runs on a `Terminal`, as a user's terminal would run it.

mod common;

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use moa_testkit::brain::TYPICAL_TZ;
use moa_testkit::{TempBrain, Terminal, wait_for};

use common::*;

fn tmux_socket(tb: &TempBrain) -> std::path::PathBuf {
    tb.paths().tmux_socket(B).unwrap().path(None, 0)
}

#[test]
fn moa_starts_the_server_and_attaches() {
    let tb = TempBrain::new(None);
    // Variables the server must not keep, so the check below can fail.
    let term = open(
        &tb,
        &[],
        &[
            ("MOA_SESSION", "x"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CONFIG_DIR", "/x"),
        ],
    );
    assert!(term.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);
    let env = tmux(&tb, &["show-environment", "-g"]);
    let root = tb.root().display().to_string();
    assert!(
        env.lines().any(|l| l == format!("MOA_ROOT={root}")),
        "{env}"
    );
    // (`TMUX` can't be in the terminal's env here; `front_env`'s unit test covers it.)
    for gone in ["MOA_SESSION=", "CLAUDECODE=", "CLAUDE_CONFIG_DIR="] {
        assert!(!env.lines().any(|l| l.starts_with(gone)), "{gone} in {env}");
    }
    assert!(
        env.lines().any(|l| l == "SHELL=/bin/sh"),
        "the shell reaches the server: {env}"
    );
    let status = status_json(&tb);
    assert!(status["tmux"].is_object(), "{status}");
}

#[test]
fn a_second_moa_attaches_to_the_same_server() {
    let tb = TempBrain::new(None);
    let first = open(&tb, &[], &[]);
    assert!(first.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);
    let pid = server_pid(&tb);
    let second = open(&tb, &[], &[]);
    assert!(second.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa", "moa"]);
    assert_eq!(server_pid(&tb), pid);
    assert_eq!(
        status_json(&tb)["tmux"]["pid"].to_string(),
        pid,
        "the daemon is linked to that server"
    );
}

#[test]
fn moa_inside_its_own_server_switches_instead_of_nesting() {
    let tb = TempBrain::new(None);
    let term = open(&tb, &[], &[]);
    assert!(term.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);
    // Move the terminal to a shell in another session, so the switch back can be seen.
    let root = tb.root().display().to_string();
    tmux(&tb, &["new-session", "-d", "-s", "other", "-c", &root]);
    tmux(&tb, &["switch-client", "-c", &term.tty(), "-t", "=other"]);
    clients_become(&tb, &["other"]);
    term.press(format!("'{MOA}'\r").as_bytes());
    clients_become(&tb, &["moa"]);
    // Still one terminal: no client was started inside a shell.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(user_clients(&tb), ["moa"]);

    // With the `moa` session gone, `moa` there makes it, then switches to it.
    tmux(&tb, &["switch-client", "-c", &term.tty(), "-t", "=other"]);
    clients_become(&tb, &["other"]);
    tmux(&tb, &["kill-session", "-t", "=moa"]);
    term.press(format!("'{MOA}'\r").as_bytes());
    clients_become(&tb, &["moa"]);
    let home = std::fs::canonicalize(tb.root()).unwrap();
    let path = tmux(
        &tb,
        &[
            "display-message",
            "-p",
            "-t",
            "=moa:",
            "#{pane_current_path}",
        ],
    );
    assert_eq!(Path::new(&path), home);
}

/// The `moa` session gone (its last shell exited) while `_moa` keeps the server: the next
/// `moa` makes it again, from the user's terminal, so its first shell has the user's
/// variables and starts in the home folder.
#[test]
fn moa_makes_a_missing_moa_session_in_the_users_environment() {
    let tb = TempBrain::new(None);
    let first = open(&tb, &[], &[("SSH_AUTH_SOCK", "/tmp/first.sock")]);
    assert!(first.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);
    tmux(&tb, &["kill-session", "-t", "=moa"]);
    clients_become(&tb, &[]);

    let term = open(&tb, &[], &[("SSH_AUTH_SOCK", "/tmp/second.sock")]);
    assert!(term.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);
    let env = tmux(&tb, &["show-environment", "-t", "=moa"]);
    assert!(
        env.lines().any(|l| l == "SSH_AUTH_SOCK=/tmp/second.sock"),
        "{env}"
    );
    // The first shell itself has it, not only the sessions made after an attach.
    let seen = tb.root().join("seen");
    term.press(format!("echo \"$SSH_AUTH_SOCK\" > '{}'\r", seen.display()).as_bytes());
    let said = wait_for(SHOWS, || {
        std::fs::read_to_string(&seen)
            .ok()
            .filter(|t| t.ends_with('\n'))
    });
    assert_eq!(said.as_deref(), Some("/tmp/second.sock\n"));
    let home = std::fs::canonicalize(tb.root()).unwrap();
    let path = tmux(
        &tb,
        &[
            "display-message",
            "-p",
            "-t",
            "=moa:",
            "#{pane_current_path}",
        ],
    );
    assert_eq!(Path::new(&path), home);
}

#[test]
fn moa_inside_another_tmux_refuses() {
    let tb = TempBrain::new(None);
    let o = run(&tb, &[], &[("TMUX", "/tmp/elsewhere/default,1,0")]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        out(&o),
        "the default moa is running; open it from a new terminal tab: moa\n"
    );
    assert!(!tmux_socket(&tb).exists());
    assert!(!tb.paths().socket(B).unwrap().exists());

    // A brain other than the default is named in the line.
    let tb = TempBrain::new(Some("[brains.work]\n"));
    let o = run(&tb, &["work"], &[("TMUX", "/tmp/elsewhere/default,1,0")]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        out(&o),
        "the work moa is running; open it from a new terminal tab: moa work\n"
    );

    // `--sandbox` points to the sandbox, not to a plain `moa` that refuses.
    let tb = TempBrain::sandbox_home();
    let o = assert_cmd::Command::new(MOA)
        .arg("--sandbox")
        .env_clear()
        .env("HOME", tb.home())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMUX", "/tmp/elsewhere/default,1,0")
        .current_dir(tb.home())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        out(&o),
        "the default moa is running; open it from a new terminal tab: moa --sandbox\n"
    );
    assert!(!tmux_socket(&tb).exists());
}

#[test]
fn moa_sandbox_opens_a_moa_under_home() {
    // The root is `<home>/.local/state/moa-sandbox`; dropping `tb` stops what runs there.
    let tb = TempBrain::sandbox_home();
    // TZ as `TempBrain::env` gives it: the daemon needs a zone, and a build sandbox has no
    // /etc/localtime.
    let env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), tb.home().into()),
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("SHELL".into(), "/bin/sh".into()),
        ("TZ".into(), TYPICAL_TZ.into()),
    ];
    let args = [OsString::from("--sandbox")];
    let term = Terminal::spawn(Path::new(MOA), &args, &env, 120, 30).unwrap();
    assert!(term.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    clients_become(&tb, &["moa"]);

    let root = tb.home().join(".local/state/moa-sandbox");
    assert_eq!(tb.root(), root);
    let meta = std::fs::metadata(&root).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    // The daemon and the server are the sandbox's, and its shells reach it.
    assert!(tb.paths().socket(B).unwrap().exists());
    let env = tmux(&tb, &["show-environment", "-g"]);
    let want = format!("MOA_ROOT={}", root.display());
    assert!(env.lines().any(|l| l == want), "{env}");
    let home = format!("HOME={}", tb.home().display());
    assert!(env.lines().any(|l| l == home), "{env}");
    assert!(status_json(&tb)["tmux"].is_object());
}

#[test]
fn an_old_tmux_is_refused_before_a_server_is_made() {
    let tb = TempBrain::new(None);
    // Not tmux: a script that says it's 3.2a and fails anything else.
    let bin = tb.root().join("old-bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("tmux");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = -V ] && { echo 'tmux 3.2a'; exit 0; }\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = run(&tb, &[], &[("PATH", &path)]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert_eq!(err(&o), "moa needs tmux 3.4 or newer; this one is 3.2a\n");
    assert!(!tmux_socket(&tb).exists());
}

#[test]
fn moa_names_a_missing_brain() {
    let tb = TempBrain::new(None);
    let o = run(&tb, &["ghost"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        format!(
            "there's no brain named ghost in {}\n",
            tb.paths().config_file().display()
        )
    );
    let o = run(&tb, &["Bad"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with("\"Bad\" isn't a brain name (use "),
        "{}",
        err(&o)
    );
    assert!(!tmux_socket(&tb).exists());
    assert!(!tb.paths().socket(B).unwrap().exists());
}

#[test]
fn status_says_whether_the_terminal_is_connected() {
    let tb = TempBrain::new(None);
    // A verb starts the daemon without a terminal server.
    let o = run(&tb, &["ls"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let o = run(&tb, &["status"], &[]);
    assert!(
        out(&o).contains("\nterminal: not connected\n"),
        "{}",
        out(&o)
    );
    assert!(status_json(&tb)["tmux"].is_null());

    let term = open(&tb, &[], &[]);
    assert!(term.wait_for_text(STATUS_LINE, SHOWS), "no status line");
    let o = run(&tb, &["status"], &[]);
    assert!(out(&o).contains("\nterminal: connected\n"), "{}", out(&o));
    let tmux = &status_json(&tb)["tmux"];
    assert_eq!(tmux["pid"].to_string(), server_pid(&tb));
    assert!(tmux["version"].as_str().is_some_and(|v| !v.is_empty()));
    // The plain line names no tmux version or pid.
    assert!(!out(&o).contains("tmux"), "{}", out(&o));
}
