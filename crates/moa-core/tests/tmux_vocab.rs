use std::collections::BTreeSet;

use moa_core::model::SessionId;
use moa_core::tmux::cmd::{ALLOWED, Cmd, quote};
use moa_core::tmux::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use moa_core::tmux::mailbox::{HookMsg, KeyAction, Mailbox, MailboxError};
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::{Applied, Topology, refresh_cmds};
use proptest::prelude::*;

#[test]
fn ids_print_and_parse_like_tmux() {
    assert_eq!("%3".parse::<PaneId>(), Ok(PaneId(3)));
    assert!("@3".parse::<PaneId>().is_err());
    assert!("%".parse::<PaneId>().is_err());
    assert!("%x".parse::<PaneId>().is_err());
    assert_eq!("@2".parse::<WindowId>(), Ok(WindowId(2)));
    assert_eq!("$0".parse::<TmuxSessionId>(), Ok(TmuxSessionId(0)));
    assert_eq!(TmuxSessionId(0).to_string(), "$0");
    assert_eq!(PaneId(3).to_string(), "%3");
    assert_eq!(WindowId(2).to_string(), "@2");
}

#[test]
fn quote_leaves_safe_words_bare_and_quotes_the_rest() {
    assert_eq!(quote("=moa:moa"), "=moa:moa");
    assert_eq!(quote("it's"), r"'it'\''s'");
    assert_eq!(quote("a\nb"), r#"'a'"\n"'b'"#);
    assert_eq!(quote("a\rb"), r#"'a'"\r"'b'"#);
    assert_eq!(quote("\n"), r#""\n""#);
    assert_eq!(quote(""), "''");
    assert_eq!(quote("~/x"), "'~/x'");
    assert_eq!(quote("#{pane_id}"), "'#{pane_id}'");
    assert_eq!(quote("a b"), "'a b'");
}

proptest! {
    #[test]
    fn a_line_never_holds_a_newline(args in proptest::collection::vec(any::<String>(), 0..5)) {
        let line = Cmd::new("display-message").args(args).line();
        prop_assert!(!line.contains('\n'));
        prop_assert!(!line.contains('\r'));
    }
}

#[test]
fn the_allow_list_refuses_waiting_and_unknown_commands() {
    for name in [
        "wait-for",
        "run-shell",
        "display-popup",
        "confirm-before",
        "if-shell",
        "set",
        "",
    ] {
        assert!(Cmd::new(name).check().is_err(), "{name}");
    }
    assert_eq!(
        Cmd::new("run-shell").check().unwrap_err().0,
        "that isn't a command moa sends"
    );
    for name in ALLOWED {
        if *name != "command-prompt" && *name != "source-file" {
            assert!(Cmd::new(name).check().is_ok(), "{name}");
        }
    }
    let prompt = Cmd::new("command-prompt");
    assert_eq!(
        prompt.clone().arg("x").check().unwrap_err().0,
        "command-prompt needs -b"
    );
    assert!(prompt.arg("-b").arg("x").check().is_ok());
    let source = |p: &str| Cmd::new("source-file").arg(p).check();
    assert_eq!(
        source("/x/tmux.conf").unwrap_err().0,
        "moa only sources its own config"
    );
    assert!(source("/x/notmoa.tmux").is_err());
    assert!(Cmd::new("source-file").check().is_err());
    assert!(source("/x/moa.tmux").is_ok());
}

#[test]
fn source_file_checks_every_path() {
    let source = |args: &[&str]| Cmd::new("source-file").args(args.iter().copied()).check();
    assert!(source(&["/x/moa.tmux"]).is_ok());
    assert!(source(&["-q", "/x/moa.tmux"]).is_ok());
    assert!(source(&["/evil.conf", "/x/moa.tmux"]).is_err());
    assert!(source(&["/x/*/moa.tmux"]).is_err());
    assert!(source(&["/x/[a]/moa.tmux"]).is_err());
    assert!(source(&["/x/?/moa.tmux"]).is_err());
    assert!(source(&["moa.tmux"]).is_err());
    assert!(source(&["-q"]).is_err());
}

#[test]
fn a_pane_change_for_a_pane_outside_the_window_refreshes() {
    let mut t = lists(false);
    assert_eq!(
        t.apply(&Notification::WindowPaneChanged {
            window: WindowId(2),
            pane: PaneId(0)
        }),
        Applied::Refresh
    );
    assert_eq!(
        t.client_pane(&ClientName("/dev/ttys1".into())),
        Some(PaneId(3))
    );
}

#[test]
fn prompt_lines_without_text_or_with_a_signed_token() {
    assert_eq!(
        Mailbox::parse("moa:prompt 8"),
        Ok(Mailbox::Prompt {
            token: 8,
            text: String::new()
        })
    );
    assert_eq!(Mailbox::parse("moa:prompt +7 x"), Err(MailboxError::Bad));
    assert_eq!(Mailbox::parse("moa:prompt  x"), Err(MailboxError::Bad));
}

#[test]
fn a_command_line_is_the_name_and_quoted_arguments() {
    let c = Cmd::new("display-message").arg("-p").arg("#{pane_id}");
    assert_eq!(c.line(), "display-message -p '#{pane_id}'");
    assert_eq!(c.name(), "display-message");
    assert_eq!(c.argv(), ["-p", "#{pane_id}"]);
}

#[test]
fn mailbox_lines_parse() {
    assert_eq!(
        Mailbox::parse("moa:key console %3 $0 /dev/ttys1"),
        Ok(Mailbox::Key {
            action: KeyAction::Console,
            pane: Some(PaneId(3)),
            session: TmuxSessionId(0),
            client: ClientName("/dev/ttys1".into()),
        })
    );
    assert_eq!(
        Mailbox::parse("moa:key layout-2 - $1 client 7"),
        Ok(Mailbox::Key {
            action: KeyAction::Layout(2),
            pane: None,
            session: TmuxSessionId(1),
            client: ClientName("client 7".into()),
        })
    );
    assert_eq!(
        Mailbox::parse("moa:hook pane-died %4"),
        Ok(Mailbox::Hook(HookMsg::PaneDied(PaneId(4))))
    );
    assert_eq!(
        Mailbox::parse("moa:hook pane-exited %5"),
        Ok(Mailbox::Hook(HookMsg::PaneExited(PaneId(5))))
    );
    assert_eq!(
        Mailbox::parse("moa:hook layout @6"),
        Ok(Mailbox::Hook(HookMsg::Layout(WindowId(6))))
    );
    assert_eq!(
        Mailbox::parse("moa:hook client-attached client 9"),
        Ok(Mailbox::Hook(HookMsg::ClientAttached(ClientName(
            "client 9".into()
        ))))
    );
    assert_eq!(
        Mailbox::parse(r#"moa:prompt 7 it's "a" \$HOME"#),
        Ok(Mailbox::Prompt {
            token: 7,
            text: r#"it's "a" $HOME"#.into()
        })
    );
    assert_eq!(
        Mailbox::parse("moa:prompt 8 "),
        Ok(Mailbox::Prompt {
            token: 8,
            text: String::new()
        })
    );
    assert_eq!(Mailbox::parse("hello"), Err(MailboxError::NotMoa));
    for bad in [
        "moa:key nope %1 $0 c",
        "moa:key console %1 $0",
        "moa:key console x $0 c",
        "moa:key console %1 0 c",
        "moa:hook pane-died @4",
        "moa:hook nope %4",
        "moa:prompt x text",
        "moa:wat",
        "moa:",
    ] {
        assert_eq!(Mailbox::parse(bad), Err(MailboxError::Bad), "{bad}");
    }
}

#[test]
fn every_action_has_a_word_key_label_and_notice() {
    let mut keys = BTreeSet::new();
    for a in KeyAction::ALL {
        assert_eq!(KeyAction::from_word(&a.word()), Some(a));
        assert!(keys.insert(a.key()), "{a:?}");
        assert!(!a.label().is_empty());
        for word in ["tmux", "pane", "window"] {
            assert!(!a.not_yet().contains(word), "{a:?}: {}", a.not_yet());
        }
    }
    assert_eq!(KeyAction::ALL.len(), 14);
    assert_eq!(KeyAction::from_word("layout-5"), None);
    assert_eq!(KeyAction::from_word("layout-0"), None);
    assert_eq!(KeyAction::Layout(2).label(), "Layout: focus (agent)");
    assert_eq!(KeyAction::FindOther.not_yet(), KeyAction::Find.not_yet());
}

const MOA_SESSION: &str = "0b9e6c6e-2a43-4a7e-9d51-6e3f0d3c8a11";

fn lists(zoomed: bool) -> Topology {
    let z = u8::from(zoomed);
    let sessions = vec!["S $0 work".to_string(), "S $1 ctl".to_string()];
    let panes = vec![
        format!("P %0 @0 $0 1 0 0 {MOA_SESSION}"),
        format!("P %2 @2 $0 0 1 {z} "),
        format!("P %3 @2 $0 1 1 {z} "),
        "P garbage".to_string(),
    ];
    let clients = vec![
        "C 0 $0 /dev/ttys1".to_string(),
        "C 1 $1 control-client".to_string(),
        "nonsense".to_string(),
    ];
    Topology::from_lists(&sessions, &panes, &clients)
}

fn set(ids: &[u32]) -> BTreeSet<PaneId> {
    ids.iter().map(|n| PaneId(*n)).collect()
}

#[test]
fn topology_from_lists_and_visible() {
    let t = lists(false);
    assert_eq!(t.visible(), set(&[2, 3]));
    assert_eq!(lists(true).visible(), set(&[3]));
    assert!(t.users_attached());
    assert_eq!(t.session_named("work"), Some(TmuxSessionId(0)));
    assert_eq!(t.session_named("nope"), None);
    assert!(t.has_pane(PaneId(0)));
    assert!(!t.has_pane(PaneId(9)));
    assert_eq!(t.window_of(PaneId(3)), Some(WindowId(2)));
    assert_eq!(t.window_of(PaneId(9)), None);
    assert_eq!(
        t.client_pane(&ClientName("/dev/ttys1".into())),
        Some(PaneId(3))
    );
    assert_eq!(t.client_pane(&ClientName("control-client".into())), None);
    assert_eq!(
        t.moa_session(PaneId(0)),
        Some(MOA_SESSION.parse::<SessionId>().unwrap())
    );
    assert_eq!(t.moa_session(PaneId(2)), None);

    let none = Topology::from_lists(
        &["S $0 work".to_string()],
        &["P %2 @2 $0 1 1 0 ".to_string()],
        &["C 1 $0 control-client".to_string()],
    );
    assert!(none.visible().is_empty());
    assert!(!none.users_attached());
}

#[test]
fn topology_apply() {
    let mut t = lists(false);
    let user = ClientName("/dev/ttys1".into());
    assert_eq!(
        t.apply(&Notification::SessionWindowChanged {
            session: TmuxSessionId(0),
            window: WindowId(0)
        }),
        Applied::Changed
    );
    assert_eq!(t.visible(), set(&[0]));
    assert_eq!(
        t.apply(&Notification::SessionWindowChanged {
            session: TmuxSessionId(0),
            window: WindowId(0)
        }),
        Applied::Same
    );
    assert_eq!(
        t.apply(&Notification::WindowPaneChanged {
            window: WindowId(2),
            pane: PaneId(2)
        }),
        Applied::Changed
    );
    assert_eq!(t.client_pane(&user), Some(PaneId(0)));
    assert_eq!(
        t.apply(&Notification::ClientSessionChanged {
            client: user.clone(),
            session: TmuxSessionId(1)
        }),
        Applied::Changed
    );
    assert!(t.visible().is_empty());
    assert_eq!(
        t.apply(&Notification::ClientSessionChanged {
            client: ClientName("stranger".into()),
            session: TmuxSessionId(0)
        }),
        Applied::Refresh
    );
    assert_eq!(
        t.apply(&Notification::ClientDetached {
            client: user.clone()
        }),
        Applied::Changed
    );
    assert!(!t.users_attached());
    assert_eq!(
        t.apply(&Notification::ClientDetached { client: user }),
        Applied::Same
    );
    for n in [
        Notification::WindowAdd {
            window: WindowId(5),
        },
        Notification::WindowClose {
            window: WindowId(5),
        },
        Notification::SessionsChanged,
        Notification::LayoutChange {
            window: WindowId(2),
        },
    ] {
        assert_eq!(t.apply(&n), Applied::Refresh, "{n:?}");
    }
    for n in [
        Notification::Exit,
        Notification::Message("x".into()),
        Notification::Other("output"),
        Notification::Unknown("zzz".into()),
        Notification::SessionChanged {
            session: TmuxSessionId(1),
        },
    ] {
        assert_eq!(t.apply(&n), Applied::Same, "{n:?}");
    }
}

#[test]
fn a_detached_user_hides_everything_and_refresh_lists_three_queries() {
    let mut t = lists(false);
    t.apply(&Notification::ClientDetached {
        client: ClientName("/dev/ttys1".into()),
    });
    assert!(t.visible().is_empty());
    let names: Vec<_> = refresh_cmds()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    assert_eq!(names, ["list-sessions", "list-panes", "list-clients"]);
    assert!(refresh_cmds().iter().all(|c| c.check().is_ok()));
}
