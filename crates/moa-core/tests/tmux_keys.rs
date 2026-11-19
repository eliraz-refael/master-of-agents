//! Keys, menu items and the rest of the mailbox, every input through `handle_checked`.

use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{
    Effect, Input, LogLevel, LogLine, TmuxIn, TmuxToken, check_order, handle_checked,
};
use moa_core::state::State;
use moa_core::tmux::ids::{ClientName, Generation};
use moa_core::tmux::mailbox::KeyAction;
use moa_core::tmux::notify::Notification;
use moa_core::tmux::topology::refresh_cmds;
use moa_core::{Now, UtcMs};

fn now() -> Now {
    let wall = UtcMs(
        "2026-11-15T17:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    Now::at(
        wall,
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn feed(s: &mut State, t: TmuxIn) -> Vec<Effect> {
    handle_checked(s, Input::Tmux(t), now())
}

fn message(s: &mut State, text: &str) -> Vec<Effect> {
    let fx = feed(s, TmuxIn::Notification(Notification::Message(text.into())));
    check_order(&fx).unwrap();
    fx
}

fn started() -> State {
    let mut s = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut s, Input::Startup(Box::default()), now());
    s
}

/// A connected link whose first refresh has been answered, so nothing is left to write. The
/// cockpit is built (its console panel `%3`), so the link builds nothing.
fn up() -> State {
    let mut s = started();
    feed(
        &mut s,
        TmuxIn::Connected {
            conn: 1,
            generation: Generation {
                pid: 10,
                start: 1000,
            },
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        },
    );
    feed(
        &mut s,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Refresh,
            results: vec![
                Ok(vec!["S $0 moa".into(), "S $1 _moa".into()]),
                Ok(vec!["P %3 @1 $0 1 1 0 1  0 console".into()]),
                Ok(vec!["C 0 $0 /dev/ttys1".into()]),
            ],
        },
    );
    s
}

fn logs(fx: &[Effect]) -> Vec<&LogLine> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(l),
            _ => None,
        })
        .collect()
}

fn tmux_effects(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| matches!(e, Effect::Tmux { .. }))
        .collect()
}

/// The argv of the one `Tmux` effect's one command, which is untokened.
fn the_notice(fx: &[Effect]) -> Vec<String> {
    let [
        Effect::Tmux {
            conn: 1,
            cmds,
            token: None,
        },
    ] = tmux_effects(fx)[..]
    else {
        panic!("{fx:?}")
    };
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].name(), "display-message");
    cmds[0].argv().to_vec()
}

fn expected(client: &str, a: KeyAction) -> Vec<String> {
    ["-c", client, "-d", "4000", "-l"]
        .into_iter()
        .map(String::from)
        .chain([format!("moa: {}", a.not_yet())])
        .collect()
}

/// The keys whose slice hasn't landed (5c) still say so.
#[test]
fn every_key_gets_its_notice() {
    let mut s = up();
    for a in [
        KeyAction::Find,
        KeyAction::FindOther,
        KeyAction::New,
        KeyAction::Help,
    ] {
        let fx = message(&mut s, &format!("moa:key {} %3 $0 /dev/ttys1", a.word()));
        assert_eq!(the_notice(&fx), expected("/dev/ttys1", a), "{a:?}");
        assert!(logs(&fx).is_empty(), "{a:?}");
    }
}

/// The cockpit's keys (L13): `c t p g` go to a panel, `1`–`4` apply a preset and `r`
/// repairs. Each starts a cockpit operation, which lists the panes first (L17); none says
/// "not yet".
#[test]
fn the_cockpit_keys_act() {
    for k in ["c", "t", "p", "g", "1", "2", "3", "4", "r"] {
        let a = KeyAction::ALL.into_iter().find(|a| a.key() == k).unwrap();
        let mut s = up();
        let fx = message(&mut s, &format!("moa:key {} %3 $0 /dev/ttys1", a.word()));
        let [
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Query(_)),
            },
        ] = tmux_effects(&fx)[..]
        else {
            panic!("{k}: {fx:?}")
        };
        assert_eq!(*cmds, refresh_cmds(), "{k}");
        assert!(logs(&fx).is_empty(), "{k}");
    }
}

/// `$` asks tmux where tab `%3` is, and says nothing yet.
#[test]
fn the_terminal_key_is_no_longer_a_notice() {
    let mut s = up();
    let fx = message(&mut s, "moa:key terminal %3 $0 /dev/ttys1");
    let [
        Effect::Tmux {
            conn: 1,
            cmds,
            token: Some(TmuxToken::Query(_)),
        },
    ] = tmux_effects(&fx)[..]
    else {
        panic!("{fx:?}")
    };
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].name(), "display-message");
    assert_eq!(cmds[0].argv(), ["-p", "-t", "%3", "#{pane_current_path}"]);
    assert!(logs(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_menu_item_without_a_pane_still_answers() {
    let mut s = up();
    let fx = message(&mut s, "moa:key help - $0 /dev/ttys1");
    assert_eq!(the_notice(&fx), expected("/dev/ttys1", KeyAction::Help));
}

#[test]
fn a_key_does_nothing_unless_the_link_is_up() {
    let mut s = started();
    let fx = message(&mut s, "moa:key tree - $0 /dev/ttys1");
    assert!(tmux_effects(&fx).is_empty());
}

// The four hooks, the topology notifications and the coalescing are pinned by
// `structural_changes_refresh` in tmux_focus.rs. Here: a hook after a key still refreshes.
#[test]
fn hook_messages_refresh() {
    let mut s = up();
    message(&mut s, "moa:key console %3 $0 /dev/ttys1");
    let fx = message(&mut s, "moa:hook layout @1");
    let [
        Effect::Tmux {
            token: Some(TmuxToken::Refresh),
            ..
        },
    ] = tmux_effects(&fx)[..]
    else {
        panic!("{fx:?}")
    };
    assert!(logs(&fx).is_empty());
}

#[test]
fn messages_moa_did_not_send_are_logged_without_their_text() {
    let mut s = up();
    let cases = [
        (
            "moa:prompt 9 SECRET-7",
            LogLevel::Info,
            "a prompt answer arrived that nothing waits for",
        ),
        (
            "SECRET-7 hello",
            LogLevel::Warn,
            "tmux sent a message moa didn't ask for",
        ),
        (
            "moa:key SECRET-7 %3 $0 c",
            LogLevel::Warn,
            "a moa message didn't parse",
        ),
        ("moa:SECRET-7", LogLevel::Warn, "a moa message didn't parse"),
    ];
    for (m, level, text) in cases {
        let fx = message(&mut s, m);
        let l = logs(&fx);
        assert_eq!(l.len(), 1, "{m}");
        assert_eq!(l[0].level, level, "{m}");
        assert_eq!(l[0].message, text, "{m}");
        assert!(!l[0].message.contains("SECRET-7"));
        assert!(tmux_effects(&fx).is_empty(), "{m}");
    }
}
