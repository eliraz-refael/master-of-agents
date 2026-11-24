//! The console (B6): moa's conversation, the newest at the bottom. Your prompts; moa's turns,
//! each under a header and behind a gutter (its answers as Markdown, its thoughts, tool calls,
//! plans and notes, and its permission requests as cards you answer, K12); the sessions'
//! events, one line per session (B3); your commands' cards; errors; and a separator where the
//! thread changes (B2). A row it can't read is one dim line (B10): nothing here panics,
//! whatever the daemon sends.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use moa_core::UtcMs;
use moa_core::central::feed::kind;
use moa_core::central::wire::PermKind;
use moa_core::console::COMMANDS;
use moa_core::model::SessionId;
use moa_core::op::PermAnswer;
use moa_core::publish::{EventRow, FeedRow};
use moa_core::state::EventWhat;
use moa_core::time::local_format;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::editor::Editor;
use crate::line::{Button, Line, Span, Tone};
use crate::markdown::{self, button_span};
use crate::text::{self, clean, cut, cut_spans, ellipsize, width, wrap_spans};
use crate::views::{label, spinner};

/// What a moa turn draws in front of each of its lines.
const GUTTER: &str = "│ ";
/// What a card draws in front of each line of its reply.
const CARD_GUTTER: &str = "┊ ";
/// The most rows the bar wraps onto (B5).
pub const BAR_ROWS: usize = 3;
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
/// the cards of the commands sent from it, the errors it was told of, with when; its focused
/// button, its input line and history (B7), where its feed is scrolled to (B8, F15), what it
/// waits on, and when it last showed each error (B10).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConsoleUi {
    pub older: BTreeMap<i64, FeedRow>,
    pub cards: Vec<Card>,
    pub errors: Vec<(UtcMs, String)>,
    /// The button Tab is on, as an index into `ConsoleView.buttons`: that one is drawn
    /// reversed, and no other, whatever its command. It holds for one build;
    /// [`focused`](ConsoleUi::focused) finds the button again in the next.
    pub focus: Option<u16>,
    /// The focused button, as a rebuild finds it again.
    pub focused: Option<Focused>,
    /// The input line.
    pub draft: Editor,
    /// What Up and Down step through, taken when Up was first pressed ([`history`]).
    pub history: Vec<String>,
    /// The entry of `history` the input shows; `None` while it shows the draft.
    pub hist_at: Option<usize>,
    /// The draft as it was when Up was first pressed: Down past the newest entry brings it
    /// back.
    pub kept: String,
    /// The lines typed here and sent, with when, oldest first: the history has them before
    /// their rows come.
    pub sent: Vec<(UtcMs, String)>,
    /// The view follows the newest line (B8).
    pub follow: bool,
    /// While it doesn't: the line at its bottom (F15).
    pub anchor: Option<Anchor>,
    /// How many lines were below the view when it was last scrolled: more are new.
    pub below: usize,
    /// A request a key made, until its tag is known ([`Panel::sent`](crate::panel::Panel::sent)).
    pub asked: Option<Asked>,
    /// The requests sent, by tag, until their replies come.
    pub waits: BTreeMap<u64, Asked>,
    /// A page came back short: there is nothing older to ask for.
    pub exhausted: bool,
    /// When each error text was last drawn (B10).
    pub shown: HashMap<String, UtcMs>,
    /// The data's revision when the topic's rows were last kept in `older`.
    pub kept_at: Option<u64>,
    /// The palette's chosen entry.
    pub pick: usize,
    /// The draft Esc closed the palette at: it stays shut until the draft changes.
    pub closed: Option<String>,
    /// The rows the last build gave the palette: none, and it is as good as closed.
    pub palette_rows: u16,
}

impl Default for ConsoleUi {
    fn default() -> ConsoleUi {
        ConsoleUi {
            older: BTreeMap::new(),
            cards: Vec::new(),
            errors: Vec::new(),
            focus: None,
            focused: None,
            draft: Editor::default(),
            history: Vec::new(),
            hist_at: None,
            kept: String::new(),
            sent: Vec::new(),
            follow: true,
            anchor: None,
            below: 0,
            asked: None,
            waits: BTreeMap::new(),
            exhausted: false,
            shown: HashMap::new(),
            kept_at: None,
            pick: 0,
            closed: None,
            palette_rows: 0,
        }
    }
}

/// A request the console sent, and what its reply is for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Asked {
    /// `console.page` for `n` rows (B2).
    Page { n: u16 },
    /// A prompt, typed in the input or pressed, at `at`.
    Prompt {
        text: String,
        at: UtcMs,
        typed: bool,
    },
    /// A command, typed or pressed at `at`: its reply is a card (B4).
    Command {
        line: String,
        at: UtcMs,
        typed: bool,
    },
}

/// A line of the console, as a rebuild finds it again (F15): by the row drawn at or above
/// it, and how many lines below that row's first it is (above, when negative: a line before
/// every row). `row` is `None` only when no row is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Anchor {
    pub row: Option<i64>,
    pub offset: isize,
}

/// A focused button, as a rebuild finds it again: where it is, and its command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Focused {
    /// Its line; `None` for the bar's.
    pub line: Option<Anchor>,
    /// For the bar's: which of its buttons it is.
    pub nth: usize,
    pub command: String,
}

/// The lines a feed of a given height shows: `lines[top..end]`, under an `↑ N more` line when
/// `more` (the lines above) isn't 0; `new` lines came below since it was scrolled up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Window {
    pub top: usize,
    pub end: usize,
    pub more: usize,
    pub new: usize,
}

/// The console's lines, and the buttons their spans press.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConsoleView {
    pub lines: Vec<Line>,
    /// The bar under the input (B5), one or more lines of buttons.
    pub bar: Vec<Line>,
    /// The buttons of `lines` first, then the bar's: a span's `button` indexes this.
    pub buttons: Vec<Button>,
    /// Each row drawn, by id, with its first own line (after a separator or a turn's header
    /// before it): both in order.
    pub starts: Vec<(i64, usize)>,
}

impl ConsoleView {
    /// The buttons Tab walks (F26), as indexes into `buttons`: those of the `visible` lines
    /// in reading order (top to bottom, left to right), then the bar's, left to right.
    pub fn order(&self, visible: Range<usize>) -> Vec<u16> {
        let from = visible.start.min(self.lines.len());
        let to = visible.end.min(self.lines.len()).max(from);
        self.lines[from..to]
            .iter()
            .chain(&self.bar)
            .flat_map(|l| l.spans.iter().filter_map(|s| s.button))
            .collect()
    }

    /// The button Tab (BTab when `back`) goes to from `from` (F26): the next in
    /// [`order`](ConsoleView::order), wrapping at both ends. From none, or one out of sight,
    /// the newest visible text button (the first on the lowest line with one), else the
    /// bar's first.
    pub fn tab(&self, visible: Range<usize>, from: Option<u16>, back: bool) -> Option<u16> {
        let order = self.order(visible.clone());
        let n = order.len();
        match from.and_then(|f| order.iter().position(|b| *b == f)) {
            Some(i) if back => Some(order[(i + n - 1) % n]),
            Some(i) => Some(order[(i + 1) % n]),
            None => {
                let from = visible.start.min(self.lines.len());
                let to = visible.end.min(self.lines.len()).max(from);
                let newest = self.lines[from..to]
                    .iter()
                    .rev()
                    .find_map(|l| l.spans.iter().find_map(|s| s.button));
                newest.or_else(|| order.first().copied())
            }
        }
    }

    /// Draws `focus` reversed, and no other button.
    pub fn set_focus(&mut self, focus: Option<u16>) {
        for span in self
            .lines
            .iter_mut()
            .chain(&mut self.bar)
            .flat_map(|l| &mut l.spans)
        {
            if span.button.is_some() {
                span.reversed = focus.is_some() && span.button == focus;
            }
        }
    }

    /// Line `line`, as a rebuild finds it again.
    pub fn anchor(&self, line: usize) -> Anchor {
        let below = self.starts.partition_point(|(_, start)| *start <= line);
        let (id, start) = match (below.checked_sub(1), self.starts.first()) {
            (Some(i), _) => self.starts[i],
            (None, Some(first)) => *first,
            (None, None) => {
                return Anchor {
                    row: None,
                    offset: isize::try_from(line).unwrap_or(isize::MAX),
                };
            }
        };
        Anchor {
            row: Some(id),
            offset: signed(line) - signed(start),
        }
    }

    /// The line `anchor` stands for in this build, within the lines; `None` when there are
    /// none. A row no longer drawn gives the first line of the next one drawn, else the last
    /// line.
    pub fn line_of(&self, anchor: &Anchor) -> Option<usize> {
        let last = self.lines.len().checked_sub(1)?;
        let line = match anchor.row {
            None => anchor.offset,
            Some(id) => match self.starts.binary_search_by_key(&id, |(r, _)| *r) {
                Ok(i) => signed(self.starts[i].1) + anchor.offset,
                Err(i) => self.starts.get(i).map_or(signed(last), |s| signed(s.1)),
            },
        };
        Some(usize::try_from(line).unwrap_or(0).min(last))
    }

    /// Button `button` of this build, as the next finds it again.
    pub fn focused(&self, button: u16) -> Option<Focused> {
        let command = self.buttons.get(usize::from(button))?.command.clone();
        let has = |l: &Line| l.spans.iter().any(|s| s.button == Some(button));
        if let Some(line) = self.lines.iter().position(has) {
            return Some(Focused {
                line: Some(self.anchor(line)),
                nth: 0,
                command,
            });
        }
        let nth = self.bar_buttons().position(|b| b == button)?;
        Some(Focused {
            line: None,
            nth,
            command,
        })
    }

    /// The button `focused` was, in this build: the one with its command on its line or
    /// nearest it (the bar's, at its place or else the first), or none.
    pub fn find(&self, focused: &Focused) -> Option<u16> {
        let same = |b: &u16| {
            self.buttons
                .get(usize::from(*b))
                .is_some_and(|b| b.command == focused.command)
        };
        let Some(anchor) = &focused.line else {
            let bar: Vec<u16> = self.bar_buttons().collect();
            let at = bar.get(focused.nth).copied().filter(same);
            return at.or_else(|| bar.into_iter().find(same));
        };
        let line = self.line_of(anchor)?;
        self.lines
            .iter()
            .enumerate()
            .flat_map(|(i, l)| l.spans.iter().filter_map(move |s| Some((i, s.button?))))
            .filter(|(_, b)| same(b))
            .min_by_key(|(i, _)| i.abs_diff(line))
            .map(|(_, b)| b)
    }

    fn bar_buttons(&self) -> impl Iterator<Item = u16> + '_ {
        self.bar
            .iter()
            .flat_map(|l| l.spans.iter().filter_map(|s| s.button))
    }
}

fn signed(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

impl ConsoleUi {
    /// What a feed `rows` lines high shows of `view` (B8, F15): its newest lines while it
    /// follows, else those up to its anchor; never fewer than fill it. The `↑ N more` line
    /// takes the top row when lines are above.
    pub fn window(&self, view: &ConsoleView, rows: usize) -> Window {
        let n = view.lines.len();
        let anchored = self.anchor.as_ref().and_then(|a| view.line_of(a));
        let end = match (self.follow, anchored) {
            (false, Some(line)) => line + 1,
            _ => n,
        };
        let end = end.clamp(n.min(rows), n);
        let top = if end <= rows {
            0
        } else if rows < 2 {
            end - rows
        } else {
            end - (rows - 1)
        };
        Window {
            top,
            end,
            more: if rows < 2 { 0 } else { top },
            new: if self.follow {
                0
            } else {
                (n - end).saturating_sub(self.below)
            },
        }
    }

    /// Scrolls the feed `by` lines (up when negative) from where it is: at the newest line it
    /// follows again; elsewhere it anchors there.
    pub fn scroll(&mut self, view: &ConsoleView, rows: usize, by: isize) {
        let n = view.lines.len();
        let end = self
            .window(view, rows)
            .end
            .saturating_add_signed(by)
            .clamp(n.min(rows), n);
        self.follow = end == n;
        self.anchor = (!self.follow).then(|| view.anchor(end.saturating_sub(1)));
        self.below = n - end;
    }

    /// The feed follows its newest line again.
    pub fn to_bottom(&mut self) {
        self.follow = true;
        self.anchor = None;
        self.below = 0;
    }

    /// Whether a page of older rows is asked for, or on its way.
    pub fn paging(&self) -> bool {
        self.waits_on(|a| matches!(a, Asked::Page { .. }))
    }

    /// Whether a request `is` matches is asked for, or on its way.
    pub fn waits_on(&self, is: impl Fn(&Asked) -> bool) -> bool {
        self.asked.as_ref().is_some_and(&is) || self.waits.values().any(is)
    }
}

/// What Up and Down step through (B7), oldest first: the text of every prompt you typed, in
/// the rows loaded (moa's own, F20, are left out), and the lines sent here that no row has
/// (commands, and prompts whose rows haven't come), each at its time; one entry for a line
/// typed twice in a row.
pub fn history(data: &Data, ui: &ConsoleUi) -> Vec<String> {
    let typed: Vec<(i64, String)> = rows(data, ui)
        .into_values()
        .filter(|r| r.kind == kind::YOU && !r.automatic)
        .filter_map(|r| Some((r.at, r.body.get("text")?.as_str()?.to_string())))
        .collect();
    let mut all = typed.clone();
    all.extend(
        ui.sent
            .iter()
            .filter(|(_, s)| !typed.iter().any(|(_, t)| t == s))
            .map(|(at, s)| (at.0, s.clone())),
    );
    // Stable: rows keep their order, and a line sent keeps its place among the others.
    all.sort_by_key(|(at, _)| *at);
    let mut all: Vec<String> = all.into_iter().map(|(_, s)| s).collect();
    all.dedup();
    all
}

/// `↑ N more`, dim, in `width` cells.
pub fn more(n: usize, width: u16) -> Line {
    Line::plain(cut(&format!("↑ {n} more"), usize::from(width)), Tone::Dim)
}

/// The head line (B9): moa's thread and state, and `↓ N new` when `new` lines came below a
/// view scrolled up. While a permission request waits, the state is `⚠ answer y/a/n`, the
/// keys the oldest offers (K12). `up` is whether the panel's connection is. The thread goes
/// first when the width is short, then the end of the rest.
pub fn head(data: &Data, ctx: &Ctx, up: bool, new: usize, width: u16) -> Line {
    let dim = |s: &str| Span::new(s, Tone::Dim);
    let moa = data.moa.as_ref().filter(|_| up);
    let phase = moa.map_or("off", |m| m.phase.as_str());
    let waiting = moa.and_then(|_| waiting(data));
    let mut spans = match phase {
        _ if !up => vec![dim("connecting…")],
        _ if let Some(waiting) = &waiting => {
            let keys: Vec<String> = waiting
                .offered
                .iter()
                .map(|a| answer_key(*a).to_string())
                .collect();
            let answer = format!("⚠ answer {}", keys.join("/"));
            let mut spans = vec![Span::new(answer.trim_end(), Tone::Warn)];
            if phase == "thinking" {
                spans.push(dim(" Esc cancels"));
            }
            spans
        }
        "off" => vec![dim("○ off · type to start moa")],
        "starting" => vec![Span::new("◌ starting", Tone::Work)],
        "thinking" => vec![
            Span::new(format!("{} thinking…", spinner(ctx.tick)), Tone::Work),
            dim(" Esc cancels"),
        ],
        "ready" => vec![Span::new("● ready", Tone::Ok)],
        "failed" => {
            let why = moa
                .and_then(|m| m.why.as_deref())
                .map(clean)
                .filter(|w| !w.trim().is_empty())
                .unwrap_or_else(|| "failed".to_string());
            vec![fail(&format!("✗ {why}"))]
        }
        other => vec![dim(&clean(other))],
    };
    if new > 0 {
        spans.push(dim(" · "));
        spans.push(Span::new(format!("↓ {new} new"), Tone::Work));
    }
    if let Some(thread) = moa.and_then(|m| m.thread.as_deref()) {
        let thread: String = clean(thread).chars().take(8).collect();
        let thread = dim(&format!("thread {thread} · "));
        if text::width(&thread.text) + text::spans_width(&spans) <= usize::from(width) {
            spans.insert(0, thread);
        }
    }
    let mut line = Line::new(cut_spans(spans, usize::from(width)));
    line.animated = up && phase == "thinking" && waiting.is_none();
    line
}

/// The key that answers a waiting request with `answer`, as its card's button shows it:
/// `[y once]`, `[a always]`, `[n no]`.
fn answer_key(answer: PermAnswer) -> char {
    match answer {
        PermAnswer::Once => 'y',
        PermAnswer::Always => 'a',
        PermAnswer::No => 'n',
    }
}

/// The answer key `c` gives, if it is one.
pub fn answer_of(c: char) -> Option<PermAnswer> {
    PermAnswer::ALL.into_iter().find(|a| answer_key(*a) == c)
}

/// The line a card's button sends to answer request `id` (the hidden `/permission`).
pub fn answer_line(id: i64, answer: PermAnswer) -> String {
    format!("/permission {id} {}", answer.name())
}

/// The options of a `permission` row's body.
fn options(body: &Value) -> impl Iterator<Item = &Value> {
    body.get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The answers a `permission` row's body offers, in [`PermAnswer::ALL`]'s order.
fn offered(body: &Value) -> Vec<PermAnswer> {
    let kinds: Vec<&str> = options(body)
        .filter_map(|o| o.get("kind")?.as_str())
        .collect();
    PermAnswer::ALL
        .into_iter()
        .filter(|a| a.kinds().iter().any(|k| kinds.contains(&k.name())))
        .collect()
}

/// The oldest of moa's agent's permission requests waiting for your answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Waiting {
    /// Its `feed` row.
    pub id: i64,
    /// The answers it offers; all of them when its row isn't loaded (moa's core refuses one it
    /// doesn't).
    pub offered: Vec<PermAnswer>,
}

/// The request y, a and n answer (K12): the oldest in moa's `asking`, if any.
pub fn waiting(data: &Data) -> Option<Waiting> {
    let id = *data.moa.as_ref()?.asking.first()?;
    let offered = data
        .feed
        .get(&id)
        .map_or_else(|| PermAnswer::ALL.to_vec(), |r| offered(&r.body));
    Some(Waiting { id, offered })
}

/// What the input line says while it is empty.
const INPUT_HINT: &str = "ask moa, or / for commands";

/// The input line (B7), `width` cells: `› ` and the draft with the cursor in sight, or a dim
/// hint while it is empty; and the cursor's column.
pub fn input(ui: &ConsoleUi, width: u16) -> (Line, usize) {
    let w = usize::from(width);
    let lead = Span::new("› ", Tone::You);
    let room = w.saturating_sub(text::width(&lead.text));
    let (text, at) = ui.draft.view(room);
    let rest = if ui.draft.text.is_empty() {
        Span::new(INPUT_HINT, Tone::Dim)
    } else {
        Span::bold(text, Tone::Text)
    };
    let line = Line::new(cut_spans(vec![lead, rest], w));
    (line, (w - room) + at)
}

/// The most rows the palette shows (B11).
pub const PALETTE_ROWS: usize = 8;

/// One thing the palette offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Choice {
    pub label: String,
    pub help: String,
    /// What Tab puts in the word's place.
    pub insert: String,
    /// More is typed after it, so the palette stays open (a command's name).
    pub chain: bool,
}

/// The palette (B11, spec 6.11): the commands for a `/word`; the live sessions' titles after
/// `/resume`, `/focus`, `/close` and each argument of `/archive`; the layouts' names after
/// `/layout apply` and `/layout show`. `from` is the byte where the word being typed starts.
/// It narrows as the word grows, and is `None` while it offers nothing, while the draft isn't
/// a command, or after Esc closed it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Palette {
    pub from: usize,
    pub entries: Vec<Choice>,
}

pub fn palette(data: &Data, ui: &ConsoleUi) -> Option<Palette> {
    let t = ui.draft.text.as_str();
    // It completes the last word, so it shows only while the cursor is at the end.
    if !t.starts_with('/') || ui.closed.as_deref() == Some(t) || !ui.draft.at_end() {
        return None;
    }
    let Some(sp) = t.find(char::is_whitespace) else {
        let word = t[1..].to_lowercase();
        let entries: Vec<Choice> = COMMANDS
            .iter()
            .filter(|c| !c.hidden && c.name.starts_with(&word))
            .map(|c| Choice {
                label: format!("/{}", c.name),
                help: c.help.to_string(),
                insert: format!("/{} ", c.name),
                chain: true,
            })
            .take(PALETTE_ROWS)
            .collect();
        return (!entries.is_empty()).then_some(Palette { from: 0, entries });
    };
    let from = t
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = t[from..].trim_start_matches(['\'', '"']).to_lowercase();
    let done: Vec<&str> = t[sp..from].split_whitespace().collect();
    let entries = match (&t[1..sp], done.as_slice()) {
        ("resume" | "focus" | "close", []) | ("archive", _) => sessions(data, &word),
        ("layout", ["apply" | "show"]) => layouts(data, &word),
        _ => Vec::new(),
    };
    (!entries.is_empty()).then_some(Palette { from, entries })
}

/// The live sessions (not archived) whose name or id has `word`, the latest first; Tab puts
/// in the id, quoted if the shell would split it.
fn sessions(data: &Data, word: &str) -> Vec<Choice> {
    let mut live: Vec<&SessionRow> = data
        .sessions
        .values()
        .filter(|r| r.session.archived_at.is_none())
        .collect();
    live.sort_by_key(|r| std::cmp::Reverse(r.session.activity_at));
    live.into_iter()
        .filter_map(|r| {
            let (name, id) = (label(r), r.session.id.to_string());
            (name.to_lowercase().contains(word) || id.starts_with(word)).then(|| Choice {
                label: name,
                help: r.handle.clone(),
                insert: shell_words::quote(&clean(&id)).into_owned(),
                chain: false,
            })
        })
        .take(PALETTE_ROWS)
        .collect()
}

/// The layouts whose name has `word`, in their order.
fn layouts(data: &Data, word: &str) -> Vec<Choice> {
    let mut all: Vec<_> = data.layouts.values().collect();
    all.sort_by_key(|l| l.index);
    all.into_iter()
        .filter(|l| l.name.to_lowercase().contains(word))
        .map(|l| Choice {
            label: clean(&l.name),
            help: if l.current { "current" } else { "" }.to_string(),
            insert: shell_words::quote(&clean(&l.name)).into_owned(),
            chain: false,
        })
        .take(PALETTE_ROWS)
        .collect()
}

/// The palette's `rows` lines, `width` cells: the chosen entry reversed, kept in sight when
/// the screen has fewer rows than entries.
pub fn palette_lines(p: &Palette, pick: usize, rows: usize, width: u16) -> Vec<Line> {
    let w = usize::from(width);
    let pick = pick.min(p.entries.len().saturating_sub(1));
    let first = (pick + 1).saturating_sub(rows);
    p.entries
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, e)| {
            let chosen = i == pick;
            let mut spans = vec![
                Span::bold(format!("  {}", clean(&e.label)), Tone::Text),
                Span::new(format!("  {}", clean(&e.help)), Tone::Dim),
            ];
            for span in &mut spans {
                span.reversed = chosen;
            }
            Line::new(cut_spans(spans, w))
        })
        .collect()
}

/// The button drawn at cell `col` of `line`, if any.
pub fn button_at(line: &Line, col: usize) -> Option<u16> {
    let mut at = 0;
    for span in &line.spans {
        let end = at + width(&span.text);
        if col < end {
            return span.button;
        }
        at = end;
    }
    None
}

/// The button moa's text makes of inline `code` (B13): a `/command` the console knows (its
/// own text), or `moactl resume|focus|open|close|archive SESSION` with no flag (`[↩ name]`,
/// the session's name as it is now; the id for one not known). `None` for any other code.
pub fn find(data: &Data, code: &str) -> Option<Button> {
    let code = code.trim();
    if code.contains(char::is_control) {
        return None;
    }
    if let Some(rest) = code.strip_prefix('/') {
        let name = rest.split_whitespace().next()?;
        let known = COMMANDS.iter().any(|c| c.name == name);
        return known.then(|| Button {
            label: code.to_string(),
            command: code.to_string(),
        });
    }
    let words: Vec<&str> = code.split_whitespace().collect();
    let [moactl, verb, who] = words[..] else {
        return None;
    };
    // No flag, and nothing a shell would read otherwise.
    let id = !who.starts_with('-')
        && who
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if moactl != "moactl" || !id {
        return None;
    }
    let known = match verb {
        // `moactl open` takes a folder: only a session's id is a session.
        "open" => who.parse::<SessionId>().is_ok(),
        "resume" | "focus" | "close" | "archive" => true,
        _ => false,
    };
    if !known {
        return None;
    }
    let name = session_named(data, who).map_or_else(|| who.to_string(), session_name);
    Some(Button {
        label: format!("↩ {}", ellipsize(&name, 40)),
        command: format!("/{verb} {who}"),
    })
}

/// The session `who` names: its id, or the start of it (4 characters or more) or its handle.
fn session_named<'a>(data: &'a Data, who: &str) -> Option<&'a SessionRow> {
    if let Ok(id) = who.parse::<SessionId>() {
        return data.sessions.get(&id);
    }
    data.sessions
        .values()
        .find(|r| r.handle == who || (who.len() >= 4 && r.session.id.to_string().starts_with(who)))
}

/// `Project · title`, or what [`label`] says when it lacks either.
fn session_name(row: &SessionRow) -> String {
    let title = row.session.title.as_deref().map(clean).unwrap_or_default();
    let project = row.project.as_deref().map(clean).unwrap_or_default();
    if title.trim().is_empty() || project.trim().is_empty() {
        label(row)
    } else {
        format!("{project} · {title}")
    }
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
        held: Vec::new(),
        last: Last::Nothing,
        starts: Vec::new(),
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
    draw.end_turn();
    let bar = draw.bar();
    let mut view = ConsoleView {
        lines: draw.lines,
        bar,
        buttons: draw.buttons,
        starts: draw.starts,
    };
    view.set_focus(ui.focus);
    view
}

/// One thing the console shows, in the order it shows them.
#[derive(Clone, Copy)]
enum Entry<'a> {
    Row(&'a FeedRow),
    Event(Folded),
    Card(&'a Card),
    Error(&'a str),
}

/// A session's events folded into its one line (B3): the newest, and when it is `Finished`,
/// how many turns it finished since it last started or needed you, from when.
#[derive(Clone, Copy)]
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

/// The rows, paged-in ones and the topic's merged by id: an overlap shows once, as the topic
/// has it.
fn rows<'a>(data: &'a Data, ui: &'a ConsoleUi) -> BTreeMap<i64, &'a FeedRow> {
    let mut rows: BTreeMap<i64, &FeedRow> = ui.older.iter().map(|(id, r)| (*id, r)).collect();
    rows.extend(data.feed.iter().map(|(id, r)| (*id, r)));
    rows
}

/// The rows ([`rows`]), in id order; the session lines, cards and errors each placed before
/// the first row after their time.
fn entries<'a>(data: &'a Data, ui: &'a ConsoleUi) -> Vec<Entry<'a>> {
    let rows = rows(data, ui);
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

/// A moa turn: from a `you` row, or a row of moa's with none before it, to the next `you`
/// row or a thread change.
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
    /// The session lines, cards and errors whose time falls inside the turn: drawn right after it.
    held: Vec<Entry<'a>>,
    last: Last,
    /// Each row drawn, by id, with its first own line: after what [`Draw::enter`] draws
    /// before it, which a page arriving can change (F15).
    starts: Vec<(i64, usize)>,
}

impl<'a> Draw<'a> {
    fn entry(&mut self, entry: &Entry<'a>) {
        match entry {
            Entry::Row(row) => {
                self.enter(row);
                self.starts.push((row.id, self.lines.len()));
                if row.kind != kind::YOU {
                    let lines = self.row(row, self.room());
                    self.push_row(lines);
                } else if !row.automatic {
                    let lines = self.row(row, self.width);
                    self.lines.extend(lines);
                    self.last = Last::Row;
                }
            }
            other => {
                if self.turn.is_some() {
                    self.held.push(*other);
                } else {
                    self.other(other);
                }
            }
        }
    }

    /// A session's line, a card or an error, at its place between turns.
    fn other(&mut self, entry: &Entry<'a>) {
        match entry {
            Entry::Row(_) => {}
            Entry::Event(f) => self.session(f),
            Entry::Card(card) => {
                self.gap();
                self.card(card);
                self.last = Last::Card;
            }
            Entry::Error(text) => {
                if self.last != Last::Error {
                    self.gap();
                }
                let lines = hanging(fail("✗ "), vec![fail(&clean(text))], self.width);
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
        let at = self.lines.len();
        self.starts.extend(run.iter().map(|r| (r.id, at)));
        let hidden = run.len().saturating_sub(TOOLS_SHOWN);
        if hidden > 0 {
            let calls = if hidden == 1 { "call" } else { "calls" };
            let more = format!("⚙ {hidden} more tool {calls}");
            let line = Line::plain(cut(&more, self.room()), Tone::Dim);
            self.push_row(vec![line]);
        }
        for row in &run[hidden..] {
            let lines = self.row(row, self.room());
            self.push_row(lines);
        }
    }

    /// Before `row`: the turn ends at a `you` row or a thread change, and a separator marks
    /// the change. A `you` row starts a turn; the first other row starts one when none is
    /// open, and draws the turn's header.
    fn enter(&mut self, row: &'a FeedRow) {
        let new_thread = self.thread.is_some_and(|t| t != row.thread);
        if new_thread || row.kind == kind::YOU {
            self.end_turn();
        }
        if new_thread {
            self.gap();
            self.separator(row);
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
        let turn = self.turn.get_or_insert(Turn {
            automatic: false,
            headed: false,
        });
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

    /// The turn is over: what was held while it ran is drawn.
    fn end_turn(&mut self) {
        self.turn = None;
        for entry in std::mem::take(&mut self.held) {
            self.other(&entry);
        }
    }

    /// A session's line, after a blank line unless another session's is just above.
    fn session(&mut self, f: &Folded) {
        if self.last != Last::Event {
            self.gap();
        }
        self.event(f);
        self.last = Last::Event;
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
                hanging(you("› "), vec![you(&clean(t))], room)
            }),
            kind::MOA => text.map(|t| {
                let (w, data) = (u16::try_from(room).unwrap_or(u16::MAX), self.data);
                markdown::render(t, w, &mut self.buttons, &|code| find(data, code))
            }),
            // Its first line that isn't blank; nothing when every line is.
            kind::THOUGHT => text.map(|t| {
                let mut lines = t.lines().map(|l| clean(l).trim().to_string());
                let Some(first) = lines.find(|l| !l.is_empty()) else {
                    return Vec::new();
                };
                let first = cut(&first, room.saturating_sub(2));
                let line = Span::new(format!("{first} …"), Tone::Dim);
                vec![Line::new(cut_spans(vec![line], room))]
            }),
            kind::TOOL => self.tool(row, room).map(|line| vec![line]),
            kind::PLAN => plan(row, room),
            kind::SYSTEM => text.map(|t| {
                let dim = |s: &str| Span::new(s, Tone::Dim);
                hanging(dim("· "), vec![dim(&clean(t))], room)
            }),
            kind::ERROR => text.map(|t| hanging(fail("✗ "), vec![fail(&clean(t))], room)),
            kind::PERMISSION => self.permission(row, room),
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

    /// A permission request (K12). While it waits: `⚠ moa asks: <title>`, then behind the
    /// card's gutter a button for each answer it offers and the name moa's agent gives
    /// `always`. Answered: `✓ allowed once`, `✓ allowed always` or `✗ refused`, and its title;
    /// `⊘ not answered` when it was cancelled, or is `pending` with nothing waiting on it (a
    /// restart lost it). `None` without a title.
    fn permission(&mut self, row: &FeedRow, room: usize) -> Option<Vec<Line>> {
        let title = clean(row.body.get("title")?.as_str()?);
        let waits = row.state.as_deref() == Some("pending")
            && self
                .data
                .moa
                .as_ref()
                .is_some_and(|m| m.asking.contains(&row.id));
        if !waits {
            let (glyph, done, tone) = match row.state.as_deref() {
                Some("allow_once") => ("✓ ", "allowed once", Tone::Ok),
                Some("allow_always") => ("✓ ", "allowed always", Tone::Ok),
                Some("reject_once" | "reject_always") => ("✗ ", "refused", Tone::Fail),
                _ => ("⊘ ", "not answered", Tone::Dim),
            };
            let text = Span::new(format!("{done}: {title}"), tone);
            return Some(hanging(Span::new(glyph, tone), vec![text], room));
        }
        let warn = |s: &str| Span::new(s, Tone::Warn);
        let mut lines = hanging(warn("⚠ "), vec![warn(&format!("moa asks: {title}"))], room);
        if room <= width(CARD_GUTTER) {
            return Some(lines);
        }
        let inner = room - width(CARD_GUTTER);
        let mut buttons = Vec::new();
        for answer in offered(&row.body) {
            let button = Button {
                label: format!("{} {}", answer_key(answer), answer.name()),
                command: answer_line(row.id, answer),
            };
            if let Some(span) = button_span(&mut self.buttons, button, false) {
                if !buttons.is_empty() {
                    buttons.push(Span::new(" ", Tone::Text));
                }
                buttons.push(span);
            }
        }
        let mut card: Vec<Line> = Vec::new();
        if !buttons.is_empty() {
            card.extend(wrap_spans(&buttons, inner).into_iter().map(Line::new));
        }
        let always = options(&row.body)
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(PermKind::AllowAlways.name()))
            .and_then(|o| o.get("name")?.as_str())
            .map(clean)
            .filter(|name| !name.trim().is_empty());
        if let Some(name) = always {
            let dim = |s: &str| Span::new(s, Tone::Dim);
            card.extend(hanging(dim("always: "), vec![dim(&name)], inner));
        }
        lines.extend(behind(CARD_GUTTER, card));
        Some(lines)
    }

    /// A session's folded line (B3). When it needs you or finished a turn: a button to go to
    /// it, and its last line under it (under a start or an end that line may be stale).
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
        self.lines.extend(hanging(glyph, spans, self.width));
        let why = row.map(|r| clean(&r.why)).unwrap_or_default();
        if press.is_some() && !why.trim().is_empty() {
            let spans = vec![Span::new("  ", Tone::Text), Span::new(why, Tone::Dim)];
            self.lines.push(Line::new(cut_spans(spans, self.width)));
        }
    }

    /// `▸ <command>`, then its reply as Markdown behind the card's gutter.
    fn card(&mut self, card: &Card) {
        let dim = |s: &str| Span::new(s, Tone::Dim);
        let lines = hanging(dim("▸ "), vec![dim(&clean(&card.line))], self.width);
        self.lines.extend(lines);
        if self.width <= width(CARD_GUTTER) {
            return;
        }
        let w = u16::try_from(self.width - width(CARD_GUTTER)).unwrap_or(u16::MAX);
        let data = self.data;
        let reply = markdown::render(&card.text, w, &mut self.buttons, &|c| find(data, c));
        self.lines.extend(behind(CARD_GUTTER, reply));
    }

    /// The bar (B5): `data.bar`'s commands as buttons, wrapped to the width on at most
    /// [`BAR_ROWS`] rows, the last ending in `…` when buttons are left out; none when it has
    /// none. Its buttons come after the text's (those left out included, never drawn).
    fn bar(&mut self) -> Vec<Line> {
        let commands = self.data.bar.iter().flat_map(|b| &b.commands);
        let mut spans = Vec::new();
        for command in commands {
            let command = clean(command);
            let button = Button {
                label: command.clone(),
                command,
            };
            if let Some(span) = button_span(&mut self.buttons, button, false) {
                if !spans.is_empty() {
                    spans.push(Span::new(" ", Tone::Text));
                }
                spans.push(span);
            }
        }
        if spans.is_empty() {
            return Vec::new();
        }
        let mut rows = wrap_spans(&spans, self.width);
        if rows.len() > BAR_ROWS {
            rows.truncate(BAR_ROWS);
            // The last row's buttons go from its end until a `…` fits after them.
            let last = rows.last_mut().expect("BAR_ROWS rows");
            while !last.is_empty() && text::spans_width(last) + 2 > self.width {
                last.pop();
            }
            text::trim_end_spans(last);
            let more = if last.is_empty() { "…" } else { " …" };
            last.push(Span::new(more, Tone::Dim));
            *last = cut_spans(std::mem::take(last), self.width);
        }
        rows.into_iter().map(Line::new).collect()
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

/// `lead`, then `spans` wrapped to the cells left beside it ([`text::hang`]); the wrapped
/// lines start under the text. All of it cut to one line when `lead` leaves no room.
fn hanging(lead: Span, spans: Vec<Span>, w: usize) -> Vec<Line> {
    let lead_w = width(&lead.text);
    if lead_w >= w {
        let mut all = vec![lead];
        all.extend(spans);
        return vec![Line::new(cut_spans(all, w))];
    }
    text::hang(&[lead], &spans, w - lead_w)
        .into_iter()
        .enumerate()
        .map(|(i, (mut front, line))| {
            front.extend(line);
            if i == 0 {
                Line::new(front)
            } else {
                Line::wrapped(front)
            }
        })
        .collect()
}

/// `lines`, each behind `gutter` (Dim); a blank line keeps only the gutter's mark.
fn behind(gutter: &str, lines: Vec<Line>) -> Vec<Line> {
    lines
        .into_iter()
        .map(|mut line| {
            let blank = line.text().trim().is_empty();
            line.spans.insert(0, Span::new(gutter, Tone::Dim));
            if blank {
                text::trim_end_spans(&mut line.spans);
            }
            line
        })
        .collect()
}
