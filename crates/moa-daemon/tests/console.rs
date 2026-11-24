//! The console's `/commands`: an in-process daemon on a `/tmp` root with the Typical brain,
//! whose moa account runs `stub-acp`. Each command runs as the `moactl` verb it names, and
//! none of them is a turn of moa's.

mod common;

use common::Client;
use common::rig::{Rig, Up, answer, refusal};
use moa_core::console::COMMANDS;
use moa_platform::spawn;
use serde_json::{Value, json};

/// A rig with the Typical brain in it, started.
async fn typical() -> (Rig, Up) {
    let rig = Rig::new(&[]);
    rig.tb.adopt_typical("default");
    let up = rig.start().await;
    (rig, up)
}

fn command(c: &mut Client, line: &str) -> Value {
    c.call("console.command", json!({ "line": line }))
}

/// A reply without its id: what it says, either way.
fn outcome(reply: &Value) -> Value {
    reply
        .get("result")
        .or_else(|| reply.get("error"))
        .cloned()
        .unwrap_or_else(|| panic!("a reply: {reply}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ls_runs_as_moactl_ls() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    let typed = c.call("session.ls", json!({}));
    let ls = command(&mut c, "/ls");
    assert_eq!(answer(&ls), answer(&typed));
    assert!(answer(&ls).contains("Fix the flicker"), "{ls}");
    assert_eq!(ls["result"]["data"], typed["result"]["data"]);
    let all = command(&mut c, "  /ls --all ");
    assert_eq!(
        outcome(&all),
        outcome(&c.call("session.ls", json!({"all": true})))
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_takes_a_quoted_title() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    let focus = command(&mut c, "/focus \"Rust scaffold\"");
    assert_eq!(
        outcome(&focus),
        outcome(&c.call("session.focus", json!({"q": "Rust scaffold"})))
    );
    // Unquoted, the title is two words, and `focus` takes one.
    assert!(
        refusal(&command(&mut c, "/focus Rust scaffold")).starts_with("unexpected argument"),
        "two words are two arguments"
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn help_lists_every_command() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    let help = command(&mut c, "/help");
    let text = answer(&help);
    let lines: Vec<&str> = text.lines().collect();
    let shown: Vec<_> = COMMANDS.iter().filter(|c| !c.hidden).collect();
    assert_eq!(lines.len(), shown.len(), "{text}");
    for (line, cmd) in lines.iter().zip(&shown) {
        assert_eq!(*line, format!("- `/{}` {}", cmd.name, cmd.help));
    }
    assert!(!text.contains("`/suggestion`"), "{text}");
    let names: Vec<&str> = shown.iter().map(|c| c.name).collect();
    assert_eq!(help["result"]["data"]["commands"], json!(names));
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn later_commands_say_so() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    for line in ["/setup", "/setup acct-b"] {
        assert_eq!(
            refusal(&command(&mut c, line)),
            "/setup comes in a later version of moa"
        );
    }
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_and_bad_lines_are_refused() {
    let (rig, up) = typical().await;
    let mut c = up.client();

    assert_eq!(refusal(&command(&mut c, "ls")), "a command starts with /");
    assert_eq!(
        refusal(&command(&mut c, "/nope")),
        "there's no /nope command; /help lists them"
    );
    // A moactl verb the console doesn't run is no command either.
    assert_eq!(
        refusal(&command(&mut c, "/task ls")),
        "there's no /task command; /help lists them"
    );
    assert_eq!(
        refusal(&command(&mut c, "/resume")),
        "the following required arguments were not provided: <ID>"
    );
    assert_eq!(
        refusal(&command(&mut c, "/ls --help")),
        "a console command can't ask for help; /help lists them"
    );
    // The daemon never makes a folder: one that isn't there is refused.
    let missing = rig.tb.root().join("harbor");
    for verb in ["open", "term"] {
        assert_eq!(
            refusal(&command(&mut c, &format!("/{verb} {}", missing.display()))),
            format!("no folder {}", missing.display())
        );
    }
    assert!(!missing.exists());
    assert_eq!(
        refusal(&command(&mut c, "/quit")),
        "/quit is the console's own: it closes the console's panel"
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_never_starts_moa() {
    let (rig, up) = typical().await;
    let mut c = up.client();
    let launched = spawn::launches();

    for line in [
        "/ls",
        "/focus \"Rust scaffold\"",
        "/help",
        "/auto",
        "/sleep",
        "/setup",
        "ls",
        "/nope",
        "/resume",
        "/quit",
        "/brief",
        "/attention",
        "/suggestions",
        "/new",
    ] {
        let reply = command(&mut c, line);
        assert!(
            reply.get("result").is_some() || reply.get("error").is_some(),
            "{line}: {reply}"
        );
    }
    assert_eq!(spawn::launches(), launched, "a command launched a process");
    assert!(rig.logged("initialize").is_empty());
    assert!(rig.feed().is_empty(), "{:?}", rig.feed());
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suggestions_lists_as_suggestion_ls() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    assert_eq!(
        outcome(&command(&mut c, "/suggestions")),
        outcome(&c.call("suggestion.ls", json!({})))
    );
    assert_eq!(
        answer(&command(&mut c, "/new")),
        "the next prompt starts a new thread"
    );
    up.stop().await;
}
