//! The generated config against a real tmux, through the testkit's `Sandbox`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_core::tmux::ids::{ClientName, PaneId};
use moa_core::tmux::mailbox::{HookMsg, KeyAction, Mailbox};
use moa_testkit::{Sandbox, Terminal, tmux_path, wait_for};
use moa_tmux::Notification;
use moa_tmux::client::{ConnectOptions, ControlClient, Event, Events, ServerInfo};
use moa_tmux::conf::{hooks, moa_tmux, tmux_conf, unhooks, write_if_changed};

#[test]
fn the_generated_config_is_stable() {
    insta::assert_snapshot!("moa_tmux_default", moa_tmux("default"));
    insta::assert_snapshot!("moa_tmux_work", moa_tmux("work"));
    insta::assert_snapshot!(
        "tmux_conf",
        tmux_conf(Path::new("/r/.local/share/moa/default/moa.tmux"))
    );
    insta::assert_snapshot!(
        "tmux_conf_odd_path",
        tmux_conf(Path::new("/r/it's here/moa.tmux"))
    );
}

#[test]
fn write_if_changed_writes_once_and_privately() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::without_server().unwrap();
    let path = sb.home().join("moa.tmux");
    assert!(write_if_changed(&path, "a\n").unwrap());
    assert!(!write_if_changed(&path, "a\n").unwrap());
    assert!(write_if_changed(&path, "b\n").unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let names: Vec<_> = std::fs::read_dir(sb.home())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "a temp file was left: {names:?}");
}

fn write_conf(sb: &Sandbox) -> PathBuf {
    let path = sb.home().join("moa.tmux");
    std::fs::write(&path, moa_tmux("default")).unwrap();
    path
}

fn load(sb: &Sandbox) {
    let path = write_conf(sb);
    let out = sb.tmux(&["source-file", &path.to_string_lossy()]).unwrap();
    assert_eq!(out, "");
}

#[test]
fn it_sources_cleanly_on_this_tmux() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    assert_eq!(
        sb.tmux(&["show-options", "-gv", "status-interval"])
            .unwrap(),
        "0"
    );
    let keys = sb.tmux(&["list-keys", "-T", "moa"]).unwrap();
    // The key is the word after `-T moa`, with tmux's quoting taken off.
    let bound: Vec<String> = keys
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            f.by_ref().find(|w| *w == "moa")?;
            f.next().map(|k| k.replace(['\'', '\\'], ""))
        })
        .collect();
    let mut want: Vec<&str> = KeyAction::ALL.iter().map(|a| a.key()).collect();
    want.extend([
        "|", "-", "m", "z", "[", "]", "a", "Left", "Right", "Up", "Down",
    ]);
    want.extend(["h", "j", "k", "l", "Space", "C-a"]);
    for k in want {
        assert!(
            bound.iter().any(|b| b == k),
            "{k} isn't bound in: {bound:?}"
        );
    }
}

fn opts(sb: &Sandbox) -> ConnectOptions {
    ConnectOptions {
        tmux: tmux_path().unwrap_or_else(|e| panic!("{e}")),
        socket: sb.socket_args(),
        env: sb.env(),
    }
}

async fn connect(sb: &Sandbox) -> (ControlClient, Events, ServerInfo) {
    ControlClient::connect(&opts(sb))
        .await
        .unwrap_or_else(|e| panic!("connect failed: {e}"))
}

fn attach(sb: &Sandbox) -> Terminal {
    let tmux = tmux_path().unwrap_or_else(|e| panic!("{e}"));
    let mut args: Vec<std::ffi::OsString> = vec!["-u".into()];
    args.extend(sb.socket_args());
    args.extend(["attach".into(), "-t".into(), "=sandbox".into()]);
    let term = Terminal::spawn(&tmux, &args, &sb.env(), 120, 30).unwrap();
    let tty = term.tty();
    let seen = wait_for(Duration::from_secs(5), || {
        let list = sb.tmux(&["list-clients", "-F", "#{client_name}"]).ok()?;
        list.lines().any(|l| l == tty).then_some(())
    });
    assert!(seen.is_some(), "the terminal never attached");
    term
}

/// The next `moa:` message, parsed.
async fn next_mailbox(events: &mut Events) -> Mailbox {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(ev) = events.recv().await {
            if let Event::Notification(Notification::Message(m)) = ev
                && m.starts_with("moa:")
            {
                return Mailbox::parse(&m).unwrap_or_else(|e| panic!("{m:?}: {e:?}"));
            }
        }
        panic!("the events ended")
    })
    .await
    .expect("no moa message within 5 s")
}

/// The next hook message that `pick` takes, skipping other hooks (a new window also
/// changes a layout).
async fn next_hook<T>(events: &mut Events, pick: impl Fn(HookMsg) -> Option<T>) -> T {
    loop {
        match next_mailbox(events).await {
            Mailbox::Hook(h) => {
                if let Some(v) = pick(h) {
                    return v;
                }
            }
            other => panic!("{other:?}"),
        }
    }
}

/// Waits until `client` is back in the root key table, so the next Ctrl-a starts a fresh key.
fn wait_for_root(sb: &Sandbox, client: &ClientName) {
    let back = wait_for(Duration::from_secs(5), || {
        let list = sb
            .tmux(&["list-clients", "-F", "#{client_name} #{client_key_table}"])
            .ok()?;
        list.lines()
            .any(|l| l == format!("{} root", client.0))
            .then_some(())
    });
    assert!(
        back.is_some(),
        "the client never went back to the root keys"
    );
}

fn active_pane(sb: &Sandbox) -> PaneId {
    let out = sb
        .tmux(&["list-panes", "-t", "=sandbox:", "-F", "#{pane_id}"])
        .unwrap();
    out.parse().unwrap_or_else(|e| panic!("{out:?}: {e}"))
}

async fn with_ctl(sb: &Sandbox) -> (ControlClient, Events, ServerInfo) {
    let (client, events, info) = connect(sb).await;
    sb.tmux(&["set-option", "-g", "@moa_ctl", &info.client.0])
        .unwrap();
    (client, events, info)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_daemon_key_reaches_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let (client, mut events, _) = with_ctl(&sb).await;
    let term = attach(&sb);
    let pane = active_pane(&sb);
    let tty = ClientName(term.tty());
    for action in KeyAction::ALL {
        term.press(b"\x01");
        term.press(action.key().as_bytes());
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action: got,
                pane: p,
                client: c,
                ..
            } => {
                assert_eq!(got, action);
                assert_eq!(p, Some(pane));
                assert_eq!(c, tty);
            }
            other => panic!("{action:?}: {other:?}"),
        }
        wait_for_root(&sb, &tty);
    }
    client.close().await;
}

/// Types `open` and waits until the menu is drawn, so the next key lands in it.
fn open_menu(term: &Terminal, open: &[u8]) {
    term.mark();
    term.press(open);
    assert!(
        term.wait_for_text("Big picture", Duration::from_secs(5)),
        "the menu never showed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_menu_and_its_items_reach_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let (client, mut events, _) = with_ctl(&sb).await;
    let term = attach(&sb);
    let tty = ClientName(term.tty());
    // Both ways in: Ctrl-a Space and a second Ctrl-a.
    for open in [&b"\x01 "[..], &b"\x01\x01"[..]] {
        open_menu(&term, open);
        term.press(b"c");
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action,
                pane,
                client,
                ..
            } => {
                assert_eq!(action, KeyAction::Console);
                assert_eq!(pane, None);
                assert_eq!(client, tty);
            }
            other => panic!("{other:?}"),
        }
    }
    // Every other daemon item. The menu has no item for "find in a full window".
    for action in KeyAction::ALL
        .into_iter()
        .filter(|a| *a != KeyAction::FindOther && *a != KeyAction::Console)
    {
        open_menu(&term, b"\x01 ");
        term.press(action.key().as_bytes());
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action: got, pane, ..
            } => {
                assert_eq!(got, action);
                assert_eq!(pane, None);
            }
            other => panic!("{action:?}: {other:?}"),
        }
    }
    client.close().await;
}

#[test]
fn keys_say_moa_isnt_running_without_a_ctl() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let term = attach(&sb);
    term.mark();
    term.press(b"\x01c");
    assert!(
        term.wait_for_text("moa isn't running: run moa", Duration::from_secs(5)),
        "no notice"
    );
}

fn ctl_option(sb: &Sandbox) -> String {
    sb.tmux(&["show-options", "-gqv", "@moa_ctl"]).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ctl_option_is_cleared_when_the_ctl_dies() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, info) = with_ctl(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }
    assert_eq!(ctl_option(&sb), info.client.0);
    let killed = std::process::Command::new("kill")
        .args(["-9", &client.child_pid().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let cleared = wait_for(Duration::from_secs(2), || {
        ctl_option(&sb).is_empty().then_some(())
    });
    assert!(cleared.is_some(), "@moa_ctl is still {:?}", ctl_option(&sb));
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hooks_at_other_indexes_survive() {
    let sb = Sandbox::start().unwrap();
    sb.tmux(&["set-hook", "-g", "pane-exited[5]", "set-option -g @user 1"])
        .unwrap();
    let (client, _events, info) = connect(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }
    // Session hooks and window hooks are listed apart.
    let shown = |sb: &Sandbox| {
        let s = sb.tmux(&["show-hooks", "-g"]).unwrap();
        s + "\n" + &sb.tmux(&["show-hooks", "-gw"]).unwrap()
    };
    let before = shown(&sb);
    assert!(before.contains("pane-exited[90]"), "{before}");
    assert!(before.contains("client-detached[90]"), "{before}");
    for c in unhooks() {
        client.run(&c).await.unwrap();
    }
    let shown = shown(&sb);
    assert!(shown.contains("pane-exited[5]"), "{shown}");
    assert!(!shown.contains("[90]"), "{shown}");
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_hook_reaches_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, info) = connect(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }

    // A window whose command exits.
    sb.tmux(&["new-window", "-d", "-t", "=sandbox:", "exit 0"])
        .unwrap();
    next_hook(&mut events, |h| {
        matches!(h, HookMsg::PaneExited(_)).then_some(())
    })
    .await;

    // The same, kept on screen.
    sb.tmux(&["set-option", "-g", "remain-on-exit", "on"])
        .unwrap();
    sb.tmux(&["new-window", "-d", "-t", "=sandbox:", "exit 0"])
        .unwrap();
    next_hook(&mut events, |h| {
        matches!(h, HookMsg::PaneDied(_)).then_some(())
    })
    .await;

    // A split changes the layout.
    sb.tmux(&["split-window", "-d", "-t", "=sandbox:"]).unwrap();
    next_hook(&mut events, |h| {
        matches!(h, HookMsg::Layout(_)).then_some(())
    })
    .await;

    // A terminal attaching.
    let term = attach(&sb);
    let c = next_hook(&mut events, |h| match h {
        HookMsg::ClientAttached(c) => Some(c),
        _ => None,
    })
    .await;
    assert_eq!(c.0, term.tty());
    client.close().await;
}

#[test]
fn menu_items_say_moa_isnt_running_without_a_ctl() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let term = attach(&sb);
    open_menu(&term, b"\x01 ");
    term.mark();
    term.press(b"c");
    assert!(
        term.wait_for_text("moa isn't running: run moa", Duration::from_secs(5)),
        "no notice"
    );
}

#[test]
fn the_hook_commands_are_allowed_ones() {
    let ctl = ClientName("client-9".into());
    for c in hooks(&ctl).into_iter().chain(unhooks()) {
        assert_eq!(c.check(), Ok(()), "{}", c.line());
        assert!(!c.line().contains('\n'));
    }
}
