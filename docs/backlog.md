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

## 2. Goes into a later slice's plan

### Terminals: open, find and close them without the mess

**Asked for:** other tools leave many terminals open for no clear reason, and you can't find
the one you want. moa needs a better way to open a terminal, find it, and end it.

**What moa has today:**
- `moa term [PATH]`, `Ctrl-a $` or the console's `/term` opens a shell tab named `⌨ name`.
  moa marks it as a terminal.
- `Ctrl-a |` and `-` split an agent's tab with a shell in its folder.
- The picker lists open terminals, and Enter jumps to one.

**What's missing:**
- **Why it exists.** Record who opened each terminal (you, an agent, or moa) and for which
  session or project, so no terminal is a mystery.
- **Finding.** Terminals get a place of their own: a terminals view, not just rows mixed into
  the picker. Each row shows:
  - its folder;
  - the session or project it belongs to;
  - its age;
  - when it was last used;
  - **working or idle.**
    - *Working:* a program other than the shell is running in it.
    - *Idle:* just the shell prompt, or a program that has printed nothing for a while.

    tmux already knows both: the pane's current command and its last activity.
- **Ending.**
  - A key to close a terminal from that list.
  - When a session is closed or archived, offer to close its terminals.
  - Idle terminals (only a shell, nothing running, unused for a while) are listed for
    cleanup. moa never closes a terminal with something running in it without asking.
- **Opening.** A new terminal opens in the folder of the agent you're on, already linked to
  that session, as `Ctrl-a $` does today. The difference is that it's recorded, so all of the
  above works.

**Where:** a small plan of its own, right after slice 5c and before slice 6 (decided 11-21).
The user wants terminals to have their own place, and this is a daily pain.

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
