//! The verb grammar: one clap grammar shared by the `moactl` CLI, suggestion actions and the
//! console's `/commands`.
//!
//! Pure: parsing and mapping to an [`Op`], no IO. A verb the daemon can't run yet maps to
//! [`VerbError::NotYet`].

use std::fmt;
use std::path::{Component, Path, PathBuf};

use clap::builder::PossibleValuesParser;
use clap::{Parser, Subcommand, ValueEnum};
use moa_core::console;
use moa_core::layout::{LEAVES, Leaf};
use moa_core::model::{ACTION_VERBS, SuggestionAction};
use moa_core::op::{ADOPT_USAGE, Answer, NoParams, Op, SLEEP_USAGE, TaskAdd, TaskSet};
use moa_core::terminals::parse_after;
use moa_core::tmux::ids::PaneId;

/// What `moactl` parses: a global `--json` and one verb.
#[derive(Parser, Debug)]
#[command(name = "moactl", infer_long_args = true)]
pub struct Moactl {
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub verb: Verb,
}

impl Moactl {
    /// The parse with a `--json` that `term`'s trailing words took in (`term ls --json`,
    /// `term FOLDER --json`) given back to the global flag. A name's or a note's words are
    /// kept as they are: they may hold anything.
    pub fn settled(mut self) -> Moactl {
        if let Verb::Term { args, .. } = &mut self.verb {
            let free_text = matches!(args.first().map(String::as_str), Some("name" | "note"));
            if !free_text && args.iter().any(|a| a == "--json") {
                args.retain(|a| a != "--json");
                self.json = true;
            }
        }
        self
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum GroupOp {
    Add,
    Rename,
    Rm,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TaskOp {
    Add,
    Set,
    Ls,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SuggestionOp {
    Yes,
    No,
    Later,
    Ls,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LayoutOp {
    List,
    Show,
    Apply,
    Set,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SetupOp {
    List,
    Install,
    Only,
    Remove,
    Skip,
}

fn who() -> PossibleValuesParser {
    PossibleValuesParser::new(["you", "agent", "other"])
}

fn priority() -> PossibleValuesParser {
    PossibleValuesParser::new(["p0", "p1", "p2", ""])
}

/// Every verb of `moactl`.
#[derive(Subcommand, Debug)]
pub enum Verb {
    /// List sessions.
    Ls {
        #[arg(long)]
        all: bool,
    },
    /// Find a session.
    Find { q: String },
    /// Set a session's title or group.
    Set {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        group: Option<String>,
    },
    /// Archive sessions.
    Archive {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Add, rename or remove a group.
    Group {
        op: GroupOp,
        #[arg(required = true)]
        args: Vec<String>,
    },
    /// Remember a note.
    Note {
        text: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "fact")]
        kind: String,
    },
    /// Show a project; with --add PATH, make that folder a new project.
    Project {
        name: String,
        #[arg(long, value_name = "PATH")]
        add: Option<String>,
        #[arg(long)]
        summary: Option<String>,
    },
    /// Add, change or list tasks.
    Task {
        op: TaskOp,
        args: Vec<String>,
        #[arg(long)]
        today: bool,
        #[arg(long)]
        not_today: bool,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        blocked_on: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, value_parser = who())]
        who: Option<String>,
        #[arg(long)]
        due: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, value_parser = priority())]
        priority: Option<String>,
    },
    /// Propose something; each --do is a button action.
    Suggest {
        text: String,
        #[arg(long = "do", value_name = "MOACTL-ARGS")]
        actions: Vec<String>,
    },
    /// Answer a suggestion.
    Suggestion { op: SuggestionOp, id: Option<i64> },
    /// The morning brief.
    Brief,
    /// What needs attention.
    Attention,
    /// Open a folder in a new agent session.
    Open {
        path: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        bg: bool,
    },
    /// Resume a session.
    Resume {
        id: String,
        #[arg(long)]
        bg: bool,
    },
    /// Focus a session.
    Focus { q: String },
    /// Close a session's agent.
    Close { id: String },
    /// Put agents to sleep to free memory.
    Sleep {
        ids: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        // A negative number reaches `to_op`, which says what is wrong with it.
        #[arg(long, value_name = "HOURS", allow_negative_numbers = true)]
        idle: Option<f64>,
    },
    /// A plain shell in a new tab.
    #[command(override_usage = TERM_USAGE)]
    Term {
        /// `[FOLDER]`, or `ls`, `name`, `note`, `close` or `timeout` and what it takes.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        pane: Option<String>,
    },
    /// Adopt an agent running in a tab.
    Adopt {
        pane: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        id: Option<String>,
    },
    /// Restart an adopted agent in a tab moa tracks.
    Restart { id: String },
    /// Go to a panel.
    Goto { panel: String },
    /// List, show, apply or set layouts.
    Layout { op: LayoutOp, args: Vec<String> },
    /// The agents you can ask.
    Peers,
    /// Ask another agent and wait for the reply.
    Ask {
        to: String,
        text: String,
        #[arg(long, default_value_t = 540)]
        timeout: i64,
    },
    /// Wait again for the reply to an ask.
    Reply {
        id: i64,
        #[arg(long, default_value_t = 540)]
        timeout: i64,
    },
    /// Recent asks between agents.
    Asks,
    /// Install moa's tracking hook in your accounts.
    Setup {
        op: Option<SetupOp>,
        name: Option<String>,
        folders: Vec<String>,
        #[arg(long)]
        brain: Option<String>,
        #[arg(long = "except", num_args = 1.., value_name = "FOLDER")]
        exceptions: Vec<String>,
    },
    /// Import herdr's state.
    ImportHerdr,
}

/// Every verb name, for `moa <verb>` dispatch.
pub const VERB_NAMES: &[&str] = &[
    "ls",
    "find",
    "set",
    "archive",
    "group",
    "note",
    "project",
    "task",
    "suggest",
    "suggestion",
    "brief",
    "attention",
    "open",
    "resume",
    "focus",
    "close",
    "sleep",
    "term",
    "adopt",
    "restart",
    "goto",
    "layout",
    "peers",
    "ask",
    "reply",
    "asks",
    "setup",
    "import-herdr",
];

/// What turning a verb into an op needs to know about where it was typed.
pub struct VerbCtx {
    /// The directory a relative path is relative to; `None` in a suggestion action, and for a
    /// console command with no folder to give.
    pub cwd: Option<PathBuf>,
    pub home: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VerbError {
    NotYet(&'static str),
    Usage(String),
    /// A relative path, with no folder to join it to.
    NotFull,
}

impl fmt::Display for VerbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerbError::NotYet(v) => write!(f, "moactl {v} isn't in this build yet"),
            VerbError::Usage(u) => f.write_str(u),
            VerbError::NotFull => f.write_str("use a full path"),
        }
    }
}

impl std::error::Error for VerbError {}

const GROUP_USAGE: &str = "moactl group add PATH | group rename OLD NEW | group rm PATH";
const TASK_ADD_USAGE: &str = "moactl task add PROJECT TITLE… [--who you|agent|other] [--due DATE] [--session ID] [--status S]";
const TASK_SET_USAGE: &str = "moactl task set ID [--status S] [--title T] [--who W] [--due D] [--priority P] [--session ID] [--note N]";

const TERM_USAGE: &str = "moactl term [FOLDER] [--title T] | term ls | term name N NAME… | term note N NOTE… | term close N… | term timeout N DURATION";

fn usage<T>(s: &str) -> Result<T, VerbError> {
    Err(VerbError::Usage(s.to_string()))
}

/// The words of `term FOLDER` with its `--title` and `--pane` taken out, and the title (the
/// parsed one, unless the words name another). `--pane` is ignored, as it always was.
fn term_flags<'a>(args: &'a [String], parsed: &Option<String>) -> (Vec<&'a str>, Option<String>) {
    let mut title = parsed.clone();
    let mut words = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
            "--title" => title = args.next().map(str::to_string).or(title),
            "--pane" => {
                args.next();
            }
            _ => match arg.strip_prefix("--title=") {
                Some(t) => title = Some(t.to_string()),
                None if arg.starts_with("--pane=") => {}
                None => words.push(arg),
            },
        }
    }
    (words, title)
}

/// `term [FOLDER]`: a shell in `folder`, else where it was typed.
fn term_open(folder: Option<&str>, title: &Option<String>, ctx: &VerbCtx) -> Result<Op, VerbError> {
    Ok(Op::TermOpen {
        path: resolve_path(folder.unwrap_or("."), ctx)?,
        title: title.clone(),
    })
}

/// `~` and `~/x` expand with home; a relative path joins cwd; `.` and `..` are resolved
/// lexically.
fn resolve_path(raw: &str, ctx: &VerbCtx) -> Result<PathBuf, VerbError> {
    let full = if raw == "~" {
        ctx.home.clone()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        ctx.home.join(rest)
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        match &ctx.cwd {
            Some(cwd) => cwd.join(raw),
            None => return Err(VerbError::NotFull),
        }
    };
    let mut out = PathBuf::new();
    for c in full.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

impl Verb {
    /// The verb as typed.
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Ls { .. } => "ls",
            Verb::Find { .. } => "find",
            Verb::Set { .. } => "set",
            Verb::Archive { .. } => "archive",
            Verb::Group { .. } => "group",
            Verb::Note { .. } => "note",
            Verb::Project { .. } => "project",
            Verb::Task { .. } => "task",
            Verb::Suggest { .. } => "suggest",
            Verb::Suggestion { .. } => "suggestion",
            Verb::Brief => "brief",
            Verb::Attention => "attention",
            Verb::Open { .. } => "open",
            Verb::Resume { .. } => "resume",
            Verb::Focus { .. } => "focus",
            Verb::Close { .. } => "close",
            Verb::Sleep { .. } => "sleep",
            Verb::Term { .. } => "term",
            Verb::Adopt { .. } => "adopt",
            Verb::Restart { .. } => "restart",
            Verb::Goto { .. } => "goto",
            Verb::Layout { .. } => "layout",
            Verb::Peers => "peers",
            Verb::Ask { .. } => "ask",
            Verb::Reply { .. } => "reply",
            Verb::Asks => "asks",
            Verb::Setup { .. } => "setup",
            Verb::ImportHerdr => "import-herdr",
        }
    }

    /// The op this verb asks the core to run.
    pub fn to_op(&self, ctx: &VerbCtx) -> Result<Op, VerbError> {
        Ok(match self {
            Verb::Ls { all } => Op::SessionLs { all: *all },
            Verb::Find { q } => Op::SessionFind { q: q.clone() },
            Verb::Set { id, title, group } => Op::SessionSet {
                q: id.clone(),
                title: title.clone(),
                group: group.clone(),
            },
            Verb::Archive { ids } => Op::SessionArchive { qs: ids.clone() },
            Verb::Group { op, args } => match (op, args.as_slice()) {
                (GroupOp::Add, [p]) => Op::GroupAdd { path: p.clone() },
                (GroupOp::Rename, [old, new]) => Op::GroupRename {
                    old: old.clone(),
                    new: new.clone(),
                },
                (GroupOp::Rm, [p]) => Op::GroupRm { path: p.clone() },
                _ => return usage(GROUP_USAGE),
            },
            Verb::Note {
                text,
                project,
                kind,
            } => Op::NoteAdd {
                text: text.clone(),
                project: project.clone(),
                kind: Some(kind.clone()),
            },
            Verb::Project {
                name,
                add: Some(path),
                summary,
            } => Op::ProjectAdd {
                name: name.clone(),
                path: resolve_path(path, ctx)?,
                summary: summary.clone(),
            },
            Verb::Project { name, .. } => Op::ProjectShow { name: name.clone() },
            Verb::Task {
                op,
                args,
                today,
                not_today,
                status,
                blocked_on,
                title,
                who,
                due,
                session,
                note,
                all,
                priority,
            } => match op {
                TaskOp::Ls => Op::TaskLs {
                    project: args.first().cloned(),
                    all: *all,
                },
                TaskOp::Add => {
                    if args.len() < 2 {
                        return usage(TASK_ADD_USAGE);
                    }
                    Op::TaskAdd(TaskAdd {
                        project: args[0].clone(),
                        title: args[1..].join(" "),
                        today: *today,
                        status: status.clone(),
                        blocked_on: blocked_on.clone(),
                        who: who.clone(),
                        due: due.clone(),
                        session: session.clone(),
                        note: note.clone(),
                        priority: priority.clone(),
                    })
                }
                TaskOp::Set => {
                    let Some(id) = args.first().and_then(|a| a.parse::<i64>().ok()) else {
                        return usage(TASK_SET_USAGE);
                    };
                    Op::TaskSet(TaskSet {
                        id,
                        status: status.clone(),
                        title: title.clone(),
                        who: who.clone(),
                        due: due.clone(),
                        priority: priority.clone(),
                        session: session.clone(),
                        note: note.clone(),
                        blocked_on: blocked_on.clone(),
                        today: *today,
                        not_today: *not_today,
                    })
                }
            },
            Verb::Brief => Op::Brief(NoParams),
            Verb::Attention => Op::Attention(NoParams),
            Verb::Suggest { text, actions } => Op::Suggest {
                text: text.clone(),
                actions: actions.clone(),
            },
            Verb::Suggestion { op, id } => match op {
                SuggestionOp::Yes => Op::SuggestionAnswer {
                    id: *id,
                    answer: Answer::Yes,
                },
                SuggestionOp::No => Op::SuggestionAnswer {
                    id: *id,
                    answer: Answer::No,
                },
                SuggestionOp::Later => Op::SuggestionAnswer {
                    id: *id,
                    answer: Answer::Later,
                },
                SuggestionOp::Ls => Op::SuggestionLs(NoParams),
            },
            Verb::Open {
                path,
                account,
                title,
                group,
                prompt,
                bg,
            } => Op::SessionOpen {
                path: resolve_path(path, ctx)?,
                account: account.clone(),
                title: title.clone(),
                group: group.clone(),
                prompt: prompt.clone(),
                bg: *bg,
            },
            Verb::Focus { q } => Op::SessionFocus { q: q.clone() },
            Verb::Resume { id, bg } => Op::SessionResume {
                q: id.clone(),
                bg: *bg,
            },
            Verb::Close { id } => Op::SessionClose { q: id.clone() },
            Verb::Sleep { ids, dry_run, idle } => {
                let idle_ms = match idle {
                    Some(h) if !h.is_finite() || *h < 0.0 => {
                        return usage("--idle needs a number of hours, 0 or more");
                    }
                    // `as` saturates: an absurd number of hours matches nothing.
                    Some(h) => Some((h * 3_600_000.0).round() as i64),
                    None => None,
                };
                if ids.is_empty() && idle_ms.is_none() {
                    return usage(SLEEP_USAGE);
                }
                Op::SessionSleep {
                    qs: ids.clone(),
                    idle_ms,
                    dry_run: *dry_run,
                }
            }
            // `--pane` is for slice 5's "here"; until then a terminal opens where it was
            // typed.
            Verb::Term { args, title, .. } => match args.split_first() {
                None => term_open(None, title, ctx)?,
                // `--title` names a new terminal; a sub-command has no use for it.
                Some((word, _))
                    if title.is_some()
                        && matches!(
                            word.as_str(),
                            "ls" | "name" | "note" | "close" | "timeout"
                        ) =>
                {
                    return usage(TERM_USAGE);
                }
                Some((word, rest)) => match word.as_str() {
                    "ls" if rest.is_empty() => Op::TermLs(NoParams),
                    "name" | "note" => match rest.split_first() {
                        Some((target, words)) if word == "note" => Op::TermNote {
                            target: target.clone(),
                            note: words.join(" "),
                        },
                        Some((target, words)) if !words.is_empty() => Op::TermName {
                            target: target.clone(),
                            name: words.join(" "),
                        },
                        _ => return usage(TERM_USAGE),
                    },
                    "close" if !rest.is_empty() => Op::TermClose {
                        target: rest.join(" "),
                    },
                    // The last word is how long; the words before it, the terminal.
                    "timeout" => match rest.split_last() {
                        Some((after, target)) if !target.is_empty() => Op::TermTimeout {
                            target: target.join(" "),
                            after_ms: parse_after(after).map_err(VerbError::Usage)?,
                        },
                        _ => return usage(TERM_USAGE),
                    },
                    "ls" | "close" => return usage(TERM_USAGE),
                    // The words after a folder may carry its flags: the trailing arguments
                    // took them in, so a note can hold anything.
                    _ => {
                        let (words, title) = term_flags(args, title);
                        match words[..] {
                            [folder] => term_open(Some(folder), &title, ctx)?,
                            _ => return usage(TERM_USAGE),
                        }
                    }
                },
            },
            Verb::Adopt {
                pane,
                title,
                group,
                id,
            } => {
                if pane.parse::<PaneId>().is_err() {
                    return usage(ADOPT_USAGE);
                }
                Op::SessionAdopt {
                    pane: pane.clone(),
                    title: title.clone(),
                    group: group.clone(),
                    id: id.clone(),
                }
            }
            Verb::Restart { id } => Op::SessionRestart { q: id.clone() },
            // `set` joins the words after the name with spaces, as the spike did.
            Verb::Layout { op, args } => match (op, args.as_slice()) {
                (LayoutOp::List, []) => Op::LayoutList(NoParams),
                (LayoutOp::Show, [name]) => Op::LayoutShow { name: name.clone() },
                (LayoutOp::Set, [name, json @ ..]) if !json.is_empty() => Op::LayoutSet {
                    name: name.clone(),
                    layout: serde_json::from_str(&json.join(" "))
                        .map_err(|e| VerbError::Usage(format!("layout: {e}")))?,
                },
                (LayoutOp::Apply, []) => Op::LayoutApply { name: None },
                (LayoutOp::Apply, [name]) => Op::LayoutApply {
                    name: Some(name.clone()),
                },
                (LayoutOp::List, _) => return usage("usage: moa layout list"),
                (LayoutOp::Show, _) => return usage("usage: moa layout show NAME"),
                (LayoutOp::Set, _) => return usage("usage: moa layout set NAME JSON"),
                (LayoutOp::Apply, _) => return usage("usage: moa layout apply [NAME]"),
            },
            Verb::Goto { panel } => Op::Goto {
                panel: panel.parse::<Leaf>().map_err(|_| {
                    VerbError::Usage(format!(
                        "usage: moa goto PANEL (panels: {})",
                        LEAVES.join(", ")
                    ))
                })?,
            },
            // Listed one by one, so a new verb can't become "not yet" by default.
            Verb::Peers
            | Verb::Ask { .. }
            | Verb::Reply { .. }
            | Verb::Asks
            | Verb::Setup { .. }
            | Verb::ImportHerdr => return Err(VerbError::NotYet(self.name())),
        })
    }
}

/// Where a verb line comes from, and so which verbs it may run and how it is told no. Every
/// source parses with the same `moactl` grammar.
struct Source {
    /// The verbs it may run.
    verbs: &'static [&'static str],
    /// What any other verb is told, given that verb.
    other_verb: fn(&str) -> String,
    /// What `--help` is told: help is a page, not a reason.
    no_help: &'static str,
    /// What a relative path is told when there is no folder to join it to.
    not_full: &'static str,
}

/// A suggestion's action.
const SUGGESTION: Source = Source {
    verbs: ACTION_VERBS,
    other_verb: |_| {
        let mut verbs = ACTION_VERBS.to_vec();
        verbs.sort_unstable();
        format!("a suggestion can run moactl {}", verbs.join(", "))
    },
    no_help: "a suggestion action can't ask for help",
    not_full: "use a full path in a suggestion",
};

/// A console `/command` that runs a verb.
const CONSOLE: Source = Source {
    verbs: console::VERBS,
    other_verb: console::unknown,
    no_help: "a console command can't ask for help; /help lists them",
    not_full: "use a full path",
};

/// Parses one verb and its words with the `moactl` grammar, as `from` may: the op, or why
/// not in one line (clap's first line, for a line clap refuses).
fn parse_verb(a: &SuggestionAction, ctx: &VerbCtx, from: &Source) -> Result<Op, String> {
    if !from.verbs.contains(&a.verb.as_str()) {
        return Err((from.other_verb)(&a.verb));
    }
    let argv = ["moactl", a.verb.as_str()]
        .into_iter()
        .chain(a.args.iter().map(String::as_str));
    let parsed = Moactl::try_parse_from(argv)
        .map(Moactl::settled)
        .map_err(|e| {
            // `--help` is an `Err` whose text is the help page; its first line isn't a reason.
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | clap::error::ErrorKind::DisplayVersion
            ) {
                return from.no_help.to_string();
            }
            let text = e.render().to_string();
            let mut lines = text.lines();
            let first = lines.next().unwrap_or("");
            let first = first.strip_prefix("error: ").unwrap_or(first);
            // A first line that ends in a colon names nothing: what it is about follows, one
            // per line, until a blank one (the missing `<ID>`).
            if first.ends_with(':') {
                let what: Vec<&str> = lines.map(str::trim).take_while(|l| !l.is_empty()).collect();
                if !what.is_empty() {
                    return format!("{first} {}", what.join(" "));
                }
            }
            first.to_string()
        })?;
    parsed.verb.to_op(ctx).map_err(|e| match e {
        VerbError::NotFull => from.not_full.to_string(),
        e => e.to_string(),
    })
}

/// Parse a suggestion's action with the `moactl` grammar. No cwd: paths must be full.
pub fn parse_action(a: &SuggestionAction, home: &Path) -> Result<Op, String> {
    let ctx = VerbCtx {
        cwd: None,
        home: home.to_path_buf(),
    };
    parse_verb(a, &ctx, &SUGGESTION)
}

/// Parse a console command's verb with the `moactl` grammar; a relative path is relative to
/// `cwd`, and refused without one.
pub fn parse_command(a: &SuggestionAction, cwd: Option<&Path>, home: &Path) -> Result<Op, String> {
    let ctx = VerbCtx {
        cwd: cwd.map(Path::to_path_buf),
        home: home.to_path_buf(),
    };
    parse_verb(a, &ctx, &CONSOLE)
}
