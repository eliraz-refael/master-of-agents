//! Talking to moa from the command line, end to end: the real `moa`, autostarted on a
//! TempBrain, with `stub-acp` as the account's ACP adapter. No terminal server is needed.

mod common;

use std::process::{Child, Stdio};

use moa_platform::ensure_private_dir;
use moa_testkit::{StubStep, TempBrain, stub_log, stub_moa_toml, wait_for, write_stub_script};
use serde_json::Value;

use common::*;

/// Names the brain, as a terminal outside any session must (no `moa.toml` account is guessed).
const BRAIN: &[(&str, &str)] = &[("MOA_BRAIN", B)];

fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

fn end() -> StubStep {
    StubStep::End { stop: None }
}

/// A root whose moa runs the stub, with `steps` as its script.
fn rig(steps: &[StubStep]) -> TempBrain {
    let tb = TempBrain::new(Some(&stub_moa_toml()));
    let agent = tb.paths().agent_dir(B);
    ensure_private_dir(&agent).expect("the agent folder");
    write_stub_script(&agent, steps);
    tb
}

/// `moa args…`, which must succeed; its stdout.
#[track_caller]
fn ok(tb: &TempBrain, args: &[&str]) -> String {
    let o = run(tb, args, BRAIN);
    assert!(o.status.success(), "{args:?}: {}{}", out(&o), err(&o));
    out(&o)
}

/// `moa sql query`'s rows, without the header.
fn sql(tb: &TempBrain, query: &str) -> Vec<String> {
    ok(tb, &["sql", query])
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// The stub's log lines for `method`.
fn logged(tb: &TempBrain, method: &str) -> Vec<Value> {
    stub_log(&tb.paths().agent_dir(B))
        .into_iter()
        .filter(|l| l["method"] == method)
        .collect()
}

#[test]
fn moa_answers_from_the_command_line() {
    let answer = "🦀 salut, שלום — it's \"odd\"";
    let tb = rig(&[say(answer, "m1"), end()]);

    assert_eq!(
        ok(&tb, &["console", "prompt", "hello 🦀 שלום"]),
        format!("{answer}\n")
    );

    let feed = ok(&tb, &["console", "feed"]);
    assert!(feed.contains(" you  hello 🦀 שלום\n"), "{feed}");
    assert!(feed.contains(&format!(" moa  {answer}\n")), "{feed}");
    assert_eq!(
        sql(&tb, "SELECT kind FROM feed ORDER BY id"),
        ["you", "moa"]
    );
}

#[test]
fn a_permission_is_refused_and_said() {
    let tb = rig(&[
        StubStep::Ask {
            title: "delete the Harbor".into(),
            options: vec![
                ("yes".into(), "allow_once".into()),
                ("no".into(), "reject_once".into()),
            ],
        },
        say("understood", "m1"),
        end(),
    ]);

    ok(&tb, &["console", "prompt", "tidy up"]);

    let outcome = logged(&tb, "permission.outcome");
    assert_eq!(outcome.len(), 1, "{outcome:?}");
    assert_eq!(outcome[0]["params"]["optionId"], "no", "{outcome:?}");
    let feed = ok(&tb, &["console", "feed"]);
    assert!(
        feed.contains("moa wanted to delete the Harbor, and was refused: moa can't ask you yet"),
        "{feed}"
    );
    assert_eq!(
        sql(&tb, "SELECT kind FROM feed ORDER BY id"),
        ["you", "system", "moa"]
    );
}

#[test]
fn the_thread_survives_a_daemon_restart() {
    let tb = rig(&[say("one", "m1"), end(), say("two", "m2"), end()]);

    assert_eq!(ok(&tb, &["console", "prompt", "first"]), "one\n");
    ok(&tb, &["shutdown"]);
    assert_eq!(ok(&tb, &["console", "prompt", "second"]), "two\n");

    assert_eq!(logged(&tb, "session/resume").len(), 1);
    assert_eq!(logged(&tb, "session/new").len(), 1);
    assert_eq!(sql(&tb, "SELECT count(*) FROM moa_threads"), ["1"]);
}

#[test]
fn a_cancel_from_another_terminal_ends_a_hang() {
    let tb = rig(&[say("thinking", "m1"), StubStep::Hang]);
    let asker: Child = std::process::Command::new(MOA)
        .args(["console", "prompt", "take your time"])
        .env_clear()
        .envs(tb.env())
        .env("MOA_BRAIN", B)
        .current_dir(tb.root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Its first words are saved: the turn has begun and waits for the cancel.
    let begun = wait_for(SHOWS, || {
        (sql_quiet(&tb, "SELECT kind FROM feed ORDER BY id") == ["you", "moa"]).then_some(())
    });
    assert!(
        begun.is_some(),
        "the turn never began\n{}",
        tb.log_tail(B, 40)
    );

    assert_eq!(ok(&tb, &["console", "cancel"]), "cancelling moa's turn\n");

    let o = asker.wait_with_output().unwrap();
    assert_eq!(
        out(&o),
        "moa's turn ended early: cancelled\n",
        "{}",
        err(&o)
    );
}

/// [`sql`] that tolerates a failure (the brain may not exist yet): no rows.
fn sql_quiet(tb: &TempBrain, query: &str) -> Vec<String> {
    let o = run(tb, &["sql", query], BRAIN);
    out(&o).lines().skip(1).map(str::to_string).collect()
}
