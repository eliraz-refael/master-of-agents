//! The terminals view (5d, T9): every terminal moa knows, with what is running in it, when you
//! last touched it and how long it has been open, and the keys to jump to one, close it, name
//! it, note it and set its timeout. A terminal you haven't touched for a while is dimmed and
//! marked `idle`.

use moa_core::UtcMs;
use moa_core::publish::{TermRow, default_idle_ms};
use moa_core::terminals::is_idle;
use moa_core::time::until;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, fit, fit_right};
use crate::views::picker::{cut_spans, tab_number, term_folder, term_title};

/// The width of the `what` column: the program and how long it has run, or `shell`, and the
/// session it belongs to.
const WHAT: usize = 22;

/// The idle setting: the brain's, until its row comes.
fn idle_ms(data: &Data) -> i64 {
    data.brain
        .as_ref()
        .map_or_else(default_idle_ms, |b| b.terminal_idle_ms)
}

/// When a terminal was last touched, else opened: what idleness counts from.
fn last(row: &TermRow) -> UtcMs {
    UtcMs(row.touched_at.unwrap_or(row.opened_at))
}

fn row_idle(row: &TermRow, idle_ms: i64, now: UtcMs) -> bool {
    row.opened_at > 0 && is_idle(last(row), row.active, idle_ms, now)
}

/// The terminals in the view's order: the ones you are looking at first, then the most
/// recently touched (or opened, when never touched), newest first; ties by tab number.
pub fn rows(data: &Data) -> Vec<(&String, &TermRow)> {
    let mut rows: Vec<(&String, &TermRow)> = data.terms.iter().collect();
    rows.sort_by_key(|(pane, t)| {
        (
            !t.active,
            std::cmp::Reverse(last(t)),
            tab_number(pane),
            *pane,
        )
    });
    rows
}

/// The view's lines, `width` cells wide: a dim count, then a row per terminal; with none,
/// only `no terminals open`.
pub fn lines(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let rows = rows(data);
    if rows.is_empty() {
        return vec![Line::plain(cut("no terminals open", width), Tone::Dim)];
    }
    let idle_ms = idle_ms(data);
    let idle_count = rows
        .iter()
        .filter(|(_, t)| row_idle(t, idle_ms, ctx.now))
        .count();
    let noun = if rows.len() == 1 {
        "terminal"
    } else {
        "terminals"
    };
    let head = format!("{} {noun} · {idle_count} idle", rows.len());
    let mut out = vec![Line::plain(cut(&head, width), Tone::Dim)];
    out.extend(
        rows.into_iter()
            .map(|(pane, t)| row_line(pane, t, idle_ms, ctx, width)),
    );
    out
}

/// What a terminal is running: `node 2h`, or `shell`, then `· <session>` for the session it
/// belongs to.
fn what(row: &TermRow, now: UtcMs) -> String {
    let mut out = if row.running {
        let program = clean(&row.command);
        match row.since {
            0 => program,
            since => format!("{program} {}", text::ago(Some(UtcMs(since)), now)),
        }
    } else {
        "shell".to_string()
    };
    if let Some(session) = &row.session {
        out.push_str(" · ");
        out.push_str(&clean(session));
    }
    out
}

/// ` ⌨ <name><folder><what><touched> <open>  idle  ⏱<until>  <note>`: the name in
/// `clamp(w/5, 8, 24)` cells, the folder in `clamp(w/5, 0, 28)` from 70 columns, `what` in
/// [`WHAT`], `touched` and `open` right-aligned in 4; an idle row is dim, and its note
/// always is.
fn row_line(pane: &str, row: &TermRow, idle_ms: i64, ctx: &Ctx, width: usize) -> Line {
    let nw = (width / 5).clamp(8, 24);
    let fw = if width >= 70 { (width / 5).min(28) } else { 0 };
    let idle = row_idle(row, idle_ms, ctx.now);
    let touched = match row.touched_at {
        _ if row.active => "now".to_string(),
        Some(at) => text::ago(Some(UtcMs(at)), ctx.now),
        None => "-".to_string(),
    };
    let opened = match row.opened_at {
        0 => String::new(),
        at => text::ago(Some(UtcMs(at)), ctx.now),
    };
    let mut main = format!(
        " {}{}{}{}{} {}",
        fit("⌨", 2),
        fit(&cut(&term_title(pane, row), nw - 1), nw),
        fit(
            &cut(&term_folder(&row.folder, ctx), fw.saturating_sub(1)),
            fw
        ),
        fit(&cut(&what(row, ctx.now), WHAT - 1), WHAT),
        fit_right(&touched, 4),
        fit_right(&opened, 4),
    );
    if idle {
        main.push_str("  idle");
    }
    if let Some(at) = row.timeout_at {
        main.push_str(&format!("  ⏱{}", until(UtcMs(at), ctx.now)));
    }
    let tone = if idle { Tone::Dim } else { Tone::Text };
    let mut spans = vec![Span::new(main, tone)];
    if let Some(note) = row.note.as_deref().map(clean).filter(|n| !n.is_empty()) {
        spans.push(Span::new(format!("  {note}"), Tone::Dim));
    }
    let mut line = Line::new(cut_spans(spans, width)).with_item(Item::Term {
        pane: pane.to_string(),
    });
    line.changes_at = changes_at(row, idle_ms, ctx.now);
    line
}

/// The soonest moment a text of the row changes: its ages, its countdown, the moment it
/// turns idle.
fn changes_at(row: &TermRow, idle_ms: i64, now: UtcMs) -> Option<UtcMs> {
    let ago = |at: i64| (at > 0).then(|| text::next_ago_change(UtcMs(at), now));
    let idles = (idle_ms > 0 && row.opened_at > 0 && !row.active)
        .then(|| UtcMs(last(row).0.saturating_add(idle_ms)));
    [
        row.touched_at.filter(|_| !row.active).and_then(ago),
        ago(row.opened_at),
        row.running.then(|| ago(row.since)).flatten(),
        row.timeout_at
            .and_then(|at| text::next_until_change(UtcMs(at), now)),
        idles,
    ]
    .into_iter()
    .flatten()
    .filter(|t| *t > now)
    .min()
}
