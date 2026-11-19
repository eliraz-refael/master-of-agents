# moa v1 in Rust: design

*Status: draft for review, 2026-11-15. Written so a Rust engineer can plan the build without
talking to the user. Method: Superpowers (brainstorming, then writing-plans).*

**How to read this.**
- Decisions the user made are tagged **[user]**. Decisions this document makes are tagged
  **[call]**, each with its reason. A call can be revisited, but the reason has to be answered.
- "The spike" is the Python/bash prototype under `spike/` (`spike/moa-proto/moa` and
  friends). `spike/LEARNINGS.md` is what it taught us.
- Deep detail lives in six research notes. They're cited as R1–R6:
  R1 `1-inventory.md` (the spike's full feature inventory, data model, polls, 75 lessons,
  outside contracts: **the parity checklist**), R2 `2-tmux-control-mode.md`, R3 `3-agents.md`
  (ACP, Claude Code hooks/plugins, Codex), R4 `4-rust-architecture.md`, R5 `5-prior-art.md`,
  R6 `6-native-gui.md`, all in `docs/superpowers/research/` (account names in them are
  replaced with made-up ones).
- Examples use a made-up project, "Lantern", made-up accounts (`acct-a`, `acct-b`) and the
  example folder `~/private`.

---

## 1. Intent and why

moa (master-of-agents) is a cockpit for running many coding agents at once, plus a central
agent, **moa**, that keeps the big picture: what each session is doing, what's waiting on
you, which tasks are due, and what to do next. It briefs, suggests, and acts when asked.

The spike started as a throwaway prototype and drifted into daily use. The user uses all of
it, every day, and likes it more than herdr or aoe. But it's a set of Python and bash scripts
that share one SQLite file, poll it at 10 Hz, launch 8–15 processes per hook event, and
crash when its own files are edited out of order. v1 is the real product: the same
features, the spike's mistakes fixed, in Rust, event-driven, and built so other people can
install it.

Coding is the last step. This document is the step before it.

## 2. Goals, non-goals, anti-goals

### Goals
1. **Full parity** with the spike (section 7), with nothing dropped because it "looked unused".
2. **The user's work carries over**: both brains (the private/default one and a named one
   such as `work`), sessions, groups, titles, projects, tasks (priority, due), notes,
   suggestions, asks, moa's thread, layouts, account setup and plugin.
3. **The switch-over keeps live agents running**: v1 takes over the live tmux servers.
4. **Strictly event-driven.** Nothing polls. moa launches no process per poll, per event or
   per action (section 6.1 lists the few launches that are allowed, and why).
5. **Fix the spike's mistakes** (section 8): pane identity, conversation-id rotation, brain
   leaks, silent console death, hot-reload crashes, duplicated logic.
6. **Easy for other people to set up**, from the CLI or the TUI. It's a public MIT project.
7. **Designed for Codex and a desktop GUI**, built later.

### Non-goals for v1
- Codex support (designed for, built after the switch-over).
- A desktop GUI or web UI (the protocol is ready for one; section 6.16).
- Worktree lifecycle (the model leaves room; built after the switch-over; section 6.17).
- Collectors (git/PR/CI state), notifications outside the terminal, remote access.
- Public release and packaging for other people (U16; after the switch-over). New spike features
  (the spike is frozen, U13).
- Replacing tmux with moa-owned PTYs.

### Anti-goals: it fails even if it "works" when…
| Anti-goal | Why it's fatal | Guarded by |
|---|---|---|
| Anything polls (a timer that checks state, an mtime loop, a 1 Hz tmux subscription) | The spike's 10 Hz loops and aoe's per-check process launches are what this replaces | Core takes time as input; spawn counter and idle-wakeup tests (section 11) |
| A process launched per check, event, key, menu item or button | Each exec costs 5–10 ms on a machine with an endpoint-security agent, and the tmux server is single-threaded | One spawn module with a counter; tests assert zero spawns in scripted flows |
| A conversation that can't be resumed (id lost, id rotated and not recorded, wrong account) | "Nothing gets lost" is the core promise | Conversation history table; recorded-store resume; transcript checks |
| Conversations deleted silently | Claude Code deletes transcripts after 30 days by default | Retention rule (section 6.12) |
| Work data in the private brain (or the reverse) | The user split brains for exactly this | One daemon and DB per brain; one shared routing function; isolation tests |
| The switch-over kills agents or loses state | The user works in the spike every day | Adopt the live server; rehearsed switch; fallback (section 9) |
| tmux becomes visible (prefix chords, tmux words in the UI) | The user isn't a tmux user | moa key table, one menu key, no tmux vocabulary in help text |
| A dead console takes moa down, or a pane dies silently | Happened in the spike | ACP lives in the daemon; panels are supervised, log, and come back with Repair |
| An upgrade loses state or needs a manual restart nobody announced | The spike's hot reload is loved; its crashes are not | State in SQLite and tmux; re-exec; explicit restart notices |
| A web UI that needs an npm build | Breaks the Nix build and the constitution | Vendored static files only |

## 3. Constraints (the constitution, generalized)

1. **Native.** No Electron, no V8 in moa's own processes. One daemon per brain plus thin
   clients. (The ACP adapter is a Node program, but it *is* the agent, like every Claude
   session; moa doesn't embed it.)
2. **Keyboard-first in every client, mouse supported** (click acts, wheel scrolls), as in herdr.
3. **Event-driven, not polling.** Timers only fire at exact deadlines.
4. **Opt-in per agent.** Sessions moa didn't start behave exactly as before. Adopting one shows it
   alive/dead in the tree; restarting it under moa (same conversation) gets it tracked (U14).
5. **Multi-account by construction.** An account is a store (config dir) plus a launcher.
   Every conversation is bound to the store it was recorded in.
6. **Nothing gets lost.** Every conversation stays resumable by id across restarts, crashes
   and reboots. Deletion is opt-in and announced.
7. **Brains are isolated.** A brain is a whole moa (daemon, DB, tmux server, scope). The
   folder decides the brain, not the account.
8. **Clients:** the TUI first; the daemon protocol is client-agnostic so a web UI or a GUI
   can follow without daemon changes. Any web UI is served by the daemon with no npm build.
9. **macOS first, Linux too** (U15). **Builds with Nix** (a flake, aarch64-darwin first) and with
   plain `cargo`; packaging for other people comes after the switch-over (U16).
10. **tmux is invisible plumbing.** moa's own keys, discoverable from one key and one menu.
11. **Alive and fun is a requirement**: spinners, pulses, flashes, a nested tree with guides.
12. **moa briefs, it doesn't relay; moa is proactive** (priorities, suggestions with buttons).
13. **Live updates without losing state, and always say when a restart is needed.**
14. **Exact colours**: the Nord-based light palette in section 6.14, in true colour.
15. **Public repo hygiene**: no hostnames, employer, security products, account names or
    emails in code, docs, fixtures or tests.

## 4. Approaches

| | Approach | Verdict |
|---|---|---|
| **A** | Build v1 alongside the spike. Test it on copies of the brains, in a sandbox tmux server, with a stub ACP agent, while the user keeps working in the spike. Then switch over in one go: migrate the brains, adopt the live tmux servers, keep the spike as a fallback for a while. | **Picked [user].** Daily work is never on half-built code; everything is rehearsed on copies; the switch is one reversible event. |
| B | Replace the spike piece by piece under live use. | Rejected [user]. Needs throwaway glue that speaks the spike's protocol (its DB conventions, env vars, tmux options), and half-built code would run under daily work. |
| C | Grow the Python spike into the product. | Rejected [user]. Rules out a native GUI, and "scripts sharing one DB" is the structure to replace (two writers, migrations on every connect, duplicated logic). |

## 5. Decisions

### 5.1 Decisions the user made
| # | Decision |
|---|---|
| U1 | Build the real product now; the spike is a prototype that drifted into daily use. Learn from its mistakes first; coding is the last step. The user is patient. |
| U2 | v1 scope is **full parity** with the spike plus fixes for its mistakes. Existing work carries over (goal 2). |
| U3 | **Approach A** (section 4). |
| U4 | **Rust.** |
| U5 | **No polling anywhere.** No process per poll, event or action inside moa. Hooks are the agent's launches, made as cheap as possible. Timers fire at exact deadlines. |
| U6 | **Stay on tmux** as invisible plumbing, through one control-mode connection per brain. moa doesn't own PTYs. |
| U7 | **After a reboot or crash, sessions come back asleep (💤)**, the tree as it was left; Enter resumes one. No mass auto-resume. |
| U8 | **Codex**: designed for (pluggable agent kinds, normalized event contract), built after the switch-over. |
| U9 | **Desktop GUI decided later.** Day one is the TUI. The protocol must be client-agnostic. Leading direction, for context only: a daemon-served web UI with no npm build step, optionally in a Tauri window; GPUI native as runner-up (R6). |
| U10 | **Conversations are never deleted silently.** Keeping everything is the default; deletion is opt-in, and the user is warned with the names of what will go. The exact mechanism is an open item to discuss (section 12). |
| U11 | **Brains**: separate brains are separate moas (one daemon per brain), isolated. The folder decides the brain. An account can be routed to a brain with folder exceptions (`setup install ACCOUNT --brain work --except ~/private ~/.config`). Setup must work from the TUI too, and be easy for other people. |
| U13 | **The spike is frozen** from 2026-11-15: bug fixes at most, no new features. Section 7 and R1 are the fixed parity target; new ideas go to v1's backlog. |
| U14 | **Adopted sessions** (started outside moa) show alive/dead only; a one-key **"restart under moa"** resumes the same conversation inside moa with full tracking. No account-wide tracking hooks to cover them. |
| U15 | **Platforms**: macOS is the priority (built, dogfooded and switched over first); Linux is supported in v1 too, through the same `moa-platform` seams (timerfd, `/proc`, inotify, systemd). The user expects no special adjustments. |
| U16 | **Public release is later**, after the switch-over: at the switch v1 is installed for the user via Nix. Packaging for others (binaries, install docs, getting the pinned ACP adapter without Nix) is its own slice. |
| U17 | **The picker comes both ways**: a floating popup over the current view and a full-window view; the user picks which per key (and a setting sets the default). The popup's one process per open is an accepted, user-chosen exception to U5: it's user-triggered, never per poll or event. |
| U18 | **No timed nags**: due dates ride on the opening brief and wake-ups only; no extra moa turn at a set time. |
| U19 | **moa's thread is one per day**, as in the spike; yesterday's summary note feeds the opening brief. |
| U20 | **The fallback lasts until v1 works like the spike**: the moment it does (an hour or two days), the spike, its forwarders and the frozen DBs are removed. |
| U12 | Standing requirements from the spike's life: keyboard-first with mouse; tmux invisible (moa's keys, one menu key); fun/alive; moa briefs rather than relays; moa is proactive (priorities, suggestion cards with yes/later/no that run safe actions); live updates without losing state; always say when a restart is needed; exact Nord-ish light colours. |

### 5.2 Index of designer's calls
Each is justified where it's described.
| # | Call | Where |
|---|---|---|
| C1 | One `moa` binary; a Cargo workspace of 11 crates with a pure core | 6.2 |
| C2 | Core is `handle(&mut State, Input, now) -> Vec<Effect>`; IO in actors | 6.3 |
| C3 | IPC: newline-delimited JSON-RPC 2.0 over a Unix socket; subscribe = snapshot + revisioned patches; builds must match | 6.4 |
| C4 | SQLite via rusqlite; the daemon is the only writer; versioned migrations; UTC instants; daily `VACUUM INTO` | 6.5 |
| C5 | The spike's DB is imported read-only into a **new** v1 DB; the spike's file stays frozen as the fallback; v1 migrations stay additive during the transition | 6.5, 9 |
| C6 | Session identity: a stable moa session id plus a history of conversation ids | 6.5, 6.7 |
| C7 | tmux: one control client per brain, hooks and keys reach the daemon through a `display-message -c` mailbox; min tmux 3.4, tested on 3.8; keep the spike's socket names | 6.6 |
| C8 | Hooks: exec-form `moa hook`, async except SessionStart, spool file when the daemon is down | 6.7 |
| C9 | Tracking hooks reach moa-launched sessions through an injected plugin (`CLAUDE_CODE_PLUGIN_DIRS`), not per-account settings; per-account install is only for "every session knows moa" | 6.7, 6.10 |
| C10 | Hook events: PermissionRequest for "needs you", StopFailure for errors, PostToolUse to clear "needs you", a quiet-timer for interrupts | 6.7 |
| C11 | Multi-account resume: the recorded store's launcher; refuse rather than guess | 6.7 |
| C12 | The central agent is hosted by the daemon over ACP (pinned crate and adapter, always drained, watchdog); the console is just a client | 6.11 |
| C13 | Wall-clock timers on kqueue `EVFILT_TIMER` (absolute, continuous) / timerfd; short timers on a `DelayQueue` | 6.8 |
| C14 | Brains: one daemon per brain; one shared, pure routing function; opening a session in another brain's folder is refused | 6.9 |
| C15 | Per-brain scope config (`folders`, accounts) replaces the hardcoded `PRIVATE_ROOTS` and account tables | 6.9 |
| C16 | Panels are long-lived generic `moa panel` processes; layouts move them; spare ones are parked in a hidden session | 6.13 |
| C17 | The full-window picker is a parked panel shown as a linked window (zero launches); the popup form is `display-popup -E moa pick` (U17) | 6.13 |
| C18 | The status-line clock is updated by a minute-boundary timer; tmux's `status-interval` is off | 6.6 |
| C19 | The daily opening brief runs on the user's first presence after the day boundary, not at midnight | 6.11 |
| C20 | Due-date nag dedupe is persisted; nags ride on the opening brief and wake-ups | 6.11 |
| C21 | Live update: daemon re-exec with the listening socket inherited; panels follow the build id | 6.15 |
| C22 | Process inspection (adopt, untracked panes) uses libproc/sysctl in-process, never `ps`/`pgrep`/`lsof`; repo seeding uses `gix`, not `git` | 6.7, 6.10 |
| C23 | Worktree lifecycle: model sketched now, built after the switch-over | 6.17 |
| C24 | moa's own account failover is a card the user answers, not automatic | 6.11 |
| C25 | Switch-over through small forwarders added to the spike (inactive until a flag file exists) | 9 |
| C26 | Build slices and their order | 10 |
| C27 | File locations (XDG on both macOS and Linux) | 6.5 |
| C28 | The cockpit and panels are created lazily when the user first attaches, not at login | 6.13 |

---

## 6. Architecture

### 6.0 Glossary (with terms to avoid)
| Term | Meaning | Avoid |
|---|---|---|
| **brain** | A whole moa for one part of life: its daemon, DB, tmux server, scope. `default` and named ones like `work`. | "profile", "workspace" |
| **moa** | The central agent (an ACP session hosted by the daemon). | "master" |
| **console** | The client view where you talk to moa. | "chat pane" |
| **session** | moa's stable record of one agent's work: title, group, account, state. Has a stable moa id. | using it for a tmux session |
| **conversation** | The agent's own resumable id (Claude session UUID, Codex thread id). A session has a history of them. | "session id" for this |
| **pane** | A tmux pane. Identified only together with its server generation. | treating a pane id as identity |
| **account** | A store (config dir: `CLAUDE_CONFIG_DIR`, later `CODEX_HOME`) plus a launcher. | "profile" |
| **cockpit** | The tmux window assembled from panels by a layout. | |
| **panel** | A moa view (tree, plan, status, console…) in a pane. | |
| **agent spot** | The cockpit slot where a live agent pane is swapped in. | |
| **report** | A session event moa should know about (turn ended, needs you). | |
| **wake-up** | A hidden prompt that hands moa a batch of reports. | |
| **suggestion** | moa's proposal with actions that "yes" runs. | |
| **ask** | A message from one agent (or moa) to another, answered by its next final message. | |
| **asleep** | Ended to free memory, resumable, shown 💤. A flag, not a state. | |
| **archived** | Hidden from the tree, still resumable. | "deleted" |

### 6.1 Processes

```
           user's terminal (Ghostty, …)
                 │  tmux attach (exec'd by `moa`, once per open)
                 ▼
 ┌──────────────── tmux server  -L moa  (or moa-<brain>) ─────────────────┐
 │ session "moa": window 0 = cockpit (panels + agent spot), 1..N = agents │
 │ session "_moa": hidden; parked panels, the picker, the control client  │
 │ agent panes: claude … (env MOA_SESSION, MOA_BRAIN, plugin dir)         │
 └───────▲──────────────────────────────▲──────────────────────────▲──────┘
         │ tmux -C (one, long-lived)    │ moa panel <view> (long-  │ moa hook (exec form,
         │ commands + %notifications    │ lived, one per pane)     │ per agent hook event)
 ┌───────┴──────────────────────────────┴──────────────────────────┴──────┐
 │ moa daemon --brain B  (one per brain; launchd agent or autostarted)    │
 │  Core  ◄── inputs: hooks · tmux · ACP · clients · timers · fs watch    │
 │   └─► effects: store · tmux · ACP · timers · publish · reply           │
 │  store (SQLite, single writer) · scheduler · ACP host ──► claude-agent-acp
 │  socket ~/.local/state/moa/run/B.sock  (moactl, panels, hooks, later web)
 └────────────────────────────────────────────────────────────────────────┘
```

**Allowed process launches** (the complete list; everything else is in-process):
| Launch | When | Why it's acceptable |
|---|---|---|
| tmux server | Once per brain, if it isn't running | It's the host; it outlives the daemon |
| `tmux -C` control client | Once per daemon connect (restart with backoff) | The only tmux process moa runs |
| ACP adapter (`claude-agent-acp`) | Once per moa account; restart on crash | It is the agent |
| Agent sessions and plain shells | When the user or moa opens or resumes one | They are the product; tmux launches them |
| `moa panel` | Once per panel pane; parked, reused; respawned on crash or Repair | Long-lived, not per action |
| `moa daemon` (autostart) | Once per daemon start, by a client that found none | The daemon is the product |
| `moa hook` | Per agent hook event | The agent's launch; exec form, ~6 ms, async (C8) |
| `moactl` / `moa <verb>` | When a user, script or agent runs it | Their launch, not moa's |
| `moa pick` in a `display-popup` | When the user opens the popup picker | User-chosen (U17); never per poll or event |
| `tmux attach` | When the user runs `moa` | The user's terminal becomes the client |
| `claude plugin …` CLI | Per account, on an explicit setup action (install/remove the account plugin) | Rare; no supported alternative (lesson 28) |
| launchd/systemd registration | On setup | Once |

Not allowed: `ps`, `pgrep`, `lsof`, `git`, `sqlite3`, `jq`, `bash` helpers, `run-shell`,
`display-popup` commands (except the user-chosen picker popup, U17), `less`. Each spike use has an in-process replacement (C22, C17, 6.6).

### 6.2 Crates **[call C1]**
One binary, `moa`, with subcommands (`moa`, `moa daemon`, `moa panel`, `moa hook`, the
agent-facing verbs; `moactl` is a symlink that dispatches on `argv[0]`). One binary means
the hook, panels and daemon can't drift apart (R4 §7). `moa-core` does no IO of its own: IO
the core needs is injected as plain functions in `Deps`.

| Crate | Holds | Depends on IO? |
|---|---|---|
| `moa-core` | Domain types, the pure Core (`State`, `Input`, `Effect`, `handle`), the routing function, the live-state model, wake/sleep/ask state machines, brief/attention text | No (no tokio, no clock) |
| `moa-proto` | Wire types (requests, results, patches, events, hook envelope) and the verb grammar, JSON Schema export | No |
| `moa-store` | Store actor (rusqlite), schema and migrations, spike import, spike export, backups | SQLite |
| `moa-tmux` | Control-mode client: byte parser, FIFO reply matching, typed notifications, mailbox messages, command escaping | tmux |
| `moa-agents` | Agent kinds (Claude now, Codex later): launch/resume argv, hook normalization, plugin-dir generation, transcript checks; the ACP host | ACP |
| `moa-daemon` | Actor wiring, executor, scheduler, hook ingest + spool drain, client sessions, fs watcher, upgrade/re-exec | all |
| `moa-client` | Async client library: connect, `hello`, subscribe, reconnect + resync | socket |
| `moa-view` | View-models shared by every client (tree rows, plan, status, feed items, cards, `Action` enum, default keymap) | No |
| `moa-tui` | ratatui panels, Markdown renderer, theme, input line, mouse | terminal |
| `moa-platform` | Wall-clock alarm, process inspection (libproc / `/proc`), paths, launchd/systemd glue | OS |
| `moa` | The binary: clap subcommands; the `hook` path uses std only (no tokio) | |
| `moa-testkit` | Sandbox tmux server, stub ACP agent binary, fake agent commands, hook fixtures, temp brains | tests only |

Reasons: the pure core is where the hard logic lives and must be testable without tmux,
Claude or sleeps (R4 §1.2). `moa-view` keeps the TUI and any later web or GUI client
identical in behaviour (R6 §3). ACP sits behind `moa-agents` so the churning ACP crate is
one module to fix (R3 §1.2).

### 6.3 The daemon core **[call C2]**

```rust
fn handle(state: &mut State, input: Input, now: Now) -> Vec<Effect>
// Now = { wall: UtcInstant, mono: Monotonic, local_day: Date, tz_offset }
```
- **Inputs**: `Hook(Envelope)`, `Tmux(Notification)`, `TmuxReply{token, result}`,
  `Acp(Update | Request | Exited)`, `Client(ReqId, Request)`, `ClientGone`,
  `Timer(Key)`, `ConfigChanged(File)`, `Startup(Snapshot)`, `StoreCommitted(Changes)`.
- **Effects**: `Persist(Vec<Change>)`, `Tmux(Vec<Cmd>, token)`, `Acp(Op)`,
  `SetTimer{key, at}`, `CancelTimer(key)`, `Publish(Patch | Event)`, `Reply(ReqId, Result)`,
  `Spawn(ChildSpec)` (only the allowed launches), `Log`.
- **Ordering rule**: the executor applies `Persist` before any `Publish` or `Reply` from the
  same `handle` call, so no client ever sees state the DB doesn't have.
- Multi-step operations (open a session, apply a layout, deliver an ask) are small state
  machines in `State` (`pending_ops`) driven by `TmuxReply` and `Timer` inputs. No `await`
  in the core.
- One `Core` task processes inputs one at a time. Actors (store, tmux, ACP host, scheduler,
  client sessions, hook listener, fs watcher) are plain tokio tasks with mpsc inboxes,
  supervised by a restart loop (`TaskTracker` + `CancellationToken`). No actor framework.
- Every actor logs through `tracing` to the brain's log; a panicking actor restarts and the
  panic is published as a visible error event.

### 6.4 Protocol **[call C3]**
Newline-delimited JSON-RPC 2.0 over a Unix socket (`~/.local/state/moa/run/<brain>.sock` in a
`0700` directory; peer uid checked; length checked against macOS's 104-byte `sun_path`,
falling back to `$TMPDIR/moa-$UID/`). Same messages later over a WebSocket for a web client.
Why: same framing as ACP and MCP, debuggable with `nc -U`, any language can speak it, no
codegen, no npm (R4 §2.1).

- `hello {client, build}` → `{brain, build, rev}`. **Builds must match** (atuin's rule): a
  client on another build may only call `hello`, `status`, `upgrade`, `shutdown`, and the
  hook notification. A newer client asks the daemon to upgrade (6.15); an older panel
  re-execs itself. No protocol version to bump.
- `subscribe {topics, since?}` → snapshot (or the patches since `rev`), then
  `patch {rev, upsert:{topic:[…]}, delete:{topic:[ids]}}` notifications. The Core publishes
  `Arc<Model>` into a `watch` channel; each client task diffs its topics and sends one patch,
  so slow clients skip intermediate states and the Core never blocks.
- `event {kind, …}` for discrete things (turn done, needs you, toast, error), via `broadcast`;
  a lagging client gets a fresh snapshot.
- Topics: `brain`, `sessions` (with derived display state and "why"), `groups`, `projects`,
  `tasks`, `suggestions`, `asks`, `activity` (tail), `feed` (console, paged), `status`
  (counts), `layouts`, `accounts` (setup state), `terminals`.
- Methods mirror the agent-facing verbs (section 6.10): `session.open|resume|focus|close|
  archive|sleep|set|adopt|find|ls`, `group.add|rename|rm`, `term.open`, `layout.list|show|
  apply|set`, `goto`, `brief`, `attention`, `project.show|add`, `note.add`,
  `task.ls|add|set`, `suggest`, `suggestion.answer|ls`, `peers`, `ask`, `reply`, `asks`,
  `setup.*`, `brain.*`, `import.herdr`, `console.prompt|cancel|command|permission|new|auto|
  sleep_after`, `ui.here` (a panel's selected folder), `ui.select_view`, `status`, `upgrade`.
- Long requests (`ask`, `reply`, `session.focus` that waits for a pane) simply reply later on
  the open connection. No asker-side loops.
- Results are structured JSON; the CLI renders text (and `--json` passes it through).
- `schemars` exports a JSON Schema of `moa-proto`; it documents the protocol for agents,
  scripts and future clients.
- Hook messages and the spool format are additive (unknown fields ignored), because they
  cross upgrades.

### 6.5 Store and schema **[calls C4, C5, C6, C27]**

**Engine.** `rusqlite` (bundled; features `hooks`, `backup`, `functions`). The store actor
owns the only write connection on its own OS thread, batches queued writes into one
transaction, and emits typed `Change`s to the Core after commit. Reads the Core needs come
from its in-memory model; ad-hoc reads (search, history) use a read-only connection.
PRAGMAs: WAL, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`. Nothing else
writes the file; agents and the CLI go through the socket (lesson 57). `moa sql` opens
read-only for debugging. Why rusqlite: synchronous and thin, exposes the hooks and backup
API, no compile-time `DATABASE_URL` friction in Nix (R4 §4.1).

**Migrations.** `rusqlite_migration`, version in `PRAGMA user_version`, run once by the
daemon at start, each preceded by `VACUUM INTO backups/pre-v<N>.db`. During the transition
window (switch-over until the spike is retired) v1's own migrations are **additive only**
(new tables/columns, no drops or renames), so an older v1 build can still run on a newer
DB if a v1 upgrade must be rolled back.

**The spike's DB is imported, not migrated in place [C5].** `moa adopt-spike` takes a
`VACUUM INTO` snapshot of the spike DB, attaches it read-only, and imports into a **new** v1
DB. The spike's file is never written by v1. Reasons: the spike keys sessions by
conversation id, writes local-time text, and runs `ALTER`s and creates triggers on every
connect; keeping one file usable by both would make v1 maintain the spike's representation
in its hot path for the whole transition, and a v1 write the spike misreads would corrupt
the fallback. A frozen spike DB plus a tested export (v1 → spike schema, section 9) is less
code in the hot path and keeps the fallback pristine. The import is the "adopt the
prototype" migration in spirit; it's idempotent and has a dry-run report.

**Timestamps.** Instants are `INTEGER` UTC milliseconds; rendering converts to local time.
Calendar dates (`due`, `today`, `last_activity_on`) stay `YYYY-MM-DD` local dates, because
they are dates, not instants (lesson 43).

**Backups.** Daily `VACUUM INTO` on the wall-clock scheduler (7 dailies, 4 weeklies) plus one
before every migration and before the switch-over.

**File locations [C27]** (XDG on macOS and Linux alike, matching the spike's `~/.local/share/moa`):
| What | Where |
|---|---|
| Shared config | `~/.config/moa/moa.toml` (accounts, brains, routing, settings), `layouts.json` (spike format, unchanged), `theme.toml`, `<brain>.md` scope files |
| Brain data | `~/.local/share/moa/<brain>/moa.db`, `backups/`, `agent/` (moa's cwd: its CLAUDE.md fallback and `.claude/settings.json` allow-list) |
| Generated plugin dirs | `~/.local/share/moa/plugin/session/` (injected) and `plugin/account/` (installable marketplace) |
| Runtime | `~/.local/state/moa/run/<brain>.sock`, `<brain>.lock`, `spool/<brain>.jsonl`, `logs/<brain>.log` |

The default brain's directory name is `default`. Today the spike's default brain DB and its
scope file live inside the repo; both move out at the switch-over.

**Schema (v1) and the spike → v1 mapping.**
| v1 table | Columns (main) | From the spike |
|---|---|---|
| `projects` | id, name (unique, case-insensitive), path, status (idea/active/paused/done), summary, last_activity_on, created_at, updated_at | `projects`; `area` dropped (never used); times converted |
| `tasks` | id, project_id, title, status, blocked_on, today, session_id (moa id), note, who, due, priority, created_at, updated_at, **done_at** | `tasks`; `session_id` mapped from conversation id to moa id; `done_at` = `updated_at` for done rows ("done today" no longer depends on `updated_at`) |
| `notes` | id, project_id, kind, text, created_at | `notes` |
| `suggestions` | id, text, status (open/accepted/rejected/later), actions (JSON list of `{verb, args}`), result, created_at, resolved_at | `suggestions.action` (moactl arg strings) re-parsed and re-validated; invalid ones kept as text and flagged |
| `sessions` | **id (UUIDv7, moa's)**, agent_kind, account, cwd, start_cwd, project_id, title, title_locked, group_path, lifecycle (live/ended), activity (working/idle/needs_permission/needs_answer/failed), activity_at, activity_why, last_prompt, last_turn_end_at, seen_at, asleep_at, archived_at, location (here/elsewhere), resume_failed, created_at, last_seen_at | `sessions`: new id per row; `state` split: working/idle/waiting → live + activity (waiting → needs_permission); ended → ended; archived → `archived_at`; elsewhere → `location`; `grp` → `group_path` |
| `conversations` | agent_kind, conversation_id, session_id, store_dir, reason (start/clear/fork/resume/import), first_seen_at, last_seen_at, transcript_path, transcript_present, transcript_checked_at | v1 only; one `import` row per spike session from its `session_id` + account's store |
| `pane_bindings` | session_id, server_pid, server_start, socket, pane_id, pane_pid, bound_at, tracked (0 for an adopted agent) | `sessions.pane` + `sessions.tmux`, kept only if verified against the live server at adoption |
| `events` | id, at, session_id, conversation_id, account, cwd, kind, text, detail (JSON) | `events`; kinds mapped (start → session.started, prompt → turn.started, stop → turn.ended, waiting → needs.permission, end → session.ended, ui → ui) |
| `activity` | id, at, who (brain/session/you), text, ref_kind, ref_id | `activity` copied; new rows are written by the Core, not SQL triggers |
| `groups` | path, collapsed, created_at | `groups` |
| `asks` | id, at, from_session, from_name, to_session ('moa' or id), text, status, reply, claimed_at, answered_at | `asks`; session ids mapped |
| `moa_threads` | thread_id, account, day, kind (daily/manual), started_at | `meta.console_thread` |
| `feed` | id, thread_id, at, kind, message_id, automatic, body (JSON), state | v1 only (the console's persisted feed; fixes "replay has no timestamps") |
| `nags` | task_id, due, day | v1 only (persisted nag dedupe, lesson 65) |
| `meta` | key, value | `meta.cursor` dropped (one channel, 6.11); `setup_skipped` → `moa.toml` |

Not in the DB any more: the `<db>-focus` file and `@moa_seen` pane options (→ `sessions.seen_at`
from the daemon's focus model), `@moa_here` (→ `ui.here` in daemon memory), the spike's
triggers. Moved to config: `<brain>.env` (→ `moa.toml` `brains.<b>.moa_account` /
`open_account`), scope files.

**Session identity [C6].** The tree, tasks and asks reference the moa session id, which never
changes. The agent's conversation id rotates on `/clear`, fork, and interactive `/resume`
(R3 §4, R5 §7.1), so it lives in `conversations` with a reason; the current one is the
latest. Fresh Claude launches get `--session-id <uuid>` from moa, so the id is known before
any hook. Every moa-launched process carries `MOA_SESSION=<moa id>`, and the hook echoes it
back, which links a rotated conversation to the same session.

**Retention.** No pruning of events or activity in v1 (they're small, and "nothing lost"
applies). What's *fed to the LLM* is capped (6.11) (lesson 45).

### 6.6 tmux layer **[calls C7, C18]**
Details and measurements: R2.

- **Servers.** Keep the spike's socket names (`-L moa`, `-L moa-<brain>`) and the cockpit
  session name `moa`, so v1 can adopt the live servers. The daemon starts the server if it's
  missing (one exec, with a generated config that first `source-file -q ~/.tmux.conf` so the
  user's extended keys and mouse survive, lesson 8). The server environment holds nothing
  moa-specific; every pane gets its env explicitly with `-e` (lesson 5).
- **One control client per brain**: `tmux -L <sock> -C attach -f no-output,ignore-size,
  no-detach-on-destroy -t '=_moa'` in a hidden `_moa` session. Read
  `#{pid} #{start_time} #{version} #{client_name}` at connect; refuse tmux < 3.4 with a clear
  message. Never `refresh-client -C` (it resizes the user's windows).
- **Parser.** Own implementation (~500 lines): read bytes, match `%begin/%end/%error` on the
  exact `(time, number, flags)`, skip flag-0 blocks, accept notifications inside blocks on
  pre-3.8 servers, `-F` formats that can't start with `%`, one command per line in simple
  mode, `a ; b` plus a sentinel for transactions, unknown notifications kept as `Unknown`.
  Fuzzed. Never send waiting commands (`command-prompt`/`confirm-before` without `-b`,
  `wait-for`, `run-shell` without `-C`, `display-popup`, `source-file` of such files).
- **Events with no process.** Server-wide notifications (`%window-pane-changed`,
  `%session-window-changed`, `%client-session-changed`, `%sessions-changed`,
  `%unlinked-window-*`, `%client-detached`, `%pane-mode-changed`) plus a **mailbox**: hooks
  and keys run `display-message -c <ctl> 'moa:…'`, which arrives as `%message`. Hooks
  installed at fixed array indexes so user hooks survive: `pane-exited[90]`,
  `pane-died[90]`, `window-layout-changed[90]`, `client-attached[90]`, `client-active[90]`
  (and `alert-bell`). Reinstalled idempotently on every connect; removed on clean shutdown.
  **No `refresh-client -B` subscriptions**: they're evaluated by a 1 Hz timer inside tmux,
  which is polling (U5).
- **Keys.** Static bindings in the config, routed through a server option so they survive
  daemon restarts:
  `bind -T moa g run -C "display-message -c '#{@moa_ctl}' 'moa:key g ##{pane_id} ##{session_id} ##{client_name}'"`.
  The daemon sets `@moa_ctl` at connect. When the daemon is down, an `if -F '#{@moa_ctl}'`
  shows "moa isn't running: run moa" instead of doing nothing. Keys that need no moa logic
  stay pure tmux (arrows/hjkl select-pane, `z`, `[`/`]`, `a`).
- **Menus and prompts**: `display-menu` items and `command-prompt -b … "display-message -c
  <ctl> -l 'moa:prompt %%'"` feed the mailbox; menus opened by the daemon pass `-t <user's
  pane>` and `-c <client>`.
- **Pane identity.** A pane binding is `(server pid, server start_time, %pane_id, pane_pid)`
  plus the pane option `@moa_session=<moa id>` (it travels through swap/break/join/respawn).
  A binding from another server generation is never trusted (lessons 1–4). Liveness comes
  from `pane-exited`/`pane-died` and `%window-close`, never from a hook's last event or an
  exit code (lesson 2).
- **Focus model.** `client → session → current window → active pane`, seeded once at
  connect with `list-clients`/`list-sessions`/`list-windows -a`, then kept by notifications.
  A pane is *visible* when it's in the current window of a session attached by a non-control
  client (and not hidden by another pane's zoom). Visible panes mark their session seen.
  This replaces the spike's "touch a file on window switch" hooks (lesson 11: never terminal
  focus reports).
- **Status line.** Two lines as in the spike; no `#()` segments. The daemon writes
  `@moa_status` (coloured counts `⚠ 2  ? 2  ✓ 3  ◐ 4  ● 19`), per-window `@moa_state`, and
  `@moa_clock` in one command when they change. **[C18]** `status-interval 0`; the clock is
  updated by a wall-clock timer at each minute boundary (a deadline, not a poll). The window
  list shows only ⚠/? windows; the current window is a pill on the left; `⤢` when zoomed;
  the bell colours a window red. While the daemon is down the clock stops, which also says
  "moa isn't running".
- **Mutations** are batched into one write (a layout switch is ~3 ms instead of ~270 ms of
  processes, R2 §5.3). New ids come from `-P -F '#{pane_id}'` replies (two round trips; a
  command after `split-window` on the same line targets the old pane).
- **Typing into a pane** (asks, 6.12): `set-buffer -b moa-ask-<id> "<escaped text>"`
  (newlines as `\n` inside double quotes; verify on 3.4), `paste-buffer -p -d -b … -t %N`
  (bracketed), then `send-keys -t %N Enter` after a 0.5 s timer.

### 6.7 Agents and hooks **[calls C8, C9, C10, C11, C22]**
Contracts and sources: R3.

**Agent kinds.** A trait in `moa-agents`: launch argv/env, resume argv, hook payload →
normalized envelope, transcript location check, delivery method for asks. `claude` now;
`codex` later (hooks via `CODEX_HOME/hooks.json` with one-time trust, `codex resume <id>`,
no `--session-id`, id learned from SessionStart) (U8).

**Launching** (open/resume): `new-window -d -P -F '#{pane_id}' -n <title> -c <cwd>` with an
explicit env allow-list: `CLAUDE_CONFIG_DIR=<store>`, `MOA_SESSION=<moa id>`,
`MOA_BRAIN=<brain>`, `CLAUDE_CODE_PLUGIN_DIRS=<plugin/session>`, `TERM` untouched; then the
account's launcher (absolute path, resolved at setup; default `claude`) with
`--session-id <uuid>` (fresh) or `--resume <conversation id>` (resume), plus `--name <title>`.
Window names are titles cut to 24 columns. The daemon binds the pane from the reply.

**The hook binary [C8].** Installed as a command hook in **exec form** (no shell):
`{"type":"command","command":"<stable path>/moa","args":["hook","claude"],"async":true}`.
The stable path is a profile symlink, never a Nix store path, so upgrades don't change the
hook string (Codex trusts hooks by hash). `moa hook` is a std-only path of the binary: read
stdin (cap 8 MiB), add env fields (`MOA_SESSION`, `MOA_BRAIN`, `CLAUDE_CONFIG_DIR`,
`CLAUDE_CODE_SESSION_ID`, `TMUX`, `TMUX_PANE`), stamp `at` (wall clock, ns) and its pid, pick
the brain's socket (from `MOA_BRAIN`, else the routing function over `moa.toml`), connect with
a 200 ms timeout, write one line, exit 0. **Only SessionStart is synchronous**: it waits up to
1 s for `additionalContext` (the peers note). If the daemon is down it appends the line to
`spool/<brain>.jsonl` (`O_APPEND`, `flock` for long lines) and exits 0; the daemon drains the
spool at start before reconciling. It never fails the agent. Measured: 5.9 ms per event
against 96 ms for the spike's `track.sh` (R4 §3.2). `type: "http"` hooks (no exec) are a later
optimisation; they lose events and show "hook error" notices when the daemon is down.

**How hooks reach sessions [C9].** For sessions moa launches, the hooks come from a plugin
dir moa generates and injects with `CLAUDE_CODE_PLUGIN_DIRS` (Claude Code ≥ 2.1.280): no
per-account install, no settings edits, no env-var gating, works on any account (R3 §3). The
per-account plugin install (section 6.10) is only for making sessions started *outside* moa
aware of moa (skill + `moactl`), and it carries no tracking hooks, so untracked sessions don't
pay a launch per event. Both plugins are named `moa`, so the injected one shadows the
installed one. Fallback when side-loading is disabled by managed policy: settings-file hooks
per account, written the spike's way (marker-tagged, backup once, atomic, symlink-safe,
others' hooks untouched; lesson 32).

**Normalized envelope** (R3 §6.1): `{v, source, account, slot (MOA_SESSION), session_id,
kind, at, seq, cwd, transcript_path, permission_mode, detail, raw}`. Kinds:
`session.started|ended`, `turn.started|ended|failed|interrupted`,
`needs.permission|answer|cleared`, `cwd.changed`, `model.changed`, `subagent.*`, `ui`.

**Events subscribed [C10]:**
| Claude event | Kind | Why |
|---|---|---|
| SessionStart (sync) | session.started (+source: startup/resume/clear/compact/fork) | Lifecycle, rotation, peers note |
| UserPromptSubmit | turn.started | Working; last prompt; ask receipt |
| Stop | turn.ended (+ last_assistant_message, background tasks) | Done/idle; report; ask answer |
| **StopFailure** | turn.failed (+ error) | Without it, an API error leaves the session "working" forever |
| **PermissionRequest** | needs.permission | Fires the moment the dialog opens. `Notification:permission_prompt` fires only after ~6 s without typing |
| PreToolUse, matcher `AskUserQuestion\|ExitPlanMode` | needs.answer | A question or plan approval |
| PostToolUse / PostToolUseFailure / PermissionDenied | needs.cleared | The only reliable "permission answered" signal. Async, ~6 ms of background CPU per tool call on the agent's side; if that shows up in measurements at 30 agents, narrow the matcher to tools that can prompt |
| Notification `elicitation_dialog` | needs.answer | MCP forms. `idle_prompt` and the rest are dropped (lesson 25) |
| SessionEnd | session.ended (+reason) | 1.5 s budget: the hook only enqueues |
| CwdChanged | cwd.changed | Display only; never moves a session between brains |

**Interrupts.** Esc in Claude fires no Stop. The Core arms a per-session *quiet* timer
(default 10 min, reset by every event of that session); when it fires on a working session,
the display becomes "◌ quiet 10m" (not idle), and the next event corrects it. If tmux offers
a pane-title-change hook on the supported versions, route it to the mailbox and use Claude's
title glyph as a second signal (verify; section 13).

**Event ordering.** Async hooks can race. The Core orders a session's events by the hook's
`at`; an event older than the session's current `activity_at` goes to history but doesn't
regress state. Duplicates are dropped by `(pid, seq)`.

**Live-state model (one implementation, in `moa-core`).**
| Display state | Rule | Glyph, colour |
|---|---|---|
| waiting (needs you) | activity = needs_permission | ⚠ yellow bold, blinking |
| question (asks you) | needs_answer, or the last turn's final line (Markdown stripped) ends in `?` | ? orange |
| failed | activity = failed | ✗ red, with the error |
| done (unseen) | last_turn_end_at > seen_at | ✓ green |
| working | activity = working (quiet after the timer) | spinner ⠋⠙⠹… / ◌ |
| idle | live, seen | ● dim green |
| asleep | lifecycle ended and asleep_at set | 💤 |
| ended | lifecycle ended | ○ dim |
| archived | archived_at set | hidden |
"Why" text per state as in the spike (prompt, permission tool, question line, last reply
line; lines starting with `<` blanked). Sorted by state, then recency. Every view, the
status line and moa read this one model (lesson 44).

**Rotation and lifecycle.** SessionEnd with reason `clear` followed by SessionStart with a new
id and the same `MOA_SESSION` (or, for spike-launched sessions with no `MOA_SESSION`, the same
pane binding) adds a `conversations` row (reason `clear`); the session stays live. A clean
quit (SessionEnd reason `prompt_input_exit` or `logout`) sets `ended`, whichever of it and the
pane's exit arrives first. An unexpected end (any other reason, or a pane exit with no
SessionEnd) puts the session to sleep (asleep, U7): moa can't tell a crash, a kill or a reboot
from a choice, and both resume. A tab moa kills (sleep, close, restart) takes its agent with
it, but a hook that agent sent between the commit and the kill can arrive after the commit,
stamped later: while the session isn't bound to that tab, any event from it but its end is
history only, so a late SessionStart can't bring back a session moa just put to sleep or
closed. If the kill is lost, the tab counts again once the session is bound to it again: at
once when a failed batch's reload restores the binding, or when reconcile or `adopt` rebinds
it after the link drops. Liveness still comes from pane events, never from a hook's last event
(lesson 3). moa's own ACP session carries `MOA_SESSION=moa` and is never listed as a worker
(lesson 31).

**Multi-account resume [C11]** (R5 §8.3):
1. Resume uses the launcher of the store the current conversation was **recorded** in, never
   the session's current default account.
2. Before resuming, check `<store>/projects/*/<id>.jsonl` exists (directory reads, no
   process). If it's missing there but present in another store, say so and don't resume in
   the wrong account; switching accounts is an explicit operation (copy the transcript with
   a backup, then resume) and comes later.
3. Refuse rather than guess: no "latest conversation in this folder". A failed resume sets
   `resume_failed`; the next open asks "start fresh or pick a conversation".
4. Refuse to resume a session running elsewhere (`location = elsewhere`), on another brain's
   server, or whose folder is gone, with a one-line reason.
5. Never auto-answer trust prompts. A new folder shows Claude's trust prompt; the console says
   so instead of reporting a failure.
6. Resume also checks the conversation's store belongs to an account this brain accepts.

**Adopt and untracked panes [C22].** `adopt PANE` and the untracked-pane list in `ls` identify
agents from `pane_pid`, `pane_start_command`, and the process tree read in-process (macOS
`proc_listchildpids` + `sysctl KERN_PROCARGS2` for argv and env; Linux `/proc`). The
conversation id comes from `--resume`/`--session-id`, `--id`, or the newest transcript for that
folder not held by another live pane. Restricted to the brain's folders (C15). No `ps`,
`pgrep` or `lsof`. **What adoption gives [U14]**: the session joins the tree with its title,
group and conversation id, and shows alive/dead (from tmux pane notifications), not working /
needs-you / done, since it started without moa's hooks. Its row offers **restart under moa**
(one key): close the pane and resume the same conversation in a moa-launched pane, which carries
the injected plugin and so full tracking.

### 6.8 Timers **[call C13]**
- **Short monotonic timers** on `tokio_util::time::DelayQueue` in the scheduler: wake-up
  settle and gap, quiet timers, ask delivery steps (0.5 s Enter delay, 20 s receipt),
  reconnect backoff, ACP watchdog. Set and cancelled by Core effects.
- **Wall-clock alarms** on one kqueue `EVFILT_TIMER` with `NOTE_ABSOLUTE |
  NOTE_MACH_CONTINUOUS_TIME` on macOS (timerfd `CLOCK_REALTIME` + `TFD_TIMER_ABSTIME` on
  Linux), armed for the earliest deadline and wrapped in `AsyncFd`. They keep time across
  Mac sleep and fire immediately on wake if the deadline passed; tokio's `Instant` stops
  while the Mac sleeps (R4 §5.1). Used for: idle-sleep deadlines, the minute clock, the
  local-day boundary, daily backups, due-date boundaries, retention warnings.
- Deadlines are **derived** from data at start (last activity + sleep-after, due dates,
  backup schedule), so there's no timers table. An idle brain with nothing due has no
  wake-ups except the minute clock while a client is attached.
- A time-zone change re-derives day boundaries (macOS posts it through `notify(3)` on a file
  descriptor; Linux watches `/etc/localtime`).
- **Animation frames are not polls**: panels run an animation interval only while an animated
  element is on screen (spinner 10 fps, blink, flash), and it reads no state.

### 6.9 Brains and routing **[calls C14, C15]**
- **One daemon per brain** (U11): its own socket, DB, tmux server, launchd agent, log. Nothing
  is shared at runtime. Isolation is enforced by the OS, not by a `BrainId` in every query
  (R4 §1.1). `moa.toml` is the only shared thing, and it holds config, not data.
- **Brain config (C15)** replaces the spike's hardcoded `PRIVATE_ROOTS` and account tables
  (lesson 36):
  ```toml
  [brains.default]
  folders = ["~/private", "~/.config"]   # folders this brain owns: adopt, project add,
  moa_account = "acct-b"                 #   untracked panes, seeding, new-session prefill
  open_account = "acct-b"
  [brains.work]
  folders = ["~/work"]
  moa_account = "acct-a"

  [accounts.acct-a]
  kind = "claude"
  store = "~/.claude-acct-a"
  brain = "work"                         # setup install acct-a --brain work --except …
  except = ["~/private", "~/.config"]
  [accounts.acct-b]
  kind = "claude"
  store = "~/.claude-acct-b"
  only = ["~/private"]                   # setup only: the account works with moa only here
  ```
- **The routing function** (`moa-core`, pure, one implementation used by the hook, `moactl`,
  the daemon's guard and setup): `route(account, cwd, opener) -> Option<Brain>`, with the
  spike's tested semantics (lesson 34): if the account has `brain = B`, a cwd under one of its
  `except` folders stays with the opener (or the default brain when there's no opener),
  anything else goes to B; with `only`, a cwd outside those folders gets `None` ("moa isn't
  available here"); otherwise the opener or the default brain (but see unlisted accounts below). Paths are compared on whole
  components after `~` expansion and canonicalization; empty entries are ignored (lesson 37).
  Isolation guards (v1 slice 2a): an `except` cwd never goes to B. It stays with the opener
  when the opener isn't B; otherwise it goes to the brain that owns the folder (the most
  specific configured folder wins), else `default`, and `None` when that would be B. For an account with limits, a cwd that is relative or contains `..`
  gets `None`. The same folder in two brains is a config error.
  A session's brain is fixed when it's first seen and never changes when its cwd moves.
- **Unlisted accounts don't write (owner, slice 2b).** When `moa.toml` lists any `[accounts]`, a `moactl` write from an account it doesn't list (its store, `CLAUDE_CONFIG_DIR` or `~/.claude`, matches no `store`) is refused in one line, so a forgotten work account can't write into the default brain. Reads still route to the default brain. With no accounts listed, routing is unchanged. `MOA_BRAIN` is an explicit override. The hook path is not covered by this.
- **Opening in another brain's folder is refused [C14]**: if `route(account, cwd, self)` isn't
  this brain, `open` says "~/work/x belongs to the work brain; open it from `moa work`". In the
  spike such a session would run on this brain's tmux server but be tracked by the other
  brain, which can't reach its pane (lesson 4). Refusing keeps pane and brain together.
- The daemon re-checks the route on every hook event (a guard against accidents, not a wall)
  and drops events that belong elsewhere, logging them.

### 6.10 Setup, accounts, CLI
- **Account discovery**: every `~/.claude` and `~/.claude-*` dir with `projects/` or
  `settings.json` (directory reads). State per account: tracked by which brain, limits
  (`brain`/`except`/`only`), account plugin installed, transcript retention setting, last used,
  unreadable. Later: `~/.codex*`.
- **Setup from the TUI and the CLI, same operations**: `moa setup` (interactive),
  `setup list|install|only|remove|skip [ACCOUNT] [--brain B --except F…] [FOLDERS…]`, and a
  setup card in the console that appears while an account is unconfigured (buttons: track
  here / only under these folders / send to brain… / not now), plus a Setup view reachable
  from the menu. Creating a brain from the TUI: name, folders, accounts; it writes `moa.toml`,
  registers the launchd agent, starts the daemon. Wording follows lesson 35 ("send acct-a's
  sessions to the work brain, except under ~/private").
- **First run for a new user**: check tmux ≥ 3.4 and the Claude Code version, find accounts,
  ask where projects live (no hardcoded folder), create the default brain, seed projects from
  git repos under its folders (name, path, last commit date and subject, read in-process with
  `gix`), register the launchd agent, attach. A Nix home-manager module is optional.
- **Per-account plugin** (opt-in "every session on this account knows moa"): a directory
  marketplace generated at `plugin/account/`, installed with `claude plugin marketplace add`
  + `claude plugin install moa@moa-local` (per `CLAUDE_CONFIG_DIR`, 60 s timeout, stdin
  closed; the one allowed CLI launch). It holds the `moa-peers` skill (allowed-tools for
  `moactl peers|ask|reply|asks`), `bin/moactl` (a symlink to the moa binary), and one
  synchronous SessionStart hook that computes the route once and writes `MOA_BRAIN` and the
  start cwd into `CLAUDE_ENV_FILE`, so later `moactl` calls in that session know their brain
  without `lsof` (lesson 29). Removal also deletes the empty keys Claude leaves behind.
- **Agent-facing CLI**: the spike's `moactl` verbs and flags are kept as the stable surface
  (moa's instructions, the skill and moa's habits use them; lesson 26): `ls`, `open`, `resume`,
  `focus`, `close`, `archive`, `sleep`, `term`, `set`, `group add|rename|rm`, `adopt`, `goto`,
  `layout list|show|apply|set`, `brief`, `attention`, `find`, `project`, `note`, `suggest`,
  `suggestion yes|no|later|ls`, `task ls|add|set`, `peers`, `ask`, `reply`, `asks`, `setup`,
  `import-herdr` (full flag list: R1 §2.13). Session names resolve as in the spike (unique id
  prefix, else title/project/group substring, exact title wins, ambiguous lists up to 8). Due
  dates accept today, tomorrow, +N, weekday names, ISO. Every verb supports `--json`; errors
  are one human line with a non-zero exit. Caller identity: `MOA_SESSION`, else
  `CLAUDE_CODE_SESSION_ID` looked up in `conversations`, else "unknown in <folder>".
- **herdr import** (`import-herdr`): parity. Records go to the brain their folder routes to;
  missing folders import as archived with the reason; live ones become `elsewhere`; dry-run
  report first (R5 §8.4).

### 6.11 moa, the central agent, and the console **[calls C12, C19, C20, C24]**
**Hosting [C12].** The daemon owns one ACP adapter process per moa account
(`claude-agent-acp` with `CLAUDE_CONFIG_DIR=<moa's store>`, `CLAUDE_CODE_EXECUTABLE=<the same
claude the sessions use>`, `CLAUDE_AGENT_ACP_PROTOCOL=v1`, `MOA_SESSION=moa`, stderr captured
to the log; lesson 66). Pin the exact `agent-client-protocol` crate version and the exact
adapter version (packaged by Nix); all ACP code sits behind `moa-agents::acp`, with
`agent-client-protocol-schema` plus a hand-rolled JSON-RPC loop as the fallback if the crate
churns (R3 §1.2). **The daemon always drains `session/update`** and persists every entry into
`feed`, whether or not a console is attached (adapter issue #1269 stalls otherwise).
Consoles never speak ACP; they subscribe to `feed`. Closing or crashing the console doesn't
touch moa's turn (lesson 54).
- **Turn watchdog**: no update for 5 min during a turn → `session/cancel`; unresolved 30 s
  later → kill and respawn the adapter and resume the thread; a card says so. A half-answered
  ask to moa fails with the reason (lesson 49).
- **Permissions**: `session/request_permission` is held in a pending table and shown as a
  card (`y once / a always / n no`, keys y/a/n when the input is empty, answered by option
  `kind`, never position). With no console attached, it counts as "moa needs you" in the
  status line. moa's agent dir carries an allow-list (`Bash(moactl:*)`, read-only git, `ls`,
  reads of the brain's folders) so most calls never ask.
- **Threads**: one per local day per account. Same day after a restart → `session/resume`
  when advertised (no replay; the feed is already in the DB), else `session/load` with
  dedupe by message id. A new day starts a thread with `systemPrompt {mode: append}` = moa's
  constitution (embedded, overridable) + the brain's scope file + the compact brief (~2 KB);
  without the `systemPrompt` capability, the brief goes into the first turn and the
  constitution into the agent dir's CLAUDE.md. Changing moa's account starts a new thread
  (lesson 62). **[C24]** When moa's account fails (auth, billing), a card offers switching to
  another account; nothing switches automatically.
- **Opening brief [C19]**: on the user's first presence after the local day boundary (a
  client attach or `client-active` mailbox message), not at midnight. Nobody pays for an LLM
  turn nobody reads.
- **Hidden prompts** (opening, wake-up, ask) are marked automatic by the message id the
  daemon sent, not recognised by text prefix (lesson 61). Wake turns show "· on its own".
- **Reports and wake-ups** (lessons 59, 60): turn-ended and needs-you reports with text queue
  per (session, kind), keeping up to 6 turns per session (earlier ones shortened to 500
  characters, the last up to 2600). Wake when auto is on, moa is idle, no permission is
  pending: after reports settle (45 s since the last) unless the oldest waited the full gap;
  at most one wake per 180 s; at most 8 sessions per wake. All three are settings. **One
  channel**: each report reaches moa exactly once, either in a wake prompt or prefixed to
  the user's next prompt (the spike's `since.sh` double feed is gone). Reports from moa's own
  folder are ignored. The console shows "moa is reading N session reports…".
- **Nags [C20]**: open tasks due by tomorrow ride on the opening brief and on wake-ups, at most
  once per (task, due, day); the dedupe is persisted in `nags`, so restarts don't re-nag.
  moa ends the brief with `⏰ Due today: …` / `⏰ 2 days overdue: …`, overdue first.
- **Asks to moa**: between turns, the oldest pending ask to `moa` becomes a card and a hidden
  ask prompt; the turn's final text becomes the reply ("moa's turn ended early: reason" when
  the stop reason isn't `end_turn`).
- **Sleep for sessions** is daemon logic (6.13), announced in the feed as one dim line.

**The console view** (parity, R1 §2.8):
- Feed entries: you, moa (Markdown), tool calls (one dim line each, `⚙ title: command` with
  ✓/✗/spinner, runs over 3 collapsed), system lines, errors, cards (ctl output, setup,
  help, permission, suggestion "✦ moa suggests", ask "✉ NAME asks moa"), session event lines
  (`▶ started`, `○ ended` (hidden when put to sleep), `⚠ needs you ↩ answer`, `● finished a turn
  ↩ open`, folded per session into "X · N turns since 21:10" moving to the bottom; turn text
  only when auto is off). Each moa turn grouped under `◆ moa  HH:MM` with a gutter.
- Markdown: headings with a bar, bullets by depth, numbered lists, task boxes ☑/☐, quotes,
  fenced code, rules, tables (box-drawn if they fit, else one bullet per row), bold, code,
  links (text only), coloured status glyphs; word wrap with hanging indents; buttons never
  split.
- Buttons: backticked `/commands` and `moactl resume|focus|open|close|archive X` (no flags);
  session buttons show the session's name (`↩ Lantern · Rust scaffold`, ≤ 40 columns),
  refreshed live. Tab/BTab hop between buttons when the input is empty, Enter presses, click
  presses.
- Input line with cursor and history, the `/` command palette (commands, then session titles
  after `/resume|focus|close|archive `, layout names after `/layout `), the button bar, and the
  commands `/attention /brief /ls /open /resume /focus /term /layout /close /archive /new
  /auto /sleep /suggestions /setup /help /quit` (plus hidden `/suggestion yes|no|later N`).
  Console commands run in the daemon with no LLM turn.
- Frame: `◆ moa · BRAIN`, `thread XXXXXXXX`, `◷ N for moa`, state (connecting / ⠋ thinking /
  ⚠ answer y/a/n / ● ready). "moa is thinking… Esc cancels". PgUp/PgDn, wheel, "↑ N more".
- Errors render once per message per 30 s; a failing entry is drawn degraded rather than
  breaking the frame (lesson 54).

### 6.12 Partners (peers and asks), suggestions, retention
**Peers and asks** (R1 §2.12; lessons 46–53). The daemon owns the protocol; nobody loops.
- `peers [--json]`: moa first, then every live session on this brain's server except the
  caller (id prefix, label, state, folder, last asked; JSON adds agent kind, project, cwd,
  doing).
- `ask TO TEXT [--timeout S]` (default 540 s, under agents' 10-minute Bash limit) is a long
  request. Refused: asking yourself, an unreachable peer (with the resume command), or a peer
  that's waiting on your answer (deadlock guard). Names prefer live sessions over ended ones.
- **Delivery** when the peer is idle (on its Stop event, or immediately if already idle): fail
  stale sent/received asks to it older than 2× the wait; atomically claim (pending → sent) only
  if no other ask to that peer is in flight; type the framed message
  `[moa · message from NAME · ask #N] …` with bracketed paste, Enter after 0.5 s; if no
  `turn.started` containing the marker (matched anywhere in the full prompt, ask id parsed)
  arrives within 20 s → failed ("a dialog or copy mode had the pane"). The peer's next Stop →
  answered with the **full** final message → the parked request replies.
- Timeout while pending → dropped ("stayed busy"); while sent/received → "no reply yet; `moactl
  reply ID` waits again", exit 0. Peer died → dropped or failed.
- Every moa-launched session's SessionStart context tells it about peers (the `moactl` command,
  the 600000 ms Bash timeout advice, the message marker).
- Later delivery paths behind the same trait: Claude's cross-session messaging socket or
  Channels (once verified and out of preview), `codex queue`/app-server for Codex.

**Suggestions** (R1 §2.9; lesson 63). `suggest TEXT --do "VERB ARGS"…`: actions are parsed and
validated at creation against the whitelist (task, set, archive, close, group, note, project),
so failures are moa's, not the user's; duplicate open text isn't added again (the answer is `already suggested (#N)`). Yes runs the actions
**in-process** in the daemon (the spike launched a Python process per action), records the
result, logs a ui event; without actions, yes is a signal delivered to moa through the one
channel. Cards update everywhere when answered anywhere. moa's rules (≤ 2 per wake, never
repeat open or rejected, phrase as a question that says what yes does, morning picks as one
suggestion) live in moa's constitution.

**Retention (U10).** The rule: keeping everything is the default; deletion is opt-in, warned,
and names what will go. moa itself deletes nothing (close keeps the conversation, archive hides,
sleep frees memory, `group rm` only removes empty groups). Claude Code deletes transcripts after
`cleanupPeriodDays` (default 30), so v1 must at least: record `transcript_present` per
conversation and check it before resume; show each account's retention in setup; and arm a
wall-clock warning before a tracked conversation reaches its account's cleanup age, as a card
naming the sessions. **How it actually protects them is an open item** (section 12, Q1).

### 6.13 TUI: panels, layouts, keys, mouse, picker, sleep **[calls C16, C17, C28]**
**Panels as long-lived processes [C16].** Each panel pane runs `moa panel` (ratatui 0.30 +
crossterm 0.29, true colour, SGR mouse, bracketed paste). It connects with its pane id; the
daemon assigns its view (`console`, `tree`, `plan`, `status`, `today`, `activity`, `projects`,
`picture` (tabbed), `picker`, `empty-slot`, `help`, `setup`) and can reassign it. Views come
from `moa-view`. Redraw only on daemon pushes, input, resize, or the animation interval while
something animated is visible. Measured 7.8 ms exec-to-frame (R4 §6.3).
- **Layouts** keep the spike's format and file (`layouts.json`: `{"split":"h|v",
  "children":[…], "size"}` / `{"panel": NAME}`, validated: known names, ≤ 1 console, ≤ 1
  agent; presets cockpit, focus, classic, triage merged in, deleted presets come back).
  `layout apply` **moves** existing panes (swap/join/break) and resizes, never restarts them;
  views it needs that aren't on screen come from **parked** panel panes in the hidden `_moa`
  session; panes it doesn't need go back there. So switching layouts launches nothing; only
  the very first use of a view (or Repair after a crash) spawns a panel. The live agent in
  the spot moves with swap-pane; leftover agents go back to their own windows, named by
  title. Splitting the cockpit is refused ("the cockpit is a layout").
- **Lazy cockpit [C28]**: the daemon builds the cockpit when a user first attaches, not at
  login.
- `goto PANEL` selects a panel, re-applying the layout if it's missing; `focus Q` resumes if
  needed and swaps the session into the agent spot (the previous occupant returns to its
  window; an empty slot is parked), else selects its window.
- Panels publish their selected row's folder to the daemon (`ui.here`), which `term` uses for
  "here" (no tmux option writes).
- Frames: rounded, title on the top edge, drawn by the panel in the default colour; tmux colours
  them per pane with pane-level `window-style fg=#4c566a` / `window-active-style fg=#88c0d0`,
  so focus shows with no events (lessons 11, 12). Border gutters `pane-border-lines spaces`,
  `pane-border-status top` with `@moa_label` for unframed panes.

**Views** (parity, R1 §2.3): tree (header counts; nested groups with `├─ └─ │` guides; empty
groups; fold; rows with glyph, title, account ≥ 34 cols, path ≥ 76 cols, age; keys `n` new
(prefilled from the group's latest session, title, account), `+` group, `m` move, `r` rename
(groups rename their subtree), `D` delete empty group, `a` archive, `x` close with y/N),
plan (date and "✓ N done today"; per project sorted by needs-you, priority, you-tasks, due,
name; session-needs-you rows, tasks you → agent → wait; ★ ▸ ◐ ◷ marks; P0/P1; due chips red
overdue, yellow today, blue within a week, dim later; inline when narrow; idle-project footer;
`d` done, `x` drop), status (live sessions by urgency group, state age, why; empty message),
today (legacy view, kept), projects (expand to summary/tasks/sessions; `n` new session with the
default account, `N` choose the account: replaces the spike's two hardcoded accounts),
activity (last 300, coloured by who), picture (tabs + 7-line activity strip flashing ✦ for
4 s, `moa ⠋` brand). Common: j/k/↑/↓, g/G/Home/End, PgUp/PgDn, Enter/Space, footer hints or
messages or y/N confirm, an inline footer editor (Enter, Esc, Backspace, Ctrl-u, Ctrl-w), the
scroll model that keeps the selected item and its headings whole (lesson 20), display width
by grapheme (lesson 18).

**Mouse.** Wheel scrolls the view ±3 and drags the selection along so keys never act on a row
you can't see; a left click selects and acts (herdr-style); clicks on headings are ignored.
Help says "hold Shift to select text".

**Picker [C17].** `Ctrl-a /` (and `s`) link a parked picker panel's window into the user's
session and select it; Enter focuses or resumes the pick (or selects a terminal's window),
Esc returns to the previous window; the window is unlinked afterwards. Two tmux commands, zero
launches. **Both forms [U17]**: `Ctrl-a /` opens the default form (a setting, popup by default as in
the spike), `Ctrl-a s` the other; the menu offers both. The popup is `display-popup -E -w 80% -h 70%
moa pick`, the same view in a short-lived `moa panel --once` process. Same content as the spike: query line and counts, fuzzy multi-term subsequence match
over title/group/state/folder/id with contiguous matches first and highlights, order live by
urgency → open terminals → ended/asleep/elsewhere by recency (≤ 300), animated glyphs, the
spike's keys. Reason: `display-popup` always launches a process (R2 §8), and a user-initiated
popup is acceptable only if there's no event-only design; this is one. Cost: the picker is
full-window rather than an overlay, which is why the popup form exists too (one ~8 ms launch per
open, user-chosen). Help (`Ctrl-a ?`) uses the same
mechanism with the `help` view instead of `less`.

**Keys** (moa key table on `Ctrl-a`, no prefix; the status line lists the keys while it's held):
| Key | Action | Key | Action |
|---|---|---|---|
| `c t p g` | go to console / tree / plan / agent spot | `1 2 3 4` | layouts cockpit / focus / classic / triage |
| `/` `s` | picker | `r` | repair (re-apply the layout, respawn dead panels) |
| `n` | new session (prompt folder, title; prefill the brain's first folder) | `z` | zoom |
| `$` | terminal here | `[` `]` | previous / next window |
| `\|` `-` | split the current agent/terminal pane with a shell in its folder (refused in the cockpit) | `a` | send a real Ctrl-a |
| `m` | back to the cockpit | `?` | keys and help |
| arrows (repeat) / `h j k l` (no repeat) | move between panes | `Space`, `Ctrl-a` | the full menu |
Prefix (Ctrl-b) `Space` opens the same menu (instead of next-layout), `m` back to moa, `C`
repair. The menu lists every action above with its key. Pill colours: blue normal, orange while
the key table is active, yellow in copy mode.

**Plain terminals**: `term [PATH]`, `Ctrl-a $`, `/term`: a shell window `⌨ name` in "here",
not a session in the brain, listed by the picker.

**Sleep (parity + U7).** Eligible: live, display state idle (not working, waiting, question,
done, failed), not a panel or terminal, not visible, not in an open ask, last activity older
than the threshold (default 6 h, `0` = never, `/sleep` changes it live), and the bound pane
verified as running that conversation. Event-driven: each idle session gets a wall-clock
deadline at last activity + threshold; when it fires and the session isn't eligible (for
example it's on screen), it re-evaluates on the next relevant event instead of polling.
Sleeping: in the agent spot → park an empty slot there first; then kill the pane; set
`asleep_at` (a flag, so the SessionEnd it causes can't race it; lesson 40). One dim line
"💤 put N idle sessions to sleep: names · resume from the tree" and a ui event.
`sleep Q…` and `sleep --idle H [--dry-run]` remain.

**After a reboot or crash (U7).** Daemon start: drain the spool; connect (creating the server
if needed); compare the server generation with stored bindings. A different generation →
every session that was live becomes asleep, bindings cleared. Same generation → verify each
binding with one `list-panes -a -F 'P #{pane_id} #{@moa_session} #{pane_pid} #{pane_dead}…'`;
a pane that vanished with no SessionEnd received → asleep (resumable; we don't know it was
closed on purpose); panes running a tracked conversation without a binding → bound. The tree
looks as it was left; Enter resumes one. Nothing auto-resumes.

### 6.14 Colours
The spike's palette, exactly, in 24-bit (no 256-colour mapping; lesson 17). Theme file,
hot-reloaded.
| Token | Hex | Use |
|---|---|---|
| text | `#d8dee9` | body |
| bright | `#eceff4` | emphasis |
| dim | `#8c9ab3` | secondary text |
| line | `#4c566a` | inactive frames, rules |
| surface | `#3b4252` | cards, pills |
| sel | `#5e81ac` | selection |
| dark | `#2e3440` | background accents |
| blue | `#88c0d0` | accent, active frame |
| cyan | `#8fbcbb` | secondary accent |
| green | `#a3be8c` | done, live |
| yellow | `#ebcb8b` | needs you, today |
| orange | `#d08770` | question, key-table pill |
| red | `#d57780` | P0, overdue, errors |
| magenta | `#b48ead` | moa |
Truecolor goes straight to the terminal through crossterm; tmux needs `terminal-features
…:RGB` for the outer terminal (the daemon's config sets it for terminals known to have RGB
and setup explains how to add others). If the client lacks RGB, fall back to the nearest
256-colour values. Glyphs in chrome are single-width (✦ ✉); status pills use Nerd Font half
circles with a plain fallback setting for people without the font. TERM is never changed for
children (lesson 16).

### 6.15 Live update and restart notices **[call C21]**
1. **State is never only in memory**: SQLite plus tmux. `kill -9` of the daemon is safe:
   restart = load, drain spool, one `list-panes`, reconcile.
2. **Daemon re-exec**: when the binary changes (FSEvents/inotify on the profile directory, or
   `target/debug/moa` in dev) or on `moa upgrade`, the daemon waits until moa is between turns,
   flushes, clears `FD_CLOEXEC` on the listening socket and `execv`s the new binary with the
   fd passed in (`MOA_LISTEN_FDS`). Same pid; no connection is refused. The control client and
   ACP adapter restart; the thread resumes.
3. **Clients follow**: a panel whose build differs from the daemon's re-execs itself in its
   pane, keeping the pane. The whole product upgrades as a unit, so the spike's
   half-updated-modules crashes can't happen (lesson 55).
4. **Config hot reload**: `moa.toml`, `layouts.json`, theme, scope files, moa's constitution.
   tmux bindings and options are re-applied over the control channel on every connect, so tmux
   config changes need no manual `source-file`.
5. **Restart notices**: every upgrade publishes what it needs, in one console line: "nothing",
   "daemon restarted itself", "N running sessions use the old hooks; they get the new ones
   when resumed", or "re-attach your terminal". Every PR description says the same.

### 6.16 Clients beyond the TUI (context, not a decision; U9)
The protocol is client-agnostic: everything a panel does is a request or subscription. The
leading direction for a desktop GUI is a daemon-served web UI (axum, server-rendered `moa-view`
fragments over SSE with one vendored script such as Datastar or htmx, no npm, `127.0.0.1` with a
per-brain token), optionally wrapped in a Tauri 2 or bare `wry` window for native chrome; GPUI
is the runner-up if "no web view" becomes a requirement. Live terminals would come in stages:
open in the terminal app, then read-only previews, then interactive (R6).

### 6.17 Worktree lifecycle (model only) **[call C23]**
The spike has none, and every tool that left worktree cleanup to a prompt leaked them (R5 §7.3).
Designed now so the schema doesn't block it, built after the switch-over: a `workspaces`
table (path, branch, base_ref, base_commit_sha, created_by moa/claude/user, lock state, last
clean check) linked from tasks/sessions; worktrees moa creates carry a marker and a
`git worktree lock` while an agent runs; ending is a verb on the task (finish, abandon, park);
a daily sweep auto-removes only owned + ended + clean + pushed/merged + unlocked + old ones,
trash-by-move first, and shows the rest as a card (R5 §8.2). Uses `gix` in-process.

---

## 7. Parity checklist

The spike is frozen (U13), so this list and R1 are the fixed target.
Every row is a feature from R1 §2; "Slice" refers to section 10. "Change" is what v1 fixes.
| Feature (R1 §) | v1 home | Slice | Change vs spike |
|---|---|---|---|
| `moa [BRAIN] [--account A]`, refuse nesting (2.1) | `moa` front door, `moa.toml` | 3, 8 | Inside moa's own server: switch client instead of refusing |
| Brain = own tmux server, DB, scope (2.1) | daemon per brain | 2 | Daemon process per brain |
| Explicit env for children, unset inherited MOA_* (2.1) | launch env allow-list | 4 | Per-pane `-e`; server env clean |
| First-run seeding from git (2.1, 2.18) | setup + `gix` | 8 | Folders from brain config, not hardcoded; no hardcoded notes |
| Already running + `--account` updates defaults (2.1) | `moa.toml` + daemon | 8 | |
| Layout tree, validation, presets, merge (2.2) | `moa-core` layouts, `layouts.json` | 5 | Same file |
| `layout apply/list/show/set` (2.2) | daemon + parked panels | 5 | Moves panes; no respawn per switch |
| `goto`, `focus`, agent spot swap (2.2) | daemon | 5 | |
| Panel tagging, labels, frame colours (2.2) | daemon tmux ops | 5 | |
| Repair, cockpit split refused (2.2) | keys + daemon | 5 | |
| Panel common chrome, rows, keys, mouse, scroll model, footer editor (2.3) | `moa-tui`, `moa-view` | 5 | Width by grapheme |
| `@moa_here` publishing (2.3) | `ui.here` request | 5 | No tmux option writes |
| Animation (spinner, blink, flash) (2.3) | panels | 5 | Interval only while visible |
| Self-reload of panels (2.3, 2.17) | build-id re-exec | 9 | Whole-unit upgrade |
| tree view and its keys (2.3) | `tree` view | 5 | |
| plan view, due chips, d/x (2.3) | `plan` view | 5 | |
| status view (2.3) | `status` view | 5 | Adds failed and quiet |
| today, projects, activity, picture views (2.3) | views | 5 | projects `n/N` use account choice |
| Five live states, why text, idle-ping filter, seen (2.4) | `moa-core` live model | 4 | + failed, quiet; PermissionRequest; seen in DB |
| Status line: pills, urgent-only window list, counts, clock, bell (2.5) | daemon tmux ops | 3 | Minute-boundary clock, no status-interval |
| Ctrl-a key table, menu, prefix keys, help (2.6) | tmux config + mailbox | 3, 5 | No `run-shell`; help is a view |
| `moamenu` 4 s messages (2.6) | daemon `display-message` to the client | 3 | |
| Picker (2.7) | parked `picker` view | 5 | Linked window, no popup process |
| Console: ACP connection, account, threads per day, load/new (2.8) | daemon ACP host | 6 | Daemon-hosted; resume; feed persisted |
| Hidden prompts, "on its own" (2.8) | feed `automatic` by message id | 6 | Not by text prefix |
| Agent exit, Esc cancel (2.8) | ACP host, watchdog | 6 | Watchdog + restart + resume |
| Feed rendering, Markdown, tool lines, turn grouping (2.8) | `moa-view` feed + `moa-tui` | 6 | |
| Input line, commands, palette, button bar (2.8) | console view + daemon commands | 6 | Commands run in-process |
| Buttons from backticks and moactl output, names on buttons, Tab hopping (2.8) | console view | 6 | |
| Cards: ctl, setup, permission, suggestion, ask (2.8) | feed cards | 6 | |
| Session reports, folding, cursor (2.8) | Core + feed | 6 | Persisted feed, no cursor loss |
| Wake-ups: settle/gap/batch, due-by-tomorrow, instructions (2.8) | Core timers | 6 | One channel |
| Asks to moa (2.8) | Core + ACP host | 7 | |
| Sleep / doze (2.8, 2.11) | Core deadlines | 4 | Deadline timers, no 600 s loop |
| Robustness: logged errors, dedupe (2.8) | tracing + views | 2, 5 | Degrade the entry, not the frame |
| Suggestions: suggest, validate, yes/no/later, ls (2.9) | Core | 6 | Actions run in-process |
| Due dates as data, relative input, nagging (2.10) | Core | 2, 6 | Persisted nag dedupe |
| `sleep` verbs, eligibility, display 💤 (2.11) | Core + views | 4 | Pane verified by generation |
| peers, ask, reply, asks, delivery, guards, failure modes (2.12) | Core + tmux | 7 | No asker loop; daemon delivers on Stop |
| Peers note at SessionStart; plugin skill (2.12, 2.15) | hook reply + plugin | 4, 8 | |
| Every moactl verb and flag (2.13) | `moa` CLI → socket | 2, 4, 7, 8 | Same names; `--json` everywhere |
| Session name resolution (2.13) | `moa-core` | 2 | |
| `open`, `resume`, `focus`, `close`, `archive`, `term`, `set`, `group …`, `adopt` (2.13) | daemon | 4 | Recorded-store resume; no `ps`/`pgrep` |
| `brief`, `attention`, `find`, `project`, `note`, `task …` (2.13) | Core | 2 | |
| `import-herdr`, `elsewhere` state (2.13, 2.18) | store + CLI | 8 | Routed per brain |
| Untracked-pane detection in `ls` (2.18) | `moa-platform` proc reads | 4 | In-process |
| `ui` events (2.18) | events + activity | 2 | |
| Account discovery, `setup` interactive/list/install/only/remove/skip (2.14) | setup in daemon + CLI + TUI | 8 | TUI setup; brain creation from TUI |
| Hook install with marker, backup, atomic writes (2.14) | injected plugin; settings fallback | 4, 8 | No settings edits for moa-launched sessions |
| Plugin install/remove via Claude CLI (2.14, 2.15) | setup | 8 | Plugin generated, bin is a symlink |
| Routing: only / brain / except; never moves brains (2.14) | `moa-core::route` | 2 | One function; open refused across brains |
| Tracking is opt-in per launch (2.14) | `MOA_SESSION` gate | 4 | |
| moa's instructions, scope per brain, briefing (2.16) | embedded constitution + scope + brief | 6 | systemPrompt; no `since.sh` |
| moa's allow-list (2.16) | agent dir settings | 6 | |
| Hot reload, "say when to restart" (2.17) | 6.15 | 9 | |
| Layouts file, env files, scope files, setup_skipped (§1 files) | `~/.config/moa/` | 1, 8 | Out of the repo |

## 8. Lessons from the spike and the rule each becomes
Numbers refer to R1 §4, which has the full list of 75.
| # | Lesson | Rule in v1 | Where |
|---|---|---|---|
| 1, 4, 14† | Pane ids are reused after a server restart; panes from another server | Bindings carry the server generation and pane pid; never trusted across generations | 6.6 |
| 2 | `display-message -t %gone` exits 0 | Never rely on tmux exit codes; structured replies and notifications | 6.6 |
| 3 | A session that died without SessionEnd stays "idle" | Liveness from pane events; reconcile on start | 6.6, 6.13 |
| 5, 6 | Env inherited across tmux servers; PATH reset hid stubs | Explicit env allow-list per pane; absolute tool paths from config; tests inject stubs by config | 6.7, 11 |
| 7 | tmux inside tmux | Detect nesting; switch client on moa's own server | 7 |
| 8, 10, 15 | `-f` lost user config; Space scrambles; arrows repeat | Source the user's config first; own every layout-breaking key; repeat only arrows | 6.6, 6.13 |
| 11, 12 | Focus reports fail for moved panes; tmux can't round borders | Frame colour via pane styles; panels draw frames | 6.13 |
| 16–19 | TERM leak, 256-colour murk, emoji widths | Truecolor via crossterm, never touch TERM, width by grapheme, tested glyph set | 6.14 |
| 20, 21 | Scroll-to-top bug; mouse needs to be requested | Keep the selected item and its headings visible; wheel drags selection; click acts | 6.13 |
| 22–24 | Empty `last_assistant_message`; data in argv; head-only truncation | Payload over a socket; fall back to transcript tail only for previews; truncate by characters keeping head and tail | 6.7 |
| 25 | Idle notifications made everything ⚠ | Only permission/elicitation means needs you; PermissionRequest is the source | 6.7 |
| 26 | Permission rules match the command as written | Stable `moactl` name, allow-listed, documented | 6.10 |
| 27 | Pasted prompts arrive wrapped | Match the ask marker anywhere; the marker carries the ask id | 6.12 |
| 28, 30, 33 | Plugins need the CLI; startup note reaches only moa's sessions; running sessions pick up hooks on restart | Injected plugin for moa sessions; optional account plugin; announce restarts | 6.7, 6.10, 6.15 |
| 29 | Plugin couldn't find the session's start folder | Route once at SessionStart into `CLAUDE_ENV_FILE`; daemon registry by id | 6.10 |
| 31 | Hooks fire for moa's own thread | `MOA_SESSION=moa` is never a worker | 6.7 |
| 32 | Editing users' settings | Marker, backup once, atomic, symlink-safe, others untouched, unreadable reported | 6.7 |
| 34, 35, 37 | Brain isolation; confusing wording; empty path entries | One routing function, whole-component matching, wording tested with the user | 6.9 |
| 36 | Hardcoded private roots and account maps | Per-brain folders and accounts in config | 6.9 |
| 38, 39 | sqlite3 creates files; migrations on every connect | Daemon is the only opener and writer; versioned migrations once | 6.5 |
| 40 | Asleep raced SessionEnd | Orthogonal facts in separate columns | 6.5 |
| 41, 42 | Empty groups; dates in titles | Groups table; due as data with relative input | 6.5 |
| 43 | Local-time text timestamps; replay without times | UTC instants; persisted feed with times | 6.5, 6.11 |
| 44 | Status logic duplicated in four files | One live model in the core; every view subscribes | 6.7 |
| 45 | Tables grow; huge `since` dumps | Keep data; cap what's fed to the LLM | 6.5, 6.11 |
| 46–53 | Ask protocol fixes | Atomic claim, 20 s receipt, drop on timeout, deadlock guard, 9-minute wait, marker id, bracketed paste | 6.12 |
| 54 | Console died silently; error loops | Logged, deduped errors; degrade the entry; ACP in the daemon; Repair | 6.11, 6.13 |
| 55 | Hot reload crashed on half-updated files | Upgrade as a unit; re-exec; supervised panels | 6.15 |
| 56–58 | Passive master; 9 commands to see what runs; 11.6 KB brief | moa speaks first; registry in the daemon; 2 KB brief + verbs | 6.11 |
| 59, 60 | Relaying; reports fed twice; wake cost | Fold turns; one channel; settle/gap/batch budget | 6.11 |
| 61 | Hidden prompts recognised by text | Marked by message id | 6.11 |
| 62 | moa's account died, moa with it | Account per thread; failover card | 6.11 |
| 63, 64 | Unclear suggestions; ids on buttons | Validated actions, "what yes does"; names on buttons | 6.11, 6.12 |
| 65 | Nag state in memory; no timer | Persisted nags; wall-clock day boundary | 6.11 |
| 66 | ACP stderr discarded | Captured to the log | 6.11 |
| 68–70, 73 | Scale; tree upkeep; alive; terminals | Urgent-only status line, picker, status view; moa tidies; animations; terminals listed | 6.13 |
| 71 | tmux must be invisible | moa key table and menu | 6.13 |
| 72 | Conversation ids surviving restarts is essential | Conversation history, recorded store, asleep after reboot | 6.5, 6.7 |
| 75 | Sandbox testing worked | Same harness, systematic | 11 |
| (process map) | 8–15 launches per hook, Python per moactl, 10 Hz loops | Allowed-launch list; exec-form hook; zero-launch keys | 6.1 |
†14 = splitting the cockpit breaks the layout model; covered in 6.13.

## 9. Migration and switch-over **[calls C5, C25]**
Goal: one switch, live agents untouched, everything reversible.

**What carries over**: both brains' DBs (imported), scope files, layouts (same file), account
config (from the spike's env files, hook markers and settings), the live tmux servers (adopted),
today's moa thread (resumed by id on the same account), the herdr import state (already in the
brains).

**Before the switch**
1. All slices 0–10 done and green; the switch rehearsal test passes (section 11).
2. **Spike forwarders [C25]**, a small spike PR merged well before the switch: `track.sh`, the
   plugin's `bin/moactl` and `moa-proto/moactl` check for `~/.local/state/moa/v1-live` and, if
   it exists, forward (`exec moa hook claude --from-spike` / `exec moactl "$@"`). Without the
   flag nothing changes. Why: running sessions keep the hook config they started with, but
   the script file runs fresh each time, so this is the only way spike-launched agents can
   report to v1 without being restarted. `--from-spike` maps by conversation id (no
   `MOA_SESSION`) and picks the brain from the spike's `MOA_DB` path, recorded per brain in
   `moa.toml` at import. v1-launched sessions don't set `MOA_TRACK`, so a spike settings
   hook never double-reports them.
3. **Dry run** (`moa switch --dry-run`, any number of times): snapshot both spike DBs, import
   into a temp location, print a per-record report (ok / warn: pane unverified, transcript
   missing, account unknown, invalid suggestion action / skip) and per-table counts compared
   with the source; export back to spike schema and diff against the source.

**The switch** (`moa switch`, each step logged, stops at the first failure)
| # | Step | If it fails |
|---|---|---|
| 1 | Backups: `VACUUM INTO` both spike DBs; copy `layouts.json`, env and scope files, account settings; snapshot `list-panes -a` of each server | Nothing changed |
| 2 | Quiesce the spike console: wait until moa isn't mid-turn; fail in-flight asks with "moa is switching over" | Nothing changed |
| 3 | Write the flag file. From now on spike hooks and moactl forward; with no daemon yet, hooks spool | Remove the flag |
| 4 | Stop the spike console process (its adapter exits; the thread survives on disk) | Restart the spike console |
| 5 | Import both brains into new v1 DBs; write `moa.toml`; move scope files out of the repo | Remove the flag; spike unchanged |
| 6 | Start the v1 daemons (launchd). Each drains the spool, connects to the live server, removes the spike's tmux hooks, re-binds keys and options, creates `_moa`, verifies panes and binds sessions (`@moa_session`) | `moa switch back` |
| 7 | Apply the current layout: v1 panels replace the spike's panels; the agent in the spot moves, never restarts; spike panel processes end | `moa switch back` |
| 8 | Resume today's moa thread by id on the same account (else start a new one) | Start a new thread |
| 9 | Verify: the same agent panes alive as in the step-1 snapshot (count and ids), tree counts match the import report, a test key reaches the daemon, the next agent turn updates its state | `moa switch back` |

**Fallback** (`moa switch back`, kept working until v1 works like the spike, U20; no agent is killed):
stop the v1 daemons (tmux servers untouched); export v1 brains into spike-schema DBs written to
the spike's paths (after backing up the frozen ones), so work done in v1 isn't lost; delete the
flag (spike hooks write the spike DB again); re-source the spike's tmux config; run the spike
launcher, which attaches and rebuilds its cockpit. Sessions launched by v1 keep running; the
spike knows them from the export but receives their hook events only after they're resumed
under the spike (they appear in the spike's untracked list and can be adopted). The export is
lossy only for v1-only facts (conversation history beyond the current id, failed/quiet states,
feed).

**Once v1 works like the spike [U20]** (an hour or two days): delete the spike, retire the forwarders, remove `# moa-spike` hooks from account
settings once no spike-launched session is live (the daemon knows; it offers this as a
suggestion card), archive the frozen spike DBs with the backups, and lift the additive-only
migration rule.

## 10. Build slices **[call C26]**
Each slice is testable on its own, in the sandbox, against copies of the brains, with the
stub ACP agent. The user keeps using the spike throughout.
| # | Slice | Done when |
|---|---|---|
| 0 | Workspace, Nix flake (crane), CI `ci` check (fmt, clippy, nextest, tmux installed), `moa-testkit` skeleton: sandbox tmux server, stub ACP agent, synthetic spike-schema fixture brain | CI green on macOS and Linux (U15) |
| 1 | Store: v1 schema and migrations, backups, **spike import** with dry-run report, **spike export**, round-trip tests | Both real brain copies import locally with an all-ok/warn report; export round-trips |
| 2 | Core + daemon skeleton + protocol + `moa-client`: snapshot/patch, build match, data verbs (`task`, `note`, `project`, `suggest`, `suggestion`, `brief`, `attention`, `find`, name resolution, due parsing), routing function, `moa.toml`, logging, launchd/autostart with lock | `moactl` data verbs work against a copied brain via the daemon; no tmux needed |
| 3 | tmux layer: parser (fuzzed), control client, reconnect, mailbox, key table and menu, focus model, status line and clock, server adoption | Keys, menu and status line work in the sandbox with zero spawns per key |
| 4 | Agents: `moa hook` + spool, injected plugin, envelope, live-state model, open/resume/focus/close/archive/set/group/term/adopt/ls, rotation, recorded-store resume, sleep deadlines, reboot → asleep, quiet timer, untracked panes via libproc | Fake agents emitting recorded hook payloads drive every state; kill the server and sessions come back 💤 |
| 5 | TUI: `moa panel`, every view, layouts with parked panels, picker, help, mouse, colours, animations, Repair | TestBackend snapshots at many widths; sandbox e2e of layouts, picker and clicks |
| 6 | moa: ACP host (pinned), threads, feed, console view, Markdown, buttons, palette, cards, permissions, wake-ups, nags, suggestions, opening brief, watchdog | Stub ACP scenarios: permission, slow turn, hang → watchdog, crash → resume, wake batching |
| 7 | Partners: peers, ask/reply/asks, delivery and guards, asks to moa, peers note, skill text | Two fake agents and moa exchange asks, including every failure mode |
| 8 | Setup and brains: discovery, setup CLI + TUI, brain creation, account plugin, settings-hook fallback, first-run seeding, herdr import, retention display and warnings | A fresh sandbox HOME goes from nothing to a working cockpit using only the TUI |
| 9 | Live update: re-exec with inherited socket, panels follow the build, config hot reload, restart notices | `cargo build` during a sandbox session updates everything with no lost state |
| 10 | Switch-over kit: spike forwarders PR, `moa switch --dry-run`, `moa switch`, `moa switch back`, rehearsal test | Rehearsal passes repeatedly; then the real switch |
| — | **Switch-over** | |
| 11+ | After: public release and packaging (U16), Codex agent kind, worktree lifecycle, the retention mechanism (once decided), `type: http` hooks if measurements justify, Claude messaging-socket delivery, web UI/GUI, collectors | |

Order rationale: data first (it's what must carry over), then the daemon that owns it, then
tmux (needed by everything visible), then agents (needed by views), then views, then moa (the
most churn-prone dependency, isolated late but before the switch), then the switch kit last,
when everything it relies on exists.

## 11. Testing strategy
1. **Core unit tests** (most of the logic): inputs in, effects out, `insta` snapshots. Property
   tests for wake batching, the ask state machine, routing, event ordering, sleep eligibility,
   due-date parsing. Time is an input, so there are no sleeps.
2. **Anti-goal tests**: one spawn module with a counter; scripted flows (keys, menu, picker,
   layout switch, suggestion yes, focus, ask) assert **zero spawns**. An idle daemon test asserts
   no wake-ups beyond scheduled deadlines over a period. A brain-isolation test routes work
   fixtures and asserts nothing lands in the private brain's DB.
3. **Store tests**: every migration on an empty DB and on the synthetic spike fixture;
   `validate()`; import → export round-trip; a **local-only** test target runs against copies of
   the user's real brains (never committed).
4. **tmux tests**: sandbox servers `-L moa-test-<rand>` with env set before any pane exists
   (lesson 5); parser fuzzing (cargo-fuzz); notifications, mailbox, keys via `send-keys -K`,
   pane identity across `kill-server`; a version matrix (3.4 and 3.8) in CI where available.
5. **Hook tests**: recorded payloads (synthetic, scrubbed) piped into `moa hook`; daemon-down
   spool and drain; out-of-order async events; rotation via `/clear`.
6. **ACP tests**: the stub agent (same SDK, Agent role) with scripted turns, permission
   requests, hangs, crashes, load/resume; a smoke test against a recorded real-adapter
   transcript when the pinned version changes.
7. **TUI tests**: ratatui `TestBackend` snapshots per view at widths 40–200, with wide glyphs
   (💤, CJK); one sandbox e2e comparing `capture-pane` with expectations; SGR mouse events.
8. **Switch rehearsal**: a sandbox server running the actual spike code from the repo with fake
   agents and copied brains; run `moa switch`; assert no pane died and every session is mapped;
   run `moa switch back`; assert the spike works with the exported data.
9. **Dogfooding**: per the user's preference, each slice gets a runnable version in the sandbox
   for the user to try, with "restart needed: …" stated.
Tests may launch processes; the product may not (section 6.1).

## 12. Open questions
| # | Question | What the answer changes |
|---|---|---|
| Q1 | **Retention mechanism** (U10; the user wants to discuss it): raise `cleanupPeriodDays` in each account's settings (with consent), archive transcripts of tracked conversations into the brain and restore before resume, or warn only? | Whether setup edits accounts' settings, whether the brain grows a transcript archive (disk, privacy), and what slice 8 builds beyond warnings |
| Q4 | Opening a session in another brain's folder: refuse (current call) or forward the request to that brain's daemon? | A narrow cross-brain request path, which isolation currently rules out |
| Q8 | Should the quiet timer's default (10 min) and the PostToolUse subscription be tuned after measuring at ~30 agents? | Hook volume and how quickly an interrupted session stops looking busy |

## 13. What's left to the builder
- Exact crate versions (pin at build time; ACP crate and adapter pinned together and smoke-tested).
- Exact SQL, indexes, and the per-table import/export code.
- The layout-apply algorithm (pane moves, sizes, parking) and the tmux command sequences.
- `moa-view` structures and the Markdown renderer's internals.
- Verifying on the supported tmux versions: `\n` escapes in `set-buffer`, a pane-title-change
  hook (for interrupts), `-l` on `display-message`, menu targeting.
- Verifying Claude Code details at build time: `CLAUDE_CODE_PLUGIN_DIRS` with hooks in an
  injected plugin, exec-form hooks with `async`, `CLAUDE_ENV_FILE` from a plugin SessionStart
  hook, `--session-id` with `--name`.
- Log format, error wording (one human line; say it the way people think), help text.
- The Nix flake details, the home-manager module, and the launchd plist.
- Glyph set tests against tmux's widths.
- Measuring hook cost at scale and narrowing the PostToolUse matcher if needed (Q8).
