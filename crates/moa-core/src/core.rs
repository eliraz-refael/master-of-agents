//! `handle`: the one pure function the daemon runs. State and an input go in; effects come
//! out, in the order they must happen.

use std::collections::BTreeSet;

use crate::change::Change;
use crate::effect::{Effect, Input, LogLevel, LogLine, Publish, TimerKey};
use crate::hooks;
use crate::live::{QUIET_AFTER, quiet_candidate};
use crate::model::{ActivityId, ActivityRow, ActivityWho, Event, EventId, EventKind};
use crate::op::{Op, OpError};
use crate::ops;
use crate::publish::{CoreEvent, Topic};
use crate::state::{State, Table};
use crate::time::start_of_next_day;
use crate::tmux::focus::mark_seen;
use crate::tmux::link;
use crate::tmux::status;
use crate::{Now, UtcMs};

/// The effects of one input. Persist first, then what people hear about it.
pub fn handle(state: &mut State, input: Input, now: Now) -> Vec<Effect> {
    state.set_now(now.wall);
    let mut fx = decide(state, input, &now);
    // Last, so the status line shows what this input saved, after its `Persist`.
    status::run(state, &now, &mut fx);
    fx
}

fn decide(state: &mut State, input: Input, now: &Now) -> Vec<Effect> {
    match input {
        Input::Startup(snapshot) => {
            state.refuse_writes = None;
            state.load(*snapshot);
            let mut fx = vec![Effect::Backup { day: now.local_day }, backup_timer(now)];
            fx.push(quiet_timer(state, now.wall));
            // A row's notes sit together, one per reason; the line names no reason, so one
            // line per row.
            let mut rows: Vec<&crate::state::LoadNote> = state.unknown().iter().collect();
            rows.dedup_by(|a, b| a.table == b.table && a.key == b.key);
            fx.extend(rows.into_iter().map(|n| {
                // A group's key is its path: a name, so it stays out of the log.
                let message = if n.table == "groups" {
                    "a row in groups has a value this build doesn't know".to_string()
                } else {
                    format!(
                        "row {} in {} has a value this build doesn't know",
                        n.key, n.table
                    )
                };
                Effect::Log(LogLine {
                    level: LogLevel::Warn,
                    message,
                })
            }));
            // A suggestion whose action no longer parses is flagged and saved, before anyone
            // is told about the state.
            let flags = ops::suggestions::startup_flags(state);
            if !flags.is_empty() {
                fx.extend(flags.iter().map(|s| {
                    Effect::Log(LogLine {
                        level: LogLevel::Warn,
                        message: format!("suggestion #{} has an action that doesn't parse", s.id),
                    })
                }));
                let changes: Vec<Change> = flags.into_iter().map(Change::Suggestion).collect();
                for c in &changes {
                    state.apply(c);
                }
                fx.push(Effect::Persist(changes));
            }
            fx.extend(publish_all(state));
            fx.extend(state.tmux.startup());
            fx
        }
        Input::Reload {
            snapshot,
            refuse_writes,
        } => {
            state.load(*snapshot);
            state.forget_hooks();
            state.refuse_writes = refuse_writes;
            let mut fx = vec![backup_timer(now), quiet_timer(state, now.wall)];
            fx.extend(publish_all(state));
            fx.extend(state.tmux.reload());
            fx.extend(ops::sessions::lost_opens(state));
            fx
        }
        Input::Tmux(t) => {
            // A pane that exited ends its session, before the link looks at the tree.
            let mut fx = match link::exited_pane(&t) {
                Some(pane) => {
                    let mut tx = Tx::new(now);
                    let saved = hooks::pane_gone(state, &mut tx, pane);
                    let mut fx = post_commit(state, tx);
                    if !saved {
                        fx.push(Effect::Log(LogLine {
                            level: LogLevel::Warn,
                            message: "a pane exit was not saved; reconcile will end its session at the next start".into(),
                        }));
                    }
                    if !fx.is_empty() {
                        fx.push(quiet_timer(state, now.wall));
                    }
                    fx
                }
                None => Vec::new(),
            };
            // Opens this answers, while the link is as they knew it. Never the same input as
            // a pane exit, so this commit is the input's only one.
            let settled = state.tmux.settle(&t);
            if !settled.is_empty() {
                fx.extend(ops::sessions::opened(state, now, settled));
            }
            fx.extend(state.tmux.input(t));
            // Whatever moved, a session a user can now see becomes seen.
            mark_seen_into(state, now, &mut fx);
            fx
        }
        Input::Hook { reply, envelope } => {
            let mut fx = hooks::run(state, now, reply, &envelope);
            if fx.iter().any(|e| matches!(e, Effect::Persist(_))) {
                fx.push(quiet_timer(state, now.wall));
            }
            fx
        }
        Input::Timer(TimerKey::Backup) => {
            vec![Effect::Backup { day: now.local_day }, backup_timer(now)]
        }
        // The status clock reads the time when `handle` ends.
        Input::Timer(TimerKey::Clock) => {
            state.tmux.clock_fired();
            Vec::new()
        }
        // Sessions that just went quiet show differently: tell the views.
        Input::Timer(TimerKey::Quiet) => {
            let topics = BTreeSet::from([Topic::Sessions, Topic::Status]);
            let mut fx: Vec<Effect> = state
                .publish(&topics)
                .map(|m| Effect::Publish(Publish::Model(m)))
                .into_iter()
                .collect();
            fx.push(quiet_timer(state, now.wall));
            fx
        }
        Input::ZoneChanged => vec![backup_timer(now)],
        Input::ActorFailed { actor, message: _ } => {
            // The message may carry contents; it stays with the executor.
            vec![
                Effect::Publish(Publish::Event(CoreEvent::Error {
                    message: format!("{actor} stopped and was restarted"),
                })),
                Effect::Log(LogLine {
                    level: LogLevel::Error,
                    message: format!("actor {actor} failed and was restarted"),
                }),
            ]
        }
        Input::Client {
            conn,
            req,
            op: Op::TmuxEnsure { tmux },
            ..
        } => state.tmux.ensure(conn, req, tmux),
        // Answered once tmux says how the new tab turned out. Every tab opens detached
        // (`-d`) for now, so `bg` changes nothing yet.
        Input::Client {
            conn,
            req,
            op:
                Op::SessionOpen {
                    path,
                    account,
                    title,
                    group,
                    prompt,
                    bg: _,
                },
            ..
        } => {
            let mut tx = Tx::new(now);
            let asked = ops::sessions::OpenArgs {
                path: &path,
                account: account.as_deref(),
                title: title.as_deref(),
                group: group.as_deref(),
                prompt: prompt.as_deref(),
            };
            let result = ops::sessions::open(state, &mut tx, now, &asked);
            let mut fx = post_commit(state, tx);
            match result {
                Ok((id, new_window)) => {
                    state.tmux.wait_open(id, conn, req);
                    fx.push(new_window);
                }
                Err(e) => fx.push(Effect::Reply {
                    conn,
                    req,
                    result: Err(e),
                }),
            }
            fx
        }
        Input::Client {
            conn,
            req,
            caller,
            op,
        } => {
            let caller = state.caller(&caller);
            let mut tx = Tx::new(now);
            let result = ops::run(state, &mut tx, &caller, op, now);
            // Handlers commit once, last: a refusal has applied nothing, so there is nothing
            // to persist that the reply would deny.
            debug_assert!(
                result.is_ok() || tx.changes.is_empty(),
                "an op returned Err after committing"
            );
            if result.is_err() {
                tx.events.clear();
            }
            let mut fx = post_commit(state, tx);
            fx.push(Effect::Reply { conn, req, result });
            fx
        }
    }
}

/// What a transaction's commits owe the world, in order: the `Persist`, the model it
/// changed, then its events. The caller adds any reply after.
pub(crate) fn post_commit(state: &mut State, tx: Tx) -> Vec<Effect> {
    let Tx {
        changes, events, ..
    } = tx;
    let topics = topics_of(&changes);
    let mut fx = Vec::new();
    if !changes.is_empty() {
        fx.push(Effect::Persist(changes));
    }
    if let Some(model) = state.publish(&topics) {
        fx.push(Effect::Publish(Publish::Model(model)));
    }
    fx.extend(
        events
            .into_iter()
            .map(|e| Effect::Publish(Publish::Event(e))),
    );
    fx
}

/// Runs `mark_seen` and adds what it changed to `fx`: the `Persist` after any `Persist`
/// already there (the store applies them in the order they were made) and before any
/// reply or view, and the `Publish` after.
pub(crate) fn mark_seen_into(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let mut tx = Tx::new(now);
    mark_seen(state, &mut tx, now);
    if tx.changes.is_empty() {
        return;
    }
    let topics = topics_of(&tx.changes);
    let at = fx.iter().position(Effect::is_heard).unwrap_or(fx.len());
    fx.insert(at, Effect::Persist(tx.changes));
    fx.extend(
        state
            .publish(&topics)
            .map(|m| Effect::Publish(Publish::Model(m))),
    );
}

/// The topics a set of changes touches.
fn topics_of(changes: &[Change]) -> BTreeSet<Topic> {
    changes
        .iter()
        .flat_map(|c| c.topics().iter().copied())
        .collect()
}

fn backup_timer(now: &Now) -> Effect {
    Effect::SetTimer {
        key: TimerKey::Backup,
        at: start_of_next_day(now.local_day, &now.tz),
    }
}

/// The one deadline for sessions turning quiet: the earliest `activity_at + QUIET_AFTER`
/// still ahead among sessions that are working, or no timer when there is none.
pub(crate) fn quiet_timer(state: &State, now: UtcMs) -> Effect {
    let next = state
        .sessions()
        .filter(|s| quiet_candidate(s))
        .filter_map(|s| s.activity_at)
        .map(|t| UtcMs(t.0 + QUIET_AFTER))
        .filter(|at| *at > now)
        .min();
    match next {
        Some(at) => Effect::SetTimer {
            key: TimerKey::Quiet,
            at,
        },
        None => Effect::CancelTimer(TimerKey::Quiet),
    }
}

fn publish_all(state: &mut State) -> Option<Effect> {
    let all: BTreeSet<Topic> = Topic::ALL.iter().copied().collect();
    state
        .publish(&all)
        .map(|m| Effect::Publish(Publish::Model(m)))
}

/// One op's edits. Handlers validate, build their changes, then call [`Tx::commit`] once: a
/// handler that returns `Err` has applied nothing.
pub(crate) struct Tx {
    pub(crate) wall: UtcMs,
    /// The local date of `wall`.
    pub(crate) today: jiff::civil::Date,
    /// Everything committed so far, in order.
    pub(crate) changes: Vec<Change>,
    pub(crate) events: Vec<CoreEvent>,
    /// The last id handed out per table, for ids asked for before the commit.
    issued: Vec<(Table, i64)>,
}

impl Tx {
    pub(crate) fn new(now: &Now) -> Tx {
        Tx {
            wall: now.wall,
            today: now.local_day,
            changes: Vec::new(),
            events: Vec::new(),
            issued: Vec::new(),
        }
    }

    /// Checks every change, then applies all of them to `state` and records them. A refusal
    /// applies none.
    pub(crate) fn commit(
        &mut self,
        state: &mut State,
        changes: Vec<Change>,
    ) -> Result<(), OpError> {
        if let Some(r) = state.refuse_writes()
            && self.wall < r.until
        {
            return Err(OpError(format!(
                "moa can't save right now ({}); nothing was changed",
                r.reason
            )));
        }
        // Most brains have no unknown rows; then no key needs building.
        let checked = if state.unknown().is_empty() {
            &[][..]
        } else {
            &changes[..]
        };
        for c in checked {
            let Some((table, key)) = c.row() else {
                continue;
            };
            if state.is_unknown(table, &key) {
                return Err(OpError(unknown_message(state, table, &key)));
            }
        }
        for c in &changes {
            state.apply(c);
        }
        self.changes.extend(changes);
        Ok(())
    }

    /// The id the next row of `table` takes, counting ids already handed out by this
    /// transaction.
    pub(crate) fn next_id(&mut self, state: &State, table: Table) -> i64 {
        let want = state.next_id(table);
        match self.issued.iter_mut().find(|(t, _)| *t == table) {
            Some((_, last)) => {
                *last = (*last + 1).max(want);
                *last
            }
            None => {
                self.issued.push((table, want));
                want
            }
        }
    }

    /// Commits `changes` together with a `ui` event and its activity row (`who = you`, the
    /// same text), as the spike's `log_ui` and `a_event` trigger did, and queues the
    /// `Ui` event for the views.
    pub(crate) fn ui(
        &mut self,
        state: &mut State,
        changes: Vec<Change>,
        text: String,
    ) -> Result<(), OpError> {
        self.ui_each(state, changes, vec![text])
    }

    /// An activity row stamped with this transaction's time, for the caller to commit.
    /// `of` is the `(ref_kind, ref_id)` the row points at.
    pub(crate) fn activity_row(
        &mut self,
        state: &State,
        who: ActivityWho,
        text: String,
        of: Option<(&str, String)>,
    ) -> Change {
        let (ref_kind, ref_id) = match of {
            Some((kind, id)) => (Some(kind.to_string()), Some(id)),
            None => (None, None),
        };
        Change::Activity(ActivityRow {
            id: ActivityId(self.next_id(state, Table::Activity)),
            at: self.wall,
            who,
            text,
            ref_kind,
            ref_id,
        })
    }

    /// [`Tx::ui`] with one `ui` event and activity row per text, all in one commit.
    pub(crate) fn ui_each(
        &mut self,
        state: &mut State,
        mut changes: Vec<Change>,
        texts: Vec<String>,
    ) -> Result<(), OpError> {
        for text in &texts {
            changes.push(Change::Event(Event {
                id: EventId(self.next_id(state, Table::Event)),
                at: self.wall,
                session_id: None,
                conversation_id: None,
                account: None,
                cwd: None,
                kind: EventKind(EventKind::UI.to_string()),
                text: Some(text.clone()),
                detail: None,
            }));
            changes.push(self.activity_row(state, ActivityWho::You, text.clone(), None));
        }
        self.commit(state, changes)?;
        self.events
            .extend(texts.into_iter().map(|text| CoreEvent::Ui { text }));
        Ok(())
    }
}

/// `<table word> <key> has a value this build doesn't know; update moa`.
pub(crate) fn unknown_message(state: &State, table: &str, key: &str) -> String {
    let word = match table {
        "projects" => "project",
        "tasks" => "task",
        "notes" => "note",
        "suggestions" => "suggestion",
        "sessions" => "session",
        "conversations" => "conversation",
        "groups" => "group",
        "asks" => "ask",
        "moa_threads" => "thread",
        "nags" => "nag",
        "pane_bindings" | "spike_panes" => "pane",
        other => other,
    };
    let key = match table {
        "sessions" => key
            .parse()
            .map(|id| state.handle_of(id))
            .unwrap_or_else(|_| key.to_string()),
        _ => key.to_string(),
    };
    format!("{word} {key} has a value this build doesn't know; update moa")
}

#[cfg(test)]
mod tests;
