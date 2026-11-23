//! The console's own keys and clicks (B7, B8): typing to moa, its history, Tab over the
//! buttons, scrolling and paging back; what comes back for what it sent; and the parts of its
//! screen (B9).

use moa_core::UtcMs;
use moa_core::op::{Done, NoParams, Op};
use moa_core::panel::View;
use moa_core::publish::FeedRow;
use serde_json::Value;

use super::{Mark, Out, Panel, build_width};
use crate::ctx::Ctx;
use crate::data::Data;
use crate::editor::Editor;
use crate::geometry::Geometry;
use crate::keys::Key;
use crate::line::Line;
use crate::views::console::{self, Asked, Card};

/// The same error text is drawn in the console at most once in this many milliseconds (B10).
pub const ERROR_AGAIN_MS: i64 = 30_000;
/// What `/quit` says in a console moa placed in a layout (F10).
pub const CONSOLE_MANAGED: &str = "the console is part of this layout; switch layouts to close it";
/// How many rows a page asks for (B2).
pub const PAGE: u16 = 100;
/// What Enter says while a typed line waits for its reply.
pub const STILL_WAITING: &str = "still waiting for the reply to the last line";
/// The console keeps its newest this many error lines.
const ERRORS_KEPT: usize = 50;
/// The history keeps the newest this many lines sent from the panel.
const SENT_KEPT: usize = 200;

/// The console's parts of a screen.
pub(super) struct Parts {
    pub head: Vec<Line>,
    pub body: Vec<(Line, Mark)>,
    pub cursor: Option<(u16, u16)>,
    pub input: Option<Line>,
    pub palette: Vec<Line>,
    pub bar: Vec<Line>,
}

/// Whether `line` is `/quit`, which the console does itself and never sends (F10).
fn quits(line: &str) -> bool {
    line.split_whitespace().next() == Some("/quit")
}

fn thinking(data: &Data) -> bool {
    data.moa.as_ref().is_some_and(|m| m.phase == "thinking")
}

impl Panel {
    /// Builds the console for `g`, and gives `g` back with as many bar rows as the bar has:
    /// its focused button found again (by its place and command), the bar cut to the rows
    /// it got, and what its feed shows. Once it has paged, it keeps every row the topic shows
    /// it, so a row that leaves the topic's tail leaves no gap.
    pub(super) fn console_refresh(&mut self, data: &Data, ctx: &Ctx, g: Geometry) -> Geometry {
        let ui = &mut self.console;
        // `older` grows for the panel's life once it has paged: it is never cut.
        if !ui.older.is_empty() && ui.kept_at != Some(data.revision()) {
            ui.kept_at = Some(data.revision());
            for (id, row) in &data.feed {
                if ui.older.get(id) != Some(row) {
                    ui.older.insert(*id, row.clone());
                }
            }
        }
        let mut built = console::build(data, ctx, ui, build_width(&g));
        let rows = u16::try_from(built.bar.len()).unwrap_or(u16::MAX);
        let g = g.with_bar_rows(rows);
        let open = console::palette(data, ui).map_or(0, |p| p.entries.len());
        let g = g.with_palette_rows(u16::try_from(open).unwrap_or(u16::MAX));
        built.bar.truncate(usize::from(g.bar.map_or(0, |r| r.h)));
        let focus = ui.focused.as_ref().and_then(|f| built.find(f));
        if focus != ui.focus {
            built.set_focus(focus);
            ui.focus = focus;
        }
        ui.focused = focus.and_then(|b| built.focused(b));
        self.window = ui.window(&built, usize::from(g.body.h));
        self.built = built;
        self.keep_focus_in_sight();
        g
    }

    /// The focus goes when its button is out of sight, as Tab takes it to be: Enter never
    /// presses a button you can't see.
    fn keep_focus_in_sight(&mut self) {
        let Some(focus) = self.console.focus else {
            return;
        };
        let visible = self.window.top..self.window.end;
        if !self.built.order(visible).contains(&focus) {
            self.focus(None);
        }
    }

    /// A key in the console, after Ctrl-c, a question and a prompt have had theirs. Every
    /// printable key is typed (letters never move the feed); Enter sends the draft, or with
    /// none presses the focused button; Esc cancels moa's turn while it thinks, else clears
    /// the draft; Tab and BTab move the focus while the draft is empty; Up and Down walk the
    /// history; PgUp and PgDn scroll `rows` lines less one, and PgUp at the top asks for a
    /// page of older rows.
    pub(super) fn console_key(&mut self, k: Key, data: &Data, ctx: &Ctx, rows: usize) -> Vec<Out> {
        if self.console.draft.step(k) {
            return Vec::new();
        }
        if self.palette_key(k, data) {
            return Vec::new();
        }
        let page = isize::try_from(rows.saturating_sub(1).max(1)).unwrap_or(isize::MAX);
        match k {
            Key::Enter => {
                let line = self.console.draft.text.trim().to_string();
                if !line.is_empty() {
                    return self.send_line(line, true, ctx.now);
                }
                let focused = self
                    .console
                    .focus
                    .and_then(|b| self.built.buttons.get(usize::from(b)));
                if let Some(button) = focused {
                    return self.press(button.command.clone(), ctx.now);
                }
            }
            Key::Esc if thinking(data) => return vec![Out::Send(Op::ConsoleCancel(NoParams))],
            Key::Esc => {
                self.clear_draft();
                self.focus(None);
            }
            Key::Tab | Key::BackTab if self.console.draft.text.is_empty() => {
                let visible = self.window.top..self.window.end;
                let focus = self
                    .built
                    .tab(visible, self.console.focus, k == Key::BackTab);
                self.focus(focus);
            }
            Key::Up | Key::Down => self.walk_history(data, k == Key::Up),
            Key::PageUp if self.window.top == 0 => return self.page(),
            Key::PageUp => self.console_scroll(rows, -page),
            Key::PageDown => self.console_scroll(rows, page),
            _ => {
                self.console.draft.key(k);
                self.console.pick = 0;
                self.console.closed = None;
                if !self.console.draft.text.is_empty() {
                    self.focus(None);
                }
            }
        }
        Vec::new()
    }

    /// A key while the palette is open: Tab completes the word with the chosen entry, Up and
    /// Down (and BackTab) choose, Esc closes it until the draft changes. Whether the palette
    /// took the key.
    fn palette_key(&mut self, k: Key, data: &Data) -> bool {
        let Some(p) = console::palette(data, &self.console) else {
            return false;
        };
        let ui = &mut self.console;
        let n = p.entries.len();
        let pick = ui.pick.min(n - 1);
        match k {
            Key::Tab => {
                let e = &p.entries[pick];
                let mut text = ui.draft.text[..p.from].to_string();
                text.push_str(&e.insert);
                ui.draft.set(text);
                ui.pick = 0;
                // A name leads on to its arguments; an id or a name ends the word.
                ui.closed = (!e.chain).then(|| ui.draft.text.clone());
            }
            Key::Up | Key::BackTab => ui.pick = (pick + n - 1) % n,
            Key::Down => ui.pick = (pick + 1) % n,
            Key::Esc => ui.closed = Some(ui.draft.text.clone()),
            _ => return false,
        }
        true
    }

    /// Focuses button `focus` of the last build, or none.
    fn focus(&mut self, focus: Option<u16>) {
        self.console.focus = focus;
        self.console.focused = focus.and_then(|b| self.built.focused(b));
        self.built.set_focus(focus);
    }

    /// A button pressed, by Enter or a click (B13): its command, sent; its reply is a card.
    pub(super) fn press(&mut self, command: String, now: UtcMs) -> Vec<Out> {
        self.send_line(command, false, now)
    }

    /// Sends `line`, `typed` in the input or pressed: a `/command` as `console.command`, else
    /// a prompt; `/quit` is the console's own (F10). The feed goes back to its bottom, where
    /// the answer comes. A typed line waits for its reply before another goes; the draft is
    /// cleared only once it is accepted (B7).
    fn send_line(&mut self, line: String, typed: bool, now: UtcMs) -> Vec<Out> {
        if quits(&line) {
            return if self.managed {
                self.say(CONSOLE_MANAGED)
            } else {
                vec![Out::Quit]
            };
        }
        let ui = &mut self.console;
        let typing = |a: &Asked| match a {
            Asked::Prompt { .. } => true,
            Asked::Command { typed, .. } => *typed,
            Asked::Page { .. } => false,
        };
        if typed && (ui.asked.as_ref().is_some_and(typing) || ui.waits.values().any(typing)) {
            return self.say(STILL_WAITING);
        }
        ui.to_bottom();
        let (asked, op) = if line.starts_with('/') {
            let asked = Asked::Command {
                line: line.clone(),
                at: now,
                typed,
            };
            (asked, Op::ConsoleCommand { line })
        } else {
            let asked = Asked::Prompt { text: line.clone() };
            (
                asked,
                Op::ConsolePrompt {
                    text: line,
                    wait: false,
                },
            )
        };
        ui.asked = Some(asked);
        vec![Out::Send(op)]
    }

    /// `console.page` for the rows before the oldest loaded: one at a time, and none once a
    /// page came back short (B2).
    fn page(&mut self) -> Vec<Out> {
        let ui = &mut self.console;
        let Some(&(oldest, _)) = self.built.starts.first() else {
            return Vec::new();
        };
        if ui.exhausted || ui.paging() {
            return Vec::new();
        }
        ui.asked = Some(Asked::Page { n: PAGE });
        vec![Out::Send(Op::ConsolePage {
            before: oldest,
            n: PAGE,
        })]
    }

    /// Scrolls the feed `by` lines, `rows` high (F15).
    pub(super) fn console_scroll(&mut self, rows: usize, by: isize) {
        self.console.scroll(&self.built, rows, by);
        self.window = self.console.window(&self.built, rows);
        self.keep_focus_in_sight();
    }

    /// Up (older) or Down (newer) through the history, the draft kept to come back to.
    fn walk_history(&mut self, data: &Data, up: bool) {
        let ui = &mut self.console;
        let next = match (ui.hist_at, up) {
            (None, true) => {
                ui.history = console::history(data, ui);
                ui.kept = ui.draft.text.clone();
                ui.history.len().checked_sub(1)
            }
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < ui.history.len() => Some(i + 1),
            (Some(_), false) => {
                ui.hist_at = None;
                let kept = std::mem::take(&mut ui.kept);
                ui.draft.set(kept);
                return;
            }
            (None, false) => None,
        };
        if let Some(i) = next {
            ui.hist_at = Some(i);
            ui.draft.set(ui.history[i].clone());
            // A recalled line isn't one being typed: no palette for it.
            ui.closed = Some(ui.draft.text.clone());
        }
    }

    fn clear_draft(&mut self) {
        let ui = &mut self.console;
        ui.draft = Editor::default();
        ui.pick = 0;
        ui.hist_at = None;
        ui.history.clear();
    }

    /// The reply to something the console sent: a page's rows join the older ones; an
    /// accepted line leaves the draft (if it is still what was sent) for the history; a
    /// command's reply is a card. A refusal goes to the footer, the draft kept.
    pub(super) fn console_replied(&mut self, asked: Asked, result: Result<Done, String>) {
        let done = match result {
            Ok(done) => done,
            Err(why) => {
                self.show(Err(why));
                return;
            }
        };
        match asked {
            Asked::Page { n } => {
                let rows = done.data.get("rows").and_then(Value::as_array);
                let rows = rows.map_or(&[][..], Vec::as_slice);
                let ui = &mut self.console;
                ui.exhausted = rows.len() < usize::from(n);
                ui.kept_at = None;
                for row in rows {
                    if let Ok(row) = serde_json::from_value::<FeedRow>(row.clone()) {
                        ui.older.insert(row.id, row);
                    }
                }
            }
            Asked::Prompt { text } => {
                self.accepted(text);
                self.show(Ok(done));
            }
            Asked::Command { line, at, typed } => {
                if typed {
                    self.accepted(line.clone());
                }
                self.console.cards.push(Card {
                    at,
                    line,
                    text: done.text,
                });
            }
        }
    }

    /// `line`, typed and sent, went through: the history has it, and the draft clears if it
    /// is still that line.
    fn accepted(&mut self, line: String) {
        if self.console.draft.text.trim() == line {
            self.clear_draft();
        }
        let sent = &mut self.console.sent;
        sent.push(line);
        sent.drain(..sent.len().saturating_sub(SENT_KEPT));
    }

    /// An error the daemon told of at `now` (B10): a `✗` line in the feed, unless the same
    /// text was drawn less than [`ERROR_AGAIN_MS`] ago.
    pub(super) fn console_error(&mut self, text: &str, now: UtcMs) {
        let ui = &mut self.console;
        let recent = |t: &UtcMs| now.0.saturating_sub(t.0) < ERROR_AGAIN_MS;
        if ui.shown.get(text).is_some_and(recent) {
            return;
        }
        // Only the texts drawn in the last 30 s are remembered.
        ui.shown.retain(|_, t| recent(t));
        ui.shown.insert(text.to_string(), now);
        ui.errors.push((now, text.to_string()));
        ui.errors
            .drain(..ui.errors.len().saturating_sub(ERRORS_KEPT));
    }

    /// The console's head line, feed, input line (and the cursor in it) and bar, for `g`
    /// (B9). The feed's top row is `↑ N more` when lines are above it.
    pub(super) fn console_parts(&self, data: &Data, ctx: &Ctx, g: &Geometry) -> Parts {
        let width = build_width(g);
        let w = self.window;
        let mut body = Vec::new();
        if w.more > 0 {
            body.push((console::more(w.more, width), Mark::Plain));
        }
        let shown = self.built.lines.get(w.top..w.end).unwrap_or_default();
        body.extend(shown.iter().map(|l| (l.clone(), Mark::Plain)));
        let head = g
            .head
            .filter(|r| r.h > 0)
            .map(|_| console::head(data, ctx, !self.link_down, w.new, width));
        let palette = console::palette(data, &self.console)
            .zip(g.palette)
            .map(|(p, r)| console::palette_lines(&p, self.console.pick, usize::from(r.h), r.w))
            .unwrap_or_default();
        let (input, cursor) = match g.input.filter(|r| r.h > 0 && r.w > 0) {
            Some(r) => {
                let (line, col) = console::input(&self.console, width);
                let x = u16::try_from(col).unwrap_or(u16::MAX).min(r.w - 1);
                (Some(line), Some((r.x + x, r.y)))
            }
            None => (None, None),
        };
        Parts {
            head: head.into_iter().collect(),
            body,
            cursor,
            input,
            palette,
            bar: self.built.bar.clone(),
        }
    }

    /// The command of the console's button drawn at screen cell (`x`, `y`), in its feed or
    /// its bar, if any.
    pub(super) fn button_at(&self, g: &Geometry, x: u16, y: u16) -> Option<String> {
        if self.view != View::Console {
            return None;
        }
        let w = self.window;
        let (line, part) = if g.body.contains(x, y) {
            let row = usize::from(y - g.body.y);
            let more = usize::from(w.more > 0);
            let i = (w.top + row).checked_sub(more).filter(|_| row >= more)?;
            (self.built.lines.get(..w.end)?.get(i)?, g.body)
        } else {
            let bar = g.bar.filter(|r| r.contains(x, y))?;
            (self.built.bar.get(usize::from(y - bar.y))?, bar)
        };
        let button = console::button_at(line, usize::from(x - part.x))?;
        self.built
            .buttons
            .get(usize::from(button))
            .map(|b| b.command.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use jiff::tz::TimeZone;
    use moa_core::UtcMs;
    use moa_core::op::Op;
    use moa_core::panel::View;
    use moa_core::publish::FeedRow;
    use serde_json::json;

    use super::super::ask::Ask;
    use super::super::{Confirm, Mouse, Out, Panel, Prompt};
    use crate::ctx::Ctx;
    use crate::data::Data;
    use crate::editor::Editor;
    use crate::geometry::Size;

    const SIZE: Size = Size { w: 80, h: 16 };

    /// A click on a console button follows the panel's rules: an open question is answered
    /// no and an open prompt ignores it, neither pressing it; with neither, it presses.
    #[test]
    fn a_click_on_a_button_answers_a_question_first() {
        let ctx = Ctx::at(
            UtcMs(1_795_082_400_000),
            TimeZone::UTC,
            0,
            PathBuf::from("/h"),
            |_: &Path| false,
        );
        let mut data = Data::default();
        let row = FeedRow {
            id: 1,
            thread: "t".into(),
            at: 1_000,
            kind: "moa".into(),
            message_id: None,
            automatic: false,
            body: json!({ "text": "go `/brief` now" }),
            state: None,
        };
        data.feed.insert(1, row);
        data.touch();
        let mut panel = Panel::new(View::Console);
        let screen = panel.screen(&data, &ctx, SIZE);
        let (y, line) = screen
            .body
            .iter()
            .enumerate()
            .find(|(_, (l, _))| l.text().contains("[/brief]"))
            .map(|(y, (l, _))| (y, l.text()))
            .expect("the button is drawn");
        let col = line[..line.find('[').expect("[")].chars().count();
        let b = screen.geometry.body;
        let at = Mouse::Click {
            x: b.x + u16::try_from(col).unwrap(),
            y: b.y + u16::try_from(y).unwrap(),
        };

        panel.confirm = Some(Confirm {
            question: "close it? y/N".into(),
            op: Op::Brief(Default::default()),
        });
        let handled = panel.mouse(at, &data, &ctx, SIZE);
        assert!(handled.out.is_empty() && handled.redraw);
        assert_eq!(panel.confirm, None, "answered no");

        panel.prompt = Some(Prompt {
            editor: Editor::new("group:", ""),
            ask: Ask::NewGroup,
        });
        let handled = panel.mouse(at, &data, &ctx, SIZE);
        assert!(handled.out.is_empty() && !handled.redraw);
        assert!(panel.prompt.is_some(), "the prompt stays open");

        panel.prompt = None;
        let handled = panel.mouse(at, &data, &ctx, SIZE);
        let pressed = Op::ConsoleCommand {
            line: "/brief".into(),
        };
        assert_eq!(handled.out, vec![Out::Send(pressed)]);
    }
}
