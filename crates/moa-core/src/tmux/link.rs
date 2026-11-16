//! The link to moa's terminal server, as the core sees it: whether it is up, the focus
//! topology it keeps, the `tmux.ensure` callers waiting for it, and the server's
//! generations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::cmd::Cmd;
use super::ids::{ClientName, Generation};
use super::notify::Notification;
use super::topology::{Applied, Topology, refresh_cmds};
use crate::effect::{Down, Effect, LogLevel, LogLine, TmuxIn, TmuxToken};
use crate::op::{ConnId, Done, OpError, ReqId};

/// The session moa's front door attaches to.
const MOA_SESSION: &str = "moa";

const NOT_TMUX: &str = "that isn't a tmux program";
const NO_WINDOW: &str = "moa couldn't open its window; see the log";
const GAVE_UP: &str = "moa lost its terminal server and couldn't reconnect; see the log";
const NO_TMUX: &str = "moa can't find tmux; install tmux 3.4 or newer";
const DISABLED: &str = "moa v1 isn't switched over yet; try moa --sandbox";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    Down,
    Connecting,
    Up {
        conn: u64,
        generation: Generation,
        version: String,
        ctl: ClientName,
    },
}

/// The refresh lists: none asked, one in flight, or one in flight and another asked since.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Refresh {
    #[default]
    Idle,
    InFlight,
    Again,
}

/// The link's state. `State::load` never touches it: it describes the server, not the
/// brain.
#[derive(Debug)]
pub struct TmuxState {
    pub link: Link,
    pub topology: Topology,
    /// How many times the server was seen to restart (a new generation).
    pub restarts: u32,
    /// `tmux.ensure` callers waiting for the link and the `moa` session.
    waiters: Vec<(ConnId, ReqId)>,
    refresh: Refresh,
    /// The `moa` session was asked for since it was last seen missing.
    cockpit: bool,
    /// The last server generation the link connected to.
    generation: Option<Generation>,
    /// The tmux program the front door found.
    tmux: Option<PathBuf>,
    /// The status options last written to the server, by name. Forgotten whenever the
    /// server may not have them.
    written: BTreeMap<String, String>,
}

impl Default for TmuxState {
    fn default() -> Self {
        TmuxState {
            link: Link::Down,
            topology: Topology::default(),
            restarts: 0,
            waiters: Vec::new(),
            refresh: Refresh::Idle,
            cockpit: false,
            generation: None,
            tmux: None,
            written: BTreeMap::new(),
        }
    }
}

/// An absolute path whose last part is `tmux`.
fn is_tmux_program(p: &Path) -> bool {
    p.is_absolute() && p.as_os_str().as_encoded_bytes().ends_with(b"/tmux")
}

fn running(yes: bool) -> Result<Done, OpError> {
    Ok(Done {
        text: String::new(),
        data: json!({ "running": yes }),
    })
}

fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log(LogLine { level, message })
}

impl TmuxState {
    /// The daemon has started: connect.
    pub(crate) fn startup(&mut self) -> Vec<Effect> {
        self.link = Link::Connecting;
        vec![self.connect()]
    }

    /// `tmux.ensure`: answered now, or once the link knows.
    pub(crate) fn ensure(
        &mut self,
        conn: ConnId,
        req: ReqId,
        tmux: Option<PathBuf>,
    ) -> Vec<Effect> {
        if let Some(t) = tmux {
            if !is_tmux_program(&t) {
                return vec![Effect::Reply {
                    conn,
                    req,
                    result: Err(OpError(NOT_TMUX.into())),
                }];
            }
            self.tmux = Some(t);
        }
        match self.link {
            Link::Up { .. } if self.topology.session_named(MOA_SESSION).is_some() => {
                vec![Effect::Reply {
                    conn,
                    req,
                    result: running(true),
                }]
            }
            Link::Up { .. } => {
                self.waiters.push((conn, req));
                // Asked again: the `moa` session may be created again.
                self.cockpit = false;
                self.refresh()
            }
            Link::Connecting => {
                self.waiters.push((conn, req));
                Vec::new()
            }
            Link::Down => {
                self.waiters.push((conn, req));
                self.link = Link::Connecting;
                vec![self.connect()]
            }
        }
    }

    pub(crate) fn input(&mut self, input: TmuxIn) -> Vec<Effect> {
        match input {
            TmuxIn::Connected {
                conn,
                generation,
                version,
                ctl,
            } => self.connected(conn, generation, version, ctl),
            TmuxIn::Down(d) => self.down(d),
            TmuxIn::Notification(n) => self.notification(&n),
            TmuxIn::Replies {
                conn,
                token,
                results,
            } => {
                if self.conn() != Some(conn) {
                    return Vec::new();
                }
                match token {
                    TmuxToken::Refresh => self.listed(&results),
                    TmuxToken::Cockpit => self.created(&results),
                }
            }
        }
    }

    /// After a reload: the link and what it knows stay; its bookkeeping starts over, since a
    /// failed commit or a panic may have left it half-changed.
    pub(crate) fn reload(&mut self) -> Vec<Effect> {
        self.refresh = Refresh::Idle;
        self.cockpit = false;
        self.written.clear();
        match self.link {
            Link::Up { .. } => self.refresh(),
            Link::Connecting => vec![self.connect()],
            Link::Down => Vec::new(),
        }
    }

    fn conn(&self) -> Option<u64> {
        match self.link {
            Link::Up { conn, .. } => Some(conn),
            _ => None,
        }
    }

    fn connect(&self) -> Effect {
        Effect::TmuxConnect {
            tmux: self.tmux.clone(),
        }
    }

    /// Reads the lists again, at most one in flight and one more after it.
    fn refresh(&mut self) -> Vec<Effect> {
        let Some(conn) = self.conn() else {
            return Vec::new();
        };
        match self.refresh {
            Refresh::Idle => {
                self.refresh = Refresh::InFlight;
                vec![Effect::Tmux {
                    conn,
                    cmds: refresh_cmds(),
                    token: Some(TmuxToken::Refresh),
                }]
            }
            Refresh::InFlight | Refresh::Again => {
                self.refresh = Refresh::Again;
                Vec::new()
            }
        }
    }

    fn connected(
        &mut self,
        conn: u64,
        generation: Generation,
        version: String,
        ctl: ClientName,
    ) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.conn() != Some(conn) {
            // A new connection: nothing asked on another one will be answered.
            self.refresh = Refresh::Idle;
            self.cockpit = false;
        }
        if self.generation.is_some_and(|g| g != generation) {
            self.restarts += 1;
            fx.push(log(LogLevel::Info, "the terminal server restarted".into()));
        }
        self.generation = Some(generation);
        self.link = Link::Up {
            conn,
            generation,
            version,
            ctl,
        };
        self.written.clear();
        fx.extend(self.refresh());
        fx
    }

    fn down(&mut self, d: Down) -> Vec<Effect> {
        self.link = match d {
            // The link is reconnecting.
            Down::Lost => Link::Connecting,
            _ => Link::Down,
        };
        self.topology = Topology::default();
        self.written.clear();
        self.refresh = Refresh::Idle;
        self.cockpit = false;
        // The status clock, once there is one, stops here too.
        let result = match d {
            Down::Lost => return Vec::new(),
            Down::NoServer => running(false),
            Down::GaveUp => Err(OpError(GAVE_UP.into())),
            Down::TooOld(v) => Err(OpError(format!(
                "moa needs tmux 3.4 or newer; this one is {v}"
            ))),
            Down::NoTmux => {
                // It couldn't be started: the next ensure says which one to use.
                self.tmux = None;
                Err(OpError(NO_TMUX.into()))
            }
            Down::Disabled => Err(OpError(DISABLED.into())),
        };
        self.answer(&result)
    }

    fn notification(&mut self, n: &Notification) -> Vec<Effect> {
        if self.conn().is_none() {
            return Vec::new();
        }
        match n {
            // The mailbox is read elsewhere; the link reports its own end as `Down`.
            Notification::Message(_) | Notification::Exit => Vec::new(),
            n => match self.topology.apply(n) {
                Applied::Refresh => self.refresh(),
                Applied::Changed | Applied::Same => Vec::new(),
            },
        }
    }

    /// The refresh lists came back.
    fn listed(&mut self, results: &[Result<Vec<String>, String>]) -> Vec<Effect> {
        let again = self.refresh == Refresh::Again;
        self.refresh = Refresh::Idle;
        let mut fx = Vec::new();
        let listed = match results {
            [Ok(sessions), Ok(panes), Ok(clients)] => {
                self.topology = Topology::from_lists(sessions, panes, clients);
                true
            }
            _ => {
                fx.push(log(
                    LogLevel::Warn,
                    "listing the terminal server failed".into(),
                ));
                false
            }
        };
        if again {
            fx.extend(self.refresh());
        }
        // No information: what was known stays, and nothing is decided on it.
        if !listed {
            return fx;
        }
        if self.topology.session_named(MOA_SESSION).is_some() {
            self.cockpit = false;
            fx.extend(self.answer(&running(true)));
        } else if !self.cockpit
            && let Some(conn) = self.conn()
        {
            self.cockpit = true;
            fx.push(Effect::Tmux {
                conn,
                cmds: vec![Cmd::new("new-session").args([
                    "-d",
                    "-s",
                    MOA_SESSION,
                    "-n",
                    MOA_SESSION,
                ])],
                token: Some(TmuxToken::Cockpit),
            });
        }
        fx
    }

    /// The `moa` session create came back. On success the server says its sessions changed,
    /// and the refresh that follows answers the waiters.
    fn created(&mut self, results: &[Result<Vec<String>, String>]) -> Vec<Effect> {
        let Some(e) = results.iter().find_map(|r| r.as_ref().err()) else {
            return Vec::new();
        };
        let first = e.lines().next().unwrap_or("");
        let mut fx = vec![log(
            LogLevel::Warn,
            format!("creating the moa session failed: {first}"),
        )];
        fx.extend(self.answer(&Err(OpError(NO_WINDOW.into()))));
        fx
    }

    /// Answers every waiter with `result`.
    fn answer(&mut self, result: &Result<Done, OpError>) -> Vec<Effect> {
        std::mem::take(&mut self.waiters)
            .into_iter()
            .map(|(conn, req)| Effect::Reply {
                conn,
                req,
                result: result.clone(),
            })
            .collect()
    }
}
