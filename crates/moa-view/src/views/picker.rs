//! The session picker (S7, the spike's `picker.py`): type part of a session's name, it
//! matches as fzf would, and Enter brings that session, or a terminal, to the screen. Live
//! sessions come first, the most urgent first (the status view's order), then the terminals,
//! then the rest, the most recently seen first.

use moa_core::UtcMs;
use moa_core::live::{DisplayState, sort_key};
use moa_core::model::Location;
use moa_core::publish::TermRow;
use unicode_segmentation::UnicodeSegmentation;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::line::{Item, Line, Span, Tone};
use crate::text::{self, clean, cut, fit, fit_right};
use crate::views::{
    ViewUi, animates, folder, label, live, newest_first, state_glyph, status, tree,
};

/// The most rows listed after the terminals (the spike's `LIMIT 300`).
pub const OLD_ROWS: usize = 300;

/// What the list says when no row matches the query.
pub const NO_MATCH: &str = "no session matches";

/// The longest query, in characters: more typed or pasted is dropped, so a stray paste
/// can't make every match slow.
pub const MAX_QUERY: usize = 200;

/// What a row is: a live session in its display state, a terminal, or one of the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A live session at home.
    Live(DisplayState),
    Term,
    Asleep,
    Ended,
    /// A session whose agent runs on another machine.
    Elsewhere,
}

impl Kind {
    /// Its word in the row: a live state's is its status heading's.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Live(state) => status::word(state),
            Kind::Term => "terminal",
            Kind::Asleep => "asleep",
            Kind::Ended => "ended",
            Kind::Elsewhere => "elsewhere",
        }
    }

    /// Its glyph at `tick`: a live state's moves as in the status view.
    fn glyph(self, tick: u64) -> &'static str {
        match self {
            Kind::Live(state) => state_glyph(state, tick),
            Kind::Term => "⌨",
            Kind::Asleep => DisplayState::Asleep.glyph(),
            Kind::Ended => DisplayState::Ended.glyph(),
            Kind::Elsewhere => "◌",
        }
    }

    /// The row's tone, and whether it is bold: a live state's as in the status view, a
    /// terminal's plain, the rest dim.
    fn look(self) -> (Tone, bool) {
        match self {
            Kind::Live(state) => status::look(state),
            Kind::Term => (Tone::Text, false),
            Kind::Asleep | Kind::Ended | Kind::Elsewhere => (Tone::Dim, false),
        }
    }
}

/// One row of the picker, before the query.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickRow {
    pub item: Item,
    pub kind: Kind,
    /// What it is called, cleaned.
    pub title: String,
    /// Its group, else its project's name; cleaned, empty when it has neither.
    pub group: String,
    /// When its age is from: a live session's last activity, another's last sighting; none
    /// for a terminal.
    pub since: Option<UtcMs>,
    /// A live session's line of context, cleaned.
    pub why: String,
    /// A terminal's folder and the program in front, as published; empty for a session. Its
    /// `why` is made from them when drawn, as the home folder is the context's.
    pub folder: String,
    pub command: String,
    /// What the query matches: the title, the group, the word, the folder's last part, the
    /// handle and the id's first 8 characters, lowercased and joined by spaces.
    pub hay: String,
}

/// What a terminal is called: its name, else its tab id; cleaned.
fn term_title(pane: &str, row: &TermRow) -> String {
    let name = if row.name.trim().is_empty() {
        pane
    } else {
        &row.name
    };
    clean(name)
}

/// A terminal's why: `~/folder · node`, or the one of them it has (a row from before moa
/// knew its folder shows none).
fn term_why(row: &PickRow, ctx: &Ctx) -> String {
    let folder = clean(&text::short_path(&row.folder, &ctx.home));
    [folder, clean(&row.command)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What a pick says it is opening: the row's title.
pub fn title(data: &Data, item: &Item) -> Option<String> {
    match item {
        Item::Session(id) => data.sessions.get(id).map(label),
        Item::Term { pane } => data.terms.get(pane).map(|t| term_title(pane, t)),
        Item::Group(_) | Item::Task(_) | Item::Project(_) | Item::Suggestion(_) => None,
    }
}

fn session_row(row: &SessionRow, kind: Kind, since: Option<UtcMs>, why: String) -> PickRow {
    let s = &row.session;
    let title = label(row);
    let group = s
        .group_path
        .as_deref()
        .and_then(tree::tidy)
        .or_else(|| row.project.clone())
        .map(|g| clean(&g))
        .unwrap_or_default();
    let folder = folder(row).map(clean).unwrap_or_default();
    let id: String = s.id.to_string().chars().take(8).collect();
    let handle = clean(&row.handle);
    let hay = [
        title.as_str(),
        group.as_str(),
        kind.word(),
        folder.as_str(),
        handle.as_str(),
        id.as_str(),
    ]
    .join(" ")
    .to_lowercase();
    PickRow {
        item: Item::Session(s.id),
        kind,
        title,
        group,
        since,
        why,
        folder: String::new(),
        command: String::new(),
        hay,
    }
}

/// A live session at home.
fn is_live(row: &SessionRow) -> bool {
    row.session.location == Location::Here && live(row.display_state)
}

/// Every row the picker lists, in its order: the live sessions at home by display state in
/// the status view's order, the most recent activity first within one; then the terminals,
/// by their tab number (tmux's order: `%9` before `%10`); then at most [`OLD_ROWS`] of the
/// rest (asleep, ended, elsewhere), the most recently seen first. An archived session is never
/// listed.
pub fn rows(data: &Data) -> Vec<PickRow> {
    let mut live: Vec<&SessionRow> = data.sessions.values().filter(|r| is_live(r)).collect();
    live.sort_by_key(|r| (sort_key(&r.session, r.display_state), r.session.id));
    let mut out: Vec<PickRow> = live
        .into_iter()
        .map(|r| {
            let why = clean(&r.why);
            session_row(r, Kind::Live(r.display_state), r.session.activity_at, why)
        })
        .collect();
    let mut terms: Vec<(&String, &TermRow)> = data.terms.iter().collect();
    terms.sort_by_key(|(pane, _)| (tab_number(pane), *pane));
    out.extend(terms.into_iter().map(|(pane, t)| {
        let title = term_title(pane, t);
        // Found by its folder's last part and its program too, as the row shows them.
        let folder = t.folder.rsplit('/').next().unwrap_or("");
        let hay = [
            title.as_str(),
            Kind::Term.word(),
            folder,
            t.command.as_str(),
            pane.as_str(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
        PickRow {
            item: Item::Term { pane: pane.clone() },
            kind: Kind::Term,
            title,
            group: String::new(),
            since: None,
            why: String::new(),
            folder: t.folder.clone(),
            command: t.command.clone(),
            hay,
        }
    }));
    let old = newest_first(data, |r| {
        !is_live(r) && r.display_state != DisplayState::Archived && r.session.archived_at.is_none()
    });
    out.extend(old.into_iter().take(OLD_ROWS).map(|r| {
        let kind = if r.display_state == DisplayState::Asleep {
            Kind::Asleep
        } else if r.session.location == Location::Elsewhere {
            Kind::Elsewhere
        } else {
            Kind::Ended
        };
        // Its why is empty, or says `elsewhere` again (the spike's showed none).
        session_row(r, kind, r.session.last_seen_at, String::new())
    }));
    out
}

/// A tab id's number (`%12` is 12); one that isn't a number sorts last.
fn tab_number(pane: &str) -> u64 {
    pane.trim_start_matches('%').parse().unwrap_or(u64::MAX)
}

/// Whether `term`'s characters appear in `hay` in order.
fn subsequence(term: &str, hay: &str) -> bool {
    let mut rest = hay;
    for c in term.chars() {
        match rest.find(c) {
            Some(at) => rest = &rest[at + c.len_utf8()..],
            None => return false,
        }
    }
    true
}

/// The query's terms: lowercased and split on spaces.
fn terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// How well `query` matches `hay` (lowercase text), the spike's `match`: `None` unless every
/// term of the query is a subsequence of it; else the number of terms that aren't
/// contiguous substrings, so lower is better.
pub fn rank(query: &str, hay: &str) -> Option<usize> {
    rank_terms(&terms(query), hay)
}

/// [`rank`] for a query already split into its lowercase terms.
fn rank_terms(terms: &[String], hay: &str) -> Option<usize> {
    let mut rank = 0;
    for term in terms {
        if !subsequence(term, hay) {
            return None;
        }
        if !hay.contains(term.as_str()) {
            rank += 1;
        }
    }
    Some(rank)
}

/// The rows and what the query keeps of them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Matched {
    /// The rows the query keeps, by rank, then in the rows' order. An empty query keeps
    /// every row in order.
    pub shown: Vec<PickRow>,
    /// How many rows there are.
    pub rows: usize,
    /// How many of them are live sessions.
    pub live: usize,
}

/// The rows of `data` that `query` matches. The query is split once, not per row.
pub fn matched(data: &Data, query: &str) -> Matched {
    let terms = terms(query);
    let all = rows(data);
    let (rows, live) = (
        all.len(),
        all.iter()
            .filter(|r| matches!(r.kind, Kind::Live(_)))
            .count(),
    );
    let mut ranked: Vec<(usize, PickRow)> = all
        .into_iter()
        .filter_map(|r| rank_terms(&terms, &r.hay).map(|n| (n, r)))
        .collect();
    // Stable: rows of one rank keep their order.
    ranked.sort_by_key(|(n, _)| *n);
    Matched {
        shown: ranked.into_iter().map(|(_, r)| r).collect(),
        rows,
        live,
    }
}

/// The picker's list, `width` cells wide: a line per row the query keeps, else
/// [`NO_MATCH`].
pub fn build(data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line> {
    lines(&matched(data, &ui.query), &ui.query, ctx, width)
}

/// [`build`] from the matches already worked out for `query` (a panel keeps them until the
/// data or the query changes).
pub fn lines(m: &Matched, query: &str, ctx: &Ctx, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    if m.shown.is_empty() {
        return vec![Line::plain(cut(&format!("  {NO_MATCH}"), width), Tone::Dim)];
    }
    let terms = terms(query);
    m.shown
        .iter()
        .map(|row| row_line(row, &terms, ctx, width))
        .collect()
}

/// The picker's head, fixed above its list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Head {
    /// `› ` and the query, with `<shown>/<rows> · <live> live` against the right edge; then a
    /// rule.
    pub lines: Vec<Line>,
    /// The column after the query, where the terminal's cursor stands.
    pub cursor: usize,
}

/// The head for `query` and its matches, `width` cells wide. The count shows when it
/// leaves the query a cell and the cursor's; a query longer than its room shows its tail.
pub fn head(m: &Matched, query: &str, width: u16) -> Head {
    let width = usize::from(width);
    let count = format!("{}/{} · {} live", m.shown.len(), m.rows, m.live);
    // The prompt, then the query; a cell between the query and the count.
    let beside = width.saturating_sub(2 + text::width(&count) + 1);
    let (room, chip) = if beside >= 2 {
        (beside, Some(Span::new(count, Tone::Dim)))
    } else {
        (width.saturating_sub(2), None)
    };
    // The last cell of its room is the cursor's.
    let query = text::tail(&clean(query), room.saturating_sub(1));
    let cursor = (2 + text::width(&query)).min(width.saturating_sub(1));
    let mut line = Line::new(vec![
        Span::bold(cut("› ", width), Tone::Head),
        Span::bold(query, Tone::Text),
    ]);
    line.chip = chip;
    Head {
        lines: vec![line, Line::plain("─".repeat(width), Tone::Dim)],
        cursor,
    }
}

/// ` <glyph> <title><group><word><age>  <why>` (the spike's row): the title in
/// `clamp(w/4, 10, 30)` cells, the group in `clamp(w/6, 0, 24)` from 70 columns, the word in
/// 10, the age right-aligned in 4; the query's hits in the title and group in [`Tone::Hit`].
fn row_line(row: &PickRow, terms: &[String], ctx: &Ctx, width: usize) -> Line {
    let tw = (width / 4).clamp(10, 30);
    let gw = if width >= 70 { (width / 6).min(24) } else { 0 };
    let (tone, bold) = row.kind.look();
    // A two-cell glyph (asleep's) takes the space after it: the title starts at cell 3.
    let lead = format!(" {}", fit(row.kind.glyph(ctx.tick), 2));
    let named = format!(
        "{}{}",
        fit(&cut(&row.title, tw - 1), tw),
        fit(&cut(&row.group, gw.saturating_sub(1)), gw)
    );
    let mut rest = format!(
        "{}{}",
        fit(row.kind.word(), 10),
        fit_right(&text::ago(row.since, ctx.now), 4)
    );
    let why = if row.kind == Kind::Term {
        term_why(row, ctx)
    } else {
        row.why.clone()
    };
    if !why.trim().is_empty() {
        rest.push_str("  ");
        rest.push_str(&why);
    }
    let mut spans = vec![Span::styled(lead, tone, bold)];
    spans.extend(highlighted(&named, terms, tone, bold));
    spans.push(Span::styled(rest, tone, bold));
    let mut line = Line::new(cut_spans(spans, width)).with_item(row.item.clone());
    line.animated = matches!(row.kind, Kind::Live(state) if animates(state));
    line.changes_at = row.since.map(|at| text::next_ago_change(at, ctx.now));
    line
}

/// `text` as spans in `tone`, with the graphemes the terms hit in [`Tone::Hit`].
fn highlighted(text: &str, terms: &[String], tone: Tone, bold: bool) -> Vec<Span> {
    let hit = hits(text, terms);
    let mut spans: Vec<Span> = Vec::new();
    for (g, on) in text.graphemes(true).zip(hit) {
        let (tone, bold) = if on { (Tone::Hit, true) } else { (tone, bold) };
        match spans.last_mut() {
            Some(last) if last.tone == tone => last.text.push_str(g),
            _ => spans.push(Span::styled(g, tone, bold)),
        }
    }
    spans
}

/// Which graphemes of `text` the terms hit (the spike's `marks`): each term's first
/// substring hit if it has one, else the first places its characters appear in order.
fn hits(text: &str, terms: &[String]) -> Vec<bool> {
    // The text lowercased, and for each of its bytes the grapheme it came from.
    let mut low = String::new();
    let mut owner = Vec::new();
    let mut count = 0;
    for (i, g) in text.graphemes(true).enumerate() {
        let g = g.to_lowercase();
        owner.extend(std::iter::repeat_n(i, g.len()));
        low.push_str(&g);
        count = i + 1;
    }
    let mut hit = vec![false; count];
    for term in terms {
        if let Some(at) = low.find(term.as_str()) {
            for &i in &owner[at..at + term.len()] {
                hit[i] = true;
            }
            continue;
        }
        let mut from = 0;
        for c in term.chars() {
            let Some(at) = low[from..].find(c) else {
                break;
            };
            hit[owner[from + at]] = true;
            from += at + c.len_utf8();
        }
    }
    hit
}

/// The spans that fit `width` cells: the one that crosses the edge is cut, the rest go.
fn cut_spans(spans: Vec<Span>, width: usize) -> Vec<Span> {
    let mut left = width;
    let mut out = Vec::new();
    for mut span in spans {
        let w = text::width(&span.text);
        if w <= left {
            left -= w;
            out.push(span);
            continue;
        }
        span.text = cut(&span.text, left);
        if !span.text.is_empty() {
            out.push(span);
        }
        break;
    }
    out
}
