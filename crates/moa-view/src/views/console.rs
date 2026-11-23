//! The console (B6): moa's conversation, the newest at the bottom. Your prompts; moa's turns,
//! each under a header and behind a gutter (its answers as Markdown, its thoughts, tool calls,
//! plans and notes); the sessions' events, one line per session (B3); your commands' cards;
//! errors; and a separator where the thread changes (B2). A row it can't read is one dim line
//! (B10): nothing here panics, whatever the daemon sends.

use std::collections::BTreeMap;

use moa_core::UtcMs;
use moa_core::central::feed::kind;
use moa_core::model::SessionId;
use moa_core::publish::{EventRow, FeedRow};
use moa_core::state::EventWhat;
use moa_core::time::local_format;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::line::{Button, Line, Span, Tone};
use crate::markdown::{self, button_span};
use crate::text::{self, clean, cut, width};
use crate::views::{label, spinner};

/// What a moa turn draws in front of each of its lines.
const GUTTER: &str = "│ ";
/// What a card draws in front of each line of its reply.
const CARD_GUTTER: &str = "┊ ";
/// A run of tool calls longer than this shows only its last this many.
const TOOLS_SHOWN: usize = 3;

/// A command's reply, kept in the panel that sent it: never saved, and moa never sees it (B4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Card {
    pub at: UtcMs,
    /// The command as typed, `/` first.
    pub line: String,
    /// The reply's text, as Markdown.
    pub text: String,
}

/// What the console keeps in its panel: the rows paged in from before the topic's tail (B2),
/// the cards of the commands sent from it, and the errors it was told of, with when.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConsoleUi {
    pub older: BTreeMap<i64, FeedRow>,
    pub cards: Vec<Card>,
    pub errors: Vec<(UtcMs, String)>,
}

/// The console's lines, and the buttons their spans press.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConsoleView {
    pub lines: Vec<Line>,
    pub buttons: Vec<Button>,
}

/// The console's lines, none wider than `width` cells.
pub fn build(data: &Data, ctx: &Ctx, ui: &ConsoleUi, width: u16) -> ConsoleView {
    let mut draw = Draw {
        data,
        ctx,
        width: usize::from(width),
        lines: Vec::new(),
        buttons: Vec::new(),
        thread: None,
        turn: None,
        last: Last::Nothing,
    };
    let entries = entries(data, ui);
    let mut i = 0;
    while i < entries.len() {
        if let Entry::Row(first) = entries[i]
            && first.kind == kind::TOOL
        {
            // A run of tool calls, in one thread, with nothing between them.
            let run: Vec<&FeedRow> = entries[i..]
                .iter()
                .map_while(|e| match e {
                    Entry::Row(r) if r.kind == kind::TOOL && r.thread == first.thread => Some(*r),
                    _ => None,
                })
                .collect();
            i += run.len();
            draw.tools(&run);
            continue;
        }
        draw.entry(&entries[i]);
        i += 1;
    }
    ConsoleView {
        lines: draw.lines,
        buttons: draw.buttons,
    }
}

/// One thing the console shows, in the order it shows them.
enum Entry<'a> {
    Row(&'a FeedRow),
    Event(Folded),
    Card(&'a Card),
    Error(&'a str),
}

/// A session's events folded into its one line (B3): the newest, and when it is `Finished`,
/// how many turns it finished since it last started or needed you, from when.
struct Folded {
    session: SessionId,
    what: EventWhat,
    at: i64,
    turns: usize,
    since: Option<i64>,
}

/// Each session's line, but none for one whose newest event is `Asleep`.
fn folded(events: &BTreeMap<u64, EventRow>) -> Vec<Folded> {
    let mut by: BTreeMap<SessionId, Folded> = BTreeMap::new();
    for e in events.values() {
        let f = by.entry(e.session).or_insert(Folded {
            session: e.session,
            what: e.what,
            at: e.at,
            turns: 0,
            since: None,
        });
        f.what = e.what;
        f.at = e.at;
        match e.what {
            EventWhat::Started | EventWhat::NeedsYou => {
                f.turns = 0;
                f.since = None;
            }
            EventWhat::Finished => {
                f.turns += 1;
                f.since.get_or_insert(e.at);
            }
            EventWhat::Ended | EventWhat::Asleep => {}
        }
    }
    by.into_values()
        .filter(|f| f.what != EventWhat::Asleep)
        .collect()
}

/// The rows, paged-in ones and the topic's merged by id (an overlap shows once), in id order;
/// the session lines, cards and errors each placed before the first row after their time.
fn entries<'a>(data: &'a Data, ui: &'a ConsoleUi) -> Vec<Entry<'a>> {
    let mut rows: BTreeMap<i64, &FeedRow> = ui.older.iter().map(|(id, r)| (*id, r)).collect();
    rows.extend(data.feed.iter().map(|(id, r)| (*id, r)));
    let mut others: Vec<(i64, Entry)> = folded(&data.events)
        .into_iter()
        .map(|f| (f.at, Entry::Event(f)))
        .chain(ui.cards.iter().map(|c| (c.at.0, Entry::Card(c))))
        .chain(
            ui.errors
                .iter()
                .map(|(at, e)| (at.0, Entry::Error(e.as_str()))),
        )
        .collect();
    others.sort_by_key(|(at, _)| *at);
    let mut others = others.into_iter().peekable();
    let mut out = Vec::new();
    for row in rows.into_values() {
        while let Some((_, e)) = others.next_if(|(at, _)| *at < row.at) {
            out.push(e);
        }
        out.push(Entry::Row(row));
    }
    out.extend(others.map(|(_, e)| e));
    out
}

/// A moa turn: from a `you` row to the next, or to a thread change.
struct Turn {
    /// Its prompt was moa's own.
    automatic: bool,
    /// Its header is drawn.
    headed: bool,
}

/// What was drawn last, for the blank lines between blocks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Nothing,
    Row,
    Separator,
    Event,
    Card,
    Error,
}

struct Draw<'a> {
    data: &'a Data,
    ctx: &'a Ctx,
    width: usize,
    lines: Vec<Line>,
    buttons: Vec<Button>,
    /// The thread of the last row.
    thread: Option<&'a str>,
    turn: Option<Turn>,
    last: Last,
}

impl<'a> Draw<'a> {
    fn entry(&mut self, entry: &Entry<'a>) {
        match entry {
            Entry::Row(row) => {
                self.enter(row);
                if row.kind != kind::YOU {
                    let lines = self.row(row, self.room());
                    self.push_row(lines);
                } else if !row.automatic {
                    let lines = self.row(row, self.width);
                    self.lines.extend(lines);
                    self.last = Last::Row;
                }
            }
            Entry::Event(f) => {
                if self.last != Last::Event {
                    self.gap();
                }
                self.event(f);
                self.last = Last::Event;
            }
            Entry::Card(card) => {
                self.gap();
                self.card(card);
                self.last = Last::Card;
            }
            Entry::Error(text) => {
                if self.last != Last::Error {
                    self.gap();
                }
                let lines = hang(fail("✗ "), vec![fail(&clean(text))], self.width);
                self.lines.extend(lines);
                self.last = Last::Error;
            }
        }
    }

    /// A run of tool calls: when longer than [`TOOLS_SHOWN`], a count of the rest, then the
    /// last ones.
    fn tools(&mut self, run: &[&'a FeedRow]) {
        let Some(first) = run.first() else {
            return;
        };
        self.enter(first);
        let hidden = run.len().saturating_sub(TOOLS_SHOWN);
        if hidden > 0 {
            let more = format!("⚙ {hidden} more tool calls");
            let line = Line::plain(cut(&more, self.room()), Tone::Dim);
            self.push_row(vec![line]);
        }
        for row in &run[hidden..] {
            let lines = self.row(row, self.room());
            self.push_row(lines);
        }
    }

    /// Before `row`: a separator when its thread isn't the last row's; a `you` row starts a
    /// turn, and the turn's first other row draws its header.
    fn enter(&mut self, row: &'a FeedRow) {
        if self.thread.is_some_and(|t| t != row.thread) {
            self.gap();
            self.separator(row);
            self.turn = None;
        }
        self.thread = Some(&row.thread);
        if row.kind == kind::YOU {
            if !row.automatic {
                self.gap();
            }
            self.turn = Some(Turn {
                automatic: row.automatic,
                headed: false,
            });
            return;
        }
        if !matches!(self.last, Last::Row | Last::Separator) {
            self.gap();
        }
        let Some(turn) = &mut self.turn else {
            return;
        };
        if turn.headed {
            return;
        }
        turn.headed = true;
        let automatic = turn.automatic;
        if automatic {
            self.gap();
        }
        let at = local_format(UtcMs(row.at), &self.ctx.tz, "%H:%M");
        let mut spans = vec![Span::bold(format!("◆ moa  {at}"), Tone::Brain)];
        if automatic {
            spans.push(Span::new(" · on its own", Tone::Dim));
        }
        self.lines.push(Line::new(cut_spans(spans, self.width)));
    }

    /// `── Fri 13 Nov · thread 0193b4f0 ──`, at `row`'s day.
    fn separator(&mut self, row: &FeedRow) {
        let day = local_format(UtcMs(row.at), &self.ctx.tz, "%a %d %b");
        let thread: String = clean(&row.thread).chars().take(8).collect();
        let text = format!("── {day} · thread {thread} ──");
        self.lines
            .push(Line::plain(cut(&text, self.width), Tone::Dim));
        self.last = Last::Separator;
    }

    /// The cells a row's own text has: the width, less the gutter in a turn.
    fn room(&self) -> usize {
        if self.gutters() {
            self.width - width(GUTTER)
        } else {
            self.width
        }
    }

    /// Rows draw behind the gutter: they are in a turn, and it leaves them room.
    fn gutters(&self) -> bool {
        self.turn.is_some() && self.width > width(GUTTER)
    }

    /// A row's lines, behind the gutter in a turn.
    fn push_row(&mut self, lines: Vec<Line>) {
        if self.gutters() {
            self.lines.extend(behind(GUTTER, lines));
        } else {
            self.lines.extend(lines);
        }
        self.last = Last::Row;
    }

    /// A row's lines, `room` cells wide; one dim line when its body isn't what its kind needs,
    /// or its kind isn't one.
    fn row(&mut self, row: &FeedRow, room: usize) -> Vec<Line> {
        let text = row.body.get("text").and_then(Value::as_str);
        let lines = match row.kind.as_str() {
            kind::YOU => text.map(|t| {
                let you = |s: &str| Span::new(s, Tone::You);
                hang(you("› "), vec![you(&clean(t))], room)
            }),
            kind::MOA => text.map(|t| {
                let w = u16::try_from(room).unwrap_or(u16::MAX);
                markdown::render(t, w, &mut self.buttons, &|_| None)
            }),
            kind::THOUGHT => text.map(|t| {
                let first = t.lines().map(str::trim).find(|l| !l.is_empty());
                let first = cut(&clean(first.unwrap_or_default()), room.saturating_sub(2));
                let line = Span::new(format!("{first} …"), Tone::Dim);
                vec![Line::new(cut_spans(vec![line], room))]
            }),
            kind::TOOL => self.tool(row, room).map(|line| vec![line]),
            kind::PLAN => plan(row, room),
            kind::SYSTEM => text.map(|t| {
                let dim = |s: &str| Span::new(s, Tone::Dim);
                hang(dim("· "), vec![dim(&clean(t))], room)
            }),
            kind::ERROR => text.map(|t| hang(fail("✗ "), vec![fail(&clean(t))], room)),
            _ => None,
        };
        lines.unwrap_or_else(|| {
            let text = format!("· an entry moa couldn't show (#{})", row.id);
            vec![Line::plain(cut(&text, room), Tone::Dim)]
        })
    }

    /// `⚙ <title>: <command>` and how the call went: `✓`, `✗`, `⊘`, or the spinner while it
    /// runs. `None` without a title.
    fn tool(&self, row: &FeedRow, room: usize) -> Option<Line> {
        let title = row.body.get("title")?.as_str()?;
        // The input is the agent's JSON, as text (cut, so it may not parse).
        let input = match row.body.get("input") {
            Some(Value::String(s)) => serde_json::from_str(s).ok(),
            other => other.cloned(),
        };
        let command = input
            .as_ref()
            .and_then(|i: &Value| i.get("command"))
            .and_then(Value::as_str);
        let (mark, animated) = match row.state.as_deref() {
            Some("completed") => (Some(Span::new("✓", Tone::Ok)), false),
            Some("failed") => (Some(fail("✗")), false),
            Some("cancelled") => (Some(Span::new("⊘", Tone::Dim)), false),
            Some("pending" | "in_progress") => {
                (Some(Span::new(spinner(self.ctx.tick), Tone::Work)), true)
            }
            _ => (None, false),
        };
        let mut spans = vec![
            Span::new("⚙ ", Tone::Dim),
            Span::new(clean(title), Tone::Text),
        ];
        if let Some(command) = command {
            spans.push(Span::new(format!(": {}", clean(command)), Tone::Dim));
        }
        let mark_w = mark.as_ref().map_or(0, |m| width(&m.text) + 1);
        let mut spans = cut_spans(spans, room.saturating_sub(mark_w));
        if let Some(mark) = mark {
            spans.push(Span::new(" ", Tone::Text));
            spans.push(mark);
        }
        let mut line = Line::new(cut_spans(spans, room));
        line.animated = animated;
        Some(line)
    }

    /// A session's folded line (B3), with a button to go to it when it needs you or finished,
    /// and its last line under it.
    fn event(&mut self, f: &Folded) {
        let row = self.data.sessions.get(&f.session);
        let name = row.map_or_else(|| f.session.to_string().chars().take(8).collect(), label);
        let (glyph, rest, press) = match f.what {
            EventWhat::Started => (Span::new("▶ ", Tone::Work), " started".to_string(), None),
            EventWhat::Ended => (Span::new("○ ", Tone::Dim), " ended".to_string(), None),
            EventWhat::NeedsYou => (
                Span::new("⚠ ", Tone::Warn),
                " needs you".to_string(),
                Some("↩ answer"),
            ),
            EventWhat::Finished => {
                let since = f.since.unwrap_or(f.at);
                let since = local_format(UtcMs(since), &self.ctx.tz, "%H:%M");
                let turns = if f.turns == 1 { "turn" } else { "turns" };
                let rest = format!(" · {} {turns} since {since}", f.turns);
                (Span::new("● ", Tone::Ok), rest, Some("↩ open"))
            }
            EventWhat::Asleep => return,
        };
        let mut spans = vec![Span::new(name, Tone::Text), Span::new(rest, Tone::Dim)];
        let button = press.and_then(|label| {
            let button = Button {
                label: label.to_string(),
                command: format!("/focus {}", f.session),
            };
            button_span(&mut self.buttons, button, false)
        });
        if let Some(button) = button {
            spans.push(Span::new(" ", Tone::Text));
            spans.push(button);
        }
        self.lines.extend(hang(glyph, spans, self.width));
        let why = row.map(|r| clean(&r.why)).unwrap_or_default();
        if !why.trim().is_empty() {
            let spans = vec![Span::new("  ", Tone::Text), Span::new(why, Tone::Dim)];
            self.lines.push(Line::new(cut_spans(spans, self.width)));
        }
    }

    /// `▸ <command>`, then its reply as Markdown behind the card's gutter.
    fn card(&mut self, card: &Card) {
        let dim = |s: &str| Span::new(s, Tone::Dim);
        let lines = hang(dim("▸ "), vec![dim(&clean(&card.line))], self.width);
        self.lines.extend(lines);
        if self.width <= width(CARD_GUTTER) {
            return;
        }
        let w = u16::try_from(self.width - width(CARD_GUTTER)).unwrap_or(u16::MAX);
        let reply = markdown::render(&card.text, w, &mut self.buttons, &|_| None);
        self.lines.extend(behind(CARD_GUTTER, reply));
    }

    /// A blank line, unless the last is one or a separator is just above.
    fn gap(&mut self) {
        let blank = self.lines.last().is_none_or(|l| l.text().trim().is_empty());
        if !blank && self.last != Last::Separator {
            self.lines.push(Line::new(Vec::new()));
        }
    }
}

/// A plan's entries, one line each: `☐` pending, `◐` in progress, `☑` done. `None` when it has
/// none, or one without its text.
fn plan(row: &FeedRow, room: usize) -> Option<Vec<Line>> {
    let entries = row.body.get("entries")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    entries
        .iter()
        .map(|e| {
            let content = e.get("content")?.as_str()?;
            let (glyph, tone) = match e.get("status").and_then(Value::as_str) {
                Some("completed") => ("☑ ", Tone::Ok),
                Some("in_progress") => ("◐ ", Tone::Work),
                _ => ("☐ ", Tone::Dim),
            };
            let spans = vec![
                Span::new(glyph, tone),
                Span::new(clean(content), Tone::Text),
            ];
            Some(Line::new(cut_spans(spans, room)))
        })
        .collect()
}

fn fail(text: &str) -> Span {
    Span::new(text, Tone::Fail)
}

/// `lead`, then `spans` wrapped to the cells left beside it; the wrapped lines start under the
/// text. All of it cut to one line when `lead` leaves no room.
fn hang(lead: Span, spans: Vec<Span>, w: usize) -> Vec<Line> {
    let lead_w = width(&lead.text);
    if lead_w >= w {
        let mut all = vec![lead];
        all.extend(spans);
        return vec![Line::new(cut_spans(all, w))];
    }
    text::wrap_spans(&spans, w - lead_w)
        .into_iter()
        .enumerate()
        .map(|(i, mut line)| {
            if i == 0 {
                line.insert(0, lead.clone());
                Line::new(line)
            } else {
                line.insert(0, Span::new(" ".repeat(lead_w), Tone::Text));
                Line::wrapped(line)
            }
        })
        .collect()
}

/// `lines`, each behind `gutter` (Dim); a blank line gets the gutter's mark alone.
fn behind(gutter: &str, lines: Vec<Line>) -> Vec<Line> {
    lines
        .into_iter()
        .map(|mut line| {
            let mark = if line.text().is_empty() {
                gutter.trim_end()
            } else {
                gutter
            };
            line.spans.insert(0, Span::new(mark, Tone::Dim));
            line
        })
        .collect()
}

/// The spans that fit in `w` cells, cut at the first one that doesn't fit whole.
fn cut_spans(spans: Vec<Span>, w: usize) -> Vec<Span> {
    let mut left = w;
    let mut out = Vec::new();
    for span in spans {
        let text = cut(&span.text, left);
        let whole = text.len() == span.text.len();
        left -= width(&text);
        if !text.is_empty() {
            out.push(span.with_text(text));
        }
        if !whole {
            break;
        }
    }
    out
}
