//! A world for moa's prompts and what they carry: the typical brain with tasks, open
//! terminals and a suggestion waiting for its yes, moa's account `acct-a`, the tmux link up,
//! and a session of it that reports.

use jiff::ToSpan;
use jiff::civil::Date;
use moa_core::UtcMs;
use moa_core::central::opening::BRIEFED_KEY;
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, RefuseWrites, TmuxIn, handle_checked};
use moa_core::hook::HookKind;
use moa_core::model::{
    AgentKind, Lifecycle, Location, Nag, OpenedBy, Suggestion, SuggestionId, SuggestionStatus,
    Task, TaskId, TaskStatus, TermId, Terminal,
};
use moa_core::op::{Answer, ConnId, Op, ReqId, TaskSet};
use moa_core::state::{CallerInfo, Deps, Snapshot, State};
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;

use super::central::{end_turn, fire, play, toml};
use super::{at, generation, link_up};

pub const SEC: i64 = 1_000;
pub const MIN: i64 = 60 * SEC;
pub const HOUR: i64 = 60 * MIN;

/// The due block of [`five_due`] on the typical brain's day (D10).
pub const DUE_BLOCK: &str = "Due by tomorrow (end your reply with these lines as they are):\n\
                         ⏰ 2 days overdue: Reply to Kite; Renew the domain\n\
                         ⏰ 1 day overdue: Send the invoice\n\
                         ⏰ Due today: Merge PR #3\n\
                         ⏰ Due tomorrow: Book the venue";

/// The suggestion each world has open, with nothing to run: a yes to it waits for moa.
pub const YES_ID: i64 = 900;

/// The typical brain with the tasks and open terminals given, open suggestion [`YES_ID`],
/// moa's account `acct-a` and `settings` after its `moa.toml`, and the tmux link up; a
/// session of it that reports.
pub struct World {
    pub b: TypicalBrain,
    pub st: State,
    /// What the start said.
    pub start: Vec<Effect>,
    pub session: (String, String),
    /// The open terminals it started with.
    pub terminals: Vec<Terminal>,
    pub seq: u32,
    pub req: u64,
}

impl World {
    /// `tasks` made on the brain's day, `terminals` against its home. moa opened the day
    /// already: nothing here briefs it again ([`World::build`] for one that does).
    pub fn new(
        tasks: impl FnOnce(Date) -> Vec<Task>,
        terminals: impl FnOnce(&TypicalBrain) -> Vec<Terminal>,
        settings: &str,
    ) -> World {
        let opened = |today: Date| vec![(BRIEFED_KEY, today.to_string())];
        World::build(tasks, terminals, settings, opened)
    }

    /// [`World::new`], its `meta` rows what `meta` says on the brain's day.
    pub fn build(
        tasks: impl FnOnce(Date) -> Vec<Task>,
        terminals: impl FnOnce(&TypicalBrain) -> Vec<Terminal>,
        settings: &str,
        meta: impl FnOnce(Date) -> Vec<(&'static str, String)>,
    ) -> World {
        let b = TypicalBrain::new();
        let mut snap = b.snapshot();
        let today = b.now().local_day;
        snap.tasks = tasks(today);
        snap.meta = meta(today)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let terminals = terminals(&b);
        snap.next_ids.terminal = terminals.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
        snap.terminals = terminals.clone();
        snap.suggestions.push(Suggestion {
            id: SuggestionId(YES_ID),
            text: "Kite has 3 overdue tasks blocked on the domain; move them to Friday?".into(),
            status: SuggestionStatus::Open,
            actions: vec![],
            actions_raw: None,
            actions_invalid: None,
            result: None,
            created_at: UtcMs(1),
            resolved_at: None,
        });
        let deps = Deps {
            home: b.home.clone(),
            parse_action: Some(moa_proto::cli::parse_action),
            ..Deps::default()
        };
        let settings = format!("{}{settings}", toml("acct-a"));
        let (mut st, start) = super::started_at(&b, &settings, deps, snap, b.now());
        link_up(&b, &mut st);
        let session = st
            .sessions()
            .filter(|s| s.lifecycle == Lifecycle::Live && s.location == Location::Here)
            .filter(|s| s.archived_at.is_none())
            .find_map(|s| {
                let c = st
                    .conversations_of(s.id)
                    .find(|c| c.agent_kind == AgentKind::Claude)?;
                Some((s.id.to_string(), c.conversation_id.clone()))
            })
            .expect("a live session of the typical brain");
        World {
            b,
            st,
            start,
            session,
            terminals,
            seq: 0,
            req: 0,
        }
    }

    /// The typical brain's time, `ms` later.
    pub fn t(&self, ms: i64) -> i64 {
        self.b.now().wall.0 + ms
    }

    pub fn today(&self) -> Date {
        self.b.now().local_day
    }

    /// The session finishes a turn at `ms`.
    pub fn report(&mut self, ms: i64) -> Vec<Effect> {
        self.seq += 1;
        let kind = HookKind::TurnEnded {
            last_message: "Merged PR #3".into(),
        };
        let (id, conversation) = &self.session;
        let envelope = super::envelope(id, conversation, UtcMs(ms), self.seq, kind);
        let input = Input::Hook {
            reply: None,
            envelope,
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }

    /// A wake-up at `ms`: the turn under way ends, the session reports, and the `Wake`
    /// timer fires once the report settles; moa's agent starts and opens what it is asked.
    pub fn wake(&mut self, ms: i64) -> Vec<Effect> {
        if self.st.central().turn.is_some() {
            end_turn(&mut self.st, ms);
        }
        self.report(ms);
        let w = ms + 45 * SEC;
        let fx = fire(&mut self.st, w);
        play(&mut self.st, w, fx)
    }

    /// Your yes to [`YES_ID`] at `ms`; moa's agent starts and opens what it is asked.
    pub fn yes(&mut self, ms: i64) -> Vec<Effect> {
        let op = Op::SuggestionAnswer {
            id: Some(YES_ID),
            answer: Answer::Yes,
        };
        self.op_from(ms, CallerInfo::default(), op)
    }

    /// tmux says `text` to the daemon's control client at `ms` (a `moa:` mailbox line).
    pub fn message(&mut self, ms: i64, text: &str) -> Vec<Effect> {
        let t = TmuxIn::Notification(Notification::Message(text.into()));
        handle_checked(&mut self.st, Input::Tmux(t), at(UtcMs(ms)))
    }

    /// `op` at `ms` from `caller`; moa's agent starts and opens what it is asked.
    pub fn op_from(&mut self, ms: i64, caller: CallerInfo, op: Op) -> Vec<Effect> {
        self.req += 1;
        let input = Input::Client {
            conn: ConnId(1),
            req: ReqId(self.req),
            caller,
            op,
        };
        let fx = handle_checked(&mut self.st, input, at(UtcMs(ms)));
        play(&mut self.st, ms, fx)
    }

    /// Your prompt `text` at `ms`; moa's agent starts and opens what it is asked.
    pub fn prompt(&mut self, ms: i64, text: &str) -> Vec<Effect> {
        let op = Op::ConsolePrompt {
            text: text.into(),
            wait: false,
        };
        self.op_from(ms, CallerInfo::default(), op)
    }

    /// From `ms` until `until` the store refuses writes; the core reloads what it saved.
    pub fn refuse_writes(&mut self, ms: i64, until: i64) -> Vec<Effect> {
        let mut snapshot = Snapshot {
            tasks: self.st.tasks().cloned().collect(),
            reports: self.st.reports().cloned().collect(),
            threads: self.st.threads().cloned().collect(),
            terminals: self.terminals.clone(),
            ..self.b.snapshot()
        };
        let next = self.terminals.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
        snapshot.next_ids.terminal = next;
        let input = Input::Reload {
            snapshot: Box::new(snapshot),
            refuse_writes: Some(RefuseWrites {
                until: UtcMs(until),
                reason: "the disk is full".into(),
            }),
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }

    /// `task set ID --due DUE` at `ms`.
    pub fn set_due(&mut self, ms: i64, id: i64, due: Date) -> Vec<Effect> {
        self.req += 1;
        let op = Op::TaskSet(TaskSet {
            id,
            due: Some(due.to_string()),
            ..TaskSet::default()
        });
        let input = Input::Client {
            conn: ConnId(1),
            req: ReqId(self.req),
            caller: Default::default(),
            op,
        };
        handle_checked(&mut self.st, input, at(UtcMs(ms)))
    }
}

/// Task `id`, titled `title`, `status`, due `days` from `today` (none for `None`).
pub fn task(id: i64, title: &str, status: TaskStatus, today: Date, days: Option<i64>) -> Task {
    Task {
        id: TaskId(id),
        project_id: None,
        title: title.into(),
        status,
        blocked_on: None,
        today: None,
        session_id: None,
        note: None,
        who: None,
        due: days.map(|d| today.checked_add(d.days()).unwrap()),
        priority: None,
        created_at: UtcMs(1),
        updated_at: UtcMs(1),
        done_at: None,
    }
}

/// D10's tasks on `today`: two 2 days overdue, one 1 day overdue, one today and one
/// tomorrow; and one due in 2 days and one done, which aren't due by tomorrow.
pub fn five_due(today: Date) -> Vec<Task> {
    let todo = TaskStatus::Todo;
    vec![
        task(1, "Reply to Kite", todo, today, Some(-2)),
        task(2, "Renew the domain", TaskStatus::Active, today, Some(-2)),
        task(3, "Send the invoice", TaskStatus::Waiting, today, Some(-1)),
        task(4, "Merge PR #3", todo, today, Some(0)),
        task(5, "Book the venue", TaskStatus::Blocked, today, Some(1)),
        task(6, "Pack the booth", todo, today, Some(2)),
        task(7, "File the taxes", TaskStatus::Done, today, Some(-1)),
        task(8, "Plan Harbor", todo, today, None),
    ]
}

/// The `nags` rows `changes` saves.
pub fn nags(changes: &[Change]) -> Vec<Nag> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Nag(n) => Some(*n),
            _ => None,
        })
        .collect()
}

pub fn nag(task: i64, due: Date, day: Date) -> Nag {
    Nag {
        task_id: TaskId(task),
        due,
        day,
    }
}

// ---------- idle terminals ----------

/// Open terminal `id` named `name` in `cwd` (under the brain's home), running `command`
/// under `zsh`, opened at `opened` and touched at `touched`.
pub fn term(
    b: &TypicalBrain,
    id: i64,
    name: &str,
    cwd: &str,
    command: &str,
    opened: i64,
    touched: Option<i64>,
) -> Terminal {
    let g = generation();
    Terminal {
        id: TermId(id),
        server_pid: g.pid,
        server_start: g.start,
        pane_id: format!("%{}", 20 + id),
        opened_by: OpenedBy::Found,
        cwd: b.home.join(cwd).to_string_lossy().into_owned(),
        session_id: None,
        opened_at: UtcMs(opened),
        touched_at: touched.map(UtcMs),
        name: name.into(),
        note: None,
        timeout_at: None,
        shell: "zsh".into(),
        command: command.into(),
        command_since: UtcMs(opened),
        closed_at: None,
    }
}

/// D10's terminals at `b`'s time: #4 at its prompt, untouched for 5 h, and #7 running
/// `tail`, opened 3 h ago and never touched; and #8, touched an hour ago.
pub fn d10_terminals(b: &TypicalBrain) -> Vec<Terminal> {
    let now = b.now().wall.0;
    vec![
        term(
            b,
            4,
            "kite",
            "private/kite",
            "zsh",
            now - 9 * HOUR,
            Some(now - 5 * HOUR),
        ),
        term(
            b,
            7,
            "logs",
            "private/lantern",
            "tail",
            now - 3 * HOUR,
            None,
        ),
        term(
            b,
            8,
            "beacon",
            "private/beacon",
            "zsh",
            now - 9 * HOUR,
            Some(now - HOUR),
        ),
    ]
}

pub const IDLE_BLOCK: &str = "Idle terminals (untouched for 2 h or more; offer to close one that \
                          looks forgotten, as a suggestion with --do \"term close ID\"):\n\
                          - #4 kite · ~/private/kite · idle 5 h · at its prompt\n\
                          - #7 logs · ~/private/lantern · idle 3 h · running tail";
