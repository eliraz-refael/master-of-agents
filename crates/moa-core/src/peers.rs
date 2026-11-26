//! What a new session is told about its peers (A12): every session moa launched is told at
//! its start that it can ask the other agents and moa, and how. The injected plugin's
//! `moa-peers` skill says the same, and more.

use crate::asks::ASK_WAIT_DEFAULT_MS;
use crate::model::SessionId;
use crate::state::State;
use crate::time::MINUTE;

/// The note a session moa launched is given as its `SessionStart` context (A13). Its "9
/// minutes" is `ask`'s default wait, and the Bash timeout is the longest Claude Code's Bash
/// tool takes, which outlasts it.
pub const PEERS_NOTE: &str = "\
moa: you can message the other agents in this cockpit, and moa itself.
- `moactl peers` lists them. `moactl ask NAME \"TEXT\"` asks one and waits for the reply,
  up to 9 minutes: run it with a Bash timeout of 600000 ms.
- \"no reply yet\" means it's still open: `moactl reply ID` waits again. `moactl asks`
  lists recent asks.
- A message starting `[moa · message from NAME · ask #N]` is an ask to you: your reply
  that turn goes back to NAME as the answer.";

/// The longest timeout Claude Code's Bash tool takes, in ms: the note's.
const BASH_TIMEOUT_MAX_MS: i64 = 600_000;

// The note's numbers: the default wait is 9 minutes, and a Bash call that long survives it.
const _: () = assert!(ASK_WAIT_DEFAULT_MS == 9 * MINUTE);
const _: () = assert!(ASK_WAIT_DEFAULT_MS < BASH_TIMEOUT_MAX_MS);

/// The note a `SessionStart` request of `session` is answered with: [`PEERS_NOTE`] when moa
/// launched the agent in its tab (its binding is tracked), so the agent has moa's plugin and
/// its skill. An adopted agent, or one in no tab moa knows of, gets nothing.
pub(crate) fn peers_note(state: &State, session: SessionId) -> Option<String> {
    state
        .binding(session)
        .is_some_and(|b| b.tracked)
        .then(|| PEERS_NOTE.to_string())
}
