//! A panel's state machine: keys, clicks, pastes and replies in, ops out, and the screen to
//! draw. Keys act on the selected item, never on a row index (V8); nothing here runs a
//! program or touches a file.

use std::fmt::Write as _;
use std::path::PathBuf;

use moa_core::UtcMs;
use moa_core::op::{Done, Op};
use moa_core::panel::View;

use crate::ctx::Ctx;
use crate::data::{Data, SessionRow};
use crate::editor::{Edited, Editor};
use crate::geometry::{Geometry, Rect, Size, geometry};
use crate::keys::{self, Action, Key};
use crate::line::{Item, Line, Span, Tone, debug_lines};
use crate::list::ListState;
use crate::text::{clean, tilde};
use crate::views::tree;
use crate::views::{self, ViewUi};

/// What a panel asks its loop to do.
// One is built per key, click or reply, so boxing the op buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
    /// Send this op to the daemon; its reply comes back through [`Panel::replied`].
    Send(Op),
    Quit,
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
    /// The visible lines, each with its mark.
    pub body: Vec<(Line, Mark)>,
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

/// The screen as text, for snapshots: the body's lines as [`debug_lines`] prints them, each
/// after `> ` when selected, `~ ` when hovered and two spaces otherwise; then the footer.
pub fn debug_screen(screen: &Screen) -> String {
    let mut out = String::new();
    for (line, mark) in &screen.body {
        out.push_str(match mark {
            Mark::Plain => "  ",
            Mark::Hovered => "~ ",
            Mark::Selected => "> ",
        });
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

/// What a prompt's answer is for.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ask {
    /// `n`: the folder, then the title, then the account.
    Folder {
        group: Option<String>,
        account: String,
    },
    Title {
        group: Option<String>,
        path: PathBuf,
        account: String,
    },
    Account {
        group: Option<String>,
        path: PathBuf,
        title: Option<String>,
    },
    NewGroup,
    Move {
        q: String,
    },
    RenameSession {
        q: String,
    },
    RenameGroup {
        old: String,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Prompt {
    editor: Editor,
    ask: Ask,
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
    /// A reply or a note, until the next key.
    message: Option<String>,
    /// The item under the pointer, kept by identity like the selection (V11).
    hover: Option<Item>,
    /// Where the pointer was last seen.
    pointer: Option<(u16, u16)>,
}

const CANCELLED: &str = "cancelled";

/// The body width a view is built for: at least 10 cells (V2).
fn build_width(g: &Geometry) -> u16 {
    g.body.w.max(10)
}

/// The folder a typed answer names, checked as `n` checks it (V7): it starts with `~` or `/`,
/// and it is a folder. Which brain it belongs to is the core's to judge.
fn folder(typed: &str, ctx: &Ctx) -> Result<PathBuf, String> {
    let missing = || format!("no folder {typed}");
    let path = if typed.starts_with('/') {
        PathBuf::from(typed)
    } else if typed == "~" {
        ctx.home.clone()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        ctx.home.join(rest)
    } else if typed.starts_with('~') {
        // Another user's home: not one moa can name.
        return Err(missing());
    } else {
        return Err("a folder starts with ~ or /".to_string());
    };
    if (ctx.is_dir)(&path) {
        Ok(path)
    } else {
        Err(missing())
    }
}

/// The account prompt's label: the brain's accounts to choose from.
fn account_label(data: &Data) -> String {
    let accounts = data.brain.as_ref().map(|b| b.accounts.join(" / "));
    match accounts {
        Some(list) if !list.is_empty() => format!("account ({list}):"),
        _ => "account:".to_string(),
    }
}

/// The account a new session starts under: `want` when the brain lists it, else the brain's
/// own default, else its first account.
fn default_account(data: &Data, want: Option<&str>) -> String {
    let Some(brain) = &data.brain else {
        return want.unwrap_or_default().to_string();
    };
    want.filter(|w| brain.accounts.iter().any(|a| a == w))
        .or(brain.open_account.as_deref())
        .or(brain.accounts.first().map(String::as_str))
        .unwrap_or_default()
        .to_string()
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
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Shows another view, from its top, with nothing asked or said.
    pub fn set_view(&mut self, view: View) {
        if view != self.view {
            *self = Panel::new(view);
        }
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
        let g = geometry(self.view, size);
        let was = self.selected().cloned();
        self.lines = views::build(self.view, data, ctx, &self.ui, build_width(&g));
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

    /// A key press. It clears the hover and the last message; a prompt or a question takes
    /// it first, and Ctrl-c quits from anywhere.
    pub fn key(&mut self, k: Key, data: &Data, ctx: &Ctx, size: Size) -> Vec<Out> {
        let g = self.refresh(data, ctx, size);
        self.hover = None;
        self.message = None;
        if k == Key::Ctrl('c') {
            return vec![Out::Quit];
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
        // The picture's tab decides its rows' keys; it comes with the picture's own view.
        for action in keys::actions(self.view, None, k) {
            if let Some(out) = self.act(*action, data, ctx, body) {
                return out;
            }
        }
        Vec::new()
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

    /// Pasted text goes to the prompt being answered, if any.
    pub fn paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.editor.paste(text);
        }
    }

    /// An op's reply: the footer shows its text's last non-empty line, or nothing.
    pub fn replied(&mut self, result: Result<Done, String>) {
        let text = match result {
            Ok(done) => done.text,
            Err(e) => e,
        };
        self.message = text
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string);
    }

    /// A line for the footer from the panel's loop (the connection, say), until the next key.
    pub fn note(&mut self, text: &str) {
        self.message = Some(text.to_string());
    }

    /// The frame to draw now.
    pub fn screen(&mut self, data: &Data, ctx: &Ctx, size: Size) -> Screen {
        let g = self.refresh(data, ctx, size);
        let shown = self
            .lines
            .iter()
            .enumerate()
            .skip(self.list.top)
            .take(usize::from(g.body.h));
        let body: Vec<(Line, Mark)> = shown.map(|(i, l)| (l.clone(), self.mark(i))).collect();
        let animate = body.iter().any(|(l, _)| l.animated);
        let wake_at = body.iter().filter_map(|(l, _)| l.changes_at).min();
        Screen {
            title: g.framed.then(|| self.view.label().to_string()),
            tabs: None,
            brand: None,
            body,
            strip: Vec::new(),
            footer: self.footer(data, g.footer.w),
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
    fn footer(&self, data: &Data, width: u16) -> Span {
        if let Some(prompt) = &self.prompt {
            let shown = prompt.editor.shown(usize::from(width).saturating_sub(1));
            return Span::bold(shown, Tone::Text);
        }
        if let Some(confirm) = &self.confirm {
            return Span::new(clean(&confirm.question), Tone::Warn);
        }
        if let Some(message) = &self.message {
            return Span::new(clean(message), Tone::Warn);
        }
        let unreadable = data.unreadable();
        if unreadable > 0 {
            return Span::new(
                format!("{unreadable} rows from moa couldn't be shown"),
                Tone::Warn,
            );
        }
        let adopted = self.selected_session(data).is_some_and(|row| row.adopted);
        Span::new(keys::hint(self.view, adopted), Tone::Dim)
    }

    /// The folder the selected row is in: a session's, a project's, a task's session's.
    pub fn here(&self, data: &Data) -> Option<String> {
        match self.selected()? {
            Item::Session(id) => data.sessions.get(id)?.session.cwd.clone(),
            Item::Project(id) => data.projects.get(id)?.path.clone(),
            Item::Task(id) => {
                let session = data.tasks.get(id)?.session_id?;
                data.sessions.get(&session)?.session.cwd.clone()
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

    fn say(&mut self, text: &str) -> Vec<Out> {
        self.message = Some(text.to_string());
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

    /// The group the selection is in: a node's own path when it is a real group, or a
    /// session's group. A node made from a project's name, or `Loose`, is the top level: a new
    /// session there must not make it a permanent group (V5).
    fn selected_group(&self, data: &Data) -> Option<String> {
        match self.selected()? {
            Item::Group(path) if tree::is_group(data, path) => Some(path.clone()),
            Item::Session(id) => data
                .sessions
                .get(id)?
                .session
                .group_path
                .as_deref()
                .and_then(tree::tidy),
            _ => None,
        }
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
            Action::Quit => return Some(vec![Out::Quit]),
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
                    _ => None,
                };
            }
            Action::NewSession if self.view == View::Tree => {
                return Some(self.new_session(data, ctx));
            }
            Action::ChooseAccount => {
                let row = session?;
                return Some(self.choose_account(row, data, ctx));
            }
            Action::NewGroup => {
                let prefill = self
                    .selected_group(data)
                    .map_or(String::new(), |g| format!("{g}/"));
                return Some(self.ask("new group:".to_string(), prefill, Ask::NewGroup));
            }
            Action::Move => {
                let row = session?;
                let label = format!("move {} to group:", tree::name(row));
                let prefill = self.selected_group(data).unwrap_or_default();
                let q = row.session.id.to_string();
                return Some(self.ask(label, prefill, Ask::Move { q }));
            }
            Action::Rename => {
                if let Some(row) = session {
                    let prefill = row.session.title.clone().unwrap_or_default();
                    let q = row.session.id.to_string();
                    return Some(self.ask(
                        "rename session:".to_string(),
                        prefill,
                        Ask::RenameSession { q },
                    ));
                }
                let Item::Group(old) = selected? else {
                    return None;
                };
                let leaf = old.rsplit('/').next().unwrap_or_default().to_string();
                return Some(self.ask("rename group:".to_string(), leaf, Ask::RenameGroup { old }));
            }
            Action::DeleteGroup => {
                let Item::Group(path) = selected? else {
                    return None;
                };
                return Some(vec![Out::Send(Op::GroupRm { path })]);
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
            // The picture's tabs, the plan's tasks, suggestions and the other views' `n`
            // come with their views.
            Action::NewSession
            | Action::NextTab
            | Action::PrevTab
            | Action::Tab(_)
            | Action::Done
            | Action::Drop
            | Action::Yes
            | Action::No => return None,
        }
        // A move through the list: nothing to send.
        Some(Vec::new())
    }

    /// The tree's `n` (V7): asks the folder, prefilled from the newest session in the
    /// selected group or below it, else the selected session's, else the brain's first
    /// folder, else `~/`.
    fn new_session(&mut self, data: &Data, ctx: &Ctx) -> Vec<Out> {
        let group = self.selected_group(data);
        let from = match self.selected() {
            Some(Item::Group(path)) => tree::listed(data)
                .filter(|r| tree::is_under(&tree::node_of(r), path))
                .max_by_key(|r| (r.session.last_seen_at, r.session.id)),
            Some(Item::Session(id)) => data.sessions.get(id),
            _ => None,
        };
        // Field by field: a session without a folder still lends its account.
        let brain = data.brain.as_ref();
        let folder = from
            .and_then(|r| r.session.cwd.clone())
            .or_else(|| brain.and_then(|b| b.folders.first().cloned()));
        let account = from
            .and_then(|r| r.session.account.clone())
            .or_else(|| brain.and_then(|b| b.open_account.clone()));
        let folder = folder.map_or("~/".to_string(), |f| tilde(&f, &ctx.home));
        let label = format!(
            "new session in {} · folder:",
            group.as_deref().unwrap_or("top level")
        );
        let account = default_account(data, account.as_deref());
        self.ask(label, folder, Ask::Folder { group, account })
    }

    /// `N` on a session row (V21): a new session in its project's folder, asking the account
    /// first.
    fn choose_account(&mut self, row: &SessionRow, data: &Data, ctx: &Ctx) -> Vec<Out> {
        let Some(project) = row.session.project_id.and_then(|id| data.projects.get(&id)) else {
            return self.say("this row has no project");
        };
        let Some(path) = &project.path else {
            return self.say(&format!("no folder for {}", project.name));
        };
        let path = match folder(&tilde(path, &ctx.home), ctx) {
            Ok(path) => path,
            Err(e) => return self.say(&e),
        };
        let account = default_account(data, None);
        let ask = Ask::Account {
            group: None,
            path,
            title: None,
        };
        self.ask(account_label(data), account, ask)
    }

    /// A prompt's answer, trimmed: the next prompt, an op, or why neither.
    fn answered(&mut self, ask: Ask, text: String, data: &Data, ctx: &Ctx) -> Vec<Out> {
        let send = |op| vec![Out::Send(op)];
        match ask {
            Ask::Folder { group, account } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                match folder(&text, ctx) {
                    Ok(path) => self.ask(
                        "title:".to_string(),
                        String::new(),
                        Ask::Title {
                            group,
                            path,
                            account,
                        },
                    ),
                    Err(e) => self.say(&e),
                }
            }
            Ask::Title {
                group,
                path,
                account,
            } => {
                let title = (!text.is_empty()).then_some(text);
                let ask = Ask::Account { group, path, title };
                self.ask(account_label(data), account, ask)
            }
            Ask::Account { group, path, title } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                send(Op::SessionOpen {
                    path,
                    account: Some(text),
                    title,
                    group,
                    prompt: None,
                    bg: false,
                })
            }
            Ask::NewGroup => match tree::tidy(&text) {
                Some(path) => send(Op::GroupAdd { path }),
                None => self.say(CANCELLED),
            },
            Ask::Move { q } => match tree::tidy(&text) {
                Some(group) => send(Op::SessionSet {
                    q,
                    title: None,
                    group: Some(group),
                }),
                None => self.say(CANCELLED),
            },
            Ask::RenameSession { q } => {
                if text.is_empty() {
                    return self.say(CANCELLED);
                }
                send(Op::SessionSet {
                    q,
                    title: Some(text),
                    group: None,
                })
            }
            Ask::RenameGroup { old } => {
                let (parent, leaf) = old.rsplit_once('/').unwrap_or(("", &old));
                // Tidied as the core will, so `leaf/` is no rename either.
                let Some(text) = tree::tidy(&text).filter(|t| t != leaf) else {
                    return self.say(CANCELLED);
                };
                let new = if parent.is_empty() {
                    text
                } else {
                    format!("{parent}/{text}")
                };
                send(Op::GroupRename { old, new })
            }
        }
    }
}
