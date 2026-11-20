//! The control client against a real tmux, through the testkit's `Sandbox`.

use std::time::Duration;

use moa_core::prompt::typed as typed_text;
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::mailbox::Mailbox;
use moa_testkit::{Sandbox, Terminal, tmux_path, wait_for};
use moa_tmux::client::{
    CONNECT_WAIT, CmdError, ConnectError, ConnectOptions, ControlClient, Event, Events, ServerInfo,
    version_ok,
};
use moa_tmux::{Cmd, Notification};
use proptest::prelude::*;

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

fn display(text: &str) -> Cmd {
    Cmd::new("display-message").args(["-p", text])
}

/// The next `Replies` event, skipping notifications.
async fn next_replies(events: &mut Events) -> (u64, Vec<Result<Vec<String>, CmdError>>) {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("no replies within 5 s")
            .expect("the events ended");
        if let Event::Replies { tag, results } = ev {
            return (tag, results);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_reads_version_pid_start_and_name() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, info) = connect(&sb).await;
    let pid = sb.tmux(&["display-message", "-p", "#{pid}"]).unwrap();
    assert_eq!(info.generation.pid.to_string(), pid);
    let start = sb
        .tmux(&["display-message", "-p", "#{start_time}"])
        .unwrap();
    assert_eq!(info.generation.start.to_string(), start);
    assert!(version_ok(&info.version), "version {}", info.version);
    assert!(info.client.0.starts_with("client-"), "{:?}", info.client);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_never_starts_a_server() {
    let sb = Sandbox::without_server().unwrap();
    let r = ControlClient::connect(&opts(&sb)).await;
    assert!(matches!(r, Err(ConnectError::Exited(_))), "{:?}", r.err());
    assert!(sb.tmux(&["ls"]).is_err(), "a server was started");
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_creates_the_hidden_session_once() {
    let sb = Sandbox::start().unwrap();
    let (a, _ea, _) = connect(&sb).await;
    let (b, _eb, _) = connect(&sb).await;
    let names = sb
        .tmux(&["list-sessions", "-F", "#{session_name}"])
        .unwrap();
    assert_eq!(names.lines().filter(|n| *n == "_moa").count(), 1, "{names}");
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pipelined_replies_match_in_order() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, _) = connect(&sb).await;
    let cmds = (0..50).map(|i| display(&format!("R {i}"))).collect();
    client.send(cmds, Some(9));
    let (tag, results) = next_replies(&mut events).await;
    assert_eq!(tag, 9);
    assert_eq!(results.len(), 50);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r, &Ok(vec![format!("R {i}")]));
    }
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_reply_does_not_shift_the_rest() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, _) = connect(&sb).await;
    let bad = Cmd::new("list-panes").args(["-t", "%999"]);
    client.send(vec![display("a"), bad, display("b")], Some(1));
    let (_, results) = next_replies(&mut events).await;
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], Ok(vec!["a".to_string()]));
    match &results[1] {
        Err(CmdError::Tmux(t)) => assert!(t.contains("can't find"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(results[2], Ok(vec!["b".to_string()]));
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn after_hook_blocks_are_skipped() {
    let sb = Sandbox::start().unwrap();
    sb.tmux(&[
        "set-hook",
        "-g",
        "after-new-session[91]",
        "display-message -p from-hook ; set -g @after_ran 1",
    ])
    .unwrap();
    let (client, _events, _) = connect(&sb).await;
    client
        .run(&Cmd::new("new-session").args(["-d", "-s", "w"]))
        .await
        .unwrap();
    assert_eq!(client.run(&display("next")).await, Ok(vec!["next".into()]));
    // The hook ran, so its block was sent and skipped, not never sent.
    assert_eq!(sb.tmux(&["show", "-gv", "@after_ran"]).unwrap().trim(), "1");
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_commands_are_never_written() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, _) = connect(&sb).await;
    let r = client.run(&Cmd::new("wait-for")).await;
    assert!(matches!(r, Err(CmdError::Refused(_))), "{r:?}");
    assert_eq!(
        client.run(&display("still")).await,
        Ok(vec!["still".into()])
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_events_end_when_the_server_is_killed() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, _) = connect(&sb).await;
    sb.tmux(&["kill-server"]).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while events.recv().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "the events didn't end within 2 s");
    assert_eq!(client.run(&display("x")).await, Err(CmdError::Gone));
    client.close().await;
}

/// On tmux 3.4 a `$` before a letter, `_` or `{` is stored as `\$` (T17). Whether a `$`
/// before a non-ASCII character is too depends on the C library's `isalpha` (macOS's says
/// yes, glibc's no; found by this test): `non_ascii` picks that spelling.
fn expected_after_tmux(version: &str, s: &str, non_ascii: bool) -> String {
    if !version.starts_with("3.4") {
        return s.to_string();
    }
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '$'
            && it.peek().is_some_and(|n| {
                n.is_ascii_alphabetic() || *n == '_' || *n == '{' || (non_ascii && !n.is_ascii())
            })
        {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The parser reads a line that starts with `%` and a known notification name inside a
/// reply block as a notification before tmux 3.8, so a value with a line that starts
/// with `%` is not generated (see `Parser`'s docs).
fn no_percent_line(s: &str) -> bool {
    !s.split('\n').any(|l| l.starts_with('%'))
}

#[test]
fn quote_round_trips_through_tmux() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let sb = Sandbox::start().unwrap();
    let (client, _events, info) = rt.block_on(connect(&sb));
    let strategy = proptest::string::string_regex(r"[ -~✓中💤\n\t]{0,24}")
        .unwrap()
        .prop_filter("a line starts with %", |s| no_percent_line(s));
    let check = |s: String| {
        rt.block_on(async {
            let set = Cmd::new("set-option").args(["-g", "@q", &s]);
            client
                .run(&set)
                .await
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let got = client
                .run(&display("#{@q}"))
                .await
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            // Whole strings: dropping a `\` before a `$` to compare would also drop one the
            // value had itself.
            let got = got.join("\n");
            let plain = expected_after_tmux(&info.version, &s, false);
            let escaped = expected_after_tmux(&info.version, &s, true);
            prop_assert!(got == plain || got == escaped, "{got:?}, not {plain:?}");
            Ok(())
        })
    };
    // Found by the random cases: a value's own `\` before a 3.4-escaped `$`.
    for s in ["\\$💤", "$💤", "\\$a", "$a"] {
        check(s.to_string()).unwrap();
    }
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(64));
    runner.run(&strategy, check).unwrap();
    rt.block_on(client.close());
}

/// `Cmd::prompt` against a real client: what is typed (or left in the prefill) comes back in
/// `moa:prompt`, whatever it holds, once moa takes back tmux 3.4's `\$` (`prompt::typed`).
#[tokio::test(flavor = "multi_thread")]
async fn prompt_answers_round_trip() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, info) = connect(&sb).await;
    let tmux = tmux_path().unwrap_or_else(|e| panic!("{e}"));
    let mut args: Vec<std::ffi::OsString> = vec!["-u".into()];
    args.extend(sb.socket_args());
    args.extend(["attach".into(), "-t".into(), "=sandbox".into()]);
    let term = Terminal::spawn(&tmux, &args, &sb.env(), 100, 30).unwrap();
    let tty = ClientName(term.tty());
    let seen = wait_for(Duration::from_secs(10), || {
        let list = sb.tmux(&["list-clients", "-F", "#{client_name}"]).ok()?;
        list.lines().any(|l| l == tty.0).then_some(())
    });
    assert!(seen.is_some(), "the terminal never attached");

    let cases = [
        ("", r#"it's "a" $HOME"#, r#"it's "a" $HOME"#),
        ("", "a;b", "a;b"),
        ("", "100%", "100%"),
        ("", "#{pane_id}", "#{pane_id}"),
        ("", "ü 😀", "ü 😀"),
        // The prefill is the answer when nothing is typed.
        ("~/x/", "", "~/x/"),
        ("~/x/", "y", "~/x/y"),
    ];
    for (token, (prefill, typed, want)) in (1u64..).zip(cases) {
        let prompt = Cmd::prompt(&tty, "x", prefill, &info.client, token);
        client.run(&prompt).await.unwrap();
        term.press(typed.as_bytes());
        term.press(b"\r");
        let message = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(ev) = events.recv().await {
                if let Event::Notification(Notification::Message(m)) = ev
                    && m.starts_with("moa:prompt")
                {
                    return Some(m);
                }
            }
            None
        })
        .await
        .unwrap_or_else(|_| panic!("no answer to {typed:?} within 10 s"))
        .expect("the events ended");
        let Ok(Mailbox::Prompt { token: got, text }) = Mailbox::parse(&message) else {
            panic!("{message:?}")
        };
        assert_eq!(got, token, "{typed:?}");
        // The quirk itself: 3.4 sends a typed `$HOME` as `\$HOME`, later ones as typed. The
        // versions that do are the ones `prompt::typed` unescapes.
        if typed.contains("$HOME") {
            assert_eq!(
                text.contains(r"\$HOME"),
                typed_text(&info.version, r"\$") == "$",
                "{} sent {message:?}",
                info.version
            );
        }
        assert_eq!(typed_text(&info.version, &text), want, "{message:?}");
    }
    client.close().await;
}

fn source_cmd(sb: &Sandbox, body: &str) -> Cmd {
    let path = sb.home().join("moa.tmux");
    std::fs::write(&path, body).unwrap();
    Cmd::new("source-file").arg(path.to_string_lossy())
}

const THREE: &str = "set-option -g @a 1\nset-option -g @b 2\nset-option -g @c 3\n";

#[tokio::test(flavor = "multi_thread")]
async fn source_file_is_one_result_and_does_not_shift_the_rest() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, _) = connect(&sb).await;
    let r = client.run(&source_cmd(&sb, THREE)).await;
    assert_eq!(r, Ok(vec![]));
    assert_eq!(client.run(&display("next")).await, Ok(vec!["next".into()]));
    assert_eq!(
        client.run(&display("#{@c}")).await,
        Ok(vec!["3".to_string()])
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_source_file_is_an_error_and_does_not_shift_the_rest() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, _) = connect(&sb).await;
    let bad = "set-option -g @a 1\nno-such-command\nset-option -g @c 3\n";
    let r = client.run(&source_cmd(&sb, bad)).await;
    assert!(matches!(r, Err(CmdError::Tmux(_))), "{r:?}");
    assert_eq!(client.run(&display("next")).await, Ok(vec!["next".into()]));
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tagged_batch_with_source_file_keeps_its_results_aligned() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, _) = connect(&sb).await;
    client.send(vec![source_cmd(&sb, THREE), display("a")], Some(3));
    let (tag, results) = next_replies(&mut events).await;
    assert_eq!(tag, 3);
    assert_eq!(results, vec![Ok(vec![]), Ok(vec!["a".to_string()])]);
    client.close().await;
}

/// L4: tmux runs a chain's commands in order and skips the rest of the line after a
/// failure. The chain is one result, its first error's text, and the next command's reply
/// is still its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_chain_stops_at_its_first_error() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, _) = connect(&sb).await;
    let set = |name: &str| Cmd::new("set-option").args(["-p", "-t", "%0", name, "1"]);
    let chain = Cmd::chain(vec![
        set("@a"),
        Cmd::new("swap-pane").args(["-d", "-s", "%0", "-t", "%999"]),
        set("@b"),
    ]);
    client.send(vec![chain, display("next")], Some(4));
    let (tag, results) = next_replies(&mut events).await;
    assert_eq!(tag, 4);
    assert_eq!(results.len(), 2, "{results:?}");
    match &results[0] {
        Err(CmdError::Tmux(t)) => assert!(t.contains("%999"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(results[1], Ok(vec!["next".to_string()]));
    let option = |name: &str| sb.tmux(&["show-options", "-p", "-t", "%0", "-qv", name]);
    assert_eq!(option("@a").unwrap(), "1");
    assert_eq!(option("@b").unwrap(), "");
    // A chain that runs to its end is `Ok`, with no lines.
    let fine = Cmd::chain(vec![set("@b"), display("not returned")]);
    assert_eq!(client.run(&fine).await, Ok(vec![]));
    assert_eq!(option("@b").unwrap(), "1");
    assert_eq!(
        client.run(&display("after")).await,
        Ok(vec!["after".into()])
    );
    client.close().await;
}

/// Window names are agent titles, free text: one that starts with `-` is still a name. After
/// `--` for `rename-window`, and as the value of `break-pane -n`, tmux takes it as written.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_that_starts_with_a_dash_is_a_name() {
    let sb = Sandbox::start().unwrap();
    sb.tmux(&["new-session", "-d", "-s", "moa"]).unwrap();
    let split = || {
        sb.tmux(&["split-window", "-d", "-P", "-F", "#{pane_id}", "-t", "%0"])
            .unwrap()
    };
    let (a, b) = (split(), split());
    let (client, _events, _) = connect(&sb).await;
    let chain = Cmd::chain(vec![
        Cmd::new("rename-window").args(["-t", "%0", "--", "-v fix"]),
        Cmd::new("break-pane").args(["-d", "-s", &a, "-t", "=moa:", "-n", "-x"]),
        Cmd::new("break-pane").args(["-d", "-s", &b, "-t", "=moa:", "-n", "-v fix"]),
    ]);
    assert_eq!(client.run(&chain).await, Ok(vec![]));
    let name = |pane: &str| {
        sb.tmux(&["display-message", "-p", "-t", pane, "#{window_name}"])
            .unwrap()
    };
    assert_eq!(name("%0"), "-v fix");
    assert_eq!(name(&a), "-x");
    assert_eq!(name(&b), "-v fix");
    let session = |pane: &str| {
        sb.tmux(&["display-message", "-p", "-t", pane, "#{session_name}"])
            .unwrap()
    };
    assert_eq!(session(&a), "moa");
    assert_eq!(session(&b), "moa");
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_file_command_that_fails_at_run_time_is_an_error() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, _) = connect(&sb).await;
    let body = "set-option -g @a 1\nset-option -t nosuch: @b 2\nset-option -g @c 3\n";
    let r = client.run(&source_cmd(&sb, body)).await;
    match &r {
        Err(CmdError::Tmux(t)) => assert!(t.contains("no such session"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(client.run(&display("next")).await, Ok(vec!["next".into()]));
    client.close().await;
}
