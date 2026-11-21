//! The terminal's input, read off the loop's thread.
//!
//! crossterm's reader may never return once its terminal is gone: on an end of file it polls
//! and reads again, for ever (crossterm 0.29, `event/source/unix/tty.rs`, the reader its
//! `use-dev-tty` feature picks; the workspace's `Cargo.toml` says why). A read error it does
//! return, and one only a gone terminal gives (Linux's EIO) is the terminal's end. Run on
//! the loop's thread, as `EventStream` runs it, that spin would also starve every other
//! wake, signals included. So the reader runs on a thread of its own, and a second thread
//! waits for the terminal to hang up, which no input wakes. The loop hears both through one
//! channel and never touches crossterm's reader itself.
//!
//! **Never ask crossterm's reader anything from the loop's thread.** The reader thread holds
//! crossterm's global event-reader lock for the whole run, so a call that takes it blocks
//! the loop for good: `cursor::position`, `event::poll`, `event::read`,
//! `terminal::supports_keyboard_enhancement`, and ratatui's `Terminal::clear` and
//! `get_cursor_position` (which ask where the cursor is).
//!
//! The terminal watched is the one crossterm reads: stdin when it is a terminal, else
//! `/dev/tty`. When neither can be opened, or the watch itself fails, `panel.log` says so
//! once and nothing watches: a terminal that goes may then leave the reader spinning, and
//! the panel ends on the next failed draw (which checks the terminal) or on a signal (tmux
//! sends SIGHUP to a tab it closes).

use std::fs::File;
use std::io::{self, IsTerminal};
use std::os::fd::{AsFd, OwnedFd};

use crossterm::event::Event;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::Logbook;

/// What the terminal sends the loop: an event or a read error, and `None` once it is gone.
type Sent = Option<io::Result<Event>>;

/// The terminal's input.
pub(super) struct Input {
    rx: UnboundedReceiver<Sent>,
    /// The terminal crossterm reads, when it could be opened here.
    tty: Option<OwnedFd>,
}

impl Input {
    /// Starts the reader, and the hang-up watch on the terminal crossterm reads. Why the
    /// watch couldn't start, or failed, goes to `log`.
    pub(super) fn start(log: Option<Logbook>) -> io::Result<Input> {
        let (tx, rx) = unbounded_channel();
        let reader = tx.clone();
        std::thread::Builder::new()
            .name("panel-input".into())
            .spawn(move || read(&reader))?;
        let tty = read_terminal().inspect_err(|e| note(log.as_ref(), e)).ok();
        // The watch gets its own copy of the terminal; one that can't be made is said too.
        match tty.as_ref().map(OwnedFd::try_clone) {
            Some(Ok(watched)) => {
                std::thread::Builder::new()
                    .name("panel-hangup".into())
                    .spawn(move || watch(&watched, &tx, log.as_ref()))?;
            }
            Some(Err(e)) => note(log.as_ref(), &e),
            None => {}
        }
        Ok(Input { rx, tty })
    }

    /// The next event or read error; `None` once the terminal is gone.
    pub(super) async fn next(&mut self) -> Sent {
        self.rx.recv().await.flatten()
    }

    /// Whether the terminal has gone, asked now; false when it isn't watched.
    pub(super) fn gone(&self) -> bool {
        self.tty
            .as_ref()
            .is_some_and(|t| moa_platform::terminal_gone(t.as_fd()))
    }
}

/// The terminal crossterm reads: a copy of stdin when it is a terminal, else `/dev/tty`.
fn read_terminal() -> io::Result<OwnedFd> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        stdin.as_fd().try_clone_to_owned()
    } else {
        File::open("/dev/tty").map(OwnedFd::from)
    }
}

/// The one line saying the terminal isn't watched, and why. Nothing is said when the
/// terminal (the panel's stdout) has already gone: that is why it can't be watched, and the
/// panel's end line says so.
fn note(log: Option<&Logbook>, e: &io::Error) {
    if moa_platform::terminal_gone(io::stdout().as_fd()) {
        return;
    }
    if let Some(log) = log {
        log.write(&format!("can't watch the terminal for its end: {e}"));
    }
}

/// Reads events until the loop stops listening, or after the first error.
fn read(tx: &UnboundedSender<Sent>) {
    loop {
        let sent = sent(crossterm::event::read());
        let last = !matches!(sent, Some(Ok(_)));
        if tx.send(sent).is_err() || last {
            return;
        }
    }
}

/// What a read tells the loop. A read that fails as only a gone terminal fails (EIO once the
/// other end has closed, as Linux says it) is the terminal's end, as the watch says it.
fn sent(read: io::Result<Event>) -> Sent {
    match read {
        Err(e) if moa_platform::terminal_gone_error(&e) => None,
        read => Some(read),
    }
}

/// Says the terminal is gone once it hangs up; a watch that fails says so in `log`.
fn watch(tty: &OwnedFd, tx: &UnboundedSender<Sent>, log: Option<&Logbook>) {
    match moa_platform::wait_hangup(tty.as_fd()) {
        Ok(()) => {
            let _ = tx.send(None);
        }
        Err(e) => note(log, &e),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::sent;

    #[test]
    fn a_read_failing_as_a_gone_terminal_is_its_end() {
        // EIO, ENXIO and EPIPE, the same numbers on Linux and macOS.
        for errno in [5, 6, 32] {
            assert!(
                sent(Err(io::Error::from_raw_os_error(errno))).is_none(),
                "{errno}"
            );
        }
        let other = sent(Err(io::Error::other("odd")));
        assert!(matches!(other, Some(Err(e)) if e.to_string() == "odd"));
        let key = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(matches!(sent(Ok(key.clone())), Some(Ok(e)) if e == key));
    }
}
