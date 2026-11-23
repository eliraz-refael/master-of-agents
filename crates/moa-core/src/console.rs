//! The console's `/commands`: the list `/help` shows (and the palette and the buttons read),
//! and what a typed line runs. One grammar with `moactl`: a command that runs a verb is
//! parsed by the same parser as a suggestion's actions (`Deps.parse_command`), and none of
//! them is a turn of moa's.

use std::path::Path;

use serde_json::json;

use crate::model::SuggestionAction;
use crate::op::{Done, NoParams, Op};
use crate::ops::suggestions::split;
use crate::state::Deps;

/// One console command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Command {
    /// Its name, without the `/`.
    pub name: &'static str,
    /// One line on what it takes and does.
    pub help: &'static str,
    /// Runs, but `/help` and the palette leave it out.
    pub hidden: bool,
}

const fn shown(name: &'static str, help: &'static str) -> Command {
    Command {
        name,
        help,
        hidden: false,
    }
}

/// Every console command, in the order `/help` lists them.
pub const COMMANDS: &[Command] = &[
    shown("attention", "what needs you, ranked"),
    shown("brief", "the morning brief"),
    shown("ls", "list sessions (--all for the ended ones too)"),
    shown("open", "PATH: open a folder in a new agent session"),
    shown("resume", "SESSION: resume a session"),
    shown("focus", "SESSION: bring a session's tab to the screen"),
    shown("term", "[FOLDER]: a plain shell in a new tab"),
    shown(
        "layout",
        "list | show NAME | apply [NAME] | set NAME JSON: layouts",
    ),
    shown("close", "SESSION: close a session's agent"),
    shown("archive", "SESSION…: archive sessions"),
    shown(
        "bar",
        "[COMMAND…]: set the button bar; none puts moa's back",
    ),
    shown("new", "the next prompt starts a new thread"),
    shown("auto", "moa reads reports on its own (later)"),
    shown("sleep", "idle sessions go to sleep (later)"),
    shown("suggestions", "moa's open suggestions"),
    shown("setup", "track your accounts' sessions (later)"),
    shown("help", "these commands"),
    shown("quit", "close the console"),
    Command {
        name: "suggestion",
        help: "yes|no|later ID: answer a suggestion",
        hidden: true,
    },
];

/// The `moactl` verbs a console command may run.
pub const VERBS: &[&str] = &[
    "ls",
    "brief",
    "attention",
    "open",
    "resume",
    "focus",
    "term",
    "layout",
    "close",
    "archive",
    "suggestion",
];

/// Commands that come in a later version of moa.
const LATER: &[&str] = &["auto", "sleep", "setup"];

/// What a command that isn't one is told.
pub fn unknown(name: &str) -> String {
    format!("there's no /{name} command; /help lists them")
}

/// What a console line comes to: an op to run as if it had been sent, or the command list.
/// Short-lived: it is matched as soon as it is made.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Debug)]
pub enum Parsed {
    Run(Op),
    Help,
}

/// Parses a `/command` line without running it. A relative path is relative to `cwd`, else
/// home.
pub fn parse(line: &str, cwd: Option<&Path>, deps: &Deps) -> Result<Parsed, String> {
    parse_with(line, cwd, deps, false)
}

/// Checks `line` as a button's command without running it. A button is pressed later, from
/// wherever the console is, so its folder must be a full path, and whether the folder is
/// there is checked when it's pressed.
pub fn parse_button(line: &str, deps: &Deps) -> Result<Parsed, String> {
    parse_with(line, None, deps, true)
}

fn parse_with(line: &str, cwd: Option<&Path>, deps: &Deps, button: bool) -> Result<Parsed, String> {
    let Some(rest) = line.trim().strip_prefix('/') else {
        return Err("a command starts with /".into());
    };
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        return Err("a command's name goes right after the /; /help lists them".into());
    }
    let SuggestionAction { verb: name, args } = split(rest)?;
    if !COMMANDS.iter().any(|c| c.name == name) {
        return Err(unknown(&name));
    }
    match name.as_str() {
        "help" | "new" if !args.is_empty() => Err(format!("/{name} takes nothing after it")),
        "help" => Ok(Parsed::Help),
        "new" => Ok(Parsed::Run(Op::ConsoleNew(NoParams))),
        // The op checks each entry, as it does for `moactl bar`.
        "bar" => Ok(Parsed::Run(Op::BarSet {
            commands: args,
            user: true,
        })),
        // The console closes its own panel; a `/quit` sent here has nothing to close.
        "quit" => Err("/quit is the console's own: it closes the console's panel".into()),
        n if LATER.contains(&n) => Err(format!("/{n} comes in a later version of moa")),
        _ => {
            let Some(grammar) = deps.parse_command else {
                return Err("console commands need the moactl grammar".into());
            };
            let verb = match name.as_str() {
                "suggestions" => SuggestionAction {
                    verb: "suggestion".into(),
                    args: std::iter::once("ls".to_string()).chain(args).collect(),
                },
                _ => SuggestionAction { verb: name, args },
            };
            if button {
                return grammar(&verb, None, &deps.home).map(Parsed::Run);
            }
            let home = Some(deps.home.as_path()).filter(|h| !h.as_os_str().is_empty());
            let op = grammar(&verb, cwd.or(home), &deps.home)?;
            folder_checked(op, deps).map(Parsed::Run)
        }
    }
}

/// The folder `/open` or `/term` names is there, spelled as the system spells it, as
/// `moactl` has it before it sends either. The daemon never makes a folder (`moactl open`
/// does).
fn folder_checked(mut op: Op, deps: &Deps) -> Result<Op, String> {
    if let Op::SessionOpen { path, .. } | Op::TermOpen { path, .. } = &mut op {
        if !(deps.is_dir)(path) {
            return Err(format!("no folder {}", path.display()));
        }
        *path = (deps.canonical)(path);
    }
    Ok(op)
}

/// `/help`'s reply: every command that isn't hidden, one Markdown bullet each.
pub fn help() -> Done {
    let listed = COMMANDS.iter().filter(|c| !c.hidden);
    let text = listed
        .clone()
        .map(|c| format!("- `/{}` {}", c.name, c.help))
        .collect::<Vec<_>>()
        .join("\n");
    let names: Vec<&str> = listed.map(|c| c.name).collect();
    Done {
        text,
        data: json!({ "commands": names }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_has_its_command() {
        for v in VERBS {
            assert!(COMMANDS.iter().any(|c| c.name == *v), "{v}");
        }
    }

    #[test]
    fn only_suggestion_is_hidden() {
        let hidden: Vec<_> = COMMANDS
            .iter()
            .filter(|c| c.hidden)
            .map(|c| c.name)
            .collect();
        assert_eq!(hidden, ["suggestion"]);
    }

    #[test]
    fn lines_that_arent_commands_are_refused_before_the_grammar() {
        let deps = Deps::default();
        let no = |line: &str| parse(line, None, &deps).unwrap_err();
        assert_eq!(no("ls"), "a command starts with /");
        assert_eq!(no("  "), "a command starts with /");
        assert_eq!(no("/nope"), "there's no /nope command; /help lists them");
        assert_eq!(no("/auto on"), "/auto comes in a later version of moa");
        assert_eq!(no("/new now"), "/new takes nothing after it");
        assert_eq!(
            no("/ ls"),
            "a command's name goes right after the /; /help lists them"
        );
        assert_eq!(no("/focus \"Rust"), "missing closing quote");
        // Without the grammar, a verb can't be parsed.
        assert_eq!(no("/ls"), "console commands need the moactl grammar");
        assert_eq!(parse(" /help ", None, &deps), Ok(Parsed::Help));
        assert_eq!(
            parse("/bar /ls \"/focus x\"", None, &deps),
            Ok(Parsed::Run(Op::BarSet {
                commands: vec!["/ls".into(), "/focus x".into()],
                user: true
            }))
        );
        assert_eq!(
            parse("/new", None, &deps),
            Ok(Parsed::Run(Op::ConsoleNew(NoParams)))
        );
    }

    /// A grammar that answers with the verb, its words and the folder it was given.
    fn echo(a: &SuggestionAction, cwd: Option<&Path>, _home: &Path) -> Result<Op, String> {
        Ok(Op::SessionFind {
            q: format!("{} {:?} {:?}", a.verb, a.args, cwd),
        })
    }

    fn echoed(line: &str, cwd: Option<&Path>, home: &str) -> String {
        let deps = Deps {
            parse_command: Some(echo),
            home: home.into(),
            ..Deps::default()
        };
        match parse(line, cwd, &deps) {
            Ok(Parsed::Run(Op::SessionFind { q })) => q,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn a_path_is_relative_to_the_callers_folder_else_home() {
        let here = Path::new("/w/lantern");
        assert_eq!(
            echoed("/term", Some(here), "/h"),
            r#"term [] Some("/w/lantern")"#
        );
        assert_eq!(echoed("/term", None, "/h"), r#"term [] Some("/h")"#);
        assert_eq!(echoed("/term", None, ""), "term [] None");
        // A quoted title is one word; `/suggestions` is `suggestion ls`.
        assert_eq!(
            echoed("/focus \"Rust scaffold\"", None, "/h"),
            r#"focus ["Rust scaffold"] Some("/h")"#
        );
        assert_eq!(
            echoed("/suggestions", None, "/h"),
            r#"suggestion ["ls"] Some("/h")"#
        );
    }
}
