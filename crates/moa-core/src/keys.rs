//! moa's keys, all taken here (L13): `$` opens a terminal, `n` asks for a new session's
//! folder and title, `c t p g` go to a panel, `1`–`4` apply the presets and `r` repairs.
//! Each answers the client that pressed it with a notice. The keys whose slice hasn't landed
//! fall through to the link's "not yet" notice.

use crate::cockpit::run::{CockpitOp, Select, queue};
use crate::effect::Effect;
use crate::layout::{Leaf, PRESET_KEYS};
use crate::ops::term::{Tell, term_key};
use crate::panel::View;
use crate::prompt;
use crate::state::State;
use crate::tmux::ids::{ClientName, PaneId};
use crate::tmux::mailbox::KeyAction;

/// What `key`, pressed in `pane` (none from a menu) by `client`, does. `None` for a key
/// this build doesn't have yet.
pub(crate) fn run(
    state: &mut State,
    key: KeyAction,
    pane: Option<PaneId>,
    client: ClientName,
) -> Option<Vec<Effect>> {
    let tell = Some(Tell::Notice(client.clone()));
    let goto = |leaf| CockpitOp::Build {
        name: None,
        tell: tell.clone(),
        select: Select::Leaf(leaf),
    };
    let op = match key {
        KeyAction::Terminal => return Some(term_key(state, pane, client)),
        KeyAction::New => return Some(prompt::new_session(state, client)),
        KeyAction::Console => goto(Leaf::Panel(View::Console)),
        KeyAction::Tree => goto(Leaf::Panel(View::Tree)),
        KeyAction::Plan => goto(Leaf::Panel(View::Plan)),
        KeyAction::Agent => goto(Leaf::Agent),
        // `Ctrl-a 1` to `4`: the presets, in the spike's order.
        KeyAction::Layout(n) => CockpitOp::Build {
            name: Some(PRESET_KEYS[usize::from(n.clamp(1, 4)) - 1].to_string()),
            tell: tell.clone(),
            select: Select::Default,
        },
        KeyAction::Repair => CockpitOp::Repair { tell: tell.clone() },
        KeyAction::Find | KeyAction::FindOther | KeyAction::Help => return None,
    };
    Some(queue(state, op))
}
