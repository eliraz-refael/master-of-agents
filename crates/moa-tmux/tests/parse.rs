use moa_core::tmux::ids::{ClientName, PaneId, TmuxSessionId, WindowId};
use moa_tmux::{Frame, Notification, ParseError, Parser, notification};
use proptest::prelude::*;

fn parse_all(input: &[u8]) -> Vec<Frame> {
    Parser::new().push(input).unwrap()
}

fn reply(flags: u32, error: bool, lines: &[&str]) -> Frame {
    Frame::Reply {
        flags,
        error,
        lines: lines.iter().map(|s| s.to_string()).collect(),
    }
}

fn win(n: u32) -> WindowId {
    WindowId(n)
}
fn pane(n: u32) -> PaneId {
    PaneId(n)
}
fn sess(n: u32) -> TmuxSessionId {
    TmuxSessionId(n)
}

/// The tmux 3.4 capture. The `P` lines end in a space (an empty last field), kept with
/// explicit escapes so no editor can strip it.
const V3_4: &str = concat!(
    "%begin 1794852795 275 0\n",
    "%end 1794852795 275 0\n",
    "%window-add @1\n",
    "%sessions-changed\n",
    "%session-changed $1 _moa\n",
    "%begin 1794852795 281 1\n",
    "V 3.4 14229 1794852795 client-14231\n",
    "%end 1794852795 281 1\n",
    "%client-session-changed /dev/ttys038 $0 moa\n",
    "%message moa:key g %0 $0 /dev/ttys038\n",
    "%begin 1794852799 332 1\n",
    "P %1 @1 $1 1 1 0\x20\n",
    "P %0 @0 $0 1 1 0\x20\n",
    "P %2 @2 $0 0 1 1\x20\n",
    "P %3 @2 $0 1 1 1\x20\n",
    "%end 1794852799 332 1\n",
    "%window-pane-changed @2 %2\n",
    "%begin 1794852800 339 1\n",
    "after-1\n",
    "%end 1794852800 339 1\n",
    "%unlinked-window-add @3\n",
    "%unlinked-window-renamed @3 w3\n",
    "%exit\n",
);

/// The 3.8 capture: no initial `%window-add @1`, and a `%window-renamed @1 tmux` after
/// `%session-changed`.
const V3_8: &str = concat!(
    "%begin 1794852795 275 0\n",
    "%end 1794852795 275 0\n",
    "%sessions-changed\n",
    "%session-changed $1 _moa\n",
    "%window-renamed @1 tmux\n",
    "%begin 1794852795 281 1\n",
    "V 3.8 14229 1794852795 client-14231\n",
    "%end 1794852795 281 1\n",
    "%client-session-changed /dev/ttys038 $0 moa\n",
    "%message moa:key g %0 $0 /dev/ttys038\n",
    "%begin 1794852799 332 1\n",
    "P %1 @1 $1 1 1 0\x20\n",
    "P %0 @0 $0 1 1 0\x20\n",
    "P %2 @2 $0 0 1 1\x20\n",
    "P %3 @2 $0 1 1 1\x20\n",
    "%end 1794852799 332 1\n",
    "%window-pane-changed @2 %2\n",
    "%begin 1794852800 339 1\n",
    "after-1\n",
    "%end 1794852800 339 1\n",
    "%unlinked-window-add @3\n",
    "%unlinked-window-renamed @3 w3\n",
    "%exit\n",
);

fn panes_reply() -> Frame {
    reply(
        1,
        false,
        &[
            "P %1 @1 $1 1 1 0 ",
            "P %0 @0 $0 1 1 0 ",
            "P %2 @2 $0 0 1 1 ",
            "P %3 @2 $0 1 1 1 ",
        ],
    )
}

fn tail_frames() -> Vec<Frame> {
    vec![
        Frame::Notification(Notification::ClientSessionChanged {
            client: ClientName("/dev/ttys038".into()),
            session: sess(0),
        }),
        Frame::Notification(Notification::Message("moa:key g %0 $0 /dev/ttys038".into())),
        panes_reply(),
        Frame::Notification(Notification::WindowPaneChanged {
            window: win(2),
            pane: pane(2),
        }),
        reply(1, false, &["after-1"]),
        Frame::Notification(Notification::WindowAdd { window: win(3) }),
        Frame::Notification(Notification::Other("unlinked-window-renamed")),
        Frame::Notification(Notification::Exit),
    ]
}

fn v34_frames() -> Vec<Frame> {
    let mut v = vec![
        reply(0, false, &[]),
        Frame::Notification(Notification::WindowAdd { window: win(1) }),
        Frame::Notification(Notification::SessionsChanged),
        Frame::Notification(Notification::SessionChanged { session: sess(1) }),
        reply(1, false, &["V 3.4 14229 1794852795 client-14231"]),
    ];
    v.extend(tail_frames());
    v
}

fn v38_frames() -> Vec<Frame> {
    let mut v = vec![
        reply(0, false, &[]),
        Frame::Notification(Notification::SessionsChanged),
        Frame::Notification(Notification::SessionChanged { session: sess(1) }),
        Frame::Notification(Notification::Other("window-renamed")),
        reply(1, false, &["V 3.8 14229 1794852795 client-14231"]),
    ];
    v.extend(tail_frames());
    v
}

#[test]
fn blocks_match_on_time_number_and_flags() {
    let input = "%begin 5 7 1\n%end 1 2 1\n%end 5 7 0\nx\n%end 5 7 1\n";
    assert_eq!(
        parse_all(input.as_bytes()),
        vec![reply(1, false, &["%end 1 2 1", "%end 5 7 0", "x"])]
    );
}

#[test]
fn flag_zero_blocks_keep_their_flags() {
    assert_eq!(
        parse_all(b"%begin 5 7 0\nhi\n%end 5 7 0\n"),
        vec![reply(0, false, &["hi"])]
    );
}

#[test]
fn an_error_block_is_an_error() {
    assert_eq!(
        parse_all(b"%begin 5 7 1\ncan't find pane: %99\n%error 5 7 1\n"),
        vec![reply(1, true, &["can't find pane: %99"])]
    );
}

#[test]
fn a_notification_inside_a_block_is_a_notification() {
    let input = "%begin 5 7 1\nx\n%unlinked-window-add @3\ny\n%end 5 7 1\n";
    assert_eq!(
        parse_all(input.as_bytes()),
        vec![
            Frame::Notification(Notification::WindowAdd { window: win(3) }),
            reply(1, false, &["x", "y"]),
        ]
    );
}

#[test]
fn stray_and_malformed_markers() {
    assert_eq!(parse_all(b"hello\n"), vec![Frame::Stray]);
    // Not three numeric fields: not a block marker, just an unknown notification.
    assert_eq!(
        parse_all(b"%begin 1 2\n"),
        vec![Frame::Notification(Notification::Unknown("begin".into()))]
    );
    assert_eq!(
        parse_all(b"%begin 1 2 x\n"),
        vec![Frame::Notification(Notification::Unknown("begin".into()))]
    );
}

#[test]
fn notifications_parse() {
    use Notification::*;
    let cases: Vec<(&str, Notification)> = vec![
        (
            "%window-pane-changed @0 %3",
            WindowPaneChanged {
                window: win(0),
                pane: pane(3),
            },
        ),
        (
            "%session-window-changed $0 @2",
            SessionWindowChanged {
                session: sess(0),
                window: win(2),
            },
        ),
        ("%unlinked-window-close @2", WindowClose { window: win(2) }),
        (
            "%client-session-changed /dev/ttys033 $0 work",
            ClientSessionChanged {
                client: ClientName("/dev/ttys033".into()),
                session: sess(0),
            },
        ),
        (
            "%client-detached /dev/ttys038",
            ClientDetached {
                client: ClientName("/dev/ttys038".into()),
            },
        ),
        (
            "%layout-change @0 b653,200x60,0,0 b653,200x60,0,0 *",
            LayoutChange { window: win(0) },
        ),
        (
            "%message moa:key x %1 $0 /dev/ttys045",
            Message("moa:key x %1 $0 /dev/ttys045".into()),
        ),
        ("%exit", Exit),
        ("%exit too far behind", Exit),
        ("%window-renamed @1 bash", Other("window-renamed")),
        ("%zap 1", Unknown("zap".into())),
        ("%window-add x", Other("window-add")),
        ("%window-add", Other("window-add")),
        ("%session-changed", Other("session-changed")),
    ];
    for (line, want) in cases {
        assert_eq!(notification(line), want, "{line}");
    }
}

#[test]
fn message_text_is_verbatim() {
    assert_eq!(
        notification("%message a\\026b \\101"),
        Notification::Message("a\\026b \\101".into())
    );
}

#[test]
fn the_captures_parse_to_their_frames() {
    assert_eq!(parse_all(V3_4.as_bytes()), v34_frames());
    assert_eq!(parse_all(V3_8.as_bytes()), v38_frames());
}

#[test]
fn a_huge_line_is_an_error() {
    let big = vec![b'a'; 9 << 20];
    assert!(matches!(
        Parser::new().push(&big),
        Err(ParseError::LineTooLong)
    ));
}

#[test]
fn a_huge_block_is_an_error() {
    let mut p = Parser::new();
    p.push(b"%begin 1 2 1\n").unwrap();
    let mut line = vec![b'a'; 1 << 20];
    line.push(b'\n');
    let mut res = Ok(vec![]);
    for _ in 0..70 {
        res = p.push(&line);
        if res.is_err() {
            break;
        }
    }
    assert!(matches!(res, Err(ParseError::BlockTooLong)));
}

fn chunked(data: &[u8], sizes: &[usize]) -> Vec<Frame> {
    let mut p = Parser::new();
    let mut out = vec![];
    let mut at = 0;
    let mut i = 0;
    while at < data.len() {
        let n = sizes[i % sizes.len()].max(1).min(data.len() - at);
        out.extend(p.push(&data[at..at + n]).unwrap());
        at += n;
        i += 1;
    }
    out
}

proptest! {
    #[test]
    fn split_anywhere_gives_the_same_frames(
        sizes in proptest::collection::vec(1usize..40, 1..20),
        v38 in any::<bool>(),
    ) {
        let data = if v38 { V3_8 } else { V3_4 }.as_bytes();
        prop_assert_eq!(chunked(data, &sizes), parse_all(data));
    }

    #[test]
    fn any_bytes_never_panic(chunks in proptest::collection::vec(
        proptest::collection::vec(
            prop_oneof![Just(b'%'), Just(b'\n'), Just(b' '), any::<u8>()], 0..64),
        0..16)) {
        let mut p = Parser::new();
        for c in &chunks {
            if p.push(c).is_err() {
                break;
            }
        }
    }
}
