//! A panel in a terminal (`moa panel`): the terminal taken and given back, the connection to
//! the daemon (V13), and the one loop that wakes only for input, a push, the animation tick
//! while something visible moves, and the next moment a visible text changes (V10).
//!
//! The loop is one unbiased `select!` that handles one wake per turn, then everything the
//! connection already holds, then draws once. Nothing in it runs a program or polls.

use std::collections::BTreeSet;
use std::future::{Future, pending};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures_util::{FutureExt, StreamExt};
use jiff::tz::TimeZone;
use moa_client::{Client, ClientOptions, Live, LiveMsg};
use moa_core::UtcMs;
use moa_core::op::{Done, Op};
use moa_core::panel::View;
use moa_core::publish::CoreEvent;
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
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep, sleep_until};

use crate::draw::draw;
use crate::theme::{self, Theme};

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
}

/// A `panel.log` line (L14), without its time: the brain, the view (`-` while none is
/// known), the pane (`-` outside the brain's terminal), then what happened, on one line.
pub fn log_line(brain: &str, view: Option<View>, pane: Option<PaneId>, what: &str) -> String {
    let view = view.map_or_else(|| "-".to_string(), |v| v.to_string());
    let pane = pane.map_or_else(|| "-".to_string(), |p| p.to_string());
    let what = what.replace(['\n', '\r'], " ");
    format!("[{brain}] panel {view} in {pane}: {what}")
}

/// Where a panel writes its lines, and what they name. Every write is best-effort: a log
/// that can't be written never stops the panel, and never panics (the panic hook writes
/// too).
#[derive(Clone, Debug)]
struct Logbook {
    path: PathBuf,
    brain: String,
    pane: Option<PaneId>,
    /// The view the panel shows, once one is known; shared with the panic hook.
    view: Arc<Mutex<Option<View>>>,
}

impl Logbook {
    /// The panel's logbook, when `opts` names a log.
    fn new(opts: &PanelOptions) -> Option<Logbook> {
        Some(Logbook {
            path: opts.log.clone()?,
            brain: opts.client.brain.as_str().to_string(),
            pane: opts.pane,
            view: Arc::new(Mutex::new(opts.view)),
        })
    }

    /// Appends `what` as a line, after the time. The folder isn't made: one that is gone
    /// stays gone (L15).
    fn write(&self, what: &str) {
        // `try_lock`: the panic may have struck while the view was being set.
        let view = self.view.try_lock().ok().and_then(|v| *v);
        let line = log_line(&self.brain, view, self.pane, what);
        // No `Timestamp::now`, which panics on a clock out of its range.
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map_or_else(|| "-".to_string(), |t| t.to_string());
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path);
        if let Ok(mut file) = file {
            let _ = file.write_all(format!("{at} {line}\n").as_bytes());
        }
    }

    /// The view the next lines name.
    fn shows(&self, view: Option<View>) {
        if let Ok(mut shown) = self.view.lock() {
            *shown = view;
        }
    }
}

/// A panic's message, or a stand-in when it carries none.
fn panic_message<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

/// The signals that stop a panel (L14): `kill`'s SIGTERM, the SIGHUP tmux sends a tab it
/// kills, and SIGINT. Each ends the loop as Ctrl-c does, so the terminal is given back.
struct Signals {
    term: Signal,
    hup: Signal,
    int: Signal,
}

impl Signals {
    fn new() -> io::Result<Signals> {
        Ok(Signals {
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// The next signal's name.
    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            Some(()) = self.term.recv() => "SIGTERM",
            Some(()) = self.hup.recv() => "SIGHUP",
            Some(()) = self.int.recv() => "SIGINT",
            else => pending().await,
        }
    }
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
    /// `q` in the picture.
    Quit,
    /// The panel takes the key, as usual: a prompt or a question is open, or it is Ctrl-c,
    /// which the panel decides on everywhere (L14).
    Panel,
    /// Any other key tries again.
    Retry,
}

/// How `panel` takes `key` while the connection is down.
pub fn down_key(panel: &Panel, key: Key) -> DownKey {
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
        },
        Op::UiHere { folder: here },
    ]
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
/// editor's text (which [`Screen`] leaves free), else nowhere (hidden).
pub fn cursor(screen: &Screen, prompting: bool) -> Option<(u16, u16)> {
    let f = screen.geometry.footer;
    if !prompting || f.w == 0 || f.h == 0 {
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
async fn connect(opts: ClientOptions, first: Vec<Op>) -> Result<Linked, Failed> {
    let down = |e: moa_client::ClientError| Failed::Down(e.to_string());
    let client = Client::connect(opts).await.map_err(down)?;
    if !client.hello().matched {
        return Err(Failed::Mismatch);
    }
    let (live, replies, snapshot) = client.live(TOPICS, first).await.map_err(down)?;
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
    events: &mut EventStream,
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
        e = events.next() => Wake::Input(e),
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
    /// `panel.log`, shared with the panic hook.
    log: Option<Logbook>,
    size: Size,
    started: std::time::Instant,
    /// Exists only while the screen animates.
    tick: Option<Interval>,
    /// The next moment a visible text changes.
    deadline: Option<Pin<Box<Sleep>>>,
}

impl Loop {
    fn new(opts: PanelOptions, theme: Theme, size: Size) -> Loop {
        let mut panel = Panel::new(opts.view.unwrap_or(View::Picture));
        // moa tags a panel it places as it starts it (L7), so a placed panel is moa's
        // before its row says so; a panel started by hand isn't.
        panel.set_managed(opts.client.autostart.is_none());
        Loop {
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
            log: None,
            size,
            started: std::time::Instant::now(),
            tick: None,
            deadline: None,
        }
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
        let here = self.panel.here(&self.data);
        let (pane, placed) = (self.opts.pane, self.placed());
        let first = if self.linked_once {
            reconnect_ops(pane, &self.panel, here.clone(), placed)
        } else {
            first_ops(pane, self.opts.view, here.clone(), placed)
        };
        let mut client = self.opts.client.clone();
        if self.linked_once {
            client.autostart = None;
        }
        self.link = Link::Connecting {
            connect: Box::pin(connect(client, first)),
            waited,
            here,
        };
    }

    /// Writes `what` to `panel.log`, naming the view the panel shows now.
    fn log(&self, what: &str) {
        if let Some(log) = &self.log {
            log.shows(self.known_view());
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
            Link::Down(why) => Some(down_line(why.as_deref(), self.placed())),
            Link::Mismatch => Some(MISMATCH.to_string()),
        }
    }

    /// Clears the footer's message.
    fn hush(&mut self) {
        self.panel.replied(Ok(Done {
            text: String::new(),
            data: Default::default(),
        }));
    }

    /// Does what the panel asked. True to quit.
    fn act(&mut self, outs: Vec<Out>) -> bool {
        for out in outs {
            match out {
                Out::Quit => return true,
                Out::Send(op) => match &mut self.link {
                    Link::Up { live, .. } => {
                        self.asked.insert(live.send(op));
                    }
                    // Nothing is sent to another build; elsewhere the footer says why.
                    _ => {
                        let line = self.link_line();
                        self.panel.note(line.as_deref().unwrap_or(CONNECTING));
                    }
                },
            }
        }
        false
    }

    /// Input, a push, a connect's end, a timer.
    fn turn(&mut self, wake: Wake) -> Result<Turn, String> {
        let redraw = match wake {
            Wake::Input(None) => return Ok(Turn::Quit),
            Wake::Input(Some(Err(e))) => return Err(format!("can't read the terminal: {e}")),
            Wake::Input(Some(Ok(event))) => return Ok(self.input(event)),
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
                return Ok(Turn::Quit);
            }
        };
        Ok(Turn::Go(redraw))
    }

    fn input(&mut self, event: Event) -> Turn {
        let ctx = self.ctx();
        let redraw = match event {
            Event::Key(k) => {
                let Some(key) = key_of(k) else {
                    return Turn::Go(false);
                };
                // Ctrl-c is the panel's however the connection is (L14), and what it says
                // isn't covered by the connection's line.
                if key == Key::Ctrl('c') {
                    return match self.panel.interrupt() {
                        Some(Out::Quit) => Turn::Quit,
                        _ => Turn::Go(true),
                    };
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
                self.panel.replied(result);
            }
            LiveMsg::Closed => {
                self.asked.clear();
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
    fn wait(&mut self, waited: usize) {
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
                    Some(why) => self.panel.replied(Err(why)),
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
                self.panel.note(&down_line(Some(&line), self.placed()));
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

    /// Tells the daemon the selection's folder when it changed (V3).
    fn tell_here(&mut self) {
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
    fn draw<B: Backend>(&mut self, term: &mut Terminal<B>) -> Result<(), String> {
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
        .map_err(|e| format!("can't draw the panel: {e}"))?;
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
}

/// Runs a panel until it quits: Ctrl-c in a panel you started, `q` in the picture, a
/// signal, or the terminal's end. The terminal is given back on every way out.
pub async fn run(opts: PanelOptions) -> Result<(), String> {
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let colorterm = std::env::var("COLORTERM").ok();
    let theme = Theme {
        mode: theme::detect(in_tmux, colorterm.as_deref()),
    };
    let mut signals = Signals::new().map_err(|e| format!("can't watch for signals: {e}"))?;
    let log = Logbook::new(&opts);
    let _taken = Taken::take(log.clone()).map_err(|e| format!("can't use this terminal: {e}"))?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|e| format!("can't use this terminal: {e}"))?;
    // Not `Terminal::clear`, which asks the terminal where its cursor is.
    term.backend_mut()
        .clear_region(ClearType::All)
        .map_err(|e| format!("can't use this terminal: {e}"))?;
    let area = term
        .size()
        .map_err(|e| format!("can't use this terminal: {e}"))?;
    let size = Size {
        w: area.width,
        h: area.height,
    };
    let mut events = EventStream::new();
    let mut lp = Loop::new(opts, theme, size);
    lp.log = log;
    lp.log("started");
    lp.connect(0);
    lp.draw(&mut term)?;
    loop {
        let wake = next_wake(
            &mut events,
            &mut lp.link,
            lp.tick.as_mut(),
            lp.deadline.as_mut(),
            &mut signals,
        )
        .await;
        let mut redraw = match lp.turn(wake)? {
            Turn::Quit => return Ok(()),
            Turn::Go(redraw) => redraw,
        };
        redraw |= lp.drain();
        if redraw {
            lp.draw(&mut term)?;
        }
        lp.tell_here();
        lp.note_view();
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{Failed, Loop, PanelOptions, Turn, Wake, now};
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
        lp.turn(Wake::Input(Some(Ok(event)))).unwrap()
    }

    fn ctrl_c() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
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
            shown(&term).contains("part of moa's layout"),
            "{}",
            shown(&term)
        );

        let mut by_hand = looped(Some(View::Tree), false);
        assert!(!by_hand.panel.managed());
        assert!(matches!(input(&mut by_hand, ctrl_c()), Turn::Quit));
    }
}
