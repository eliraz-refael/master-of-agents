//! The core's in-memory copy of the brain: every row the views need, kept in step with the
//! store by [`Change`]s and published as a [`Model`].

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::UtcMs;
use crate::central::Moa;
use crate::change::Change;
use crate::cockpit::run::CockpitRun;
use crate::config::{AccountName, BrainName, Config, caller_store};
use crate::effect::RefuseWrites;
use crate::layout::{self, Layouts};
use crate::model::*;
use crate::op::{ConnId, Op, ReqId};
use crate::panel::{PanelReg, View};
use crate::prompt::Asking;
use crate::publish::{ACTIVITY_SHOWN, FEED_SHOWN, Model};
use crate::terminals::Closing;
use crate::tmux::ids::{Generation, PaneId};
use crate::tmux::link::{Link, TmuxState};

/// How many hook events' keys are remembered for deduplication.
pub(crate) const RECENT_HOOKS: usize = 4096;

/// The next id for each integer-id table. SQLite rowids start at 1, so does this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NextIds {
    pub project: i64,
    pub task: i64,
    pub note: i64,
    pub suggestion: i64,
    pub ask: i64,
    pub event: i64,
    pub activity: i64,
    pub feed: i64,
    pub terminal: i64,
}

impl Default for NextIds {
    fn default() -> Self {
        NextIds {
            project: 1,
            task: 1,
            note: 1,
            suggestion: 1,
            ask: 1,
            event: 1,
            activity: 1,
            feed: 1,
            terminal: 1,
        }
    }
}

/// An integer-id table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    Project,
    Task,
    Note,
    Suggestion,
    Ask,
    Event,
    Activity,
    Feed,
    Terminal,
}

/// A row loaded with a default for text this build doesn't know. `key` is what
/// [`Change::row`] gives for that row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoadNote {
    pub table: &'static str,
    pub key: String,
    pub reason: &'static str,
}

/// Everything the store hands the core at startup.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub notes: Vec<Note>,
    pub suggestions: Vec<Suggestion>,
    pub sessions: Vec<Session>,
    pub conversations: Vec<Conversation>,
    pub groups: Vec<Group>,
    pub asks: Vec<Ask>,
    pub pane_bindings: Vec<PaneBinding>,
    pub spike_panes: Vec<SpikePane>,
    pub threads: Vec<MoaThread>,
    pub nags: Vec<Nag>,
    /// The open terminals only: a closed one's row stays on disk, as history.
    pub terminals: Vec<Terminal>,
    pub activity_tail: Vec<ActivityRow>,
    /// The newest [`FEED_SHOWN`] feed rows, oldest first.
    pub feed_tail: Vec<FeedEntry>,
    /// `(thread id, message_id)` of every keyed feed row in each account's newest thread.
    pub feed_keys: Vec<(String, String)>,
    pub meta: Vec<(String, String)>,
    pub next_ids: NextIds,
    pub unknown: Vec<LoadNote>,
}

/// What a caller says about itself (the hook environment).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CallerInfo {
    pub moa_session: Option<String>,
    pub claude_session: Option<String>,
    pub cwd: Option<PathBuf>,
}

const OUTSIDE_MOA: &str = "an agent outside moa";

/// Who is asking.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Caller {
    Moa,
    /// The session and its display handle.
    Session(SessionId, String),
    Unknown {
        folder: String,
    },
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Caller::Moa => f.write_str("moa"),
            Caller::Session(_, handle) => f.write_str(handle),
            Caller::Unknown { folder } if folder == OUTSIDE_MOA => f.write_str(OUTSIDE_MOA),
            Caller::Unknown { folder } => write!(f, "unknown in {folder}"),
        }
    }
}

fn first8(s: &str) -> String {
    s.chars().take(8).collect()
}

type ConvKey = (&'static str, String);

fn nag_key(n: &Nag) -> (TaskId, Date, Date) {
    (n.task_id, n.due, n.day)
}

fn conv_key(c: &Conversation) -> ConvKey {
    (c.agent_kind.as_str(), c.conversation_id.clone())
}

/// The moactl grammar for one suggestion action: its op, or why it doesn't parse.
pub type ParseAction = fn(&SuggestionAction, &Path) -> Result<Op, String>;

/// The moactl grammar for a console command's verb, given the folder a relative path is
/// relative to (if any) and home: its op, or why it doesn't parse.
pub type ParseCommand = fn(&SuggestionAction, Option<&Path>, &Path) -> Result<Op, String>;

/// What the core needs from outside to run suggestions, compare paths and open sessions.
/// The daemon supplies the real ones; the default has no grammar (so no suggestion can carry
/// actions), treats every path as already canonical, hands out sequential ids, and finds
/// every folder and transcript there.
#[derive(Clone)]
pub struct Deps {
    /// Parses one suggestion action with the moactl grammar (`moa_proto::cli::parse_action`),
    /// given the home directory. `moa-core` doesn't depend on `moa-proto`, so it arrives here.
    pub parse_action: Option<ParseAction>,
    /// Parses a console command's verb with the same grammar (`moa_proto::cli::parse_command`).
    pub parse_command: Option<ParseCommand>,
    /// Resolves a path to its one canonical spelling.
    pub canonical: fn(&Path) -> PathBuf,
    pub home: PathBuf,
    /// A new UUIDv7 at a moment (`moa_store::new_uuid`): the core makes no randomness.
    pub new_uuid: fn(UtcMs) -> uuid::Uuid,
    /// The Claude Code plugin sessions are started with, as an absolute path.
    pub session_plugin: PathBuf,
    /// The agent a pane runs, from its process (and children) and its start command
    /// (the daemon reads them in-process), or that none does, or that the reads failed.
    /// Called only where a pane is checked, never per event.
    pub inspect: fn(i64, &[String]) -> Inspected,
    /// A process's name as tmux's `#{pane_current_command}` shows it when that process is
    /// in front (`moa_platform::proc::name`), or `None` when it is gone: a terminal's shell,
    /// read once, when the terminal is recorded.
    pub process_name: fn(u32) -> Option<String>,
    /// Looks for a conversation's transcript in a store (`moa_agents::claude::find_transcript`):
    /// directory reads only, before a resume (C11 rule 2).
    pub transcript: fn(&Path, &str) -> TranscriptCheck,
    /// Whether a folder exists (`Path::is_dir`): a resume refuses one that is gone.
    pub is_dir: fn(&Path) -> bool,
    /// The newest conversation an agent in a folder wrote in a store, leaving out the ids
    /// given and any transcript last written before a moment
    /// (`moa_agents::claude::newest_conversation`): directory reads only, for `adopt` when the
    /// agent's argv names none (R21).
    pub newest_conversation: fn(&Path, &Path, &[String], Option<UtcMs>) -> Option<String>,
    /// A text file's contents, `None` when there is no file: `layouts.json`, read at start
    /// and again at every layout op (L2).
    pub read_text: fn(&Path) -> io::Result<Option<String>>,
    /// Where `layouts.json` is: `~/.config/moa/layouts.json`, shared by every brain.
    pub layouts_file: PathBuf,
    /// How a panel is started (L7).
    pub panel: PanelLaunch,
    /// The program that runs moa's agent when its account names none: the one moa was
    /// built with (`moa_agents::acp::BUILT_IN_ADAPTER`), `None` in a build without it.
    pub acp_adapter: Option<PathBuf>,
    /// The user's real home, for moa's agent alone, in a sandbox: Claude finds its login
    /// through the real home, which the sandbox's `HOME` hides. `None` outside a sandbox.
    pub agent_home: Option<PathBuf>,
    /// moa's agent's folder (`Paths::agent_dir`): it runs there, with its settings and
    /// `bin/moactl`.
    pub agent_dir: PathBuf,
    /// The brain's scope file, `~/.config/moa/<brain>.md`: what this brain is for, told to
    /// moa's agent after its constitution.
    pub scope_file: PathBuf,
    /// `~/.config/moa/constitution.md`, which replaces the constitution moa was built with.
    pub constitution_file: PathBuf,
}

/// How moa starts a panel (L7): its own program, and what each panel's environment gets
/// besides `MOA_BRAIN`. The daemon gives its `current_exe`, and `MOA_ROOT` only when it is
/// sandboxed (lesson 5: env goes explicitly, per pane).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PanelLaunch {
    pub exe: PathBuf,
    pub env: Vec<(String, String)>,
}

fn identity(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// UUIDv7s at `at` whose other bits count up from 1, per thread, so a test knows each id.
fn sequential_uuid(at: UtcMs) -> uuid::Uuid {
    thread_local! {
        static ISSUED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let n = ISSUED.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let mut bytes = [0u8; 10];
    bytes[2..].copy_from_slice(&n.to_be_bytes());
    uuid::Builder::from_unix_timestamp_millis(at.0.max(0) as u64, &bytes).into_uuid()
}

impl Default for Deps {
    fn default() -> Self {
        Deps {
            parse_action: None,
            parse_command: None,
            canonical: identity,
            home: PathBuf::new(),
            new_uuid: sequential_uuid,
            session_plugin: PathBuf::new(),
            inspect: |_, _| Inspected::Gone,
            process_name: |_| None,
            transcript: |_, _| TranscriptCheck::Present(PathBuf::new()),
            is_dir: |_| true,
            newest_conversation: |_, _, _, _| None,
            read_text: |_| Ok(None),
            layouts_file: PathBuf::new(),
            panel: PanelLaunch::default(),
            acp_adapter: None,
            agent_home: None,
            agent_dir: PathBuf::from("/tmp/moa-test-brain/agent"),
            scope_file: PathBuf::from("/tmp/moa-test-config/default.md"),
            constitution_file: PathBuf::from("/tmp/moa-test-config/constitution.md"),
        }
    }
}

/// How many session events the state keeps (B3), the newest.
pub const EVENTS_KEPT: usize = 200;

/// What a session did, as the console tells it (B3): one per committed change of a session
/// that moves it into one of these (`hooks::event_of`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventWhat {
    Started,
    Ended,
    NeedsYou,
    Finished,
    Asleep,
}

/// A session event: kept in memory and published on the `feed` topic, never saved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionEvent {
    /// Counts from 1 while this daemon runs.
    pub seq: u64,
    pub at: UtcMs,
    pub session: SessionId,
    pub what: EventWhat,
}

/// What `status` says about the brain, the sessions and the caller.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StatusData {
    pub brain: String,
    /// Live sessions per display state word (`working`, `idle`, ...).
    pub sessions: BTreeMap<String, usize>,
    /// Who is asking, as [`Caller`] shows it.
    pub caller: String,
    /// The terminal server, while linked.
    pub tmux: Option<TmuxLinkData>,
    /// How many times the terminal server restarted while this daemon ran.
    pub tmux_restarts: u32,
    /// How many panels moa started or restarted while this daemon ran (L7).
    pub panel_starts: u32,
}

/// The terminal server the daemon is linked to: its tmux version and generation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TmuxLinkData {
    pub version: String,
    pub pid: i64,
    pub start: i64,
}

pub struct State {
    deps: Deps,
    brain: BrainName,
    config: Config,
    pub(crate) now: UtcMs,
    pub(crate) refuse_writes: Option<RefuseWrites>,
    pub(crate) model: Arc<Model>,
    pub(crate) projects: BTreeMap<ProjectId, Project>,
    pub(crate) tasks: BTreeMap<TaskId, Task>,
    notes: BTreeMap<NoteId, Note>,
    pub(crate) suggestions: BTreeMap<SuggestionId, Suggestion>,
    pub(crate) sessions: BTreeMap<SessionId, Session>,
    conversations: BTreeMap<ConvKey, Conversation>,
    pub(crate) groups: BTreeMap<String, Group>,
    pub(crate) asks: BTreeMap<AskId, Ask>,
    pane_bindings: BTreeMap<SessionId, PaneBinding>,
    spike_panes: BTreeMap<SessionId, SpikePane>,
    threads: BTreeMap<String, MoaThread>,
    nags: BTreeMap<(TaskId, Date, Date), Nag>,
    /// The open terminals: applying a closed one drops it, as a load leaves it out.
    pub(crate) terminals: BTreeMap<TermId, Terminal>,
    pub(crate) activity: BTreeMap<ActivityId, ActivityRow>,
    /// The newest [`FEED_SHOWN`] feed rows by id.
    pub(crate) feed_tail: BTreeMap<i64, FeedEntry>,
    /// The `message_id`s seen in each thread that moa can resume: the adapter's answers are
    /// matched against them so a loaded session's replay never doubles a row.
    feed_keys: BTreeMap<String, BTreeSet<String>>,
    /// moa's own agent, the central one.
    pub(crate) central: Moa,
    meta: BTreeMap<String, String>,
    next_ids: NextIds,
    unknown: Vec<LoadNote>,
    pub(crate) tmux: TmuxState,
    /// The panes a user was looking at after the last tmux input ([`crate::terminals::touched`]
    /// compares it with the next). Kept in memory: a daemon restart forgets it.
    pub(crate) looked_at: BTreeSet<PaneId>,
    /// The `term.ls` requests waiting for a listing of the tabs ([`crate::ops::term::start_ls`]).
    pub(crate) term_ls: Vec<(ConnId, ReqId)>,
    /// The sleep deadline the timer is armed at, if any (R19): `SetTimer` and `CancelTimer`
    /// go out only when it changes.
    pub(crate) sleep_at: Option<UtcMs>,
    /// The closes moa started, by record, until a listing no longer shows their tab
    /// ([`crate::terminals::Closing`]). Kept in memory, and forgotten when the link is lost,
    /// another server comes, or the core reloads.
    pub(crate) closing: BTreeMap<TermId, Closing>,
    /// The terminals' deadline the timer is armed at, if any, as `sleep_at` is.
    pub(crate) terminals_at: Option<UtcMs>,
    /// The keys of the last [`RECENT_HOOKS`] hook events applied, oldest first, and the same
    /// keys for lookup: a hook that arrives twice is applied once.
    pub(crate) recent_hooks: VecDeque<(u32, u32, i64)>,
    pub(crate) recent_hook_set: HashSet<(u32, u32, i64)>,
    /// The sessions restarted under moa (R24), each with when the restart was saved and the
    /// tab it killed, since moa last took on a hand-started agent for them (an untracked
    /// binding, or a start that doesn't name them, sent after the restart). Every agent of
    /// theirs since carries `MOA_SESSION`, so a hook without it is from the agent the restart
    /// killed, or another one moa didn't start: history only, whenever it arrives; so is an
    /// end from the killed tab. Kept in memory: a daemon restart forgets it.
    pub(crate) restarted: BTreeMap<SessionId, (UtcMs, PaneId)>,
    /// The tab moa last killed for each session (`sleep`, sleeping on its own, `close`,
    /// `restart`), and the server it was on. Its agent dies with it, but a hook that agent
    /// sent between the commit and the kill can still arrive after the commit, stamped later
    /// than it: a start would bring the session back with no tab. So any event from that
    /// tab but an end is history only ([`crate::hooks`]), while the session isn't bound to
    /// it: a lost kill leaves the tab the session's. tmux never reuses a tab id on one
    /// server. A hook names its tab but not its server, so an agent in another tmux, in a
    /// tab with the same id, is taken for the killed one. Kept in memory: a daemon restart
    /// forgets it, and a late start that reaches the new daemon after its first reconcile
    /// brings the session back with no tab until the next one.
    pub(crate) killed: BTreeMap<SessionId, (Generation, PaneId)>,
    /// The sessions a look at the tabs found no agent of ([`crate::wake`]), each with the
    /// server and the panes that look listed. Their events ask no look again until they
    /// start, or tmux shows a pane that look didn't. Kept in memory.
    pub(crate) not_found: BTreeMap<SessionId, (Generation, BTreeSet<PaneId>)>,
    /// The connected panels, by connection (`ui.panel`). Kept in memory: a connection that
    /// closes drops its panel.
    pub(crate) panels: BTreeMap<ConnId, PanelReg>,
    /// The view each tab last showed in a panel while this daemon ran: a panel that comes
    /// back to a tab without naming a view gets it again.
    pub(crate) pane_views: BTreeMap<PaneId, View>,
    /// `layouts.json` as last read, merged with the presets. Kept in memory: every layout op
    /// reads the file again.
    pub(crate) layouts: Layouts,
    /// How many panels moa started, new or respawned, while this daemon ran (L7): a layout
    /// switch that starts nothing leaves it as it was. Kept in memory.
    pub(crate) panel_starts: u32,
    /// The cockpit's operations, one at a time (L17). Kept in memory: a reload starts the
    /// queue over.
    pub(crate) cockpit: CockpitRun,
    /// The prompts waiting for their answer, by token, oldest first, at most
    /// [`crate::prompt::WAITING`] (S5). A new connection clears them. Kept in memory.
    pub(crate) prompts: BTreeMap<u64, Asking>,
    /// The last prompt's token: tokens never repeat while this daemon runs.
    pub(crate) last_prompt: u64,
    /// The sheets open (S9), by their panel's pane: each open's number, its wall-clock
    /// milliseconds, which never repeat across moa's restarts. A panel's `panels` row reads
    /// its open here, so one that registers after its link still knows it. Kept in memory: a
    /// listing after a restart unlinks a sheet left open.
    pub(crate) sheets: BTreeMap<PaneId, i64>,
    /// The last open's number: two opens in one millisecond still differ.
    pub(crate) last_sheet: i64,
    /// The newest [`EVENTS_KEPT`] session events, oldest first. Kept in memory: a daemon
    /// restart forgets them, a reload keeps them.
    pub(crate) session_events: VecDeque<SessionEvent>,
    /// The last session event's `seq`.
    last_event: u64,
}

impl State {
    /// Empty until a snapshot is loaded.
    pub fn new(brain: BrainName, config: Config) -> State {
        State {
            deps: Deps::default(),
            brain,
            config,
            now: UtcMs(0),
            refuse_writes: None,
            model: Arc::new(Model::default()),
            projects: BTreeMap::new(),
            tasks: BTreeMap::new(),
            notes: BTreeMap::new(),
            suggestions: BTreeMap::new(),
            sessions: BTreeMap::new(),
            conversations: BTreeMap::new(),
            groups: BTreeMap::new(),
            asks: BTreeMap::new(),
            pane_bindings: BTreeMap::new(),
            spike_panes: BTreeMap::new(),
            threads: BTreeMap::new(),
            nags: BTreeMap::new(),
            terminals: BTreeMap::new(),
            activity: BTreeMap::new(),
            feed_tail: BTreeMap::new(),
            feed_keys: BTreeMap::new(),
            central: Moa::default(),
            meta: BTreeMap::new(),
            next_ids: NextIds::default(),
            unknown: Vec::new(),
            tmux: TmuxState::default(),
            looked_at: BTreeSet::new(),
            term_ls: Vec::new(),
            sleep_at: None,
            closing: BTreeMap::new(),
            terminals_at: None,
            recent_hooks: VecDeque::new(),
            recent_hook_set: HashSet::new(),
            restarted: BTreeMap::new(),
            killed: BTreeMap::new(),
            not_found: BTreeMap::new(),
            panels: BTreeMap::new(),
            pane_views: BTreeMap::new(),
            layouts: layout::presets(),
            panel_starts: 0,
            cockpit: CockpitRun::default(),
            prompts: BTreeMap::new(),
            last_prompt: 0,
            sheets: BTreeMap::new(),
            last_sheet: 0,
            session_events: VecDeque::new(),
            last_event: 0,
        }
    }

    /// Replaces the outside dependencies; apply before `Startup`.
    pub fn with_deps(mut self, deps: Deps) -> State {
        self.deps = deps;
        self
    }

    pub fn deps(&self) -> &Deps {
        &self.deps
    }

    pub fn brain(&self) -> &BrainName {
        &self.brain
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Set while the store can't be written to; see [`RefuseWrites`].
    pub fn refuse_writes(&self) -> Option<&RefuseWrites> {
        self.refuse_writes.as_ref()
    }

    /// Whether a write at `now` is refused: the same test as `Tx::commit`'s.
    pub fn writes_refused(&self, now: UtcMs) -> bool {
        self.refuse_writes.as_ref().is_some_and(|r| now < r.until)
    }

    /// The answer to `status`, with sessions' states as of `now`.
    pub fn status(&self, caller: &CallerInfo, now: UtcMs) -> StatusData {
        use crate::live::{DisplayState, display_state};
        let mut sessions: BTreeMap<String, usize> = DisplayState::ALL
            .iter()
            .filter(|d| !matches!(d, DisplayState::Ended | DisplayState::Archived))
            .map(|d| (crate::publish::word(*d), 0))
            .collect();
        for s in self.sessions() {
            let (state, _) = display_state(s, now);
            if let Some(n) = sessions.get_mut(&crate::publish::word(state)) {
                *n += 1;
            }
        }
        let tmux = match &self.tmux.link {
            Link::Up {
                generation,
                version,
                ..
            } => Some(TmuxLinkData {
                version: version.clone(),
                pid: generation.pid,
                start: generation.start,
            }),
            Link::Down | Link::Connecting => None,
        };
        StatusData {
            brain: self.brain.as_str().to_string(),
            sessions,
            caller: self.caller(caller).to_string(),
            tmux,
            tmux_restarts: self.tmux.restarts,
            panel_starts: self.panel_starts,
        }
    }

    /// The terminal server link.
    pub fn tmux(&self) -> &TmuxState {
        &self.tmux
    }

    pub fn model(&self) -> Arc<Model> {
        self.model.clone()
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub(crate) fn pane_bindings(&self) -> impl Iterator<Item = &PaneBinding> {
        self.pane_bindings.values()
    }

    /// The session's pane binding, of whichever server.
    pub fn binding(&self, id: SessionId) -> Option<&PaneBinding> {
        self.pane_bindings.get(&id)
    }

    /// The folder the selection of the panel in tab `pane` is in (`ui.here`).
    pub fn here_of(&self, pane: PaneId) -> Option<&str> {
        self.panel_in(pane).and_then(|p| p.here.as_deref())
    }

    /// The number of the sheet open in the panel in tab `pane`, if one is (S9).
    pub fn sheet(&self, pane: PaneId) -> Option<i64> {
        self.sheets.get(&pane).copied()
    }

    /// The panel drawing in tab `pane`, if one is.
    pub(crate) fn panel_in(&self, pane: PaneId) -> Option<&PanelReg> {
        self.panels.values().find(|p| p.pane == Some(pane))
    }

    /// The panes imported from the spike, by session: verified or cleared by reconcile.
    pub fn spike_panes(&self) -> impl Iterator<Item = &SpikePane> {
        self.spike_panes.values()
    }

    /// The agent store a `CLAUDE_CONFIG_DIR` value names, canonical: no value (or an empty
    /// one) is `<home>/.claude`, Claude's own default (R32).
    pub fn store_of(&self, dir: Option<&str>) -> PathBuf {
        let store = caller_store(dir.map(OsStr::new), &self.deps.home);
        (self.deps.canonical)(&store)
    }

    /// The listed account whose store is `store`, comparing canonical folders.
    pub fn account_of_store(&self, store: &Path) -> Option<AccountName> {
        let canonical = self.deps.canonical;
        let store = canonical(store);
        self.config
            .accounts
            .iter()
            .find(|(_, a)| canonical(&a.store) == store)
            .map(|(name, _)| name.clone())
    }

    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    pub fn projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.values()
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.values()
    }

    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.notes.values()
    }

    pub fn suggestions(&self) -> impl Iterator<Item = &Suggestion> {
        self.suggestions.values()
    }

    pub fn conversations_of(&self, id: SessionId) -> impl Iterator<Item = &Conversation> {
        self.conversations
            .values()
            .filter(move |c| c.session_id == id)
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(String::as_str)
    }

    /// Rows loaded with a default for text this build doesn't know.
    pub fn unknown(&self) -> &[LoadNote] {
        &self.unknown
    }

    /// Whether the row `key` of `table` was loaded with a value this build doesn't know.
    pub fn is_unknown(&self, table: &str, key: &str) -> bool {
        self.unknown
            .iter()
            .any(|n| n.table == table && n.key == key)
    }

    /// moa's threads (`moa_threads`).
    pub fn threads(&self) -> impl Iterator<Item = &MoaThread> {
        self.threads.values()
    }

    /// moa's thread `id`, when its row is saved.
    pub fn thread(&self, id: &str) -> Option<&MoaThread> {
        self.threads.get(id)
    }

    /// moa's own agent, as the core knows it.
    pub fn central(&self) -> &Moa {
        &self.central
    }

    /// The newest feed rows, oldest first.
    pub fn feed_tail(&self) -> impl Iterator<Item = &FeedEntry> {
        self.feed_tail.values()
    }

    /// The session events kept, oldest first.
    pub fn session_events(&self) -> impl Iterator<Item = &SessionEvent> {
        self.session_events.iter()
    }

    /// Records that session `session` did `what` at `at`, dropping the oldest event past
    /// [`EVENTS_KEPT`].
    pub(crate) fn push_session_event(&mut self, at: UtcMs, session: SessionId, what: EventWhat) {
        self.last_event += 1;
        self.session_events.push_back(SessionEvent {
            seq: self.last_event,
            at,
            session,
            what,
        });
        if self.session_events.len() > EVENTS_KEPT {
            self.session_events.pop_front();
        }
    }

    /// The last session event's `seq` so far: [`State::drop_events_after`] takes it.
    pub fn event_mark(&self) -> u64 {
        self.last_event
    }

    /// Forgets the session events pushed after `mark` ([`State::event_mark`]): the daemon's
    /// recovery, before it reloads, for the changes its failed commit didn't save. Their
    /// `seq`s aren't handed out again, so no published key is reused.
    pub fn drop_events_after(&mut self, mark: u64) {
        self.session_events.retain(|e| e.seq <= mark);
    }

    /// The `message_id`s of the rows in `thread`, when it is one of the threads kept (each
    /// account's newest, and any a row was written to since the load).
    pub fn feed_keys(&self, thread: &str) -> Option<&BTreeSet<String>> {
        self.feed_keys.get(thread)
    }

    /// The id the next row of `table` takes.
    pub(crate) fn next_id(&self, table: Table) -> i64 {
        let n = &self.next_ids;
        match table {
            Table::Project => n.project,
            Table::Task => n.task,
            Table::Note => n.note,
            Table::Suggestion => n.suggestion,
            Table::Ask => n.ask,
            Table::Event => n.event,
            Table::Activity => n.activity,
            Table::Feed => n.feed,
            Table::Terminal => n.terminal,
        }
    }

    /// The time views are computed at. `handle` sets it from `Now`.
    pub(crate) fn set_now(&mut self, now: UtcMs) {
        self.now = now;
    }

    /// The session's current conversation: the latest an agent started on. A conversation's
    /// `last_seen_at` moves only when an agent starts on it (a start hook, adopt, open), so a
    /// `/resume` inside Claude back to an older one makes that one current. Ties go to the
    /// latest first seen, then to the highest id.
    pub(crate) fn current_conversation(&self, id: SessionId) -> Option<&Conversation> {
        self.conversations_of(id)
            .max_by_key(|c| (c.last_seen_at, c.first_seen_at))
    }

    /// The conversation row for an agent's conversation id, whichever session owns it.
    pub(crate) fn conversation_of_agent(
        &self,
        agent: crate::model::AgentKind,
        conversation_id: &str,
    ) -> Option<&Conversation> {
        self.conversations
            .get(&(agent.as_str(), conversation_id.to_string()))
    }

    /// Remembers a hook event's key, forgetting the oldest past [`RECENT_HOOKS`].
    pub(crate) fn remember_hook(&mut self, key: (u32, u32, i64)) {
        if self.recent_hook_set.insert(key) {
            self.recent_hooks.push_back(key);
            if self.recent_hooks.len() > RECENT_HOOKS
                && let Some(old) = self.recent_hooks.pop_front()
            {
                self.recent_hook_set.remove(&old);
            }
        }
    }

    pub(crate) fn forget_hooks(&mut self) {
        self.recent_hooks.clear();
        self.recent_hook_set.clear();
    }

    /// What a session is called to people: its current conversation id's first 8 characters,
    /// or its own id's when it has none.
    pub fn handle_of(&self, id: SessionId) -> String {
        match self.current_conversation(id) {
            Some(c) => first8(&c.conversation_id),
            None => first8(&id.to_string()),
        }
    }

    /// Finds who is asking: moa, a known session (by moa id, then by conversation), or an
    /// unknown agent named by its folder.
    pub fn caller(&self, c: &CallerInfo) -> Caller {
        if c.moa_session.as_deref() == Some("moa") {
            return Caller::Moa;
        }
        let known = |id: SessionId| Caller::Session(id, self.handle_of(id));
        if let Some(id) = c
            .moa_session
            .as_deref()
            .and_then(|s| s.parse::<SessionId>().ok())
            .filter(|id| self.sessions.contains_key(id))
        {
            return known(id);
        }
        if let Some(conv) = c
            .claude_session
            .as_deref()
            .and_then(|s| self.conversations.values().find(|v| v.conversation_id == s))
        {
            return known(conv.session_id);
        }
        let folder = c.cwd.as_ref().and_then(|p| p.file_name()).map_or_else(
            || OUTSIDE_MOA.to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Caller::Unknown { folder }
    }

    /// Replaces every row. The published rev and the tmux link are kept.
    pub(crate) fn load(&mut self, snap: Snapshot) {
        fn by<K: Ord, V>(rows: Vec<V>, key: impl Fn(&V) -> K) -> BTreeMap<K, V> {
            rows.into_iter().map(|r| (key(&r), r)).collect()
        }
        self.projects = by(snap.projects, |r| r.id);
        self.tasks = by(snap.tasks, |r| r.id);
        self.notes = by(snap.notes, |r| r.id);
        self.suggestions = by(snap.suggestions, |r| r.id);
        self.sessions = by(snap.sessions, |r| r.id);
        self.conversations = by(snap.conversations, conv_key);
        self.groups = by(snap.groups, |r| r.path.clone());
        self.asks = by(snap.asks, |r| r.id);
        self.pane_bindings = by(snap.pane_bindings, |r| r.session_id);
        self.spike_panes = by(snap.spike_panes, |r| r.session_id);
        self.threads = by(snap.threads, |r| r.thread_id.clone());
        self.nags = by(snap.nags, nag_key);
        self.terminals = by(snap.terminals, |r| r.id);
        self.activity = by(snap.activity_tail, |r| r.id);
        self.trim_activity();
        self.feed_tail = by(snap.feed_tail, |r| r.id);
        self.trim_feed();
        self.feed_keys.clear();
        for (thread, key) in snap.feed_keys {
            self.feed_keys.entry(thread).or_default().insert(key);
        }
        self.meta = snap.meta.into_iter().collect();
        self.next_ids = snap.next_ids;
        self.unknown = snap.unknown;
    }

    fn trim_activity(&mut self) {
        while self.activity.len() > ACTIVITY_SHOWN {
            self.activity.pop_first();
        }
    }

    fn trim_feed(&mut self) {
        while self.feed_tail.len() > FEED_SHOWN {
            self.feed_tail.pop_first();
        }
    }

    fn bump(next: &mut i64, id: i64) {
        *next = (*next).max(id.saturating_add(1));
    }

    /// The in-memory twin of the store's SQL for this change. Events are not kept, only
    /// their ids are counted; feed entries keep a tail and their keys.
    pub(crate) fn apply(&mut self, c: &Change) {
        match c {
            Change::Project(r) => {
                Self::bump(&mut self.next_ids.project, r.id.0);
                self.projects.insert(r.id, r.clone());
            }
            Change::Task(r) => {
                Self::bump(&mut self.next_ids.task, r.id.0);
                self.tasks.insert(r.id, r.clone());
            }
            Change::Note(r) => {
                Self::bump(&mut self.next_ids.note, r.id.0);
                self.notes.insert(r.id, r.clone());
            }
            Change::Suggestion(r) => {
                Self::bump(&mut self.next_ids.suggestion, r.id.0);
                self.suggestions.insert(r.id, r.clone());
            }
            Change::Session(r) => {
                self.sessions.insert(r.id, r.clone());
            }
            Change::Conversation(r) => {
                self.conversations.insert(conv_key(r), r.clone());
            }
            Change::PaneBinding(r) => {
                // A hand-started agent is the session's own again.
                if !r.tracked {
                    self.restarted.remove(&r.session_id);
                }
                self.pane_bindings.insert(r.session_id, r.clone());
            }
            Change::SpikePane(r) => {
                self.spike_panes.insert(r.session_id, r.clone());
            }
            Change::Event(r) => Self::bump(&mut self.next_ids.event, r.id.0),
            Change::Activity(r) => {
                Self::bump(&mut self.next_ids.activity, r.id.0);
                self.activity.insert(r.id, r.clone());
                self.trim_activity();
            }
            Change::Group(r) => {
                self.groups.insert(r.path.clone(), r.clone());
            }
            Change::Ask(r) => {
                Self::bump(&mut self.next_ids.ask, r.id.0);
                self.asks.insert(r.id, r.clone());
            }
            Change::Thread(r) => {
                self.threads.insert(r.thread_id.clone(), r.clone());
            }
            Change::Feed(r) => {
                Self::bump(&mut self.next_ids.feed, r.id);
                if let Some(key) = &r.message_id {
                    self.feed_keys
                        .entry(r.thread_id.clone())
                        .or_default()
                        .insert(key.clone());
                }
                self.feed_tail.insert(r.id, r.clone());
                self.trim_feed();
            }
            Change::Nag(r) => {
                self.nags.insert(nag_key(r), *r);
            }
            Change::Terminal(r) => {
                Self::bump(&mut self.next_ids.terminal, r.id.0);
                if r.closed_at.is_some() {
                    self.terminals.remove(&r.id);
                } else {
                    self.terminals.insert(r.id, r.clone());
                }
            }
            Change::Meta { key, value } => match value {
                Some(v) => {
                    self.meta.insert(key.clone(), v.clone());
                }
                None => {
                    self.meta.remove(key);
                }
            },
            Change::DeletePaneBinding(id) => {
                self.pane_bindings.remove(id);
            }
            Change::DeleteSpikePane(id) => {
                self.spike_panes.remove(id);
            }
            Change::DeleteGroup(path) => {
                self.groups.remove(path);
            }
        }
    }
}

#[cfg(test)]
mod tests;
