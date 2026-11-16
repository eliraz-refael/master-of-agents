//! What the status line shows: counts of live sessions, a state per window, and a clock
//! that ticks only while a user is attached. All of it is decided here and written as one
//! batch, only what changed since the last write.

use std::collections::BTreeMap;

use super::cmd::Cmd;
use super::focus::trusted_panes;
use super::ids::WindowId;
use super::link::Link;
use crate::effect::{Effect, TimerKey};
use crate::live::{DisplayState, display_state};
use crate::state::State;
use crate::time::local_format;
use crate::{Now, UtcMs};

const STATUS: &str = "@moa_status";
const CLOCK: &str = "@moa_clock";
const WINDOW_STATE: &str = "@moa_state";

/// The colour of a state's count, for the states that are counted.
fn colour(s: DisplayState) -> Option<&'static str> {
    Some(match s {
        DisplayState::Waiting => "#ebcb8b",
        DisplayState::Question => "#d08770",
        DisplayState::Failed => "#d57780",
        DisplayState::Done => "#a3be8c",
        DisplayState::Working => "#88c0d0",
        DisplayState::Idle => "#8c9ab3",
        DisplayState::Asleep | DisplayState::Ended | DisplayState::Archived => return None,
    })
}

/// The status line's summary: a coloured count per live state, most urgent first.
pub fn counts_text(state: &State) -> String {
    let mut n: BTreeMap<DisplayState, usize> = BTreeMap::new();
    for s in state.sessions() {
        *n.entry(display_state(s, state.now).0).or_default() += 1;
    }
    n.into_iter()
        .filter_map(|(s, n)| {
            let c = colour(s)?;
            Some(format!("#[fg={c}]{} {n}", s.glyph()))
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// The local `HH:MM`.
pub fn clock_text(now: &Now) -> String {
    local_format(now.wall, &now.tz, "%H:%M")
}

/// The next whole UTC minute, strictly after `at`.
pub fn next_minute(at: UtcMs) -> UtcMs {
    UtcMs((at.0.div_euclid(60_000) + 1) * 60_000)
}

fn window_key(w: WindowId) -> String {
    format!("{WINDOW_STATE} {w}")
}

/// The commands that bring the server's status options up to date, or `None` when nothing
/// changed or the link isn't up.
pub(crate) fn sync(state: &mut State) -> Option<Effect> {
    let Link::Up { conn, .. } = state.tmux().link else {
        return None;
    };
    let text = counts_text(state);
    // Each window's most urgent trusted pane.
    let mut windows: BTreeMap<WindowId, DisplayState> = BTreeMap::new();
    for (pane, id) in trusted_panes(state) {
        let (Some(w), Some(s)) = (state.tmux().topology.window_of(pane), state.session(id)) else {
            continue;
        };
        let d = display_state(s, state.now).0;
        let e = windows.entry(w).or_insert(d);
        *e = (*e).min(d);
    }
    let mut cmds = Vec::new();
    let tmux = &mut state.tmux;
    if tmux.written.get(STATUS) != Some(&text) {
        cmds.push(Cmd::new("set-option").args(["-g", STATUS, &text]));
        tmux.written.insert(STATUS.into(), text);
    }
    for (w, d) in &windows {
        let key = window_key(*w);
        if tmux.written.get(&key).map(String::as_str) != Some(d.word()) {
            cmds.push(Cmd::new("set-option").args([
                "-w",
                "-t",
                &w.to_string(),
                WINDOW_STATE,
                d.word(),
            ]));
            tmux.written.insert(key, d.word().into());
        }
    }
    let gone: Vec<(String, WindowId)> = tmux
        .written
        .keys()
        .filter_map(|k| {
            let w: WindowId = k.strip_prefix(WINDOW_STATE)?.trim().parse().ok()?;
            (!windows.contains_key(&w)).then(|| (k.clone(), w))
        })
        .collect();
    for (key, w) in gone {
        tmux.written.remove(&key);
        // A window that is gone has nothing to unset.
        if tmux.topology.has_window(w) {
            cmds.push(Cmd::new("set-option").args([
                "-w",
                "-u",
                "-t",
                &w.to_string(),
                WINDOW_STATE,
            ]));
        }
    }
    (!cmds.is_empty()).then_some(Effect::Tmux {
        conn,
        cmds,
        token: None,
    })
}

/// Starts, ticks and stops the minute clock: it runs only while a user is attached.
pub(crate) fn clock(state: &mut State, now: &Now) -> Vec<Effect> {
    let tmux = &mut state.tmux;
    let Link::Up { conn, .. } = tmux.link else {
        return Vec::new();
    };
    if !tmux.topology.users_attached() {
        tmux.written.remove(CLOCK);
        return match tmux.clock_at.take() {
            Some(_) => vec![Effect::CancelTimer(TimerKey::Clock)],
            None => Vec::new(),
        };
    }
    let text = clock_text(now);
    // A deadline that has passed has fired and is spent.
    let due = tmux.clock_at.is_none_or(|at| at <= now.wall);
    if !due && tmux.written.get(CLOCK) == Some(&text) {
        return Vec::new();
    }
    let at = next_minute(now.wall);
    tmux.clock_at = Some(at);
    let cmds = vec![Cmd::new("set-option").args(["-g", CLOCK, &text])];
    tmux.written.insert(CLOCK.into(), text);
    vec![
        Effect::Tmux {
            conn,
            cmds,
            token: None,
        },
        Effect::SetTimer {
            key: TimerKey::Clock,
            at,
        },
    ]
}

/// Adds what the status line needs after an input to `fx`: one batch of writes, then the
/// clock's timer. It comes last, after any `Persist`.
pub(crate) fn run(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let mut cmds = Vec::new();
    let mut conn = None;
    let mut rest = Vec::new();
    for e in sync(state).into_iter().chain(clock(state, now)) {
        match e {
            Effect::Tmux {
                conn: c,
                cmds: mut more,
                ..
            } => {
                conn = Some(c);
                cmds.append(&mut more);
            }
            other => rest.push(other),
        }
    }
    if let Some(conn) = conn {
        fx.push(Effect::Tmux {
            conn,
            cmds,
            token: None,
        });
    }
    fx.extend(rest);
}
