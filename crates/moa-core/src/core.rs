//! `handle`: the one pure function the daemon runs. State and an input go in; effects come
//! out, in the order they must happen.

use std::collections::BTreeSet;

use crate::change::Change;
use crate::effect::{Effect, Input, LogLevel, LogLine, Publish, TimerKey};
use crate::model::{ActivityId, ActivityRow, ActivityWho, Event, EventId, EventKind};
use crate::op::OpError;
use crate::ops;
use crate::publish::{CoreEvent, Topic};
use crate::state::{State, Table};
use crate::time::start_of_next_day;
use crate::{Now, UtcMs};

/// The effects of one input. Persist first, then what people hear about it.
pub fn handle(state: &mut State, input: Input, now: Now) -> Vec<Effect> {
    state.set_now(now.wall);
    match input {
        Input::Startup(snapshot) => {
            state.refuse_writes = None;
            state.load(*snapshot);
            let mut fx = vec![Effect::Backup { day: now.local_day }, backup_timer(&now)];
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
            fx.extend(publish_all(state));
            fx
        }
        Input::Reload {
            snapshot,
            refuse_writes,
        } => {
            state.load(*snapshot);
            state.refuse_writes = refuse_writes;
            let mut fx = vec![backup_timer(&now)];
            fx.extend(publish_all(state));
            fx
        }
        Input::Timer(TimerKey::Backup) => {
            vec![Effect::Backup { day: now.local_day }, backup_timer(&now)]
        }
        Input::ZoneChanged => vec![backup_timer(&now)],
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
            caller,
            op,
        } => {
            let caller = state.caller(&caller);
            let mut tx = Tx::new(&now);
            let result = ops::run(state, &mut tx, &caller, op, &now);
            let Tx {
                changes,
                mut events,
                ..
            } = tx;
            // Handlers commit once, last: a refusal has applied nothing, so there is nothing
            // to persist that the reply would deny.
            debug_assert!(
                result.is_ok() || changes.is_empty(),
                "an op returned Err after committing"
            );
            if result.is_err() {
                events.clear();
            }
            let topics: BTreeSet<Topic> = changes
                .iter()
                .flat_map(|c| c.topics().iter().copied())
                .collect();
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
            fx.push(Effect::Reply { conn, req, result });
            fx
        }
    }
}

fn backup_timer(now: &Now) -> Effect {
    Effect::SetTimer {
        key: TimerKey::Backup,
        at: start_of_next_day(now.local_day, &now.tz),
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
    wall: UtcMs,
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
            if state
                .unknown()
                .iter()
                .any(|n| n.table == table && n.key == key)
            {
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
        mut changes: Vec<Change>,
        text: String,
    ) -> Result<(), OpError> {
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
        changes.push(Change::Activity(ActivityRow {
            id: ActivityId(self.next_id(state, Table::Activity)),
            at: self.wall,
            who: ActivityWho::You,
            text: text.clone(),
            ref_kind: None,
            ref_id: None,
        }));
        self.commit(state, changes)?;
        self.events.push(CoreEvent::Ui { text });
        Ok(())
    }
}

/// `<table word> <key> has a value this build doesn't know; update moa`.
fn unknown_message(state: &State, table: &str, key: &str) -> String {
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
