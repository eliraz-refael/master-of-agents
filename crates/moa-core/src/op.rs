//! What a client can ask the core to do, and what it gets back.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::central::wire::PermKind;
use crate::layout::Leaf;
use crate::panel::View;

/// A client connection, as the daemon numbers them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ConnId(pub u64);

/// A request on a connection.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ReqId(pub u64);

/// One verb with its arguments. On the wire: `{"method": "group.add", "params": {...}}`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "method", content = "params")]
pub enum Op {
    #[serde(rename = "group.add")]
    GroupAdd { path: String },
    #[serde(rename = "group.rename")]
    GroupRename { old: String, new: String },
    #[serde(rename = "group.rm")]
    GroupRm { path: String },
    /// Folds or unfolds one tree path. Saved as one value; never a group.
    #[serde(rename = "group.fold")]
    GroupFold { path: String, collapsed: bool },
    #[serde(rename = "session.ls")]
    SessionLs {
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "session.set")]
    SessionSet {
        q: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        group: Option<String>,
    },
    #[serde(rename = "session.archive")]
    SessionArchive { qs: Vec<String> },
    #[serde(rename = "session.find")]
    SessionFind { q: String },
    /// A fresh session in a new tab of moa's terminal. `path` is absolute; the reply comes
    /// once tmux says which pane the tab has. The tab comes to the screen unless `bg`.
    #[serde(rename = "session.open")]
    SessionOpen {
        path: PathBuf,
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        bg: bool,
    },
    /// Brings a session's tab to the screen. A write: a session not running is resumed.
    #[serde(rename = "session.focus")]
    SessionFocus { q: String },
    /// Relaunches a session's conversation in a new tab, under the account whose store it
    /// was recorded in (C11); one that runs is only brought to the screen. The tab comes to
    /// the screen unless `bg`.
    #[serde(rename = "session.resume")]
    SessionResume {
        q: String,
        #[serde(default)]
        bg: bool,
    },
    /// Ends a session and kills its tab, once moa can prove the tab is the session's.
    #[serde(rename = "session.close")]
    SessionClose { q: String },
    /// Puts sessions to sleep: their tabs close and they stay resumable. By name (`qs`), or
    /// every one idle `idle_ms` that could sleep on its own. A `dry_run` only says which.
    #[serde(rename = "session.sleep")]
    SessionSleep {
        #[serde(default)]
        qs: Vec<String>,
        #[serde(default)]
        idle_ms: Option<i64>,
        #[serde(default)]
        dry_run: bool,
    },
    /// Takes on the agent running in tab `pane` (`%N`), started outside moa, as a session moa
    /// doesn't track: it shows alive or not, and never sleeps on its own. Its conversation is
    /// `id`, else the one its command names, else the newest it wrote.
    #[serde(rename = "session.adopt")]
    SessionAdopt {
        pane: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        id: Option<String>,
    },
    /// Restarts an adopted session under moa: its tab is closed and its conversation resumed
    /// in a new tab moa tracks.
    #[serde(rename = "session.restart")]
    SessionRestart { q: String },
    /// A plain shell in a new tab of moa's terminal, in folder `path` (absolute). The tab is
    /// named after `title`, else the folder.
    #[serde(rename = "term.open")]
    TermOpen {
        path: PathBuf,
        #[serde(default)]
        title: Option<String>,
    },
    /// Brings terminal `pane` (`%N`), a tab `term.open` or `Ctrl-a $` opened, to the screen.
    #[serde(rename = "term.show")]
    TermShow { pane: String },
    /// The open terminals, from one fresh listing of the tabs.
    #[serde(rename = "term.ls")]
    TermLs(NoParams),
    /// Names terminal `target` (its number `7` or `#7`, its tab `%12`, or its name) `name`.
    #[serde(rename = "term.name")]
    TermName { target: String, name: String },
    /// Notes terminal `target` with `note`; an empty one clears it.
    #[serde(rename = "term.note")]
    TermNote { target: String, note: String },
    /// Closes terminal `target`, gently: a program running there gets Ctrl-c first, and the
    /// terminal closes 3 seconds later.
    #[serde(rename = "term.close")]
    TermClose { target: String },
    /// Closes terminal `target` `after_ms` from now; none clears its timeout.
    #[serde(rename = "term.timeout")]
    TermTimeout {
        target: String,
        #[serde(default)]
        after_ms: Option<i64>,
    },
    #[serde(rename = "task.ls")]
    TaskLs {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "task.add")]
    TaskAdd(TaskAdd),
    #[serde(rename = "task.set")]
    TaskSet(TaskSet),
    #[serde(rename = "note.add")]
    NoteAdd {
        text: String,
        #[serde(default)]
        project: Option<String>,
        /// `None` is `fact`.
        #[serde(default)]
        kind: Option<String>,
    },
    #[serde(rename = "project.show")]
    ProjectShow { name: String },
    #[serde(rename = "project.add")]
    ProjectAdd {
        name: String,
        path: PathBuf,
        #[serde(default)]
        summary: Option<String>,
    },
    #[serde(rename = "brief")]
    Brief(NoParams),
    #[serde(rename = "attention")]
    Attention(NoParams),
    #[serde(rename = "suggest")]
    Suggest {
        text: String,
        /// Each is a moactl action, as typed: `task set 1 --status done`.
        #[serde(default)]
        actions: Vec<String>,
    },
    #[serde(rename = "suggestion.answer")]
    SuggestionAnswer {
        #[serde(default)]
        id: Option<i64>,
        answer: Answer,
    },
    #[serde(rename = "suggestion.ls")]
    SuggestionLs(NoParams),
    /// The front door's handshake: answers `{"running": bool}` once moa's terminal server is
    /// linked and has its `moa` session, or when there is no server. `tmux` is the
    /// absolute path of the tmux program the front door found.
    #[serde(rename = "tmux.ensure")]
    TmuxEnsure {
        #[serde(default)]
        tmux: Option<PathBuf>,
    },
    /// A panel says which it is: the tab `pane` (`%N`) it draws in, if any, and the `view`
    /// it shows, and whether moa `placed` it (it runs with `MOA_NO_AUTOSTART`). The view moa
    /// tagged the tab with wins over the one asked for; without either it gets the one this
    /// pane last had, else the picture. Answers `{key, view, managed}`: `managed` when the
    /// view is moa's. `once`: a popup (`moa panel … --once`), which tmux ends when its
    /// program does.
    #[serde(rename = "ui.panel")]
    UiPanel {
        #[serde(default)]
        pane: Option<String>,
        #[serde(default)]
        view: Option<View>,
        #[serde(default)]
        placed: bool,
        #[serde(default)]
        once: bool,
    },
    /// Gives the panel in tab `pane` (`%N`) another view: its tag, its frame and its
    /// `panels` row. Answers `{pane, view}`.
    #[serde(rename = "ui.select_view")]
    UiSelectView { pane: String, view: View },
    /// The folder a panel's selection is in, for a terminal opened "here".
    #[serde(rename = "ui.here")]
    UiHere {
        #[serde(default)]
        folder: Option<String>,
        /// A key or click in the panel moved its selection there: the user is here. Never
        /// set for the folder a connect restores, or one that data moved the selection to.
        #[serde(default)]
        by_user: bool,
    },
    /// A sheet (the picker or help, linked into your session) is done: its tab is unlinked
    /// from every session but moa's hidden one. Only a sheet's own panel sends it.
    #[serde(rename = "ui.close")]
    UiClose(NoParams),
    /// The layouts in `layouts.json` and the presets, the brain's current one marked.
    #[serde(rename = "layout.list")]
    LayoutList(NoParams),
    /// One layout's JSON.
    #[serde(rename = "layout.show")]
    LayoutShow { name: String },
    /// Saves `layout` as `name` in `layouts.json`, once it validates.
    #[serde(rename = "layout.set")]
    LayoutSet {
        name: String,
        layout: serde_json::Value,
    },
    /// Builds layout `name` in the cockpit (the current one when `None`), and makes it the
    /// brain's current layout.
    #[serde(rename = "layout.apply")]
    LayoutApply {
        #[serde(default)]
        name: Option<String>,
    },
    /// Selects a panel in the cockpit, putting the current layout back first when the panel
    /// is missing from it. `agent` is the agent spot.
    #[serde(rename = "goto")]
    Goto { panel: Leaf },
    /// Says `text` to moa. Answered `moa has it` once it is saved; with `wait`, with moa's
    /// answer once its turn ends. Refused while moa is answering, or still starting.
    #[serde(rename = "console.prompt")]
    ConsolePrompt {
        text: String,
        #[serde(default)]
        wait: bool,
    },
    /// Cancels moa's turn under way.
    #[serde(rename = "console.cancel")]
    ConsoleCancel(NoParams),
    /// The `n` feed rows before id `before`, oldest first, from every thread: what the
    /// console pages back through. Answered by the daemon from the store, not by the core.
    #[serde(rename = "console.page")]
    ConsolePage {
        before: i64,
        #[serde(default = "page_n")]
        n: u16,
    },
    /// The next prompt starts a new thread, even the same day.
    #[serde(rename = "console.new")]
    ConsoleNew(NoParams),
    /// A `/command` line typed in the console, run as the op it names with no turn of moa's
    /// (`moa_core::console`). The reply is that op's reply.
    #[serde(rename = "console.command")]
    ConsoleCommand { line: String },
    /// Answers moa's agent's permission request `id` (its `feed` row; the oldest waiting when
    /// `None`) with its option of the kind `answer` names.
    #[serde(rename = "console.permission")]
    ConsolePermission {
        #[serde(default)]
        id: Option<i64>,
        answer: PermAnswer,
    },
    /// `/auto`: whether moa reads the session reports on its own, with wake-ups. `None` flips
    /// it.
    #[serde(rename = "console.auto")]
    ConsoleAuto {
        #[serde(default)]
        on: Option<bool>,
    },
    /// `/sleep`: how long an idle session waits before it goes to sleep: `on` (moa.toml's),
    /// `off` (never) or a number of hours. `None` says the wait.
    #[serde(rename = "console.sleep_after")]
    ConsoleSleepAfter {
        #[serde(default)]
        value: Option<String>,
    },
    /// `/account`: switches moa's agent to account `name` with the next prompt; only you can.
    /// `None` says which account moa is on, and the others it can run on.
    #[serde(rename = "console.account")]
    ConsoleAccount {
        #[serde(default)]
        name: Option<String>,
    },
    /// Sets the console's button bar: `/command` lines, as moa's list (`user: false`, from
    /// `moactl bar`) or yours (`user: true`, from `/bar`), which wins. No commands clears it.
    #[serde(rename = "bar.set")]
    BarSet {
        commands: Vec<String>,
        #[serde(default)]
        user: bool,
    },
    /// The agents the caller can ask: moa, then every live session but the caller's own.
    #[serde(rename = "peers")]
    Peers(NoParams),
    /// Asks agent `to` (a session's name, or `moa`) `text`, and waits up to `timeout_s`
    /// seconds for the answer. Only a session or moa can ask.
    #[serde(rename = "ask")]
    Ask {
        to: String,
        text: String,
        #[serde(default = "ask_wait_s")]
        timeout_s: i64,
    },
    /// Waits again, up to `timeout_s` seconds, for the answer to ask `id`.
    #[serde(rename = "reply")]
    Reply {
        id: i64,
        #[serde(default = "ask_wait_s")]
        timeout_s: i64,
    },
    /// The caller's recent asks, from it or to it (every ask for a caller that is neither a
    /// session nor moa).
    #[serde(rename = "asks")]
    Asks(NoParams),
    /// Panics when run, so tests can see the core survive it. Not on the wire: it is not in
    /// [`Op::METHODS`] or the schema, and a client can't send it.
    #[doc(hidden)]
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    DebugPanic,
}

/// How many rows `console.page` returns when it isn't told.
fn page_n() -> u16 {
    100
}

/// How long `ask` and `reply` wait when they aren't told, in seconds.
fn ask_wait_s() -> i64 {
    crate::asks::ASK_WAIT_DEFAULT_MS / 1_000
}

/// The most rows `console.page` returns.
pub const PAGE_MAX: u16 = 200;

/// How a suggestion is answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Yes,
    No,
    Later,
}

/// How a permission request of moa's agent is answered: by the kind of option it offers,
/// never its place.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum PermAnswer {
    /// Its `allow_once` option.
    Once,
    /// Its `allow_always` option.
    Always,
    /// Its `reject_once` option, else its `reject_always`.
    No,
}

impl PermAnswer {
    pub const ALL: [PermAnswer; 3] = [PermAnswer::Once, PermAnswer::Always, PermAnswer::No];

    /// Its word: `once`, `always` or `no`.
    pub fn name(self) -> &'static str {
        match self {
            PermAnswer::Once => "once",
            PermAnswer::Always => "always",
            PermAnswer::No => "no",
        }
    }

    /// The kinds of option it stands for, in the order they are tried.
    pub fn kinds(self) -> &'static [PermKind] {
        match self {
            PermAnswer::Once => &[PermKind::AllowOnce],
            PermAnswer::Always => &[PermKind::AllowAlways],
            PermAnswer::No => &[PermKind::RejectOnce, PermKind::RejectAlways],
        }
    }
}

impl std::str::FromStr for PermAnswer {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        PermAnswer::ALL
            .into_iter()
            .find(|a| a.name() == s)
            .ok_or_else(|| "an answer is once, always or no".into())
    }
}

/// The params of an op that takes none. Missing, `null` and `{}` all decode (a unit variant
/// would reject `{}`); it encodes as `{}`. Anything else is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NoParams;

/// Derived, the schema would say `null`; the wire is an empty object (or missing, or `null`).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for NoParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NoParams".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": ["object", "null"],
            "properties": {},
            "additionalProperties": false
        })
    }
}

impl Serialize for NoParams {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        s.serialize_map(Some(0))?.end()
    }
}

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Empty {}
        // A missing `params` arrives as `None` (serde's missing-field deserializer).
        Option::<Empty>::deserialize(d).map(|_| NoParams)
    }
}

/// `task add`'s arguments, all the user's raw text.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskAdd {
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub today: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub blocked_on: Option<String>,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

/// `task set`'s arguments. `Some("")` clears `due`, `priority`, `session`, `note` and
/// `blocked_on`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskSet {
    pub id: i64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub blocked_on: Option<String>,
    #[serde(default)]
    pub today: bool,
    #[serde(default)]
    pub not_today: bool,
}

impl Op {
    /// Every method name on the wire, in variant order (`DebugPanic` is not one). A test ties
    /// this to the variants.
    pub const METHODS: &'static [&'static str] = &[
        "group.add",
        "group.rename",
        "group.rm",
        "group.fold",
        "session.ls",
        "session.set",
        "session.archive",
        "session.find",
        "session.open",
        "session.focus",
        "session.resume",
        "session.close",
        "session.sleep",
        "session.adopt",
        "session.restart",
        "term.open",
        "term.show",
        "term.ls",
        "term.name",
        "term.note",
        "term.close",
        "term.timeout",
        "task.ls",
        "task.add",
        "task.set",
        "note.add",
        "project.show",
        "project.add",
        "brief",
        "attention",
        "suggest",
        "suggestion.answer",
        "suggestion.ls",
        "tmux.ensure",
        "ui.panel",
        "ui.here",
        "ui.close",
        "ui.select_view",
        "layout.list",
        "layout.show",
        "layout.set",
        "layout.apply",
        "goto",
        "console.prompt",
        "console.cancel",
        "console.page",
        "console.new",
        "console.command",
        "console.permission",
        "console.auto",
        "console.sleep_after",
        "console.account",
        "bar.set",
        "peers",
        "ask",
        "reply",
        "asks",
    ];

    /// Whether this op changes the brain. Exhaustive on purpose: a new op has to be
    /// classified (`moactl` refuses writes from an account `moa.toml` doesn't list).
    pub fn writes(&self) -> bool {
        match self {
            Op::GroupAdd { .. }
            | Op::GroupRename { .. }
            | Op::GroupRm { .. }
            | Op::GroupFold { .. }
            | Op::SessionSet { .. }
            | Op::SessionArchive { .. }
            | Op::SessionOpen { .. }
            | Op::SessionFocus { .. }
            | Op::SessionResume { .. }
            | Op::SessionClose { .. }
            | Op::SessionSleep { .. }
            | Op::SessionAdopt { .. }
            | Op::SessionRestart { .. }
            | Op::TermName { .. }
            | Op::TermNote { .. }
            | Op::TermClose { .. }
            | Op::TermTimeout { .. }
            | Op::TaskAdd(_)
            | Op::TaskSet(_)
            | Op::NoteAdd { .. }
            | Op::ProjectAdd { .. }
            | Op::Suggest { .. }
            | Op::SuggestionAnswer { .. }
            | Op::LayoutSet { .. }
            | Op::LayoutApply { .. }
            | Op::ConsolePrompt { .. }
            | Op::ConsoleNew(_)
            | Op::BarSet { .. }
            | Op::ConsolePermission { .. }
            | Op::ConsoleAuto { .. }
            | Op::ConsoleSleepAfter { .. }
            | Op::ConsoleAccount { .. }
            // It saves the requests it cancels as `cancelled`.
            | Op::ConsoleCancel(_)
            // It may run a write (`/archive`, `/open`), so it is counted as one. A read-only
            // command still answers while writes are refused: the core refuses only at a
            // commit, and a read never commits.
            | Op::ConsoleCommand { .. }
            // It saves the ask's row.
            | Op::Ask { .. } => true,
            Op::SessionLs { .. }
            | Op::SessionFind { .. }
            | Op::TaskLs { .. }
            | Op::ProjectShow { .. }
            | Op::Brief(_)
            | Op::Attention(_)
            | Op::SuggestionLs(_)
            | Op::TmuxEnsure { .. }
            | Op::TermOpen { .. }
            | Op::TermShow { .. }
            | Op::TermLs(_)
            | Op::UiPanel { .. }
            | Op::UiHere { .. }
            | Op::UiClose(_)
            | Op::UiSelectView { .. }
            | Op::LayoutList(_)
            | Op::LayoutShow { .. }
            | Op::Goto { .. }
            | Op::ConsolePage { .. }
            | Op::Peers(_)
            | Op::Reply { .. }
            | Op::Asks(_)
            | Op::DebugPanic => false,
        }
    }

    /// This op's method name.
    pub fn method(&self) -> &'static str {
        match self {
            Op::GroupAdd { .. } => "group.add",
            Op::GroupRename { .. } => "group.rename",
            Op::GroupRm { .. } => "group.rm",
            Op::GroupFold { .. } => "group.fold",
            Op::SessionLs { .. } => "session.ls",
            Op::SessionSet { .. } => "session.set",
            Op::SessionArchive { .. } => "session.archive",
            Op::SessionFind { .. } => "session.find",
            Op::SessionOpen { .. } => "session.open",
            Op::SessionFocus { .. } => "session.focus",
            Op::SessionResume { .. } => "session.resume",
            Op::SessionClose { .. } => "session.close",
            Op::SessionSleep { .. } => "session.sleep",
            Op::SessionAdopt { .. } => "session.adopt",
            Op::SessionRestart { .. } => "session.restart",
            Op::TermOpen { .. } => "term.open",
            Op::TermShow { .. } => "term.show",
            Op::TermLs(_) => "term.ls",
            Op::TermName { .. } => "term.name",
            Op::TermNote { .. } => "term.note",
            Op::TermClose { .. } => "term.close",
            Op::TermTimeout { .. } => "term.timeout",
            Op::TaskLs { .. } => "task.ls",
            Op::TaskAdd(_) => "task.add",
            Op::TaskSet(_) => "task.set",
            Op::NoteAdd { .. } => "note.add",
            Op::ProjectShow { .. } => "project.show",
            Op::ProjectAdd { .. } => "project.add",
            Op::Brief(_) => "brief",
            Op::Attention(_) => "attention",
            Op::Suggest { .. } => "suggest",
            Op::SuggestionAnswer { .. } => "suggestion.answer",
            Op::SuggestionLs(_) => "suggestion.ls",
            Op::TmuxEnsure { .. } => "tmux.ensure",
            Op::UiPanel { .. } => "ui.panel",
            Op::UiHere { .. } => "ui.here",
            Op::UiClose(_) => "ui.close",
            Op::UiSelectView { .. } => "ui.select_view",
            Op::LayoutList(_) => "layout.list",
            Op::LayoutShow { .. } => "layout.show",
            Op::LayoutSet { .. } => "layout.set",
            Op::LayoutApply { .. } => "layout.apply",
            Op::Goto { .. } => "goto",
            Op::ConsolePrompt { .. } => "console.prompt",
            Op::ConsoleCancel(_) => "console.cancel",
            Op::ConsolePage { .. } => "console.page",
            Op::ConsoleNew(_) => "console.new",
            Op::BarSet { .. } => "bar.set",
            Op::ConsoleCommand { .. } => "console.command",
            Op::ConsolePermission { .. } => "console.permission",
            Op::ConsoleAuto { .. } => "console.auto",
            Op::ConsoleSleepAfter { .. } => "console.sleep_after",
            Op::ConsoleAccount { .. } => "console.account",
            Op::Peers(_) => "peers",
            Op::Ask { .. } => "ask",
            Op::Reply { .. } => "reply",
            Op::Asks(_) => "asks",
            Op::DebugPanic => "debug.panic",
        }
    }
}

/// A successful reply: the line the CLI prints and the data a script reads. A client decodes
/// it, so it round-trips.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Done {
    pub text: String,
    pub data: serde_json::Value,
}

/// What `sleep` is for, said when it is given neither names nor `--idle`.
pub const SLEEP_USAGE: &str = "moactl sleep ID... | moactl sleep --idle HOURS [--dry-run]";

/// What `adopt` says to a tab id that isn't one (`%<digits>`).
pub const ADOPT_USAGE: &str = "adopt needs a tab id like %5";

/// A refusal, as one human line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpError(pub String);
