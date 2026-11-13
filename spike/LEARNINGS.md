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

## The console needs structure, not a transcript (2026-11-11)
- Plain streamed text from moa read as a wall: "not decorated enough… I would want to
  see it more structured, and the / commands as buttons". The console now renders
  Markdown (headings, lists, tables, quotes, code), groups each moa turn under one
  header with its tool calls collapsed, and puts command output in cards.
- **Next steps are buttons.** A command in backticks (`/resume ID`, `/focus Q`) becomes
  a button you click or reach with Tab; session events carry resume/answer buttons; a
  button bar sits under the input; typing `/` opens a command list that also completes
  session titles and layout names. moa is told to end suggestions with the button.
- For this, the brain API grew structured output (`moactl ls --json`): a UI shouldn't
  parse column text.

## The big picture is moa's job, fed by session reports (2026-11-12)
- The Today and Activity panels repeated the tree. Eliraz: "I don't have a clear vision
  yet", but wanted "a picture being built… by project/session of tasks that need to be
  done", reported by the other agents. Trying it as a spike to find out.
- Sessions already say what's next at the **end** of their final message; the hook kept
  only the first 500 characters. It now keeps the start and the end.
- **moa wakes on its own**: the console batches session reports (45 s to settle, at most
  one wake every 3 min) and sends them as a hidden prompt; moa updates tasks per project
  (who: you / agent / other, due dates, linked session) and replies in one line unless
  something needs Eliraz. Each wake is an LLM turn: the cost to watch.
- The **plan** panel replaces Today in the presets: per project, what's on you, what
  agents are doing, what waits on someone else; projects needing you sort first.

## Looks and moving around (2026-11-12)
- "It looks way better… starting to feel like an app." Next ask: thicker borders and
  titles on them, like aoe. tmux does both natively: `pane-border-lines heavy` and
  `pane-border-status top` with a format reading a per-pane label (@moa_label).
- Moving between panes had no key (only the mouse). Ctrl-a became a key mode instead
  of a menu: arrows move (and repeat), letters jump to panels, z zooms, Space or a
  second Ctrl-a opens the menu, and the status line lists the keys while it's held.
- moa is an app that uses tmux (its own server, config and keys), not a tmux plugin.
- Testing note: panes started by a scratch tmux server inherit that server's
  environment, not the shell's: a test cockpit reached the real moa account. Set
  PATH/MOA_* with `set-environment -g` before building panes.
- Second pass ("spacing is tight… rounded corners… more shiny"): tmux can't round pane
  borders, so borders became blank gutters (`pane-border-lines spaces`) and moa's panels
  draw their own rounded frames. The frame is drawn in the *default* colour and tmux
  colours it per pane (`window-style` / `window-active-style` as pane options), so focus
  shows without any events. Focus reports (`focus-events`) were tried first: curses maps
  them to kxIN/kxOUT, but tmux only reports focus-out to panes it saw gain focus, which
  swap-pane and startup races break. The status bar got a spacer line and rounded pills.

## Setup has to be one step (2026-11-12)
- "When a user downloads this, you don't expect them to write a Python script to install
  the hooks." moa now finds the Claude accounts itself (config folders with projects/ or
  settings.json in $HOME), shows which have its hook, and asks per account: install,
  only under some folders, or not now. In a terminal: `moactl setup`; in the console, a
  card with buttons appears while an account isn't tracked (and `/setup` brings it back).
- The hook commands carry a marker, so moa can find, update and remove only its own
  hooks and leave the others (herdr, aoe) alone; the first write keeps a backup.

## moa briefs; it doesn't relay (2026-11-12)
- "moa master should not act as a replicant of the session he follows": the console
  printed every finished turn, and moa's wake-up reply repeated it. Now a session's
  turns fold into one line ("TS-BOOK · 6 turns since 21:10") and moa's reply is the
  brief: one line on what's going on there, in its words, then only what's on Eliraz.
  Q&A turns ("is this price OK?") tell moa what Eliraz is focused on; they aren't tasks.
- moa inventing follow-up tasks from what it knows should happen next ("order author
  copies once the book is live") was the moment it felt useful: "freaking awesome".
- Due dates have to be data, not text: moa wrote "due around Nov 15" into a title. Now
  `--due` is a column on the big picture (red overdue, yellow today), moa works dates out
  from reports ("review takes 72h" → +3), and wake-ups carry what's due, so moa nags.
  No timer: an active moa wakes often enough from session reports.
- A shell variable (`M=moactl; $M …`) dodges permission rules, which match the command
  as written, so every call asked for approval.

## Colours: exact and lighter (2026-11-12)
- Tokyo Night read as too dark and low-contrast. Nord, lightened (dim text #8c9ab3,
  light frost blue #88c0d0 as the accent), reads better.
- Part of the murk was the panels: curses was mapping the hex palette to the 256-colour
  cube, which turns Nord's blue-greys neutral and its green olive. ncurses can do direct
  colour with the `tmux-direct` terminfo (colour number = 0xRRGGBB), and tmux passes RGB
  on to terminals it knows have it (tmux.conf marks Ghostty; other terminals need a
  `terminal-features` line), so the panels now use the exact hex colours. Only curses
  starts on tmux-direct: TERM goes back before anything runs, since the system terminfo
  lacks that entry and child processes (the agent's shell) would break on it.

## Partners: agents talk through moa (2026-11-13)
- The trigger: another session had something for moa and no way to reach it, so it sent it
  to a design session instead. Agents need an address book and a way to talk.
- What made herdr easy for agents (vs aoe, where they had to work out a protocol): every
  pane has a name and a state, `agent prompt X --wait` sends and waits, and the CLI's help
  speaks to agents. moa does the same: `moactl peers`, `moactl ask NAME "…"` (types into
  the peer once it's idle, waits for its turn to end, prints its full final message), and
  the SessionStart hook tells every session moa starts that this exists. Tested: a fresh
  session told only "ask Peer B to pick a number" found `moactl peers` and asked, in 8 s.
- Asking moa itself works the same way (`moactl ask moa`): the console puts the message
  in moa's thread as a card and hands moa's reply back.
- Asks are rows in the brain (who asked whom, what, the answer), which is what moa needs
  to broker pairings later.
- Codex has Claude-style hooks now (~/.codex/hooks.json, each hook trusted once), so the
  same hook can track it; not installed yet.

## Thirty agents (2026-11-13)
- "What happens when I have 30 agents?" A tab per window overflows at about six. The
  status line now names only what's urgent (agents blocked on a permission prompt, or
  that asked a question) after a pill for the window you're in, and counts the rest
  (`⚠ 2  ? 2  ✓ 3  ◐ 4  ● 19`). tmux's window list can do this with no scripts: a window
  format that expands to nothing hides that window, and the current window moved to
  status-left so it stays first.
- Finding a session became typing: Ctrl-a / opens a picker popup (part of a title, group
  or folder; Enter jumps to it or resumes it), live ones first by urgency. fzf isn't
  installed here; a small curses list in moa's palette was enough, and it can animate
  the state glyphs.
- workmux's dashboard and sidebar sort agents waiting > done > working > stale, and its
  ✅ "done" clears when you focus the window. "Finished since you looked" is the useful
  state at scale, more than idle. moa now shows five: needs you, asks you (the reply
  ends in "?"), done (unseen), working, idle. "Seen" is a pane option set while the pane
  is on screen; a tmux hook touches a file on window and pane switches, so panels notice
  without polling.
- The hook's "waiting" was noisy: Claude sends a Notification after a minute at the
  prompt ("waiting for your input"), so every finished session turned ⚠. Only permission
  prompts count as needing you now.
- The status panel is its own panel (the triage layout, Ctrl-a 4), not a mode of the
  tree: the tree answers "where is it" (by group, every session, editing), the status
  panel answers "what needs me" (by urgency, live only).
