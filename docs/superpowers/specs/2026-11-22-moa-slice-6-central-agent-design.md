# Slice 6: moa, the central agent, design

Agreed in conversation on 2026-11-22. This is the slice-level design: it splits slice 6 into four
parts and records the choices the main spec left open. **What moa does** is already settled. It
lives in the main spec (`2026-11-15-moa-rust-design.md`, §6.11 and the console part of §6.12)
and in the roadmap's slice 6 section (`../plans/2026-11-15-moa-v1-roadmap.md`). This document
doesn't repeat it. Where they differ, the spec wins.

## Why

Slices 0–5 built everything around moa: the database, the daemon, tmux, hooks, the cockpit,
the picker and terminals. Slice 6 adds moa itself, the agent you talk to:
- it briefs you;
- it is woken by what your sessions report;
- it asks before it acts.

The user has stopped using the spike and is waiting for v1 to continue their work. So the order
below aims to make moa usable as early as possible.

The user's ruling (2026-11-22): **the spec's version, as it is.** No changes from daily use
of the spike are pending.

## The four parts

Each part is its own sub-slice, like 5a–5d. Its detailed plan is written just before it starts,
and it ships as one or more PRs.

### 6a: moa's conversation runs

Roadmap items 1, 2 and 8.

- **The ACP host in the daemon (`moa_agents::acp`)**:
  - one adapter process per moa account, started through `Spawner` with the spec's env;
  - its stderr goes to the log;
  - every `session/update` is always read and written to `feed`, whether or not anything is
    watching.
- **Threads**:
  - one per local day per account, recorded in `moa_threads`;
  - `session/resume` after a restart the same day, else `session/load` with dedupe by message
    id;
  - a new day starts with the `systemPrompt` append: the embedded constitution, the scope and
    the brief;
  - changing moa's account starts a new thread.
- **moa's agent dir:**
  - the allow-list `settings.json`, so most calls never ask;
  - the CLAUDE.md fallback for adapters without `systemPrompt`.
- **Protocol ops** a client needs to talk to moa:
  - send a prompt;
  - cancel a turn;
  - read and subscribe to the feed.

  Each prompt's message id is recorded so automatic prompts can be marked.
- **`stub-acp`** in the testkit:
  - built on the same library, in the Agent role;
  - plays scripted turns, permission requests, hangs and crashes.

  The flake's default package keeps building only `-p moa`.
- **Try it:** a sandbox moa against `stub-acp`, with a prompt sent from the command line. The
  answer lands in `feed`.

### 6b: the console

Roadmap item 3.

- **The feed model and its paging.**
- **Rendering** each entry kind in the spec's list:
  - the Markdown renderer;
  - tool lines;
  - turns grouped under `◆ moa  HH:MM`.
- **Buttons:**
  - the session name on each button;
  - Tab hopping, Enter and click.

  The button bar is a runtime list that moa sets and the user can override (the user's call,
  2026-11-18).
- **The input line:** history and the `/` palette. Console commands run in the daemon with no
  LLM turn.
- **The frame and its states:** thinking, Esc cancels, PgUp/PgDn.
- **Errors:** each error message is shown at most once per 30 s, and a failing entry is drawn
  degraded instead of breaking the frame.
- **The real-adapter smoke test** (spec §11.6). It replays a recorded real-adapter transcript,
  and is re-recorded whenever the pinned adapter changes.
- **Try it:** `moa --sandbox` with the real adapter, against a copy of the private brain. From
  here you can talk to moa.

### 6c: permissions, reports, the watchdog

Roadmap items 4, 5 and 7, plus the account failover card (C24).

- **Permissions:**
  - a pending table;
  - cards answered with `y/a/n` by option `kind`;
  - "moa needs you" in the status line when no console is attached.
- **Reports and wake-ups:**
  - a queue per (session, kind), with the spec's caps and timings;
  - one channel: each report reaches moa exactly once, either in a wake prompt or prefixed to
    your next prompt;
  - proptests prove the exactly-once rule;
  - reports from moa's own folder are ignored.
- **The watchdog:**
  - after 5 minutes with no update during a turn, cancel;
  - 30 s later, kill, respawn and resume;
  - a card says so.
- **When moa's account fails** (auth, billing), a card offers to switch accounts. Nothing
  switches automatically.

### 6d: the opening brief, reminders, suggestions

Roadmap item 6, plus the backlog's slice-6 items.

- **The opening brief:**
  - it runs on your first presence after the day boundary (C19), not at midnight;
  - yesterday's thread-summary note feeds it (U19).
- **Reminders (nags)** are recorded in `nags` (C20) and shown as `⏰ Due today` /
  `⏰ N days overdue`. They ride only on the brief and on wake-ups (U18).
- **Suggestion cards:**
  - shown everywhere, and updated wherever they're answered;
  - "yes" runs the actions in the daemon;
  - "yes" with no actions reaches moa through the one channel.

  The data verbs (`suggest`, `suggestion yes|no|later|ls`) were built in slice 2a.
- **From the backlog (terminals):**
  - **`opened_by = moa`** for terminals opened by moa's own conversation, recognised by its
    `MOA_SESSION=moa` caller.
  - **Idle terminals ride on the brief and on wake-ups.** moa sees the idle list, and may
    offer to close one with a suggestion card. `term close` joins the suggestion action
    whitelist.
  - **Closing or archiving a session:** if it still has open terminals, moa offers to close
    them with a suggestion card.

## Not in slice 6

- **Asks between sessions and moa:** slice 7. 6c's wake-up channel is built so that asks can
  join it.
- **Setup, accounts beyond the configured ones, and import:** slice 8.
- **Live update** (re-exec, panels following the build): slice 9.
- **moa acting without a yes.** It only suggests. Safe actions still need your answer.
- **Codex or Gemini as moa's agent.** The host is written for "an ACP command plus env", but
  only Claude's adapter is tested.

## Must never happen

- **The daemon stops reading the adapter's updates.** If it does, moa's turn stalls (adapter
  issue #1269).
- **Closing or crashing the console touches moa's turn.**
- **A session report reaches moa twice, or never.**
- **moa pays for a turn nobody reads.** The brief waits for you to show up.
- **moa's account changes without your answer.**
- **A test runs the real `claude` or the real adapter.** Only the recorded replay stands in
  for them.
- **The recorded transcript commits account, machine or employer details** to this public
  repo.

## Approaches considered

- **Chosen: the official Rust ACP library, pinned to an exact version,** behind one module
  (`moa_agents::acp`).
- **Rejected for now: writing the JSON-RPC loop ourselves** (about 300 lines). It stays the
  spec's fallback if the library's churn gets painful.
- **Rejected: running `claude -p` once per turn.** It loses the always-on conversation, the
  permission cards and resume.
- **The adapter comes from nixpkgs (`claude-agent-acp`), pinned by `flake.lock`.**
  - Rejected: fetching the adapter's prebuilt binaries ourselves. They bundle a second
    `claude`, and each platform needs its own hash.
  - Rejected: using whatever adapter is on the PATH. Nothing would pin it.

## My calls (for the user to check)

- **The split and its order:** 6a, 6b, 6c, then 6d. After 6b you can talk to moa in a sandbox.
  After 6c it's safe to leave running.
- **Until 6c, a permission request is refused,** with one system line in the feed saying so.
  Holding it with no way to answer would hang the turn. The allow-list keeps these rare.
- **Versions are picked when each plan is written,** because both change almost daily:
  - the library: the newest release that day, pinned exactly;
  - the adapter: whatever version this flake's nixpkgs pin carries. If that version is
    unusable, the plan bumps the pin or overrides the version.

  The plan records both versions.
- **The real-adapter recording.**
  - **Who records it:** you, or I do with your explicit go-ahead for that one run, in 6b.
  - **Where it runs:** against a scratch brain.
  - **Before it's committed:** it's scrubbed of names, paths and accounts.
- **The backlog's terminal items** go in 6d through suggestions, not as a timed sweep. That
  keeps U18's rule (no extra turn at a set time). Moving them to slice 7 costs nothing.
- **6a packages the real adapter:** in the flake and the dev shell, launched with the spec's
  env (`CLAUDE_CODE_EXECUTABLE` and the rest).
  - 6a's tests run only against `stub-acp`.
  - 6a's "try it" may also run the real adapter, by hand.
  - So the risk that the pinned version is unusable shows up in 6a, not 6b.
- **The fallback is allowed during 6a:** `agent-client-protocol-schema` plus our own JSON-RPC
  loop.
  - **The trigger:** 6a's plan starts with a short check of the library, done once the version
    is picked. The library must play both roles: Client for the host, Agent for `stub-acp`. It
    must also support `session/resume` and `session/load`. If it can't, the plan takes the
    fallback.
  - **What stays the same either way:** `moa_agents::acp` exposes only moa's own types.
- **The refusal before 6c** answers with the `reject_once` option. If the adapter doesn't offer
  one, the turn is cancelled.
- **The recording is a manual, one-off step, not a test.**
  - The allow-list applies during it.
  - A small scrub tool lives in the testkit and runs before the file is committed.
  - CI only replays the recording.
- **Presence:**
  - **"A console is attached"** means at least one console panel is subscribed to the feed
    (6b). 6c reads that.
  - **"First presence"** for the brief: tmux's `client-attached` hook already reaches the
    daemon's mailbox. 6d adds a `client-active` message beside it.
- **Session event lines in the feed** (`▶ started`, `○ ended`, `⚠ needs you`, `● finished a
  turn`):
  - **Who writes them:** the daemon, in 6b, from the session events it already handles.
  - **Folding:** the console folds them per session.
- **Report queues are kept in the database, not only in memory,** so a restart neither loses a
  report nor delivers it twice.
  - **When a report counts as delivered:** in the same transaction that writes the prompt
    carrying it to the feed, before the prompt is sent.
  - **If the daemon dies mid-send:** the report is treated as delivered, so a duplicate can't
    happen.
  - The proptests cover restarts.
- **The terminal items build on code that exists:**
  - agents already carry `MOA_SESSION`;
  - moa's adapter gets `MOA_SESSION=moa`;
  - session close and archive already exist in the core.

## Left to the plans

- the names and shapes of the protocol ops and feed entries;
- schema additions, if any (`feed`, `moa_threads`, `nags` and `suggestions` already exist);
- the console's key map in detail, within the spec's list;
- the stub-acp script format;
- the e2e test list. Each part's "done when" comes from the roadmap's list:
  - permission;
  - slow turn;
  - hang → watchdog;
  - crash → resume;
  - wake batching;
  - opening brief after midnight.
