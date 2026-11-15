# Spike: master chat (THROWAWAY)

This tests whether a central "master" chat feels good: a normal Claude Code session
whose memory is a SQLite file, briefed at start, and told what tracked sessions did
since your last message. It isn't product code. Delete this folder when the spike is
done.

## Run

    ./seed.sh                  # create brain.db from ~/private git state (--reset to start over)
    cd master && claude-lead   # talk to the master

## Make a session report to the master

    MOA_TRACK=1 claude-lead    # any folder; without MOA_TRACK it behaves as today

## Undo the hook

`hooks/track.sh` is registered in `~/.claude-lead/settings.json` (entries whose command
contains `moa-spike`). The backup from before it was added is
`~/.claude-lead/settings.json.bak-moa-spike`. Restore it, or run:

    jq 'walk(if type=="array" then map(select((.hooks? // [] | map(.command) | join(" ") | test("moa-spike")) | not)) else . end)' \
      ~/.claude-lead/settings.json > /tmp/s && mv /tmp/s ~/.claude-lead/settings.json

## More than one moa (brains)

Each brain is its own moa: its own tmux server, `brain.db` and scope (what it's for).
None sees another's data. `moa` alone is the default brain, kept here in the repo. A
named brain lives outside the repo, so its data can't be committed:

    moa work --account claude-work      # first run: creates the brain, then opens its cockpit
    moa work                            # every time after (the account is remembered)

| What | Where |
|---|---|
| the brain | `~/.local/share/moa/work/brain.db` |
| what this moa is for (edit it) | `~/.config/moa/work.md`, read at the start of every thread |
| its settings (`MOA_ACCOUNT`, `MOA_OPEN_ACCOUNT`) | `~/.config/moa/work.env` (`default.env` for the default brain) |

`--account` takes an account's command name or its config folder (`~/.claude-something`).

Sessions belong to the moa that opened them. To send an account's sessions to a brain
wherever they start, and keep some folders with the moa that opens them:

    moactl setup install claude-work --brain work --except ~/private ~/.config

| claude-work session started in | goes to |
|---|---|
| `~/private/…`, `~/.config/…` | the moa that opened it (the default moa, unless `moa work` did) |
| anywhere else (`~/workspace/…`, `~`, a clone in `/tmp`) | the work moa |

Anything not listed as an exception is the brain's, so its data can't reach the default
brain from a folder you forgot to list. `moactl setup list` shows each account's route.
