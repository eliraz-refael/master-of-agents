# Slice 5d: terminals, design

Short design, agreed in conversation on 2026-11-21. It builds on the main spec
(`2026-11-15-moa-rust-design.md`), its "Plain terminals" rules, and the backlog's "Terminals"
entry (`docs/backlog.md`).

## Why

In other tools, terminals pile up: many are open for no clear reason, and you can't find the
one you want. moa is the pilot and the user is the captain. moa should know about every
terminal opened inside it, so it can report on them and, once it asks and gets a yes, act on
them. 5d gives moa that knowledge and the means to act. The judging comes later.

## What 5d builds

1. **Every terminal is registered when it opens.** A terminal is any shell in moa's tmux that
   isn't an agent or a panel. That covers:
   - `moa term` and `Ctrl-a $` tabs;
   - the shells from `Ctrl-a |` and `-`;
   - ones opened with plain tmux keys.

   Each gets a record in moa's database:
   - who opened it: you, moa at your request, or *found* (moa first saw it already open);
   - its folder;
   - the session it belongs to (for a split, the agent whose tab it was split from);
   - when it opened;
   - when you last touched it;
   - its name, a note, and a timeout, if you set any;
   - when it closed. A closed terminal's record stays, as history.
2. **A terminals view.** One list of every open terminal. Each row shows:
   - its name;
   - its folder;
   - what's running in it now (tmux's current command), and since when moa first saw it;
   - how long it's been open;
   - when you last touched it.

   Anything untouched for **2 hours** or more is marked. Enter jumps to the terminal.
3. **Commands to act on a terminal,** from the view and from the command line
   (`moa term ls|close|name|note|timeout`):
   - **close.** If a program is running, moa stops it gently first, and only closes the
     terminal by force if the program won't stop. In the view, closing one with something
     running asks y/N first.
   - **name / note.** Say what a terminal is for, such as "dev server for checkout, keep".
   - **timeout.** Close it after a time you choose, such as 3 h. You can clear it. When it
     fires, moa closes the terminal as above and says so.

## Not in 5d

- **The sweep.** Every few hours, moa sends an agent to look at each terminal once and decide
  what needs nagging, naming or closing, and that agent reports to you. That's slice 6 or 7,
  once moa itself exists. It acts through 5d's commands, so 5d needs no change for it.
  Whether moa or a dedicated terminal agent does the judging is decided then.
- **Reading terminal output.** 5d never reads it. Only the later sweep looks, once per run.
- **Autonomy** (moa acting without asking). That's a general question for slice 6.
- **Terminals outside moa's tmux.**

## Must never happen

- moa closes a terminal you didn't ask it to close. In 5d the only automatic close is a
  timeout you set yourself.
- A terminal costs moa anything ongoing just for being open: no polling of output, no
  per-terminal process.

## Approaches considered

- **A. Snapshot every terminal's screen every few minutes, so moa's code can summarise its
  output.** Rejected by the user: it's constant work across many terminals for something only
  the later sweep needs.
- **B. Stream every line of output to moa.** Rejected: far too much for what's needed.
- **C. No database; keep notes and timers as marks on tmux panes.** Rejected: they'd vanish
  when tmux restarts, and moa would have no history.
- **Chosen:** register on open, keep records in the database, and build the view and the
  commands. Judging is a later sweep, on a schedule.

## My calls (for the user to check)

- **"Touched" means** the terminal was the active pane in a window you were looking at. moa
  already hears these switches from tmux. Typing inside it while it's on screen counts as long
  as it stays the active pane.
- **"Running since"** is when moa first saw that command in the terminal. tmux doesn't report
  a program's start time.
- **The 2-hour mark is a setting,** defaulting to 2 h.
- **Closing gently** means sending Ctrl-c, waiting a few seconds, then closing the pane.
- **Closed terminals' records are kept.** Pruning them waits for the retention decision (Q1).
- **The view's key and where it sits in the menu** are picked in the plan. A key that's already
  taken is never reused.

## Left to the plan

- The schema migration.
- How registration hooks into the tmux events moa already reads.
- The timeout timer, reusing the daemon's existing deadline machinery (as sleep does).
- The view's layout at narrow widths.
- The e2e tests on real tmux, on 3.4 and 3.8.
