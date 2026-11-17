use std::path::{Path, PathBuf};

use clap::Parser;
use moa_core::model::SuggestionAction;
use moa_core::op::{Action, Answer, NoParams, Op, TaskAdd, TaskSet};
use moa_proto::cli::{Moactl, VERB_NAMES, VerbCtx, parse_action};

fn ctx() -> VerbCtx {
    VerbCtx {
        cwd: Some(PathBuf::from("/h/private/lantern")),
        home: PathBuf::from("/h"),
    }
}

fn parse(argv: &[&str]) -> Moactl {
    let mut v = vec!["moactl"];
    v.extend_from_slice(argv);
    Moactl::try_parse_from(v).unwrap()
}

fn op(argv: &[&str]) -> Op {
    parse(argv).verb.to_op(&ctx()).unwrap()
}

fn err(argv: &[&str]) -> String {
    match parse(argv).verb.to_op(&ctx()) {
        Err(e) => e.to_string(),
        Ok(o) => panic!("expected an error, got {o:?}"),
    }
}

#[test]
fn spike_style_invocations_parse() {
    assert_eq!(
        op(&[
            "task",
            "add",
            "Lantern",
            "Order",
            "more",
            "boards",
            "--due",
            "fri",
            "--priority",
            "p1"
        ]),
        Op::TaskAdd(TaskAdd {
            project: "Lantern".into(),
            title: "Order more boards".into(),
            due: Some("fri".into()),
            priority: Some("p1".into()),
            ..Default::default()
        })
    );
    assert_eq!(
        op(&[
            "suggest",
            "Close it?",
            "--do",
            "task set 1 --status done",
            "--do",
            "note x"
        ]),
        Op::Suggest {
            text: "Close it?".into(),
            actions: vec!["task set 1 --status done".into(), "note x".into()]
        }
    );
    assert_eq!(
        op(&["task", "set", "12", "--stat", "done"]),
        Op::TaskSet(TaskSet {
            id: 12,
            status: Some("done".into()),
            ..Default::default()
        })
    );
    assert_eq!(
        op(&["task", "set", "3", "--priority", "", "--due", ""]),
        Op::TaskSet(TaskSet {
            id: 3,
            priority: Some("".into()),
            due: Some("".into()),
            ..Default::default()
        })
    );
    for argv in [["--json", "ls", "--all"], ["ls", "--all", "--json"]] {
        let m = parse(&argv);
        assert!(m.json);
        assert_eq!(m.verb.to_op(&ctx()).unwrap(), Op::SessionLs { all: true });
    }
    assert_eq!(
        op(&["set", "3f9a1c52", "--title", "New name"]),
        Op::SessionSet {
            q: "3f9a1c52".into(),
            title: Some("New name".into()),
            group: None
        }
    );
    assert_eq!(
        op(&["project", "Harbour", "--add", "./harbour"]),
        Op::ProjectAdd {
            name: "Harbour".into(),
            path: PathBuf::from("/h/private/lantern/harbour"),
            summary: None
        }
    );
    assert_eq!(
        op(&["project", "Shore", "--add", "~/private"]),
        Op::ProjectAdd {
            name: "Shore".into(),
            path: PathBuf::from("/h/private"),
            summary: None
        }
    );
    assert_eq!(
        op(&["group", "rename", "Private", "Home"]),
        Op::GroupRename {
            old: "Private".into(),
            new: "Home".into()
        }
    );
    assert_eq!(
        op(&["project", "Shore"]),
        Op::ProjectShow {
            name: "Shore".into()
        }
    );
    assert_eq!(
        op(&["archive", "a", "b"]),
        Op::SessionArchive {
            qs: vec!["a".into(), "b".into()]
        }
    );
    assert_eq!(
        op(&["task", "ls", "Lantern", "--all"]),
        Op::TaskLs {
            project: Some("Lantern".into()),
            all: true
        }
    );
    assert_eq!(
        op(&["note", "hi"]),
        Op::NoteAdd {
            text: "hi".into(),
            project: None,
            kind: Some("fact".into())
        }
    );
}

#[test]
fn suggest_and_suggestion_map_to_their_ops() {
    assert_eq!(
        op(&[
            "suggest",
            "Close it?",
            "--do",
            "task set 1 --status done",
            "--do",
            "note x",
        ]),
        Op::Suggest {
            text: "Close it?".into(),
            actions: vec!["task set 1 --status done".into(), "note x".into()]
        }
    );
    assert_eq!(
        op(&["suggest", "Just text"]),
        Op::Suggest {
            text: "Just text".into(),
            actions: vec![]
        }
    );
    for (word, answer) in [
        ("yes", Answer::Yes),
        ("no", Answer::No),
        ("later", Answer::Later),
    ] {
        assert_eq!(
            op(&["suggestion", word, "4"]),
            Op::SuggestionAnswer {
                id: Some(4),
                answer
            }
        );
        assert_eq!(
            op(&["suggestion", word]),
            Op::SuggestionAnswer { id: None, answer }
        );
    }
    assert_eq!(op(&["suggestion", "ls"]), Op::SuggestionLs(NoParams));
}

/// A minimal valid command line for each verb.
const EACH_VERB: &[&[&str]] = &[
    &["ls"],
    &["find", "q"],
    &["set", "x"],
    &["archive", "x"],
    &["group", "add", "A"],
    &["note", "x"],
    &["project", "x"],
    &["task", "ls"],
    &["suggest", "x"],
    &["suggestion", "ls"],
    &["brief"],
    &["attention"],
    &["open", "x"],
    &["resume", "x"],
    &["focus", "x"],
    &["close", "x"],
    &["sleep"],
    &["term"],
    &["adopt", "x"],
    &["goto", "x"],
    &["layout", "list"],
    &["peers"],
    &["ask", "x", "y"],
    &["reply", "1"],
    &["asks"],
    &["setup"],
    &["import-herdr"],
];

#[test]
fn verb_names_match_clap_and_the_enum() {
    use clap::CommandFactory;
    let mut clap_names: Vec<String> = Moactl::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    let mut listed: Vec<String> = VERB_NAMES.iter().map(|s| s.to_string()).collect();
    clap_names.sort();
    listed.sort();
    assert_eq!(listed, clap_names);
    for argv in EACH_VERB {
        assert_eq!(parse(argv).verb.name(), argv[0], "{argv:?}");
    }
    let mut each: Vec<&str> = EACH_VERB.iter().map(|a| a[0]).collect();
    each.sort();
    assert_eq!(each, listed);
}

#[test]
fn later_verbs_say_not_yet() {
    assert_eq!(err(&["open", "~/x"]), "moactl open isn't in this build yet");
    assert_eq!(err(&["peers"]), "moactl peers isn't in this build yet");
    assert_eq!(
        err(&["layout", "list"]),
        "moactl layout isn't in this build yet"
    );
    assert_eq!(err(&["setup"]), "moactl setup isn't in this build yet");
}

#[test]
fn usage_errors_are_the_spikes() {
    assert_eq!(
        err(&["task", "set", "x"]),
        "moactl task set ID [--status S] [--title T] [--who W] [--due D] [--priority P] [--session ID] [--note N]"
    );
    assert_eq!(
        err(&["task", "add", "Lantern"]),
        "moactl task add PROJECT TITLE… [--who you|agent|other] [--due DATE] [--session ID] [--status S]"
    );
    assert_eq!(
        err(&["group", "rename", "A"]),
        "moactl group add PATH | group rename OLD NEW | group rm PATH"
    );
}

fn act(verb: &str, args: &[&str]) -> Result<Action, String> {
    parse_action(
        &SuggestionAction {
            verb: verb.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        },
        Path::new("/h"),
    )
}

#[test]
fn actions_parse_with_the_same_grammar() {
    assert!(matches!(
        act("task", &["set", "1", "--status", "done"]),
        Ok(Action::Op(Op::TaskSet(_)))
    ));
    assert!(
        act("task", &["1", "--status", "done"])
            .unwrap_err()
            .starts_with("invalid value '1'")
    );
    let wl = "a suggestion can run moactl archive, close, group, note, project, set, task";
    assert_eq!(act("rm", &["-rf", "x"]).unwrap_err(), wl);
    assert_eq!(act("open", &["~/x"]).unwrap_err(), wl);
    assert_eq!(act("close", &["3f9a1c52"]), Ok(Action::NotYet("close")));
    assert_eq!(
        act("project", &["X", "--add", "rel"]).unwrap_err(),
        "use a full path in a suggestion"
    );
}

/// T18: `moa <verb>` means `moactl <verb>`, so no brain may be called like one.
#[test]
fn every_verb_and_subcommand_is_reserved() {
    use moa_core::config::RESERVED_BRAIN_NAMES;
    for verb in VERB_NAMES {
        assert!(RESERVED_BRAIN_NAMES.contains(verb), "{verb} isn't reserved");
    }
}
