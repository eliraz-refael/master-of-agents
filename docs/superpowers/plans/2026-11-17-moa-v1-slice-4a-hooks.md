# moa v1 slice 4a: hooks and the live state

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `moactl open ~/tmp/x` in a sandbox opens a Claude session in a new moa tab. Its hooks
reach the daemon through `moa hook claude`, and its state (working, needs you, asks you, failed,
done, quiet, idle, ended) shows in `moactl ls` and the status line. Hooks fired while the daemon
is down are spooled and drained, and out-of-order hooks never move a session backwards.

**Architecture:**
- **`moa_core::hook`** (pure) holds the hook vocabulary:
  - `HookLine` is what `moa hook` sends: the raw payload plus the environment, `at`, pid and
    `seq`;
  - `HookEnvelope` and `HookKind` are the normalized event the core reads.
- **`moa_agents::claude::normalize`** turns a Claude `HookLine` into an envelope, following the
  C10 table. It runs in the daemon, so a spooled line is normalized by the build that drains
  it.
- **`moa hook claude`** is a std-only path of the `moa` binary. It runs before clap and starts
  no tokio. It writes one JSON-RPC line to the brain's socket, or appends it to the spool.
- **The core** gains `Input::Hook`. One function applies an envelope to a session:
  - activity, why and `activity_at`;
  - the lifecycle and rotation;
  - history and activity rows;
  - `last_activity_on`.
  The quiet display is a function of `now`, with a wall-clock timer only to mark the moment.
- **The daemon:**
  - routes `hook` lines to the core;
  - answers SessionStart with `{context}`;
  - drains the spool at start;
  - writes the injected plugin (`plugin/session/`) when its text changed.
- **`open`** (fresh only) creates the session and its first conversation, sends one
  `new-window` with an explicit environment, and binds the pane from the reply. A pane that
  exits with no SessionEnd ends its session.

```
 tmux pane: claude (CLAUDE_CODE_PLUGIN_DIRS=<data>/plugin/session, MOA_SESSION, MOA_BRAIN, CLAUDE_CONFIG_DIR)
   └─ hook (exec form, async) ─► moa hook claude ─(200 ms connect)─► <run>/<b>.sock ─► session.rs ─► normalize ─► Input::Hook
                                     │ daemon down: spool/<b>.jsonl (O_APPEND + lock)        core: apply ─► Persist ─► status, ls
                                     └ SessionStart only: waits ≤ 1 s for {context} ─► stdout hookSpecificOutput
 moactl open ~/tmp/x ─► Op::SessionOpen ─► Persist(Session, Conversation) + Tmux[new-window -d -P …] ─► reply P %N pid ─► PaneBinding
```

**Tech Stack:** std (`UnixStream`, `File::lock`), `serde_json`, `uuid` (`v7` feature in
`moa-store` only), `insta`, `proptest`. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.7 (agents and hooks: C8, C9, C10, the envelope, ordering, the live-state table,
  rotation, interrupts);
- §6.8 (timers), §6.1 (allowed launches), §6.6 (the tmux layer: `-e` per pane);
- §6.12 (the peers note: plumbing only here), §11.5 (hook tests).

Roadmap: Slice 4 items 1–6 and 10, Review Focus 3 and 4. Research: R3 §2.2 (payloads), §3
(plugins), §6.1 (envelope), and R4 §3.2–3.3 (hook cost, spool). Parity source:
`spike/master-chat/hooks/track.sh`, `spike/moa-proto/moactl.py` (`open_session`,
`launch_cmd`), and `crates/moa-store/src/spike/triggers.sql` (`a_event`).

**Builds on** slices 2 and 3:
- the core: `handle`, `Tx` (`commit`, `ui`, `next_id`), `TimerKey`, `Effect::SetTimer`,
  `TmuxState` (waiters, `trusted_panes`, the generation), `HookMsg::{PaneExited, PaneDied}`,
  `Cmd` and its allow-list, `live::display_state`, `CallerInfo`, `Deps`;
- the daemon: `CoreHandle`, `session.rs`'s `Incoming::Hook`, which is dropped today
  (`session.rs` ~250), `run.rs`'s bring-up, the scheduler;
- the platform: `Paths::spool` (it exists), `Paths::socket`, `ensure_private_dir`, `which`;
- the testkit: `TempBrain`, `TypicalBrain`, `wait_for`, the daemon tests' `boot()`.

## Slice 4 is two plans

| Plan | Roadmap items | Done when |
|---|---|---|
| **4a (this one): hooks and the live state** | 1–5, 10, and `open` (fresh) from 6 | Fake agents drive every display state. Daemon-down hooks spool and drain with nothing lost. Out-of-order hooks never regress state. |
| 4b: the link, resume, reconcile, sleep, adopt | 11 (first), the rest of 6, 7, 8, 9 | `kill-server` makes sessions come back 💤, resume follows C11, idle sessions sleep, and untracked panes can be adopted |

**Why `open` is here:** without it, nothing launches a session that carries the injected plugin.
The try-it would then be fake agents only, and the spec §13 checks (`CLAUDE_CODE_PLUGIN_DIRS`,
exec-form `async` hooks, `--session-id` with `--name`) would wait a whole plan for a real Claude.

## PRs (roadmap P4)

Three PRs. Each branches from `main` after the previous one merged, and each gets
`/code-review medium --fix`, green CI, and "Restart needed: nothing; the spike is untouched".

| PR | Branch | Tasks | Title |
|---|---|---|---|
| a1 | `v1/slice-4a-1` | 1–3 | `v1 slice 4a (1/3): hook vocabulary, spool, moa hook` |
| a2 | `v1/slice-4a-2` | 4–6 | `v1 slice 4a (2/3): hooks in the core and the daemon` |
| a3 | `v1/slice-4a-3` | 7–10 | `v1 slice 4a (3/3): plugin, fake agents, open, end to end` |

This plan file, and the roadmap's split of slice 4, are committed in PR a1.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| H1 | **The hook sends the raw payload, and the daemon normalizes it.** `HookLine` = `{v: 1, agent, at_ns, pid, seq, env, payload}`. `moa_agents::claude::normalize` makes the `HookEnvelope` in the daemon. | The hook stays tiny (no per-event logic to upgrade), and a spooled line is read by the build that drains it. The cost is one JSON parse in the hook, which it needs anyway to see `hook_event_name`. |
| H2 | **`HookLine`, `HookEnvelope` and `HookKind` live in `moa_core::hook`**, and `moa_proto::hook` re-exports them. The roadmap's names table says `moa_proto::hook`. | `Input::Hook` carries an envelope, and `moa-core` can't depend on `moa-proto` (same reason as slice 3's T22). Task 1 updates the roadmap table. |
| H3 | **`raw` isn't kept.** The envelope carries typed fields only, and history rows hold a small `detail` (`source`, `reason`, `tool`, `error`). | `tool_input`/`tool_response` can be megabytes per tool call, and may hold secrets. Forward compatibility comes from normalizing in the daemon (H1): a new field is a change to `normalize`. |
| H4 | **Dedupe key is `(pid, seq, at_ns)`**, kept in memory for the last 4096 events (`State::recent_hooks`). `moa hook` sends one line per process, so `seq` is always 0 for now. | Spec says `(pid, seq)`. Adding `at_ns` stops a reused pid from being taken as a duplicate. A duplicate needs the line both delivered and spooled, which happens within seconds, so a short memory is enough. |
| H5 | **Every event moves `activity_at` forward, and only a newer event changes state.** An event with `at` (ms) earlier than the session's `activity_at` writes its history row only: no state, no activity row. Equal times apply in arrival order. | Spec §6.7 event ordering. The quiet timer is "reset by every event", so `needs.cleared` and `cwd.changed` move `activity_at` too. |
| H6 | **History rows** (`events`) for every kind except `needs.cleared` and `cwd.changed`. **Activity rows** follow the spike's `a_event` trigger, `who = session`, `ref_kind = "session"`:<br>• `turn.started`: `<folder> › <prompt, one line, 90 chars>`;<br>• `turn.ended`: `<folder> ✓ turn done`;<br>• `needs.*`: `<folder> ⚠ needs you`;<br>• `session.ended` and a pane exit: `<folder> ○ ended`;<br>• new: `turn.failed`: `<folder> ✗ failed`.<br>`<folder>` is the last component of the session's cwd. | `PostToolUse` fires on every tool call, so a history row each time would grow the DB for nothing. The feed keeps the spike's wording. |
| H7 | **Quiet is a display state, computed from `now`.** `live::QUIET_AFTER` = 10 min. A live, working session whose `activity_at` is at least that old shows `Quiet` (`◌`, word `quiet`), with why `quiet <N>m`. `TimerKey::Quiet` is one wall-clock deadline: the earliest `activity_at + QUIET_AFTER` among working sessions. It's re-armed after every input that touches sessions and when it fires. The status line doesn't count quiet. | Spec §6.7 interrupts: Esc fires no Stop. Spec §6.8 lists quiet under short monotonic timers, but the daemon has one scheduler, a wall-clock one. A wall clock is right here, since the display is computed from wall `now`. Quiet means "probably not busy", so counting it as working would hide exactly what it shows. |
| H8 | **Which session an event belongs to:**<br>• `MOA_SESSION` = `moa` → dropped silently (moa's own thread, slice 6; lesson 31);<br>• `MOA_SESSION` parses as a known `SessionId` → that session;<br>• otherwise, a conversation whose id is the payload's `session_id` → its session;<br>• otherwise → dropped, with a log line naming no ids.<br>An envelope whose `MOA_BRAIN` names another brain is dropped and logged before any of this. | Roadmap item 2: "the route is re-checked on every event". A session never changes brain (Review Focus 4), so a known session's events stay here whatever its cwd says. Hooks come only from moa-launched sessions (C9), so an unknown session is a stray. |
| H9 | **Rotation.** A `session.started` whose `session_id` isn't one of the session's conversations adds a `Conversation`. Its reason comes from `source`: `clear` → `Clear`, `fork` → `Fork`, `resume` → `Resume`, `compact` → `Clear`, `startup` → `Start`. `store_dir` is the event's `CLAUDE_CONFIG_DIR` (C11 rule 1: the store it was recorded in). A `session.ended` with reason `clear` or `resume` leaves the session live. Any other reason ends it. | Spec §6.7 rotation. `compact` with a new id is a context reset, as `/clear` is. |
| H10 | **The spool is drained through a `.draining` file.** `take`:<br>1. locks `<b>.jsonl`;<br>2. appends its lines to `<b>.jsonl.draining` and syncs it;<br>3. truncates `<b>.jsonl`, and unlocks.<br>The daemon feeds every line of `.draining`, sorted by `at_ns`, after `Startup`. It deletes `.draining` once the batch is committed. A `.draining` left by a crash is drained first. Unparseable lines are counted in the log, never printed. | Nothing is lost: a hook that appends during the drain lands in the fresh `<b>.jsonl`. A crash after the commit but before the delete re-feeds lines, and H5 keeps their state change from applying twice. Their history rows can repeat, which is the one cost. |
| H11 | **The hook connects within 200 ms using a helper thread** (`connect_within`): std's `UnixStream::connect` has no timeout, and a full backlog blocks it. Writes have a 200 ms timeout. Any failure spools the line. Only SessionStart reads a reply, for up to 1 s. | Spec C8, with no `unsafe` and no new dependency. |
| H12 | **Payload size.** The hook reads at most `HOOK_STDIN_CAP` = 8 MiB + 1 byte. If stdin is larger than `HOOK_PAYLOAD_ROOM` = 8 MiB − 64 KiB, the payload is cut down to `hook_event_name`, `session_id`, `cwd`, `transcript_path`, and the short fields `normalize` reads (`tool_name`, `source`, `reason`, `notification_type`, `new_cwd`), plus `"moa_cut": true`. | The line must fit the wire's `MAX_LINE` (8 MiB). Only tool fields get that big, and moa doesn't use them (H3). |
| H13 | **The injected plugin's hook command is the daemon's own executable** (`std::env::current_exe()`), until slice 8's setup picks the stable profile path (C8). The plugin dir is `Paths::session_plugin()` = `<data_dir>/plugin/session`, one per root, shared by its brains (the hook finds its brain from `MOA_BRAIN`). | No profile path exists before the switch-over, and v1 is never on PATH. A changed exe path changes the text, so the daemon rewrites the file at its next start. |
| H14 | **`open` in 4a is fresh only, and always in the background.** Every open is `new-window -d`. `--bg` is accepted and changes nothing until `focus` (4b). `resume`, `focus`, `close` and `term` stay `NotYet`. | `focus` is the cockpit's job (4b, then slice 5). The user reaches the new tab with `Ctrl-a ]`. |
| H15 | **The window name is cut to 24 characters** (`chars`, not display columns). | Spec says columns. moa has no width function before `moa-view` (slice 5), and a wide glyph in a title only makes the name a little wider. |
| H16 | **A missing `launcher` is resolved once, at daemon start**: `load_config` fills each Claude account's `launcher` from `which("claude", PATH)` when it's unset. An account still without one can't open. | Spec: "absolute path, resolved at setup; default `claude`". Setup is slice 8, and the core can't search PATH. |
| H17 | **New ids come from `Deps::new_uuid: fn(UtcMs) -> Uuid`** (UUIDv7). `moa-store` supplies it (`moa_store::new_uuid`), and the core never generates randomness. The same function makes the session id and the fresh `--session-id`. | Roadmap: id generation lives in `moa-store` (`getrandom`). Claude accepts v4 or v7 (R3 §5). |
| H18 | **A pane that exits ends its session** (`pane-exited` or `pane-died`, through a binding trusted under slice 3's T11): lifecycle `Ended`, the binding deleted, and an `○ ended` activity row. Nothing else checks process liveness in 4a. | Spec §6.7: a pane exit with no SessionEnd sets ended (lesson 3). Reconcile at start is 4b. |
| H19 | **The peers note is plumbing only.** The core answers a SessionStart request with `Done{data: {"context": peers_note(..)}}`, and `peers_note` returns `None` until slice 7. The hook prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":TEXT}}` only for a non-empty text. | Roadmap slice 4: "§6.12 (peers-note plumbing only)". |
| H20 | **`fake-claude` is a `[[bin]]` of `moa-testkit`**, found by `moa_testkit::fake_claude()` (next to the test executable's `deps/` dir). It reads the plugin's `hooks/hooks.json` from `CLAUDE_CODE_PLUGIN_DIRS`, like Claude does, and runs each matching hook. | It tests the generated plugin, not a copy of it. `packages.default` builds `-p moa` only, so the bin never reaches the user. `CARGO_BIN_EXE_*` only works within one package. |

## Parity rows this slice delivers (spec §7)

- Hook tracking (2.3): `moa hook`, the spool, and C10's events, in place of `track.sh`.
- Live states and why (2.4): the event side, including failed and quiet (new).
- `open` (2.2): fresh, in the background (part: focus and resume are 4b).
- Activity feed lines from events (2.10): the `a_event` wording.
- Projects' "active this week" (2.7): `last_activity_on` bumped by events.

## Global Constraints

The roadmap's, 2a's, 2b's and 3's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`, tests with `cargo nextest run`.
- **`moa hook` never fails the agent:**
  - it always exits 0;
  - it writes nothing to stderr;
  - it prints to stdout only SessionStart's `hookSpecificOutput` JSON;
  - it starts no tokio runtime and launches no process.
- **Hook timing (spec C8):**
  - connect 200 ms; write 200 ms;
  - only SessionStart waits, at most 1 s;
  - median below 15 ms per event, measured.
- **Hook payloads in tests are synthetic.** They use the roadmap's made-up names (`Lantern`,
  `acct-a`, `~/private`), and are never copied from a real transcript or session.
- **No record contents in logs.**
  - Hook drops log only a reason (`a hook event for another brain was dropped`, and similar).
  - Spool problems log counts.
- **Never touch the user's tmux servers or real brains.** Tests use `TempBrain` and `Sandbox`.
  The try-it uses `moa --sandbox`.
- **Zero launches** per hook event in the daemon. `open` makes no launch either: the pane is
  tmux's child.

## Review Focus

1. **The daemon is down, starting or stopping while hooks fire** (roadmap RF 3).
   - Expected: every event lands exactly once in state; the agent sees exit 0 and no output.
   - Pinned by `hooks_while_down_are_spooled_and_drained` (Task 10),
     `a_hook_during_the_drain_is_kept` (Task 2), and `a_leftover_draining_file_is_drained_first`
     (Task 6).
2. **Async hooks race.** `Stop` and a late `PostToolUse` arrive out of order, and duplicates
   arrive.
   - Expected: state never moves to an older event; a duplicate changes nothing.
   - Pinned by `an_older_event_only_adds_history`, `a_duplicate_changes_nothing` (Task 4), and
     `out_of_order_hooks_never_regress` (Task 10).
3. **A hook from somewhere else:** another brain's session, an unknown or garbage
   `MOA_SESSION`, moa's own thread, a payload that isn't JSON, an empty stdin.
   - Expected: dropped quietly and logged without contents; never an error to the agent.
   - Pinned by `events_for_other_brains_and_moa_are_dropped` (Task 4) and `garbage_stdin_exits_0`
     (Task 3).
4. **Paths in the hook's routing** (roadmap RF 4): no `MOA_BRAIN`, cwd under `~/private2`
   when the brain's folder is `~/private`.
   - Expected: whole-component matching picks the right socket, or none.
   - Pinned by `without_moa_brain_the_route_picks_the_socket` (Task 3).
5. **Huge payloads:** a `PostToolUse` with megabytes of `tool_response`.
   - Expected: the line still fits the wire and the event still applies.
   - Pinned by `a_huge_payload_is_cut_to_fit` (Task 3).

---

## Task 1: hook vocabulary and Claude normalization

**Files:**
- Create: `crates/moa-core/src/hook.rs` (export from `lib.rs`)
- Create: `crates/moa-proto/src/hook.rs` (re-exports only)
- Modify: `crates/moa-core/src/live.rs` (`cut_middle`)
- Create: `crates/moa-agents/src/claude/normalize.rs`. Move today's `claude.rs` to
  `claude/mod.rs`, which keeps `find_transcript` and adds `pub mod normalize;
  pub use normalize::normalize;`.
- Create: `crates/moa-agents/tests/normalize.rs`, and
  `crates/moa-agents/tests/fixtures/claude/*.json` (synthetic payloads, one per event)
- Modify: `docs/superpowers/plans/2026-11-15-moa-v1-roadmap.md` (names table: `HookLine`,
  `HookEnvelope` and `HookKind` in `moa_core::hook`, re-exported by `moa_proto::hook`;
  the slice 4 section gets a line pointing to the 4a and 4b split above)

**Interfaces:**
- Produces, in `moa_core::hook`:
  - `pub struct HookLine { pub v: u32, pub agent: AgentKind, pub at_ns: i64, pub pid: u32, pub seq: u32, pub env: HookEnv, pub payload: serde_json::Value }`
    (Serialize, Deserialize, Clone, Debug, PartialEq).
  - `pub struct HookEnv { pub moa_session: Option<String>, pub moa_brain: Option<String>, pub claude_config_dir: Option<String>, pub claude_session_id: Option<String>, pub tmux: Option<String>, pub tmux_pane: Option<String> }`
    (same derives, plus Default).
  - `pub const HOOK_LINE_V: u32 = 1;`
  - `pub struct HookEnvelope { pub agent: AgentKind, pub slot: Option<String>, pub brain: Option<String>, pub store_dir: Option<String>, pub conversation_id: String, pub kind: HookKind, pub at: UtcMs, pub at_ns: i64, pub pid: u32, pub seq: u32, pub cwd: Option<String>, pub transcript_path: Option<String> }`
    (Clone, Debug, PartialEq).
  - `pub enum HookKind { SessionStarted { source: StartSource }, SessionEnded { reason: String }, TurnStarted { prompt: String }, TurnEnded { last_message: String }, TurnFailed { error: String }, NeedsPermission { tool: String }, NeedsAnswer { what: String }, NeedsCleared, CwdChanged { cwd: String } }`
    with `pub fn event_kind(&self) -> &'static str`, which returns the `EventKind` constants
    (`session.started`, … `cwd.changed`).
  - `pub enum StartSource { Startup, Resume, Clear, Compact, Fork }`, with
    `from_word(&str) -> StartSource` (unknown → `Startup`) and
    `reason(self) -> ConversationReason` (per H9).
  - `impl HookEnvelope { pub fn dedupe_key(&self) -> (u32, u32, i64) }`.
- Produces, in `moa_core::live`:
  `pub fn cut_middle(text: &str) -> String`. Over 2400 characters, it keeps the first 600, then
  `"\n[…]\n"`, then the last 1800. It counts characters, not bytes.
- Produces, in `moa_agents::claude`:
  `pub fn normalize(line: &HookLine, tail: &dyn Fn(&Path) -> Option<String>) -> Option<HookEnvelope>`.
  `tail` reads the last assistant text of a transcript (lesson 22). The daemon passes
  `last_assistant_text`, and tests pass a closure.
- Produces: `pub fn last_assistant_text(transcript: &Path) -> Option<String>`. It reads at most
  the last 256 KiB. Of the JSONL lines with `type == "assistant"`, it takes the last `text`
  item of `message.content[]`, and it never fails loudly.

**The C10 table, as `normalize` applies it** (payload field → envelope):

| `hook_event_name` | Condition | `HookKind` |
|---|---|---|
| `SessionStart` | | `SessionStarted { source: from_word(source) }` |
| `UserPromptSubmit` | | `TurnStarted { prompt: cut_middle(prompt) }` |
| `Stop` | | `TurnEnded { last_message: cut_middle(last_assistant_message, else tail(transcript_path), else "") }` |
| `StopFailure` | | `TurnFailed { error }`: `error` with `_` → space (`rate limit`), else `"failed"` |
| `PermissionRequest` | | `NeedsPermission { tool: tool_name, else "a tool" }` |
| `PreToolUse` | `tool_name` = `AskUserQuestion` | `NeedsAnswer { what: tool_input.questions[0].question, else "a question" }` |
| `PreToolUse` | `tool_name` = `ExitPlanMode` | `NeedsAnswer { what: "plan approval" }` |
| `PreToolUse` | any other tool | `None` |
| `PostToolUse`, `PostToolUseFailure`, `PermissionDenied` | | `NeedsCleared` |
| `Notification` | `notification_type` = `elicitation_dialog` | `NeedsAnswer { what: message, else "a form" }` |
| `Notification` | anything else (`idle_prompt` included) | `None`: idle pings never mean needs you |
| `SessionEnd` | | `SessionEnded { reason, else "other" }` |
| `CwdChanged` | | `CwdChanged { cwd: new_cwd }` (`None` without it) |
| anything else, or no `session_id`, or `v` ≠ `HOOK_LINE_V` | | `None` |

The other fields:
- `slot`, `brain` and `store_dir` come from `env.moa_session`, `env.moa_brain` and
  `env.claude_config_dir`.
- `conversation_id` is the payload's `session_id`, else `env.claude_session_id`.
- `at` = `UtcMs(at_ns / 1_000_000)`.
- `cwd` and `transcript_path` come from the payload.

- [ ] **Step 1: Write the failing tests** in `crates/moa-agents/tests/normalize.rs`. Each test
  builds a `HookLine` from a fixture plus a `HookEnv`:
  - `each_subscribed_event_normalizes`: one case per row of the table, comparing the whole
    `HookKind`;
  - `idle_prompt_and_other_notifications_are_dropped`: `normalize(..) == None`;
  - `a_stop_without_its_message_reads_the_transcript_tail`: the closure returns
    `Some("from the tail")` → `TurnEnded { last_message: "from the tail" }`;
  - `a_long_reply_keeps_its_head_and_tail`: 3000 `é`s → 600 + `"\n[…]\n"` + 1800 characters;
  - `a_line_from_another_version_is_dropped`: `v: 2` → `None`;
  - `env_fields_fill_the_envelope`: `slot`, `brain`, `store_dir`, `at` = `at_ns / 1e6`.
  
  In `crates/moa-core/src/hook.rs` (unit tests):
  - `start_sources_map_to_reasons`: per H9;
  - `kinds_name_their_event`: `event_kind()` for every variant matches `EventKind`'s constants.
  
  In `crates/moa-agents/tests/normalize.rs`, `last_assistant_text_takes_the_last_text_item`:
  a temp JSONL file with two assistant lines and a user line → the second assistant's last
  text.
- [ ] **Step 2: Run them to see them fail**: `nix develop --command cargo nextest run -p moa-core
  -p moa-agents`. Expected: compile errors (no `hook` module, no `normalize`).
- [ ] **Step 3: Implement** the types, `cut_middle`, `normalize` and `last_assistant_text`, as
  in the Interfaces. Also:
  - `moa_proto::hook` is `pub use moa_core::hook::*;`;
  - `moa-agents` gains a `serde_json` dependency (workspace).
- [ ] **Step 4: Run them to see them pass** (same command). Expected: PASS. Then `cargo clippy
  --workspace --all-targets -- -D warnings`.
- [ ] **Step 5: Commit**: `v1: hook vocabulary and Claude hook normalization`.

## Task 2: the spool and a bounded connect

**Files:**
- Create: `crates/moa-platform/src/spool.rs`
- Create: `crates/moa-platform/src/net.rs` (`connect_within`)
- Create: `crates/moa-platform/tests/spool.rs`, `crates/moa-platform/tests/net.rs`

**Interfaces:**
- Produces, in `moa_platform::spool`:
  - `pub fn append(path: &Path, line: &[u8]) -> io::Result<()>`:
    - creates the parent with `ensure_private_dir`;
    - opens with `O_APPEND | O_CREAT`, mode `0o600`;
    - takes `File::lock()`, writes `line` plus `\n` in one `write_all`, then unlocks.
  - `pub fn take(path: &Path) -> io::Result<Taken>`: H10's steps 1–3. A missing spool is
    fine, and gives the `.draining` lines alone.
  - `pub struct Taken { pub lines: Vec<String>, draining: PathBuf }` with
    `pub fn done(self) -> io::Result<()>` (deletes `.draining`).
  - `pub fn draining(path: &Path) -> PathBuf` (`<path>.draining`).
- Produces, in `moa_platform::net`:
  `pub fn connect_within(path: &Path, limit: Duration) -> io::Result<UnixStream>`. It runs
  `UnixStream::connect` on a helper thread and waits on a channel with `recv_timeout`. On
  timeout it returns `io::ErrorKind::TimedOut`, and the thread is left to finish.

- [ ] **Step 1: Write the failing tests:**
  - `append_writes_whole_lines_with_mode_600`;
  - `concurrent_appenders_never_interleave`: 8 threads × 200 lines of 64 KiB each → every line
    reads back whole;
  - `take_moves_lines_to_draining_and_empties_the_spool`;
  - `a_hook_during_the_drain_is_kept`: `take`, then `append` → the new line is in the spool,
    not in `Taken.lines`;
  - `a_leftover_draining_file_comes_first`: a `.draining` holding line A and a spool holding
    line B → `take` gives `[A, B]`;
  - `done_deletes_draining`;
  - `connect_within_reaches_a_listener` and `connect_within_fails_fast_without_one`: under
    50 ms, `NotFound` or `ConnectionRefused`.
- [ ] **Step 2: Run them to see them fail**: `cargo nextest run -p moa-platform`.
- [ ] **Step 3: Implement** both modules (std only, no `unsafe`).
- [ ] **Step 4: Run them to see them pass**, and clippy for the Linux target too
  (`--target x86_64-unknown-linux-gnu -p moa-platform`, as in slice 3).
- [ ] **Step 5: Commit**: `v1: the hook spool and a bounded connect`.

## Task 3: `moa hook claude`

**Files:**
- Create: `crates/moa/src/hook.rs`
- Modify: `crates/moa/src/main.rs` (intercept before `is_moactl` and clap)
- Modify: `crates/moa-core/src/config.rs` (`RESERVED_BRAIN_NAMES` gains `"hook"`)
- Create: `crates/moa/tests/hook.rs`

**Interfaces:**
- Consumes: `HookLine`, `HookEnv`, `HOOK_LINE_V` (Task 1); `spool::append`,
  `net::connect_within` (Task 2); `Paths::from_env`, `Paths::socket`, `Paths::spool`,
  `Paths::read_config`, `Config::from_file_text`, `route`, `caller_store`,
  `Config::account_for_store`, `canonical`.
- Produces: `pub fn run(args: &[OsString]) -> ExitCode`. It always returns
  `ExitCode::SUCCESS`, and `main` calls it when `args[1] == "hook"`.
- Produces: `pub const HOOK_STDIN_CAP: u64 = (8 << 20) + 1;` and
  `pub const HOOK_PAYLOAD_ROOM: usize = (8 << 20) - (64 << 10);`.

**What `run` does, in order:**
1. `at_ns` = `SystemTime::now()` since the epoch, the first thing it does.
2. Agent: `args[2]` must be `claude`. Anything else returns at once.
3. It reads stdin up to `HOOK_STDIN_CAP` and parses it as JSON. If that fails, it returns: no
   event, no spool.
4. It applies H12 when the input is bigger than `HOOK_PAYLOAD_ROOM`.
5. `env` = the six variables, each `None` when unset or empty.
6. **Brain:**
   - `MOA_BRAIN`, when set;
   - otherwise, read `moa.toml` and run `route(config, account_for_store(caller_store(CLAUDE_CONFIG_DIR)), canonical(payload.cwd), None)`;
   - `Route::NotAvailable`, or no config: return.
7. **Line:** with `id: 1` for SessionStart, else no `id`:
   `{"jsonrpc":"2.0","method":"hook","params":<HookLine>}`. `seq` is 0, and `pid` is
   `std::process::id()`.
8. **Send:**
   - `connect_within(socket, 200 ms)`, then a 200 ms write timeout and `write_all(line + "\n")`;
   - on any error, `spool::append(paths.spool(brain), <HookLine JSON>)`, and errors there are
     ignored too.
9. **SessionStart only:**
   - 1 s read timeout; read one line;
   - if it's a `result` with a non-empty string `context`, print H19's JSON on one line.
10. Return `ExitCode::SUCCESS`.

- [ ] **Step 1: Write the failing tests** in `crates/moa/tests/hook.rs`:
  - **Setup:** each test runs `env!("CARGO_BIN_EXE_moa") hook claude` with `env_clear()`, plus
    `TempBrain::env()`, plus the hook variables. A `std::os::unix::net::UnixListener` bound at
    `paths.socket(b)` stands in for the daemon.
  - `delivers_one_line_to_the_brains_socket`: the listener gets one line, a notification, with
    `method == "hook"`, `params.payload` equal to the input, `params.env.moa_session` set, and
    `params.at_ns` between the before and after times.
  - `session_start_prints_the_context`: the listener answers
    `{"jsonrpc":"2.0","id":1,"result":{"context":"hi"}}`. Stdout is then exactly
    `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"hi"}}\n`.
  - `session_start_without_context_prints_nothing`: `context: null` → empty stdout.
  - `session_start_gives_up_after_a_second`: a listener that never answers → it exits 0 in
    under 1.5 s with empty stdout.
  - `with_the_daemon_down_the_line_is_spooled`: no listener → `spool/<b>.jsonl` holds one
    `HookLine`; exit 0, empty stdout and stderr.
  - `without_moa_brain_the_route_picks_the_socket`:
    - `moa.toml` has brain `work` with folder `<root>/private`, and `default`;
    - a cwd under `<root>/private2/x` reaches `default`'s socket;
    - a cwd under `<root>/private/x` reaches `work`'s.
  - `garbage_stdin_exits_0`: an empty stdin, `not json`, and a JSON array → exit 0, nothing
    sent, nothing spooled, empty stderr.
  - `a_huge_payload_is_cut_to_fit`: a 9 MiB `tool_response` → the line received is under
    `MAX_LINE`, with `payload.moa_cut == true` and the `hook_event_name` kept.
  - `hook_costs_under_15_ms`:
    - 21 runs of a `UserPromptSubmit` against a listener thread;
    - the median wall time per process must be < 15 ms;
    - the test prints the median.
    - If the debug build alone misses it on CI, stop and report. Don't raise the number
      (roadmap: "a measured budget test: < 15 ms per event on CI").
  
  In `crates/moa/src/main.rs` tests, `every_verb_and_subcommand_is_reserved` already exists. Add
  `hook` to its subcommand list.
- [ ] **Step 2: Run them to see them fail**: `cargo nextest run -p moa --test hook`.
- [ ] **Step 3: Implement** `hook.rs` and the `main.rs` intercept. Use no `moa-client` and no
  tokio. `serde_json` writes the line.
- [ ] **Step 4: Run them to see them pass**, then the whole `moa` crate's tests.
- [ ] **Step 5: Commit**: `v1: moa hook claude, std only, never failing the agent`.

**PR a1 ends here:**
- run the full verify (fmt, clippy on mac and Linux, `nextest --workspace` three times,
  `nix flake check`);
- run `/code-review medium --fix`;
- open the PR. Its body says what the hook does, the measured median, and "Restart needed:
  nothing; the spike is untouched".

## Task 4: hooks in the core: activity, ordering, history

**Files:**
- Modify: `crates/moa-core/src/effect.rs` (`Input::Hook`)
- Create: `crates/moa-core/src/hooks.rs` (the apply logic; `core.rs` dispatches to it)
- Modify: `crates/moa-core/src/state.rs` (`recent_hooks`)
- Create: `crates/moa-core/tests/hooks.rs`

**Interfaces:**
- Consumes: `HookEnvelope`, `HookKind`, `cut_middle` (Task 1); `Tx`, `Change`, `State`.
- Produces: `Input::Hook { reply: Option<(ConnId, ReqId)>, envelope: HookEnvelope }`.
  - With `reply`, the core always answers once, after the commit:
    `Done { text: "", data: {"context": <peers_note>} }` for a `SessionStarted`, else
    `{"context": null}`.
  - An envelope that's dropped (H8, H4) still answers `{"context": null}`.
- Produces: `pub(crate) fn apply(state: &mut State, tx: &mut Tx, env: &HookEnvelope) -> Applied`,
  where `Applied { Dropped(&'static str), Duplicate, HistoryOnly, Applied }`.
- Produces: `State::recent_hooks: VecDeque<(u32, u32, i64)>` (cap `RECENT_HOOKS = 4096`), plus
  a `HashSet` for lookup. A `Reload` clears both.

**What `apply` does.** In this task, `SessionStarted` and `SessionEnded` get steps 1–4 and the
times only. Task 5 adds their lifecycle.
1. **Drop or resolve**, per H8:
   - another brain → `Dropped("a hook event for another brain was dropped")`;
   - moa → `Dropped("")`, which isn't logged;
   - unknown → `Dropped("a hook event for an unknown session was dropped")`.
   A non-empty reason becomes `Effect::Log` at info.
2. **Dedupe**, per H4: a seen key → `Duplicate`, with no change.
3. **History row** (H6 kinds):
   - `Change::Event` with `at`, `session_id`, `conversation_id`, `account` (the session's),
     `cwd` and `kind` = `event_kind()`;
   - `text` is the kind's text (prompt, last message, error, tool, what, reason, source);
   - `detail` is `{"source"|"reason"|"tool"|"error": …}` for the kinds that have one, else
     `None`.
4. **Ordering**, per H5: `env.at < session.activity_at` → `HistoryOnly`. The row from step 3
   is still committed.
5. **State**, on a session clone, then one `Change::Session`:
   - always: `activity_at` = `at`, `last_seen_at` = `at`, and `cwd` = the envelope's cwd when
     there is one;
   - `TurnStarted`: `activity` = `Working`, `last_prompt` = prompt, `activity_why` = prompt;
   - `TurnEnded`: `Idle`, `last_turn_end_at` = `at`, why = last message;
   - `TurnFailed`: `Failed`, why = error;
   - `NeedsPermission`: `NeedsPermission`, why = `permission: <tool>`;
   - `NeedsAnswer`: `NeedsAnswer`, why = what;
   - `NeedsCleared`: when `activity` is `NeedsPermission` or `NeedsAnswer`, set `Working` with
     why = `last_prompt`; otherwise nothing but the times;
   - `CwdChanged`: `cwd` = the new cwd (display only; never a brain change).
6. **Project:**
   - when the session has a `project_id` and that project's `last_activity_on` ≠
     `now.local_day`, add a `Change::Project` with it set to today;
   - `updated_at` doesn't change: it's not an edit.
7. **Activity row**, per H6, through the same row builder `Tx::ui_each` uses, with `who` =
   `Session`. Skipped for `HistoryOnly`.
8. **Commit** through `tx.commit`. A refused write (`refuse_writes`) answers `{"context": null}`
   and logs, as ops do.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/hooks.rs`:
  - **Setup:** `TypicalBrain::state()` plus one live session `S`, seeded with
    `activity_at = T0`, a project, and a current conversation `C`. Inputs go through `handle`.
  - `a_prompt_makes_it_working`: `TurnStarted` at `T0+1s` → `display_state` is `Working`, why
    is the prompt, and `last_prompt` is set. The effects hold one `Persist`, whose changes are
    a `Session`, an `Event` of kind `turn.started` and an `Activity` with text
    `<folder> › <prompt>`.
  - `stop_makes_it_done_until_seen`: `TurnEnded` → `Done`, `last_turn_end_at` = `at`. With
    `seen_at` after it → `Idle`.
  - `a_reply_ending_in_a_question_asks_you`: last message `"…ship it?"` → `Question`.
  - `permission_waits_and_post_tool_clears_it`: `NeedsPermission { tool: "Bash" }` → `Waiting`
    with why `permission: Bash`. Then `NeedsCleared` → `Working`, and no `Event` change for the
    clear.
  - `stop_failure_is_failed`: → `Failed`, why `rate limit`, activity row `<folder> ✗ failed`.
  - `an_older_event_only_adds_history`: after `TurnEnded` at `T0+5s`, a `NeedsCleared` at
    `T0+4s` → the session is unchanged and there's no activity row. A `TurnStarted` at `T0+4s`
    → only its `Event`.
  - `a_duplicate_changes_nothing`: the same envelope twice → the second gives no `Persist`.
  - `events_for_other_brains_and_moa_are_dropped`: `brain: Some("work")` on brain `default`;
    `slot: Some("moa")`; `slot: Some("not-a-uuid")` with an unknown conversation id → no
    `Persist`, and a `Log` for the first and third only.
  - `the_conversation_id_finds_a_session_without_a_slot`: `slot: None` with `conversation_id`
    = `C` → applied to `S`.
  - `an_event_bumps_the_projects_last_activity_once_a_day`: two events on the same day → one
    `Change::Project`.
  - `a_session_start_request_is_answered_after_the_commit`: `reply: Some(..)` → one `Reply`
    after the `Persist` (`check_order` passes), with `data == {"context": null}`.
  - `a_dropped_request_is_still_answered`: the moa slot with `reply` → a `Reply` with
    `{"context": null}`.
  
  And a proptest, `no_order_of_events_regresses_state`:
  - generate a sequence of envelopes with distinct `at`s, from `TurnStarted`, `TurnEnded`,
    `TurnFailed`, `NeedsPermission` and `NeedsAnswer`. `NeedsCleared` is left out because its
    effect depends on the state before it;
  - apply any permutation of them;
  - the final `activity` and `activity_why` equal what the newest event gives on its own, and
    `activity_at` equals the newest `at`.
- [ ] **Step 2: Run them to see them fail**: `cargo nextest run -p moa-core --test hooks`.
- [ ] **Step 3: Implement** `Input::Hook`, `hooks::apply`, the dispatch in `core.rs` `decide`,
  and `recent_hooks`.
- [ ] **Step 4: Run them to see them pass**, then the whole `moa-core` crate.
- [ ] **Step 5: Commit**: `v1: hook events in the core (activity, ordering, history)`.

## Task 5: lifecycle, rotation, quiet, pane exit

**Files:**
- Modify: `crates/moa-core/src/hooks.rs`
- Modify: `crates/moa-core/src/live.rs` (`DisplayState::Quiet`, `QUIET_AFTER`)
- Modify: `crates/moa-core/src/effect.rs` (`TimerKey::Quiet`)
- Modify: `crates/moa-core/src/tmux/link.rs` (pane exit → `hooks::pane_gone`)
- Modify: `crates/moa-core/src/tmux/status.rs` (quiet not counted, no window state)
- Create: `crates/moa-core/src/peers.rs` (`peers_note`)
- Modify: `crates/moa-core/tests/hooks.rs`

**Interfaces:**
- Produces: `DisplayState::Quiet`, declared right after `Working`, with `word()` = `"quiet"`
  and `glyph()` = `"◌"`. Also `pub const QUIET_AFTER: i64 = 600_000;` (ms).
  `display_state(s, now)` returns `(Quiet, "quiet <N>m")` for a live, here, `Working` session
  with `now - activity_at >= QUIET_AFTER`, where `N` = whole minutes.
- Produces: `TimerKey::Quiet`, plus
  `pub(crate) fn quiet_timer(state: &State, now: UtcMs) -> Effect`: `SetTimer` at the earliest
  future deadline, or `CancelTimer(TimerKey::Quiet)`. Emitted after `Startup`, `Reload`, every
  `Input::Hook` that changed a session, and `Timer(Quiet)`.
- Produces: `pub(crate) fn peers_note(state: &State, session: SessionId) -> Option<String>`.
  It returns `None`, with a doc comment that slice 7 fills it.
- Produces: `pub(crate) fn pane_gone(state: &mut State, tx: &mut Tx, pane: PaneId)`, called
  from the link's `HookMsg::PaneExited`/`PaneDied` handling, before its refresh.

**What this adds to `apply`:**
- **`SessionStarted`**, applied when newer per H5:
  - `lifecycle` = `Live`, `asleep_at` = `None`, `location` = `Here`, `activity` = `Idle`,
    `resume_failed` = `false`;
  - plus H9's rotation: a new `Change::Conversation` with `first_seen_at` = `last_seen_at` =
    `at` and `transcript_path` from the envelope. If the conversation already exists, it only
    moves its `last_seen_at`.
- **`SessionEnded`:**
  - reason `clear` or `resume` → the history row only;
  - otherwise, when newer, `lifecycle` = `Ended` and the activity row `<folder> ○ ended`.
- **Pane exit** (`pane_gone`):
  - the session whose binding is trusted (T11) for `pane` and is still `Live` → `Ended`,
    `Change::DeletePaneBinding`, and the activity row `<folder> ○ ended`;
  - an untrusted or unknown pane changes nothing.

- [ ] **Step 1: Write the failing tests** (added to `crates/moa-core/tests/hooks.rs`):
  - `start_brings_an_ended_session_back_and_clears_asleep`;
  - `clear_rotates_the_conversation_and_stays_live`:
    - `SessionEnded { reason: "clear" }`, then `SessionStarted { source: Clear }` with a new
      id;
    - result: two conversations, the new one with reason `Clear` and `store_dir` from the
      envelope; lifecycle `Live`.
  - `fork_and_compact_reasons`: `Fork` → `Fork`, `Compact` → `Clear`;
  - `exit_ends_the_session`: reason `prompt_input_exit` → `Ended`, activity row `○ ended`;
  - `a_late_session_end_does_not_end_a_restarted_session`: `SessionEnded` with `at` older than
    the newer `SessionStarted` → still `Live`;
  - `quiet_after_ten_minutes_of_silence`:
    - a `TurnStarted` at `T`;
    - `display_state` at `T + 9m59s` is `Working`;
    - at `T + 10m` it's `(Quiet, "quiet 10m")`;
    - the effects of the `TurnStarted` include `SetTimer { key: Quiet, at: T + 10m }`.
  - `the_quiet_timer_follows_the_earliest_working_session`: two working sessions → the earlier
    deadline. When both are idle → `CancelTimer(Quiet)`.
  - `quiet_is_not_counted_in_the_status_line`: in `StatusData`'s counts, the session is in
    none of working, waiting, question, failed or done;
  - `a_pane_exit_ends_its_bound_session`:
    - seed a current-generation binding to `%5` (as slice 3's `trusted_panes` tests do);
    - a `moa:hook pane-exited %5` mailbox notification → `Ended`, the binding deleted, an
      activity row;
    - an old-generation binding → nothing.
  - `session_start_reply_carries_the_peers_note`: `reply: Some` → `{"context": null}` (slice 7
    changes the value, not the plumbing).
  
  Update slice 3's status snapshot tests if adding `Quiet` changes them, and say so in the
  report.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`. Slice 3's
  status, link and focus tests must stay green.
- [ ] **Step 5: Commit**: `v1: session lifecycle, rotation, quiet, pane exit`.

## Task 6: hooks in the daemon, and the spool drain

**Files:**
- Modify: `crates/moa-daemon/src/session.rs` (route `Incoming::Hook`)
- Modify: `crates/moa-daemon/src/core_task/mod.rs` (`CoreHandle::hook`; the drain in `start`)
- Modify: `crates/moa-daemon/src/run.rs` (passes the spool path in `CoreOptions`)
- Create: `crates/moa-daemon/tests/hooks.rs`

**Interfaces:**
- Consumes: `normalize`, `last_assistant_text` (Task 1); `spool::take`, `Taken::done` (Task 2);
  `Input::Hook` (Task 4).
- Produces: `CoreHandle::hook(&self, reply: Option<(ConnId, ReqId)>, envelope: HookEnvelope)`.
- Produces: `CoreOptions.spool: Option<PathBuf>`, the brain's spool. It's `None` only in tests
  that opt out.

**Behaviour:**
- **`session.rs`, for each `Incoming::Hook { id, params }`:**
  - deserialize a `HookLine`. On failure:
    - with `id` → an `INVALID_PARAMS` error;
    - always → a debug log with no contents.
  - `normalize(&line, &last_assistant_text)`. With `None` and an `id` → the result
    `{"context": null}`.
  - otherwise `core.hook(id.map(|i| (conn, req_for(i))), envelope)`. The reply maps back to
    the request's `Id`, the way `Call::Op` replies do.
  - A hook needs no `hello` (as in `wire.rs` today), and a hook connection gets no caller.
- **The drain, in `CoreHandle::start`**, after `Input::Startup` and before `start` returns:
  - `spool::take`, then parse each line as a `HookLine` (bad lines are counted);
  - normalize, sort by `at_ns`, and send each as `Input::Hook { reply: None, .. }`;
  - wait until committed, then `Taken::done()`;
  - one info log: `drained N spooled hook events` (`+ M unreadable`), only when N + M > 0.
  - If the commit fails, `.draining` stays for the next start.

- [ ] **Step 1: Write the failing tests** in `crates/moa-daemon/tests/hooks.rs`, using
  `common::boot_with` with a seeded live session:
  - `a_hook_line_updates_the_session`:
    - send `{"jsonrpc":"2.0","method":"hook","params":<HookLine UserPromptSubmit>}` with the
      std `Client`, with no hello;
    - `session.ls` then shows it working (through the `display` field added below).
  - `session_start_is_answered_with_a_null_context`: with `id: 7` → `{"id":7,"result":{"context":null}}`.
  - `a_bad_hook_line_is_refused_only_when_it_has_an_id`.
  - `spooled_events_are_drained_in_order_at_start`:
    - write three `HookLine`s, out of `at` order, into `paths.spool(b)` before `boot`;
    - after boot the session reflects the newest one, `events` has all three, and neither
      the spool nor `.draining` holds a line.
  - `a_leftover_draining_file_is_drained_first`.
  - `a_drain_whose_commit_fails_keeps_the_draining_file`. Use the slice 2b way to fail a
    commit (the store's test hook); the implementer finds it in `core_task` tests.
  
  For these tests, `session.ls`'s JSON gains `"display": <DisplayState::word>` and
  `"why": <Why>` (`crates/moa-core/src/ops/sessions.rs`). This change is in this task, with a
  core test `ls_shows_the_display_state`.
- [ ] **Step 2: Run them to see them fail**: `cargo nextest run -p moa-daemon --test hooks`.
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: hooks reach the core; the spool is drained at start`.

**PR a2 ends here.** Verify, review and open the PR as for a1. The body's "try it" is the daemon
test, since nothing launches a session yet.

## Task 7: the injected plugin

**Files:**
- Create: `crates/moa-agents/src/claude/plugin.rs`
- Modify: `crates/moa-platform/src/paths.rs` (`session_plugin`)
- Create: `crates/moa-platform/src/fs.rs`. It moves `write_if_changed` here from
  `crates/moa-tmux/src/conf.rs`, and `moa-tmux` uses it from here.
- Modify: `crates/moa-daemon/src/run.rs` (write the plugin at start)
- Create: `crates/moa-agents/tests/plugin.rs` (+ insta snapshot)

**Interfaces:**
- Produces:
  `pub fn session_plugin(exe: &Path) -> Vec<(&'static str, String)>`. It returns the
  relative path and text of `.claude-plugin/plugin.json` and `hooks/hooks.json`.
- Produces: `Paths::session_plugin(&self) -> PathBuf` = `data_dir()/plugin/session`.
- Produces: `moa_platform::fs::write_if_changed(path: &Path, text: &str) -> io::Result<bool>`,
  which keeps its behaviour from slice 3: private, and written atomically.

**Exact files.**

`plugin.json`:
```json
{"name":"moa","version":"1","description":"moa tracks this session (generated; edits are replaced)"}
```

`hooks.json`:
- `E` is the exe path as a JSON string.
- `A` is `{"type":"command","command":E,"args":["hook","claude"],"async":true}`.
- `S` is `{"type":"command","command":E,"args":["hook","claude"],"timeout":5}`.

```json
{"hooks":{
 "SessionStart":[{"hooks":[S]}],
 "UserPromptSubmit":[{"hooks":[A]}],
 "Stop":[{"hooks":[A]}],
 "StopFailure":[{"hooks":[A]}],
 "PermissionRequest":[{"hooks":[A]}],
 "PreToolUse":[{"matcher":"AskUserQuestion|ExitPlanMode","hooks":[A]}],
 "PostToolUse":[{"hooks":[A]}],
 "PostToolUseFailure":[{"hooks":[A]}],
 "PermissionDenied":[{"hooks":[A]}],
 "Notification":[{"matcher":"elicitation_dialog","hooks":[A]}],
 "SessionEnd":[{"hooks":[A]}],
 "CwdChanged":[{"hooks":[A]}]
}}
```

Pretty-print it with `serde_json::to_string_pretty`, so the snapshot is readable.

- [ ] **Step 1: Write the failing tests:**
  - `the_plugin_is_stable`: an insta snapshot with exe `/opt/moa/bin/moa`;
  - `every_c10_event_is_subscribed`: the keys equal the C10 list;
  - `only_session_start_waits`: SessionStart has no `async`, and every other entry has
    `async: true`;
  - in `crates/moa-daemon/tests/hooks.rs`, `the_daemon_writes_the_plugin_once`:
    - boot, then check the files exist, with mode `0o600` and a directory mode of `0o700`;
    - boot again with the same exe → mtime unchanged.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
  - The daemon writes the plugin in `bring_up`, right after `tmux_link()`'s config writes,
    for every brain (sandboxed or not).
  - A write failure fails the start, as slice 3's config write does.
- [ ] **Step 4: Run them to see them pass.** Also `nix flake check`: `.json` snapshots are
  already in the source filter (`.snap`), so confirm nothing new needs adding.
- [ ] **Step 5: Commit**: `v1: the injected session plugin`.

## Task 8: `fake-claude`

**Files:**
- Create: `crates/moa-testkit/src/bin/fake-claude.rs`
- Modify: `crates/moa-testkit/Cargo.toml` (`[[bin]] name = "fake-claude"`)
- Modify: `crates/moa-testkit/src/lib.rs` (`fake_claude()`, `FakeStep`)
- Create: `crates/moa-testkit/tests/fake_claude.rs`

**Interfaces:**
- Produces: `pub fn fake_claude() -> PathBuf`. The path is
  `current_exe()?.parent()` (`deps`), then `..`, then `fake-claude`. A missing file panics with
  `fake-claude isn't built: run the tests with --workspace`.
- Produces: `pub enum FakeStep`, serialized one per line with `#[serde(tag = "do")]`:
  - `Hook { event: String, #[serde(flatten)] fields: serde_json::Map<String, Value> }`;
  - `Sleep { ms: u64 }`;
  - `Clear`;
  - `Exit`;
  - `Die`;
  - `Hang`.
  Also `pub fn write_script(dir: &Path, steps: &[FakeStep])`, which writes
  `<dir>/.fake-claude.jsonl`.

**What `fake-claude` does:**
- **Arguments:**
  - it accepts Claude's: `--session-id <id>`, `--resume <id>`, `--name <t>`, `--`, and a
    positional prompt;
  - its conversation id is the `--session-id` or `--resume` id, else a fixed
    `00000000-0000-4000-8000-000000000001`.
- **Script:** `FAKE_CLAUDE_SCRIPT`, else `./.fake-claude.jsonl`, else
  `[Hook SessionStart, Hang]`.
- **Hooks:**
  - read from `$CLAUDE_CODE_PLUGIN_DIRS` (the first entry), at `hooks/hooks.json`;
  - for a `Hook` step, take the matching entries:
    - the matcher regex is applied to `tool_name`, or to `notification_type` for
      `Notification`;
    - an alternation of plain names is enough, so it needs no regex crate.
  - each entry runs `command` with `args`. Its stdin is the payload:
    - the step's fields;
    - plus `hook_event_name`, `session_id`, `cwd` (the process cwd) and `transcript_path`
      (`<CLAUDE_CONFIG_DIR>/projects/x/<id>.jsonl`, not created).
  - the process environment passes through.
  - `async` entries are run and waited for in order (deterministic), and their stdout is
    ignored.
  - SessionStart's stdout is appended to `<cwd>/.fake-claude.context`, so tests can see the
    peers note plumbing.
- **The other steps:**
  - a positional prompt sends a `UserPromptSubmit` with it before the script;
  - `Clear` sends `SessionEnd {reason: "clear"}`, takes a new id
    (`…-000000000002`, `…003`, and so on), then sends `SessionStart {source: "clear"}`;
  - `Exit` sends `SessionEnd {reason: "prompt_input_exit"}` and exits 0;
  - `Die` exits 1 at once;
  - `Hang` sleeps forever;
  - at the end of the script, it hangs.

- [ ] **Step 1: Write the failing tests** in `crates/moa-testkit/tests/fake_claude.rs`. Each
  uses a plugin dir whose `hooks.json` points at a small recorder: a `/bin/sh` script that
  appends its stdin and `$1` to a file. Tests may launch anything.
  - `runs_matching_hooks_in_order_with_the_payload`;
  - `matchers_filter_by_tool_and_notification_type`;
  - `clear_rotates_the_session_id`;
  - `a_prompt_argument_is_the_first_prompt`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** it (std plus `serde_json`).
- [ ] **Step 4: Run them to see them pass**, and check that `nix build` still builds only `moa`:
  `nix build .#default` and `find result/bin` → `moa`, `moactl`.
- [ ] **Step 5: Commit**: `v1: fake-claude in the testkit`.

## Task 9: `open` (fresh)

**Files:**
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionOpen`, `"session.open"` in `METHODS`,
  `writes()`)
- Modify: `crates/moa-core/src/ops/sessions.rs` (`open`)
- Modify: `crates/moa-core/src/state.rs` (`Deps::new_uuid`)
- Modify: `crates/moa-core/src/effect.rs` (`TmuxToken::Open(SessionId)`)
- Modify: `crates/moa-core/src/tmux/link.rs` (`opens`, reply handling, failing them on `Down`)
- Modify: `crates/moa-core/src/tmux/cmd.rs` (allow `new-window`; rules below)
- Modify: `crates/moa-core/src/effect.rs` (`TmuxIn::Connected` gains `socket: String`)
  - and `crates/moa-daemon/src/tmux.rs`, which fills it from the `TmuxSocket`
- Modify: `crates/moa-proto/src/cli.rs` (`Verb::Open` → `Op::SessionOpen`)
- Modify: `crates/moa/src/ctl.rs` (create a missing folder; canonicalize)
- Modify: `crates/moa-store/src/lib.rs` + `Cargo.toml` (`new_uuid`, uuid `v7` feature)
- Modify: `crates/moa-daemon/src/run.rs` (`load_config` fills launchers, H16; passes
  `new_uuid` in `Deps`)
- Create: `crates/moa-core/tests/open.rs`

**Interfaces:**
- Produces: `Op::SessionOpen { path: PathBuf, account: Option<String>, title: Option<String>, group: Option<String>, prompt: Option<String>, bg: bool }`
  (method `session.open`, a write).
- Produces: `Deps::new_uuid: fn(UtcMs) -> uuid::Uuid`. `Deps`' test default gives sequential
  v7 ids.
- Produces: `TmuxToken::Open(SessionId)`.
- Produces: `moa_store::new_uuid(at: UtcMs) -> Uuid` (UUIDv7 at `at`, with random bits).

**`open`, in the core:**
1. **The link must be `Up`.** Otherwise: `moa isn't open in a terminal yet; run moa first`.
2. **Account:**
   - `account` (the flag) else the brain's `open_account`. With neither:
     `no account to open with: set open_account in moa.toml`;
   - unknown: `there's no account called <a>`;
   - kind ≠ `Claude`: `only Claude accounts can open sessions for now`;
   - no launcher: `can't find claude for <a>: set its launcher in moa.toml`.
3. **Route:** `route(config, Some(account), path, Some(this brain))`.
   - `Brain(b)` with b ≠ this brain: `that folder belongs to the <b> brain; open it there`;
   - `NotAvailable`: `<a> can't open sessions in that folder`.
4. **Ids:** `id = SessionId(new_uuid(now))`, `conv = new_uuid(now).to_string()`.
5. **Title:** `title` else the path's last component, else `agent`. The window name is its
   first 24 chars.
6. **One commit:**
   - `Change::Session`:
     - `lifecycle Live`, `activity Idle`, `activity_at = now`;
     - `cwd = start_cwd = path`, `account`, `project_id`;
     - `title`, `title_locked = title given`, `group_path = group`;
     - `location Here`, `created_at = last_seen_at = now`.
     `project_id` follows the longest project-path prefix of `path` (whole components).
     Reuse slice 1/2's rule if one exists; otherwise add `pub(crate) fn project_for(state, path)`
     to `ops/projects.rs`.
   - `Change::Conversation`: reason `Start`, `store_dir` = the account's store,
     `first_seen_at = last_seen_at = now`.
   - an activity row via `Tx::ui`: `opened <title> with <account>`.
7. **`Effect::Tmux`** with `token: Some(Open(id))`, one `Cmd`:
   ```
   new-window -d -P -F 'P #{pane_id} #{pane_pid}' -n <name> -c <path> -t =moa:
     -e CLAUDE_CONFIG_DIR=<store> -e MOA_SESSION=<id> -e MOA_BRAIN=<brain>
     -e CLAUDE_CODE_PLUGIN_DIRS=<session_plugin dir>
     <launcher> --session-id <conv> --name <title> [-- <prompt>]
   ```
   - The plugin dir reaches the core as `Deps::session_plugin: PathBuf`, which the daemon
     fills from `Paths::session_plugin()`.
   - `TERM` isn't touched.
8. **Holding the reply:** the client's reply waits in `TmuxState::opens` (`BTreeMap<SessionId,
   (ConnId, ReqId)>`).
   - **On `Replies { token: Open(id), results }`:**
     - `Ok(["P %N PID"])`:
       - a `Change::PaneBinding`, with `server_pid`/`server_start` from the generation,
         `socket` from `Connected.socket`, `pane_id`, `pane_pid` and `bound_at = now`;
       - then an untokened `set-option -p -t %N @moa_session <id>`;
       - then the reply `Done { text: "opened <title> with <account> in <path>", data: {"id": id, "title": title} }`.
     - `Err` or a line that doesn't parse:
       - the session becomes `Ended` with `resume_failed = true`;
       - the reply is `couldn't open a tab for it; see moa's log`;
       - the log has tmux's first error line (ids only).
   - **On `Down`:** every waiting open fails with
     `moa's terminal went away before it opened`, and its session becomes `Ended`.
9. **`Cmd` rules for `new-window`:**
   - it must have `-d` and `-P`;
   - every argument after the launcher is quoted by the existing rules;
   - a `-e` value may not contain a newline.

**`Verb::Open` and `ctl.rs`:**
- `to_op` resolves `path` against `VerbCtx` (`~`, relative).
- `ctl.rs`, for `open`:
  - creates the folder when it's missing and prints `created <path>` (spike parity);
  - canonicalizes, then sends.
- `bg` passes through.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/open.rs`:
  - **Setup:** `TypicalBrain` with `moa.toml` holding brain `default` (folder `<home>/work`,
    `open_account = "acct-a"`) and `acct-a` (`kind = "claude"`, store `<home>/.claude-a`,
    launcher `/opt/claude`). The link is brought `Up` with a `Connected` input, as slice 3's
    link tests do.
  - `open_sends_one_new_window_with_the_env`: the exact `Cmd` text, with ids from the
    sequential `new_uuid`.
  - `open_replies_after_the_pane_is_bound`: the reply comes only after
    `Replies { Open(id), ["P %7 4242"] }`, with the binding and the `@moa_session`
    `set-option` in the effects.
  - `open_refuses_without_a_terminal`, `…_without_an_account`, `…_an_unknown_account`,
    `…_without_a_launcher`, `…_another_brains_folder`: each exact message, and no `Persist`.
  - `a_failed_new_window_ends_the_session`.
  - `the_link_going_down_fails_waiting_opens`.
  - `the_window_name_is_cut_to_24_chars`.
  
  The launch counter is checked end to end (Task 10).
  
  In `crates/moa-proto` tests, `open_parses_to_session_open`. In `cmd.rs` tests,
  `new_window_needs_d_and_p`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: open a fresh session in a moa tab`.

## Task 10: end to end, and the try-it

**Files:**
- Create: `crates/moa/tests/hooks_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs`, for helpers: `moa_toml_with_fake_account(tb)` and
  `ls_display(tb, id)`

**The setup:**
- A `TempBrain` with `moa.toml` holding brain `default` (folder `<root>/work`,
  `open_account = "acct-a"`) and account `acct-a` (`kind = "claude"`, store
  `<root>/.claude-a`, launcher = `fake_claude()`).
- The front door's create command (slice 3's `TempBrain::tmux` path), then `moa --sandbox`'s
  `tmux.ensure` handshake, the same way `tmux_e2e.rs` does.
- Every wait goes through `wait_for` with a 10 s limit.

- [ ] **Step 1: Write the failing tests:**
  - `fake_agents_drive_every_display_state`:
    - `write_script` into `<root>/work/a` with:
      - `Hook UserPromptSubmit`; `Sleep 300`;
      - `Hook PermissionRequest {tool_name: "Bash"}`; `Sleep 300`;
      - `Hook PostToolUse`; `Sleep 300`;
      - `Hook Stop {last_assistant_message: "Done. Ship it?"}`; `Sleep 300`;
      - `Hook UserPromptSubmit`; `Hook StopFailure {error: "rate_limit"}`; `Sleep 300`;
      - `Hook UserPromptSubmit`; `Hook Stop {last_assistant_message: "All done."}`; `Sleep 300`;
      - `Exit`.
    - `moactl open <root>/work/a`;
    - then wait in turn for `display` = `working`, `waiting`, `working`, `question`, `failed`,
      `done`, `ended`.
    - Quiet, idle-after-seen and asleep are covered by Task 5's core tests and 4b, and are
      named in a comment.
  - `a_dying_agent_ends_its_session`: script `[Hook SessionStart, Die]` → `ended` (through the
    pane exit, H18).
  - `hooks_while_down_are_spooled_and_drained`:
    - open with script `[Hook SessionStart, Hang]`; wait until `idle`;
    - `moactl shutdown`;
    - run `moa hook claude` directly 5 times, with the pane's env (`MOA_SESSION`, `MOA_BRAIN`,
      `MOA_ROOT`, `CLAUDE_CONFIG_DIR`) and alternating `UserPromptSubmit`/`Stop` payloads;
      each exits 0 with empty output;
    - the spool has 5 lines;
    - the next `moactl ls` autostarts the daemon;
    - the session's `display` is `done`, the 5 events are in the DB (`moa sql`, as slice 2b's
      e2e reads rows), and the spool is empty.
  - `out_of_order_hooks_never_regress`:
    - start hook process A with a `Stop` payload, keeping its stdin open (its `at` is stamped
      now);
    - start B with `UserPromptSubmit`, feed it, and wait for `working`;
    - then feed A → it stays `working` (A is older), and its event row exists.
    - With the order swapped, it's `done`.
  - `session_start_context_reaches_the_agent`: `.fake-claude.context` exists and is empty
    (peers note `None`). Slice 7 changes this assertion.
  - `open_and_hooks_launch_nothing_in_the_daemon`: `status --json`'s launch count is the same
    before the open and after the script ends.
- [ ] **Step 2: Run them to see them fail** (or pass where earlier tasks already did the work;
  say which in the report).
- [ ] **Step 3: Fix what they find.** A failure here is a bug in an earlier task: fix it there,
  with its own test.
- [ ] **Step 4: Run** the full verify, three times, and the tmux-facing crates on tmux 3.4 (the
  slice 3 way).
- [ ] **Step 5: Commit**: `v1: hooks end to end with fake agents`.

**PR a3 ends here.** Verify, review and open the PR as before. Its body carries the try-it below.

## Try it (PR a3)

In your own terminal, from this checkout. This uses your real Claude account inside the
throwaway sandbox brain. Your real brains and the spike are untouched.

1. **Tell the sandbox about your account.** `CLAUDE_STORE` is your Claude settings folder (the
   one `CLAUDE_CONFIG_DIR` points at, else `$HOME/.claude`):
   ```
   mkdir -p $HOME/.local/state/moa-sandbox/.config/moa $HOME/tmp/moa-try
   printf '[brains.default]\nfolders = ["%s/tmp"]\nopen_account = "mine"\n\n[accounts.mine]\nkind = "claude"\nstore = "%s"\n' $HOME CLAUDE_STORE > $HOME/.local/state/moa-sandbox/.config/moa/moa.toml
   ```
2. **Open moa:** `nix run . -- --sandbox`.
3. **Open a session:**
   - in moa's shell, `cd` to this checkout;
   - run `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- open $HOME/tmp/moa-try`;
   - go to the new tab with `Ctrl-a ]`.
4. **Watch:** type a prompt, approve a tool, and press Esc mid-turn. In moa's first tab,
   `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- ls` shows working, needs you, done,
   and quiet after 10 minutes. The status line counts the same.

**What this checks for real** (spec §13): Claude loads hooks from `CLAUDE_CODE_PLUGIN_DIRS`,
runs exec-form `async` hooks, and accepts `--session-id` with `--name`.

**If no state ever changes:** the hooks didn't load. Tell me the Claude Code version
(`claude --version`), and paste the plugin's `hooks/hooks.json` from
`$HOME/.local/state/moa-sandbox/.local/share/moa/plugin/session/hooks/`.

Restart needed: nothing; the spike is untouched.
