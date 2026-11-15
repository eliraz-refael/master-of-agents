# 5. Prior art: agent/session managers, and what moa should take from them

*Research for the moa design phase, 2026-11-15. Sources were read at these commits
(shallow clones in the scratchpad): agent-of-empires `5b68460`, workmux `e76bb06`,
herdr `ee5bfb5`, claude-squad `070eb93`; herdr-sessions at `961fc3f` (local). Web pages
were fetched on 2026-11-15. This file avoids machine and employer specifics so parts
of it can go into `docs/`.*

---

## 0. The short version

1. **The data model the field converged on is `Task → Workspace(worktree, branch) →
   Session(agent conversation) → turns/events`.** vibe-kanban migrated from
   "task attempts" to exactly that (`workspaces` + `sessions` tables); aoe split a
   "Session" (its record) from an "Agent conversation" (the native ID); Claude Code's
   own resume keeps a *worktree binding* on the conversation. moa's starting hypothesis
   is right. The one thing to fix in the moa prototype is that it keys sessions by
   conversation ID, which rotates.
2. **Conversation IDs need a stable owner record and a rotation history.** IDs change
   on `/clear`, fork and compact. herdr-sessions and aoe both keep a stable ID of
   their own and update the conversation ID each time the agent reports a new one.
3. **The account belongs to the conversation, not only to the session's profile.** In
   herdr-sessions' live data, one record labelled `acct-a` points to a
   conversation whose transcript exists only in the acct-b account's config dir. aoe
   says outright that it "never scans a shared store or infers an identity from
   recency".
4. **Claude Code deletes transcripts after 30 days by default** (`cleanupPeriodDays`).
   None of the five Claude config dirs on this machine sets it. 6 of the 43
   herdr-sessions records point at transcripts that are gone, and all 6 have been
   idle for more than 30 days. "Every conversation resumable by ID" is false today
   unless moa manages this setting.
5. **Worktree cleanup has to be a sweep the system runs, not a question at delete
   time.** aoe asks "delete worktree?" when you delete a session, and has a manual
   `aoe worktree cleanup`, and still left 53 stale worktrees behind. Claude Code's
   model is the one to copy: a marker in the worktree's git metadata, a
   `git worktree lock` while an agent runs, and an age-based sweep that removes only
   clean, pushed, unlocked worktrees.
6. **Status comes from hooks, not screen scraping.** Keep a "done, unseen" state that
   clears when you focus the session (workmux, herdr, Claude's agent view), and sort
   by urgency: blocked > done-unseen > working > idle/stale.
7. **The fastest tools launch nothing per check.** aoe still forks
   `tmux capture-pane` every 25–250 ms per previewed pane and repaints every 33 ms.
   herdr reads its terminals in-process. workmux's sidebar is a daemon that writes one
   runtime file.
8. **Claude Code now ships much of this itself:** `claude agents` (agent view +
   supervisor), `claude --bg`, `--worktree`, cross-session messaging over a
   per-session socket, and `claude agents --json`. moa should build on these where
   they fit (the messaging socket in particular), not duplicate them. It also has to
   cope with them being **per `CLAUDE_CONFIG_DIR`**: each account is a separate
   supervisor with its own sessions.

---

## 1. herdr-sessions (the user's own Go tool)

Source: `~/private/herdr-sessions/DESIGN.md`, `SPIKE.md`, `internal/{store,core,config,aoe}/*.go`.

### Data model
- **Profiles** (`~/.config/herdr-sessions/config.toml`): `kind` (claude|codex),
  `command` (the launcher you type, e.g. `acct-b`), `config_dir` (where the
  transcripts live), `extra_args`. Resume syntax depends on `kind`:
  `<cmd> [args] --resume <id>` for Claude, `<cmd> [args] resume <id>` for Codex
  (`core.LaunchCommand`).
- **Session record** (`~/.local/state/herdr-sessions/sessions.json`, `version: 1`):
  ```
  id (own, stable; aoe ids kept on import), title, group ("A/B" path), path,
  profile, extra_args[], conversation_id, worktree{branch, repo, managed}|null,
  binding{workspace_id, tab_id, pane_id}|null, live, agent_status,
  created_at, last_active_at, last_error
  ```
  `groups[]` holds `{path, collapsed}` so the picker keeps its order and empty groups
  survive.
- **Concurrency:** writers take an `flock` on `sessions.lock`, write a temp file and
  rename it; readers never lock (`store.Update`). There is no daemon.

### What it does well
- The session is the **app's** record: the app spawns the agent, remembers the
  conversation and resumes it, all with the right launcher for the account. The user
  liked this most about aoe.
- **Opening is idempotent:** if the pane is live, focus it. If the bound tab still
  exists, resume in that same tab. Otherwise create a tab (`core.Open`).
- **Adoption** (`core.adopt`): a session with no binding gets bound to a pane that
  already runs its conversation, so opening it never starts a second agent on the same
  conversation. moa needs the same guard.
- **Resume checks the transcript first** (`Profile.HasTranscript`): it resumes only if
  `<config_dir>/projects/*/<id>.jsonl` exists, and starts fresh otherwise.
- The aoe import runs a dry-run report per session: folder missing, no conversation
  ID, transcript not found.

### Pitfalls it found (§3, §11, SPIKE.md), all of which apply to moa
- **Built-in resume is wrong for multiple accounts.** Herdr hard-coded
  `claude --resume <id>` and lost the pane's environment on restore, so the wrong
  account would resume. Fix: turn off the host's auto-resume and own resume yourself,
  using the profile's launcher.
- **Hooks are installed per config dir.** A Claude hook installed in `~/.claude` does
  nothing for `CLAUDE_CONFIG_DIR=~/.acct-b`. Install into every account dir, plus
  Codex's `~/.codex/hooks.json`, which needs a one-time trust step. A `doctor`
  command should check this.
- **Capture the conversation ID while the agent is live.** Herdr cleared
  `agent_session` when the agent exited, so the ID had to be saved on every status
  event, not at stop.
- **Codex has no ID until the first prompt** (the same is true after resume). A new
  Codex session shows "no conversation yet".
- **First-run prompts belong to the user.** Claude's folder-trust prompt *defaults to
  "No, exit"*, and Codex has its own trust prompts. Never auto-answer them. Every new
  worktree path is a new folder that needs trust (see §9).
- **Env vars set on panes don't survive a restore.** Don't depend on them for identity
  after a restart.
- Events fire for every multiplexer session the plugin is loaded in, so handlers must
  filter by socket or instance.

### What failed
It inherited Herdr's model mismatch (spaces vs sessions) and mouse-first UI. It never
had a "task" or "project" level above the group path.

### State of its records today (a migration input; counts only)
43 sessions in 14 groups (group depth 1–2). Profiles: acct-work 21, acct-b 14,
codex 5, acct-a 3. 42 have a conversation ID. 13 are managed worktrees: 12 at
`../{repo}-worktrees/{branch}` and 1 under `.claude/worktrees/`. 19 still have a
binding (Herdr pane IDs, now meaningless). 3 paths no longer exist. I checked only
whether transcript files exist, not their contents: 35 found; 6 acct-b missing,
all idle for more than 30 days (retention sweep, see §0.4); 1 acct-a record
whose transcript is in the acct-b account (wrong account recorded).

---

## 2. aoe / agent-of-empires (Rust)

Sources: https://github.com/agent-of-empires/agent-of-empires, `CONTEXT.md` (glossary),
`DESIGN.md`, `docs/adr/0003…`, `docs/adr/0004…`, `docs/guides/worktrees.md`,
`docs/guides/session-resume.md`, `src/tui/app.rs`, `src/tui/home/live_send.rs`,
`src/session/{storage,worktree_reconcile,idle_reap,deletion,purge_owners}.rs`.

### Data model
- **Profile** (an isolated set of sessions, projects and settings) → **Group**
  (slash path; can be archived; Archived, Trash and Scratch are synthetic groups) →
  **Session** (title, project, status, flags) → owns several **tmux sessions** (the
  agent, a paired terminal, tool sessions such as lazygit) and an **Agent
  conversation** (the native ID, "survives independently of the Session that wraps
  it").
- **Project** (a registered directory with defaults such as base branch), kept apart
  from **Repository** (the git object) and **Working directory**. **Worktree** =
  branch + checkout created or attached for one session. **Workspace** = several
  repos on one shared branch name.
- "Do not bother me" states: **Archive**, **Trash** (soft delete), **Snooze** (until a
  time), plus **Pin** and **Favorite**. **Unread** = output you haven't looked at.
- Storage: `~/.agent-of-empires/profiles/<p>/{sessions,groups}.json` with sidecar
  flocks and a long lock-ordering rulebook (`storage.rs` header). It is moving to
  **a mandatory daemon as the only writer** (ADR 0004: "out-of-band write is …
  the race class of #2541"; the CLI auto-starts the daemon and talks to it over a
  Unix socket with peer credentials, ADR 0003).
- The web **Board** separates **Stage** (the column you put a card in, sticky) from
  **Signal** (a transient, rule-raised condition such as a pending approval or changes
  requested on a PR, which overrides the column while active).

### What it does well
- **The app owns the agent:** sessions survive, groups nest, and each account is a
  custom agent with its own `agent_config_dir`.
- **A strict resume contract** (`session-resume.md`). The conversation ID is captured
  per pane through the agent's own hook, together with the store (config dir) it
  belongs to. The doc says: "AoE never scans a shared store or infers an identity from
  recency". A recorded store outranks the current config. A failed resume probe sets
  a durable `resume_failed` marker, so the next restart starts fresh instead of looping.
- **Worktree hygiene came late but is thorough:** trash *moves* the checkout to
  `.aoe-trash/<id>` (so restore works), purge renames before removing, the default
  branch's checkout is never removed, dirty worktrees survive purge without force, and
  paths are reconciled against `git worktree list` by branch when a directory moved
  (`worktree_reconcile.rs`: Current / Moved / Missing / Ambiguous).
- A glossary (`CONTEXT.md`) with an "Avoid:" list for every term. Worth copying for
  moa's docs: "session" vs "tmux session" vs "agent conversation" is exactly the
  confusion moa will have.
- Idle reap (stop sessions idle past a threshold), smart rename (title derived from the
  work), and a `aoe serve` web dashboard with push notifications.

### What fails at scale
- **A process launch per poll.** The TUI repaints on a 33 ms interval
  (`src/tui/app.rs:673`). The preview worker forks `tmux capture-pane` every 25 ms
  while live-sending and every 250 ms when idle (`live_send.rs:652–712`), with a
  10 s "authoritative capture" on top. Status still falls back to parsing pane text
  (`tmux/status_detection.rs`) for agents without hooks. Cost grows as agents × rate,
  and on this machine each exec costs 5–10 ms. The tmux server is single-threaded, so
  your keystrokes queue behind the captures.
- **Two writers to a JSON file** (CLI and TUI/daemon) caused stale-reload and race bugs
  (#2541, #4035). The answer was the daemon-only-writer ADR, years in.
- **Worktrees outlive sessions.** Removing a worktree is a prompt at delete time, and
  orphan cleanup is a manual command (`aoe worktree cleanup`). Nothing sweeps, so 53
  stale worktrees filled the disk.
- **Complexity has run away:** 20+ agents, sandboxes, ACP workers, plugins, a
  multi-tenant "CityHall". The resume guide alone is pages of per-agent caveats. The
  web UI is React with an npm build, which is incompatible with moa's no-npm
  constraint.

### Borrow
The session ≠ conversation split; the resume refusal rules and `resume_failed` marker;
trash-by-move for worktrees; never removing the default branch's checkout; reconciling
by branch; Stage vs Signal; Snooze; a glossary with "Avoid:" lists; a daemon as the
single writer from day one.

### Avoid
Per-pane polling with a subprocess; a 33 ms repaint ticker with no events; worktree
deletion that hangs on a prompt the user can skip; JSON plus a thicket of flocks;
supporting every agent CLI.

---

## 3. herdr (Rust, its own multiplexer)

Sources: https://github.com/herdrdev/herdr, `skills/herdr/SKILL.md`,
`docs/next/website/src/content/docs/{concepts,session-state,add-herdr-support,cli-reference}.mdx`.

### Data model
Server **session** (namespace) → **workspace** (project container, rolls up agent
state) → **tab** (layout) → **pane** (a real terminal). An **agent** is whatever
recognised process currently occupies a pane. Its name is "cleared when that agent
exits". IDs are opaque and never reused (`w1`, `w1:t1`, `w1:p1`). Restore state lives
in `session.json` (+ 48 rolling `session-snapshots/`, failed-load backups, opt-in
`session-history.json` for screen replay). Worktrees are workspaces "with git checkout
provenance", grouped with their parent repo workspace. `worktree remove` never deletes
the branch.

### What it does well
- **Fast:** it reads its own PTYs in-process (it embeds a VT parser), with no
  subprocess per check.
- **An agent CLI built for agents:** `agent start NAME --kind codex`,
  `agent prompt NAME "…" --wait`, `agent wait --until blocked`, `agent read --source
  recent-unwrapped`. `--wait` requires working/blocked activity within 5 s and
  otherwise returns `agent_prompt_stalled`. It refuses to type into an agent that is
  `blocked`. JSON on every response, and a skill that teaches agents the CLI. This is
  what made agent-to-agent messaging easy (moa's `moactl ask` already copies it).
- **A reporting protocol for agents** (`pane report-agent --state … --seq N --
  <resume argv>`). The agent reports its state *and its own resume command*, and a
  monotonic `--seq` drops reports that arrive late or out of order.
- **States:** blocked / working / done (finished and *unseen*) / idle (seen) /
  unknown. Seen-tracking is per client.
- Careful restore: on a bad directory, keep the pane in an error state instead of
  silently moving it to `$HOME`; back up an unreadable state file before overwriting it.

### What fails
- **Two parallel structures.** Workspaces/tabs/panes are the terminals, agents are
  transient occupants, and neither one is the *work*. A "session" outliving its pane
  doesn't exist, which is why herdr-sessions had to be built.
- **Built-in resume is per pane and hard-codes the binary** (`claude --resume <id>`),
  so it breaks with multiple accounts (see §1). Its resume-argv rules even require "a
  plain command name on PATH", so the account has to be in the launcher name.
- **Mouse-first** ("Everything … reachable by mouse; keyboard bindings are an optional
  layer").

### Borrow
The agent CLI's verbs and `--wait` semantics; `--seq` ordering for hook events;
agent-reported resume argv; done-vs-idle seen-state; a single-character state glyph
vocabulary; snapshot history of the layout file.

### Avoid
Building moa around terminal topology; per-pane agent names that vanish when the
process exits (moa's names should be session names that persist).

---

## 4. workmux (Rust; git worktree + tmux window per task)

Sources: https://github.com/raine/workmux, `README.md`, `docs/src/content/docs/reference/commands/{resurrect,merge,remove,status,last-done}.md`,
`docs/…/guide/{git-worktree-caveats,dashboard/patch-mode,configuration}.md`,
`src/state/types.rs`, `src/agent_staleness.rs`.

### Data model
There are no sessions. One JSON **AgentState** file per pane under
`~/.local/state/workmux/agents/`, keyed by `PaneKey{backend, instance(socket path),
pane_id}`. Fields: `workdir, status (working|waiting|done), status_ts, activity_ts,
pane_title, pane_pid (detects a recycled pane ID), command (foreground cmd; a change
to "zsh" means the agent exited), boot_id (tmux server start time: crash vs.
intentional close), agent_kind, agent_session_id, prompt`. The worktree is the unit
of work. Config lives in the repo's `.workmux.yaml`.

### What it does well
- **Worktree lifecycle commands that clean up everything:** `merge` (merge/rebase/
  squash into main, then delete the window, worktree and local branch);
  `remove --gone` (fetch --prune, then remove worktrees whose upstream branch was
  deleted after the PR merged); `remove --all` (skips dirty or unmerged ones).
- **Files for a fresh checkout:** `files.copy` / `files.symlink` (e.g. `.env`),
  post-create hooks, and moving `node_modules` aside to delete it in the background
  so `remove` returns instantly.
- `workmux claude prune` deletes `.claude.json` entries for worktree paths that no
  longer exist. With several accounts, every config dir has its own `.claude.json`.
- **Status from hooks, no daemon.** The dashboard sorts Waiting > Done > Working >
  Stale. ✅ done and 💬 waiting *clear when you focus the window*. `last-done` jumps to
  the most recently finished agent and cycles; the cycle resets when a new one
  finishes. A stale filter hides agents with no activity.
- **Diff review in the dashboard:** a WIP diff view and a `git add -p`-style patch
  mode. `o` comments on a hunk and sends the file, line, hunk and comment to the agent.
- `status --json` is careful to say *why* a list is empty (no state vs. other context
  vs. filtered out).

### What fails
- **`resurrect` matches state files to worktrees by working directory** and resumes
  "with each agent's resume arguments", i.e. the *latest* conversation in that
  directory (Codex `--last`-style). It keeps one recovery candidate per directory,
  so two conversations in one worktree collapse into one, and the specific
  conversation ID isn't the key.
- Scoped to one repo, not global. No groups, no accounts, no notion of a project
  portfolio.

### Borrow
`merge` / `remove --gone` / `remove --all` as moa verbs on a task; `pane_pid` + `boot_id`
to tell a recycled pane from a live one (the moa prototype hit exactly this with tmux
pane IDs after restart); copy/symlink lists; deleting heavy directories in the
background; pruning `.claude.json` per account; done-clears-on-focus; `last-done`;
comment-on-hunk sent to the agent.

### Avoid
Resuming by directory instead of by conversation ID.

---

## 5. Claude Code's own features (2026)

Sources: https://code.claude.com/docs/en/worktrees, https://code.claude.com/docs/en/agent-view.md,
https://code.claude.com/docs/en/cross-session-messaging.md, https://code.claude.com/docs/en/claude-directory.md.

- **`claude --worktree NAME`** creates `.claude/worktrees/<name>` on branch
  `worktree-<name>`. On exit: clean + unnamed → removed with its branch; work present
  → you choose keep or remove; state can't be verified → it asks. The **resume binding**
  takes a resumed session back into its worktree (if the worktree is gone, it says so
  and clears the binding). `.worktreeinclude` copies gitignored files. There are
  `WorktreeCreate` / `WorktreeRemove` hooks, and `worktree.baseRef` = fresh|head.
- **The cleanup model to copy:** a marker in the git metadata of every worktree Claude
  creates; a `git worktree lock` held while the agent runs (and released by the sweep
  if the process died); an age sweep (`cleanupPeriodDays`) that skips worktrees with
  changes, untracked files, unpushed commits or dirty submodules, and never touches
  worktrees without the marker.
- **Agent view** (`claude agents`, research preview): a supervisor process; rows
  grouped Pinned / Ready for review / Needs input / Working / Completed; Space to peek
  and reply inline; Enter to attach; sessions idle and unattached for about an hour
  are stopped (the conversation is kept, and replying resumes it). There is
  `claude agents --json --all` (fields `id, state, status, waitingFor, sessionId,
  cwd`), plus `claude attach|logs|stop|respawn|rm`. **The state lives under the config
  dir. With `CLAUDE_CONFIG_DIR` set, the supervisor "runs as a separate instance with
  its own sessions"**, so with four accounts there are four agent views.
- **Cross-session messaging:** each session binds an inbox Unix socket, exported to
  hooks and Bash as `CLAUDE_CODE_MESSAGING_SOCKET` (+ `CLAUDE_CODE_MESSAGING_TOKEN`).
  A message to an idle session starts a turn; to a busy one it is read between tool
  calls. `notify_when_idle` gives a one-shot "tell me when it's done". Inbound
  messages can't approve permissions or change config. The docs don't say whether
  discovery crosses config dirs; verify that before relying on it.
- **Retention:** transcripts (`projects/<p>/<session>.jsonl`) are deleted after
  `cleanupPeriodDays`, **default 30**. Desktop-session transcripts are kept unless
  `desktopSessionCleanupPeriodDays` is set. All of this machine's accounts use the
  default.

**Implications for moa:** (a) moa can deliver `moactl ask` text through the target
session's messaging socket instead of typing into a tmux pane, which is safer and
works mid-turn (verify the protocol and peer rules). (b) moa should own the
cross-account view, because Claude's agent view is per account. (c) moa must set or
check `cleanupPeriodDays` per account, or "resumable by ID" breaks after a month.
(d) Use Claude's worktree conventions (`.worktreeinclude`, the marker and lock idea)
so worktrees moa creates and worktrees Claude creates are handled the same way.

---

## 6. Other managers, briefly

| Tool | Model | Worth borrowing | Mistake / caveat |
|---|---|---|---|
| **claude-squad** (Go, tmux) https://github.com/smtg-ai/claude-squad | `InstanceData{title, path, branch, status, program, worktree{repo_path, worktree_path, branch_name, base_commit_sha, is_existing_branch}, diff_stats}` in one JSON | **Records `base_commit_sha`** so the diff against the start of the task stays stable; *Pause* = commit WIP, remove the worktree, keep the branch (frees disk, cheap to resume); diff tab in the TUI | No conversation ID stored at all: resuming means a fresh agent. Commits "[claudesquad] … (paused)" into your branch. |
| **agent-deck** (Go, tmux) https://github.com/asheshgoplani/agent-deck (docs/guide/sessions.md, worktrees.md, conductor.md) | profiles → groups → sessions, SQLite state, its own tmux socket | **Per-group `CLAUDE_CONFIG_DIR`** with a precedence chain (account > conductor > group > env > profile > global); **`session switch-account`** *copies* the transcript into the target account's dir (with backup and size check) and resumes there; **worktree setup and destruction scripts approved by sha256**; `worktree finish` (merge, remove worktree, delete session); `.worktreeinclude`; **"conductor"**: a persistent agent that watches the other sessions, auto-answers routine waits and escalates to your phone (Telegram/Slack), with "watchers" (GitHub, mail) that wake it. Closest to moa's central agent. | Feature sprawl similar to aoe. The conductor is an always-running LLM session (cost). |
| **vibe-kanban** (Rust + web) https://github.com/BloopAI/vibe-kanban, https://vibekanban.com/docs/core-features/new-task-attempts | SQLite: `tasks → workspaces (was task_attempts: branch, base_branch, container_ref, PR) → sessions (executor) → execution_processes (run_reason: setup/agent/devserver/cleanup; before/after HEAD commit) → coding_agent_turns`, plus `merges` | The schema is the best existing reference for moa's Task/Workspace/Session split (migration `20251216142123_refactor_task_attempts_to_workspaces_sessions.sql`). **before/after head commit per agent run** gives a diff per turn. Per-project setup, cleanup and dev scripts. Orphan + expired-workspace cleanup with `DISABLE_WORKTREE_CLEANUP` for debugging. | The company shut down in April 2026 (https://vibekanban.com/blog/shutdown) and it is community-maintained now. A web-first kanban felt heavy. Worktrees in a hidden `.vibe-kanban-workspaces` dir are easy to forget. |
| **Conductor** (macOS app) https://conductor.build, https://www.conductor.build/.well-known/agent-skills/conductor/SKILL.md | repo → workspace (= worktree + branch, one branch per workspace) → chats | setup / run / **archive** scripts; `CONDUCTOR_PORT` (10 ports per workspace) and a concurrent/nonconcurrent run mode for dev servers; a `.context/` folder for uncommitted notes and handoffs; **checkpoints**: a snapshot before each agent reply in a private git ref, revert per turn (https://conductor.build/docs/reference/checkpoints); a review → PR → merge → archive flow | Closed, GUI-only ("no CLI or API" for local workspaces). Revert also deletes the conversation turns. |
| **Crystal → Nimbalyst** https://github.com/stravu/crystal | sessions in worktrees, Electron + SQLite | One-button "rebase from main" and "squash and rebase to main"; a diff per prompt | Deprecated February 2026; its successor is an Electron editor suite (breaks moa's "native" rule). |
| **ccmanager** (Node, no tmux), **Superset** (desktop) https://superset.sh/compare/superset-vs-claude-squad | worktree-per-session managers | ccmanager can copy a conversation into a new worktree's session | Electron/Node runtimes |

---

## 7. Cross-cutting lessons

### 7.1 Identity: three IDs, never conflated
| ID | Owner | Stable? | Notes |
|---|---|---|---|
| moa session ID | moa | yes | what the tree, tasks and asks reference |
| conversation ID | agent (Claude/Codex) | **no**: rotates on /clear, fork, compact, `/resume` of another | keep a history table, not one column |
| pane / terminal ID | tmux | no: reused after a server restart | validate with `pane_pid` + server `boot_id` (workmux) before trusting it |

The moa prototype uses `session_id` (= the Claude conversation ID) as the primary key
(`spike/moa-proto/moactl.py:import_herdr`). The real design should not.

### 7.2 Accounts are stores
Every tool that got multiple accounts right treats an account as **a store**
(config dir) plus **a launcher**, and binds each conversation to the store it was seen
in (aoe "recorded store outranks configuration"; agent-deck's precedence chain and
transcript migration; herdr-sessions' `HasTranscript` check). Tools that got it wrong
hard-coded `claude --resume` (herdr) or resumed "latest in this dir" (workmux).

### 7.3 Worktrees leak unless something owns them
Every tool that leaked worktrees cleaned up only when you deleted the session (aoe,
herdr-sessions, Conductor archive). The tools that don't leak run a sweep (Claude
Code; vibe-kanban's expired-workspace cleanup) or tie removal to an event that always
happens (workmux `merge`, `remove --gone` after the PR merges).

### 7.4 Status: hooks + seen-state + urgency sort
The states that matter at 30+ agents: **blocked/needs-you**, **done-unseen**,
working, idle, stale/asleep. "Done" clears on focus (workmux, herdr, Claude agent
view). Sort by urgency, then by recency. A `last-done` jump key. Order hook events with a
sequence number (herdr `--seq`) or the agent's timestamp.

### 7.5 Performance
Process launches per poll killed aoe. Event sources that cost nothing per check:
agent hooks writing to a socket or DB, tmux control mode (`tmux -C`) notifications,
Claude's `CLAUDE_CODE_MESSAGING_SOCKET`, filesystem watch on a single runtime file
(workmux's sidebar daemon writes `runtime/<backend>__<instance>.json`).

---

## 8. Recommendations for moa

### 8.1 Session / task / worktree model
```
Brain (a whole moa: work | private)
 └─ Project            path of the main checkout, remotes, default branch, defaults
     └─ Task           title, state (active → waiting → review/PR → merged | abandoned | parked),
                       priority, due, who (you/agent/other), group path (the tree)
         ├─ Workspace  0..1 per task (usually 1): kind (main-checkout | worktree),
         │             path, branch, base_ref, base_commit_sha, created_by (moa|claude|user),
         │             lock state, last_clean_check {dirty, untracked, unpushed, pr_state}
         └─ Session    1..n per task: moa id (stable), title, account (store), launcher,
             │         agent kind, state, seen_at, asleep_at
             ├─ Conversation[]  conversation_id, store (config dir), first_seen, last_seen,
             │                   reason (start | clear | fork | resume), transcript_present
             └─ Pane binding     tmux pane id + pane_pid + server boot_id (all three
                                 must match before you trust it)
```
- **Tasks and groups:** keep the user's nested tree (groups → sessions) as the *view*.
  A group path is a property of a task, not a parallel structure. A one-off session
  can have an implicit task, so starting a session never requires creating a task
  first.
- **A task may have several sessions** (e.g. a reviewer next to the implementer) sharing
  one workspace. A session may also run in the main checkout (no worktree) for
  read-only and planning work.
- **Steal from aoe:** Archive, Snooze-until, Pin, and Stage vs Signal (a hand-placed
  stage + rule-raised signals like "PR changes requested" or "CI red") for the big
  picture.

### 8.2 Worktree lifecycle and cleanup
1. **Creation:** moa creates the worktree, marks it (e.g. `moa` file in
   `<gitdir>/worktrees/<name>/`, same idea as Claude's marker and aoe's `aoe-purge`),
   applies `.worktreeinclude` (Claude's syntax, so it's shared), runs an optional setup
   script **approved by sha256** (agent-deck), and records `base_commit_sha`
   (claude-squad). One path template. Adopt the worktrees Claude creates under
   `.claude/worktrees/` as `created_by=claude` instead of making a parallel scheme.
2. **While running:** `git worktree lock --reason "moa session <id>"`; unlock when the
   session ends or sleeps (Claude's model).
3. **End of task** is a verb on the task, not a prompt on session delete:
   `finish` = merge locally (workmux `merge`, rebase/squash) **or** "PR merged"
   detected by the collector (upstream gone / PR state) → remove the worktree; the
   branch goes only if it's merged. `abandon` = remove, keep the branch by default.
   `park` = claude-squad pause (commit WIP to the branch, remove the checkout, keep
   the record; resume recreates it).
4. **A sweep the daemon runs** (daily, and on `doctor`): for every worktree of every
   known repo (from `git worktree list --porcelain`), classify it as owned-live,
   owned-ended-clean, owned-ended-dirty, foreign, or orphan (no record, or record gone).
   Auto-remove only **owned + ended + clean + pushed/merged + unlocked + older than N
   days**. Everything else becomes a card in the console ("3 worktrees hold unpushed
   work; 2.1 GB") with buttons. Never remove the default branch's checkout (aoe).
   Delete heavy dirs like `node_modules` in the background (workmux).
5. **Trash by move** (aoe): removing first moves the checkout to `.moa-trash/<id>` and
   purges later, so a mistaken delete can be undone.
6. Also prune per-account `.claude.json` project entries for removed worktree paths
   (workmux `claude prune`), with a backup.
7. Show disk use per worktree in the tree, so the cost is visible.

### 8.3 Multi-account resume rules
1. An **account = store (config dir) + launcher + agent kind**. Store it with every
   conversation ID at capture time, from the hook that fired (the hook runs inside the
   account's config, so it knows its own `CLAUDE_CONFIG_DIR`).
2. **Resume = the launcher of the store the conversation was recorded in**, never the
   session's current default profile. If the transcript isn't at
   `<store>/projects/*/<id>.jsonl`, look in other stores *only to report a mismatch*
   (as found in the live data), and don't resume silently in the wrong account.
   Switching accounts is an explicit operation: copy the transcript with a backup,
   then resume (agent-deck `switch-account`).
3. **Refuse rather than guess** (aoe): no "latest conversation in this directory"
   fallback. If resume fails, set a `resume_failed` marker; the next open asks:
   start fresh or pick a conversation.
4. **Hooks per store:** install the moa hook (marker-tagged, already done in the
   prototype) and the plugin in every account, plus Codex `hooks.json` (it needs
   trust). `moa doctor` checks every store.
5. **Retention:** `doctor` sets or warns about `cleanupPeriodDays` in every account's
   `settings.json` (e.g. 3650). moa also records `transcript_present` and the last
   check time, so a deleted transcript shows up in the tree before you try to resume.
6. Codex: the ID arrives with the first prompt; resume with `codex resume <id>`; the
   record says "no conversation yet" until then.
7. **Never auto-answer trust prompts.** A new worktree is a new folder, so Claude's
   trust prompt (default "No") appears; the console should say so instead of reporting
   the session as failed. Pre-trusting through `.claude.json` would be possible but is
   a security decision for the user to make.
8. Brain boundaries are already decided (folder picks the brain, accounts are listed
   per brain). Resume must also check that the conversation's store belongs to the
   current brain.

### 8.4 Migrating herdr-sessions records
- **Source:** `~/.local/state/herdr-sessions/sessions.json` (`version: 1`, schema in §1),
  plus profiles in `~/.config/herdr-sessions/config.toml` (profile → command + config_dir).
  aoe's originals are in `~/.agent-of-empires/profiles/main/{sessions,groups}.json`;
  herdr-sessions already maps those, so import herdr-sessions, not aoe.
- **Mapping:**
  - `id` → moa session id (keep it; the ids go back to aoe).
  - `conversation_id` + the profile's `config_dir` → first `Conversation` row, but
    **only after checking the transcript exists in that store**. Otherwise mark it
    `transcript_missing`, or `found_in_other_store=<x>` (1 record), and don't silently
    move it.
  - `profile` → account (the launcher names match moa's: acct-work/b/a, codex).
  - `group` → the task's group path; `groups[]` → the group table (order, collapsed;
    keeps empty groups).
  - `worktree{branch, repo, managed}` → Workspace with `created_by=aoe` (if managed),
    then run the sweep classification on it immediately.
  - `path` missing (3) → import as `archived` with the reason, not dropped.
  - `binding`, `live`, `agent_status` → **drop** (Herdr pane IDs mean nothing to moa).
  - `extra_args` → session launcher args (1 record).
  - `created_at`, `last_active_at` → keep.
- **Per brain:** the prototype skipped `Work/*` groups. With brains, `Work/*` records go
  to the work brain's DB and the rest to private. Decide by folder, as the brain rule
  says, with the group only as a hint.
- Make it a **dry-run report first** (herdr-sessions' `import-aoe` style: ok / warn /
  skip per record), idempotent (upsert by id), and leave the source file untouched.

### 8.5 UI patterns worth copying
- **Urgency first:** sort and filter needs-you > done-unseen > working > idle > asleep
  (workmux, Claude agent view); "done" clears on focus; a **last-done** key that
  cycles; a status line that shows counts and names only the urgent ones (the moa
  prototype already does this).
- **Peek and reply without attaching** (Claude agent view, Space): a panel with the
  agent's question or last message and a one-line reply that goes in through the
  session's inbox.
- **One-line AI summary per row** (Claude agent view, aoe smart rename), with a title
  lock so it doesn't overwrite a name you chose (agent-deck).
- **PR badge on the row** (`#1234`, coloured by state) from the collectors.
- **Diff review in the TUI:** diff against `base_commit_sha`, hunk-by-hunk staging,
  **comment-on-hunk sent to the agent** (workmux patch mode); per-turn diffs from
  before/after HEAD (vibe-kanban) or per-turn checkpoints (Conductor) later.
- **Agent-facing CLI:** herdr's verbs (`ask NAME --wait`, `wait --until blocked`,
  `read`), JSON on every command, refusal to type into a blocked agent, stall
  detection, and help text written for agents.
- **Notifications** only for needs-input, finished-unseen and failed (Claude agent
  view's set), plus a tab title with the count waiting on you.
- **A glossary in the design doc** with "Avoid:" terms (aoe `CONTEXT.md`).
- **Don't copy:** herdr's mouse-first chrome (mouse stays optional), aoe's 33 ms
  ticker, Electron/React/npm dashboards (moa's web UI has to work without an npm
  build).

---

## Sources

- herdr-sessions: `~/private/herdr-sessions/DESIGN.md`, `SPIKE.md`, `internal/store/store.go`,
  `internal/core/core.go`, `internal/config/config.go`, `internal/aoe/aoe.go`; data at
  `~/.local/state/herdr-sessions/sessions.json`, `~/.config/herdr-sessions/config.toml`.
- moa prototype: `spike/LEARNINGS.md`, `spike/moa-proto/moactl.py` (`import_herdr`).
- aoe: https://github.com/agent-of-empires/agent-of-empires — `CONTEXT.md`, `DESIGN.md`,
  `docs/adr/0003-local-transport-uds-service-and-disk-fallback.md`,
  `docs/adr/0004-cli-writes-through-the-daemon.md`, `docs/guides/worktrees.md`,
  `docs/guides/session-resume.md`, `src/tui/app.rs`, `src/tui/home/live_send.rs`,
  `src/tmux/status_detection.rs`, `src/session/worktree_reconcile.rs`, `src/session/storage.rs`.
- herdr: https://github.com/herdrdev/herdr — `skills/herdr/SKILL.md`; https://herdr.dev/docs/concepts/,
  https://herdr.dev/docs/session-state/, https://herdr.dev/docs/add-herdr-support/ (read from
  `docs/next/website/src/content/docs/*.mdx` in the repo).
- workmux: https://github.com/raine/workmux — README (status, dashboard, `claude prune`),
  docs `reference/commands/{resurrect,merge,remove,status,last-done}.md`,
  `guide/git-worktree-caveats.md`, `guide/dashboard/patch-mode.md`, `src/state/types.rs`,
  `src/agent_staleness.rs`.
- claude-squad: https://github.com/smtg-ai/claude-squad — `session/storage.go`, `session/instance.go` (Pause).
- agent-deck: https://github.com/asheshgoplani/agent-deck — `docs/guide/{sessions,worktrees,conductor}.md`.
- vibe-kanban: https://github.com/BloopAI/vibe-kanban — `crates/db/migrations/*`
  (esp. `20251216142123_refactor_task_attempts_to_workspaces_sessions.sql`);
  https://vibekanban.com/docs/core-features/new-task-attempts; https://vibekanban.com/blog/shutdown.
- Conductor: https://conductor.build, https://www.conductor.build/.well-known/agent-skills/conductor/SKILL.md,
  https://conductor.build/docs/concepts/workspaces-and-branches, https://conductor.build/docs/reference/checkpoints.
- Crystal/Nimbalyst: https://github.com/stravu/crystal (README: deprecated Feb 2026).
- Superset/ccmanager: https://superset.sh/compare/superset-vs-claude-squad,
  https://codex.danielvaughan.com/2026/05/05/codex-cli-session-managers-ccmanager-agent-deck-parallel-agent-orchestration/.
- Claude Code: https://code.claude.com/docs/en/worktrees, https://code.claude.com/docs/en/agent-view.md,
  https://code.claude.com/docs/en/cross-session-messaging.md, https://code.claude.com/docs/en/claude-directory.md.
- Local measurements quoted: `docs/context.md` (process-launch cost, aoe slowness), not copied here.
