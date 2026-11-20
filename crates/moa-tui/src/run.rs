//! A panel in a terminal (`moa panel`): the terminal taken and given back, the connection to
//! the daemon (V13), and the one loop that wakes only for input, a push, the animation tick
//! while something visible moves, and the next moment a visible text changes (V10).
//!
//! The loop is one unbiased `select!` that handles one wake per turn, then everything the
//! connection already holds, then draws once. Nothing in it runs a program or polls.

use std::collections::BTreeSet;
use std::future::{Future, pending};
use std::io::{self, IsTerminal, Write};
use std::mem::ManuallyDrop;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::FutureExt;
use jiff::tz::TimeZone;
use moa_client::{Client, ClientOptions, Live, LiveMsg};
use moa_core::UtcMs;
use moa_core::op::{Done, Op};
use moa_core::panel::View;
use moa_core::publish::{CoreEvent, Topic};
use moa_core::tmux::ids::PaneId;
use moa_platform::{TmuxSocket, canonical};
use moa_proto::SnapshotMsg;
use moa_view::ctx::Ctx;
use moa_view::data::{Data, TOPICS};
use moa_view::geometry::Size;
use moa_view::keys::Key;
use moa_view::panel::{Mouse, Out, Panel, Screen};
use moa_view::text;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep, sleep_until};

use crate::draw::draw;
use crate::theme::{self, Theme};

mod input;
mod logbook;
mod signals;

use input::Input;
pub use logbook::log_line;
use logbook::{Logbook, panic_message};
use signals::Signals;

/// What a panel needs to run.
#[derive(Clone, Debug)]
pub struct PanelOptions {
    /// How to reach the brain's daemon. Its `autostart` is used for the first connect only;
    /// without one, a first connect that fails waits and tries again, as a reconnect does.
    pub client: ClientOptions,
    /// The view asked for; without one the daemon picks (V3).
    pub view: Option<View>,
    /// The pane this panel draws in, when it is one of this brain's (V14).
    pub pane: Option<PaneId>,
    pub home: PathBuf,
    pub tz: TimeZone,
    /// `panel.log`, where the panel writes a line per start, give-up, signal and panic
    /// (L14); `None` writes nothing.
    pub log: Option<PathBuf>,
    /// A popup's panel (`--once`, S8): it claims no pane, is never moa's, draws no frame,
    /// reads only what its view needs, and ends when it closes or when moa is down.
    pub once: bool,
}

/// Taking the terminal: the alternate screen, the cursor hidden, every mouse event (motion
/// included) in SGR form, focus reports and bracketed paste (V11).
pub const INIT: &str = "\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h";

/// Giving it back: each of [`INIT`]'s modes off in reverse, the cursor shown, and the screen
/// the panel took left.
pub const RESTORE: &str = "\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1003l\x1b[?25h\x1b[?1049l";

/// The footer while the daemon is another build (V13).
const MISMATCH: &str = "this panel is from another build of moa; close it and run moa panel again";
/// The footer while reconnecting.
const TRYING: &str = "moa isn't running; trying again…";
/// The footer once reconnecting has given up.
const GAVE_UP: &str = "moa isn't running: press any key to try again";
/// A panel moa placed gave up: a key can't start moa for it (L15), so it says to run moa.
const PLACED_GAVE_UP: &str = "moa isn't running: run moa, then press any key";
/// A popup found moa down: it tries no more, and any key closes it.
const ONCE_DOWN: &str = "moa isn't running: press any key to close";
/// The footer for an op sent while the first connect is under way.
const CONNECTING: &str = "not connected to moa yet; try again in a moment";

/// The animation tick (V10).
const TICK: Duration = Duration::from_millis(100);
/// The longest the loop sleeps for a deadline: one far off (up to `i64::MAX` ms) is
/// re-measured after this rather than overflowing the clock.
const LONGEST_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// The pane in `TMUX_PANE`, claimed only when `TMUX` (`<socket>,<pid>,<session>`) names
/// this brain's server: `socket`, resolved under `tmux_tmpdir` (else `/tmp`) and `uid`,
/// compared with `TMUX`'s by canonical path (V14). Anything unreadable claims nothing.
pub fn claim_pane(
    tmux: Option<&str>,
    tmux_pane: Option<&str>,
    socket: &TmuxSocket,
    tmux_tmpdir: Option<&Path>,
    uid: u32,
) -> Option<PaneId> {
    let pane: PaneId = tmux_pane?.parse().ok()?;
    // The socket path may hold commas; the pid and the session are the last two fields.
    let mut fields = tmux?.rsplitn(3, ',');
    let (session, pid, path) = (fields.next()?, fields.next()?, fields.next()?);
    let numbers = session.parse::<u64>().is_ok() && pid.parse::<u64>().is_ok();
    if !numbers || path.is_empty() {
        return None;
    }
    let theirs = canonical(Path::new(path));
    let ours = canonical(&socket.path(tmux_tmpdir, uid));
    (theirs == ours).then_some(pane)
}

/// What a key does while moa is down and the panel waits for a key to try again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DownKey {
    /// `q` in a picture moa doesn't manage.
    Quit,
    /// The panel takes the key, as usual: a prompt or a question is open, or it is Ctrl-c,
    /// which the panel decides on everywhere (L14).
    Panel,
    /// Any other key tries again.
    Retry,
}

/// How `panel` takes `key` while the connection is down.
pub fn down_key(panel: &Panel, key: Key) -> DownKey {
    // The loop hands Ctrl-c to the panel before asking this; the answer agrees with it.
    if key == Key::Ctrl('c') || panel.typing() {
        DownKey::Panel
    } else if panel.quits(key) {
        DownKey::Quit
    } else {
        DownKey::Retry
    }
}

/// The footer while the panel waits for a key to try again: why the first connect failed,
/// else that reconnecting gave up. A panel moa `placed` can't start moa, so it says to run
/// moa first.
pub fn down_line(why: Option<&str>, placed: bool) -> String {
    match (why, placed) {
        (Some(why), _) => format!("{why}; press any key to try again"),
        (None, false) => GAVE_UP.to_string(),
        (None, true) => PLACED_GAVE_UP.to_string(),
    }
}

/// What the panel subscribes to: every topic a view reads, or a popup's view's own (S8).
pub fn topics(view: View, once: bool) -> &'static [Topic] {
    match (once, view) {
        (true, View::Picker) => &[
            Topic::Sessions,
            Topic::Groups,
            Topic::Projects,
            Topic::Terms,
            Topic::Brain,
        ],
        (true, View::Help) => &[Topic::Brain, Topic::Layouts],
        _ => TOPICS,
    }
}

/// What every connect sends first (V13): which panel this is, whether moa `placed` it (L18),
/// then its selection's folder.
pub fn first_ops(
    pane: Option<PaneId>,
    view: Option<View>,
    here: Option<String>,
    placed: bool,
) -> Vec<Op> {
    vec![
        Op::UiPanel {
            pane: pane.map(|p| p.to_string()),
            view,
            placed,
            once: false,
        },
        Op::UiHere { folder: here },
    ]
}

/// What a popup (`--once`, S8) sends first: that it is one, showing `view`. It has no tab of
/// its own, so it claims none and has no folder to tell.
pub fn once_ops(view: View) -> Vec<Op> {
    vec![Op::UiPanel {
        pane: None,
        view: Some(view),
        placed: false,
        once: true,
    }]
}

/// What a reconnect sends first: the view `panel` shows now, the one moa last gave it
/// rather than the one it started with (L8), and `here`, its selection's folder.
pub fn reconnect_ops(
    pane: Option<PaneId>,
    panel: &Panel,
    here: Option<String>,
    placed: bool,
) -> Vec<Op> {
    first_ops(pane, Some(panel.view()), here, placed)
}

/// The waits between reconnects (V13): 250 ms doubling to 8 s, then 8 s, for at most a
/// minute in all.
pub fn reconnect_delays() -> Vec<Duration> {
    const ALL: Duration = Duration::from_secs(60);
    const MOST: Duration = Duration::from_secs(8);
    let mut delays = Vec::new();
    let mut next = Duration::from_millis(250);
    let mut sum = Duration::ZERO;
    while sum + next <= ALL {
        delays.push(next);
        sum += next;
        next = (next * 2).min(MOST);
    }
    delays
}

/// A key press as a panel reads it. A release, an Alt chord and the keys no panel uses are
/// `None`. Shift-Tab is `BackTab` however the terminal sends it.
pub fn key_of(ev: KeyEvent) -> Option<Key> {
    if ev.kind == KeyEventKind::Release || ev.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    Some(match ev.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab if shift => Key::BackTab,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    })
}

/// A mouse event as a panel reads it: a move or a drag is `Move`, a left press `Click`, the
/// wheel `Wheel*`; the rest (releases, other buttons) is `None`.
pub fn mouse_of(ev: MouseEvent) -> Option<Mouse> {
    let (x, y) = (ev.column, ev.row);
    Some(match ev.kind {
        MouseEventKind::Moved | MouseEventKind::Drag(_) => Mouse::Move { x, y },
        MouseEventKind::Down(MouseButton::Left) => Mouse::Click { x, y },
        MouseEventKind::ScrollUp => Mouse::WheelUp,
        MouseEventKind::ScrollDown => Mouse::WheelDown,
        _ => return None,
    })
}

/// Where the terminal's cursor stands: while a prompt is answered, in the cell after the
/// editor's text (which [`Screen`] leaves free); else where the screen puts it (after the
/// picker's query); else nowhere (hidden).
pub fn cursor(screen: &Screen, prompting: bool) -> Option<(u16, u16)> {
    let f = screen.geometry.footer;
    if !prompting {
        return screen.cursor;
    }
    if f.w == 0 || f.h == 0 {
        return None;
    }
    let used = u16::try_from(text::width(&screen.footer.text)).unwrap_or(u16::MAX);
    Some((f.x + used.min(f.w - 1), f.y))
}

/// Whether this process has the terminal: what [`restore`] gives back once.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// Gives the terminal back: [`RESTORE`] and raw mode off. Only the first call after taking
/// it does anything, so the panic hook and the guard's drop can both call it.
fn restore() {
    if TAKEN.swap(false, Ordering::SeqCst) {
        let mut out = io::stdout();
        let _ = out.write_all(RESTORE.as_bytes());
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The terminal, taken for as long as this lives: given back when it drops (a quit, Ctrl-c,
/// a signal, an error) and, through the panic hook, before a panic is logged and its
/// message printed. Any panic ends the panel, even one in a task the runtime would survive
/// (the connection's): a panel never goes on drawing into a terminal it gave back.
struct Taken;

impl Taken {
    fn take(log: Option<Logbook>) -> io::Result<Taken> {
        crossterm::terminal::enable_raw_mode()?;
        TAKEN.store(true, Ordering::SeqCst);
        // From here every way out gives the terminal back.
        let taken = Taken;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            if let Some(log) = &log {
                log.write(&format!("panicked: {}", panic_message(info)));
            }
            previous(info);
            std::process::exit(101);
        }));
        let mut out = io::stdout();
        out.write_all(INIT.as_bytes())?;
        out.flush()?;
        Ok(taken)
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        restore();
    }
}

/// A connect that worked.
struct Linked {
    live: Live,
    /// The connection's `panels` row key, from `ui.panel`'s reply.
    key: Option<String>,
    /// The view the daemon gave this panel.
    view: Option<View>,
    snapshot: SnapshotMsg,
    /// Why `ui.panel` was refused, if it was.
    refused: Option<String>,
}

/// A connect that didn't.
enum Failed {
    /// The daemon is another build: nothing was sent.
    Mismatch,
    /// Its line.
    Down(String),
}

type Connecting = Pin<Box<dyn Future<Output = Result<Linked, Failed>>>>;

/// Connects (V13): hello, then on a matching build the first ops, the topics and their
/// snapshot.
async fn connect(
    opts: ClientOptions,
    first: Vec<Op>,
    topics: &'static [Topic],
) -> Result<Linked, Failed> {
    let down = |e: moa_client::ClientError| Failed::Down(e.to_string());
    let client = Client::connect(opts).await.map_err(down)?;
    if !client.hello().matched {
        return Err(Failed::Mismatch);
    }
    let (live, replies, snapshot) = client.live(topics, first).await.map_err(down)?;
    let mut linked = Linked {
        live,
        key: None,
        view: None,
        snapshot,
        refused: None,
    };
    match replies.into_iter().next() {
        Some(Ok(done)) => {
            linked.key = done
                .data
                .get("key")
                .and_then(|k| k.as_str())
                .map(Into::into);
            linked.view = done
                .data
                .get("view")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok());
        }
        Some(Err(e)) => linked.refused = Some(e),
        None => {}
    }
    Ok(linked)
}

/// The connection's state.
enum Link {
    /// A connect under way; `waited` counts the reconnect delays already slept.
    Connecting {
        connect: Connecting,
        waited: usize,
        /// The folder its first ops say.
        here: Option<String>,
    },
    Up {
        live: Live,
        key: Option<String>,
    },
    /// Sleeping the `waited`th delay before the next try.
    Waiting {
        waited: usize,
        sleep: Pin<Box<Sleep>>,
    },
    /// Waiting for a key to try again: why the first connect failed, or `None` once
    /// reconnecting gave up.
    Down(Option<String>),
    /// Another build: the panel says so and sends nothing.
    Mismatch,
}

/// What woke the loop.
enum Wake {
    Input(Option<io::Result<Event>>),
    Msg(LiveMsg),
    Linked(Result<Linked, Failed>),
    Retry,
    Tick,
    Deadline,
    /// A signal that stops the panel, by name.
    Signal(&'static str),
}

/// What the connection has for the loop; never ready while it has nothing to say.
async fn link_wake(link: &mut Link) -> Wake {
    match link {
        Link::Up { live, .. } => Wake::Msg(live.recv().await),
        Link::Connecting { connect, .. } => Wake::Linked(connect.as_mut().await),
        Link::Waiting { sleep, .. } => {
            sleep.as_mut().await;
            Wake::Retry
        }
        Link::Down(_) | Link::Mismatch => pending().await,
    }
}

/// The next wake: input, the connection, the tick (only while it exists), the deadline
/// (only while there is one) and a signal, in no fixed order.
async fn next_wake(
    input: &mut Input,
    link: &mut Link,
    tick: Option<&mut Interval>,
    deadline: Option<&mut Pin<Box<Sleep>>>,
    signals: &mut Signals,
) -> Wake {
    let tick = async {
        match tick {
            Some(t) => {
                t.tick().await;
            }
            None => pending().await,
        }
    };
    let deadline = async {
        match deadline {
            Some(d) => d.as_mut().await,
            None => pending().await,
        }
    };
    tokio::select! {
        e = input.next() => Wake::Input(e),
        w = link_wake(link) => w,
        () = tick => Wake::Tick,
        () = deadline => Wake::Deadline,
        name = signals.recv() => Wake::Signal(name),
    }
}

/// The wall clock now.
fn now() -> UtcMs {
    UtcMs(jiff::Timestamp::now().as_millisecond())
}

/// How long until `at`, from `now`: never less than nothing, never more than a day.
fn until(at: UtcMs, now: UtcMs) -> Duration {
    let ms = u64::try_from(at.0.saturating_sub(now.0)).unwrap_or(0);
    Duration::from_millis(ms).min(LONGEST_WAIT)
}

/// What a turn asks of the loop.
enum Turn {
    Quit,
    /// Go on, redrawing first when true.
    Go(bool),
}

/// The panel, its data and its connection.
struct Loop {
    opts: PanelOptions,
    theme: Theme,
    panel: Panel,
    data: Data,
    link: Link,
    delays: Vec<Duration>,
    /// The tags of the ops the panel sent: their replies go to its footer. `ui.here`'s
    /// don't.
    asked: BTreeSet<u64>,
    /// The folder the daemon was last told (`ui.here`).
    told_here: Option<String>,
    /// A connect has worked: from now on connects don't start a daemon.
    linked_once: bool,
    /// The first connect has ended, either way.
    first_ended: bool,
    /// The picker's pick went through: the panel closes once this turn is done.
    closing: bool,
    /// `panel.log`, shared with the panic hook.
    log: Option<Logbook>,
    size: Size,
    started: std::time::Instant,
    /// Exists only while the screen animates.
    tick: Option<Interval>,
    /// The next moment a visible text changes.
    deadline: Option<Pin<Box<Sleep>>>,
}

impl Drop for Loop {
    /// A popup's connection outlives its loop, in whatever state the link is (up, or a
    /// connect under way that moa may have heard `ui.panel` on): its program exits with the
    /// connection still open (`moa panel --once`), so moa hears the popup go only once tmux
    /// can reap it, and the reap nudge moa sends then on tmux 3.4 and 3.5 comes after the
    /// exit (S8).
    fn drop(&mut self) {
        if self.opts.once {
            std::mem::forget(std::mem::replace(&mut self.link, Link::Down(None)));
        }
    }
}

impl Loop {
    fn new(opts: PanelOptions, theme: Theme, size: Size) -> Loop {
        let panel = Panel::new(opts.view.unwrap_or(View::Picture));
        let mut lp = Loop {
            opts,
            theme,
            panel,
            data: Data::default(),
            link: Link::Down(None),
            delays: reconnect_delays(),
            asked: BTreeSet::new(),
            told_here: None,
            linked_once: false,
            first_ended: false,
            closing: false,
            log: None,
            size,
            started: std::time::Instant::now(),
            tick: None,
            deadline: None,
        };
        // moa tags a panel it places as it starts it (L7), so a placed panel is moa's
        // before its row says so; a panel started by hand isn't, and a popup never is,
        // whatever `MOA_NO_AUTOSTART` says: it has no pane for moa to manage (S8).
        lp.panel.set_managed(lp.placed() && !lp.opts.once);
        lp.panel.set_popup(lp.opts.once);
        lp
    }

    fn ctx(&self) -> Ctx {
        let tick = u64::try_from(self.started.elapsed().as_millis() / TICK.as_millis())
            .unwrap_or(u64::MAX);
        Ctx::at(
            now(),
            self.opts.tz.clone(),
            tick,
            self.opts.home.clone(),
            Path::is_dir,
        )
    }

    /// Starts a connect, as the first ops say from the panel's view and folder now. Only
    /// the first connect may start the daemon.
    fn connect(&mut self, waited: usize) {
        let here = self.here();
        let (pane, placed) = (self.opts.pane, self.placed());
        let first = if self.opts.once {
            once_ops(self.panel.view())
        } else if self.linked_once {
            reconnect_ops(pane, &self.panel, here.clone(), placed)
        } else {
            first_ops(pane, self.opts.view, here.clone(), placed)
        };
        let mut client = self.opts.client.clone();
        if self.linked_once {
            client.autostart = None;
        }
        let topics = topics(self.panel.view(), self.opts.once);
        self.link = Link::Connecting {
            connect: Box::pin(connect(client, first, topics)),
            waited,
            here,
        };
    }

    /// The folder the selection is in, which `Ctrl-a $` opens a terminal in. A popup has no
    /// tab for that key to come from, so it has none to tell.
    fn here(&self) -> Option<String> {
        match self.opts.once {
            true => None,
            false => self.panel.here(&self.data),
        }
    }

    /// The footer while moa is down and the panel waits for a key: why the first connect
    /// failed, if it did. A popup's key closes it.
    fn down_note(&self, why: Option<&str>) -> String {
        match (self.opts.once, why) {
            (true, Some(why)) => format!("{why}; press any key to close"),
            (true, None) => ONCE_DOWN.to_string(),
            (false, _) => down_line(why, self.placed()),
        }
    }

    /// Writes `what` to `panel.log`, naming the view the panel shows now.
    fn log(&self, what: &str) {
        self.note_view();
        if let Some(log) = &self.log {
            log.write(what);
        }
    }

    /// The view the panel is known to show: with no view asked, none until the first
    /// connect ends.
    fn known_view(&self) -> Option<View> {
        (self.opts.view.is_some() || self.first_ended).then(|| self.panel.view())
    }

    /// Keeps the panic hook's view up to date.
    fn note_view(&self) {
        if let Some(log) = &self.log {
            log.shows(self.known_view());
        }
    }

    /// The footer's line for a connection that isn't up, if any.
    fn link_line(&self) -> Option<String> {
        match &self.link {
            Link::Up { .. } | Link::Connecting { .. } => None,
            Link::Waiting { .. } => Some(TRYING.to_string()),
            Link::Down(why) => Some(self.down_note(why.as_deref())),
            Link::Mismatch => Some(MISMATCH.to_string()),
        }
    }

    /// Clears the footer's message. It answers no pick, so it never closes the picker.
    fn hush(&mut self) {
        let _ = self.panel.replied(Ok(Done {
            text: String::new(),
            data: Default::default(),
        }));
    }

    /// Does what the panel asked. True to quit.
    fn act(&mut self, outs: Vec<Out>) -> bool {
        for out in outs {
            match out {
                Out::Quit => return true,
                // A picker you started quits, as Ctrl-c quits any panel you started; one moa
                // placed stays where it is.
                Out::Close if !self.panel.managed() => return true,
                Out::Close => {}
                Out::Send(op) => match &mut self.link {
                    Link::Up { live, .. } => {
                        self.asked.insert(live.send(op));
                    }
                    // Nothing is sent to another build; elsewhere the footer says why. A
                    // pick that wasn't sent waits for no reply.
                    _ => {
                        self.panel.forget_pick();
                        let line = self.link_line();
                        self.panel.note(line.as_deref().unwrap_or(CONNECTING));
                    }
                },
            }
        }
        false
    }

    /// Input, a push, a connect's end, a timer.
    fn turn(&mut self, wake: Wake) -> Turn {
        let redraw = match wake {
            // The terminal is gone: nothing more can be shown or read there.
            Wake::Input(None) => {
                self.log("stopped: the terminal closed");
                return Turn::Quit;
            }
            Wake::Input(Some(Err(e))) => {
                self.log(&format!("stopped: can't read the terminal: {e}"));
                return Turn::Quit;
            }
            Wake::Input(Some(Ok(event))) => {
                let turn = self.input(event);
                if matches!(turn, Turn::Quit) {
                    self.log("stopped: quit");
                }
                return turn;
            }
            Wake::Msg(msg) => self.take(msg),
            Wake::Linked(result) => self.linked(result),
            Wake::Retry => {
                if let Link::Waiting { waited, .. } = self.link {
                    self.connect(waited + 1);
                }
                false
            }
            Wake::Tick => true,
            Wake::Deadline => {
                self.deadline = None;
                true
            }
            Wake::Signal(name) => {
                self.log(&format!("stopped by {name}"));
                return Turn::Quit;
            }
        };
        Turn::Go(redraw)
    }

    fn input(&mut self, event: Event) -> Turn {
        let ctx = self.ctx();
        let redraw = match event {
            Event::Key(k) => {
                let Some(key) = key_of(k) else {
                    return Turn::Go(false);
                };
                // Ctrl-c is the panel's however the connection is (L14). A cancel gives the
                // footer back to the connection's line, as Esc does; the managed panel's
                // line answers the key, so it stays.
                if key == Key::Ctrl('c') {
                    let cancels = self.panel.typing();
                    let out = self.panel.interrupt();
                    if self.act(out.into_iter().collect()) {
                        return Turn::Quit;
                    }
                    if let Some(line) = self.link_line().filter(|_| cancels) {
                        self.panel.note(&line);
                    }
                    return Turn::Go(true);
                }
                // A popup tries moa no more once it is down: any key closes it.
                if self.opts.once && matches!(self.link, Link::Down(_) | Link::Mismatch) {
                    return Turn::Quit;
                }
                if matches!(self.link, Link::Down(_)) {
                    match down_key(&self.panel, key) {
                        DownKey::Quit => return Turn::Quit,
                        DownKey::Retry => {
                            self.hush();
                            self.connect(0);
                            return Turn::Go(true);
                        }
                        DownKey::Panel => {}
                    }
                }
                let outs = self.panel.key(key, &self.data, &ctx, self.size);
                if self.act(outs) {
                    return Turn::Quit;
                }
                // The footer's editor or question keeps the footer while it is open.
                if let Some(line) = self.link_line().filter(|_| !self.panel.typing()) {
                    self.panel.note(&line);
                }
                true
            }
            Event::Mouse(m) => {
                let Some(m) = mouse_of(m) else {
                    return Turn::Go(false);
                };
                let handled = self.panel.mouse(m, &self.data, &ctx, self.size);
                let redraw = handled.redraw || !handled.out.is_empty();
                if self.act(handled.out) {
                    return Turn::Quit;
                }
                // A click clears the footer; the connection's line stays.
                if let Some(line) = self.link_line().filter(|_| redraw) {
                    self.panel.note(&line);
                }
                redraw
            }
            Event::Paste(text) => {
                self.panel.paste(&text);
                true
            }
            Event::Resize(w, h) => {
                self.size = Size { w, h };
                true
            }
            Event::FocusLost => self.panel.blur(),
            // The redraw measures the deadline again (M7): it may have lapsed while the
            // panel was out of sight (a laptop that slept, say).
            Event::FocusGained => true,
        };
        Turn::Go(redraw)
    }

    /// A message from the connection. True when the screen may have changed.
    fn take(&mut self, msg: LiveMsg) -> bool {
        match msg {
            LiveMsg::Patch(patch) => {
                for (topic, keys) in patch.delete {
                    self.data.delete(topic, &keys);
                }
                for (topic, rows) in patch.upsert {
                    self.data.upsert(topic, rows);
                }
                self.follow_row();
            }
            LiveMsg::Snapshot(snapshot) => {
                for (topic, rows) in snapshot.topics {
                    self.data.replace(topic, rows);
                }
                self.follow_row();
            }
            LiveMsg::Event(CoreEvent::Error { message }) => self.panel.note(&message),
            // Every panel hears it: it shows a while, dim, and goes (its deadline wakes us).
            LiveMsg::Event(CoreEvent::Ui { text }) => self.panel.event(&text, now()),
            LiveMsg::Reply { tag, result } => {
                if !self.asked.remove(&tag) {
                    return false;
                }
                let out = self.panel.replied(result);
                self.closing |= self.act(out.into_iter().collect());
            }
            LiveMsg::Closed => {
                // No reply still owed will come: a pick waiting on one is forgotten.
                self.asked.clear();
                self.panel.forget_pick();
                self.wait(0);
            }
        }
        true
    }

    /// Whether moa placed this panel itself: it has no way to start moa (L15).
    fn placed(&self) -> bool {
        self.opts.client.autostart.is_none()
    }

    /// Sleeps the `waited`th delay before trying again, or gives up when there are no more.
    /// A popup doesn't wait: it is open only while you look at it, so it says moa is down
    /// at once, and a key closes it.
    fn wait(&mut self, waited: usize) {
        if self.opts.once {
            self.link = Link::Down(None);
            self.panel.note(ONCE_DOWN);
            return;
        }
        match self.delays.get(waited) {
            Some(delay) => {
                self.link = Link::Waiting {
                    waited,
                    sleep: Box::pin(sleep(*delay)),
                };
                self.panel.note(TRYING);
            }
            None => {
                self.link = Link::Down(None);
                self.panel.note(&down_line(None, self.placed()));
                self.log("gave up reaching moa");
            }
        }
    }

    /// The panel follows its own `panels` row: the view moa gives it, and whether moa
    /// manages it (L8).
    fn follow_row(&mut self) {
        let Link::Up { key: Some(key), .. } = &self.link else {
            return;
        };
        self.panel.follow(&self.data, key);
    }

    /// A connect ended.
    fn linked(&mut self, result: Result<Linked, Failed>) -> bool {
        let (waited, here) = match &mut self.link {
            Link::Connecting { waited, here, .. } => (*waited, here.take()),
            _ => (0, None),
        };
        self.first_ended = true;
        match result {
            Ok(linked) => {
                self.data = Data::default();
                for (topic, rows) in linked.snapshot.topics {
                    self.data.replace(topic, rows);
                }
                if let Some(view) = linked.view {
                    self.panel.set_view(view);
                }
                self.link = Link::Up {
                    live: linked.live,
                    key: linked.key,
                };
                self.linked_once = true;
                self.asked.clear();
                self.told_here = here;
                match linked.refused {
                    Some(why) => {
                        let _ = self.panel.replied(Err(why));
                    }
                    None => self.hush(),
                }
                self.follow_row();
            }
            Err(Failed::Mismatch) => {
                self.link = Link::Mismatch;
                self.panel.note(MISMATCH);
            }
            Err(Failed::Down(line))
                if !self.linked_once && self.opts.client.autostart.is_some() =>
            {
                // The first connect, which could start the daemon, says why it couldn't and
                // that a key tries again. One that couldn't (a panel moa placed) waits for the
                // daemon as a reconnect does.
                self.panel.note(&self.down_note(Some(&line)));
                self.link = Link::Down(Some(line));
            }
            Err(Failed::Down(_)) => self.wait(waited),
        }
        true
    }

    /// Takes everything the connection already holds. True when the screen may have
    /// changed.
    fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Link::Up { live, .. } = &mut self.link
            && let Some(msg) = live.recv().now_or_never()
        {
            changed |= self.take(msg);
        }
        changed
    }

    /// Tells the daemon the selection's folder when it changed (V3). A popup never does.
    fn tell_here(&mut self) {
        if self.opts.once {
            return;
        }
        let Link::Up { live, .. } = &mut self.link else {
            return;
        };
        let here = self.panel.here(&self.data);
        if here != self.told_here {
            // Its reply isn't the footer's.
            live.send(Op::UiHere {
                folder: here.clone(),
            });
            self.told_here = here;
        }
    }

    /// Draws the panel once, then sets the tick and the deadline from what it showed.
    fn draw<B: Backend>(&mut self, term: &mut Terminal<B>) -> Result<(), Fault>
    where
        B::Error: 'static,
    {
        // With no view asked, nothing is drawn until the first connect ends: moa may give
        // the panel another view than the picture it starts with, which would flash (L14).
        if self.known_view().is_none() {
            return Ok(());
        }
        let ctx = self.ctx();
        let mut shown = None;
        term.draw(|f| {
            let area = f.area();
            let size = Size {
                w: area.width,
                h: area.height,
            };
            let screen = self.panel.screen(&self.data, &ctx, size);
            draw(f.buffer_mut(), area, &screen, &self.theme);
            if let Some(at) = cursor(&screen, self.panel.prompting()) {
                f.set_cursor_position(at);
            }
            shown = Some((size, screen.animate, screen.wake_at));
        })
        .map_err(|e| {
            let io = (&e as &(dyn std::error::Error + 'static)).downcast_ref::<io::Error>();
            match io {
                Some(io) => Fault::terminal("can't draw the panel", io),
                None => Fault::Line(format!("can't draw the panel: {e}")),
            }
        })?;
        let Some((size, animate, wake_at)) = shown else {
            return Ok(());
        };
        self.size = size;
        match (animate, self.tick.is_some()) {
            (true, false) => {
                let mut tick = interval_at(Instant::now() + TICK, TICK);
                tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
                self.tick = Some(tick);
            }
            (false, true) => self.tick = None,
            _ => {}
        }
        // A deadline that stays is moved in place rather than built again each frame.
        match (wake_at, &mut self.deadline) {
            (None, _) => self.deadline = None,
            (Some(at), Some(d)) => d.as_mut().reset(Instant::now() + until(at, now())),
            (Some(at), None) => {
                self.deadline = Some(Box::pin(sleep_until(Instant::now() + until(at, now()))));
            }
        }
        Ok(())
    }

    /// One turn of the loop: `wake`, then everything the connection already holds, then one
    /// draw. True to quit.
    fn step<B: Backend>(&mut self, wake: Wake, term: &mut Terminal<B>) -> Result<bool, Fault>
    where
        B::Error: 'static,
    {
        let mut redraw = match self.turn(wake) {
            Turn::Quit => return Ok(true),
            Turn::Go(redraw) => redraw,
        };
        redraw |= self.drain();
        if self.closing {
            self.log("stopped: closed");
            return Ok(true);
        }
        if redraw {
            self.draw(term)?;
        }
        self.tell_here();
        self.note_view();
        Ok(false)
    }
}

/// Runs a panel until it quits: Ctrl-c or `q` in a panel you started, a signal, or the
/// terminal's end. The terminal is given back on every way out, and `panel.log` gets one
/// line when the panel starts and one when it ends, unless it panics (L14).
pub async fn run(opts: PanelOptions) -> Result<(), String> {
    // Asked first, before anything can close it: a terminal that hung up no longer counts
    // as one.
    let stdout = Stdout {
        terminal: io::stdout().is_terminal(),
    };
    let log = Logbook::new(&opts);
    if let Some(log) = &log {
        log.write("started");
    }
    let ended = run_logged(opts, log.clone(), stdout).await;
    if let (Err(e), Some(log)) = (&ended, &log) {
        log.write(&format!("stopped: {e}"));
    }
    ended
}

/// Why a run stopped before the panel quit.
#[derive(Debug)]
enum Fault {
    /// The terminal went: a write into it, or a question to it, failed as only a gone
    /// terminal fails.
    Gone,
    /// Anything else, in its line.
    Line(String),
}

impl Fault {
    /// `e`, met using the terminal that has worked (it took a write) while doing `what`.
    fn terminal(what: &str, e: &io::Error) -> Fault {
        if moa_platform::terminal_gone_error(e) {
            Fault::Gone
        } else {
            Fault::Line(format!("{what}: {e}"))
        }
    }
}

/// Where the panel draws: stdout.
#[derive(Clone, Copy)]
struct Stdout {
    /// It was a terminal when the panel started.
    terminal: bool,
}

impl Stdout {
    /// Whether it was a terminal and has hung up since.
    fn went(self) -> bool {
        self.terminal && moa_platform::terminal_gone(io::stdout().as_fd())
    }

    /// `e`, met while taking or asking the terminal before it has taken a write: it may
    /// not be a terminal at all (no controlling terminal gives ENXIO, as a gone one does),
    /// so only a terminal seen to hang up makes it `Gone`; anything else refuses.
    fn refused(self, e: &io::Error) -> Fault {
        if self.went() {
            Fault::Gone
        } else {
            Fault::Line(format!("can't use this terminal: {e}"))
        }
    }

    /// `e`, met using the terminal once it has taken a write.
    fn failed(self, what: &str, e: &io::Error) -> Fault {
        if self.went() {
            Fault::Gone
        } else {
            Fault::terminal(what, e)
        }
    }
}

/// [`run`], which logs how it ended when it ends with an error. A run that fails while a
/// signal came, or while its terminal went, ends for that reason, however it found out:
/// a failed draw (the first one included), a write, or a question to the terminal.
async fn run_logged(
    opts: PanelOptions,
    log: Option<Logbook>,
    stdout: Stdout,
) -> Result<(), String> {
    let mut signals = Signals::new().map_err(|e| format!("can't watch for signals: {e}"))?;
    let mut input = None;
    let fault = match drive(opts, log.clone(), stdout, &mut input, &mut signals).await {
        Ok(()) => return Ok(()),
        Err(fault) => fault,
    };
    let end = match (signals.pending().await, fault) {
        (Some(name), _) => format!("stopped by {name}"),
        (None, Fault::Gone) => "stopped: the terminal closed".to_string(),
        (None, Fault::Line(_)) if stdout.went() || input.as_ref().is_some_and(Input::gone) => {
            "stopped: the terminal closed".to_string()
        }
        (None, Fault::Line(line)) => return Err(line),
    };
    if let Some(log) = &log {
        log.write(&end);
    }
    Ok(())
}

/// Takes the terminal and runs the loop until the panel quits or something fails. The
/// terminal's input, once started, is left in `input`, for the caller to ask whether the
/// terminal went.
async fn drive(
    opts: PanelOptions,
    log: Option<Logbook>,
    stdout: Stdout,
    input: &mut Option<Input>,
    signals: &mut Signals,
) -> Result<(), Fault> {
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let colorterm = std::env::var("COLORTERM").ok();
    let theme = Theme {
        mode: theme::detect(in_tmux, colorterm.as_deref()),
    };
    // Taking it is its first write.
    let _taken = Taken::take(log.clone()).map_err(|e| stdout.refused(&e))?;
    let unusable = |e: io::Error| stdout.failed("can't use this terminal", &e);
    // Never dropped: ratatui's `Terminal` shows the cursor again as it drops, and prints a
    // failure with `eprintln!`, which panics on a terminal that is gone (tmux killed the
    // tab). `restore` shows the cursor anyway.
    let mut term =
        ManuallyDrop::new(Terminal::new(CrosstermBackend::new(io::stdout())).map_err(unusable)?);
    // Not `Terminal::clear`, which asks the terminal where its cursor is.
    term.backend_mut()
        .clear_region(ClearType::All)
        .map_err(unusable)?;
    // crossterm asks `tput` when the terminal won't say its size, so a gone terminal can
    // fail here with any error.
    let area = term.size().map_err(unusable)?;
    let size = Size {
        w: area.width,
        h: area.height,
    };
    let input = input.insert(
        Input::start(log.clone())
            .map_err(|e| Fault::Line(format!("can't use this terminal: {e}")))?,
    );
    let mut lp = Loop::new(opts, theme, size);
    lp.log = log;
    lp.connect(0);
    lp.draw(&mut *term)?;
    loop {
        let mut wake = next_wake(
            input,
            &mut lp.link,
            lp.tick.as_mut(),
            lp.deadline.as_mut(),
            signals,
        )
        .await;
        // When tmux kills the tab, the terminal closes and its SIGHUP comes at the same
        // moment: the signal, if it came, says why the panel ends.
        if matches!(wake, Wake::Input(None | Some(Err(_))))
            && let Some(name) = signals.pending().await
        {
            wake = Wake::Signal(name);
        }
        // A failed draw goes to `run_logged`, which tells a gone terminal from a failure.
        if lp.step(wake, &mut *term)? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use moa_client::{ClientOptions, OnMismatch};
    use moa_core::config::BrainName;
    use moa_core::panel::View;
    use moa_core::state::CallerInfo;
    use moa_platform::Paths;
    use moa_view::geometry::Size;
    use moa_view::keys::Key;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use moa_client::LiveMsg;
    use moa_core::op::Done;
    use moa_view::panel::Out;

    use super::{
        Failed, Fault, Link, Logbook, Loop, MISMATCH, ONCE_DOWN, PanelOptions, TRYING, Turn, Wake,
        now,
    };
    use crate::theme::{ColorMode, Theme};

    const SIZE: Size = Size { w: 60, h: 12 };

    /// A panel's loop that asked for `view`; `placed` when moa placed it (no autostart).
    /// Nothing here connects: the tests hand the loop what a connect would.
    fn looped(view: Option<View>, placed: bool) -> Loop {
        let paths =
            Paths::from_vars(Some("/nonexistent/moa".into()), None, None, 501, false).unwrap();
        let client = ClientOptions {
            paths,
            brain: BrainName::default_brain(),
            client: "panel",
            caller: CallerInfo::default(),
            autostart: (!placed).then(|| "/nonexistent/moa/bin/moa".into()),
            on_mismatch: OnMismatch::Accept,
            build: "test".into(),
        };
        let opts = PanelOptions {
            client,
            view,
            pane: None,
            home: "/nonexistent".into(),
            tz: jiff::tz::TimeZone::UTC,
            log: None,
            once: false,
        };
        let theme = Theme {
            mode: ColorMode::True,
        };
        Loop::new(opts, theme, SIZE)
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(SIZE.w, SIZE.h)).unwrap()
    }

    /// What the terminal shows, row by row.
    fn shown(term: &Terminal<TestBackend>) -> String {
        let buffer = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn input(lp: &mut Loop, event: Event) -> Turn {
        lp.turn(Wake::Input(Some(Ok(event))))
    }

    fn ctrl_c() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    }

    /// A terminal whose every write fails with `error`.
    struct Failing(fn() -> std::io::Error);

    impl std::io::Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err((self.0)())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err((self.0)())
        }
    }

    /// L14: a draw that fails as only a gone terminal fails (EIO, ENXIO, EPIPE) is the
    /// terminal gone, whichever draw it is: the run then ends with `stopped: the terminal
    /// closed`. Any other failure is reported as it is.
    #[tokio::test]
    async fn a_draw_into_a_gone_terminal_is_the_terminal_gone() {
        use ratatui::backend::CrosstermBackend;
        use ratatui::layout::Rect;
        use ratatui::{TerminalOptions, Viewport};

        let errors: [(fn() -> std::io::Error, bool); 4] = [
            (|| std::io::Error::from_raw_os_error(5), true),
            (|| std::io::Error::from_raw_os_error(6), true),
            (|| std::io::Error::from_raw_os_error(32), true),
            (|| std::io::Error::other("disk full"), false),
        ];
        for (error, gone) in errors {
            let mut lp = looped(Some(View::Tree), false);
            let viewport = Viewport::Fixed(Rect::new(0, 0, SIZE.w, SIZE.h));
            let mut term = Terminal::with_options(
                CrosstermBackend::new(Failing(error)),
                TerminalOptions { viewport },
            )
            .unwrap();
            match lp.draw(&mut term) {
                Err(Fault::Gone) => assert!(gone, "{}", error()),
                Err(Fault::Line(line)) => {
                    assert!(!gone, "{line}");
                    assert_eq!(line, "can't draw the panel: disk full");
                }
                Ok(()) => panic!("a failed write drew"),
            }
        }
    }

    /// M7: focus coming back redraws, and the draw sets the deadline again from what shows,
    /// so a deadline that lapsed while the panel was out of sight (a sleeping laptop) is
    /// measured again.
    #[tokio::test]
    async fn focus_gained_redraws() {
        let mut lp = looped(Some(View::Tree), false);
        let mut term = terminal();
        lp.panel.event("moa restarted", now());
        lp.draw(&mut term).unwrap();
        assert!(lp.deadline.is_some(), "the event's end is a deadline");

        lp.deadline = None;
        assert!(matches!(input(&mut lp, Event::FocusGained), Turn::Go(true)));
        lp.draw(&mut term).unwrap();
        assert!(lp.deadline.is_some(), "the redraw re-armed it");
    }

    /// L14: a panel with no view asked shows nothing until its first connect ends: moa may
    /// give it another view than the picture it starts with. One that asked draws at once.
    #[tokio::test]
    async fn nothing_is_drawn_before_the_first_link_when_no_view_was_asked() {
        let mut lp = looped(None, false);
        let mut term = terminal();
        lp.draw(&mut term).unwrap();
        assert!(shown(&term).trim().is_empty(), "{}", shown(&term));
        assert!(matches!(
            input(&mut lp, Event::Resize(SIZE.w, SIZE.h)),
            Turn::Go(true)
        ));
        lp.draw(&mut term).unwrap();
        assert!(shown(&term).trim().is_empty(), "{}", shown(&term));

        // The first connect failed: the panel says so, in the view it has.
        lp.linked(Err(Failed::Down("moa couldn't start: no such file".into())));
        lp.draw(&mut term).unwrap();
        assert!(
            shown(&term).contains("moa couldn't start"),
            "{}",
            shown(&term)
        );

        let mut asked = looped(Some(View::Tree), false);
        let mut term = terminal();
        asked.draw(&mut term).unwrap();
        assert!(shown(&term).contains("sessions"), "{}", shown(&term));
    }

    /// L14: moa tags every panel it places as it starts it (L7), so a placed panel is
    /// managed before any row says so, and Ctrl-c leaves it in the layout. One started by
    /// hand quits.
    #[tokio::test]
    async fn a_placed_panel_is_managed_from_the_start() {
        let mut placed = looped(Some(View::Tree), true);
        assert!(placed.panel.managed());
        assert!(matches!(input(&mut placed, ctrl_c()), Turn::Go(true)));
        let mut term = terminal();
        placed.draw(&mut term).unwrap();
        assert!(
            shown(&term).contains("moa's layout · Ctrl-a r repairs"),
            "{}",
            shown(&term)
        );

        let mut by_hand = looped(Some(View::Tree), false);
        assert!(!by_hand.panel.managed());
        assert!(matches!(input(&mut by_hand, ctrl_c()), Turn::Quit));
    }

    /// A pick made while the link isn't up is dropped, and forgotten: the footer keeps the
    /// link's line, and the next Enter offers the pick again (sent once the link is back)
    /// rather than waiting on a reply that will never come.
    #[tokio::test]
    async fn a_pick_that_couldnt_be_sent_is_forgotten() {
        let (data, ctx) = moa_view::fixture::lantern();
        let enter = || Event::Key(KeyEvent::from(KeyCode::Enter));
        let links = [
            (
                Link::Waiting {
                    waited: 0,
                    sleep: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(60))),
                },
                TRYING,
            ),
            (Link::Mismatch, MISMATCH),
        ];
        for (link, line) in links {
            let mut lp = looped(Some(View::Picker), false);
            lp.data = data.clone();
            lp.link = link;
            for _ in 0..2 {
                assert!(matches!(input(&mut lp, enter()), Turn::Go(true)));
                assert!(lp.asked.is_empty(), "nothing was sent");
                let screen = lp.panel.screen(&lp.data, &ctx, SIZE);
                let fits = moa_view::text::ellipsize(line, usize::from(screen.geometry.footer.w));
                assert_eq!(screen.footer.text, fits);
            }
            // What Enter gives the loop now: the pick itself, which a link that is up sends.
            let outs = lp.panel.key(Key::Enter, &lp.data, &ctx, SIZE);
            assert!(matches!(outs[..], [Out::Send(_)]), "{outs:?}");
        }
    }

    /// S7: Esc, Ctrl-c and Ctrl-g close a picker you started; one moa placed stays where it
    /// is. A pick closes the picker once its reply says it went through, with an end line in
    /// `panel.log`; a refused one leaves it open.
    #[tokio::test]
    async fn the_picker_closes_itself() {
        let esc = Event::Key(KeyEvent::from(KeyCode::Esc));
        let ctrl_g = Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        for key in [esc, ctrl_c(), ctrl_g] {
            let mut by_hand = looped(Some(View::Picker), false);
            assert!(matches!(input(&mut by_hand, key), Turn::Quit));
        }
        let mut placed = looped(Some(View::Picker), true);
        assert!(matches!(input(&mut placed, ctrl_c()), Turn::Go(true)));
        assert_eq!(placed.panel.view(), View::Picker);

        let dir = tempfile::tempdir().unwrap();
        let (data, ctx) = moa_view::fixture::lantern();
        let mut lp = looped(Some(View::Picker), false);
        lp.log = Logbook::new(&PanelOptions {
            log: Some(dir.path().join("panel.log")),
            ..lp.opts.clone()
        });
        lp.data = data;
        let reply = |tag, result| LiveMsg::Reply { tag, result };
        let outs = lp.panel.key(Key::Enter, &lp.data, &ctx, SIZE);
        assert!(matches!(outs[..], [Out::Send(_)]), "{outs:?}");
        lp.asked.insert(1);
        lp.take(reply(1, Err("its folder ~/x is gone".to_string())));
        assert!(!lp.closing);
        let mut term = terminal();
        assert!(!lp.step(Wake::Tick, &mut term).unwrap());
        assert!(shown(&term).contains("its folder ~/x is gone"));

        lp.panel.key(Key::Enter, &lp.data, &ctx, SIZE);
        lp.asked.insert(2);
        let done = Done {
            text: "went to Fix login".to_string(),
            data: Default::default(),
        };
        assert!(lp.take(reply(2, Ok(done))));
        assert!(lp.closing);
        assert!(lp.step(Wake::Tick, &mut term).unwrap());
        let written = std::fs::read_to_string(dir.path().join("panel.log")).unwrap();
        assert!(
            written.lines().last().unwrap().ends_with("stopped: closed"),
            "{written}"
        );
    }

    /// A popup's loop (`--once`), as `looped` makes one otherwise.
    fn popup(placed: bool) -> Loop {
        let mut lp = looped(Some(View::Picker), placed);
        lp.opts.once = true;
        let opts = lp.opts.clone();
        let mut lp = Loop::new(opts, lp.theme, SIZE);
        lp.data = moa_view::fixture::lantern().0;
        lp
    }

    /// S8: a popup is never moa's, whatever `MOA_NO_AUTOSTART` says, so its Ctrl-c and Esc
    /// close it, and it draws no frame of its own (tmux frames the popup).
    #[tokio::test]
    async fn a_popup_is_never_managed_and_draws_no_frame() {
        let esc = Event::Key(KeyEvent::from(KeyCode::Esc));
        for placed in [true, false] {
            let mut lp = popup(placed);
            assert!(!lp.panel.managed(), "placed: {placed}");
            let mut term = terminal();
            lp.draw(&mut term).unwrap();
            assert!(!shown(&term).contains('╭'), "{}", shown(&term));
            assert!(matches!(input(&mut lp, ctrl_c()), Turn::Quit));
            assert!(matches!(input(&mut popup(placed), esc.clone()), Turn::Quit));
        }
    }

    /// S8: a popup that finds moa down says so at once rather than trying again, and any
    /// key closes it; so does one that met another build. It never tells moa a folder.
    #[tokio::test]
    async fn a_popup_with_moa_down_closes_on_any_key() {
        let key = || Event::Key(KeyEvent::from(KeyCode::Char('k')));
        let mut lp = popup(true);
        lp.linked(Err(Failed::Down("moa isn't running".to_string())));
        assert!(matches!(lp.link, Link::Down(None)), "it doesn't try again");
        assert_eq!(lp.link_line().as_deref(), Some(ONCE_DOWN));
        let mut term = terminal();
        lp.draw(&mut term).unwrap();
        assert!(
            shown(&term).contains("moa isn't running"),
            "{}",
            shown(&term)
        );
        assert!(matches!(input(&mut lp, key()), Turn::Quit));

        // One that could start moa and couldn't says why.
        let mut lp = popup(false);
        lp.linked(Err(Failed::Down("can't start moa".to_string())));
        assert_eq!(
            lp.link_line().as_deref(),
            Some("can't start moa; press any key to close")
        );
        assert!(matches!(input(&mut lp, key()), Turn::Quit));

        let mut lp = popup(true);
        lp.link = Link::Mismatch;
        assert!(matches!(input(&mut lp, key()), Turn::Quit));
        assert_eq!(lp.here(), None);
    }

    /// What a scripted daemon tells its test: a `subscribe` came, and the connection closed.
    struct Heard {
        subscribed: tokio::sync::oneshot::Receiver<()>,
        closed: tokio::sync::oneshot::Receiver<()>,
    }

    /// The daemon's side of one connection, in a fresh `/tmp` folder: it says hello and
    /// answers every request, the `subscribe` too unless `hold`, which leaves a connect
    /// waiting on it; it says when the `subscribe` comes and when the connection closes.
    fn scripted_daemon(hold: bool) -> (tempfile::TempDir, ClientOptions, Heard) {
        use moa_proto::{Call, HelloOk, Incoming, SnapshotMsg, decode_incoming, encode_result};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = Paths::with_root(dir.path());
        moa_platform::ensure_private_dir(&paths.run_dir()).unwrap();
        let listener = tokio::net::UnixListener::bind(paths.socket("default").unwrap()).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (subscribed_tx, subscribed) = tokio::sync::oneshot::channel();
        let mut subscribed_tx = Some(subscribed_tx);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(Incoming::Request { id, call }) = decode_incoming(&line) else {
                    continue;
                };
                let answer = match call {
                    Call::Hello(_) => encode_result(
                        &id,
                        &HelloOk {
                            brain: "default".into(),
                            build: "b".into(),
                            rev: 0,
                            matched: true,
                            pid: std::process::id(),
                        },
                    ),
                    Call::Subscribe(_) => {
                        if let Some(tx) = subscribed_tx.take() {
                            let _ = tx.send(());
                        }
                        if hold {
                            continue;
                        }
                        encode_result(
                            &id,
                            &SnapshotMsg {
                                rev: 0,
                                topics: Default::default(),
                            },
                        )
                    }
                    _ => encode_result(
                        &id,
                        &Done {
                            text: String::new(),
                            data: Default::default(),
                        },
                    ),
                };
                write
                    .write_all(format!("{answer}\n").as_bytes())
                    .await
                    .unwrap();
            }
            let _ = closed_tx.send(());
        });
        let client = ClientOptions {
            paths,
            brain: BrainName::default_brain(),
            client: "panel",
            caller: CallerInfo::default(),
            autostart: None,
            on_mismatch: OnMismatch::Accept,
            build: "b".into(),
        };
        (dir, client, Heard { subscribed, closed })
    }

    /// Whether `closed` says the connection closed within 50 turns of the runtime: time
    /// enough for a connection's task to see its loop go and close it, as the tests' other
    /// legs show.
    async fn closes_soon(closed: &mut tokio::sync::oneshot::Receiver<()>) -> bool {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            if closed.try_recv().is_ok() {
                return true;
            }
        }
        false
    }

    /// S8: a popup's loop leaves its connection open when it goes, so the program exits
    /// with it still open (`moa panel --once`) and moa hears the popup go only with that
    /// exit, never before it. Any other panel's connection closes with its loop.
    #[tokio::test]
    async fn a_popups_connection_outlives_its_loop() {
        for once in [false, true] {
            let (_dir, client, mut heard) = scripted_daemon(false);
            let mut lp = looped(Some(View::Picker), true);
            lp.opts.once = once;
            let topics = super::topics(View::Picker, once);
            let Ok(linked) = super::connect(client, super::once_ops(View::Picker), topics).await
            else {
                panic!("the connect failed");
            };
            lp.link = Link::Up {
                live: linked.live,
                key: linked.key,
            };
            drop(lp);
            assert_eq!(closes_soon(&mut heard.closed).await, !once, "once: {once}");
        }
    }

    /// The same for a popup that goes while it is still connecting (Esc as it opens), once
    /// moa has heard which panel it is: the connect it was making stays open too.
    #[tokio::test]
    async fn a_popups_connect_outlives_its_loop() {
        for once in [false, true] {
            let (_dir, client, mut heard) = scripted_daemon(true);
            let mut lp = looped(Some(View::Picker), true);
            lp.opts.once = once;
            let topics = super::topics(View::Picker, once);
            let mut connect: super::Connecting = Box::pin(super::connect(
                client,
                super::once_ops(View::Picker),
                topics,
            ));
            // `ui.panel` is answered, and the connect waits on its `subscribe`.
            tokio::select! {
                _ = connect.as_mut() => panic!("the connect ended"),
                asked = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    &mut heard.subscribed,
                ) => assert!(asked.is_ok(), "no subscribe came"),
            }
            lp.link = Link::Connecting {
                connect,
                waited: 0,
                here: None,
            };
            drop(lp);
            assert_eq!(closes_soon(&mut heard.closed).await, !once, "once: {once}");
        }
    }

    /// Ctrl-c cancelling a prompt while moa is down gives the footer back to the down line,
    /// as Esc does: the line that says how to bring moa back isn't lost to `cancelled`.
    #[tokio::test]
    async fn a_cancel_while_moa_is_down_keeps_the_down_line() {
        let (data, ctx) = moa_view::fixture::lantern();
        let down = "moa isn't running: press any key to try again";
        for cancel in [ctrl_c(), Event::Key(KeyEvent::from(KeyCode::Esc))] {
            let mut lp = looped(Some(View::Tree), false);
            lp.data = data.clone();
            lp.panel.note(down);
            lp.panel.key(Key::Char('+'), &lp.data, &ctx, SIZE);
            assert!(lp.panel.typing(), "a prompt is open");
            assert!(matches!(input(&mut lp, cancel), Turn::Go(true)));
            assert!(!lp.panel.typing());
            let footer = lp.panel.screen(&lp.data, &ctx, SIZE).footer.text;
            assert_eq!(footer, down);
        }
    }

    /// L14: a terminal that closes, or can't be read, ends the panel cleanly with one plain
    /// line in `panel.log`, never an error that would be printed to the gone terminal. A
    /// quit from the keyboard gets its end line too.
    #[tokio::test]
    async fn a_terminal_that_goes_ends_the_panel_with_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.log");
        let ends: [(Wake, &str); 3] = [
            (Wake::Input(Some(Ok(ctrl_c()))), "stopped: quit"),
            (Wake::Input(None), "stopped: the terminal closed"),
            (
                Wake::Input(Some(Err(std::io::Error::other("gone")))),
                "stopped: can't read the terminal: gone",
            ),
        ];
        for (wake, line) in ends {
            let mut lp = looped(Some(View::Tree), false);
            lp.log = Logbook::new(&PanelOptions {
                log: Some(path.clone()),
                ..lp.opts.clone()
            });
            assert!(matches!(lp.turn(wake), Turn::Quit));
            let written = std::fs::read_to_string(&path).unwrap();
            assert!(
                written
                    .lines()
                    .last()
                    .unwrap()
                    .ends_with(&format!("[default] panel tree in -: {line}")),
                "{written}"
            );
        }
    }
}
