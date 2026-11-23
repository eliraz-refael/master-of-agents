use std::path::{Path, PathBuf};

use clap::Parser;
use moa_core::model::SuggestionAction;
use moa_core::op::{Answer, NoParams, Op, TaskAdd, TaskSet};
use moa_proto::cli::{Moactl, VERB_NAMES, VerbCtx, parse_action, parse_command};

fn ctx() -> VerbCtx {
    VerbCtx {
        cwd: Some(PathBuf::from("/h/private/lantern")),
        home: PathBuf::from("/h"),
    }
}

fn parse(argv: &[&str]) -> Moactl {
    let mut v = vec!["moactl"];
    v.extend_from_slice(argv);
    Moactl::try_parse_from(v).unwrap().settled()
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
    &["restart", "x"],
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

/// `set` joins the words after the name with spaces, as the spike did (L3).
#[test]
fn layout_verbs_parse() {
    assert_eq!(op(&["layout", "list"]), Op::LayoutList(NoParams));
    assert_eq!(
        op(&["layout", "show", "cockpit"]),
        Op::LayoutShow {
            name: "cockpit".into()
        }
    );
    assert_eq!(
        op(&["layout", "set", "mine", "{\"panel\":", "\"tree\"}"]),
        Op::LayoutSet {
            name: "mine".into(),
            layout: serde_json::json!({"panel": "tree"}),
        }
    );
    assert_eq!(err(&["layout", "show"]), "usage: moa layout show NAME");
}

/// `layout apply` builds the current layout, or the one named (L3).
#[test]
fn layout_apply_parses() {
    assert_eq!(op(&["layout", "apply"]), Op::LayoutApply { name: None });
    assert_eq!(
        op(&["layout", "apply", "focus"]),
        Op::LayoutApply {
            name: Some("focus".into())
        }
    );
    assert_eq!(
        err(&["layout", "apply", "focus", "x"]),
        "usage: moa layout apply [NAME]"
    );
}

/// `goto` takes one of the nine leaves, and names them when it gets another word.
#[test]
fn goto_parses() {
    use moa_core::layout::Leaf;
    use moa_core::panel::View;
    assert_eq!(
        op(&["goto", "tree"]),
        Op::Goto {
            panel: Leaf::Panel(View::Tree)
        }
    );
    assert_eq!(op(&["goto", "agent"]), Op::Goto { panel: Leaf::Agent });
    assert_eq!(
        err(&["goto", "nope"]),
        "usage: moa goto PANEL (panels: console, tree, today, activity, projects, plan, \
         status, picture, agent)"
    );
    // A view that is no leaf of a layout is no panel to go to.
    assert!(err(&["goto", "empty-slot"]).starts_with("usage: moa goto PANEL"));
}

#[test]
fn later_verbs_say_not_yet() {
    assert_eq!(err(&["peers"]), "moactl peers isn't in this build yet");
    assert_eq!(err(&["setup"]), "moactl setup isn't in this build yet");
}

#[test]
fn open_parses_to_session_open() {
    assert_eq!(
        op(&[
            "open",
            "~/x",
            "--account",
            "acct-a",
            "--title",
            "Boards",
            "--group",
            "Lantern/boards",
            "--prompt",
            "look at the boards",
            "--bg",
        ]),
        Op::SessionOpen {
            path: "/h/x".into(),
            account: Some("acct-a".into()),
            title: Some("Boards".into()),
            group: Some("Lantern/boards".into()),
            prompt: Some("look at the boards".into()),
            bg: true,
        }
    );
    // A relative path is the folder it was typed in.
    assert_eq!(
        op(&["open", "../tidepool"]),
        Op::SessionOpen {
            path: "/h/private/tidepool".into(),
            account: None,
            title: None,
            group: None,
            prompt: None,
            bg: false,
        }
    );
}

#[test]
fn resume_parses_to_session_resume() {
    assert_eq!(
        op(&["resume", "lantern"]),
        Op::SessionResume {
            q: "lantern".into(),
            bg: false
        }
    );
    assert_eq!(
        op(&["resume", "a1b2", "--bg"]),
        Op::SessionResume {
            q: "a1b2".into(),
            bg: true
        }
    );
}

#[test]
fn focus_parses_to_session_focus() {
    assert_eq!(
        op(&["focus", "lantern"]),
        Op::SessionFocus {
            q: "lantern".into()
        }
    );
}

/// `sleep` by name, or by how long sessions have been idle (R20).
#[test]
fn sleep_parses_to_session_sleep() {
    assert_eq!(
        op(&["sleep", "a1b2", "lantern"]),
        Op::SessionSleep {
            qs: vec!["a1b2".into(), "lantern".into()],
            idle_ms: None,
            dry_run: false,
        }
    );
    assert_eq!(
        op(&["sleep", "--idle", "1.5", "--dry-run"]),
        Op::SessionSleep {
            qs: Vec::new(),
            idle_ms: Some(5_400_000),
            dry_run: true,
        }
    );
    assert_eq!(
        op(&["sleep", "--idle", "0"]),
        Op::SessionSleep {
            qs: Vec::new(),
            idle_ms: Some(0),
            dry_run: false,
        }
    );
    let hours = "--idle needs a number of hours, 0 or more";
    for bad in ["-1", "NaN", "inf", "-0.5"] {
        assert_eq!(err(&["sleep", "--idle", bad]), hours, "{bad}");
    }
    let usage = "moactl sleep ID... | moactl sleep --idle HOURS [--dry-run]";
    assert_eq!(err(&["sleep"]), usage);
    assert_eq!(err(&["sleep", "--dry-run"]), usage);
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

fn act(verb: &str, args: &[&str]) -> Result<Op, String> {
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
        Ok(Op::TaskSet(_))
    ));
    assert!(
        act("task", &["1", "--status", "done"])
            .unwrap_err()
            .starts_with("invalid value '1'")
    );
    let wl = "a suggestion can run moactl archive, close, group, note, project, set, task";
    assert_eq!(act("rm", &["-rf", "x"]).unwrap_err(), wl);
    assert_eq!(act("open", &["~/x"]).unwrap_err(), wl);
    assert_eq!(
        act("close", &["3f9a1c52"]),
        Ok(Op::SessionClose {
            q: "3f9a1c52".into()
        })
    );
    assert_eq!(
        act("project", &["X", "--add", "rel"]).unwrap_err(),
        "use a full path in a suggestion"
    );
}

fn cmd(cwd: Option<&str>, verb: &str, args: &[&str]) -> Result<Op, String> {
    parse_command(
        &SuggestionAction {
            verb: verb.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        },
        cwd.map(Path::new),
        Path::new("/h"),
    )
}

/// A console command parses with the same grammar, as the console may: its own verbs, its
/// own words, and a relative path joined to the folder it is given.
#[test]
fn console_commands_parse_with_the_same_grammar() {
    assert_eq!(
        cmd(None, "focus", &["Rust scaffold"]),
        Ok(Op::SessionFocus {
            q: "Rust scaffold".into()
        })
    );
    assert_eq!(
        cmd(Some("/h/private"), "term", &["lantern"]),
        Ok(Op::TermOpen {
            path: PathBuf::from("/h/private/lantern"),
            title: None,
        })
    );
    assert_eq!(
        cmd(None, "term", &["lantern"]).unwrap_err(),
        "use a full path"
    );
    assert_eq!(
        cmd(None, "task", &["ls"]).unwrap_err(),
        "there's no /task command; /help lists them"
    );
    assert_eq!(
        cmd(None, "ls", &["--help"]).unwrap_err(),
        "a console command can't ask for help; /help lists them"
    );
    assert_eq!(
        cmd(None, "resume", &[]).unwrap_err(),
        "the following required arguments were not provided: <ID>"
    );
    assert_eq!(
        act("close", &[]).unwrap_err(),
        "the following required arguments were not provided: <ID>"
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

/// `close` maps to its op, from moactl and from a suggestion alike; `term` opens a shell in
/// the folder it names, else where it was typed; the help says "tab" (R30).
#[test]
fn close_and_term_parse() {
    assert_eq!(op(&["close", "x"]), Op::SessionClose { q: "x".into() });
    assert_eq!(act("close", &["x"]), Ok(Op::SessionClose { q: "x".into() }));
    assert_eq!(
        op(&["term"]),
        Op::TermOpen {
            path: PathBuf::from("/h/private/lantern"),
            title: None
        }
    );
    assert_eq!(
        op(&["term", "../web", "--title", "dev", "--pane", "%3"]),
        Op::TermOpen {
            path: PathBuf::from("/h/private/web"),
            title: Some("dev".into())
        }
    );
    assert_eq!(
        op(&["term", "~"]),
        Op::TermOpen {
            path: PathBuf::from("/h"),
            title: None
        }
    );

    use clap::CommandFactory;
    let help = Moactl::command().render_help().to_string();
    let line = help
        .lines()
        .find(|l| l.trim_start().starts_with("term "))
        .unwrap_or_else(|| panic!("{help}"));
    assert!(line.ends_with("A plain shell in a new tab"), "{line}");
}

/// `term FOLDER` still opens a shell there, with `--title` and `--pane` where they were.
#[test]
fn term_keeps_its_folder_form() {
    let open = |path: &str, title: Option<&str>| Op::TermOpen {
        path: PathBuf::from(path),
        title: title.map(Into::into),
    };
    assert_eq!(op(&["term"]), open("/h/private/lantern", None));
    assert_eq!(op(&["term", "~"]), open("/h", None));
    assert_eq!(op(&["term", "../web"]), open("/h/private/web", None));
    for argv in [
        &["term", "../web", "--title", "dev"][..],
        &["term", "--title", "dev", "../web"],
        &["term", "../web", "--title", "dev", "--pane", "%3"],
        &["term", "--pane", "%3", "--title", "dev", "../web"],
    ] {
        assert_eq!(op(argv), open("/h/private/web", Some("dev")), "{argv:?}");
    }
    assert_eq!(
        op(&["term", "--title", "dev"]),
        open("/h/private/lantern", Some("dev"))
    );
    // More than a folder, and a sub-command with nothing to act on, are usage errors.
    assert!(err(&["term", "a", "b"]).contains("moactl term [FOLDER]"));
}

/// `term ls`, `name` and `note` are sub-commands; the rest of the words are the name or the
/// note, so a note may start with a dash.
#[test]
fn term_ls_and_friends_parse() {
    assert_eq!(op(&["term", "ls"]), Op::TermLs(NoParams));
    assert_eq!(
        op(&["term", "name", "7", "Kite", "server"]),
        Op::TermName {
            target: "7".into(),
            name: "Kite server".into()
        }
    );
    assert_eq!(
        op(&["term", "name", "%12", "Beacon"]),
        Op::TermName {
            target: "%12".into(),
            name: "Beacon".into()
        }
    );
    assert_eq!(
        op(&["term", "note", "#7", "-", "waiting", "on", "--Harbor"]),
        Op::TermNote {
            target: "#7".into(),
            note: "- waiting on --Harbor".into()
        }
    );
    // No words clears a note.
    assert_eq!(
        op(&["term", "note", "7"]),
        Op::TermNote {
            target: "7".into(),
            note: String::new()
        }
    );
    for argv in [
        &["term", "name"][..],
        &["term", "name", "7"],
        &["term", "note"],
        &["term", "ls", "7"],
    ] {
        assert!(err(argv).contains("moactl term [FOLDER]"), "{argv:?}");
    }
    use clap::CommandFactory;
    let help = Moactl::command()
        .find_subcommand_mut("term")
        .unwrap()
        .render_help()
        .to_string();
    for word in [
        "term ls",
        "term name",
        "term note",
        "term close",
        "term timeout",
    ] {
        assert!(help.contains(word), "{word}: {help}");
    }
}

/// The global `--json` still works after `term`'s words; a note keeps it as a word.
#[test]
fn term_json_after_the_words() {
    let p = parse(&["term", "ls", "--json"]);
    assert!(p.json);
    assert_eq!(p.verb.to_op(&ctx()).unwrap(), Op::TermLs(NoParams));
    let p = parse(&["term", "../web", "--json"]);
    assert!(p.json);
    assert_eq!(
        p.verb.to_op(&ctx()).unwrap(),
        Op::TermOpen {
            path: PathBuf::from("/h/private/web"),
            title: None
        }
    );
    let p = parse(&["term", "note", "7", "try", "--json"]);
    assert!(!p.json);
    assert_eq!(
        p.verb.to_op(&ctx()).unwrap(),
        Op::TermNote {
            target: "7".into(),
            note: "try --json".into()
        }
    );
    // `--title` belongs to a new terminal.
    assert!(err(&["term", "--title", "x", "ls"]).contains("moactl term [FOLDER]"));
    assert!(err(&["term", "--title", "x", "name", "7", "y"]).contains("moactl term [FOLDER]"));
}

/// A folder called `ls` is still a folder when it is written as one.
#[test]
fn term_dot_ls_is_a_folder() {
    assert_eq!(
        op(&["term", "./ls"]),
        Op::TermOpen {
            path: PathBuf::from("/h/private/lantern/ls"),
            title: None
        }
    );
    assert_eq!(
        op(&["term", "ls/"]),
        Op::TermOpen {
            path: PathBuf::from("/h/private/lantern/ls"),
            title: None
        }
    );
}

/// `adopt %N` takes a tab's id, its flags pass through as they are, and anything that isn't
/// a tab id is refused before the daemon hears of it; the help says "tab" (R30).
#[test]
fn adopt_parses() {
    assert_eq!(
        op(&["adopt", "%5"]),
        Op::SessionAdopt {
            pane: "%5".into(),
            title: None,
            group: None,
            id: None
        }
    );
    assert_eq!(
        op(&[
            "adopt",
            "%12",
            "--title",
            "boards",
            "--group",
            "Work/Lantern",
            "--id",
            "C9"
        ]),
        Op::SessionAdopt {
            pane: "%12".into(),
            title: Some("boards".into()),
            group: Some("Work/Lantern".into()),
            id: Some("C9".into())
        }
    );
    for bad in ["5", "%", "%x", "%+5", "@5", "x"] {
        assert_eq!(
            err(&["adopt", bad]),
            "adopt needs a tab id like %5",
            "{bad}"
        );
    }
    // Not a suggestion action.
    assert!(act("adopt", &["%5"]).is_err());

    use clap::CommandFactory;
    let help = Moactl::command().render_help().to_string();
    let line = help
        .lines()
        .find(|l| l.trim_start().starts_with("adopt "))
        .unwrap_or_else(|| panic!("{help}"));
    assert!(line.ends_with("Adopt an agent running in a tab"), "{line}");
}

/// `restart Q` maps to its op; it isn't a suggestion action, and the help says "tab" (R30).
#[test]
fn restart_parses() {
    assert_eq!(
        op(&["restart", "3f9a"]),
        Op::SessionRestart { q: "3f9a".into() }
    );
    assert!(act("restart", &["3f9a"]).is_err());

    use clap::CommandFactory;
    let help = Moactl::command().render_help().to_string();
    let line = help
        .lines()
        .find(|l| l.trim_start().starts_with("restart "))
        .unwrap_or_else(|| panic!("{help}"));
    assert!(
        line.ends_with("Restart an adopted agent in a tab moa tracks"),
        "{line}"
    );
}

/// `term close` takes the rest of the words as its terminal; `term timeout` takes the last
/// word as how long, the words before it as the terminal, and `off` clears it.
#[test]
fn term_close_and_timeout_parse() {
    const HOUR: i64 = 3_600_000;
    assert_eq!(
        op(&["term", "close", "7"]),
        Op::TermClose { target: "7".into() }
    );
    assert_eq!(
        op(&["term", "close", "Kite", "server"]),
        Op::TermClose {
            target: "Kite server".into()
        }
    );
    assert_eq!(
        op(&["term", "timeout", "%12", "3h"]),
        Op::TermTimeout {
            target: "%12".into(),
            after_ms: Some(3 * HOUR)
        }
    );
    assert_eq!(
        op(&["term", "timeout", "Kite", "server", "2h30m"]),
        Op::TermTimeout {
            target: "Kite server".into(),
            after_ms: Some(5 * HOUR / 2)
        }
    );
    assert_eq!(
        op(&["term", "timeout", "7", "off"]),
        Op::TermTimeout {
            target: "7".into(),
            after_ms: None
        }
    );
    assert_eq!(
        err(&["term", "timeout", "7", "soon"]),
        "a timeout is like 3h or 90m, up to 7 days"
    );
    for argv in [
        &["term", "close"][..],
        &["term", "timeout"],
        &["term", "timeout", "7"],
        &["term", "--title", "x", "close", "7"],
    ] {
        assert!(err(argv).contains("moactl term [FOLDER]"), "{argv:?}");
    }
    // The global `--json` still works after their words.
    let p = parse(&["term", "close", "7", "--json"]);
    assert!(p.json);
    assert_eq!(
        p.verb.to_op(&ctx()).unwrap(),
        Op::TermClose { target: "7".into() }
    );
    let p = parse(&["term", "timeout", "7", "90m", "--json"]);
    assert!(p.json);
    assert_eq!(
        p.verb.to_op(&ctx()).unwrap(),
        Op::TermTimeout {
            target: "7".into(),
            after_ms: Some(HOUR * 3 / 2)
        }
    );
}
