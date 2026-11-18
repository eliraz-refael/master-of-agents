//! The verb grammar: one clap grammar shared by the `moactl` CLI and by suggestion actions.
//!
//! Pure: parsing and mapping to an [`Op`], no IO. A verb the daemon can't run yet maps to
//! [`VerbError::NotYet`].

use std::fmt;
use std::path::{Component, Path, PathBuf};

use clap::builder::PossibleValuesParser;
use clap::{Parser, Subcommand, ValueEnum};
use moa_core::model::{ACTION_VERBS, SuggestionAction};
use moa_core::op::{ADOPT_USAGE, Answer, NoParams, Op, SLEEP_USAGE, TaskAdd, TaskSet};
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
    Term {
        path: Option<String>,
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
    /// The directory a relative path is relative to; `None` in a suggestion action.
    pub cwd: Option<PathBuf>,
    pub home: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VerbError {
    NotYet(&'static str),
    Usage(String),
}

impl fmt::Display for VerbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerbError::NotYet(v) => write!(f, "moactl {v} isn't in this build yet"),
            VerbError::Usage(u) => f.write_str(u),
        }
    }
}

impl std::error::Error for VerbError {}

const GROUP_USAGE: &str = "moactl group add PATH | group rename OLD NEW | group rm PATH";
const TASK_ADD_USAGE: &str = "moactl task add PROJECT TITLE… [--who you|agent|other] [--due DATE] [--session ID] [--status S]";
const TASK_SET_USAGE: &str = "moactl task set ID [--status S] [--title T] [--who W] [--due D] [--priority P] [--session ID] [--note N]";

fn usage<T>(s: &str) -> Result<T, VerbError> {
    Err(VerbError::Usage(s.to_string()))
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
            None => return usage("use a full path in a suggestion"),
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
            Verb::Term { path, title, .. } => Op::TermOpen {
                path: resolve_path(path.as_deref().unwrap_or("."), ctx)?,
                title: title.clone(),
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
            // Listed one by one, so a new verb can't become "not yet" by default.
            Verb::Goto { .. }
            | Verb::Layout { .. }
            | Verb::Peers
            | Verb::Ask { .. }
            | Verb::Reply { .. }
            | Verb::Asks
            | Verb::Setup { .. }
            | Verb::ImportHerdr => return Err(VerbError::NotYet(self.name())),
        })
    }
}

/// Parse a suggestion's action with the `moactl` grammar. No cwd: paths must be full.
pub fn parse_action(a: &SuggestionAction, home: &Path) -> Result<Op, String> {
    if !ACTION_VERBS.contains(&a.verb.as_str()) {
        let mut verbs = ACTION_VERBS.to_vec();
        verbs.sort_unstable();
        return Err(format!("a suggestion can run moactl {}", verbs.join(", ")));
    }
    let argv = ["moactl", a.verb.as_str()]
        .into_iter()
        .chain(a.args.iter().map(String::as_str));
    let parsed = Moactl::try_parse_from(argv).map_err(|e| {
        // `--help` is an `Err` whose text is the help page; its first line isn't a reason.
        if matches!(
            e.kind(),
            clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                | clap::error::ErrorKind::DisplayVersion
        ) {
            return "a suggestion action can't ask for help".to_string();
        }
        let text = e.render().to_string();
        let first = text.lines().next().unwrap_or("");
        first.strip_prefix("error: ").unwrap_or(first).to_string()
    })?;
    let ctx = VerbCtx {
        cwd: None,
        home: home.to_path_buf(),
    };
    parsed.verb.to_op(&ctx).map_err(|e| e.to_string())
}
