# THROWAWAY PROTOTYPE: where a brain's moa lives. Sourced by the moa launcher, the tracking
# hook and the plugin's moactl; `moa_brain NAME` exports it for everything they start.
# Each brain is its own moa: its own tmux server, brain.db and scope (what it's for, which
# the briefing gives moa). No name is the default brain, the private one, kept in the repo.
# A named brain (`moa work`) lives outside the repo, so nothing in it can be committed.
# Not product code.
moa_brain() {
  local spike
  spike="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  export MOA_BRAIN="$1"
  if [ -z "$1" ]; then
    export MOA_TMUX=moa MOA_DB="$spike/master-chat/brain.db" MOA_SCOPE="$spike/master-chat/private-scope.md"
  else
    export MOA_TMUX="moa-$1" MOA_DB="$HOME/.local/share/moa/$1/brain.db" MOA_SCOPE="$HOME/.config/moa/$1.md"
  fi
}
