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
