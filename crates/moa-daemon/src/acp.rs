//! The link to moa's agent: the actor that hosts it ([`Adapter`]). It starts the agent when the
//! core asks (only a prompt does), carries the core's commands to the run they are for, and
//! stops the agent, with everything it started, when the daemon stops. What the agent says
//! goes straight to the core from the host's handlers, never through this actor, so a busy
//! actor never keeps the agent from being read.
//!
//! The actor holds no policy: the core decides when to start, what to open and what to send.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moa_agents::acp::{Adapter, ToCore};
use moa_core::central::wire::{AcpCmd, AcpIn, AdapterSpec};
use moa_platform::lock;
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};

use crate::supervise::supervise;

/// The actor's name, in the core's line when it is restarted.
const NAME: &str = "the link to moa's agent";

/// How long the agent may take to start and say what it offers.
const START_WAIT: Duration = Duration::from_secs(30);

/// Why a start that took longer than [`START_WAIT`] was given up on.
const START_TOO_LONG: &str = "moa's agent didn't start within 30 s";

/// Why the agent of a link that panicked is gone.
const LINK_FAILED: &str = "the link to moa's agent failed";

/// What the core asks of the link.
pub(crate) enum AcpMsg {
    Cmd(AcpCmd),
    /// Stop the agent and its process group, then answer. The link ends.
    Stop(oneshot::Sender<()>),
    /// Tests only: the actor panics, at once even while the agent starts.
    #[cfg(test)]
    Crash,
}

/// What outlives a restart: the inbox, and the run last started, which a panic says has
/// exited (the agent goes with the panicking actor).
struct Shared {
    to_core: ToCore,
    inbox: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AcpMsg>>>,
    live: Mutex<Option<u64>>,
}

/// Starts the link under `supervise`. It does nothing until it is asked to start the agent;
/// `to_core` hears everything the agent says, and `failed` hears of a panic (after the core
/// has heard that the live run exited).
pub(crate) fn start(
    to_core: impl Fn(AcpIn) + Send + Sync + 'static,
    failed: impl Fn(&'static str, String) + Send + 'static,
) -> mpsc::UnboundedSender<AcpMsg> {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        to_core: Arc::new(to_core),
        inbox: Arc::new(tokio::sync::Mutex::new(rx)),
        live: Mutex::new(None),
    });
    let sh = Arc::clone(&shared);
    let failed = move |name, why| {
        // Taken: the restarted link has no agent, and says nothing more for this run.
        let live = lock(&sh.live).take();
        if let Some(generation) = live {
            (sh.to_core)(AcpIn::Exited {
                generation,
                why: LINK_FAILED.into(),
            });
        }
        failed(name, why);
    };
    tokio::spawn(supervise(
        NAME,
        move || {
            let shared = Arc::clone(&shared);
            async move {
                let inbox = Arc::clone(&shared.inbox).lock_owned().await;
                Actor {
                    sh: shared,
                    agent: None,
                }
                .run(inbox)
                .await;
            }
        },
        failed,
    ));
    tx
}

/// How a start ended early: the link was asked to stop (`Some`), or the core has gone.
type Stopped = Option<oneshot::Sender<()>>;

struct Actor {
    sh: Arc<Shared>,
    /// The agent last started, and its run. It may have exited on its own since.
    agent: Option<(u64, Adapter)>,
}

impl Actor {
    async fn run(mut self, mut inbox: OwnedMutexGuard<mpsc::UnboundedReceiver<AcpMsg>>) {
        // What came while the agent started, in order.
        let mut queued = VecDeque::new();
        let stopped = loop {
            let msg = match queued.pop_front() {
                Some(msg) => Some(msg),
                None => inbox.recv().await,
            };
            match msg {
                Some(AcpMsg::Cmd(AcpCmd::Start { generation, spec })) => {
                    if let Err(stopped) =
                        self.start(generation, spec, &mut inbox, &mut queued).await
                    {
                        break stopped;
                    }
                }
                Some(AcpMsg::Cmd(cmd)) => self.command(cmd).await,
                Some(AcpMsg::Stop(done)) => break Some(done),
                #[cfg(test)]
                Some(AcpMsg::Crash) => panic!("a test crashed the link to moa's agent"),
                // The core has gone.
                None => break None,
            }
        };
        self.stop_agent().await;
        if let Some(done) = stopped {
            let _ = done.send(());
        }
    }

    /// Starts run `generation`'s agent once the last one has stopped, so the two never share
    /// the agent folder. A start that fails, or takes longer than [`START_WAIT`], is told to
    /// the core as that run's exit. A stop that comes meanwhile gives the start up, which ends
    /// the agent; other messages wait for the start (but a test's crash).
    async fn start(
        &mut self,
        generation: u64,
        spec: AdapterSpec,
        inbox: &mut mpsc::UnboundedReceiver<AcpMsg>,
        queued: &mut VecDeque<AcpMsg>,
    ) -> Result<(), Stopped> {
        self.stop_agent().await;
        *lock(&self.sh.live) = Some(generation);
        let to_core = Arc::clone(&self.sh.to_core);
        let starting = tokio::time::timeout(START_WAIT, Adapter::start(generation, &spec, to_core));
        tokio::pin!(starting);
        let started = loop {
            tokio::select! {
                started = &mut starting => break started,
                msg = inbox.recv() => match msg {
                    Some(AcpMsg::Stop(done)) => return Err(Some(done)),
                    None => return Err(None),
                    #[cfg(test)]
                    Some(AcpMsg::Crash) => panic!("a test crashed the link to moa's agent"),
                    Some(msg) => queued.push_back(msg),
                },
            }
        };
        let why = match started {
            Ok(Ok(agent)) => {
                self.agent = Some((generation, agent));
                return Ok(());
            }
            Ok(Err(why)) => why,
            Err(_) => START_TOO_LONG.to_string(),
        };
        // The reason may name a path: it goes to the log and to whoever asked, nowhere else.
        tracing::warn!("moa's agent couldn't start: {why}");
        (self.sh.to_core)(AcpIn::Exited { generation, why });
        Ok(())
    }

    /// A command for the agent of its run. One for another run (an earlier one, or one that
    /// never started) has nobody to go to, and is dropped.
    async fn command(&mut self, cmd: AcpCmd) {
        let agent = match &self.agent {
            Some((generation, agent)) if *generation == cmd.generation() => agent,
            _ => {
                tracing::debug!("a command for another run of moa's agent was dropped");
                return;
            }
        };
        match cmd {
            AcpCmd::Open { how, cwd, .. } => agent.open(how, &cwd),
            AcpCmd::Prompt {
                thread,
                prompt,
                text,
                ..
            } => agent.prompt(&thread, prompt, &text),
            AcpCmd::Cancel { thread, .. } => agent.cancel(&thread),
            AcpCmd::Permission { req, choice, .. } => agent.answer(req, choice),
            AcpCmd::Stop { .. } => self.stop_agent().await,
            // Handled by `run`.
            AcpCmd::Start { .. } => {}
        }
    }

    /// Stops the agent, if there is one, with its whole process group.
    async fn stop_agent(&mut self) {
        if let Some((_, agent)) = self.agent.take() {
            agent.stop().await;
        }
    }
}
