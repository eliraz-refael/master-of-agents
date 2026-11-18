//! `suggest`, and `suggestion yes|no|later|ls`. A suggestion is moa's proposal with buttons;
//! `yes` runs its actions in-process, through the same `ops::run` and the same transaction as
//! a typed command, so the whole answer is one `Persist`.

use std::path::Path;

use serde_json::json;

use super::{brain_row, run};
use crate::Now;
use crate::activity;
use crate::change::Change;
use crate::core::{Tx, unknown_message};
use crate::effect::Effect;
use crate::model::{Suggestion, SuggestionAction, SuggestionId, SuggestionStatus};
use crate::op::{Answer, Done, OpError};
use crate::state::{Caller, State, Table};

const ANSWER_USAGE: &str = "moactl suggestion yes|no|later ID";
/// Why an open suggestion can't be run, as `Startup` records it.
pub(crate) const INVALID: &str = "an action doesn't parse as moactl";

/// The action as it was typed, shell-quoted.
fn joined(a: &SuggestionAction) -> String {
    shell_words::join(std::iter::once(a.verb.as_str()).chain(a.args.iter().map(String::as_str)))
}

/// Splits a typed action into verb and arguments.
fn split(act: &str) -> Result<SuggestionAction, String> {
    let mut words = shell_words::split(act).map_err(|e| e.to_string())?;
    // An empty action has no verb; the grammar refuses it with its list of verbs.
    let verb = if words.is_empty() {
        String::new()
    } else {
        words.remove(0)
    };
    Ok(SuggestionAction { verb, args: words })
}

/// Whether `Startup` should flag this suggestion: still waiting, not flagged, has actions,
/// and one of them doesn't parse. A row loaded with a default for an unknown value is left
/// alone, since a write would overwrite what the newer build stored.
///
/// Startup flags only once. If its commit later fails and the core reloads, the flag is gone
/// from memory; that is accepted, because `yes` parses every action again and records a bad
/// one as a failure instead of running it.
pub(crate) fn startup_flags(state: &State) -> Vec<Suggestion> {
    let deps = state.deps();
    let Some(parse) = deps.parse_action else {
        return Vec::new();
    };
    state
        .suggestions()
        .filter(|s| waiting(s))
        .filter(|s| s.actions_invalid.is_none() && !s.actions.is_empty())
        .filter(|s| !is_unknown(state, s.id))
        .filter(|s| s.actions.iter().any(|a| parse(a, &deps.home).is_err()))
        .map(|s| {
            let mut s = s.clone();
            s.actions_invalid = Some(INVALID.to_string());
            s
        })
        .collect()
}

fn is_unknown(state: &State, id: SuggestionId) -> bool {
    state.is_unknown("suggestions", &format!("#{id}"))
}

/// Open or later: not answered for good yet.
fn waiting(s: &Suggestion) -> bool {
    matches!(s.status, SuggestionStatus::Open | SuggestionStatus::Later)
}

pub(crate) fn suggest(
    state: &mut State,
    tx: &mut Tx,
    now: &Now,
    text: &str,
    actions: &[String],
) -> Result<Done, OpError> {
    let mut parsed = Vec::new();
    if !actions.is_empty() {
        let deps = state.deps();
        let Some(parse) = deps.parse_action else {
            return Err(OpError("suggestions need the moactl grammar".into()));
        };
        // Checked now, so a bad action fails for moa, not later for the user.
        for act in actions {
            let a = split(act)
                .and_then(|a| parse(&a, &deps.home).map(|_| a))
                .map_err(|why| OpError(format!("--do '{act}': {why}")))?;
            parsed.push(a);
        }
    }
    if let Some(dup) = state
        .suggestions()
        .find(|s| s.status == SuggestionStatus::Open && s.text == text)
    {
        return Ok(Done {
            text: format!("already suggested (#{})", dup.id),
            data: json!({ "id": dup.id, "duplicate": true }),
        });
    }
    let id = SuggestionId(tx.next_id(state, Table::Suggestion));
    let row = Suggestion {
        id,
        text: text.to_string(),
        status: SuggestionStatus::Open,
        actions: parsed,
        actions_raw: None,
        actions_invalid: None,
        result: None,
        created_at: now.wall,
        resolved_at: None,
    };
    let activity = brain_row(state, tx, now, activity::suggests(id.0, text));
    let data = serde_json::to_value(&row).unwrap_or_default();
    tx.commit(state, vec![Change::Suggestion(row), activity])?;
    Ok(Done {
        text: format!("suggestion #{id}: the user sees it with yes / later / no buttons"),
        data,
    })
}

pub(crate) fn ls(state: &State) -> Result<Done, OpError> {
    let open: Vec<&Suggestion> = state.suggestions().filter(|s| waiting(s)).collect();
    let lines: Vec<String> = open
        .iter()
        .map(|s| {
            let mut line = format!("#{} [{}] {}", s.id, s.status, s.text);
            if let Some(why) = &s.actions_invalid {
                line.push_str(&format!("  (yes can't run its actions: {why})"));
            } else if !s.actions.is_empty() {
                let acts: Vec<String> = s.actions.iter().map(joined).collect();
                line.push_str(&format!("  (yes runs: {})", acts.join("; ")));
            }
            line
        })
        .collect();
    Ok(Done {
        text: if lines.is_empty() {
            "no open suggestions".into()
        } else {
            lines.join("\n")
        },
        data: json!({ "suggestions": open }),
    })
}

pub(crate) fn answer(
    state: &mut State,
    tx: &mut Tx,
    caller: &Caller,
    now: &Now,
    id: Option<i64>,
    answer: Answer,
    logs: &mut Vec<Effect>,
) -> Result<Done, OpError> {
    let Some(s) = id
        .and_then(|id| state.suggestions().find(|s| s.id.0 == id))
        .cloned()
    else {
        return Err(OpError(ANSWER_USAGE.into()));
    };
    // Before any action runs: the actions would commit and then this row's own write be
    // refused.
    if is_unknown(state, s.id) {
        return Err(OpError(unknown_message(
            state,
            "suggestions",
            &format!("#{}", s.id),
        )));
    }
    if !waiting(&s) {
        return Err(OpError(format!(
            "suggestion #{} is already {}",
            s.id, s.status
        )));
    }
    if answer == Answer::Yes && s.actions_invalid.is_some() {
        return Err(OpError(format!(
            "suggestion #{} has actions moa can't run; answer no or later",
            s.id
        )));
    }
    let mut failed = Vec::new();
    if answer == Answer::Yes {
        for a in &s.actions {
            if let Err(why) = run_action(state, tx, caller, now, a, logs) {
                failed.push(format!("{}: {why}", joined(a)));
            }
        }
    }
    let (status, word) = match answer {
        Answer::Yes => (SuggestionStatus::Accepted, "done"),
        Answer::No => (SuggestionStatus::Rejected, "rejected"),
        Answer::Later => (SuggestionStatus::Later, "later"),
    };
    let failures = failed.join("; ");
    let mut row = s.clone();
    row.status = status;
    row.resolved_at = Some(now.wall);
    if answer == Answer::Yes {
        row.result = Some(if failed.is_empty() {
            "done".to_string()
        } else {
            failures.clone()
        });
    }
    let mut ui = format!("{status} suggestion #{}: {}", s.id, s.text);
    let mut text = word.to_string();
    if !failed.is_empty() {
        ui.push_str(&format!(" (failed: {failures})"));
        text.push_str(&format!("; but {failures}"));
    }
    let data = serde_json::to_value(&row).unwrap_or_default();
    tx.ui(state, vec![Change::Suggestion(row)], ui)?;
    Ok(Done { text, data })
}

/// One action of a `yes`. An `Err` is the reason it didn't run, and applied nothing.
fn run_action(
    state: &mut State,
    tx: &mut Tx,
    caller: &Caller,
    now: &Now,
    a: &SuggestionAction,
    logs: &mut Vec<Effect>,
) -> Result<(), String> {
    let deps = state.deps();
    let Some(parse) = deps.parse_action else {
        return Err("suggestions need the moactl grammar".into());
    };
    let home: &Path = &deps.home;
    let op = parse(a, home)?;
    run(state, tx, caller, op, now, logs)
        .map(|_| ())
        .map_err(|e| e.0)
}
