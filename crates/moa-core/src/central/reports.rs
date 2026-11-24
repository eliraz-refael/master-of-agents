//! The session reports queue: what a session tells moa's agent, kept in the database until a
//! wake-up, or your prompt, carries it. A hook event that is applied (not history only, not a
//! duplicate) may queue one report row; its rules are here, and the change goes in the hook's
//! own commit, so a report is never queued for an event that wasn't saved. So are the texts
//! that carry them (K11).

use std::path::Path;

use crate::Now;
use crate::central::PROMPT_MAX;
use crate::change::Change;
use crate::core::Tx;
use crate::hook::{HookEnvelope, HookKind};
use crate::model::{Report, ReportKind, Session, SessionId};
use crate::ops::launch::title_of;
use crate::state::{State, Table};
use crate::text::{cut, one_line};
use crate::time::local_format;

/// `text` cut to `n` characters, with a `…` after the cut: the stored text says it was cut.
fn cut_marked(text: &str, n: usize) -> String {
    match text.char_indices().nth(n) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// How many turns a session's `turn_ended` row keeps: the newest.
pub const TURNS_KEPT: usize = 6;

/// The most characters of the newest message a report keeps; a longer one ends in `…`.
pub const LAST_MAX: usize = 2600;

/// The most characters a report keeps of a turn once a newer one follows it; a longer one
/// ends in `…`.
pub const EARLIER_MAX: usize = 500;

/// The report `env` adds for `session` (the session as the event leaves it), or `None` when
/// it adds none: the event says nothing to moa (an empty message, a start, a failure, a
/// need cleared), or the session is moa's own agent's, whose folder is `Deps.agent_dir`.
///
/// A session has one row of each kind. A turn's message joins the row's turns, the newest
/// [`TURNS_KEPT`] kept; what a session needs replaces the last. The row keeps its `queued_at`,
/// its place in line, and `last_at` moves.
pub(crate) fn queued(
    state: &State,
    tx: &mut Tx,
    session: &Session,
    env: &HookEnvelope,
) -> Option<Change> {
    let (kind, text) = match &env.kind {
        HookKind::TurnEnded { last_message } => (ReportKind::TurnEnded, last_message.clone()),
        HookKind::NeedsPermission { tool } => (ReportKind::NeedsYou, format!("permission: {tool}")),
        HookKind::NeedsAnswer { what } => (ReportKind::NeedsYou, what.clone()),
        _ => return None,
    };
    let text = cut_marked(text.trim(), LAST_MAX);
    if text.is_empty() || is_moas_own(state, session) {
        return None;
    }
    let row = match state
        .reports()
        .find(|r| r.session_id == session.id && r.kind == kind)
    {
        Some(old) => {
            let mut turns = old.turns.clone();
            match kind {
                ReportKind::TurnEnded => {
                    // The turn that was the newest is now an earlier one.
                    if let Some(prev) = turns.last_mut() {
                        *prev = cut_marked(&*prev, EARLIER_MAX);
                    }
                    turns.push(text);
                    turns.drain(..turns.len().saturating_sub(TURNS_KEPT));
                }
                ReportKind::NeedsYou => turns = vec![text],
            }
            Report {
                last_at: env.at,
                turns,
                ..old.clone()
            }
        }
        None => Report {
            id: tx.next_id(state, Table::Report),
            session_id: session.id,
            kind,
            queued_at: env.at,
            last_at: env.at,
            turns: vec![text],
        },
    };
    Some(Change::Report(row))
}

/// The most characters of a session's title, or its project's name, in a report's heading.
const HEADING_FIELD_MAX: usize = 80;

/// Room in a prompt for the first and last lines of [`wake_text`] or [`prefix_text`].
const FRAME: usize = 200;

/// What one prompt carries: the queued reports in line, until `sessions` sessions, each
/// with every report of its own, and while their text stays within [`PROMPT_MAX`]. The first
/// session always goes; the rest wait for the next prompt.
pub fn batch<'a>(state: &'a State, now: &Now, sessions: usize) -> Vec<&'a Report> {
    // The sessions in line, each with its reports.
    let mut line: Vec<(SessionId, Vec<&Report>)> = Vec::new();
    for r in state.reports() {
        match line.iter_mut().find(|(s, _)| *s == r.session_id) {
            Some((_, rs)) => rs.push(r),
            None => line.push((r.session_id, vec![r])),
        }
    }
    let mut chosen: Vec<SessionId> = Vec::new();
    let mut size = FRAME;
    for (session, reports) in line.into_iter().take(sessions) {
        // Each section, and the blank line before it.
        let more: usize = reports
            .iter()
            .map(|r| section(state, now, r).chars().count() + 2)
            .sum();
        if !chosen.is_empty() && size + more > PROMPT_MAX {
            break;
        }
        size += more;
        chosen.push(session);
    }
    state
        .reports()
        .filter(|r| chosen.contains(&r.session_id))
        .collect()
}

/// A wake-up's prompt: `reports`, and what moa does with them.
pub fn wake_text(state: &State, now: &Now, reports: &[&Report]) -> String {
    format!(
        "(moa · a wake-up, not typed by the user) {} session report(s) since your last look:\n\n\
         {}\n\nUpdate the big picture (see \"Wake-ups\" in your instructions).",
        reports.len(),
        sections(state, now, reports)
    )
}

/// What goes before your prompt's text: `reports`.
pub fn prefix_text(state: &State, now: &Now, reports: &[&Report]) -> String {
    format!(
        "(moa · {} session report(s) since your last look, sent with the user's message)\n\n{}",
        reports.len(),
        sections(state, now, reports)
    )
}

fn sections(state: &State, now: &Now, reports: &[&Report]) -> String {
    let all: Vec<String> = reports.iter().map(|r| section(state, now, r)).collect();
    all.join("\n\n")
}

/// One report: a heading line, then its turns, the earlier ones marked, as the queue keeps
/// them (already cut). Each turn is a blockquote, so a heading a session wrote can't be read
/// as the start of another report.
fn section(state: &State, now: &Now, r: &Report) -> String {
    let session = state.session(r.session_id);
    let title = match session {
        Some(s) => title_of(state, s),
        None => state.handle_of(r.session_id),
    };
    let project = session
        .and_then(|s| s.project_id)
        .and_then(|p| state.projects.get(&p))
        .map_or_else(|| "(none)".to_string(), |p| heading_field(&p.name));
    let what = match r.kind {
        ReportKind::TurnEnded => "finished a turn",
        ReportKind::NeedsYou => "needs the user",
    };
    let mut text = format!(
        "### {} · project {project} · session {} · {what} at {}",
        heading_field(&title),
        state.handle_of(r.session_id),
        local_format(r.last_at, &now.tz, "%H:%M"),
    );
    if r.turns.len() > 1 {
        text += &format!(" ({} turns; earlier ones shortened)", r.turns.len());
    }
    if let Some((last, earlier)) = r.turns.split_last() {
        let turns: Vec<String> = earlier
            .iter()
            .map(|t| quoted(&format!("(earlier) {t}")))
            .chain([quoted(last)])
            .collect();
        text += "\n";
        text += &turns.join("\n\n");
    }
    text
}

/// `text` as a Markdown blockquote: `> ` before each line, `>` alone for an empty one.
fn quoted(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|l| match l {
            "" => ">".to_string(),
            l => format!("> {l}"),
        })
        .collect();
    lines.join("\n")
}

/// A name in a heading: one line, and at most [`HEADING_FIELD_MAX`] characters.
fn heading_field(name: &str) -> String {
    cut(&one_line(name), HEADING_FIELD_MAX)
}

/// Whether `session` works in moa's own agent's folder, or under it. Both paths are
/// canonical, so `/tmp` and `/private/tmp` agree.
fn is_moas_own(state: &State, session: &Session) -> bool {
    let deps = state.deps();
    session.cwd.as_deref().is_some_and(|cwd| {
        (deps.canonical)(Path::new(cwd)).starts_with((deps.canonical)(&deps.agent_dir))
    })
}
