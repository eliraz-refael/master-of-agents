# Backlog: improvements over the spike

The user's list of what moa should do better than the spike. It's sorted by when each item
lands:

1. **Already built, or touches built work.** A change to done work is asked about first.
2. **Goes into a later slice's plan.**
3. **After parity,** meaning after slice 10, the switch-over.

The roadmap (`superpowers/plans/2026-11-15-moa-v1-roadmap.md`) stays the plan of record. This
file says where each item goes; the slice's own plan says how.

## 1. Already built, or touches built work

### Search: find any session fast (must have)

**Asked for:** the spike's search, which finds a session by typing part of it.

**Status: built in slice 5c.**
- `Ctrl-a /` opens it as a popup over whatever you're looking at.
- `Ctrl-a s` opens it as a tab of its own.
- `[ui] picker = "tab"` swaps the two keys.
- Matching works like the spike's:
  - each typed word's letters must appear in order;
  - it searches the title, project or group, state, folder name and session id;
  - whole-word hits rank first.
- Open plain terminals are listed too.
- `moa pick` runs the same search in any terminal.

**Decided (11-21):** searching by name is enough. Searching *inside* conversations is listed
under 3 as an idea only, not a request.

### Terminals: open, find and close them without the mess

**Asked for:** other tools leave many terminals open for no clear reason, and you can't find
the one you want. moa needs a better way to open a terminal, find it, and end it.

**Status: built in slice 5d** (#80–#83).
- **Every terminal is recorded:** who opened it (`you`, or `found` when it was already
  there when moa first looked), its folder, its session, when it opened and what's running.
  Splits count, and so do plain tmux windows.
- **Last touched:** moa stamps a terminal when you switch to it and when you leave it.
  Idle ones are marked (`terminal_idle_hours`).
- **`moa term ls`, `name`, `note`, `close`, `timeout`.** A close checks what's running at
  that moment. At a prompt, the terminal closes at once. Running a program, it gets one
  Ctrl-c, and the pane is killed 3 s later.
- **The terminals view, on `Ctrl-a T`:** one row per terminal, and keys to jump, close,
  rename, note and set a timeout. The picker shows a terminal's folder and program.

**Built in slice 6d:**
- **Opened by moa** (`opened_by = moa`): a terminal moa's own connection opens.
- **Idle terminals** come to moa with the day's opening and wake-ups; it may suggest closing
  one that looks forgotten, and never closes one unasked.
- **Closing or archiving a session offers to close its terminals:** one suggestion, with a
  `term close` per terminal. A close kills only the agent's pane; its splits outlive it.

**Still open, and where it goes:**
- **Pruning closed records:** with the retention decision (Q1).

## 2. Goes into a later slice's plan

Nothing waiting here now. The terminals entry's open items above say which slice each goes to.

## 3. After parity (after slice 10)

### Worktrees: know what you have, who owns it, and what's abandoned

**Asked for:** today there's no way to see:
- how many worktrees exist;
- who owns each one;
- which are active and which are abandoned;
- how much disk space they take.

moa should tell you.

**Already designed (spec §6.17, call C23; built after the switch-over):**
- a `workspaces` record for each worktree:
  - path, branch and base;
  - who created it (moa, an agent, or you);
  - whether it's locked;
  - the last clean check;
- each worktree linked to its tasks and sessions;
- ending a worktree as a choice on the task: finish, abandon or park;
- a daily sweep that removes only worktrees that are ours, ended, clean, pushed or merged,
  unlocked and old, moving them to trash first, and shows the rest as a card.

**Added by this request:**
- **Disk space** per worktree, and the total. The spec's model doesn't have it yet.
- **One list** of every worktree, including ones moa didn't create. Each row shows:
  - owner;
  - linked session and its state;
  - last commit and last activity;
  - whether it's pushed or merged;
  - size.

  Each row is marked active, idle or abandoned.

### Idea only: search inside conversations

This is not asked for. Searching by name is enough for now. If it's ever wanted, moa wouldn't
need to keep its own copy of everything. Claude's own transcript files are already on disk
and could be searched in place. The catch is that Claude deletes old transcripts after a
while (30 days by default), so old conversations would drop out of the search unless moa kept
copies. That ties in with the retention question (Q1).
