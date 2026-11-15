# 4. Rust architecture for moa

*Research for the moa design phase, 2026-11-15. Benchmark sources: `scratchpad/bench/` (hook.rs, server.rs, tuistart/). Crate versions are the latest stable on
crates.io that day. Numbers marked **measured** were taken on this Mac (aarch64-darwin,
endpoint-security agent active) with hyperfine; everything else is cited.*

## TL;DR (the recommendation)

- **One daemon process per brain**, same binary (`moa daemon --brain work`), each with its
  own socket, `brain.db`, tmux server, launchd agent and log. Nothing is shared at runtime;
  a small static `~/.config/moa/brains.toml` routing table is the only shared thing.
- Inside the daemon: a **functional core + IO actors**. One `Core` task owns the in-memory
  model and handles every input (hook event, tmux notification, ACP message, client
  request, timer) sequentially through `handle(&mut State, Input, Now) -> Vec<Effect>`.
  IO actors (store, tmux control client, ACP host, scheduler, client sessions, web) run
  the effects. Plain tokio tasks and channels, no actor framework.
- **IPC**: JSON-RPC 2.0, newline-delimited, over a Unix socket (`0700` dir, peer-uid
  check), with atuin's rule that client and daemon builds must match (no protocol version
  to bump). The same messages over WebSocket for the web page. Push = `subscribe` → snapshot
  → entity-level patches tagged with a revision; slow clients get a fresh snapshot instead
  of backpressure. Hand-rolled on `tokio-util` codecs + `serde_json` (~400 lines).
- **Hooks**: a command hook in **exec form** (`"command": "moa", "args": ["hook"]`, no
  shell) running a no-tokio path of the same binary that writes stdin to the socket and
  exits, `async: true` for every event that needs no answer, spool-to-file if the daemon
  is down. **Measured: 5.9 ms per event vs 96 ms for the prototype's bash hook.** Claude
  Code's `type: "http"` hooks (zero process launches) are a later optimisation for
  accounts that map to exactly one brain; they lose events when the daemon is down.
- **SQLite**: `rusqlite` (bundled), one writer connection on its own thread inside the
  daemon, WAL, `rusqlite_migration` with a first "adopt the prototype" migration. Change
  notification is domain-level (the store emits typed changes after commit), with
  `update_hook` as a backstop. Daily `VACUUM INTO` backups, and one before every migration.
- **Timers**: a scheduler actor. Short monotonic timers (debounce, 45 s settle / 3 min cap)
  on `tokio_util::time::DelayQueue`; wall-clock alarms (due dates, morning brief, 6 h
  idle-sleep) on a kqueue `EVFILT_TIMER` with `NOTE_ABSOLUTE | NOTE_MACH_CONTINUOUS_TIME`
  (timerfd on Linux), because tokio's clock stops while the Mac sleeps.
- **TUI**: `ratatui` 0.30 + `crossterm` 0.29, **one process per panel** (each a tmux pane,
  as the prototype), each a thin client of the daemon. **Measured: 7.8 ms from exec to a
  rendered frame** for a binary with tokio + rusqlite + ratatui + pulldown-cmark (python
  imports alone: 35 ms). Redraw only on daemon pushes, input, or while something animates.
- **tmux**: one long-lived control-mode client per brain (`tmux -C`, flags
  `no-output,ignore-size`) with `refresh-client -B` format subscriptions and `%window-*`
  notifications. This replaces the prototype's "tmux hook touches a file" and every
  per-check `tmux display-message`.
- **Packaging**: one `moa` binary with subcommands; Nix flake on **crane**; a launchd user
  agent per brain from home-manager/nix-darwin; live upgrade by re-exec with the listening
  socket inherited. Linux is cheap if confined to a `platform` module.

---

## 1. Daemon shape

### 1.1 One process per brain vs one daemon serving several

| | Process per brain | One daemon, many brains |
|---|---|---|
| Isolation (the "work data never reaches private" rule) | Enforced by the OS: separate address space, separate DB file handle, separate env (accounts, `CLAUDE_CONFIG_DIR`), separate tmux server whose env every pane inherits | Enforced by code: every query, cache, log line and channel carries a `BrainId`; one bug leaks |
| Crash / bad migration blast radius | One brain | All brains |
| Upgrade | Per brain, can stagger | All at once |
| Code complexity | No brain dimension anywhere | `BrainId` in every type, per-brain actor sets anyway |
| Cost | ~10–20 MB RSS and zero CPU per idle daemon | Saves one process |
| Cross-brain overview | Impossible by design (a feature) | Tempting, and the thing the user said they don't want |

The prototype already lives this way (`moa work`: own tmux server `-L moa-work`, own
brain.db, own scope file), LEARNINGS records the user's lean ("two brains are two separate
moas… peers never cross brains"), and the tmux env inheritance trick (set `MOA_*` before the
server starts, every pane inherits it) only works per process. **Recommendation: one
daemon per brain.**

What *is* shared lives outside any daemon:

- `~/.config/moa/brains.toml`: brain name → socket path, accepted accounts, folder rules
  (`acct-work → work, except ~/private → private`). Read by `moa hook` and the `moa` CLI
  to choose a socket; each daemon also checks it on input ("a guard, not a wall").
- The `moa` binary itself.

### 1.2 Internal structure: functional core, IO actors

```
                ┌──────────── inputs (mpsc) ────────────┐
 hook ingest ───┤                                        │
 tmux ctl ──────┤          Core (one task)               │──► watch<Arc<Model>> ──► client sessions (diff, push)
 ACP host ──────┤  handle(&mut State, Input, now)        │──► broadcast<Event>  ──► (toasts, "X finished")
 clients ───────┤     -> Vec<Effect>                     │
 scheduler ─────┤                                        │──► effects ──► store / tmux / ACP / scheduler / spawn
 fs watcher ────┘                                        │
```

- **Core** owns the authoritative in-memory model (sessions, panes, live status, open asks,
  suggestion cards, moa's wake debounce state) loaded from SQLite at start. It is a plain
  struct with a synchronous `handle` function: no IO, no `async`, no clock reads (time is an
  input). This makes the hard logic (state machine for sessions/asks/sleep, the 45 s / 3 min
  wake debounce, brain routing checks, "is this pane really that conversation") unit-testable
  with no tmux, no Claude and no sleeps: feed inputs, assert effects (insta snapshots).
- **Effects** are data: `Persist(Change)`, `Tmux(Cmd)`, `Acp(Prompt)`, `SetTimer{key, at}`,
  `CancelTimer(key)`, `Publish(Event)`, `Reply(ReqId, Result)`, `Spawn(...)`. An executor
  routes each to its actor. Writes are applied *before* the publish that depends on them, so
  a client never sees state the DB doesn't have.
- **Actors** are tokio tasks with an `mpsc` inbox and `oneshot` replies. A dozen of them at
  most; an actor framework ([ractor 0.16.6](https://crates.io/crates/ractor),
  [kameo 0.22.2](https://crates.io/crates/kameo), [actix 0.13.6](https://crates.io/crates/actix))
  adds supervision trees we can write in 30 lines with
  [`tokio_util::task::TaskTracker` + `CancellationToken`](https://docs.rs/tokio-util/0.7.20/tokio_util/task/)
  and a restart loop. Revisit only if the actor count grows a lot.
- **Runtime**: `tokio` multi-thread with 2 worker threads is plenty; SQLite gets its own OS
  thread (see §4). Everything is IO-bound and tiny.

### 1.3 Supervising child processes

| Child | Lifetime | Supervision |
|---|---|---|
| tmux server (`tmux -L moa-<brain>`) | **Independent of the daemon** (it double-forks); sessions must survive daemon restarts | Daemon starts it once if missing (one exec), sets `set-environment -g MOA_*` before building panes (LEARNINGS testing note) |
| tmux control client (`tmux -L … -C attach -t moa`) | Child of the daemon, long-lived, pipes | Restart with backoff on EOF; on reconnect, re-list panes/windows once and re-add subscriptions |
| ACP agent (`claude-agent-acp`, Node: it's the agent, not moa) | Child of the daemon | `tokio::process::Command` with `kill_on_drop(true)` and `process_group(0)` so the whole tree dies with it; restart with backoff; on restart `session/load` the thread by ID (the prototype verified replay works) |
| Agent sessions (`claude` in panes) | Children of the tmux server, never of the daemon | Not supervised; state comes from hooks, liveness from tmux `%window-close`/`%unlinked-window-close` and `pane_dead` subscriptions |

Moving the ACP connection from the console pane (prototype) into the daemon fixes the
"console died silently, moa with it" failure: the console panel can crash, restart or be
open twice (TUI and web) while moa's turn keeps running.

Every actor logs through `tracing` ([0.1.44](https://crates.io/crates/tracing),
[tracing-subscriber 0.3.23](https://crates.io/crates/tracing-subscriber)) to
`~/Library/Logs/moa/<brain>.log`; a panicking actor is restarted by its supervisor loop
(`JoinError::is_panic`), and the panic is published to clients as a visible event.

### 1.4 Graceful restart and live upgrade ("hot reload")

The prototype's loved feature: panels re-exec themselves when their source file changes.
The Rust equivalent, in three layers:

1. **State is never only in memory.** Everything that matters is in SQLite or in tmux
   (which survives the daemon). Restart = reload model from DB + `list-panes` once over the
   control client + reconcile. This alone makes `kill -9` safe.
2. **Daemon re-exec without a gap.** On `moa daemon upgrade` (or when it notices its binary
   path changed, see below) the daemon waits until the ACP agent is between turns, flushes,
   clears `FD_CLOEXEC` on the listening Unix socket (and the web TCP listener), and
   `execv`s the new binary with `MOA_LISTEN_FDS=3,4` (the systemd `LISTEN_FDS` convention;
   [listenfd 1.0.2](https://crates.io/crates/listenfd) parses it). No connection is ever
   refused; clients see their connection drop and reconnect (below). Same PID, so launchd
   doesn't notice. Children (tmux control client, ACP agent) are simply restarted in v1;
   inheriting their pipe fds too is possible later.
3. **Clients follow.** The handshake carries the server's build ID. A panel whose build ID
   differs from the daemon's re-execs itself (`std::os::unix::process::CommandExt::exec`
   on `current_exe()`) keeping its tmux pane, or the daemon does `respawn-pane -k` over
   the control client. That is the prototype's "edit, and every pane updates" loop.

Detecting a new build without polling: watch the profile symlink directory
(`~/.nix-profile/bin` or `/run/current-system/sw/bin`) with `notify` (FSEvents), and in dev
watch `target/debug/moa`. A dev loop is then `cargo build` → everything live in ~1 s.
Config and data files (layouts, theme, scope file, briefing templates) are hot-reloaded by
the same watcher, no restart at all.

## 2. IPC

### 2.1 Choice of protocol

| Option | For | Against |
|---|---|---|
| **JSON-RPC 2.0, newline-delimited, over UDS** (hand-rolled on [tokio-util 0.7.20](https://crates.io/crates/tokio-util) `LinesCodec` + [serde_json 1.0.154](https://crates.io/crates/serde_json)) | Same framing as ACP and MCP stdio, so one mental model and shared code; debuggable with `nc -U`; any language can talk to it (agents, scripts, a future Swift GUI); the web page reuses it over WebSocket with no codegen and no npm | We write request routing and subscriptions ourselves (~400 lines) |
| [jsonrpsee 0.26.1](https://crates.io/crates/jsonrpsee) | Mature, has subscriptions | Server transports are HTTP and WebSocket over TCP (hyper/tower); no line-delimited UDS transport; heavy dependency tree; its subscription model doesn't give us snapshot-then-patch-with-resync |
| [tarpc 0.38.0](https://crates.io/crates/tarpc) | Typed Rust-to-Rust RPC | Rust-only (bincode/serde transport): no `nc`, no web page, no agent scripts |
| Cap'n Proto ([capnp 0.27.3](https://crates.io/crates/capnp)) / protobuf ([prost 0.14.6](https://crates.io/crates/prost)) | Schema evolution, compact | Needs `capnp`/`protoc` codegen in the Nix build; the browser needs a JS runtime library (the npm problem); the payloads here are a few KB at a few Hz, where JSON costs nothing measurable |
| [interprocess 2.4.8](https://crates.io/crates/interprocess) | Cross-platform local sockets incl. Windows named pipes | Only matters for Windows; `tokio::net::UnixListener` is enough for macOS + Linux |

**Recommendation: newline-delimited JSON-RPC 2.0 on `tokio::net::UnixListener`**, with the
message types in a `moa-proto` crate (serde enums), and [schemars 1.2.3](https://crates.io/crates/schemars)
to emit a JSON Schema that documents the protocol for the web page and for agents. Set a
`LinesCodec::new_with_max_length` (e.g. 16 MiB: an ask's reply can be a long message).

### 2.2 Shape of the protocol

```jsonc
// client → daemon, first message
{"jsonrpc":"2.0","id":1,"method":"hello","params":{"proto":1,"client":"panel/tree","build":"g1a2b3c"}}
// ← {"id":1,"result":{"proto":1,"build":"g1a2b3c","brain":"work","rev":4211}}

{"jsonrpc":"2.0","id":2,"method":"subscribe","params":{"topics":["sessions","tasks","cards"],"since":4200}}
// ← {"id":2,"result":{"snapshot":{...},"rev":4211}}          (or just the patches since 4200)
// ← {"method":"patch","params":{"rev":4212,"upsert":{"sessions":[{...}]},"delete":{"tasks":[7]}}}
// ← {"method":"event","params":{"kind":"turn_done","session":"…","text":"…"}}

{"jsonrpc":"2.0","id":3,"method":"ask","params":{"to":"Peer B","text":"pick a number"}}
// ← answered minutes later when the peer's Stop hook arrives. No polling anywhere.
```

- **State push**: the Core publishes `Arc<Model>` into a `tokio::sync::watch` channel
  after each batch of inputs. Each client-session task wakes on `changed()`, diffs the
  topics it subscribed to against what it last sent, and writes one `patch`. A slow client
  automatically skips intermediate states (watch keeps only the latest), so the Core never
  blocks on a client. Entity-level upserts/deletes keyed by `(kind, id)` are simpler for
  clients than RFC 6902 JSON Patch.
- **Discrete events** (a turn finished, a permission prompt, a toast) go through
  `tokio::sync::broadcast`; a client that lags (`RecvError::Lagged`) gets a resync snapshot.
- **Long requests** (`ask`, `wait`) are just requests whose reply comes later; the
  connection stays open. The prototype's `moactl ask` 9-minute wait becomes a parked
  `oneshot`.
- **Versioning**: borrow atuin's rule (§9): **builds must match**. `hello` carries the
  build ID; only `hello`, `status` and `shutdown`/`upgrade` are stable across builds, and
  any other request from a different build is refused before decoding. So there is no
  protocol version to bump and no compatibility matrix, and every mismatch has one
  answer: a newer client asks an older daemon to re-exec into the new binary (§1.4), and
  an older client re-execs itself. Panels, hooks and daemon ship in one binary, so a
  matching build is always on disk. The exception is the spool file and hook payloads,
  which cross restarts: keep those additive (`#[serde(default)]`, unknown fields ignored).
  Optionally put the socket in a versioned directory (zellij's `contract_version_N/`) as a
  second guard.
- **Auth**: the socket lives in a `0700` directory owned by the user; on accept, check
  `UnixStream::peer_cred()` uid equals ours (works on macOS via `getpeereid`). The web page
  binds `127.0.0.1` only and requires a per-brain random token (in the URL once, then a
  cookie), since any local process can reach a TCP port.
- **Socket path**: macOS `sun_path` is 104 bytes. **Measured:** binding a socket under the
  124-character scratchpad path panicked with `path must be shorter than SUN_LEN`. Use
  `~/.local/state/moa/run/<brain>.sock` (check the length at startup and fall back to
  `$TMPDIR/moa-$UID/`), never a path derived from a deep project directory.

## 3. Hook delivery

### 3.1 What Claude Code offers (docs read 2026-11-15, Claude Code 2.1.311 installed)

From the [hooks reference](https://code.claude.com/docs/en/hooks):

- Handler types: `command`, `http`, `mcp_tool`, `prompt`, `agent`.
- **Exec form**: a command hook with `args` is spawned directly, *no shell*. Shell form
  runs `sh -c`.
- **`async: true`** (command hooks only): runs in the background without blocking Claude;
  output is ignored for decisions. Exactly right for Stop, UserPromptSubmit, Notification.
- **`type: "http"`**: POSTs the hook JSON to a `url`; `headers` can interpolate env vars
  listed in `allowedEnvVars` (so `$TMUX_PANE`, `$TMUX`, `$CLAUDE_CONFIG_DIR`, `$MOA_TITLE`
  can travel as headers). Connection failure or non-2xx is a **non-blocking error** unless
  `onFailure: "block"` (v2.1.295+). The URL is `http(s)://`; no Unix-socket URLs are
  documented. Managed settings can restrict them (`allowedHttpHookUrls`,
  `httpHookAllowedEnvVars`, `allowManagedHooksOnly`); this machine's managed settings add
  their own command hooks but set none of those restrictions today.
- Timeouts: default 600 s; 30 s on UserPromptSubmit; SessionEnd hooks share a 1.5 s
  budget. SessionStart's plain stdout (or `additionalContext`) is added to context, so
  SessionStart must stay synchronous to deliver the "you have peers" note.
- Plugins can ship hooks in `hooks/hooks.json`, which suits the existing moa plugin.

### 3.2 Measurements (this Mac, hyperfine, 200–300 runs, 1.5 KB Stop payload)

| Path into the daemon | Mean wall time | Process launches |
|---|---|---|
| `/usr/bin/true` (baseline exec cost) | 3.7 ms | 1 |
| **`moa-hook` (std-only Rust, exec form, writes to UDS)** | **5.9 ms** | 1 |
| `nc -U sock` | 5.8 ms | 1 |
| `moa-hook` via `sh -c` (shell form) | 17.0 ms | 2 |
| `moa-hook` via `env VAR=… moa-hook` | 12.4 ms | 2 |
| `moa-hook`, daemon down → append to spool file | 12.4 ms via `env`; ≈7.5 ms direct (rougher shell-loop timing) | 1 |
| prototype `track.sh` (bash + jq + sqlite3 …) | **96 ms** | ~15 |
| `type: "http"` hook | no exec at all (Claude's own HTTP client) | 0 |

Each extra exec costs ~4–5 ms here, so **exec form matters more than the language of the
forwarder**. `nc -U` is as fast but can't spool, route by folder, or add env fields.

### 3.3 Recommendation

- Install hooks in **exec form**: `{"type":"command","command":"/abs/path/moa","args":["hook"],"async":true}`
  for UserPromptSubmit, Stop, Notification(permission_prompt), SessionEnd; synchronous for
  SessionStart (it returns the peers note) and anything that must decide (a future
  PermissionRequest bridge). Never hook PreToolUse/PostToolUse for tracking: that's a
  launch per tool call.
- `moa hook` is a path in the main binary that **never starts tokio**: read stdin (cap at a
  few MB), add `TMUX_PANE`, `TMUX`, `CLAUDE_CONFIG_DIR`, `MOA_*` env fields, choose the
  brain from `brains.toml` + `cwd` (the folder rules move here from bash), connect with a
  short timeout (200 ms), write one line, and for SessionStart wait for the reply (≤1 s).
- **Daemon down** (`ECONNREFUSED`/`ENOENT`/timeout): append the line to
  `~/.local/state/moa/spool/<brain>.jsonl` with `O_APPEND` (one `write` of < `PIPE_BUF`-ish
  size is atomic enough; take `flock` for long lines), exit 0. The daemon drains the spool
  on start, before accepting clients, then truncates it. Never block, never fail the agent.
- As a last line of defence for "nothing lost", the daemon's reconcile on start can read
  the tail of each known session's transcript (`transcript_path`) for sessions whose last
  event is older than their transcript mtime.
- **HTTP hooks later**: for an account mapped to exactly one brain with no folder
  exceptions, `type: "http"` to `http://127.0.0.1:<port>/hook?brain=…` with a bearer token
  header saves the 6 ms and an exec per event. It loses events while the daemon is down
  (non-blocking error) and can't do client-side folder routing, so it's an optimisation,
  not the default.

## 4. SQLite

### 4.1 Crate

| Crate | Verdict |
|---|---|
| **[rusqlite 0.42.0](https://crates.io/crates/rusqlite)** (features `bundled`, `hooks`, `backup`, `functions`, `modern_sqlite`) | **Use it.** Synchronous, thin, exposes `update_hook`/`commit_hook`, online backup, custom functions; `bundled` compiles a known SQLite (with FTS5 and JSON) so the macOS system library doesn't matter |
| [sqlx 0.9.0](https://crates.io/crates/sqlx) | Async API, but SQLite runs on a worker thread anyway; compile-time-checked queries need `DATABASE_URL` or committed `.sqlx` offline data, which is friction in a Nix build |
| [sea-orm 2.0.4](https://crates.io/crates/sea-orm) | An ORM over sqlx; the schema is ~10 tables, and the brain API wants hand-written queries |
| [tokio-rusqlite 0.9.0](https://crates.io/crates/tokio-rusqlite) / [async-sqlite 0.7.0](https://crates.io/crates/async-sqlite) | Wrap a connection in a thread with a call-closure API; fine, but the store actor below is the same thing with typed messages |

### 4.2 Store design

- **One writer**, the daemon. A store actor on a dedicated `std::thread` owns the write
  `Connection`, receives typed commands over a channel, batches whatever is queued into one
  transaction, and replies via `oneshot`. Reads that the Core needs come from the in-memory
  model; ad-hoc reads (search, history, the brain API's `find`) use a second read-only
  connection on the same thread or a `spawn_blocking` pool of 2.
- Nothing else writes the file. Agents and the CLI go through the socket (the "brain API /
  fetcher layer" lesson). `moa sql` for debugging opens `?mode=ro`.
- PRAGMAs: `journal_mode=WAL`, `synchronous=NORMAL` (durable across process crashes; only a
  power loss can drop the last commits), `foreign_keys=ON`, `busy_timeout=5000`,
  `temp_store=MEMORY`.
- **Change notification**: the store is the only write path, so after `commit()` returns it
  emits typed `Change`s (`TaskUpserted(id)`, `SessionState(id, …)`) to the Core. Keep
  `Connection::update_hook` (feature `hooks`) as a backstop that records `(table, rowid)`
  for writes the store didn't anticipate (SQL triggers like the prototype's `activity`
  rows): collect in the hook, flush after commit. Caveats from SQLite: the update hook does
  not fire for `WITHOUT ROWID` tables, `REPLACE` conflict deletes or truncate-optimised
  deletes, and it can't touch the connection.

### 4.3 Migrations, including the prototype's database in place

- [rusqlite_migration 2.7.0](https://crates.io/crates/rusqlite_migration): migrations as a
  list of `M::up(sql)` in code, version in `PRAGMA user_version`, `M::up_with_hook` for
  Rust-side data transforms, optional `foreign_key_check()`, a `validate()` test helper.
  Lighter than [refinery 0.10.0](https://crates.io/crates/refinery) (history table, more
  machinery) and matches a single-binary app.
- The prototype DB has `user_version = 0` and was evolved by ad-hoc `ALTER TABLE … ADD
  COLUMN` in `picture.py`. So migration 1 is **"adopt"**: `CREATE TABLE IF NOT EXISTS` for
  every prototype table, then a hook that adds any missing column after checking
  `pragma table_info`. A fresh DB and any prototype DB both end at the same version-1
  schema. Later migrations are ordinary.
- Things to fix in early migrations: timestamps are `datetime('now','localtime')` text
  (store UTC instead: ISO-8601 with `Z` or integer ms); the `activity` rows are produced by
  SQL triggers (fine to keep, or move into the Core so the text can be localised);
  `sessions.grp` vs the `groups` table.
- Always `VACUUM INTO 'backups/pre-v<N>.db'` before running a migration; a test runs every
  migration against a sanitised copy of a real prototype DB.

### 4.4 Backups

`VACUUM INTO` (compact, consistent, works while the daemon keeps writing) or rusqlite's
online backup API (`backup` feature), daily on the wall-clock scheduler, keeping 7 dailies
and 4 weeklies under `~/Library/Application Support/moa/<brain>/backups/`.

## 5. Timers instead of polls

### 5.1 Two kinds of timer

1. **Short, monotonic, frequently reset**: the console-wake debounce (45 s after the last
   report, at most one wake per 3 min), "seen" settle delays, reconnect backoff, ask
   timeouts. Use [`tokio_util::time::DelayQueue`](https://docs.rs/tokio-util/0.7.20/tokio_util/time/delay_queue/)
   inside the scheduler actor: `insert`, `reset(key, timeout)` and `remove(key)` are exactly
   debounce semantics, all on tokio's internal timer wheel. The debounce itself is Core
   logic: on a report, `SetTimer{key: Wake, at: max(now+45s, last_wake+3min)}`.
2. **Wall-clock alarms**: due-date nags ("09:00 on the due day"), the morning brief,
   idle-sleep after 6 h. **tokio's clock is `Instant`, which on macOS is
   `CLOCK_UPTIME_RAW`** ([std docs](https://doc.rust-lang.org/std/time/struct.Instant.html))
   and does not advance while the machine sleeps, so a 6 h `sleep()` across a closed lid
   fires hours late. Use the kernel's wall-clock timers instead:
   - macOS: a kqueue with one `EVFILT_TIMER`, flags
     `NOTE_ABSOLUTE | NOTE_SECONDS | NOTE_MACH_CONTINUOUS_TIME` (SDK `sys/event.h`: "with
     NOTE_ABSOLUTE: causes the timer to continue to tick across sleep, still uses
     gettimeofday epoch"), armed for the earliest deadline, its fd wrapped in
     `tokio::io::unix::AsyncFd`. Fires on time, or immediately on wake if the time passed.
   - Linux: `timerfd` with `CLOCK_REALTIME` + `TFD_TIMER_ABSTIME` (via
     [nix 0.32.0](https://crates.io/crates/nix) or [rustix 1.1.6](https://crates.io/crates/rustix)).
   - ~100 lines behind a `WallAlarm` trait in the platform module; deadlines stored in
     SQLite so they survive restarts.

No timer exists unless something is scheduled: an idle moa with nothing due has zero
wakeups.

### 5.2 File watching, where it's actually needed

[notify 8.2.2](https://crates.io/crates/notify) (FSEvents on macOS by default, inotify on
Linux) with [notify-debouncer-full 0.7.0](https://crates.io/crates/notify-debouncer-full):

- `~/.config/moa/` (layouts, theme, brains.toml, scope files): hot reload.
- The moa binary's profile directory (live upgrade, §1.4).
- Later (collectors slice): project `.git/` directories (`HEAD`, `refs/`, `index` for
  "last activity"), worktree roots (detect dead worktrees), Claude transcript directories
  for recovery. FSEvents handles big recursive trees cheaply; debounce to ≥1 s.

tmux state is *not* file-watched: it arrives over the control client (§6.4).

## 6. TUI

### 6.1 Stack

| Need | Choice |
|---|---|
| Framework | [ratatui 0.30.2](https://crates.io/crates/ratatui) (0.30 split into `ratatui-core` / `ratatui-widgets` / `ratatui-crossterm`; widget authors depend on core only) |
| Backend | [crossterm 0.29.0](https://crates.io/crates/crossterm) with `event-stream` (async `EventStream` for `tokio::select!`), `bracketed-paste`. termion is Unix-only and less maintained; [termwiz 0.23.3](https://crates.io/crates/termwiz) (wezterm's) is good but slower-moving |
| True colour | crossterm writes 24-bit SGR directly, no terminfo lookup. The prototype's `tmux-direct` terminfo trick for curses disappears; tmux still needs `terminal-features …:RGB` for the outer terminal (already in its tmux.conf) |
| Mouse | `EnableMouseCapture` (SGR 1006). With tmux `mouse on`, clicks and wheel reach panes that asked for them, as the prototype found with curses |
| Width | ratatui uses [unicode-width 0.2.3](https://crates.io/crates/unicode-width) and [unicode-segmentation 1.13.3](https://crates.io/crates/unicode-segmentation) per grapheme. 💤 is 2 cells everywhere; text-presentation symbols with VS16 (⚠️) are where tmux and libraries can disagree, so keep a tested glyph set and add a sandbox-tmux test comparing `capture-pane` output to ratatui's layout |
| Markdown | [pulldown-cmark 0.13.6](https://crates.io/crates/pulldown-cmark) → own renderer to `ratatui::text::Text` (headings, lists, tables, quotes, code, and backticked `/commands` turned into buttons). [tui-markdown 0.3.13](https://crates.io/crates/tui-markdown) is a reference but buttons and cards are moa-specific |
| Input line | [tui-textarea 0.7.0](https://crates.io/crates/tui-textarea) (last release 2024) or a small own line editor |
| Tests | ratatui `TestBackend` + [insta 1.49.0](https://crates.io/crates/insta) buffer snapshots |

### 6.2 Redraw only on events

Each panel runs `tokio::select!` over: daemon messages, crossterm `EventStream`, resize
(SIGWINCH arrives as a crossterm event), and an animation interval **that exists only
while an animated element is on screen** (the pulsing "waiting on you", flashes). ratatui
double-buffers and writes only changed cells, so a redraw on every patch is cheap. No
33 ms tick (aoe's problem).

### 6.3 One process per panel vs one TUI process

Keep **one process per panel, one panel per tmux pane**, as the prototype does:

- It's what makes the cockpit composable: layouts are config, live agent panes join the
  cockpit by `swap-pane`, rebuilding a layout moves panels instead of restarting them.
  A single TUI process would have to reimplement tmux's layout and couldn't host the agent
  terminals without owning PTYs (herdr's path, explicitly not chosen).
- A crash takes down one panel, which tmux shows and moa can `respawn-pane`.
- Cost is small. **Measured: 7.8 ms exec-to-frame** for a 2.7 MB release binary with
  tokio, rusqlite, ratatui, crossterm, pulldown-cmark and serde_json (1.99 ms of that in
  `main`); RSS is a few MB per panel.

Each panel is `moa panel <name>`: connect to the brain socket, `hello`, `subscribe` to the
topics it draws, render, send commands back as requests. Panels know which brain from the
environment the tmux server gave them (`MOA_BRAIN`, as the prototype does with `MOA_*`).
Focus and "seen" come from the daemon (which learns them from tmux, §6.4), not from
terminal focus reports (unreliable under swap-pane, per LEARNINGS).

### 6.4 The daemon's tmux connection

One control-mode client per brain: `tmux -L moa-<brain> -C attach -t moa` with
`refresh-client -f no-output,ignore-size,no-detach-on-destroy` (tmux 3.8 man page):

- `no-output` stops `%output` floods: moa reads agent state from hooks, not screens.
- `refresh-client -B name:what:format` subscriptions, e.g. `-B panes:%*:#{pane_id} #{pane_dead} #{pane_current_path} #{@moa_panel}`,
  report changes as `%subscription-changed` (at most once per second).
- `%window-pane-changed`, `%session-window-changed`, `%window-add`, `%window-close`,
  `%unlinked-window-close`, `%layout-change` give focus and liveness instantly. This
  replaces the prototype's tmux hook that touched a file on every window switch, and the
  `display-message -t PANE` liveness check (which, per LEARNINGS, lies on tmux 3.8).
- Commands go over the same pipe (`%begin`/`%end` framed replies), so `send-keys`,
  `swap-pane`, `respawn-pane`, `set -p @moa_label` cost no process launches.

A `moa-tmux` crate holds the line parser, a command queue matching replies to requests by
command number, and typed notifications. [tmux_interface 0.4.0](https://crates.io/crates/tmux_interface)
wraps the CLI (a process per call), so it's not suitable.

## 7. Packaging

- **One binary, `moa`**, with [clap 4.6.7](https://crates.io/crates/clap) subcommands:
  `moa` / `moa work` (attach the cockpit, starting the daemon if needed), `moa daemon`,
  `moa hook`, `moa panel <name>`, the agent-facing verbs (`ls`, `ask`, `peers`, `suggest`,
  `task …`, `term`), `moa setup`. One binary means one version: the hook, panels and daemon
  can't drift. The hook path starts no runtime, so the bigger binary costs little
  (measured: tiny std-only forwarder 5.9 ms vs full binary to first frame 7.8 ms).
- **Nix flake with [crane](https://github.com/ipetkov/crane)** (v0.24.0, 2026-08): builds
  dependencies as a separate cached derivation (fast rebuilds), and gives `cargoClippy`,
  `cargoNextest`, `cargoFmt`, `cargoAudit` as flake `checks` for CI.
  [naersk](https://github.com/nix-community/naersk) is simpler but less flexible;
  nixpkgs' `rustPlatform.buildRustPackage` needs a `cargoHash` bump on every dependency
  change. Toolchain: nixpkgs' `rustc` (1.99.1 today) or
  [rust-overlay](https://github.com/oxalica/rust-overlay) to pin via `rust-toolchain.toml`.
- **Darwin gotcha, measured**: linking failed with `ld: library not found for -liconv`
  until `libiconv` was added. In the flake: `buildInputs = lib.optionals stdenv.isDarwin [ pkgs.libiconv ];`.
  `rusqlite`'s `bundled` feature needs a C compiler, which stdenv provides.
- **launchd**: one user agent per brain, from home-manager (`launchd.agents.moa-<brain>`)
  or nix-darwin (`launchd.user.agents.moa-<brain>`): `ProgramArguments = [moa daemon --brain <b>]`,
  `RunAtLoad`, `KeepAlive`, `ProcessType = Adaptive`, stdout/stderr to
  `~/Library/Logs/moa/`. After a reboot launchd starts the daemon, which recreates the
  tmux server and offers to resume sessions by conversation ID. `moa` also autostarts a
  daemon (with an `flock` on the brain's state dir against races) when no agent is
  installed, for dev and non-Nix users. launchd socket activation exists but needs FFI
  (`launch_activate_socket`) and buys little here.
- **Web UI assets**: plain ES modules and CSS checked into the repo, embedded with
  [rust-embed 8.14.0](https://crates.io/crates/rust-embed) and served by
  [axum 0.8.9](https://crates.io/crates/axum) (WebSocket for the protocol). No npm, no
  build step.
- **Linux cost**: small if isolated in a `platform` module: kqueue alarm ↔ timerfd, FSEvents
  ↔ inotify (notify handles it), launchd ↔ systemd user unit (home-manager
  `systemd.user.services`), `~/Library/...` ↔ XDG dirs. tmux, Claude Code, sockets and
  peer credentials behave the same. CI on Linux is cheap; keep one macOS job.

## 8. Workspace layout and testing

```
crates/
  moa-model/     domain types + the pure Core (State, Input, Effect, handle()). No IO, no tokio.
  moa-proto/     wire types (requests, notifications, patches), proto version, JSON Schema export
  moa-store/     rusqlite store actor, migrations (incl. "adopt prototype"), backups, change capture
  moa-tmux/      control-mode client: parser, command queue, subscriptions, typed notifications
  moa-acp/       moa-agent host over agent-client-protocol: spawn, session/load, permissions bridge
  moa-daemon/    actor wiring, scheduler (DelayQueue + WallAlarm), hook ingest + spool drain,
                 fs watcher, client sessions (watch/diff/push), web server, upgrade/re-exec
  moa-client/    async client lib: connect, hello, subscribe, auto-reconnect + resync
  moa-tui/       panels (tree, plan, status, console, picker), markdown renderer, theme
  moa-platform/  wall-clock alarm, paths, launchd/systemd glue (cfg per OS)
  moa/           the one binary: clap subcommands; `hook` path is std-only
  moa-testkit/   sandbox tmux server, stub ACP agent binary, hook payload fixtures, temp brains
```

Key external crates: [tokio 1.54.1](https://crates.io/crates/tokio),
[tokio-util 0.7.20](https://crates.io/crates/tokio-util),
[serde 1.0.229](https://crates.io/crates/serde) / serde_json,
[agent-client-protocol 5.3.0](https://crates.io/crates/agent-client-protocol) (the
official [Rust SDK](https://github.com/agentclientprotocol/rust-sdk): Client/Agent roles,
connection builders, `session/load`; `unstable_mcp_over_acp` could later hand moa's brain
API to the agent as an MCP server over the same connection, with
[rmcp 3.10.0](https://crates.io/crates/rmcp)), rusqlite, rusqlite_migration, ratatui,
crossterm, pulldown-cmark, notify, clap, tracing, axum, rust-embed.

Testing, in layers (the prototype's sandbox approach, made systematic):

1. **Core unit tests**: inputs in, effects out, insta snapshots; property tests for the
   wake debounce and ask state machine. Fast, most of the logic.
2. **Store tests**: every migration on an empty DB and on a sanitised prototype DB
   fixture; `rusqlite_migration`'s `validate()`; round-trips for each query.
3. **tmux tests**: `moa-testkit` starts `tmux -L moa-test-<rand> -f <test conf>` with
   `set-environment -g` for PATH/MOA_* *before* creating panes (the LEARNINGS leak where a
   test cockpit reached the real account), attaches the control client, asserts
   notifications. Kill the server in `Drop`.
4. **ACP tests**: a `stub-acp-agent` binary in the testkit implements the Agent role with
   the same SDK and scripted turns (including `session/request_permission` and slow turns),
   so daemon tests never call a model.
5. **Hook tests**: recorded real hook payloads (fixtures) piped to `moa hook` against a
   temp-dir daemon; daemon-down spool and drain; folder routing between two temp brains,
   asserting the "work" fixture never lands in "private".
6. **TUI tests**: ratatui `TestBackend` snapshots per panel; one end-to-end test in the
   sandbox tmux comparing `capture-pane` against expectations (tests may launch processes;
   the product may not).
7. Run with [cargo-nextest](https://nexte.st) via crane's `cargoNextest` in `nix flake
   check` and the required `ci` GitHub check; `tmux` in the check's `nativeBuildInputs`.

## 9. How comparable Rust tools are built

Read from source on GitHub with `gh`, 2026-11-15. Spot-checked: herdr's 100 ms
`CONNECTION_POLL_INTERVAL`, aoe's 33 ms TUI interval, atuin's strict build matching.

| | Process model | IPC | Push to clients | State / persistence | Agent status | Packaging |
|---|---|---|---|---|---|---|
| **[zellij](https://github.com/zellij-org/zellij)** | Server per session; client re-execs itself as `--server` and daemonizes; socket dir named `contract_version_N` | `interprocess` local socket, u32 length + protobuf (prost) (`zellij-utils/src/ipc.rs`) | Server renders and pushes `Render{content}`; per-client queue of 5000, client **dropped when full** (FIXME in `os_input_output.rs`) | rusqlite (bundled, WAL) for a session index only | New OSC **7501** "program status" (`state=idle\|working\|done\|blocked`, `kind=permission\|question`) in `zellij-server/src/attention/` | No flake |
| **[wezterm](https://github.com/wezterm/wezterm)** mux | Server per unix domain; client autostarts `wezterm-mux-server --daemonize`; flock'd pidfile | Custom PDU: leb128 length, serial, varbincode, zstd (`codec/src/lib.rs`, `CODEC_VERSION = 46`); hard error on mismatch | Notifications only schedule a push; push sends "lines changed since the client's seqno": natural coalescing | None | n/a | `buildRustPackage` |
| **[atuin](https://github.com/atuinsh/atuin)** daemon | One per user; pidfile + startup lock; backends: subprocess, launchd (socket activation, `kickstart -k`), systemd | tonic gRPC over UDS; **strict build match**, only `Status`/`Shutdown` cross builds (`crates/atuin-daemon/src/grpc/versioning.rs`) | `tokio::broadcast` event bus between components | sqlx + `migrate!` | Shell hooks are binary subcommands that call the daemon and give up after 2 s | crane + fenix flake; one binary with a `daemon` subcommand |
| **[aoe](https://github.com/agent-of-empires/agent-of-empires)** | Global `aoe serve`; HTTP (axum) over `daemon/api.sock` with peer-uid check; flock'd lifecycle | HTTP+JSON on UDS; WebSocket for the web dashboard (rust-embed `web/dist`, TS types generated from Rust) | `tokio::broadcast` → WebSocket | JSON files + ~60 numbered config migrations; rusqlite only for the ACP event log | `sh -c 'printf running > /tmp/aoe-hooks-…/status'` hooks, **polled every 2 s**, cross-checked by pane captures | crane flake, one binary |
| **[herdr](https://github.com/herdrdev/herdr)** | Server per named session; launched as a one-shot launchd job on macOS so closing the terminal doesn't kill it | Binary socket (bincode, exact `PROTOCOL_VERSION`) + newline-JSON API socket | Sequence-numbered 512-event ring, but subscribers **poll it every 100 ms** | JSON (`session.json`) | Screen scanning against per-agent TOML manifests + OSC titles; hooks run `python3` to report session IDs | flake + `buildRustPackage` |

aoe's TUI and tmux usage, confirmed in source: a 33 ms `interval` tick plus 1–5 s pollers
(`src/tui/app.rs`), and a `Command::new("tmux")` per call (`src/tmux/mod.rs`). That matches
the measurements in `docs/context.md` and is the pattern to avoid. **None of the five uses
tmux control mode**; herdr and zellij own the PTYs instead.

What to take:

- **atuin's build-hash rule** (adopted in §2.2): no protocol version numbers, a newer client
  replaces an older daemon.
- **atuin/wezterm/herdr startup hygiene**: flock'd pidfile + startup lock; treat
  `ECONNREFUSED` on an existing socket path as stale, unlink and start.
- **wezterm's "changed since your cursor" push** (and herdr's sequence ring) over zellij's
  queue-and-drop; this is what the `watch` + per-client diff in §2.2 does. Wake subscribers
  with a channel, not herdr's 100 ms sleep loop.
- **atuin's hook-as-subcommand** with a short give-up timeout (§3.3), not aoe's polled
  status files or herdr's `python3` snippets.
- **herdr/atuin on launchd**: run the daemon as its own launchd job so it never belongs to
  the terminal app's process tree.
- **zellij's OSC 7501 vocabulary** (working / blocked / idle / done; permission / question)
  is close to moa's five states; align names, and consider parsing it if agents start
  emitting it.
- **Skip herdr's SCM_RIGHTS handoff**: it exists because herdr owns the PTYs. With tmux owning
  them, moa's restart is "rebuild from SQLite + one `list-panes`", and re-exec keeps only
  the listening socket (§1.4).

## Open questions this raises

- After a reboot, should the daemon resume every session that was running (expensive: a
  Node process each), or bring them back asleep (💤) and resume on Enter? The second fits
  the idle-sleep feature and "nothing lost".
- Should `moa hook` also accept Codex's Claude-style hooks (`~/.codex/hooks.json`)? Same
  binary, a `--agent codex` arg.
- Is a power-loss window of "last commit or two" acceptable (`synchronous=NORMAL`), or use
  `FULL` for the events table only (not possible per table; it's per connection)?
- Does the web page need write access in v1, or read-only until auth is designed further?
