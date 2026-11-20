use std::collections::BTreeSet;

use moa_core::model::SessionId;
use moa_core::tmux::cmd::{ALLOWED, Cmd, Refused, quote};
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
        Cmd::new("wait-for").check().unwrap_err().0,
        "that isn't a command moa sends"
    );
    // One job only, `true`, which makes tmux reap a pane's lost exit: no shell runs moa's
    // words, and no other job.
    assert_eq!(Cmd::new("run-shell").arg("true").check(), Ok(()));
    for args in [
        &[][..],
        &["false"],
        &["true; rm -rf ~"],
        &["-b", "true"],
        &["true", "-b"],
        &["-C", "true"],
        &["-t", "%3", "true"],
        &["true", "true"],
    ] {
        assert_eq!(
            Cmd::new("run-shell")
                .args(args.iter().copied())
                .check()
                .unwrap_err()
                .0,
            "run-shell only runs true",
            "{args:?}"
        );
    }
    for name in ALLOWED {
        // These have rules of their own, tested in `cmd.rs`.
        let ruled = [
            "command-prompt",
            "source-file",
            "new-window",
            "kill-pane",
            "select-window",
            "select-pane",
            "swap-pane",
            "join-pane",
            "break-pane",
            "resize-pane",
            "respawn-pane",
            "rename-window",
            "resize-window",
            "run-shell",
        ];
        if !ruled.contains(name) {
            assert!(Cmd::new(name).check().is_ok(), "{name}");
        }
    }
    assert_eq!(
        Cmd::new("new-window").check().unwrap_err().0,
        "new-window needs -d and -P"
    );
    assert!(
        Cmd::new("new-window")
            .args(["-d", "-P", "-t", "=moa:"])
            .check()
            .is_ok()
    );
    assert_eq!(
        Cmd::new("command-prompt").arg("x").check().unwrap_err().0,
        "command-prompt needs -b"
    );
    let source = |p: &str| Cmd::new("source-file").arg(p).check();
    assert_eq!(
        source("/x/tmux.conf").unwrap_err().0,
        "moa only sources its own config"
    );
    assert!(source("/x/notmoa.tmux").is_err());
    assert!(Cmd::new("source-file").check().is_err());
    assert!(source("/x/moa.tmux").is_ok());
}

/// `Cmd::prompt` builds the one `command-prompt` moa sends, and the check takes no other: one
/// without `-b` would freeze the control client, and any other template could run anything.
#[test]
fn only_moas_prompt_shape_is_allowed() {
    let (client, ctl) = (
        ClientName("/dev/pts/3".into()),
        ClientName("client-9".into()),
    );
    let prompt = Cmd::prompt(&client, "title:", "~/x/", &ctl, 12);
    assert_eq!(prompt.check(), Ok(()));
    assert_eq!(
        prompt.line(),
        r#"command-prompt -b -t /dev/pts/3 -p title: -I '~/x/' 'display-message -c client-9 -l "moa:prompt 12 %%%"'"#
    );
    let template = r#"display-message -c client-9 -l "moa:prompt 12 %%%""#;
    let check = |args: &[&str]| {
        Cmd::new("command-prompt")
            .args(args.iter().copied())
            .check()
    };
    // No `-b`, or a `-b` that is another flag's value.
    assert_eq!(
        check(&["-t", "c", "-p", "x", "-I", "", template]),
        Err(Refused("command-prompt needs -b"))
    );
    assert_eq!(
        check(&["-p", "-b", "-t", "c", "-I", "", template]),
        Err(Refused("command-prompt needs -b"))
    );
    assert_eq!(
        check(&["-p", "x", "display-message …"]),
        Err(Refused("command-prompt needs -b"))
    );
    let shape = Err(Refused("command-prompt only sends moa:prompt back to moa"));
    for bad in [
        // Two templates.
        &["-b", "-t", "c", "-p", "x", "-I", "", template, template][..],
        // No template.
        &["-b", "-t", "c", "-p", "x", "-I", ""],
        // A flag missing, given twice, or one moa doesn't use.
        &["-b", "-p", "x", "-I", "", template],
        &["-b", "-b", "-t", "c", "-p", "x", "-I", "", template],
        &["-b", "-N", "-t", "c", "-p", "x", "-I", "", template],
        // Another template.
        &["-b", "-t", "c", "-p", "x", "-I", "", "kill-server"],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c client-9 -l "moa:prompt 12 %%%" ; kill-server"#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c client-9 -l "moa:prompt x %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c client-9 -l "moa:prompt  %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c client-9 -l "moa:key 12 %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c 'a b' -l "moa:prompt 12 %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c  -l "moa:prompt 12 %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c x;y -l "moa:prompt 12 %%%""#,
        ],
        &[
            "-b",
            "-t",
            "c",
            "-p",
            "x",
            "-I",
            "",
            r#"display-message -c client-9 -l "moa:prompt 12 %%" "#,
        ],
    ] {
        assert_eq!(check(bad), shape, "{bad:?}");
    }
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
            // As tmux sent it: only the core knows which tmux unescapes (`prompt::typed`).
            text: r#"it's "a" \$HOME"#.into()
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
            let text = a.not_yet().unwrap_or_default();
            assert!(!text.contains(word), "{a:?}: {text}");
        }
        // Whole words: "Repair panels" is fine.
        let label = a.label().to_lowercase();
        for w in label.split(|c: char| !c.is_alphanumeric()) {
            let w = w.strip_suffix('s').unwrap_or(w);
            assert!(
                !["tmux", "pane", "window"].contains(&w),
                "{a:?}: {}",
                a.label()
            );
        }
    }
    assert_eq!(KeyAction::ALL.len(), 14);
    assert_eq!(KeyAction::from_word("layout-5"), None);
    assert_eq!(KeyAction::from_word("layout-0"), None);
    assert_eq!(KeyAction::Layout(2).label(), "Layout: focus (agent)");
    assert_eq!(KeyAction::FindOther.not_yet(), KeyAction::Find.not_yet());
    // The keys the core acts on say nothing is coming (L13).
    let coming: Vec<KeyAction> = KeyAction::ALL
        .into_iter()
        .filter(|a| a.not_yet().is_some())
        .collect();
    assert_eq!(
        coming,
        [KeyAction::Find, KeyAction::FindOther, KeyAction::Help]
    );
}

const MOA_SESSION: &str = "0b9e6c6e-2a43-4a7e-9d51-6e3f0d3c8a11";

fn lists(zoomed: bool) -> Topology {
    let z = u8::from(zoomed);
    let sessions = vec!["S $0 work".to_string(), "S $1 ctl".to_string()];
    let panes = vec![
        format!("P %0 @0 $0 1 0 0 0 {MOA_SESSION}"),
        format!("P %2 @2 $0 0 1 {z} 0 "),
        format!("P %3 @2 $0 1 1 {z} 0 "),
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
        &["P %2 @2 $0 1 1 0 0 ".to_string()],
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
        // A rename of a window or session the lists don't hold yet.
        Notification::WindowRenamed {
            window: WindowId(5),
            name: "moa".into(),
        },
        Notification::SessionRenamed {
            session: TmuxSessionId(7),
            name: "moa".into(),
        },
        // A rename whose arguments didn't parse.
        Notification::Other("session-renamed"),
        Notification::Other("window-renamed"),
        Notification::Other("unlinked-window-renamed"),
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

/// L4: the commands that move panes name panes by id and sessions exactly, so a prefix
/// never matches a stranger; each takes only its own shape.
#[test]
fn the_pane_moving_commands_take_only_their_safe_shapes() {
    let cmd = |name: &'static str, args: &[&str]| Cmd::new(name).args(args.iter().copied());
    for ok in [
        cmd("swap-pane", &["-d", "-s", "%1", "-t", "%2"]),
        cmd("join-pane", &["-d", "-h", "-s", "%1", "-t", "%2"]),
        cmd("join-pane", &["-d", "-v", "-s", "%1", "-t", "%2"]),
        cmd("break-pane", &["-d", "-s", "%1", "-t", "=moa:"]),
        cmd("break-pane", &["-d", "-s", "%1", "-t", "=_moa:"]),
        cmd(
            "break-pane",
            &["-d", "-s", "%1", "-t", "=moa:", "-n", "⌨ shell #1"],
        ),
        cmd("resize-pane", &["-t", "%1", "-x", "30%"]),
        cmd("resize-pane", &["-t", "%1", "-y", "12"]),
        cmd("resize-pane", &["-t", "%1", "-y", "1"]),
        cmd("resize-pane", &["-t", "%1", "-x", "1%"]),
        cmd("resize-pane", &["-t", "%1", "-x", "99%"]),
        cmd(
            "respawn-pane",
            &["-k", "-t", "%1", "/opt/moa", "panel", "empty-slot"],
        ),
        cmd(
            "respawn-pane",
            &[
                "-k", "-t", "%1", "-c", "/w", "-e", "A=b", "-e", "C=d", "/opt/moa", "panel",
            ],
        ),
        // A name that starts with `-` is an option's value here, and tmux takes it as one.
        cmd("break-pane", &["-d", "-s", "%1", "-t", "=moa:", "-n", "-x"]),
        // `--` always, so a title that starts with `-` is a name and never flags.
        cmd("rename-window", &["-t", "%1", "--", "Tide tables"]),
        cmd("rename-window", &["-t", "%1", "--", "-v fix"]),
    ] {
        assert_eq!(ok.check(), Ok(()), "{}", ok.line());
    }
    let refused = |c: Cmd, why: &str| {
        assert_eq!(c.check().map_err(|r| r.0), Err(why), "{}", c.line());
    };
    let swap = "swap-pane needs -d and two tab ids";
    refused(cmd("swap-pane", &["-s", "%1", "-t", "%2"]), swap);
    refused(cmd("swap-pane", &["-d", "-s", "%1", "-t", "moa"]), swap);
    refused(
        cmd("swap-pane", &["-d", "-s", "%1", "-t", "%2", "-Z"]),
        swap,
    );
    let join = "join-pane needs -d, -h or -v, and two tab ids";
    refused(
        cmd("join-pane", &["-d", "-h", "-s", "%1", "-t", "moa"]),
        join,
    );
    refused(cmd("join-pane", &["-d", "-s", "%1", "-t", "%2"]), join);
    refused(cmd("join-pane", &["-h", "-s", "%1", "-t", "%2"]), join);
    refused(
        cmd("join-pane", &["-d", "-b", "-s", "%1", "-t", "%2"]),
        join,
    );
    let brk = "break-pane needs -d, a tab id, and =moa: or =_moa:";
    refused(cmd("break-pane", &["-d", "-s", "%1", "-t", "moa:"]), brk);
    refused(cmd("break-pane", &["-d", "-s", "%1", "-t", "_moa2:"]), brk);
    refused(cmd("break-pane", &["-d", "-s", "%1", "-t", "=_moa2:"]), brk);
    refused(cmd("break-pane", &["-d", "-s", "%1", "-t", "=moa:1"]), brk);
    refused(cmd("break-pane", &["-s", "%1", "-t", "=moa:"]), brk);
    refused(cmd("break-pane", &["-d", "-s", "moa", "-t", "=moa:"]), brk);
    refused(
        cmd("break-pane", &["-d", "-s", "%1", "-t", "=moa:", "-n"]),
        brk,
    );
    refused(
        cmd("break-pane", &["-d", "-s", "%1", "-t", "=moa:", "-P"]),
        brk,
    );
    let resize = "resize-pane needs a tab id, and -x or -y with a size";
    refused(cmd("resize-pane", &["-t", "%1"]), resize);
    refused(cmd("resize-pane", &["-t", "%1", "-Z"]), resize);
    refused(cmd("resize-pane", &["-t", "moa", "-x", "30"]), resize);
    refused(cmd("resize-pane", &["-t", "%1", "-x", "30%%"]), resize);
    refused(cmd("resize-pane", &["-t", "%1", "-x", "%"]), resize);
    refused(cmd("resize-pane", &["-t", "%1", "-x", "+3"]), resize);
    // L1's sizes: cells from 1, percentages from 1 to 99.
    for size in ["0", "00", "0%", "100%", "150%", "70000"] {
        refused(cmd("resize-pane", &["-t", "%1", "-y", size]), resize);
    }
    let respawn = "respawn-pane needs -k, a tab id and a command";
    refused(cmd("respawn-pane", &["-t", "%1", "/opt/moa"]), respawn);
    refused(
        cmd("respawn-pane", &["-k", "-t", "moa", "/opt/moa"]),
        respawn,
    );
    refused(cmd("respawn-pane", &["-k", "-t", "%1"]), respawn);
    refused(
        cmd("respawn-pane", &["-k", "-t", "%1", "-c", "/w"]),
        respawn,
    );
    // A word after the flags that tmux would read as another flag.
    refused(
        cmd("respawn-pane", &["-k", "-t", "%1", "-t", "%2", "/opt/moa"]),
        respawn,
    );
    refused(
        cmd(
            "respawn-pane",
            &["-k", "-t", "%1", "-e", "A=b\nc", "/opt/moa"],
        ),
        "a tab's environment can't hold a newline",
    );
    let rename = "rename-window needs a tab id, then -- and a name";
    refused(cmd("rename-window", &["-t", "moa", "--", "x"]), rename);
    refused(cmd("rename-window", &["-t", "%1", "x"]), rename);
    refused(cmd("rename-window", &["-t", "%1", "-v fix"]), rename);
    refused(cmd("rename-window", &["-t", "%1", "--"]), rename);
    refused(cmd("rename-window", &["-t", "%1", "--", "x", "y"]), rename);
    // tmux expands the name as a format: a `#` must come doubled, as `literal_format` writes it.
    let format = "rename-window's name must keep # literal (##)";
    refused(cmd("rename-window", &["-t", "%1", "--", "#(id)"]), format);
    refused(cmd("rename-window", &["-t", "%1", "--", "a###"]), format);
    assert_eq!(
        cmd("rename-window", &["-t", "%1", "--", "shell ##1"]).check(),
        Ok(())
    );
    // `select-window` and `select-pane` name a pane by its id, like every other move.
    let select = "select-window and select-pane need -t and a tab id";
    for name in ["select-window", "select-pane"] {
        assert_eq!(cmd(name, &["-t", "%3"]).check(), Ok(()), "{name}");
        for args in [
            &["-t", "moa"][..],
            &["-t", "=moa:"],
            &["-t", "@1"],
            &["-t", "%3x"],
            &["-Z", "-t", "%3"],
            &["-t", "%3", "-Z"],
        ] {
            refused(cmd(name, args), select);
        }
    }
    // A rebuilt cockpit's window takes its terminal's size, then follows it again (L6).
    assert_eq!(cmd("resize-window", &["-A", "-t", "%3"]).check(), Ok(()));
    let resize_window = "resize-window needs -A and a tab id";
    for args in [
        &["-t", "%3"][..],
        &["-A", "-t", "moa"],
        &["-A", "-t", "@1"],
        &["-a", "-t", "%3"],
        &["-x", "80", "-t", "%3"],
        &["-A", "-t", "%3", "-x", "80"],
    ] {
        refused(cmd("resize-window", args), resize_window);
    }
    assert_eq!(
        cmd("set-option", &["-w", "-u", "-t", "%3", "window-size"]).check(),
        Ok(())
    );
    // Only the option named `window-size` is held to that shape: a value that reads
    // `window-size` (an agent titled so) is any other option's.
    assert_eq!(
        cmd(
            "set-option",
            &["-p", "-t", "%5", "@moa_label", "window-size"]
        )
        .check(),
        Ok(())
    );
    assert_eq!(
        cmd(
            "set-option",
            &["-p", "-t", "%5", "@moa_label", "● window-size"]
        )
        .check(),
        Ok(())
    );
    let window_size = "window-size is only unset, on a tab id's window";
    for args in [
        &["-w", "-t", "%3", "window-size", "manual"][..],
        &["-w", "-u", "-t", "@1", "window-size"],
        &["-g", "window-size", "smallest"],
        &["-u", "-t", "%3", "window-size"],
    ] {
        refused(cmd("set-option", args), window_size);
    }
    refused(
        cmd("move-window", &["-s", "%1", "-t", "=moa:"]),
        "that isn't a command moa sends",
    );
}

/// A new window opens in one of moa's own sessions, named exactly: never where tmux would
/// put it on its own, and never in a session whose name only starts like moa's.
#[test]
fn a_new_window_opens_only_in_moas_sessions() {
    let new_window = |target: &[&str]| {
        Cmd::new("new-window")
            .args(["-d", "-P", "-F", "#{pane_id}", "-n", "x", "-c", "/w"])
            .args(target.iter().copied())
            .args(["/opt/moa", "panel", "tree", "-t", "elsewhere"])
            .check()
    };
    assert_eq!(new_window(&["-t", "=moa:"]), Ok(()));
    assert_eq!(new_window(&["-t", "=_moa:"]), Ok(()));
    let target = "new-window needs -t and =moa: or =_moa:";
    for refused in [
        &[][..],
        &["-t", "moa"],
        &["-t", "moa:"],
        &["-t", "=moa"],
        &["-t", "=moa:1"],
        &["-t", "=_moa2:"],
        &["-t", "%1"],
        &["-t", "=moa:", "-t", "elsewhere"],
        &["-t"],
    ] {
        assert_eq!(
            new_window(refused).map_err(|r| r.0),
            Err(target),
            "{refused:?}"
        );
    }
}

/// L4: a chain is one line, ` ; `-joined, checked part by part, and answered as one.
#[test]
fn a_chain_is_one_line_and_checks_every_part() {
    let select = Cmd::new("select-pane").args(["-t", "%1"]);
    let swap = Cmd::new("swap-pane").args(["-d", "-s", "%1", "-t", "%2"]);
    let chain = Cmd::chain(vec![select.clone(), swap.clone()]);
    assert_eq!(chain.line(), "select-pane -t %1 ; swap-pane -d -s %1 -t %2");
    assert_eq!(chain.check(), Ok(()));
    assert!(chain.nests());
    assert_eq!(chain.parts(), [select.clone(), swap.clone()]);
    // A plain command is its own one part.
    assert_eq!(swap.parts(), std::slice::from_ref(&swap));
    assert!(!swap.nests());
    // A chain in a chain is its parts.
    let nested = Cmd::chain(vec![chain.clone(), select.clone()]);
    assert_eq!(
        nested.parts(),
        [select.clone(), swap.clone(), select.clone()]
    );
    // A `;` inside an argument is the argument's, never a separator.
    let semi = Cmd::chain(vec![
        Cmd::new("rename-window").args(["-t", "%1", "--", "a ; b"]),
    ]);
    assert_eq!(semi.line(), "rename-window -t %1 -- 'a ; b'");
    assert_eq!(semi.check(), Ok(()));

    let bad = Cmd::new("swap-pane").args(["-s", "%1", "-t", "%2"]);
    assert_eq!(
        Cmd::chain(vec![select.clone(), bad])
            .check()
            .map_err(|r| r.0),
        Err("swap-pane needs -d and two tab ids")
    );
    assert_eq!(
        Cmd::chain(vec![select, Cmd::new("move-window")])
            .check()
            .map_err(|r| r.0),
        Err("that isn't a command moa sends")
    );
    assert_eq!(
        Cmd::chain(Vec::new()).check().map_err(|r| r.0),
        Err("a chain needs at least one command")
    );
}
