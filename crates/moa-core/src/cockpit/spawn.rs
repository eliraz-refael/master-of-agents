//! Panels started parked (L7): one `new-window` each in the hidden session, with its view,
//! its brain and an explicit environment, answered per command (L18). Every pane that
//! started is tagged, recorded in the topology and counted, whatever the others' answers.

use super::build::{keep_on_exit, view_tags};
use super::scene::HIDDEN;
use crate::effect::{Effect, LogLevel};
use crate::panel::View;
use crate::state::State;
use crate::tmux::cmd::{Cmd, literal_format};
use crate::tmux::ids::PaneId;
use crate::tmux::link::{Answers, Asked, log};

/// What a panel's `new-window -P` prints: its pane.
const SPAWNED_FORMAT: &str = "#{pane_id}";

/// `<exe> panel <view> --brain <brain>`: what a panel runs, in a new window or respawned in
/// place.
pub(crate) fn panel_argv(state: &State, view: View) -> Vec<String> {
    vec![
        state.deps().panel.exe.display().to_string(),
        "panel".to_string(),
        view.word().to_string(),
        "--brain".to_string(),
        state.brain().as_str().to_string(),
    ]
}

/// L7: one `new-window` per view, parked in `_moa`, each printing its pane. It opens at
/// home, since `moa panel` refuses a folder that's gone and `_moa`'s is wherever moa first
/// ran. The environment goes explicitly: `MOA_BRAIN`, then the daemon's (`MOA_ROOT` when
/// sandboxed).
pub fn spawn_cmds(state: &State, views: &[View]) -> Vec<Cmd> {
    let deps = state.deps();
    let home = deps.home.display().to_string();
    views
        .iter()
        .map(|view| {
            let env = std::iter::once(("MOA_BRAIN", state.brain().as_str()))
                .chain(deps.panel.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .flat_map(|(k, v)| ["-e".to_string(), format!("{k}={v}")]);
            // tmux expands `-c` and `-n` as formats; a view's word has no `#`.
            Cmd::new("new-window")
                .args(["-d", "-P", "-F", SPAWNED_FORMAT, "-c"])
                .arg(literal_format(&home))
                .args(["-t", &format!("={HIDDEN}:"), "-n", view.word()])
                .args(env)
                .args(panel_argv(state, *view))
        })
        .collect()
}

/// L7's tags for a panel just started in `pane`: its view, its label, and a pane that stays
/// when its program stops, saying so. Its frame comes with the build that places it (L10).
pub fn spawn_tags(pane: PaneId, view: View) -> Vec<Cmd> {
    let mut cmds = view_tags(pane, view).to_vec();
    cmds.extend(keep_on_exit(pane));
    cmds
}

/// Starts `views` for the cockpit operation `seq`: the tokened commands, whose answer comes
/// back as `Asked::Spawn` for [`spawned`]. `None` while the link isn't up.
#[allow(dead_code)] // Unused until the cockpit queue (L17) sends spawns.
pub(crate) fn spawn(state: &mut State, views: Vec<View>, seq: u64) -> Option<Effect> {
    let cmds = spawn_cmds(state, &views);
    state.tmux.ask(Asked::Spawn { views, seq }, cmds)
}

/// tmux answered the spawn of `views`, one answer per command (L18): every pane that
/// started is tagged (all in one `Tmux` effect), recorded in the topology at once and
/// counted in `panel_starts`. Gives the effects and the views that didn't start, each of
/// which is logged with tmux's words.
pub(crate) fn spawned(
    state: &mut State,
    views: &[View],
    answers: Answers,
) -> (Vec<Effect>, Vec<View>) {
    let mut answers = answers.into_iter();
    let (mut cmds, mut failed, mut fx) = (Vec::new(), Vec::new(), Vec::new());
    for view in views {
        let why = match answers.next() {
            Some(Ok(lines)) => match lines.as_slice() {
                [line] => match line.parse::<PaneId>() {
                    Ok(pane) => {
                        cmds.extend(spawn_tags(pane, *view));
                        state.tmux.view_sent(pane, view.word());
                        state.panel_starts += 1;
                        continue;
                    }
                    Err(_) => "tmux's answer didn't parse".to_string(),
                },
                _ => "tmux's answer didn't parse".to_string(),
            },
            Some(Err(e)) => e.lines().next().unwrap_or("").to_string(),
            None => "tmux didn't answer".to_string(),
        };
        fx.push(log(
            LogLevel::Warn,
            format!("the {} panel couldn't start: {why}", view.word()),
        ));
        failed.push(*view);
    }
    if let (Some(conn), false) = (state.tmux.conn(), cmds.is_empty()) {
        fx.insert(
            0,
            Effect::Tmux {
                conn,
                cmds,
                token: None,
            },
        );
    }
    (fx, failed)
}

#[cfg(test)]
mod tests;
