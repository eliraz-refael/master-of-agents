---
name: moa-peers
description: Talk to the user's other agent sessions (Claude, Codex, …) and to moa, the agent that keeps the big picture across all their sessions (projects, tasks, who's working on what). Use when the user asks you to ask, tell, consult, hand something to or coordinate with another session, agent or moa; when they want a second opinion from a peer; or when another session clearly knows something you need.
allowed-tools: Bash(moactl peers) Bash(moactl peers *) Bash(moactl ask *) Bash(moactl reply *) Bash(moactl asks)
---

# Talking to other agents through moa

moa (master-of-agents) runs the user's agent sessions and keeps the big picture. `moactl`
is on your PATH.

## Who's there

```bash
moactl peers          # moa, plus every live session: ID, name [group], state, folder, what it was last asked
moactl peers --json   # the same, as JSON
```

`moa` itself is always a peer. Ask it about the big picture: what other projects are
doing, what's waiting on the user, decisions made elsewhere. Tell it things it should
know, like a new project or a fact other sessions need.

## Ask

```bash
moactl ask NAME-OR-ID "message"
```

- It waits until the peer is free, types your message into its session (marked as from
  you, with an ask number), waits for its turn to end, and prints its reply.
- **It can take minutes: run it with a 600000 ms timeout on the Bash tool.** It gives up
  after 9 minutes; `moactl reply ID` waits again for the same ask. `moactl asks` lists
  recent ones.
- Name a peer by the ID `moactl peers` shows (safest), its title or its project.
- Write the message for someone without your context: say what you need and why, and
  include the facts it needs. One clear question gets a better answer than several.
- Don't ask a peer that's waiting on your answer; answer it first.

## When a peer asks you

A message that starts with `[moa · message from NAME · ask #N]` comes from another agent,
not the user. Your final message that turn goes back to it as the answer, so end with
a complete answer. Don't ask it back in the same turn: it's waiting on you.

## If moactl says moa isn't available here

On some accounts moa only works in some folders. Tell the user; don't work around it.
