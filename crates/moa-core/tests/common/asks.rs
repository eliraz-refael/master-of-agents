//! Asks between agents: three live sessions (Kite, Harbor, Beacon) and an ended one
//! (Lantern), their link played by hand, and what an ask's tests read back.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, RefuseWrites, TimerKey, TmuxIn, handle_checked};
use moa_core::hook::HookKind;
use moa_core::model::{
    Activity, AgentKind, Ask, AskId, AskStatus, AskTarget, Conversation, ConversationReason,
    Lifecycle, Location, PaneBinding, Session, SessionId,
};
use moa_core::op::{ConnId, Done, Op, OpError, ReqId};
use moa_core::state::{CallerInfo, Deps, Snapshot, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::ClientName;
use moa_core::{Now, UtcMs};

use super::{envelope, generation, timers};

pub const HOME: &str = "/tmp/moa-asks-test/home";

pub const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
"#;

/// The moment every test starts at.
pub const T0: i64 = 1_790_000_000_000;
pub const SEC: i64 = 1_000;
pub const MIN: i64 = 60 * SEC;

pub const KITE: &str = "00000000-0000-4000-8000-0000000000a1";
pub const HARBOR: &str = "00000000-0000-4000-8000-0000000000a2";
pub const BEACON: &str = "00000000-0000-4000-8000-0000000000a3";
/// Ended, and bound to no tab.
pub const LANTERN: &str = "00000000-0000-4000-8000-0000000000a4";

/// Each session's conversation: its handle is the first 8 characters.
pub fn conversation_of(id: &str) -> &'static str {
    match id {
        KITE => "7c41d0e2-0000-4000-8000-0000000000a1",
        HARBOR => "3f2a91c4-0000-4000-8000-0000000000a2",
        BEACON => "b9e0c3d5-0000-4000-8000-0000000000a3",
        LANTERN => "5d8e2f10-0000-4000-8000-0000000000a4",
        _ => "e0e0e0e0-0000-4000-8000-0000000000ff",
    }
}

pub fn sid(id: &str) -> SessionId {
    id.parse().unwrap()
}

pub fn now(ms: i64) -> Now {
    Now::at(UtcMs(ms), Duration::ZERO, TimeZone::UTC)
}

/// A live session titled `title` in `~/private/<folder>`, `activity` since a minute before
/// T0.
pub fn session(id: &str, title: &str, folder: &str, activity: Activity) -> Session {
    let cwd = format!("{HOME}/private/{folder}");
    Session {
        id: sid(id),
        agent_kind: AgentKind::Claude,
        account: Some("acct-a".into()),
        cwd: Some(cwd.clone()),
        start_cwd: Some(cwd),
        project_id: None,
        title: Some(title.into()),
        title_locked: false,
        group_path: None,
        lifecycle: Lifecycle::Live,
        activity,
        activity_at: Some(UtcMs(T0 - MIN)),
        activity_why: Some("Looking at the tide tables".into()),
        last_prompt: None,
        last_turn_end_at: None,
        seen_at: None,
        asleep_at: None,
        archived_at: None,
        location: Location::Here,
        resume_failed: false,
        created_at: UtcMs(1),
        last_seen_at: Some(UtcMs(T0 - MIN)),
    }
}

pub fn conversation(id: &str) -> Conversation {
    Conversation {
        agent_kind: AgentKind::Claude,
        conversation_id: conversation_of(id).into(),
        session_id: sid(id),
        store_dir: None,
        spike_account: None,
        reason: ConversationReason::Start,
        first_seen_at: UtcMs(1),
        last_seen_at: UtcMs(1),
        transcript_path: None,
        transcript_present: None,
        transcript_checked_at: None,
    }
}

pub fn binding(id: &str, pane: &str) -> PaneBinding {
    PaneBinding {
        session_id: sid(id),
        server_pid: 10,
        server_start: 1000,
        socket: "moa".into(),
        pane_id: pane.into(),
        pane_pid: 100,
        bound_at: UtcMs(1),
        tracked: true,
    }
}

/// Ask `id` from `from` to `to`, saved at `at`, `status`.
pub fn ask_row(
    id: i64,
    from: AskTarget,
    from_name: &str,
    to: AskTarget,
    text: &str,
    at: i64,
) -> Ask {
    Ask {
        id: AskId(id),
        at: UtcMs(at),
        from_session: Some(from),
        from_name: Some(from_name.into()),
        to_session: Some(to),
        text: text.into(),
        status: AskStatus::Pending,
        reply: None,
        claimed_at: None,
        answered_at: None,
    }
}

pub fn to(id: &str) -> AskTarget {
    AskTarget::Session(sid(id))
}

/// Kite (idle, `%1`), Harbor (working, `%2`) and Beacon (idle, `%3`), live; Lantern ended
/// with no tab.
pub fn snapshot() -> Snapshot {
    let mut lantern = session(LANTERN, "Lantern", "lantern", Activity::Idle);
    lantern.lifecycle = Lifecycle::Ended;
    let ids = [KITE, HARBOR, BEACON, LANTERN];
    Snapshot {
        sessions: vec![
            session(KITE, "Kite", "kite", Activity::Idle),
            session(HARBOR, "Harbor", "harbor", Activity::Working),
            session(BEACON, "Beacon", "beacon", Activity::Idle),
            lantern,
        ],
        conversations: ids.iter().map(|id| conversation(id)).collect(),
        pane_bindings: vec![
            binding(KITE, "%1"),
            binding(HARBOR, "%2"),
            binding(BEACON, "%3"),
        ],
        ..Snapshot::default()
    }
}

pub struct Rig {
    pub st: State,
    pub req: u64,
    /// The rows the rig started from, which a reload loads again (asks aside).
    pub base: Snapshot,
    /// The last hook's sequence number.
    pub seq: u32,
}

impl Rig {
    pub fn new(snapshot: Snapshot) -> Rig {
        Rig::started_at(snapshot, T0).0
    }

    /// The daemon starts at `ms` from `snapshot`, its link not yet up; what its start said
    /// too.
    pub fn started_at(snapshot: Snapshot, ms: i64) -> (Rig, Vec<Effect>) {
        let home = PathBuf::from(HOME);
        let config = Config::parse(TOML, &home).unwrap();
        let deps = Deps {
            home,
            ..Deps::default()
        };
        let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
        let fx = handle_checked(&mut st, Input::Startup(Box::new(snapshot.clone())), now(ms));
        let rig = Rig {
            st,
            req: 0,
            base: snapshot,
            seq: 0,
        };
        (rig, fx)
    }

    /// [`Rig::new`], its link up at T0: an ask can be typed.
    pub fn linked(snapshot: Snapshot) -> Rig {
        let mut r = Rig::new(snapshot);
        r.link_up(T0);
        r
    }

    /// The link comes up at `ms`, on connection 1, to the server the tabs are bound on.
    pub fn link_up(&mut self, ms: i64) -> Vec<Effect> {
        let connected = TmuxIn::Connected {
            conn: 1,
            generation: generation(),
            version: "3.5a".into(),
            ctl: ClientName("client-9".into()),
            socket: "moa".into(),
        };
        handle_checked(&mut self.st, Input::Tmux(connected), now(ms))
    }

    /// Session `who`'s hook of `kind` at `ms`.
    pub fn hook(&mut self, ms: i64, who: &str, kind: HookKind) -> Vec<Effect> {
        self.seq += 1;
        let envelope = envelope(who, conversation_of(who), UtcMs(ms), self.seq, kind);
        let input = Input::Hook {
            reply: None,
            envelope,
        };
        handle_checked(&mut self.st, input, now(ms))
    }

    /// Session `who`'s turn starts at `ms` with `prompt`.
    pub fn started(&mut self, ms: i64, who: &str, prompt: &str) -> Vec<Effect> {
        let kind = HookKind::TurnStarted {
            prompt: prompt.into(),
        };
        self.hook(ms, who, kind)
    }

    /// Session `who`'s turn ends at `ms`, its last message `message`.
    pub fn stopped(&mut self, ms: i64, who: &str, message: &str) -> Vec<Effect> {
        let kind = HookKind::TurnEnded {
            last_message: message.into(),
        };
        self.hook(ms, who, kind)
    }

    /// `op` from `caller` on connection `conn`, at `ms`.
    pub fn op(&mut self, conn: u64, caller: CallerInfo, op: Op, ms: i64) -> Vec<Effect> {
        self.req += 1;
        let input = Input::Client {
            conn: ConnId(conn),
            req: ReqId(self.req),
            caller,
            op,
        };
        handle_checked(&mut self.st, input, now(ms))
    }

    /// `caller` asks `to` `text` on connection `conn` at T0, waiting `timeout_s`.
    pub fn ask(
        &mut self,
        conn: u64,
        caller: CallerInfo,
        to: &str,
        text: &str,
        timeout_s: i64,
    ) -> Vec<Effect> {
        let op = Op::Ask {
            to: to.into(),
            text: text.into(),
            timeout_s,
        };
        self.op(conn, caller, op, T0)
    }

    /// The asks timer fires at `ms`.
    pub fn fire(&mut self, ms: i64) -> Vec<Effect> {
        handle_checked(&mut self.st, Input::Timer(TimerKey::Asks), now(ms))
    }

    /// The core reloads at `ms` with the rows it has, these asks put in by hand.
    pub fn reload(&mut self, ms: i64, asks: Vec<Ask>) -> Vec<Effect> {
        self.reload_refusing(ms, asks, None)
    }

    /// [`Rig::reload`], the store refusing writes until `until` when given.
    pub fn reload_refusing(&mut self, ms: i64, asks: Vec<Ask>, until: Option<i64>) -> Vec<Effect> {
        let mut snap = self.base.clone();
        snap.asks = asks;
        snap.next_ids.ask = snap.asks.iter().map(|a| a.id.0 + 1).max().unwrap_or(1);
        let input = Input::Reload {
            snapshot: Box::new(snap),
            refuse_writes: until.map(|until| RefuseWrites {
                until: UtcMs(until),
                reason: "the disk is full".into(),
            }),
        };
        handle_checked(&mut self.st, input, now(ms))
    }

    pub fn row(&self, id: i64) -> Ask {
        let topic = &self.st.model().topics[&moa_core::publish::Topic::Asks];
        serde_json::from_value(topic[&id.to_string()].clone()).unwrap()
    }
}

pub fn from(id: &str) -> CallerInfo {
    CallerInfo {
        moa_session: Some(id.into()),
        ..CallerInfo::default()
    }
}

pub fn moa() -> CallerInfo {
    from("moa")
}

/// A shell in a folder: no session, not moa.
pub fn shell() -> CallerInfo {
    CallerInfo {
        cwd: Some(PathBuf::from(format!("{HOME}/private/kite"))),
        ..CallerInfo::default()
    }
}

pub fn replies(fx: &[Effect]) -> Vec<(ConnId, ReqId, Result<Done, OpError>)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply { conn, req, result } => Some((*conn, *req, result.clone())),
            _ => None,
        })
        .collect()
}

pub fn the_error(fx: &[Effect]) -> String {
    match &replies(fx)[..] {
        [(_, _, Err(e))] => e.0.clone(),
        other => panic!("not one refusal: {other:?}"),
    }
}

pub fn saved_asks(fx: &[Effect]) -> Vec<Ask> {
    super::changes(fx)
        .into_iter()
        .filter_map(|c| match c {
            Change::Ask(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

pub fn persisted(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Persist(_)))
}

pub fn set_at(fx: &[Effect]) -> Option<UtcMs> {
    timers(fx, TimerKey::Asks)
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::SetTimer { at, .. } => Some(*at),
            _ => None,
        })
}

pub fn cancelled(fx: &[Effect]) -> bool {
    timers(fx, TimerKey::Asks)
        .iter()
        .any(|e| matches!(e, Effect::CancelTimer(TimerKey::Asks)))
}

/// The commands typed into panes in `fx`: those of its untokened `Tmux` effects that
/// `set-buffer`, `paste-buffer` or `send-keys`.
pub fn typed(fx: &[Effect]) -> Vec<Cmd> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds.iter()),
            _ => None,
        })
        .flatten()
        .filter(|c| ["set-buffer", "paste-buffer", "send-keys"].contains(&c.name()))
        .cloned()
        .collect()
}

/// Whether `fx` starts a process: a new window, a respawn, or a command for moa's agent.
pub fn launches(fx: &[Effect]) -> bool {
    fx.iter().any(|e| match e {
        Effect::Tmux { cmds, .. } => cmds
            .iter()
            .flat_map(Cmd::parts)
            .any(|c| ["new-window", "respawn-pane", "split-window"].contains(&c.name())),
        Effect::Acp(_) => true,
        _ => false,
    })
}
