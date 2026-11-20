//! The generated config against a real tmux, through the testkit's `Sandbox`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_core::keymap::ctrl_a;
use moa_core::tmux::ids::{ClientName, PaneId};
use moa_core::tmux::mailbox::{HookMsg, KeyAction, Mailbox};
use moa_testkit::{Sandbox, Terminal, tmux_path, wait_for};
use moa_tmux::Notification;
use moa_tmux::client::{CONNECT_WAIT, ConnectOptions, ControlClient, Event, Events, ServerInfo};
use moa_tmux::conf::{hooks, moa_tmux, tmux_conf, unhooks};

#[test]
fn the_generated_config_is_stable() {
    insta::assert_snapshot!("moa_tmux_default", moa_tmux("default", true));
    insta::assert_snapshot!("moa_tmux_work", moa_tmux("work", true));
    insta::assert_snapshot!(
        "tmux_conf",
        tmux_conf(Path::new("/r/.local/share/moa/default/moa.tmux"))
    );
    insta::assert_snapshot!(
        "tmux_conf_odd_path",
        tmux_conf(Path::new("/r/it's here/moa.tmux"))
    );
}

/// A failed agent's tab shows a red cross in the current-window pill.
#[test]
fn the_current_window_pill_shows_failed() {
    assert!(
        moa_tmux("default", true).contains("#{?#{==:#{@moa_state},failed},#[fg=#d57780]✗ ,"),
        "the pill has no branch for failed"
    );
    // And tmux reads the branch as meant: the pill renders the cross.
    let sb = Sandbox::start().unwrap();
    load(&sb);
    sb.tmux(&["set-option", "-w", "@moa_state", "failed"])
        .unwrap();
    let left = sb
        .tmux(&["display-message", "-p", "#{T:status-left}"])
        .unwrap();
    assert!(left.contains("✗ "), "{left}");
}

/// V11: moa's file turns the mouse on, so tmux hands a panel its clicks, motion and wheel;
/// with `[ui] mouse = false` it says nothing about the mouse, leaving the user's own setting.
#[test]
fn moa_tmux_turns_the_mouse_on_unless_told_not_to() {
    let on = moa_tmux("default", true);
    let lines: Vec<&str> = on.lines().filter(|l| l.contains("mouse")).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("set -g  mouse on"), "{lines:?}");
    let off = moa_tmux("default", false);
    assert!(
        !off.lines().any(|l| l.contains("mouse")),
        "no mouse line at all"
    );
    // And tmux takes it.
    let sb = Sandbox::start().unwrap();
    load(&sb);
    assert_eq!(sb.tmux(&["show-options", "-gv", "mouse"]).unwrap(), "on");
}

/// Sources `tmux_conf` on a server whose home holds the user's three possible config files,
/// each appending its name to `@order`, with `XDG_CONFIG_HOME` as given (a path under the
/// home, or none).
fn order_with_xdg(xdg: Option<&str>) -> String {
    let sb = Sandbox::start().unwrap();
    let home = sb.home().to_path_buf();
    for (file, name) in [
        (".tmux.conf", "home"),
        (".config/tmux/tmux.conf", "dotconfig"),
        ("xdg/tmux/tmux.conf", "xdg"),
    ] {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("set -ag @order '{name} '\n")).unwrap();
    }
    let moa = home.join("moa.tmux");
    std::fs::write(&moa, "set -ag @order 'moa '\n").unwrap();
    let conf = home.join("tmux.conf");
    std::fs::write(&conf, tmux_conf(&moa)).unwrap();
    if let Some(x) = xdg {
        let x = home.join(x).to_string_lossy().into_owned();
        sb.tmux(&["set-environment", "-g", "XDG_CONFIG_HOME", &x])
            .unwrap();
    }
    sb.tmux(&["source-file", &conf.to_string_lossy()]).unwrap();
    sb.tmux(&["show-options", "-gqv", "@order"]).unwrap()
}

#[test]
fn tmux_conf_loads_the_users_files_as_tmux_would_then_moas() {
    assert_eq!(order_with_xdg(None).trim(), "home dotconfig moa");
    assert_eq!(order_with_xdg(Some("xdg")).trim(), "home xdg dotconfig moa");
    // Named twice, loaded once, as tmux does.
    assert_eq!(order_with_xdg(Some(".config")).trim(), "home dotconfig moa");
}

fn write_conf(sb: &Sandbox) -> PathBuf {
    let path = sb.home().join("moa.tmux");
    std::fs::write(&path, moa_tmux("default", true)).unwrap();
    path
}

fn load(sb: &Sandbox) {
    let path = write_conf(sb);
    let out = sb.tmux(&["source-file", &path.to_string_lossy()]).unwrap();
    assert_eq!(out, "");
}

/// The keys bound in the `moa` table, from tmux's own listing: the word after `-T moa`, with
/// tmux's quoting taken off.
fn moa_keys(sb: &Sandbox) -> Vec<String> {
    let keys = sb.tmux(&["list-keys", "-T", "moa"]).unwrap();
    keys.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            f.by_ref().find(|w| *w == "moa")?;
            f.next().map(|k| k.replace(['\'', '\\'], ""))
        })
        .collect()
}

/// The key table and the generated file agree: every key the table names is bound. Read
/// from tmux's listing, not the file's text: the template spells `[` bare, `quote` gives
/// `'['`.
#[test]
fn every_table_key_is_bound() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let bound = moa_keys(&sb);
    let table: Vec<&str> = ctrl_a()
        .into_iter()
        .flatten()
        .map(|b| b.key)
        .filter(|k| !k.is_empty())
        .collect();
    assert!(!table.is_empty());
    for k in table {
        assert!(
            bound.iter().any(|b| b == k),
            "{k} isn't bound in: {bound:?}"
        );
    }
}

/// Each key the status line's hint names (`/ find · c console · …`) is a key in the table,
/// or `Space` (the menu) or the arrows.
#[test]
fn the_status_hint_names_only_table_keys() {
    let template = include_str!("../src/moa.tmux.tpl");
    let left = template
        .lines()
        .find(|l| l.starts_with("set -g  status-left "))
        .expect("no status-left line");
    // The hint is the one styled run whose text lists keys: `key what · key what · …`.
    let hints: Vec<&str> = left
        .split("#[")
        .filter_map(|run| run.split_once(']').map(|(_, text)| text.trim()))
        .filter(|text| text.contains(" · "))
        .collect();
    assert_eq!(hints.len(), 1, "{hints:?}");
    let named: Vec<&str> = hints[0]
        .split(" · ")
        .map(|item| item.split_whitespace().next().unwrap_or(""))
        .collect();
    assert!(named.len() >= 9, "{named:?}");
    let table: Vec<&str> = ctrl_a().into_iter().flatten().map(|b| b.key).collect();
    for k in named {
        assert!(
            k == "Space" || k == "←↑↓→" || table.contains(&k),
            "the hint names {k:?}, which isn't in the table"
        );
    }
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
    let bound = moa_keys(&sb);
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
    // The prefix table's own bindings.
    let prefix = sb.tmux(&["list-keys", "-T", "prefix"]).unwrap();
    for k in ["Space", "m", "C"] {
        let found = prefix.lines().any(|l| {
            let mut f = l.split_whitespace();
            f.by_ref().find(|w| *w == "prefix").is_some() && f.next() == Some(k)
        });
        assert!(found, "prefix {k} isn't bound");
    }
}

fn opts(sb: &Sandbox) -> ConnectOptions {
    ConnectOptions {
        tmux: tmux_path().unwrap_or_else(|e| panic!("{e}")),
        socket: sb.socket_args(),
        env: sb.env(),
        wait: CONNECT_WAIT,
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
    // Listed is not drawn: keys typed before the first draw can be lost. The status line
    // is moa's when its config is loaded, else tmux's own, which starts with `[sandbox]`.
    let left = sb.tmux(&["show-options", "-gqv", "status-left"]).unwrap();
    let drawn = if left.contains("◆ moa") {
        "◆ moa"
    } else {
        "[sandbox]"
    };
    assert!(
        term.wait_for_text(drawn, Duration::from_secs(5)),
        "the status line never drew; the terminal showed: {}",
        term.tail(600)
    );
    term
}

/// The next `moa:` message, parsed, with no time limit.
async fn recv_mailbox(events: &mut Events) -> Mailbox {
    while let Some(ev) = events.recv().await {
        if let Event::Notification(Notification::Message(m)) = ev
            && m.starts_with("moa:")
        {
            return Mailbox::parse(&m).unwrap_or_else(|e| panic!("{m:?}: {e:?}"));
        }
    }
    panic!("the events ended")
}

/// The next `moa:` message, parsed.
async fn next_mailbox(events: &mut Events) -> Mailbox {
    tokio::time::timeout(Duration::from_secs(5), recv_mailbox(events))
        .await
        .expect("no moa message within 5 s")
}

/// The next hook message that `pick` takes, skipping other hooks (a new window also
/// changes a layout). `what` names the hook a failure waited for.
async fn next_hook<T>(events: &mut Events, what: &str, pick: impl Fn(HookMsg) -> Option<T>) -> T {
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match recv_mailbox(events).await {
                Mailbox::Hook(h) => {
                    if let Some(v) = pick(h) {
                        return v;
                    }
                }
                other => panic!("waiting for {what}: {other:?}"),
            }
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("no {what} hook within 5 s"))
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
        "the menu never showed; the terminal showed: {}",
        term.tail(600)
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
    // Every other daemon item, `s` (find, full tab) among them.
    for action in KeyAction::ALL
        .into_iter()
        .filter(|a| *a != KeyAction::Console)
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
        "no notice; the terminal showed: {}",
        term.tail(600)
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

/// A new window, in the background, whose command exits at once; its pane.
fn new_window(sb: &Sandbox) -> PaneId {
    let out = sb
        .tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "=sandbox:",
            "exit 0",
        ])
        .unwrap();
    out.parse().unwrap_or_else(|e| panic!("{out:?}: {e}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_hook_reaches_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, info) = connect(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }

    // A window whose command exits.
    let pane = new_window(&sb);
    next_hook(&mut events, "pane-exited", |h| {
        (h == HookMsg::PaneExited(pane)).then_some(())
    })
    .await;

    // The same, kept on screen. tmux 3.4 and 3.5 built with utempter (Linux distro
    // packages) can lose this pane's SIGCHLD: when the pty closes before the child is
    // reaped, tmux closes the pane and runs the utmp helper with SIGCHLD set to SIG_DFL,
    // and a SIGCHLD that lands then is dropped (3.6 raises it again after the helper).
    // The pane is then dead with no exit status, so no pane-died until tmux next reaps a
    // child. Once the pane is dead, a run-shell job makes it reap.
    sb.tmux(&["set-option", "-g", "remain-on-exit", "on"])
        .unwrap();
    let pane = new_window(&sb);
    let target = pane.to_string();
    let dead = wait_for(Duration::from_secs(5), || {
        let d = sb
            .tmux(&["display-message", "-p", "-t", &target, "#{pane_dead}"])
            .ok()?;
        (d == "1").then_some(())
    });
    assert!(dead.is_some(), "{pane} never died");
    sb.tmux(&["run-shell", "true"]).unwrap();
    next_hook(&mut events, "pane-died", |h| {
        (h == HookMsg::PaneDied(pane)).then_some(())
    })
    .await;

    // A split changes the layout.
    sb.tmux(&["split-window", "-d", "-t", "=sandbox:"]).unwrap();
    next_hook(&mut events, "layout", |h| {
        matches!(h, HookMsg::Layout(_)).then_some(())
    })
    .await;

    // A terminal attaching.
    let term = attach(&sb);
    let c = next_hook(&mut events, "client-attached", |h| match h {
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
        "no notice; the terminal showed: {}",
        term.tail(600)
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
