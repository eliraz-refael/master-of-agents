//! What a new session is told about its peers.

use crate::model::SessionId;
use crate::state::State;

/// The note a `SessionStart` request is answered with. Nothing yet: slice 7 fills it, and
/// the answer's shape (`{"context": ..}`) is already in place.
pub(crate) fn peers_note(_state: &State, _session: SessionId) -> Option<String> {
    None
}
