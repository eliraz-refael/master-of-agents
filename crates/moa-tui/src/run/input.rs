//! The terminal's input, read off the loop's thread.
//!
//! crossterm's reader never returns once its terminal is gone: on an end of file or a read
//! error it reads again, for ever (crossterm 0.29, `event/source/unix/mio.rs`). Run on the
//! loop's thread, as `EventStream` runs it, that spin would also starve every other wake,
//! signals included. So the reader runs on a thread of its own, and a second thread waits
//! for the terminal to hang up, which no input wakes. The loop hears both through one
//! channel and never touches crossterm's reader itself.

use std::io::{self, IsTerminal};
use std::os::fd::AsFd;

use crossterm::event::Event;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// What the terminal sends the loop: an event or a read error, and `None` once it is gone.
type Sent = Option<io::Result<Event>>;

/// The terminal's input.
pub(super) struct Input {
    rx: UnboundedReceiver<Sent>,
}

impl Input {
    /// Starts the reader, and the hang-up watch when stdin is the terminal (crossterm reads
    /// stdin then; otherwise it reads `/dev/tty`, which nothing here watches).
    pub(super) fn start() -> io::Result<Input> {
        let (tx, rx) = unbounded_channel();
        let reader = tx.clone();
        std::thread::Builder::new()
            .name("panel-input".into())
            .spawn(move || read(&reader))?;
        if io::stdin().is_terminal() {
            std::thread::Builder::new()
                .name("panel-hangup".into())
                .spawn(move || watch(&tx))?;
        }
        Ok(Input { rx })
    }

    /// The next event or read error; `None` once the terminal is gone.
    pub(super) async fn next(&mut self) -> Sent {
        self.rx.recv().await.flatten()
    }
}

/// Reads events until the loop stops listening, or after the first error.
fn read(tx: &UnboundedSender<Sent>) {
    loop {
        let event = crossterm::event::read();
        let failed = event.is_err();
        if tx.send(Some(event)).is_err() || failed {
            return;
        }
    }
}

/// Says the terminal is gone once stdin hangs up. If the watch itself fails, it says
/// nothing: the reader and the signals still end the panel.
fn watch(tx: &UnboundedSender<Sent>) {
    if moa_platform::wait_hangup(io::stdin().as_fd()).is_ok() {
        let _ = tx.send(None);
    }
}
