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

#[test]
fn with_the_knob_off_in_block_notifications_are_content() {
    let mut p = Parser::new();
    p.set_notifications_in_blocks(false);
    let input = "%begin 5 7 1\n%message moa:key x %1 $0 c\n%exit\n%end 5 7 1\n";
    assert_eq!(
        p.push(input.as_bytes()).unwrap(),
        vec![reply(1, false, &["%message moa:key x %1 $0 c", "%exit"])]
    );
}

#[test]
fn the_closing_marker_must_match_the_raw_fields() {
    let input = "%begin 5 7 1\n%end +5 7 1\n%end 05 7 1\n%end 5 7 01\n%end 5 7 1\n";
    assert_eq!(
        parse_all(input.as_bytes()),
        vec![reply(
            1,
            false,
            &["%end +5 7 1", "%end 05 7 1", "%end 5 7 01"]
        )]
    );
}

/// A line inside a block in the model.
#[derive(Clone, Debug)]
enum Line {
    Content(String),
    /// A `%end` or `%error` with exactly one of time, number, flags changed.
    Decoy {
        error: bool,
        field: u8,
    },
    Notif(usize),
}

#[derive(Clone, Debug)]
enum Item {
    Block {
        time: u32,
        number: u32,
        flags: u32,
        error: bool,
        lines: Vec<Line>,
    },
    Notif(usize),
}

fn samples() -> Vec<(&'static str, Notification)> {
    vec![
        ("%sessions-changed", Notification::SessionsChanged),
        ("%window-add @4", Notification::WindowAdd { window: win(4) }),
        ("%exit", Notification::Exit),
        (
            "%message hi there",
            Notification::Message("hi there".into()),
        ),
        (
            "%window-renamed @1 x",
            Notification::Other("window-renamed"),
        ),
        (
            "%window-pane-changed @0 %3",
            Notification::WindowPaneChanged {
                window: win(0),
                pane: pane(3),
            },
        ),
    ]
}

fn line_strategy() -> impl Strategy<Value = Line> {
    prop_oneof![
        4 => "[a-z][a-z0-9 ]{0,20}".prop_map(Line::Content),
        2 => (any::<bool>(), 0u8..3).prop_map(|(error, field)| Line::Decoy { error, field }),
        2 => (0..samples().len()).prop_map(Line::Notif),
    ]
}

fn item_strategy() -> impl Strategy<Value = Item> {
    prop_oneof![
        3 => (
            0u32..1000,
            0u32..1000,
            0u32..2,
            any::<bool>(),
            proptest::collection::vec(line_strategy(), 0..8),
        )
            .prop_map(|(time, number, flags, error, lines)| Item::Block {
                time,
                number,
                flags,
                error,
                lines,
            }),
        1 => (0..samples().len()).prop_map(Item::Notif),
    ]
}

/// Renders the items to bytes and the frames a correct parser gives.
fn model(items: &[Item], knob: bool) -> (Vec<u8>, Vec<Frame>) {
    let samples = samples();
    let mut bytes = String::new();
    let mut frames = vec![];
    for item in items {
        match item {
            Item::Notif(i) => {
                bytes += &format!("{}\n", samples[*i].0);
                frames.push(Frame::Notification(samples[*i].1.clone()));
            }
            Item::Block {
                time,
                number,
                flags,
                error,
                lines,
            } => {
                bytes += &format!("%begin {time} {number} {flags}\n");
                let mut content = vec![];
                for l in lines {
                    let text = match l {
                        Line::Content(s) => s.clone(),
                        Line::Decoy { error, field } => {
                            let (t, n, f) = match field {
                                0 => (time + 1, *number, *flags),
                                1 => (*time, number + 1, *flags),
                                _ => (*time, *number, 1 - flags),
                            };
                            format!("{} {t} {n} {f}", if *error { "%error" } else { "%end" })
                        }
                        Line::Notif(i) => {
                            let (text, n) = &samples[*i];
                            if knob {
                                bytes += &format!("{text}\n");
                                frames.push(Frame::Notification(n.clone()));
                                continue;
                            }
                            text.to_string()
                        }
                    };
                    bytes += &format!("{text}\n");
                    content.push(text);
                }
                let word = if *error { "%error" } else { "%end" };
                bytes += &format!("{word} {time} {number} {flags}\n");
                frames.push(Frame::Reply {
                    flags: *flags,
                    error: *error,
                    lines: content,
                });
            }
        }
    }
    (bytes.into_bytes(), frames)
}

fn chunked_with(data: &[u8], sizes: &[usize], knob: bool) -> Vec<Frame> {
    let mut p = Parser::new();
    p.set_notifications_in_blocks(knob);
    let mut out = vec![];
    let (mut at, mut i) = (0, 0);
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
    fn well_formed_streams_give_the_model_frames(
        items in proptest::collection::vec(item_strategy(), 0..12),
        sizes in proptest::collection::vec(1usize..50, 1..10),
        knob in any::<bool>(),
    ) {
        let (bytes, want) = model(&items, knob);
        prop_assert_eq!(chunked_with(&bytes, &sizes, knob), want);
    }

    #[test]
    fn arbitrary_bytes_chunked_equal_one_shot(
        data in proptest::collection::vec(
            prop_oneof![Just(b'%'), Just(b'\n'), Just(b' '), any::<u8>()], 0..300),
        sizes in proptest::collection::vec(1usize..30, 1..10),
        knob in any::<bool>(),
    ) {
        let mut one = Parser::new();
        one.set_notifications_in_blocks(knob);
        let whole = one.push(&data).unwrap();
        prop_assert_eq!(chunked_with(&data, &sizes, knob), whole);
    }

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
