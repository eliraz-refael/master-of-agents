//! Which tabs run Claude (R9): read from a tab's start command, as tmux kept it, and from
//! the argv and kept environment of the processes in it.

use std::path::Path;

use moa_core::model::{AgentKind, PaneAgent};

/// The last path component of a word: `/opt/bin/claude` is `claude`.
fn last_component(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// A shell-style `NAME=value` word, split.
fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some((name, value))
}

/// The conversation an argv names: `--resume X`, `--resume=X`, `-r X`, `--session-id X` or
/// `--session-id=X`. A flag followed by another flag, or by nothing, names none.
fn conversation(args: &[String]) -> Option<String> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let value = match word.as_str() {
            "--resume" | "-r" | "--session-id" => words.next().map(String::as_str),
            w => w
                .strip_prefix("--resume=")
                .or_else(|| w.strip_prefix("--session-id=")),
        };
        if let Some(v) = value.filter(|v| !v.is_empty() && !v.starts_with('-')) {
            return Some(v.to_string());
        }
    }
    None
}

/// The agent an argv runs, if it is Claude (R9):
/// - argv[0]'s last component is `claude`;
/// - or argv[0] is `node` or `bun`, and argv[1]'s last component is `claude` or it ends in
///   `claude-code/cli.js`.
///
/// Leading `K=V` words are read as environment, after `env` (its `-u NAME` and `-i` options
/// too). `env` fills `moa_session`, `moa_brain`, `store_dir` (`CLAUDE_CONFIG_DIR`), `title`
/// and `group`; an empty value counts as none. `started_at` is left for the caller.
pub fn pane_agent(argv: &[String], env: &[(String, String)]) -> Option<PaneAgent> {
    let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut words = argv;
    loop {
        let (first, rest) = words.split_first()?;
        if let Some((k, v)) = assignment(first) {
            env.retain(|(name, _)| *name != k);
            env.push((k, v));
            words = rest;
        } else if last_component(first) == "env" {
            words = rest;
            loop {
                match words {
                    [i, more @ ..] if i == "-i" => {
                        env.clear();
                        words = more;
                    }
                    [u, name, more @ ..] if u == "-u" => {
                        env.retain(|(k, _)| *k != name.as_str());
                        words = more;
                    }
                    _ => break,
                }
            }
        } else {
            break;
        }
    }
    let (program, args) = words.split_first()?;
    let args = match (last_component(program), args.split_first()) {
        ("claude", _) => args,
        ("node" | "bun", Some((script, rest)))
            if last_component(script) == "claude" || script.ends_with("claude-code/cli.js") =>
        {
            rest
        }
        _ => return None,
    };
    let var = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
    };
    Some(PaneAgent {
        kind: AgentKind::Claude,
        store_dir: var("CLAUDE_CONFIG_DIR"),
        conversation: conversation(args),
        moa_session: var("MOA_SESSION"),
        moa_brain: var("MOA_BRAIN"),
        title: var("MOA_TITLE"),
        group: var("MOA_GROUP"),
        started_at: None,
    })
}

/// One view of a tab from its start command and its running process (R9's order). The
/// session is `MOA_SESSION` from the process's environment; the conversation is the start
/// command's, then the process argv's (Node can retitle a running Claude to plain
/// `claude`). Store, brain, title and group come from the process's environment first, then
/// from `K=V` words in the start command. The start time is the process's.
pub fn merge(start: Option<PaneAgent>, process: Option<PaneAgent>) -> Option<PaneAgent> {
    let (start, process) = match (start, process) {
        (None, None) => return None,
        (Some(one), None) | (None, Some(one)) => return Some(one),
        (Some(start), Some(process)) => (start, process),
    };
    Some(PaneAgent {
        kind: process.kind,
        store_dir: process.store_dir.or(start.store_dir),
        conversation: start.conversation.or(process.conversation),
        moa_session: process.moa_session.or(start.moa_session),
        moa_brain: process.moa_brain.or(start.moa_brain),
        title: process.title.or(start.title),
        group: process.group.or(start.group),
        started_at: process.started_at,
    })
}

/// Claude's project folder name for a cwd: every character that isn't ASCII alphanumeric
/// becomes `-`. `/Users/x/a.b` is `-Users-x-a-b`.
pub fn project_slot(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
