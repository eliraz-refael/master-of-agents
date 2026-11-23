//! A panel's state machine: keys, clicks, pastes and replies in, ops out, and the screen to
//! draw. Keys act on the selected item, never on a row index (V8); nothing here runs a
//! program or touches a file.

use std::fmt::Write as _;

use moa_core::UtcMs;
use moa_core::model::{Project, TaskStatus};
use moa_core::op::{Answer, Done, Op, TaskSet};
use moa_core::panel::View;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::editor::{Edited, Editor};
use crate::geometry::{Geometry, Rect, Size, geometry};
use crate::keys::{self, Action, Key};
use crate::line::{Button, Item, Line, Span, Tone, debug_lines};
use crate::list::ListState;
use crate::text::{self, clean};
use crate::views::console::{self, ConsoleUi};
use crate::views::{self, ViewUi, picker, picture, tree};

mod ask;
mod pick_keys;
mod term_keys;
mod tree_keys;

use ask::Ask;

/// What a panel asks its loop to do.
// One is built per key, click or reply, so boxing the op buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
    /// Send this op to the daemon; its reply comes back through [`Panel::replied`].
    Send(Op),
    Quit,
    /// The picker is done: it picked, or Esc, Ctrl-c or Ctrl-g closed it (S7). Help is done:
    /// `q`, Esc or Ctrl-c closed it (S10).
    Close,
}

/// A mouse event, in screen cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mouse {
    Move { x: u16, y: u16 },
    Click { x: u16, y: u16 },
    WheelUp,
    WheelDown,
}

/// What a mouse event did: the ops it sent, and whether the screen changed (V11).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Handled {
    pub out: Vec<Out>,
    pub redraw: bool,
}

/// How a body line is marked. The selection wins over the hover.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Plain,
    Hovered,
    Selected,
}

/// Everything a frame shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Screen {
    pub geometry: Geometry,
    /// The frame's title (a framed view).
    pub title: Option<String>,
    /// The picture's tab names and the one shown.
    pub tabs: Option<(Vec<&'static str>, usize)>,
    /// The picture's `moa` mark at the right of its tabs.
    pub brand: Option<Span>,
    /// The picker's head lines: its query and a rule.
    pub head: Vec<Line>,
    /// The cell the terminal's cursor stands in, when the view types into its head (the
    /// picker's query).
    pub cursor: Option<(u16, u16)>,
    /// The visible lines, each with its mark.
    pub body: Vec<(Line, Mark)>,
    /// The rule above the picture's activity strip, when the strip shows.
    pub rule: Option<Line>,
    /// The picture's activity strip.
    pub strip: Vec<Line>,
    /// The footer, as one span. Only the editor is fitted to the footer here (its end is
    /// kept, with a cell left for the cursor); drawing clips the rest.
    pub footer: Span,
    /// A visible line moves: tick while it shows (V10).
    pub animate: bool,
    /// The earliest moment a visible text changes.
    pub wake_at: Option<UtcMs>,
}

/// The screen as text, for snapshots: the picture's tabs and brand; the picker's head; the
/// body's lines as [`debug_lines`] prints them, each after `> ` when selected, `~ ` when
/// hovered and two spaces otherwise; the picture's rule and strip; then the footer.
pub fn debug_screen(screen: &Screen) -> String {
    let mut out = String::new();
    if let Some((_, shown)) = &screen.tabs {
        let _ = writeln!(out, "tabs: {}", picture::tabs_text(*shown));
    }
    if let Some(brand) = &screen.brand {
        out.push_str("brand: ");
        out.push_str(&debug_lines(&[Line::new(vec![brand.clone()])]));
    }
    for line in &screen.head {
        out.push_str("head: ");
        out.push_str(&debug_lines(std::slice::from_ref(line)));
    }
    for (line, mark) in &screen.body {
        out.push_str(match mark {
            Mark::Plain => "  ",
            Mark::Hovered => "~ ",
            Mark::Selected => "> ",
        });
        out.push_str(&debug_lines(std::slice::from_ref(line)));
    }
    if let Some(line) = &screen.rule {
        out.push_str("rule: ");
        out.push_str(&debug_lines(std::slice::from_ref(line)));
    }
    for line in &screen.strip {
        out.push_str("strip: ");
        out.push_str(&debug_lines(std::slice::from_ref(line)));
    }
    let bold = if screen.footer.bold { "*" } else { "" };
    let _ = writeln!(
        out,
        "footer: [{}{bold}]{}",
        screen.footer.tone.word(),
        screen.footer.text
    );
    out
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Prompt {
    editor: Editor,
    ask: Ask,
}

/// A line in the footer. A broadcast event's ends at its deadline and is drawn dim; any
/// other (a reply, an error, the connection's) stays in the warning tone until a key.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Message {
    text: String,
    until: Option<UtcMs>,
}

impl Message {
    fn sticky(text: impl Into<String>) -> Message {
        Message {
            text: text.into(),
            until: None,
        }
    }
}

/// A y/N question and the op `y` sends, fixed when it was asked (V8).
#[derive(Clone, PartialEq, Eq, Debug)]
struct Confirm {
    question: String,
    op: Op,
}

/// One panel: its view, what it lists, what is selected and hovered, and the footer's state.
#[derive(Clone, Debug)]
pub struct Panel {
    view: View,
    ui: ViewUi,
    list: ListState,
    /// The lines of the last build.
    lines: Vec<Line>,
    prompt: Option<Prompt>,
    confirm: Option<Confirm>,
    /// A reply or a note, until the next key; an event's, until its deadline too.
    message: Option<Message>,
    /// The item under the pointer, kept by identity like the selection (V11).
    hover: Option<Item>,
    /// Where the pointer was last seen.
    pointer: Option<(u16, u16)>,
    /// moa gave this panel its view (its `panels` row says `managed`, L8).
    managed: bool,
    /// It draws in a tmux popup (`--once`, S8), which tmux frames: it draws no frame of its
    /// own (S11).
    popup: bool,
    /// Its `panels` row names a tab: moa can place it there.
    in_tab: bool,
    /// The sheet's open its row last said (S9): a new number opens it afresh, and `None` in
    /// a sheet moa placed is a closed one, which draws nothing.
    open: Option<i64>,
    /// The title of the pick the picker sent and waits on: its `Ok` closes the picker.
    /// Another Enter waits for it.
    picking: Option<String>,
    /// The tag of that pick's send: only its reply closes the view. Other ops the view sends
    /// (the terminals view's name, note, timeout and close) have replies of their own.
    pick_tag: Option<u64>,
    /// The picker's matches, kept while the data's revision and the query stay.
    picks: Option<Picks>,
    /// What the console keeps: its paged-in rows, cards and errors.
    console: ConsoleUi,
    /// The buttons of the console's last build (the text's, then the bar's), and the bar's
    /// lines.
    buttons: Vec<Button>,
    bar: Vec<Line>,
}

/// The picker's matches for one revision of the data and one query.
#[derive(Clone, Debug)]
struct Picks {
    revision: u64,
    query: String,
    matched: picker::Matched,
}

/// The picker's matches for `data` and `query`: those in `cache` while they still hold.
fn picks<'c>(cache: &'c mut Option<Picks>, data: &Data, query: &str) -> &'c picker::Matched {
    let stale = cache
        .as_ref()
        .is_none_or(|p| p.revision != data.revision() || p.query != query);
    if stale {
        *cache = Some(Picks {
            revision: data.revision(),
            query: query.to_string(),
            matched: picker::matched(data, query),
        });
    }
    &cache.as_ref().expect("just filled").matched
}

const CANCELLED: &str = "cancelled";
/// What Ctrl-c, or `q` in the picture, says in a panel moa manages (L14).
const MANAGED: &str = "this panel is part of moa's layout; Ctrl-a r repairs the layout";
/// How every y/N question ends.
const QUESTION_END: &str = "? y/N";
/// The managed line for a footer too narrow for [`MANAGED`] (31 columns).
const MANAGED_SHORT: &str = "moa's layout · Ctrl-a r repairs";

/// How long a broadcast event stays in the footer, in milliseconds.
pub const EVENT_SHOWN_MS: i64 = 8_000;

/// The body width a view is built for: at least 10 cells (V2).
fn build_width(g: &Geometry) -> u16 {
    g.body.w.max(10)
}

impl Panel {
    pub fn new(view: View) -> Panel {
        Panel {
            view,
            ui: ViewUi::default(),
            list: ListState::default(),
            lines: Vec::new(),
            prompt: None,
            confirm: None,
            message: None,
            hover: None,
            pointer: None,
            managed: false,
            popup: false,
            in_tab: false,
            open: None,
            picking: None,
            pick_tag: None,
            picks: None,
            console: ConsoleUi::default(),
            buttons: Vec::new(),
            bar: Vec::new(),
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Whether moa gave this panel its view, as its row last said (L8).
    pub fn managed(&self) -> bool {
        self.managed
    }

    /// Shows another view, from its top, with nothing asked or said and nothing the console
    /// kept. Whether moa manages the panel, whether it is a popup, and what its row last said
    /// of its tab and its sheet's open stay as they are.
    pub fn set_view(&mut self, view: View) {
        if view != self.view {
            let (managed, popup, in_tab, open) = (self.managed, self.popup, self.in_tab, self.open);
            *self = Panel::new(view);
            self.managed = managed;
            self.popup = popup;
            self.in_tab = in_tab;
            self.open = open;
        }
    }

    /// Follows the panel's own `panels` row (`key`, from `ui.panel`'s reply): the view moa
    /// gives it, and whether moa manages it (L8); for a sheet, its open (S9), a new one of
    /// which starts it afresh. A row for another key, or none, changes nothing. True when
    /// the view changed.
    pub fn follow(&mut self, data: &Data, key: &str) -> bool {
        let Some(row) = data.panels.get(key) else {
            return false;
        };
        self.managed = row.managed;
        self.in_tab = row.pane.is_some();
        let changed = row.view != self.view;
        self.set_view(row.view);
        if row.sheet.is_some() && row.sheet != self.open {
            self.reset_pick();
        }
        self.open = row.sheet;
        changed
    }

    /// A sheet moa placed in a tab whose row says it is closed (S9): it waits, parked, for
    /// its next open, and draws nothing meanwhile. One you started, or a popup, always
    /// draws.
    fn closed(&self) -> bool {
        self.view.is_sheet() && self.managed && self.in_tab && !self.popup && self.open.is_none()
    }

    /// Marks the panel as moa's before any row says so: moa tags a panel it places as it
    /// starts it (L7). The panel's row, once it comes, decides from then on.
    pub fn set_managed(&mut self, managed: bool) {
        self.managed = managed;
    }

    /// Marks the panel as drawn in a tmux popup (S8), which tmux frames, so it draws no frame
    /// of its own (S11).
    pub fn set_popup(&mut self, popup: bool) {
        self.popup = popup;
    }

    /// Ctrl-c (L14): it cancels an open prompt or y/N; with nothing open it quits as `q`
    /// does in the picture (see [`quit`](Panel::quit)). The picker and help close, as Esc
    /// closes the picker, and never say the managed line (S7, S9).
    pub fn interrupt(&mut self) -> Option<Out> {
        self.hover = None;
        if self.prompt.take().is_some() || self.confirm.take().is_some() {
            self.say(CANCELLED);
            return None;
        }
        if self.view.is_popup() {
            return Some(Out::Close);
        }
        self.quit()
    }

    /// The picker, or help, opens again (S9): the query empty, the first row selected and
    /// help at its top, nothing said.
    pub fn reset_pick(&mut self) {
        self.ui.query.clear();
        self.list = ListState::default();
        self.hover = None;
        self.message = None;
        self.forget_pick();
    }

    /// The connection ended: the pick's reply won't come, so another Enter may pick.
    pub fn forget_pick(&mut self) {
        self.picking = None;
        self.pick_tag = None;
    }

    /// An op the panel asked to send went out under `tag`. The first one after a pick is the
    /// pick's: its reply is the one that closes the view.
    pub fn sent(&mut self, tag: u64) {
        if self.picking.is_some() && self.pick_tag.is_none() {
            self.pick_tag = Some(tag);
        }
    }

    /// Quits a panel you started, and leaves one moa manages where it is, saying why
    /// (L14): a managed panel that quit would leave a hole in moa's layout.
    fn quit(&mut self) -> Option<Out> {
        if self.managed {
            self.say(MANAGED);
            None
        } else {
            Some(Out::Quit)
        }
    }

    /// The view the picture's shown tab draws; `None` for every other view.
    fn tab(&self) -> Option<View> {
        (self.view == View::Picture).then(|| picture::tab_view(&self.ui))
    }

    /// The view whose lines the body shows: the picture's tab, else the panel's own.
    fn shown(&self) -> View {
        self.tab().unwrap_or(self.view)
    }

    /// The selected line's item.
    pub fn selected(&self) -> Option<&Item> {
        self.list.selected(&self.lines)
    }

    /// Selects the line standing for `item`, if the last build listed it.
    pub fn select(&mut self, item: &Item) -> bool {
        let at = self
            .lines
            .iter()
            .position(|l| l.selectable() && l.item.as_ref() == Some(item));
        if at.is_some() {
            self.list.sel = at;
            self.list.tracking = true;
        }
        at.is_some()
    }

    /// Rebuilds the lines from `data`, keeps the selection on its item (V8) and the view on
    /// the selection, and drops a hover whose item left the list.
    fn refresh(&mut self, data: &Data, ctx: &Ctx, size: Size) -> Geometry {
        let g = geometry(self.view, self.view.framed() && !self.popup, size);
        let was = self.selected().cloned();
        let width = build_width(&g);
        self.lines = if self.view == View::Picker {
            let m = picks(&mut self.picks, data, &self.ui.query);
            picker::lines(m, &self.ui.query, ctx, width)
        } else if self.view == View::Console {
            let mut built = console::build(data, ctx, &self.console, width);
            self.buttons = built.buttons;
            self.bar = built.bar;
            std::mem::take(&mut built.lines)
        } else {
            views::build(self.view, data, ctx, &self.ui, width)
        };
        self.list.reselect(was.as_ref(), &self.lines);
        self.list.follow(&self.lines, usize::from(g.body.h));
        if let Some(hover) = &self.hover {
            let listed = self
                .lines
                .iter()
                .any(|l| l.selectable() && l.item.as_ref() == Some(hover));
            if !listed {
                self.hover = None;
            }
        }
        g
    }

    /// A key press. It clears the hover and the last message; Ctrl-c is
    /// [`interrupt`](Panel::interrupt) from anywhere, and a prompt or a question takes any
    /// other key first.
    pub fn key(&mut self, k: Key, data: &Data, ctx: &Ctx, size: Size) -> Vec<Out> {
        let g = self.refresh(data, ctx, size);
        self.hover = None;
        self.message = None;
        if k == Key::Ctrl('c') {
            return self.interrupt().into_iter().collect();
        }
        if let Some(confirm) = self.confirm.take() {
            return if k == Key::Char('y') {
                vec![Out::Send(confirm.op)]
            } else {
                Vec::new()
            };
        }
        if let Some(mut prompt) = self.prompt.take() {
            return match prompt.editor.key(k) {
                Edited::Typing => {
                    self.prompt = Some(prompt);
                    Vec::new()
                }
                Edited::Cancelled => self.say(CANCELLED),
                Edited::Done(text) => self.answered(prompt.ask, text, data, ctx),
            };
        }
        let body = usize::from(g.body.h);
        if self.view == View::Picker {
            return self.pick_key(k, data, body);
        }
        let tab = self.ui.tab;
        let mut out = Vec::new();
        // The picture's rows follow its tab's keys (V21).
        for action in keys::actions(self.view, self.tab(), k) {
            if let Some(done) = self.act(*action, data, ctx, body) {
                out = done;
                break;
            }
        }
        if self.ui.tab != tab {
            // The new tab's lines, so its first item is selected now.
            self.refresh(data, ctx, size);
        }
        out
    }

    /// A mouse event (V11): a move marks the row under the pointer, a click selects it and
    /// acts as Enter, the wheel scrolls three lines.
    pub fn mouse(&mut self, m: Mouse, data: &Data, ctx: &Ctx, size: Size) -> Handled {
        let g = self.refresh(data, ctx, size);
        let was = self.hover.clone();
        let mut out = Vec::new();
        match m {
            Mouse::Move { x, y } => {
                self.pointer = Some((x, y));
                self.hover = self.item_at(g.body, x, y);
            }
            Mouse::Click { x, y } => {
                self.pointer = Some((x, y));
                if let Some(command) = self.button_at(g.body, x, y) {
                    // A click on a button presses it.
                    self.message = None;
                    let out = vec![Out::Send(Op::ConsoleCommand { line: command })];
                    return Handled { out, redraw: true };
                }
                let hit = self.row_at(g.body, x, y);
                self.hover = hit.and_then(|i| self.lines[i].item.clone());
                if let Some(i) = hit {
                    if self.confirm.take().is_some() {
                        // A click answers a question no.
                        return Handled { out, redraw: true };
                    }
                    if self.prompt.is_none() {
                        self.list.sel = Some(i);
                        self.list.tracking = true;
                        self.message = None;
                        out = self
                            .act(Action::Activate, data, ctx, usize::from(g.body.h))
                            .unwrap_or_default();
                        return Handled { out, redraw: true };
                    }
                }
            }
            Mouse::WheelUp | Mouse::WheelDown => {
                let down = m == Mouse::WheelDown;
                self.list.wheel(&self.lines, usize::from(g.body.h), down);
                self.hover = self.pointer.and_then(|(x, y)| self.item_at(g.body, x, y));
                return Handled { out, redraw: true };
            }
        }
        Handled {
            out,
            redraw: was != self.hover,
        }
    }

    /// The command of the console's button drawn at screen cell (`x`, `y`), if any.
    fn button_at(&self, body: Rect, x: u16, y: u16) -> Option<String> {
        if self.view != View::Console || !body.contains(x, y) {
            return None;
        }
        let line = self.lines.get(self.list.top + usize::from(y - body.y))?;
        let button = console::button_at(line, usize::from(x - body.x))?;
        self.buttons
            .get(usize::from(button))
            .map(|b| b.command.clone())
    }

    /// The selectable line drawn at screen cell (`x`, `y`), if any.
    fn row_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        if !body.contains(x, y) {
            return None;
        }
        self.list
            .row_at(&self.lines, usize::from(y - body.y), usize::from(body.h))
    }

    fn item_at(&self, body: Rect, x: u16, y: u16) -> Option<Item> {
        self.row_at(body, x, y)
            .and_then(|i| self.lines[i].item.clone())
    }

    /// Focus left the panel: the hover mark goes. True when there was one.
    pub fn blur(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// Whether a prompt is being answered: the footer is its editor, and the terminal's
    /// cursor stands after it.
    pub fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Whether the next key is typed into the footer: a prompt's editor or a y/N question.
    pub fn typing(&self) -> bool {
        self.prompt.is_some() || self.confirm.is_some()
    }

    /// Whether `k` quits this panel when nothing is being typed: Ctrl-c, `q` in the picture,
    /// and the picker's closing keys ([`pick_keys::closes`]), in a panel moa doesn't manage
    /// (L14).
    pub fn quits(&self, k: Key) -> bool {
        if self.managed {
            false
        } else if self.view == View::Picker {
            pick_keys::closes(k)
        } else {
            keys::actions(self.view, self.tab(), k).contains(&Action::Quit)
        }
    }

    /// Pasted text goes to the prompt being answered, else into the picker's query.
    pub fn paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.editor.paste(text);
        } else if self.view == View::Picker {
            self.edit_query(|e| e.paste(text));
        }
    }

    /// An op's reply: the footer shows its text's last non-empty line, or nothing. The
    /// pick that went through closes the view ([`Out::Close`]); one refused stays in its
    /// footer, the view still open (S7). Only the reply to the pick's own send (`tag`, see
    /// [`Panel::sent`]) counts: another op's reply neither closes nor frees the pick.
    pub fn replied(&mut self, tag: u64, result: Result<Done, String>) -> Option<Out> {
        let ok = self.show(result);
        if self.picking.is_some() && self.pick_tag == Some(tag) {
            self.forget_pick();
            return ok.then_some(Out::Close);
        }
        None
    }

    /// The footer shows a result's last non-empty line, or nothing, and says whether it was
    /// an `Ok`. It answers no pick, so it never closes the view.
    pub fn show(&mut self, result: Result<Done, String>) -> bool {
        let ok = result.is_ok();
        let text = match result {
            Ok(done) => done.text,
            Err(e) => e,
        };
        self.message = text
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(Message::sticky);
        ok
    }

    /// A line for the footer from the panel's loop (the connection, say, or an error event),
    /// until the next key.
    pub fn note(&mut self, text: &str) {
        self.message = Some(Message::sticky(text));
    }

    /// A broadcast event that came at `now`: the footer shows it, dim, for
    /// [`EVENT_SHOWN_MS`] or until a key. It never hides a line that stays until a key.
    pub fn event(&mut self, text: &str, now: UtcMs) {
        if self.message.as_ref().is_some_and(|m| m.until.is_none()) {
            return;
        }
        self.message = Some(Message {
            text: text.to_string(),
            until: Some(UtcMs(now.0.saturating_add(EVENT_SHOWN_MS))),
        });
    }

    /// The message the footer may show at `now`: none once an event's deadline has passed.
    fn message_at(&self, now: UtcMs) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|m| m.until.is_none_or(|t| now < t))
    }

    /// The frame to draw now. A closed sheet's is blank, with nothing to move or change.
    pub fn screen(&mut self, data: &Data, ctx: &Ctx, size: Size) -> Screen {
        if self.closed() {
            return Screen {
                geometry: geometry(self.view, false, size),
                title: None,
                tabs: None,
                brand: None,
                head: Vec::new(),
                cursor: None,
                body: Vec::new(),
                rule: None,
                strip: Vec::new(),
                footer: Span::new("", Tone::Dim),
                animate: false,
                wake_at: None,
            };
        }
        let g = self.refresh(data, ctx, size);
        let shown = self
            .lines
            .iter()
            .enumerate()
            .skip(self.list.top)
            .take(usize::from(g.body.h));
        let body: Vec<(Line, Mark)> = shown.map(|(i, l)| (l.clone(), self.mark(i))).collect();
        let mut tabs = None;
        let mut brand = None;
        let mut head = Vec::new();
        let mut cursor = None;
        if let Some(r) = g.head.filter(|r| r.h > 0 && r.w > 0) {
            let m = picks(&mut self.picks, data, &self.ui.query);
            let built = picker::head(m, &self.ui.query, build_width(&g));
            head = built.lines.into_iter().take(usize::from(r.h)).collect();
            let x = u16::try_from(built.cursor).unwrap_or(u16::MAX).min(r.w - 1);
            cursor = Some((r.x + x, r.y));
        }
        let mut rule = None;
        let mut strip = Vec::new();
        if self.view == View::Picture {
            let tab = self.ui.tab % picture::TABS.len();
            tabs = Some((picture::tab_names(), tab));
            // Only the strip's drawn rows make the brand spin: none when it is left out.
            let strip_rows = g.strip.map_or(0, |r| r.h);
            let line = picture::brand(data, ctx, strip_rows);
            // A tab row with no cells shows no brand, so nothing spins for it.
            let row = g.tabs.filter(|r| r.h > 0).map_or(0, |r| r.w);
            brand = picture::brand_fits(tab, &line, row).then_some(line);
            if let Some(r) = g.strip {
                rule = Some(picture::rule(r.w));
                strip = picture::strip(data, ctx, r.w, r.h);
            }
        }
        // Only what is on screen moves or changes (V10).
        let visible = || body.iter().map(|(l, _)| l).chain(&strip).chain(&brand);
        let animate = visible().any(|l| l.animated);
        // An event in the footer goes at its deadline.
        let footer_ends = if self.typing() {
            None
        } else {
            self.message_at(ctx.now).and_then(|m| m.until)
        };
        // A deadline already past would wake the panel again at once, forever.
        let wake_at = visible()
            .filter_map(|l| l.changes_at)
            .chain(footer_ends)
            .filter(|t| *t > ctx.now)
            .min();
        Screen {
            title: g.framed.then(|| self.view.label().to_string()),
            tabs,
            brand: brand.and_then(|l| l.spans.into_iter().next()),
            head,
            cursor,
            body,
            rule,
            strip,
            footer: self.footer(data, ctx.now, g.footer.w),
            animate,
            wake_at,
            geometry: g,
        }
    }

    /// Line `i`'s mark; a wrapped line takes its item's.
    fn mark(&self, i: usize) -> Mark {
        let mut owner = i;
        while owner > 0 && self.lines[owner].more {
            owner -= 1;
        }
        let line = &self.lines[owner];
        if !line.selectable() {
            Mark::Plain
        } else if self.list.sel == Some(owner) {
            Mark::Selected
        } else if line.item.is_some() && line.item == self.hover {
            Mark::Hovered
        } else {
            Mark::Plain
        }
    }

    /// The footer: the prompt, else the question, else the message, else the count of rows
    /// that couldn't be read (V18), else the key hints.
    fn footer(&self, data: &Data, now: UtcMs, width: u16) -> Span {
        if let Some(prompt) = &self.prompt {
            let shown = prompt.editor.shown(usize::from(width).saturating_sub(1));
            return Span::bold(shown, Tone::Text);
        }
        if let Some(confirm) = &self.confirm {
            return Span::new(
                fit_question(&clean(&confirm.question), usize::from(width)),
                Tone::Warn,
            );
        }
        if let Some(message) = self.message_at(now) {
            let tone = if message.until.is_some() {
                Tone::Dim
            } else {
                Tone::Warn
            };
            let text = if message.text == MANAGED && text::width(MANAGED) > usize::from(width) {
                MANAGED_SHORT.to_string()
            } else {
                clean(&message.text)
            };
            return Span::new(text::ellipsize(&text, usize::from(width)), tone);
        }
        let unreadable = data.unreadable();
        if unreadable > 0 {
            return Span::new(
                text::ellipsize(
                    &format!("{unreadable} rows from moa couldn't be shown"),
                    usize::from(width),
                ),
                Tone::Warn,
            );
        }
        let adopted = self.selected_session(data).is_some_and(|row| row.adopted);
        // The picture's hints are its shown tab's (V21).
        let hint = match self.tab() {
            Some(tab) => keys::picture_hint(tab, adopted),
            None => keys::hint(self.view, adopted),
        };
        Span::new(hint, Tone::Dim)
    }

    /// The folder the selected row is in: a session's, a project's, a task's session's, a
    /// terminal's.
    pub fn here(&self, data: &Data) -> Option<String> {
        match self.selected()? {
            Item::Session(id) => data.sessions.get(id)?.session.cwd.clone(),
            Item::Project(id) => data.projects.get(id)?.path.clone(),
            Item::Task(id) => {
                let session = data.tasks.get(id)?.session_id?;
                data.sessions.get(&session)?.session.cwd.clone()
            }
            Item::Term { pane } => {
                Some(data.terms.get(pane)?.folder.clone()).filter(|f| !f.is_empty())
            }
            Item::Group(_) | Item::Suggestion(_) => None,
        }
    }

    fn selected_session<'d>(&self, data: &'d Data) -> Option<&'d SessionRow> {
        match self.selected()? {
            Item::Session(id) => data.sessions.get(id),
            _ => None,
        }
    }

    /// The project of the selected row, for `n` and `N` (V21): `None` when the row can't have
    /// one (a group, a suggestion), `Some(None)` when it has none.
    fn selected_project<'d>(&self, data: &'d Data) -> Option<Option<&'d Project>> {
        let id = match self.selected()? {
            Item::Session(id) => data.sessions.get(id)?.session.project_id,
            Item::Task(id) => data.tasks.get(id)?.project_id,
            Item::Project(id) => Some(*id),
            Item::Group(_) | Item::Suggestion(_) | Item::Term { .. } => return None,
        };
        Some(id.and_then(|id| data.projects.get(&id)))
    }

    fn say(&mut self, text: &str) -> Vec<Out> {
        self.message = Some(Message::sticky(text));
        Vec::new()
    }

    fn ask(&mut self, label: String, prefill: String, ask: Ask) -> Vec<Out> {
        self.prompt = Some(Prompt {
            editor: Editor::new(label, prefill),
            ask,
        });
        Vec::new()
    }

    fn confirm(&mut self, question: String, op: Op) -> Vec<Out> {
        self.confirm = Some(Confirm { question, op });
        Vec::new()
    }

    /// Does `action` to the selected item. `None` when it doesn't apply to it, so the key's
    /// next meaning is tried.
    fn act(&mut self, action: Action, data: &Data, ctx: &Ctx, body: usize) -> Option<Vec<Out>> {
        let selected = self.selected().cloned();
        let session = self.selected_session(data);
        match action {
            Action::Down | Action::Up => self.list.step(&self.lines, action == Action::Down),
            Action::Top | Action::Bottom => self.list.ends(&self.lines, action == Action::Bottom),
            Action::PageUp | Action::PageDown => {
                self.list
                    .page(&self.lines, body, action == Action::PageDown);
            }
            // Help's and the terminals view's `q` and Esc close them, as Ctrl-c does (S10).
            Action::Quit if self.view.is_popup() && self.view != View::Picker => {
                return Some(vec![Out::Close]);
            }
            Action::Quit => return Some(self.quit().into_iter().collect()),
            Action::Activate if self.view.is_popup() && self.view != View::Help => {
                return Some(self.pick(data));
            }
            Action::Activate => {
                return match selected? {
                    Item::Session(id) => {
                        Some(vec![Out::Send(Op::SessionFocus { q: id.to_string() })])
                    }
                    Item::Group(path) => {
                        let folded = data
                            .brain
                            .as_ref()
                            .is_some_and(|b| b.folded.contains(&path));
                        Some(vec![Out::Send(Op::GroupFold {
                            path,
                            collapsed: !folded,
                        })])
                    }
                    Item::Task(id) => match data.tasks.get(&id)?.session_id {
                        Some(s) => Some(vec![Out::Send(Op::SessionFocus { q: s.to_string() })]),
                        None => Some(self.say("no session linked to this task")),
                    },
                    // Opens or closes the project's rows; the selection stays on it (V8).
                    Item::Project(id) => {
                        if !self.ui.expanded.remove(&id) {
                            self.ui.expanded.insert(id);
                        }
                        Some(Vec::new())
                    }
                    Item::Suggestion(_) | Item::Term { .. } => None,
                };
            }
            Action::Close | Action::Rename | Action::Note | Action::Timeout
                if self.view == View::Terms =>
            {
                return self.term_key(action, data);
            }
            Action::NewSession if self.shown() == View::Tree => {
                return self.tree_key(action, data, ctx);
            }
            Action::NewSession | Action::ChooseAccount => {
                let project = self.selected_project(data)?;
                let choose = action == Action::ChooseAccount;
                return Some(self.open_in_project(project, choose, data, ctx));
            }
            Action::NewGroup | Action::Move | Action::Rename | Action::DeleteGroup => {
                return self.tree_key(action, data, ctx);
            }
            Action::Archive => {
                let q = session?.session.id.to_string();
                return Some(vec![Out::Send(Op::SessionArchive { qs: vec![q] })]);
            }
            Action::Close => {
                let row = session?;
                let question = format!("close {}? y/N", tree::name(row));
                let q = row.session.id.to_string();
                return Some(self.confirm(question, Op::SessionClose { q }));
            }
            Action::Restart => {
                let row = session.filter(|r| r.adopted)?;
                let question = format!("restart {} under moa? y/N", tree::name(row));
                let q = row.session.id.to_string();
                return Some(self.confirm(question, Op::SessionRestart { q }));
            }
            // Only the terminals view's keys: it takes them before this.
            Action::Note | Action::Timeout => return None,
            Action::Done | Action::Drop => {
                let Item::Task(id) = selected? else {
                    return None;
                };
                let status = if action == Action::Done {
                    TaskStatus::Done
                } else {
                    TaskStatus::Dropped
                };
                return Some(vec![Out::Send(Op::TaskSet(TaskSet {
                    id: id.0,
                    status: Some(status.as_str().to_string()),
                    ..TaskSet::default()
                }))]);
            }
            // `y` and `n` answer a suggestion; on any other row `n` is a new session.
            Action::Yes | Action::No => {
                let Item::Suggestion(id) = selected? else {
                    return None;
                };
                let answer = if action == Action::Yes {
                    Answer::Yes
                } else {
                    Answer::No
                };
                return Some(vec![Out::Send(Op::SuggestionAnswer {
                    id: Some(id.0),
                    answer,
                })]);
            }
            Action::NextTab | Action::PrevTab | Action::Tab(_) => {
                if self.view != View::Picture {
                    return None;
                }
                let n = picture::TABS.len();
                let tab = self.ui.tab % n;
                let next = match action {
                    Action::NextTab => (tab + 1) % n,
                    Action::PrevTab => (tab + n - 1) % n,
                    Action::Tab(d) => usize::from(d).clamp(1, n) - 1,
                    _ => tab,
                };
                // The new tab opens at its top, its first item selected; the shown tab's
                // digit leaves it as it is.
                if next != tab {
                    self.ui.tab = next;
                    self.list = ListState::default();
                    self.hover = None;
                }
            }
        }
        // A move through the list: nothing to send.
        Some(Vec::new())
    }
}

/// A y/N question as it fits `w` cells: a long one loses the end of its text, never its
/// `? y/N`, so what is being asked stays clear.
fn fit_question(question: &str, w: usize) -> String {
    match question.strip_suffix(QUESTION_END) {
        Some(head) if text::width(question) > w && w > text::width(QUESTION_END) => {
            let room = w.saturating_sub(text::width(QUESTION_END));
            format!("{}{QUESTION_END}", text::ellipsize(head, room))
        }
        _ => text::ellipsize(question, w),
    }
}
