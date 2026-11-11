# What the spikes taught us (feeds the design doc)

## master-chat spike (2026-11-10)
- The memory loop works: the master briefed itself from brain.db and recalled Lantern
  (with its resume command) with nothing typed in.
- Unprompted, it registered a project it had never seen (net-lapraklit, nested in
  `~/private/projects/`) and linked the tracked session to it.
- "What's running?" took it 9 shell commands (pgrep, lsof, ps, jq over transcripts).
  The brain needs a live session registry owned by the daemon, not discovery by the agent.
- Private projects (questline, wimm-new) run on `claude-work`, so the brain is chosen by
  project folder, not by account.
- A chat alone can't show what the product feels like: Eliraz needs the picture beside it
  and the aoe/herdr side to judge it.

## moa-proto v0 (2026-11-10): master left, live picture right, agents in tmux windows
- First reaction: "a very strong vibe". Seeing all the projects known from day one and
  "waiting on you" were the highlights.
- Seeding from git state is what made "it knows all my projects" land, which supports
  backfilling on first run.

## Session tree (2026-11-10, after v0)
- Eliraz loves aoe's nested session tree (copied it into herdr-sessions: 43 sessions,
  groups like Private/Neeo and Work/…). Creating sessions is fun; **maintaining the
  tree is the chore**: deleting finished ones, regrouping, keeping focus.
- Eliraz wants to talk to the master in plain words: "open a session on ~/private/my-idea,
  I'll continue there", "what project needs attention first?", and have it open
  sessions itself.
- Conversation IDs surviving restarts is essential.
- Eliraz is unsure what they want overall; small runnable versions with feedback beat
  up-front questions.
- v1 test: import the herdr-sessions tree, show it nested, give the master `moactl`
  (open/resume/close/archive/ls), and make tree upkeep the master's job.
- The tree has to be *visibly* nested (guides, subgroups), and the screen should feel
  alive: animated working state, pulsing "waiting on you", new activity flashing.
  "Fun" is a requirement, not polish.
- The master tidied the tree on its own (filed net-lapraklit under Praklit as "Site
  intro", named Tamarouge and Lantern's "Rust scaffold").

## Dogfooding v1 (2026-11-11)
- Eliraz is dogfooding it on a real project: "so far, it's not bad". tmux feels powerful.
- **A plain Claude session is the wrong master**: it's passive, and has to be told to
  look. Wanted: a central *console* in the TUI with commands plus a free input line,
  speaking first, that "knows everything on my other running sessions". This settles
  the earlier (a)/(b) question in favor of (b), the app's own chat view.
- SQLite is right (the app and agents both query it), but agents should go through a
  **fetcher layer / brain API**, not raw SQL. That layer is also where the trust ladder
  and brain isolation get enforced.
- Wants to be told explicitly when a restart is needed.
- ACP is on the table: `claude-agent-acp` is already installed via Nix (Emacs agent-shell uses it).

## v2: console over ACP + brain API + tree editing (2026-11-11)
- `claude-agent-acp` (0.71, via Nix) on CLAUDE_CONFIG_DIR=~/.claude-lead works: the
  project's CLAUDE.md and hooks load, threads resume by ID (`session/load` replays them),
  and permission requests arrive as `session/request_permission`.
- The old briefing (11.6 KB) was too big; Claude cut it to a 2 KB preview. A compact
  brief from the brain API (~2 KB) plus on-demand verbs (`attention`, `project`, `find`)
  works better.
- The console speaks first: on a new daily thread it asks the master for an opening brief.
  First real one: hot / waiting on you / stale, then 3 proposed goals.
- Tree editing needs empty groups to exist, so groups got their own table, not just
  `sessions.grp`.

## Not a tmux user (2026-11-11)
- Eliraz doesn't use tmux and would have to learn it. **tmux must be invisible
  plumbing**: moa's own keys, discoverable from one menu key, with no prefix chords to
  memorize. tmux features (join-pane, popups, menus) are implementation details.
- The console died silently once (no logging). Every pane must log its errors and
  survive a bad message, and there must be an obvious way to bring a pane back.

## Cockpit layouts (2026-11-11)
- Shown three layouts (cockpit, herdr-style focus, polished classic), Eliraz liked all of
  them: "can we have multiple views and change with a config or a keystroke?" → **the
  cockpit is assembled from panels**, layouts are config, and moa can edit them.
- tmux makes this cheap: each panel is a pane tagged with @moa_panel; a live agent
  session joins the cockpit by swap-pane, keeping its process. Rebuilding a layout moves
  the console instead of restarting it.
- The central agent is named **moa**, not "master" (confusing next to git).
