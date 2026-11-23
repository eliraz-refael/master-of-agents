//! The console's button bar: moa sets a list, you override it, and the `feed` topic's `bar` row
//! shows yours, else moa's, else the default. An in-process daemon on a `/tmp` root.

mod common;

use common::Client;
use common::rig::{Rig, Up, refusal};
use moa_core::publish::Topic;
use serde_json::{Value, json};

async fn typical() -> (Rig, Up) {
    let rig = Rig::new(&[]);
    rig.tb.adopt_typical("default");
    let up = rig.start().await;
    (rig, up)
}

/// The published `bar` row.
fn bar(up: &Up) -> Value {
    let model = up.core.model().borrow().clone();
    model.topics[&Topic::Feed]["bar"].clone()
}

fn shown(up: &Up) -> (Vec<String>, String) {
    let row = bar(up);
    let commands = serde_json::from_value(row["commands"].clone()).unwrap();
    (commands, row["by"].as_str().unwrap().to_string())
}

fn set(c: &mut Client, commands: &[&str], user: bool) -> Value {
    c.call("bar.set", json!({ "commands": commands, "user": user }))
}

fn lines(commands: &[&str]) -> Vec<String> {
    commands.iter().map(|c| c.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_bar() {
    let (_rig, up) = typical().await;
    assert_eq!(
        shown(&up),
        (
            lines(&["/brief", "/attention", "/ls", "/help"]),
            "default".into()
        )
    );
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moa_sets_and_you_override() {
    let (_rig, up) = typical().await;
    let mut c = up.client();

    assert!(set(&mut c, &["/ls"], false).get("result").is_some());
    assert_eq!(shown(&up), (lines(&["/ls"]), "moa".into()));

    // `/bar` is yours, and wins.
    let typed = c.call("console.command", json!({ "line": "/bar /brief" }));
    assert!(typed.get("result").is_some(), "{typed}");
    assert_eq!(shown(&up), (lines(&["/brief"]), "you".into()));

    // moa's list moves underneath, and nothing shows.
    set(&mut c, &["/help"], false);
    assert_eq!(shown(&up), (lines(&["/brief"]), "you".into()));

    // A bare `/bar` gives yours up: moa's shows.
    c.call("console.command", json!({ "line": "/bar" }));
    assert_eq!(shown(&up), (lines(&["/help"]), "moa".into()));

    // And moa clearing its own gives the default.
    set(&mut c, &[], false);
    assert_eq!(shown(&up).1, "default");
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_bar_is_refused() {
    let (_rig, up) = typical().await;
    let mut c = up.client();
    let before = bar(&up);

    let nine = ["/ls"; 9];
    assert_eq!(
        refusal(&set(&mut c, &nine, false)),
        "a bar holds at most 8 buttons"
    );
    let long = format!("/focus {}", "x".repeat(34));
    assert_eq!(long.chars().count(), 41);
    assert_eq!(
        refusal(&set(&mut c, &["/ls", &long], false)),
        format!("{long}: a button's command is at most 40 characters")
    );
    assert_eq!(
        refusal(&set(&mut c, &["/ls", "/nope"], true)),
        "/nope: there's no /nope command; /help lists them"
    );
    assert_eq!(
        refusal(&set(&mut c, &["ls"], false)),
        "ls: a command starts with /"
    );
    for (entry, why) in [
        ("/quit", "the console's own"),
        ("/auto", "later version"),
        ("/bar /ls", "can't set the bar"),
    ] {
        assert!(
            refusal(&set(&mut c, &[entry], false)).contains(why),
            "{entry}"
        );
    }
    // The valid extras are taken: `/help` and `/new`.
    assert!(
        set(&mut c, &["/help", "/new"], false)
            .get("result")
            .is_some()
    );
    set(&mut c, &[], false);
    // Nothing the refusals tried got in.
    assert_eq!(bar(&up), before);
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entries_are_trimmed_before_the_length_check_and_stored_trimmed() {
    let (_rig, up) = typical().await;
    let mut c = up.client();
    // 40 characters plus padding: the padding doesn't count.
    let forty = format!("/focus {}", "x".repeat(33));
    assert_eq!(forty.chars().count(), 40);
    let padded = format!("  {forty}  ");
    let told = set(&mut c, &["  /ls ", &padded], false);
    assert!(told.get("result").is_some(), "{told}");
    assert_eq!(shown(&up), (vec!["/ls".to_string(), forty], "moa".into()));
    let cleared = set(&mut c, &[], true);
    assert_eq!(cleared["result"]["text"], "cleared your bar");
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bar_survives_a_restart() {
    let (rig, up) = typical().await;
    let mut c = up.client();
    set(&mut c, &["/ls", "/attention"], false);
    set(&mut c, &["/brief"], true);
    up.stop().await;

    let up = rig.start().await;
    assert_eq!(shown(&up), (lines(&["/brief"]), "you".into()));
    up.client()
        .call("console.command", json!({ "line": "/bar" }));
    assert_eq!(shown(&up), (lines(&["/ls", "/attention"]), "moa".into()));
    up.stop().await;
}
