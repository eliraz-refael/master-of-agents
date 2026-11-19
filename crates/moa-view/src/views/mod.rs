//! One builder per view: the data, the clock and the panel's own state in, the view's lines
//! out (V2), built for the body's width.

use std::collections::BTreeSet;

use moa_core::live::DisplayState;
use moa_core::model::ProjectId;
use moa_core::panel::View;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Tone};
use crate::text::{self, SPIN, clean, cut, short_account};

pub mod activity;
pub mod empty_slot;
pub mod picture;
pub mod plan;
pub mod projects;
pub mod status;
pub mod today;
pub mod tree;

/// What a view keeps in the panel rather than in the brain: the projects opened in the
/// projects view, and the tab the picture shows (an index into [`picture::TABS`]).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ViewUi {
    pub expanded: BTreeSet<ProjectId>,
    pub tab: usize,
}

/// The lines of `view`, `width` cells wide.
pub fn build(view: View, data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    match view {
        View::Tree => tree::build(data, ctx, ui, width),
        View::Plan => plan::build(data, ctx, ui, width),
        View::Status => status::build(data, ctx, ui, width),
        View::Today => today::build(data, ctx, ui, width),
        View::Projects => projects::build(data, ctx, ui, width),
        View::Activity => activity::build(data, ctx, ui, width),
        View::Picture => picture::build(data, ctx, ui, width),
        View::EmptySlot => empty_slot::build(width),
        View::Console | View::Picker | View::Help | View::Setup => {
            let words = not_yet(view).unwrap_or_default();
            vec![Line::plain(cut(words, usize::from(width)), Tone::Dim)]
        }
    }
}

/// What a view this build doesn't draw says instead (V15), in the wording of the key that
/// opens it.
pub fn not_yet(view: View) -> Option<&'static str> {
    match view {
        View::Console => Some("the console isn't in this build yet"),
        View::Picker => Some("finding a session isn't in this build yet"),
        View::Help => Some("help isn't in this build yet"),
        View::Setup => Some("setup isn't in this build yet"),
        View::EmptySlot
        | View::Tree
        | View::Plan
        | View::Status
        | View::Today
        | View::Activity
        | View::Projects
        | View::Picture => None,
    }
}

/// What a status or today line calls a session (the spike's): its title, else its project's
/// name, else its folder's last part, else its handle; cleaned.
pub fn label(row: &SessionRow) -> String {
    let s = &row.session;
    let folder = s
        .cwd
        .as_deref()
        .and_then(|c| c.rsplit('/').find(|p| !p.is_empty()));
    let name = [s.title.as_deref(), row.project.as_deref(), folder]
        .into_iter()
        .flatten()
        .find(|n| !n.is_empty())
        .unwrap_or(&row.handle);
    clean(name)
}

/// The sessions `keep` keeps, the most recently seen first.
pub fn newest_first(data: &Data, keep: impl Fn(&SessionRow) -> bool) -> Vec<&SessionRow> {
    let mut rows: Vec<&SessionRow> = data.sessions.values().filter(|r| keep(r)).collect();
    rows.sort_by(|a, b| by_newest(a, b));
    rows
}

/// The most recently seen session first (none last), then by id.
pub fn by_newest(a: &SessionRow, b: &SessionRow) -> std::cmp::Ordering {
    b.session
        .last_seen_at
        .cmp(&a.session.last_seen_at)
        .then(a.session.id.cmp(&b.session.id))
}

/// `<indent><glyph> <title or project> [<account>] <age>  <prompt>` in `tone` (the spike's
/// `session_line`), as today and an open project list a session; a prompt that is a command
/// or a system message (`<…`) isn't words for you, so it is left out.
pub fn session_line(row: &SessionRow, ctx: &Ctx, width: usize, tone: Tone, indent: &str) -> Line {
    let s = &row.session;
    let account = clean(short_account(s.account.as_deref().unwrap_or_default()));
    let age = text::ago(s.last_seen_at, ctx.now);
    let mut text = format!(
        "{indent}{} {} [{account}] {age:>3}",
        state_glyph(row.display_state, ctx.tick),
        label(row)
    );
    let prompt = s.last_prompt.as_deref().map(clean).unwrap_or_default();
    if !prompt.trim().is_empty() && !prompt.starts_with('<') {
        text.push_str("  ");
        text.push_str(&prompt);
    }
    let mut line = Line::plain(cut(&text, width), tone).with_item(Item::Session(s.id));
    line.animated = animates(row.display_state);
    line.changes_at = s
        .last_seen_at
        .map(|seen| text::next_ago_change(seen, ctx.now));
    line
}

/// The spinner's frame at `tick`.
pub fn spinner(tick: u64) -> &'static str {
    SPIN[(tick % SPIN.len() as u64) as usize]
}

/// A state's glyph at `tick`: working spins and waiting blinks every half second, `!` then
/// `⚠`, as the spike's did (`"⚠" if int(t * 2) % 2 else "!"`); the rest are still.
pub fn state_glyph(state: DisplayState, tick: u64) -> &'static str {
    match state {
        DisplayState::Working => spinner(tick),
        DisplayState::Waiting if (tick / 5) % 2 == 1 => "⚠",
        DisplayState::Waiting => "!",
        _ => state.glyph(),
    }
}

/// Whether a state's glyph moves.
pub fn animates(state: DisplayState) -> bool {
    matches!(state, DisplayState::Working | DisplayState::Waiting)
}

/// A session running now, in any state (the spike's `LIVE`, with v1's question, failed,
/// done and quiet).
pub fn live(state: DisplayState) -> bool {
    matches!(
        state,
        DisplayState::Waiting
            | DisplayState::Question
            | DisplayState::Failed
            | DisplayState::Done
            | DisplayState::Working
            | DisplayState::Quiet
            | DisplayState::Idle
    )
}
