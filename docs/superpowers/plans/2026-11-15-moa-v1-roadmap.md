# moa v1 roadmap: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build moa v1 in Rust to full parity with the spike, then switch the user's live
setup over to it without stopping a single agent.

**Architecture:** One `moa` binary. Each brain runs one daemon. The daemon has a pure core
(`handle(&mut State, Input, Now) -> Vec<Effect>`) and IO actors for SQLite, tmux control
mode, ACP, timers and clients. Thin clients (ratatui panels, `moactl`, `moa hook`) speak
newline JSON-RPC over a Unix socket.

**Tech Stack:** Rust (edition 2024), tokio, rusqlite (bundled) + rusqlite_migration, jiff,
serde/serde_json, clap, ratatui 0.30 + crossterm 0.29, agent-client-protocol (pinned),
tracing, Nix flake on crane, GitHub Actions, cargo-nextest, insta, proptest.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md` (cited as "spec §N").
Research notes R1–R6 are in `docs/superpowers/research/`. Read the spec section a task
names before starting the task.

---

## How this plan is organised [call P1]

This file is the whole build at task level. It holds:

- the decisions that cross slices: crates, shared type names, CI, and the PR rhythm;
- each slice's tasks and its "done when";
- the calls the spec left to the builder.

Each slice also gets a **detailed plan** of its own: test-first steps, exact signatures,
exact test assertions. The detailed plan is written and reviewed **just before that slice
starts**, and no slice's code starts before its detailed plan is approved. The plans are
`docs/superpowers/plans/2026-11-15-moa-v1-slice-<N>-<name>.md`.

- Slices 0 and 1 are detailed now.
- **Why not all at once:** a detailed plan names real signatures from the slices before it.
  A slice-6 plan written today would guess at slice-2 types, and those guesses would rot.
  This file fixes what has to be fixed now. That covers the names, the crate boundaries, the
  order, and what each slice must deliver.

## Global Constraints

Every task in every slice includes these. Values are verbatim from the spec.

- **No polling.** Nothing checks state on a timer. Timers fire only at exact deadlines.
  Animation intervals run only while something animated is on screen, and read no state.
  (spec §2 anti-goals, U5)
- **Allowed process launches only** (spec §6.1 table): the tmux server, one `tmux -C`
  control client, the ACP adapter, agent sessions and shells, `moa panel`, `moa hook`, user
  or agent runs of `moactl`, `moa pick` in a popup, `tmux attach`, the `claude plugin` CLI
  on an explicit setup action, and launchd/systemd registration.
  - **Never** `ps`, `pgrep`, `lsof`, `git`, `sqlite3`, `jq`, bash helpers, `run-shell`, or
    `less`.
  - **Never** a `display-popup` command, except the user-chosen picker popup.
  - Every product launch goes through `moa_platform::spawn` (slice 2), which counts.
  - Tests may launch anything.
- **The daemon is the only writer** of a brain's DB. The offline tools in slice 1 and slice
  10 refuse to touch a DB that a running daemon owns.
- **The spike's data is never changed by v1.** Its DBs are opened read-only and snapshotted
  with `VACUUM INTO`.
  - The bytes of the main file and the `-wal` never change.
  - `-shm` is SQLite's shared memory. A reader may touch it, and SQLite may create empty
    `-wal`/`-shm` files when they're missing.
  - Nothing creates a missing DB by opening it.
  - Spike DBs are backed up with `VACUUM INTO`, never with `cp`. Most of the private brain's
    data lives in its `-wal`.
- **Instants** are `INTEGER` UTC milliseconds. **Calendar dates** (`due`, `today`,
  `last_activity_on`, `day`) are `YYYY-MM-DD` local dates. (spec §6.5)
- **Minimum tmux 3.4**, tested on 3.8. **Claude Code ≥ 2.1.280** (needed for
  `CLAUDE_CODE_PLUGIN_DIRS`). (spec §6.6, §6.7)
- **Platforms:** macOS (aarch64-darwin) first, Linux too. The build works with Nix and with
  plain `cargo`. (U15, constitution 9)
- **Colours:** the palette in spec §6.14, exactly, in 24-bit.
- **Public repo hygiene** (constitution 15, CLAUDE.md). Never commit any of these:
  - hostnames, employer names, security products, real account names, emails, or personal
    paths;
  - in code, docs, fixtures, tests, snapshots or commit messages.

  Fixtures use made-up data: the project "Lantern", accounts `acct-a` and `acct-b`, and the
  folder `~/private`.
- **Reports never print record contents** [call P8]. This applies to import, export, switch
  and doctor reports.
  - Records are named by table and id, or by an 8-character conversation-id prefix.
  - Reasons are fixed sentences (`&'static str`) that name a column at most. They never
    quote a value: no titles, text, names, group paths, paths, account names or unknown
    status strings.
  - Accounts show as `account #n`. Names are printed only with `--names`, which is for the
    user's own terminal.
  - This keeps the reports safe to paste into a PR, and keeps work data out of other
    sessions.
- **The work brain is touched only from the work account.** Local runs against a copy of the
  user's `work` brain are run **by the user in their own terminal**, never by an agent
  session on another account.
- **Wording**: one plain human line per error, said the way people think. No tmux words in
  anything the user sees. (U12, lesson 35)
- **v1 is never put on PATH before the switch-over.** No `nix profile install` and no
  `cargo install`. The user's `moa` and `moactl` are the spike's, and they use it every day.
  Run v1 with `nix run .` or `cargo run -p moa`.
- **Commits** use the repo-local git identity the user set, never a work identity.

## Review Focus

These are the cross-cutting conditions most likely to bite the user that no single task's
happy path exercises. Each one is owned by a slice, and that slice's detailed plan pins it
with a test.

1. **Real spike data the spike tolerated but v1's types reject.** Examples: unknown status
   strings, NULLs, malformed dates, project names that differ only in case, suggestion
   actions that don't parse, and asks or tasks pointing at sessions that don't exist.
   - Expected: the import keeps every record (an ask whose target is gone is kept as
     failed), warns, and never aborts the whole brain.
   - Owner: slice 1.
2. **Local-time text across DST and time-zone changes.** The spike wrote local wall-clock
   text, and conversion has to be deterministic.
   - A time that happens twice (fall back) takes the earlier instant.
   - A time that never happened (spring forward) shifts forward.
   - Converting back gives the same text wherever possible.
   - Owner: slice 1.
3. **The daemon is down, restarting or upgrading while agents fire hooks, and async hooks
   arrive duplicated or out of order.**
   - Expected: nothing is lost (spool, then drain).
   - State never regresses to an older event.
   - The agent never sees a hook error.
   - Owner: slice 4, with re-exec in slice 9.
4. **Paths in routing and folders.** Cases: `~`, symlinks, trailing slashes, empty entries,
   prefix look-alikes (`~/private2` vs `~/private`), and a cwd that moves after start.
   - Expected: whole-component matching; a session never changes brain.
   - Owner: slice 2, with the hook path in slice 4.
5. **The tmux server is killed or restarted under moa, and pane ids get reused.**
   - Expected: no stale binding is ever trusted.
   - Sessions come back 💤 and nothing auto-resumes.
   - Owner: slices 3–4.
6. **Going back to the spike (fallback) while agents run.**
   - The spike resumes, in a new window, any live session it thinks has no pane. That's a
     second process on a running conversation.
   - Expected: pane links survive in both directions (P18).
   - The exported `meta.cursor` is the newest *exported* event.
   - The activity triggers exist.
   - Accounts come back as strings the spike can launch.
   - Owner: slice 1, rehearsed in slice 10.

## Plan-level calls

These are calls the spec left to the builder, or small deviations from it. Each has its
reason. A call can be revisited, but the reason has to be answered.

| # | Call | Reason |
|---|---|---|
| P1 | A roadmap plus one detailed plan per slice, each written just before its slice | Signatures in later slices depend on code from earlier ones (see above) |
| P2 | **Crates are created by the slice that first needs them**, not all in slice 0. Moved from slice 0: the stub ACP agent → slice 6; the synthetic spike fixture → slice 1; the spawn counter and `moa-platform` → slice 2 | YAGNI. An empty crate or a stub with no consumer is untested code, and the ACP crate is pinned when it's used (spec §13) |
| P3 | **CI** has jobs `linux` (Ubuntu 24.04, apt tmux **3.4**, the minimum), `macos` (Homebrew tmux, latest) and `nix` (`nix flake check` on macOS arm64, nixpkgs tmux **3.8**), plus a job named **`ci`** that `needs` all three and fails if any failed | The ruleset requires a check named `ci`. A matrix would rename it `ci (…)`. This also gives the spec's 3.4/3.8 version matrix for free |
| P4 | **One PR per slice**, split at task boundaries when the diff passes ~1500 lines. Each PR gets `/code-review medium --fix`, the fixes pushed, CI green, and a restart notice in the description ("Restart needed: nothing; the spike is untouched" until slice 10) | Repo rules and the user's standing preference |
| P5 | **No `CHECK` constraints on enum text columns.** Enums are validated in Rust (`FromStr` on load). Tables are `STRICT`. **Import keeps the spike's integer ids** for projects, tasks, notes, suggestions, asks, events and activity | Adding a value to a `CHECK` later needs a table rebuild, which the additive-only rule (C5) forbids. Keeping ids keeps "task #12" in moa's notes and in the user's head valid |
| P6 | **Until the switch-over, migration 1 may be edited** instead of stacking migrations, because no v1 DB is kept (they're rebuilt from the spike). From the switch-over, migrations are append-only and additive until the spike is retired | Keeps the pre-switch schema in one readable place, without the shuffle of 10 tiny migrations |
| P7 | **Time handling uses `jiff`.** Spike local text is converted with the zone given to the import, using jiff's `compatible` disambiguation (earlier instant on fold, shift forward on gap). Tests use POSIX TZ strings (`EST5EDT,M3.2.0,M11.1.0`), never the system tzdb | Correct DST handling. The tests must not depend on `/usr/share/zoneinfo`, which the Nix build sandbox lacks |
| P8 | Reports are contents-free (Global Constraints) | Safe to paste. The work brain stays out of other accounts' sessions |
| P9 | **Offline tools** (`moa adopt-spike`, `moa export-spike`) write SQLite directly. They refuse an output path that exists and never write their input | They predate the daemon, and `moa switch` (slice 10) runs them while no daemon owns the target |
| P10 | **Build id** (C3, C21) = FNV-1a-64 over (canonical exe path, file length, mtime ns), as 16 hex digits | Each `cargo build` and each Nix store path gives a new id, with no git or build-script dependency. Every process computes its own id, so mixing builds is detected |
| P11 | **Every agent-facing verb is defined once**, as a clap `Subcommand` in `moa-proto::cli`. The CLI and suggestion-action validation both parse with it (slice 2) | One grammar. A suggestion's actions are validated by the same parser `moactl` runs |
| P12 | **One toolchain pin**: `rust-toolchain.toml` (an exact `1.x.y`) is read by rustup in CI and by the flake through rust-overlay's `fromRustupToolchainFile` | Nix and plain-cargo builds can't drift apart |
| P13 | **`MOA_ROOT=<dir>`** moves every moa path under `<dir>` as if it were `HOME` (slice 2) | Tests, the sandbox and dogfood runs must never touch the user's real brains. One switch beats overriding each path |
| P14 | **`Now.tz` is a full `jiff::tz::TimeZone`**, not the spec's `tz_offset` | An offset can't tell where the next DST change falls. Day boundaries and due dates need the zone's rules |
| P15 | **Test code may use `unsafe`**, only for `std::env::set_var` in a test binary with exactly one test, with a `// SAFETY:` comment | Edition 2024 makes `set_var` unsafe. Proving that the parent's env doesn't leak into the sandbox needs it |
| P16 | **`rusqlite` is held at 0.41** in `[workspace.dependencies]`, with a comment | `rusqlite_migration` 2.7 (the latest) requires `^0.41`. Newer `rusqlite` fails to resolve (`links = "sqlite3"`). Raise both together |
| P17 | **Imported session ids are derived, not random**: UUIDv7 with timestamp = the session's `created_at`, and random bits = the first 10 bytes of SHA-256(brain ‖ 0 ‖ conversation id) | The import is idempotent (spec §6.5). A dry run and the real run agree, and a re-switch after `switch back` keeps the ids already given to v1-launched sessions (`MOA_SESSION`). Ids still sort by start time |
| P18 | **The spike's pane links are kept unverified** in a `spike_panes` table. Slice 4's reconcile and switch step 6 verify them or clear them. The export uses them when there's no verified binding | Spike-launched agents have no `MOA_SESSION`, and often no id in their argv, so the pane is their only link. Dropping it would make the fallback resume running agents a second time |
| P19 | **Schema additions beyond spec §6.5:** `suggestions.actions_raw` (the spike's action text, verbatim), `conversations.spike_account` (the raw spike account string), `spike_panes` (P18), and a nullable `asks.to_session` (target gone). **In-flight spike asks are closed at import**: pending → dropped; sent and received → failed | Spec §6.5 keeps invalid actions as text. The spike can only launch the exact account strings it wrote. An ask nobody waits for any more must never be typed into an agent days later |
| P20 | **The transcript check is passed into the import** (`ImportEnv.transcript`, a fn pointer) by the binary, so `moa-store` never depends on `moa-agents` | `moa-agents` grows ACP and tokio in slice 6. The store must stay light |

## Crates and who creates them

```
moa (bin) ──► moa-client ──► moa-proto ──► moa-core
   │            moa-tui ──► moa-view ──► moa-core
   └────────► moa-daemon ──► moa-store ──► moa-core
                  │      ──► moa-tmux
                  │      ──► moa-agents ──► moa-proto
                  └──────► moa-platform
moa-testkit (dev-dependency only)
```

The binary passes `moa_agents::claude::find_transcript` into the store's import (P20):
`moa-store` never depends on `moa-agents`. `moa-core`, `moa-proto` and `moa-view` never
depend on tokio, SQLite or the OS. Their `uuid` has the `serde` feature only; id generation,
which pulls in `getrandom`, lives in `moa-store`. CI checks
this with `cargo tree -p moa-core -e normal`, which must contain no `tokio`, `rusqlite` or
`libc` (a test in slice 2).

| Crate | Created in | Spec |
|---|---|---|
| `moa`, `moa-testkit` | 0 | §6.2 |
| `moa-core` (model and time), `moa-store`, `moa-agents` (Claude transcript lookup only, at first) | 1 | §6.5, §6.7 |
| `moa-proto`, `moa-daemon`, `moa-client`, `moa-platform` | 2 | §6.3, §6.4 |
| `moa-tmux` | 3 | §6.6 |
| `moa-view`, `moa-tui` | 5 | §6.13 |

## Shared names (the vocabulary every slice uses)

A slice that introduces a name owns its exact signature in its detailed plan. Later slices
use these names unchanged.

| Name | Crate path | Slice |
|---|---|---|
| `UtcMs(i64)`, `Now { wall: UtcMs, mono: Duration, local_day: jiff::civil::Date, tz: jiff::tz::TimeZone }` | `moa_core::time` | 1 (`UtcMs`), 2 (`Now`) |
| `spike_local_to_utc`, `utc_to_spike_local`, `parse_date` | `moa_core::time` | 1 |
| Ids: `SessionId(Uuid)`, `ProjectId(i64)`, `TaskId(i64)`, `NoteId(i64)`, `SuggestionId(i64)`, `AskId(i64)`, `EventId(i64)`, `ActivityId(i64)` | `moa_core::model` | 1 |
| Enums (see slice 1 plan): `ProjectStatus`, `TaskStatus`, `Who`, `Priority`, `NoteKind`, `SuggestionStatus`, `AgentKind`, `Lifecycle`, `Activity`, `Location`, `AskStatus`, `EventKind`, `ActivityWho`, `ConversationReason`, `ThreadKind` | `moa_core::model` | 1 |
| Row structs: `Project`, `Task`, `Note`, `Suggestion`, `Session`, `Conversation`, `PaneBinding`, `SpikePane`, `Event`, `ActivityRow`, `Group`, `Ask`, `MoaThread`, `FeedEntry`, `Nag`; `SuggestionAction`, `AskTarget`, `ACTION_VERBS` | `moa_core::model` | 1 |
| `Store::open`, `Store::conn` | `moa_store` | 1 |
| `AccountMap` (+ `AccountMap::from_config` in slice 2) | `moa_store::accounts` | 1 |
| `import_spike`, `ImportEnv`, `IdSource`, `ImportReport`, `TableCounts`, `RecordNote`, `export_spike`, `ExportEnv`, `ExportReport` | `moa_store::{import, export}` | 1 |
| `find_transcript`, `TranscriptCheck` | `moa_agents::claude` | 1 |
| `Snapshot` (defined in `moa_core::state`, re-exported here), `Store::load_snapshot`, `Store::apply`, `StoreHandle` (actor; one transaction per commit), `CommitError` | `moa_store` | 2 |
| `Change` (typed writes the core emits) | `moa_core::change` | 2 |
| `State`, `Input`, `Effect`, `handle`; `Op`, `Done`, `OpError`, `Deps`, `CallerInfo` (`moa_core::op`, `state`); `Model`, `Topic`, `CoreEvent` (`moa_core::publish`) | `moa_core` | 2 |
| `Config` (`moa.toml`, `Config::from_file_text`), `BrainName`, `AccountName`, `caller_store` | `moa_core::config` | 2 |
| `route(&Config, account, cwd, opener) -> Route` | `moa_core::route` | 2 |
| `Call`, `Incoming`, `ServerLine`, `RpcError`, `Patch`, `SnapshotMsg`, `Topic` (re-exported from `moa_core::publish`), `cli::{Moactl, Verb, parse_action}` | `moa_proto` | 2 |
| `Paths` (incl. `check_v1_allowed`), `ensure_private_dir`, `spawn::Spawner` (counting), `WallTimer`, `WallAlarm`, `ZoneWatch`, `build_id()` | `moa_platform` | 2 |
| `Notification`, `Cmd`, `Mailbox` (in `moa_core::tmux`: the core can't depend on tokio-based `moa_tmux`) | `moa_core::tmux` | 3 |
| `ControlClient` | `moa_tmux` | 3 |
| `HookLine`, `HookEnvelope`, `HookKind` (defined in `moa_core::hook`, re-exported by `moa_proto::hook`) | `moa_core::hook` | 4a |
| `DisplayState`, `display_state(&Session, now) -> (DisplayState, Why)` | `moa_core::live` | 2 (from rows), 4 (quiet, events) |
| `AgentKindImpl` trait, `claude`, `acp` | `moa_agents` | 1 (`claude::find_transcript`), 4, 6 |
| `Layout`, `LayoutFile` | `moa_core::layout` | 5 |
| `Action`, view-models | `moa_view` | 5 |
| `View`, `PanelReg` | `moa_core::panel` | 5a |
| `Data`, `Line`, `Panel`, `Screen` | `moa_view` | 5a |
| `Live` | `moa_client` | 5a |
| `Sandbox` (tmux), `SpikeFixture`, `TypicalBrain`, `TempBrain`, `fake-claude` bin, `stub-acp` bin | `moa_testkit` | 0, 1, 2, 4, 6 |

## Conventions for every slice

- **Branch** `v1/slice-<N>-<name>` from `main`. Commits are prefixed `v1:`. The PR title is
  `v1 slice N: <name>`.
- **Tests:** `cargo nextest run --workspace`.
  - Snapshot tests use `insta`, and their snapshots are committed.
  - Property tests use `proptest`.
  - Tests needing tmux use `moa_testkit::Sandbox` and are never skipped in CI. A missing
    tmux fails loudly.
- **Local-only tests** against copies of the user's real brains:
  - They run only when `MOA_LOCAL_BRAIN_COPY=<path>` is set, and are skipped silently
    otherwise.
  - Their output follows the contents-free rule.
  - They are never in CI. Nothing they produce is committed.
- **Lints:** `cargo fmt --check`, and `cargo clippy --workspace --all-targets -- -D
  warnings`.
  - `unsafe` is denied workspace-wide except in `moa-platform`, where each block has a
    `// SAFETY:` comment, and in tests under P15.
- **Dogfood:** every slice ends with a "try it" line for the user. That is a command
  against the sandbox or a copy of their brain, plus "restart needed: …".

---

## Slice 0: workspace, Nix, CI

**Detailed plan:** `2026-11-15-moa-v1-slice-0-workspace.md`
**Done when:** CI (`ci` = linux + macos + nix) is green, and `nix run . -- --version` and
`cargo run -p moa -- --version` both print `moa 0.1.0`.

## Slice 1: store, spike import and export

**Detailed plan:** `2026-11-15-moa-v1-slice-1-store.md`
**Done when:**
- The synthetic spike fixture round-trips (import → export → equal, except for the
  documented lossy fields).
- The `brain_valid` round-trip and `brain_any` never-fails properties pass.
- A **copy** of the user's private brain imports and round-trips, with only Warn and Info
  notes (the local-only `MOA_LOCAL_BRAIN_COPY` test).
- The user runs the same against a copy of the work brain, in their own terminal.

## Slice 2: core, daemon, protocol, data verbs

**Detailed plans:** `2026-11-15-moa-v1-slice-2a-core.md` (platform, config, routing, core,
store actor, the data verbs but suggestions) and `2026-11-15-moa-v1-slice-2b-daemon.md`
(protocol, grammar, suggestions, daemon, client, `moa sql`, service; it owns this "done
when" and "try it").

Spec §6.2–6.5, §6.9, §6.10 (verbs), §6.8 (wall-clock alarm).

1. **`moa-platform`**:
   - `Paths` (XDG on both OSes, spec C27 table). The socket path is length-checked against
     104 bytes, falling back to `$TMPDIR/moa-$UID/`.
   - **`MOA_ROOT=<dir>`** moves every moa path under `<dir>`, as if `<dir>` were `HOME`
     (`<dir>/.config/moa`, `<dir>/.local/share/moa`, `<dir>/.local/state/moa`). This is how
     tests, the sandbox and the dogfood runs stay off the user's real brains.
   - `build_id()` (P10).
   - `spawn::Spawner` with a process-wide counter.
   - `WallAlarm` (kqueue `EVFILT_TIMER` `NOTE_ABSOLUTE|NOTE_MACH_CONTINUOUS_TIME` / timerfd
     `CLOCK_REALTIME`+`TFD_TIMER_ABSTIME`) in `AsyncFd`.
   - Tests: the alarm fires at an absolute deadline, re-arms earlier, and a deadline in the
     past fires immediately.
   - **Time-zone change watch** (spec §6.8): `notify(3)` on a file descriptor on macOS, and
     a watch on `/etc/localtime` on Linux. A change becomes an input that re-derives the
     day boundaries.
2. **`moa-core::config`**:
   - `moa.toml` types (spec §6.9 example):
     - `brains.<b>.{folders, moa_account, open_account, spike_db}`, where `spike_db` is the
       spike's `MOA_DB` for that brain, used by `--from-spike` until the spike is retired;
     - `accounts.<a>.{kind, store, launcher, brain, except, only}`;
     - `setup.skipped`.
   - Load and validate, with one human line per error.
   - `moa_store::AccountMap::from_config(&Config, home)` replaces slice 1's flag-built
     map. `moa-core` can't depend on `moa-store`, so the conversion lives in the store.
3. **`moa-core::route`**:
   - Pure `route()` with the spike's semantics (spec §6.9): whole-component matching after
     `~` expansion. Canonicalisation is done by the caller, because the core does no IO.
   - Proptests:
     - an `except` path routes to the opener;
     - `only` outside its folders gives `None`;
     - empty entries never match;
     - `~/private2` is not under `~/private`.
4. **`moa-core` skeleton**:
   - `State`, `Input` and `Effect` exactly as spec §6.3.
   - `handle`, plus the `pending_ops` table.
   - The ordering rule (all `Persist` before any `Publish`/`Reply` from the same call) is
     asserted by a test helper used in every core test.
   - `Change` covers every table.
   - Activity rows are produced by the core (replacing the spike's SQL triggers) with the
     spike's wording.
   - **`moa_core::live`**, the pure display-state model (spec §6.7 table): state, "why" and
     sort order from the rows alone.
     - "Asks you" uses the spike's `asks()` rule on `activity_why`: the last line, stripped
       of `` *_`)"' ``, ends in `?`.
     - Proptest: every lifecycle/activity/flag combination has exactly one display state.
     - Slice 3's status counts and slice 5's views read it. Slice 4 adds quiet and the
       event-driven parts.
   - **Scheduler actor** (C13): it executes `SetTimer`/`CancelTimer` effects, using a
     `DelayQueue` for short monotonic timers and `WallAlarm` for wall-clock deadlines.
     Deadlines are derived from data at start.
5. **`moa-store` actor**:
   - `Store::load_snapshot()`.
   - `StoreHandle` on its own OS thread: one transaction per commit; the core batches each
     drain (2a S4) and replies after commit.
   - A read-only connection for `find`/history.
   - Daily `VACUUM INTO` on a `WallAlarm` deadline, with slice 1's rotation.
6. **`moa-proto`**:
   - JSON-RPC 2.0 framing (newline codec), plus `hello` with the build match. On a mismatch,
     only `hello`, `status`, `upgrade`, `shutdown` **and the hook notification** are
     allowed (spec §6.4), so hooks keep flowing during upgrades.
   - `subscribe` (snapshot, then revisioned patches from a `watch` diff), plus `event`
     (broadcast; a lagging client gets a snapshot).
   - `RpcError` codes.
   - A `schemars` JSON Schema snapshot test.
   - `cli::Verb` (P11) holds every verb and flag from R1 §2.13.
7. **`moa-daemon`**:
   - `moa daemon --brain B` with a lock (`flock` on `<brain>.lock`, so a second daemon
     exits with "already running").
   - Socket in a `0700` dir, with a peer-uid check.
   - A client-session task per connection, plus tracing to `logs/<brain>.log`.
   - A panicking actor restarts, and the panic is published as an `event`.
   - Idle test: with no clients and nothing due, the daemon gets no wake-up for a simulated
     day except the backup deadline.
   - **Brain-isolation test** (spec §11.2): two `TempBrain`s, `default` and `work`, with
     routed work fixtures. Nothing lands in `default`'s DB.
   - `moa sql --brain B`: a read-only SQL shell on the brain for debugging (spec §6.5).
     It's in-process, with no `sqlite3` launch.
8. **Data verbs in the core**:
   - `task ls|add|set`, with due parsing (`today`, `tomorrow`, `+N`, weekday names (the next
     one, never today), ISO, `""` clears).
   - `note`, `project` (show/add, with back-linking of sessions), `find`, `brief`,
     `attention`.
   - `suggest` (actions parsed by `cli::Verb` and whitelisted; duplicate open text refused),
     and `suggestion yes|no|later|ls`. Yes runs the actions in-process. An action that needs
     tmux (`close`) answers "close isn't in this build yet" until slice 4.
   - Session data ops: `set` title/group, `group add|rename|rm`, `archive`, and `ls`. Its
     data columns come now; the tmux columns and the untracked list come in slice 4.
   - Name resolution: a unique prefix of the moa id **or of any of the session's
     conversation ids**, then substring over title/project/group. An exact title wins; an
     ambiguous name lists up to 8. Conversation-id prefixes matter because the user's
     habits, moa's notes and imported suggestion actions all name sessions that way
     (`moactl resume 3f2a1b`).
   - Imported suggestions are re-validated here (invalid ones are flagged, not dropped).
   - The output text matches the spike's `moactl` (R1 §2.13), and `--json` works on every
     verb.
9. **`moa-client` and the CLI front**:
   - `moactl` is dispatched on `argv[0]` (and `moa <verb>`). It connects and autostarts the
     daemon through `Spawner` under the lock.
   - Caller identity: `MOA_SESSION`, else `CLAUDE_CODE_SESSION_ID` → `conversations`, else
     "unknown in <folder>".
   - Errors: one line, with a non-zero exit.
10. **Service registration** (`moa service install|uninstall --brain B`): a launchd agent
    plist (`RunAtLoad`, `KeepAlive`, `ProcessType=Adaptive`), and a systemd user unit on
    Linux.

**Done when:**
- `moactl task|note|project|suggest|suggestion|brief|attention|find` work against a copy of
  the private brain imported with slice 1, through the daemon, with no tmux.
- A scripted flow of 20 verbs makes zero `Spawner` launches after the daemon is up.

**Try it:** with `MOA_ROOT=/tmp/moa-try`, run `nix run . -- adopt-spike --from <copy> --into
/tmp/moa-try/.local/share/moa/default/moa.db`, then `nix run .#moactl -- brief` (v1 isn't on
`PATH`; 2b ships `moactl` in the package). Restart needed: nothing; the spike is untouched.

## Slice 3: tmux layer

Spec §6.6; R2.

1. **`moa-tmux` parser**:
   - Bytes go to `Block{time, number, flags, lines, error}` or `Notification`.
   - Blocks are matched on `(time, number, flags)`, and flag-0 blocks are skipped.
   - Notifications inside blocks are accepted (pre-3.8).
   - Unknown lines become `Unknown`.
   - Tests use captures from R2, plus a `proptest` fuzz of the parser, since cargo-fuzz needs nightly (slice 3, T15).
2. **Control client actor**:
   - Spawns `tmux -L <sock> -C attach -f no-output,ignore-size,no-detach-on-destroy -t
     '=_moa'` through `Spawner`.
   - FIFO reply matching to `TmuxReply{token}`; no transaction mode (`a ; b` + sentinel) here, slice 5's layouts build it (slice 3, T14).
   - Command escaping.
   - Reconnect with backoff (`DelayQueue`).
   - At connect, reads `#{pid} #{start_time} #{version} #{client_name}` and refuses < 3.4 in
     plain words.
   - Never sends waiting commands or `refresh-client -C`/`-B` (a test greps the command log).
3. **Server bootstrap and adoption**:
   - Keeps the spike's socket names (`-L moa` and `-L moa-<brain>`) and the cockpit session
     name `moa` (C7), so v1 can adopt the live servers.
   - `moa_testkit::Sandbox` gains a constructor with no server (for create-if-missing tests),
     and restarts after `kill-server` pass `-f` again.
   - The front door (`moa`) creates the server only if it's missing; the daemon runs tmux
     with `-N` and never starts one (slice 3, T1).
   - The generated config runs `source-file -q ~/.tmux.conf` first and sets
     `terminal-features …:RGB` for terminals known to support it (spec §6.14).
   - Every pane's env is set with `-e`; the server env stays clean.
   - Creates the `_moa` session.
   - Hooks at index 90 (`pane-exited`, `pane-died`, `window-layout-changed`,
     `client-attached`, `client-detached`) are installed idempotently and removed on clean shutdown;
     `alert-bell` and `client-active` wait for the slices that consume them (slice 3, T7).
   - Sets `@moa_ctl`.
   - Adopting an existing server = the same steps without creating it.
4. **Mailbox**:
   - The `moa:` message grammar (`key K pane session client`, `prompt …`, `hook NAME …`).
   - Becomes `Input::Tmux(Mailbox)`.
5. **Keys and menu**:
   - The `moa` key table on `C-a` (spec §6.13 table), the prefix keys, and the full menu
     listing every action with its key.
   - When the daemon is down, keys show "moa isn't running: run moa".
   - `display-message` notices (the spike's 4 s `moamenu` messages).
   - The key-table pill colours.
6. **Focus model in the core**:
   - client → session → window → active pane, seeded once and then kept up by
     notifications.
   - Visibility (attached by a non-control client, with zoom) marks sessions seen
     (`seen_at`).
7. **Status line**:
   - Two lines with no `#()`.
   - The daemon writes `@moa_status`, `@moa_state` and `@moa_clock` in one command when
     they change. The counts come from slice 2's `moa_core::live`.
   - `status-interval 0`, and a `WallAlarm` minute-boundary clock (C18).
   - The window list shows only ⚠/? windows, with a ⤢ marker on zoom and the bell in red.
8. **Front door**:
   - `moa [BRAIN]` starts the daemon if needed and execs `tmux attach`.
   - Inside moa's own server it switches the client instead of nesting (spec §7 row 1).

**Carried from slice 2b:**
- Split `crates/moa-daemon/src/core_task.rs` (recover/reload/fatal) before tmux inputs join
  the loop: done (slice 3, Task 1).
- The daemon's PATH/environment: decided: T1 (slice 3).

**As built** (where slice 3 differs from the list above; the slice 3 plan has the detail):
- The control client runs `tmux -u -N <sock> -C new-session -A -s _moa -f
  no-output,ignore-size,no-detach-on-destroy -x 10 -y 3 /bin/sh`, not `attach`. With `-N`, it
  never starts a server.
- **The daemon never creates a user shell.** The front door attaches with `new-session -A -s
  moa -n moa -c <home>` in the user's own environment. Inside moa's own server, it creates `moa`
  with `-d` and then runs `switch-client`. `tmux.ensure` answers "running" once the link is up.
- The generated `tmux.conf` mirrors tmux's own default search, with each file sourced `-q`:
  `/etc/tmux.conf`, `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf` and
  `~/.config/tmux/tmux.conf`. It then sources `moa.tmux`, which has these properties:
  - its labels use no tmux words ("tab", "split");
  - a user prefix of `C-a` moves to `C-b`;
  - tmux 3.4 gets a plain gutter.
- **A tmux 3.4 bug is worked around:** a control client whose output write failed stays
  attached forever.
  - The workaround: the control client holds a read end of its own output pipe, and
    `Spawner` serialises every launch.
  - The limit: this covers only output that fits in the pipe. The rest is under slice 4.

**Done when:**
- In the sandbox, every key and the menu reach the daemon, and the status line updates,
  with **zero `Spawner` launches per key** (counter test).
- `kill-server` followed by a restart gives a new generation, which is detected.

**Try it:** `moa --sandbox` opens a throwaway brain (`MOA_ROOT` under
`~/.local/state/moa-sandbox`, with a socket inside the sandbox root (slice 3, T2)), with the
key table and status line.

## Slice 4: agents and hooks

Spec §6.7, §6.13 (sleep, reboot), §6.12 (peers-note plumbing only).

Slice 4 is split into two plans, 4a (hooks and the live state) and 4b (the link, resume,
reconcile, sleep, adopt). The split table is in `2026-11-17-moa-v1-slice-4a-hooks.md`, under
"Slice 4 is two plans".

**Detailed plans:** `2026-11-17-moa-v1-slice-4a-hooks.md` (4a, done) and
`2026-11-18-moa-v1-slice-4b-link-resume-sleep.md` (4b: items 6's rest, 7, 8, 9, 11 and 12).

1. **`HookEnvelope`** (spec §6.7 fields).
   - `moa hook claude` is a std-only path:
     - stdin capped at 8 MiB, plus the env fields, `at` (ns), pid and `seq`;
     - the socket is chosen by `MOA_BRAIN`, else by `route()`;
     - connect timeout 200 ms;
     - SessionStart waits up to 1 s for `additionalContext`;
     - when the daemon is down, it spools with `O_APPEND` and `flock`;
     - always exit 0.
   - Normalisation follows the C10 table.
   - Long texts are truncated by characters, keeping the head and the tail (600 + "[…]" +
     1800 when over 2400; lessons 22–24).
   - A measured budget test: < 15 ms per event on CI.
2. **Hook ingest in the daemon**:
   - The spool is drained at start, before reconcile.
   - Duplicates are dropped by `(pid, seq)`.
   - Events are ordered by `at`, and an event older than `activity_at` goes to history
     without changing state.
   - The route is re-checked on every event (events for another brain are dropped and
     logged).
   - Events bump `projects.last_activity_on`, as the spike's `track.sh` did, so "active this
     week" and "untouched since" stay true.
3. **Injected plugin**: `plugin/session/` is generated with exec-form async hooks for the
   C10 events. It is regenerated at start only when its content changes.
4. **Live-state model, event side** (`moa_core::live`, started in slice 2):
   - Events update activity, why and `activity_at`.
   - The quiet display.
   - `idle.ping` never means "needs you".
5. **Lifecycle**:
   - SessionStart/SessionEnd.
   - Rotation on `clear`, `fork` or `resume` adds a `conversations` row, linked by
     `MOA_SESSION` or by pane binding.
   - The quiet timer (10 min, "◌ quiet").
   - A pane exit with no SessionEnd → ended.
   - `MOA_SESSION=moa` is never a worker.
6. **Launch ops**:
   - `open` (prefill, title cut to 24 columns, `--session-id`, env allow-list) and `resume`
     (C11 rules 1–6, with the transcript check through `moa_agents::claude`).
   - `focus` selects the session's window, as a pending op until the pane exists. Swapping
     it into the cockpit's agent spot comes in slice 5.
   - `close`, `term`.
   - Opening across brains is refused (C14).
   - The tmux-needing suggestion actions are enabled.
7. **Reconcile at start (U7)**:
   - Compare generations: a different generation puts every live session to sleep.
   - Same generation: one `list-panes -a`, which verifies, binds or puts sessions to sleep.
     It also verifies the imported `spike_panes` candidates (P18): a pane still running
     that conversation becomes a binding, and anything else is cleared. Nothing
     auto-resumes.
8. **Sleep**:
   - A wall-clock deadline per idle session, the eligibility rules (spec §6.13), and
     `asleep_at` before the kill.
   - In this slice only panes outside the cockpit sleep. Parking an empty slot in the agent
     spot first comes in slice 5.
   - `sleep Q…` and `sleep --idle H [--dry-run]`.
   - The one dim feed line.
9. **Adopt and untracked panes** (C22, U14):
   - `moa_platform::proc` (`proc_listchildpids` + `KERN_PROCARGS2` on macOS, `/proc` on
     Linux).
   - `adopt PANE`, the untracked list in `ls`, and the "restart under moa" action.
10. **`fake-claude`** in the testkit: a binary that replays a scripted list of hook payloads
    (synthetic, scrubbed) through `moa hook`, and can `/clear`, die, or hang.
    The flake's `packages.default` builds only `-p moa` (`cargoExtraArgs`), so test binaries
    never land in the user's `bin/`.
11. **Carried from slice 3: the terminal link** (`crates/moa-daemon/src/tmux.rs`). Reconcile
    and launch ops lean on these, so they come first:
    - **Reap a stuck control client on connect.** If a stale `@moa_ctl` names a client that
      is still attached, kill its `#{client_pid}`.
      - Why: tmux 3.4 keeps a control client whose output write failed attached forever
        (3.8 does too, with many unread replies).
      - What it breaks: the keys stay silent instead of saying "moa isn't running", and
        `kill-server` blocks.
      - Slice 3's held read end covers only output that fits in the pipe (16–64 KiB).
    - **A link panic while Up** reaches the core as Lost only at the restart (1–30 s later).
      Until then, ensures answer "up".
    - **`lost()` marks the link Idle before it probes.** A panic there leaves the core at Lost,
      with no reconnect.
    - **A Stop with no live client** (after a restart, during backoff, or idle after a loss)
      leaves the index-90 hooks naming a dead client. They are replaced on the next connect.
      The front door could unhook.
    - **Backoff:** a restart during backoff starts over at step 0.
    - **Bounds:**
      - A hung server blocks the actor for up to 20 s, which is longer than the front
        door's 10 s.
      - `bootstrap` and `clean_up` aren't bounded.
      - A Protocol error on the first connect says "lost … couldn't reconnect".
    - **`ensure` answers "running" before the first refresh has listed the topology.**
      Reconcile and `focus` must not assume a filled topology at that point.
    - **Ensure waiters:**
      - waiters of a closed connection stay until a later answer (this needs a
        connection-closed signal);
      - `answer()` empties the waiters before a possible later panic.
    - **Reload:** if `Topology::from_lists` panics on the same input every time, the link
      loops forever: Reload, refresh, panic, Reload again.
    - **Reaper:** a wait-then-flag leaves a small window where a reused pid gets
      `kill_term`. It needs `waitid(WNOWAIT)` in `moa_platform::sys`.
    - **`run.rs` makes the brain dir itself**, with its own `DirBuilder`. Use
      `ensure_private_dir` there, which checks symlinks and ownership.
    - **Notices:** they are held views, so they drop with an unrelated failed commit. Revisit
      when notices start to depend on state.
    - **`detach-on-destroy off`:** with this in the user's config, the terminal drops into
      `_moa` when moa's last shell exits.
    - **`_moa`'s shell:** give it `/bin/sh` as two or more arguments, so the command doesn't
      go through the user's login shell. This applies to both the control client and the
      create command.
    - **The window pill has no glyph for a failed agent.**
    - **Prompts,** for the first slice that sends a `command-prompt`:
      - unescape `\$` only on 3.4, which needs the version in the core (on 3.5+ a typed
        `\$` would change);
      - parse `command-prompt -b` properly;
      - add a `Cmd` builder for double-quoted prompt templates (T8).
    - **Tests to watch or add:**
      - `a_server_that_detaches_the_client_on_attach_is_given_up_on` failed once on 3.8
        under load, and its panic text was lost. Capture the text if it fails again.
      - The front door's `kill-server` cleanup for a too-old tmux has no test (that needs a
        real tmux below 3.4).
      - The `after_hook` test can't tell a skipped hook from one never sent.
      - The clock e2e has no negative case.
      - The user-config e2e checks only `[5]`.
    - **Small cleanups** (any slice that touches the code):
      - The T23 guard has gaps: it matches exact needles only, and it skips tests under
        `src/`.
      - `new-session` stays on `Cmd`'s allow-list, but only a test uses it.
      - `Topology::session_named` is now test-only.
      - Front door duplication: the brain-name parse repeats `ctl::named`, and
        `make_moa_session` and `create` share their spawn and first-stderr-line code.
      - `Sandbox::tmux` looks tmux up on PATH, not through `tmux_path()`.
      - `write_if_changed` leaves an unchanged file's mode as it is.
      - A trailing `/` or `,` in `XDG_CONFIG_HOME` defeats the config dedupe, so the file is
        sourced twice or skipped.
      - The parser's `MAX_BLOCK` counts raw bytes.
      - The parse-error text is generic.
      - The front door's "no tmux" line differs from the core's.
12. **Carried from slice 4a** (for 4b):
    - **Reconcile ends more than bound sessions whose pane is gone.** It also ends:
      - a Live session with **no** binding. Two ways leave one: a bind that a refusal of
        writes turned down, and an agent that exited before its `open` was answered, when
        that end was refused too;
      - a session whose pane vanished with no `pane-exited` (a user closed the tab with
        `kill-window` or `kill-pane`).
    - **Resume puts the folder through `literal_format`.** Any `-c` built from
      `session.cwd` must be doubled like `open`'s, because a hook payload can set `cwd`
      (`CwdChanged`), and tmux would expand a `#(…)` in it.
    - **`CLAUDE_CONFIG_DIR` for the default Claude folder:** if the try-it shows it must not
      be set for `~/.claude`, unset it for that pane explicitly (for example `env -u` before
      the launcher). Never just leave it out: the tmux server's global environment may carry
      the user's own value.
    - **A relaunch uses `--resume`** (or `--session-id` with a conversation moa knows). Once
      a session has been heard from, a SessionStart with a new id and source `startup` is
      read as another agent started inside the tab, and dropped.
    - **Smaller leftovers from 4a's final review:**
      - A bind commit that fails replies "nothing was changed", although the tab opened.
      - If the store stops after an open's commit landed, `moactl open` can wait until the
        terminal link restarts (the client has no call timeout).
      - A drain while moa is up that keeps `.draining` after a failed commit (with no
        refusal) waits for the next spooled line or the next start: late, not lost.
      - Hooks replayed after a failed commit have no retry limit, so a line that always
        breaks the store would loop.
      - `moactl open` makes the folder before picking the brain, so a refusal can follow a
        "created" note: done (#46: `ctl.rs` now routes first).

**Done when:**
- Fake agents drive every display state.
- `kill-server` makes them come back 💤.
- Daemon-down hooks spool and drain with nothing lost.
- Out-of-order hooks never regress state.

**Try it:** `moa --sandbox`, then `moactl open ~/tmp/x` runs a real Claude session in the
sandbox, and its states show in `moactl ls`.

## Slice 5: TUI panels, layouts, picker

Spec §6.13, §6.14; R1 §2.2–2.7.

Slice 5 is three plans: 5a (the panels and their views), 5b (layouts, the cockpit and the agent
spot) and 5c (the picker, help and new sessions). The split table is in
`2026-11-18-moa-v1-slice-5a-panels-and-views.md`, under "Slice 5 is three plans".

**Detailed plans:** `2026-11-18-moa-v1-slice-5a-panels-and-views.md` (5a),
`2026-11-19-moa-v1-slice-5b-layouts-and-the-agent-spot.md` (5b). 5c's is written just before it
starts.

1. **`moa-view`**:
   - View-models for tree, plan, status, today, projects, activity and picture (the 7-line
     strip, with ✦ flashing for 4 s).
   - The `Action` enum and the default keymap.
   - Pure, with insta snapshots of the models.
2. **`moa panel` shell**:
   - Connects with its pane id, and the daemon assigns its view.
   - Redraws only on a push, input, resize, or the animation interval while something
     animates.
   - Theme tokens (spec §6.14), with a 256-colour fallback.
   - Rounded frames with the title on the top edge, and the footer editor.
   - The scroll model (lesson 20), width by grapheme, SGR mouse (wheel ±3 dragging the
     selection, click acts), and bracketed paste.
3. **Views**:
   - Tree, with its keys (`n + m r D a x`).
   - Plan, with its keys (`d x`), the due chips, and the ordering.
   - Status, today, projects (`n`/`N` with account choice), activity, picture.
   - TestBackend snapshots at widths 40, 60, 80, 120 and 200, including 💤 and CJK.
4. **Layouts**:
   - `moa_core::layout` (the spike's `layouts.json` format, validation, presets, merge,
     deleted presets coming back).
   - The apply algorithm in the daemon: move panes with swap/join/break, resize, park unused
     panels in `_moa`, and swap the agent spot.
   - The lazy cockpit (C28), `goto`, Repair, and refusing a cockpit split.
   - `focus` swaps the session into the agent spot, and the previous occupant returns to its
     window. Sleep parks an empty slot in the spot before killing an agent there.
   - Frame colours come from pane-level `window-style`/`window-active-style`, plus
     `@moa_label` for unframed panes (lessons 11–12).
5. **Picker and help**:
   - The full-window form (a parked panel linked as a window; two tmux commands, zero
     launches).
   - The popup form `display-popup -E -w 80% -h 70% moa pick` (U17, one launch per open).
   - `Ctrl-a /` opens the default form (a setting, popup by default) and `Ctrl-a s` the
     other.
   - The help view replaces `less`.
6. **Panel supervision**: a dead panel is logged and respawned on Repair. The panel sends
   `ui.here`.

**Carried from slice 2b:** the daemon's upgrade policy and `Patch`'s base rev, both under
slice 9, before panels subscribe.

**Done when:**
- Snapshots pass at every width.
- Sandbox e2e (via `capture-pane`) passes for: layout switch, picker, click to act.
- A layout switch, opening the full-window picker, and `focus` each make zero `Spawner`
  launches once the panels are parked.

**Try it:** `moa --sandbox` gives the full cockpit with fake agents.

## Slice 6: moa, the central agent

Spec §6.11; R3 §1.

1. **`moa_agents::acp` host**:
   - The pinned `agent-client-protocol` crate and pinned adapter; spawn through `Spawner`
     with the env (spec §6.11).
   - stderr goes to the log.
   - `session/update` is always drained into `feed`.
   - **`stub-acp`** in the testkit (same SDK, Agent role, scripted turns, permission
     requests, hangs, crashes).
     The flake's `packages.default` builds only `-p moa` (`cargoExtraArgs`), so test
     binaries never land in the user's `bin/`.
2. **Threads**:
   - One per local day per account. `session/resume`, else `session/load` with dedupe.
   - `systemPrompt` append = the embedded constitution + scope + brief (~2 KB).
   - A new account starts a new thread, and a failover card offers switching (C24).
3. **Feed and console view**:
   - The feed model and its paging.
   - The Markdown renderer (spec §6.11 list) and tool lines.
   - Turn grouping, buttons (names on buttons, Tab hopping), and the input line with its
     history and palette.
   - The button bar is a runtime list moa sets (and the user can override), not a fixed set;
     the user's call, 2026-11-18.
   - Console commands run in the daemon with no LLM turn.
   - **Real-adapter smoke test** (spec §11.6): replay a recorded real-adapter transcript
     whenever the pinned version changes.
   - Errors: once per message per 30 s, degraded rather than breaking the frame.
4. **Permissions**: a pending table, cards answered with `y/a/n` by option `kind`, and
   "moa needs you" in the status line when no console is attached.
5. **Reports and wake-ups**:
   - Per-(session, kind) queues, with caps of 6 turns, 500 and 2600 characters.
   - Timings: settle 45 s, gap 180 s, batch 8 (all settings).
   - One channel (either a wake prompt or a prefix on the user's next prompt).
   - Marked automatic by message id.
   - Reports from moa's own folder are ignored.
   - Proptests: each report is delivered exactly once.
6. **Opening brief, nags, suggestion cards**:
   - The brief runs on first presence after the day boundary (C19). Yesterday's
     thread-summary note feeds it (U19).
   - Nags are persisted in `nags` (C20), shown as `⏰ Due today` / `⏰ N days overdue`.
   - Suggestion cards appear everywhere and are updated wherever answered.
7. **Watchdog**: after 5 min with no update, cancel; 30 s later, kill + respawn + resume;
   a card says so.
8. **moa's agent dir**: the allow-list `settings.json` and the CLAUDE.md fallback.

**Done when:** the stub-ACP scenarios pass:
- a permission request;
- a slow turn;
- a hang → watchdog;
- a crash → resume;
- wake batching;
- an opening brief on the first attach after midnight.

**Try it:** `moa --sandbox` with the real adapter, against a copy of the private brain.

## Slice 7: partners

Spec §6.12; R1 §2.12; lessons 46–53.

1. **The ask state machine in the core**:
   - pending → sent → received → answered | failed | dropped.
   - Atomic claim, stale failing at 2× the wait, the deadlock guard, and preferring live
     sessions over ended ones.
   - Proptests over interleavings.
2. **Delivery**:
   - `set-buffer` + `paste-buffer -p -d`, then Enter after 0.5 s.
   - The 20 s receipt (the marker is matched anywhere, and the ask id is parsed).
   - A peer's next Stop answers with the full final message.
3. **Verbs**: `peers`, `ask` (a long request, default 540 s), `reply`, `asks`. Timeouts
   follow spec §6.12.
4. **Asks to moa**: between turns, a card plus a hidden ask prompt. "moa's turn ended
   early: reason" covers a stop that wasn't `end_turn`.
5. **The peers note** (SessionStart `additionalContext`) and the `moa-peers` skill text.

**Done when:** two fake agents and moa exchange asks, including every failure mode in spec
§6.12, and an ask makes zero `Spawner` launches.

## Slice 8: setup, brains, import

Spec §6.10, §6.12 (retention), §6.9.

1. **Account discovery** (`~/.claude`, `~/.claude-*`). State per account: tracked by which
   brain, limits, plugin installed, `cleanupPeriodDays`, last used, unreadable.
2. **Setup operations in the daemon**:
   - `setup list|install|only|remove|skip`, with the CLI wording from lesson 35.
   - The Setup view, plus the setup card in the console while an account is unconfigured.
3. **Brain creation from the TUI**: name, folders and accounts are written to `moa.toml`,
   then `moa service install`, then the daemon starts.
4. **Account plugin**:
   - A generated marketplace at `plugin/account/` and `claude plugin` CLI install/remove
     (60 s timeout, stdin closed).
   - SessionStart writes `MOA_BRAIN` and the start cwd to `CLAUDE_ENV_FILE`.
   - The settings-hook fallback (marker, backup once, atomic, symlink-safe, others' hooks
     untouched).
5. **First run**:
   - Checks the tmux and Claude Code versions.
   - Finds accounts, then asks for folders.
   - Seeds projects from git repos with `gix` (name, path, last commit date and subject).
6. **`import-herdr`**: parity, with a route per record and a dry-run report.
7. **`moa [BRAIN] --account A`** updates the brain's defaults in `moa.toml` when the brain
   is already running (spec §7).
8. **Retention**: show each account's cleanup age in setup. A wall-clock warning card arms
   before a tracked conversation reaches that age. **Deletion and the protection
   mechanism wait for Q1** (to discuss with the user before this slice's detailed plan).
9. **Carried from slice 2b** (any slice touching projects): canonicalise stored project
   paths once, at import or load, instead of `deps.canonical` per stored project inside
   `project.add` (`crates/moa-core/src/ops/projects.rs` ~155). It also fixes the "third
   spelling" back-link.
10. **Carried from slice 3: the front door** (`crates/moa/src/front.rs`), with `moa [BRAIN]`:
    - **Wording when the sandbox comes from `MOA_ROOT` rather than `--sandbox`:**
      - the "open a new terminal tab" line names plain `moa`, which refuses;
      - the reserved-word line names `--sandbox` even though it wasn't typed;
      - with `--sandbox`, the nested line still begins "the <b> moa is running".
    - **`switch-client` without `-c`** moves tmux's "best" client of the session, which may
      not be the one that ran `moa`.
    - **The config search misses Homebrew's tmux config.** Homebrew's tmux reads
      `$(brew --prefix)/etc/tmux.conf` in place of `/etc/tmux.conf`, and the generated search
      doesn't source it. Setup could find the path from the tmux binary's prefix.
    - **Two `moa` at once can lose a terminal.**
      1. A creates the server.
      2. B joins and attaches.
      3. A's second ensure then times out (10 s, on a loaded machine).
      4. A runs `kill-server` under B's live terminal.

      The fix: check `list-clients` before killing.

**Done when:** a fresh sandbox `HOME` goes from nothing to a working cockpit using only the
TUI.

## Slice 9: live update

Spec §6.15.

1. **Daemon re-exec**:
   - Triggered by a binary change (`notify` on the profile dir or `target/debug/moa`) or by
     `moa upgrade`.
   - Waits until moa is between turns, then flushes.
   - Clears `FD_CLOEXEC` on the listener, then `execv` with `MOA_LISTEN_FDS`.
   - The control client and the ACP adapter restart, and the thread resumes.
2. **Panels follow the build id** (P10): a panel re-execs itself in its pane.
3. **Config hot reload**: `moa.toml`, `layouts.json`, the theme, scope files and the
   constitution.
4. **Restart notices**: one console line per upgrade, using the four wordings in spec
   §6.15.
5. **Carried from slice 2b**:
   - Two builds used against one root restart each other's daemon on every call (D1 upgrades
     on any build mismatch; the build id hashes the binary's path). Upgrading only a "stale"
     daemon doesn't work as is: autostart canonicalises the exe to an immutable `/nix/store`
     path that never goes stale, so a Nix upgrade would then never upgrade. Settle the
     upgrade policy with re-exec, before console subscribers make every flip visible.
   - `Patch` carries only its new rev. Add a base rev when `subscribe.since` lands.

**Done when:** a `cargo build` during a sandbox session updates the daemon and every panel
with no lost state, no refused connection, and no lost hook.

## Slice 10: switch-over kit

Spec §9.

1. **Spike forwarders PR** (C25, a spike bug-fix-sized change): `track.sh`, the plugin's
   `bin/moactl` and `moa-proto/moactl` forward when `~/.local/state/moa/v1-live` exists.
   Merged well before the switch.
2. **`moa.toml` from the spike's config** (`moa switch` writes it, and the dry run shows it):
   - one brain per spike brain, with its `spike_db` path;
   - `moa_account`/`open_account` from the spike's `<brain>.env` files;
   - one account per spike account, with its store and launcher. The pairs are read locally
     from the spike's tables and confirmed by the user, never committed;
   - `setup.skipped` from `meta.spike_import`.
3. **`moa hook claude --from-spike`**: maps events by conversation id, and picks the brain
   from the matching `spike_db`.
4. **`moa switch --dry-run`**:
   - Snapshots, imports to a temp dir, and prints a per-record report plus counts against
     the source.
   - Exports back and diffs.
5. **`moa switch`**: the nine steps in spec §9, each logged, stopping at the first failure
   with that step's undo.
   - Step 1 backs up the spike DBs with `VACUUM INTO`.
   - Step 6 turns verified `spike_panes` into bindings (P18).
6. **`moa switch back`**:
   - Back up the frozen DBs with `VACUUM INTO`, move them aside, then export to the spike's
     paths. The export refuses leftover `-wal`/`-shm` files.
   - Remove the flag, re-source the spike config, run the spike launcher.
7. **The real home opens up**: remove slice 2's `MOA_ROOT` gate (`Paths::check_v1_allowed`
   and `MOA_REAL_HOME`), so `moa daemon`, autostart and `moa service` use the real home. The service file's
   `MOA_REAL_HOME=1` environment entry (slice 2b) goes with it, and so does the tmux link's sandbox-only gate (slice 3, T21).
8. **Rehearsal test**:
   - A sandbox server runs the real spike code from the repo with fake agents and copied
     brains.
   - Run `moa switch`, and assert no pane died and every session is mapped.
   - Run `moa switch back`, and assert the spike works on the exported data.
   - Repeat 3 times.
9. **Carried from slice 2b**, as one bundle:
   - The offline tools (`adopt-spike`/`export-spike`) hold the brain lock for the whole run,
     not just a probe (`crates/moa/src/adopt.rs` ~289).
   - `moa service install` creates `logs/` (launchd won't open a missing StandardErrorPath).
   - Make the fallback socket directory independent of `TMPDIR` (e.g. `/tmp/moa-<uid>`), so
     launchd, the client and the hook agree.
   - A CLI-level D20 test for `moa service install` without `MOA_ROOT` (PATH set to an empty
     dir, so a regression can't reach launchctl).
   - systemd: escape `$` as `$$` in `ExecStart`; `daemon-reload` after uninstall; reinstall
     with changed content restarts the unit.

**Done when:** the rehearsal passes repeatedly. Then the real switch happens, with the user
present.

## After the switch-over (U20, slice 11+)

- Once v1 works like the spike: remove the spike, the forwarders, the frozen DBs and the
  `# moa-spike` hooks (offered as a suggestion card), and lift the additive-only rule.
  Also remove the move-only code: `moa adopt-spike` and `moa export-spike` (export-spike is
  the way back, so it stays until the spike is gone), the store's `spike`, `import` and
  `export` modules, `moa switch`, `--from-spike`, and the testkit's spike fixture and
  generator, after re-basing `TypicalBrain` on a v1-native fixture.
- Then: packaging for others (U16), Codex, the worktree lifecycle, the retention mechanism
  (Q1), log rotation, `type: http` hooks if measured worth it, Claude messaging-socket delivery, a web
  UI/GUI, and collectors.
