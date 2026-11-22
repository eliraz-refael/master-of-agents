# moa v1 slice 6a: moa's conversation runs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon hosts moa: it starts moa's agent (Claude's ACP adapter) for the brain's
moa account, keeps one thread per local day, and writes everything moa says into `feed`,
whether or not anyone is watching. A prompt sent from the command line gets moa's answer, and
the answer is in `feed`.

**Architecture:**
- **One module speaks ACP:** `moa_agents::acp` (behind the crate feature `acp`) spawns the
  adapter through `Spawner`, speaks the pinned `agent-client-protocol` crate in the Client
  role, and turns everything into moa's own types, `moa_core::central::wire::{AcpIn, AcpCmd}`.
  No library type leaves that module.
- **The core decides, as everywhere else.** `moa_core::central` is pure: `Input::Acp(AcpIn)`
  in, `Effect::Acp(AcpCmd)` and `Persist` out. It chooses the thread (resume, load or new),
  writes each update to `feed`, ends turns, and refuses permission requests until 6c.
- **A daemon actor carries it:** `moa-daemon/src/acp.rs`, shaped like the tmux link
  (`tmux::start`, `supervise`), owns the one adapter process and its generation.
- **`stub-acp`** (testkit binary, same crate in the Agent role) stands in for the adapter in
  every test. The real adapter is packaged by the flake and is only ever run by hand.

```
 moa console prompt "…" ─► console.prompt ─► central: adapter off? Start ─► Started{caps}
        │                                     thread for today? Resume | Load | New ─► Opened
        │                                     Persist [Thread?, Feed you] ─► (after commit) Prompt
 adapter stdout ─► moa_agents::acp (always read) ─► Input::Acp(Update) ─► Feed rows ─► feed topic
                                                 └► Permission ─► reject_once (or cancel), system line
                                                 └► TurnEnded ─► reply with moa's words
```

**Tech Stack:** Rust; the existing crates. New: `agent-client-protocol = "=5.5.1"`
(`default-features = false`, no features), which brings `agent-client-protocol-schema
=1.21.2`; `tokio-util`'s `compat` feature. The adapter is nixpkgs' `claude-agent-acp`
**0.100.0** at the flake's pinned nixpkgs (`d5cc683`), with its wrapper removed (call A2).

**Spec:** `docs/superpowers/specs/2026-11-22-moa-slice-6-central-agent-design.md` ("6a" and
every "My calls" bullet naming 6a), which the user approved. It builds on the main spec
`docs/superpowers/specs/2026-11-15-moa-rust-design.md` §6.11 (hosting, threads, hidden
prompts), §6.2 (`moa-agents` holds the ACP host), §6.3 (`Input::Acp`, `Effect::Acp`), §6.4
(`console.prompt|cancel`, topic `feed`), §6.5 (`moa_threads`, `feed`, the agent dir) and §11.6.
The spec wins where they differ.

Roadmap: "Slice 6", items 1, 2 and 8. Research: `docs/superpowers/research/3-agents.md` §1,
§6.2–6.3.

**Builds on** 5d and before. Each item below is named so tasks reuse it and never copy it:
- **Store:** `crates/moa-store/src/sql/0001_v1.sql` already has `moa_threads` and `feed`
  (no schema change in 6a); `rows.rs` (`MOA_THREADS`, `FEED`, `thread()`, `write`'s upserts by
  `thread_id` and by `id`); `snapshot.rs` (`load_snapshot`, `Src::rows` with its `filter`,
  `next_ids` already counting `feed`); export's `NOT_EXPORTED` entry for `feed`.
- **Model:** `model::{MoaThread, FeedEntry, ThreadKind}`, `Change::{Thread, Feed}` (their
  topics are `[]` today), `state::{Snapshot.threads, NextIds.feed, Table::Feed, State::apply}`
  (activity's tail trim, `trim_activity`, is the template for the feed tail).
- **Core:** `core.rs` (`handle`, `decide`, `finish_op`, `post_commit`, `Tx::{commit, next_id}`,
  the `Input::Reload` and `Input::ClientGone` branches), `effect.rs` (`Effect::runs_at_once`,
  `is_heard`, `check_order`), `publish.rs` (`Topic`, `keyed`, `by_key`, `State::publish`,
  `ACTIVITY_SHOWN`), `ops::read::brief`, `ops::launch::{launch_env, CONFIG_DIR_MODE,
  launcher_of}`, `config::{BrainConfig.moa_account, AccountConfig, Config::brain, expand_one}`,
  `Deps` (`read_text`, `new_uuid`, `panel: PanelLaunch` for the daemon's exe and `MOA_ROOT`).
  `hooks::resolve` already drops every hook event whose `MOA_SESSION` is `moa`.
- **Daemon:** `core_task/mod.rs` (`Item`, `Held`, `start_inner` and its test `link` hook),
  `drain.rs` (`step`, `run_now`, `release`, and the hooks' `replay` after a failed commit),
  `recover.rs::fail_held`, `tmux.rs::start` with `supervise::supervise` (the actor's shape),
  `run.rs` (`Deps` built at `:258`, `panel_launch`, `daemon_exe`), `logging`.
- **Platform:** `Spawner` (counts every launch), `wait_exited`, `kill_term`, `Paths`
  (`brain_dir`, `config_dir`); `moa-tmux/src/client.rs`'s `after_wait`, `kill_unless_exited`
  and `first_stderr_line` (moved to `moa-platform`, Task 3, not copied).
- **Testkit:** `fake_claude()`'s lookup next to the test binaries, `FakeStep`/`write_script`
  (the script pattern), `fake_claude_named_claude`, `TempBrain`, `wait_for`.
- **CLI:** `crates/moa/src/main.rs` (`Command`), `ctl.rs` (`Here`, `running`, `finish`),
  `config::RESERVED_BRAIN_NAMES`, `moa_client::Client::{call, subscribe}`.
- **Tests:** `crates/moa-core/tests/common/mod.rs` (`loaded`, `loaded_with`, `send`, `send_on`,
  `changes`, `replies`, `at`); `moa-daemon/tests/flow.rs` `twenty_verbs_launch_nothing` and
  `idle.rs` `an_idle_brain_wakes_only_for_the_backup`, which must keep passing unchanged;
  `moa-proto`'s schema snapshot; `crates/moa/tests/common/mod.rs`.

## What the library check found (2026-11-22)

- **`agent-client-protocol` 5.5.1** (released 2026-11-21, MSRV 1.88, Apache-2.0) is the newest.
  It plays both roles: `Client.v1()…connect_with(transport, |conn| …)` for the host and
  `Agent.v1()…connect_to(transport)` for `stub-acp`. `ByteStreams::new(write, read)` takes any
  `futures` `AsyncWrite`/`AsyncRead`, so moa spawns the adapter itself (through `Spawner`) and
  needs neither the `process` nor the `stdio` feature.
- In schema 1.21.2, `session/resume` (`ResumeSessionRequest`, capability
  `sessionCapabilities.resume`), `session/load` (`agentCapabilities.loadSession`), the
  `systemPrompt` field on new/load/resume (`SystemPrompt { mode: Append, content }`,
  capability `sessionCapabilities.systemPrompt`) and `_meta` on every type are all **stable**,
  with no feature gate. (`unstable_session_system_prompt` existed up to 5.0.0 and was removed
  when it was stabilised.) `ContentChunk.message_id` is stable too. `PromptRequest` has no
  message id: the daemon's prompt id is its own (call A6).
- The dispatch loop runs handlers in arrival order and waits for each. A prompt's response
  registered with `prepare_request(…).on_receiving_result(…)` is ordered after the updates
  before it, and so is a load's after its replay.
- **So the fallback (`agent-client-protocol-schema` plus our own JSON-RPC loop) isn't taken.**
  Task 1 proves this again in code before anything is built on it.
- **The adapter:** nixpkgs `d5cc683` carries `claude-agent-acp` 0.100.0 (Apache-2.0). Its
  package wraps the binary with `--set-default CLAUDE_CODE_EXECUTABLE <nixpkgs claude-code>`.
  claude-code is unfree, so the stock package doesn't even evaluate under the flake's
  nixpkgs config, and it would pull a second `claude` into the closure. With
  `overrideAttrs (_: { postInstall = ""; })` it evaluates and needs no unfree package. No pin
  bump is needed.

## Calls for you to check

The most important come first.

1. **moa's agent starts on the first prompt, not when the daemon starts.** It then stays up.
   If it dies, it stays down until the next prompt, which starts it again and reopens today's
   thread. So nothing is spent and nothing restarts in a loop. 6c's watchdog adds the
   kill-and-respawn (A4).
2. **One prompt at a time.** A prompt sent while moa is answering, or still starting, is
   refused: `moa is still answering; wait for it, or cancel it`. Queuing belongs to 6c's one
   channel (A7).
3. **What goes into `feed`:** what you said (`you`), moa's words (`moa`), its thinking
   (`thought`), tool calls (`tool`, updated in place as they run), its plan (`plan`), and
   moa's own lines (`system`, `error`). Bookkeeping updates (token usage, modes, the slash
   command list, the title) are read and dropped. The adapter's echo of your prompt is never
   stored, since the daemon wrote your prompt itself (A5).
4. **Reopening today's thread:** `session/resume` when offered, else `session/load`, where
   replayed updates already in the feed (same message id or tool-call id) are skipped, and
   replayed updates with no id are skipped too. Neither resends the system prompt (A8).
5. **The command line:** `moa console prompt TEXT…` waits for the turn and prints moa's
   answer; `moa console cancel`; `moa console feed [-n N]`. These are `moa` subcommands, not
   `moactl` verbs, so moa can't prompt itself through its allow-list. `console` becomes a
   reserved brain name (A10).
6. **Which adapter runs:** the one the Nix package was built with. An account's
   `acp = "<path>"` in `moa.toml` overrides it. That's how tests run `stub-acp`, and how the
   dev-shell try-it runs the real one. A plain `cargo` build knows no adapter, and says so
   (A2).
7. **The constitution** is embedded (adapted from the spike's master `CLAUDE.md`, at most 8 KB),
   and `~/.config/moa/constitution.md` replaces it when present. The scope file is
   `~/.config/moa/<brain>.md`. Without the `systemPrompt` capability, the constitution and the
   scope go into the agent dir's `CLAUDE.md`, and the brief rides on the first prompt (A9).
8. **The adapter's environment** is the daemon's, minus Claude's and moa's own variables, plus
   the spec's (`CLAUDE_CONFIG_DIR` by the sessions' rule, `CLAUDE_CODE_EXECUTABLE` = the
   account's launcher, `CLAUDE_AGENT_ACP_PROTOCOL=v1`, `MOA_SESSION=moa`, `MOA_BRAIN`), with
   `<agent dir>/bin` (holding a `moactl` link) first on `PATH` (A3).
9. **The adapter runs in its own process group,** and the whole group is stopped with it, so
   no `claude` it started outlives it (A4).
10. **The adapter's stderr goes to the log line by line,** cut to 500 characters (the spec only says
    "captured to the log"; the cap is this plan's). It may hold content, unlike every other log line (A4).
11. **While the brain refuses writes,** moa's updates are still read (the turn finishes), but
    not saved; the log counts them. After a single failed commit they're applied again, like
    hooks (A5).

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged, and gets:
- `scripts/verify`: fmt, clippy (also for linux), the IO-crate checks, nextest three times, the
  tmux 3.4 pass on the tmux-facing crates, and `nix flake check`;
- per-task reviews, then `/code-review medium --fix` over the PR's whole branch, with the
  fixes verified. That is the PR's whole-branch review: it replaces a separate final-review
  pass;
- green CI.

Bodies end with the try-it, when there is one. No "Restart needed" line.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-6a-1` | 1–4 | `v1 slice 6a (1): the ACP library, stub-acp, the host, the adapter` |
| p2 | `v1/slice-6a-2` | 5–8 | `v1 slice 6a (2): moa's threads and turns` |
| p3 | `v1/slice-6a-3` | 9–11 | `v1 slice 6a (3): the daemon hosts moa, end to end` |

p1 also carries this plan and the design doc.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| A1 | **The library:** workspace `agent-client-protocol = { version = "=5.5.1", default-features = false }`. `moa-agents` gets a feature `acp` (tokio, tokio-util `compat`, the library) that only `moa-daemon` and `moa-testkit` turn on, so `moa-store`'s and `moa-core`'s dev-dependency on `moa-agents` stays std-only. Every `agent_client_protocol::` path lives under `crates/moa-agents/src/acp/` or `crates/moa-testkit/src/bin/stub-acp.rs` (tests may use it too). | The spec's "one module to fix" (R3 §1.2). |
| A2 | **The adapter's path:** `moa_agents::acp::BUILT_IN_ADAPTER: Option<&str> = option_env!("MOA_ACP_ADAPTER")`, set only by the flake's `packages.default` build to `${claude-agent-acp}/bin/claude-agent-acp`, where `claude-agent-acp = pkgs.claude-agent-acp.overrideAttrs (_: { postInstall = ""; })`, also exposed as `packages.<system>.claude-agent-acp` and put in the dev shell's `packages`. The dev shell never exports `MOA_ACP_ADAPTER`, so test builds know no adapter. `[accounts.<a>] acp = "<path>"` (expanded like `launcher`) overrides it per account. The daemon gives it to the core as `Deps.acp_adapter: Option<PathBuf>`. | Pinned by `flake.lock`, never the `PATH` (design). Tests can't reach the real adapter even by mistake. |
| A3 | **The launch spec,** `central::launch::spec(state) -> Result<(AccountName, AdapterSpec), OpError>`: program = the account's `acp`, else `Deps.acp_adapter`; cwd = `Deps.agent_dir` (`<brain dir>/agent`); env set: `CLAUDE_CODE_EXECUTABLE=<launcher>` (`launcher_of`'s rule and text), `CLAUDE_AGENT_ACP_PROTOCOL=v1`, `MOA_SESSION=moa`, `MOA_BRAIN=<brain>`, `CLAUDE_CONFIG_DIR=<store>` unless the store is Claude's default (the rule `launch_env` applies, factored into `ops::launch::store_var(store, home, mode, canonical) -> Option<String>` that both call), and `PanelLaunch.env` (`MOA_ROOT` when sandboxed); env unset: `CLAUDE_CONFIG_DIR` (default store only), `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `MOA_TITLE`, `MOA_GROUP`. The host puts `<cwd>/bin` first on the inherited `PATH`, links `<cwd>/bin/moactl` to `PanelLaunch.exe`, and writes `<cwd>/.claude/settings.json` from `spec.settings`. The allow-list: `Bash(moactl:*)`, `Bash(git status:*)`, `Bash(git log:*)`, `Bash(git diff:*)`, `Bash(git show:*)`, `Bash(git branch:*)`, `Bash(ls:*)`, and `Read(//<folder>/**)` for each of the brain's folders (absolute, leading `/` dropped after `//`). Refusals, exact: `moa has no account yet: set moa_account for the <brain> brain in moa.toml`; `launcher_of`'s text; `moa's agent isn't part of this build: set acp for <account> in moa.toml`. | Spec §6.11's env and allow-list. The sessions' store rule is reused so moa logs in where the account's sessions do. |
| A4 | **The host** (`moa_agents::acp::Adapter`): spawned through `Spawner` with `process_group(0)`; stdin/stdout through `tokio::process::{ChildStdin, ChildStdout}::from_std` and `tokio_util::compat` into `ByteStreams`; stderr read line by line to `tracing::info!("adapter: {line}")`, each cut to 500 chars. Client capabilities: `fs` read/write false, `terminal` false; client info name `moa`. **Notification and request handlers only send to the core** (an unbounded channel): nothing in them awaits the core, so the adapter is always read (#1269). Each prompt is sent with `prepare_request(…).on_receiving_result(…)`, so `TurnEnded` reaches the core after every update before it; the same for `session/load`. A permission request's responder is kept by a request number until `AcpCmd::Permission` answers it. **Exit:** one `AcpIn::Exited` per generation, from the reaper or the connection's end, whichever comes first. The reaper (moved from `moa-tmux`, see Task 3) sees the exit without reaping, sends `SIGTERM` to the process group, then reaps, so the group id can't have been reused. **Stop:** `SIGTERM` to the group while the leader isn't reaped, wait up to 2 s, then `SIGKILL` to the group. Lazy start, no automatic restart (call 1). | Nothing polls; a dead adapter costs nothing until it's wanted. |
| A5 | **Feed rows** (`central::feed`). Kinds: `you`, `moa`, `thought`, `tool`, `plan`, `system`, `error`. Bodies are JSON: `{"text": …}` for text kinds; `tool` is `{"title", "kind", "input"}` with `state` = its status (`pending`, `in_progress`, `completed`, `failed`, or `cancelled` when its turn was cancelled) and `input` its `rawInput` serialised and cut to 2,000 chars; `plan` is `{"entries": [{"content", "status", "priority"}]}`. `message_id` is the dedupe key: the daemon's UUIDv7 for `you`; the adapter's message id for `moa`/`thought` (or `NULL`); `tool:<toolCallId>` for `tool`. **Merging:** chunks with a message id extend that id's row; chunks without one extend the turn's last row if it's the same kind and has no id, else start a row. A tool call update finds its row by id; the plan is one row per turn, updated in place. Non-text content blocks become `[image]`, `[audio]`, `[resource]`. `user_message_chunk` and the bookkeeping kinds are dropped (debug log, no content). Every write is a `Change::Feed` upsert through `Tx::commit`, so the drain's group commit turns a burst of chunks into one transaction. When the commit is refused, the update still drives the turn and a counter is logged once per turn: `N of moa's updates couldn't be saved`. A failed commit's `Input::Acp`s are stepped again after the reload, like hooks' `replay`. | Spec: every update is read and persisted; the console reads `feed`, never the pipe. |
| A6 | **Prompts:** `Op::ConsolePrompt { text: String, #[serde(default)] wait: bool }` (`console.prompt`, writes) and `Op::ConsoleCancel(NoParams)` (`console.cancel`). Text is trimmed; empty → `there's nothing to send`; over 100,000 characters → `a prompt is at most 100000 characters`. The `you` row is committed first (with `moa_threads`' row when the thread is new), and `AcpCmd::Prompt` is held behind that commit: `Effect::Acp(AcpCmd::Prompt{..})` is the one ACP effect that doesn't run at once, and `is_heard` counts it, so `check_order` keeps it after its `Persist`. Without `wait` the reply is `moa has it` once the prompt is accepted; with `wait` it comes at the turn's end: text = the turn's `moa` rows joined with blank lines, or `moa's turn ended early: <reason>` for any stop reason but `end_turn` (`cancelled`, `max_tokens`, `max_turn_requests`, `refusal`); data `{thread, message_id, stop}`. A gone client's waiter is dropped; the turn goes on. `central::send(state, now, text, automatic: bool, waiter) -> Vec<Effect>` is the one way in, so 6c and 6d mark automatic prompts by the id it records. | Spec §6.4's `console.prompt|cancel`; §6.11 "marked automatic by the message id the daemon sent". |
| A7 | **Phases:** `central::Moa { phase: Phase, gen: u64, caps: Option<Caps>, thread: Option<Open>, pending: Option<Pending>, turn: Option<Turn>, replaying: bool }`, `Phase = Off \| Starting \| Ready \| Thinking \| Failed(String)`. A prompt when `Off`/`Failed` sends `AcpCmd::Start` with `gen + 1` and parks the prompt as `pending`; any other prompt while `Starting`, `pending` or `Thinking` is refused (call 2). Every `AcpIn` of another generation is dropped. `console.cancel` while `Thinking` sends `AcpCmd::Cancel` and replies `cancelling moa's turn`; otherwise `moa isn't answering anything`. | The core holds all the policy; the host holds none. |
| A8 | **Threads,** `central::threads::choose(state, account, today, caps) -> How` (`Use(id)`, `Resume(id)`, `Load(id)`, `New`): the open thread when it's this account's and today's; else today's newest `moa_threads` row for this account → `Resume` if `caps.resume`, else `Load` if `caps.load`, else `New`; else `New`. `New` sends `systemPrompt {mode: append}` = `central::prompt::system_text` when `caps.system_prompt`; resume and load send none. `Opened(New)` commits `MoaThread { thread_id: <the adapter's session id>, account, day: today, kind: Daily, started_at: now }`. A failed resume or load opens `New` instead, and its first row is the system line `today's earlier thread couldn't be reopened, so this is a new one`. A failed `New` refuses the pending prompt with `moa couldn't start a thread: <first line>` and raises a `CoreEvent::Error` (there's no thread to write a line in). **Load's dedupe:** while `replaying`, an update is written only when its key (A5) isn't in `State.feed_keys` for that thread. A changed day or account is noticed on the next prompt. | Spec §6.11 threads; one per local day per account; nothing happens at midnight. |
| A9 | **The system text,** `central::prompt::system_text(state, now) -> SystemText { full, claude_md, brief }`: `claude_md` = the constitution (`Deps.constitution_file` when `read_text` finds it, else the embedded `central/constitution.md`) + `\n\n# This brain: <name>\n\n` + the scope file (`Deps.scope_file`, `~/.config/moa/<brain>.md`; `(no scope file)` when missing); `brief` = `ops::read::brief`'s text; `full` = `claude_md` + `\n\n` + `brief`. `ThreadHow::New { system_prompt, claude_md }`: with the capability, `(Some(full), None)` and the host removes any `CLAUDE.md`; without it, `(None, Some(claude_md))` and the thread's first prompt is sent as `brief + "\n\n---\n\n" + text` (the `you` row keeps only your text). | Spec §6.11's fallback, with the scope placed next to the constitution, where it's read every turn. |
| A10 | **Read and subscribe:** `Topic::Feed` (`feed`) carries the newest `FEED_SHOWN = 100` rows of every thread, keyed `#<id>` (`publish::FeedRow { id, thread, at, kind, message_id, automatic, body: Value, state }`), plus one row keyed `state`: `publish::MoaRow { phase: "off"\|"starting"\|"ready"\|"thinking"\|"failed", thread: Option<String>, why: Option<String> }`. `Change::Feed` and `Change::Thread` publish `Topic::Feed`. `Snapshot` gains `feed_tail` (those 100 rows) and `feed_keys: Vec<(String, String)>` (thread id, `message_id`) for each account's newest thread. Paging older rows is 6b's. The CLI reads the topic with `Client::subscribe`, so no read op is added. | Spec §6.4 lists `feed` as a topic. A `state` row beside the rows is the `layouts` topic's `""` row's precedent. |
| A11 | **The daemon side:** `Input::Acp(AcpIn)`, `Effect::Acp(AcpCmd)` (spec §6.3; the adapter's launch is `AcpCmd::Start` rather than a separate `Effect::Spawn`, since only the ACP actor ever starts it). `crates/moa-daemon/src/acp.rs`: `start(to_core, failed) -> UnboundedSender<AcpMsg>` with `AcpMsg = Cmd(AcpCmd) \| Stop(oneshot::Sender<()>) \| Crash` (tests), under `supervise`; a panic reports `Exited` for the live generation. `Held::Acp(AcpCmd)` is released after the commit and dropped by `fail_held`. The core task's stop stops the adapter after the tmux link (≤ 3 s). `Input::Reload` runs `central::reload`: a pending or in-turn prompt whose `you` row isn't in the reloaded tail is forgotten and its waiter told `moa couldn't save your prompt; nothing was sent`; an open thread whose row is missing is saved again. | Same shape as the tmux link, so supervision, ordering and reload work the way they already do. |

## Global Constraints

The roadmap's, every earlier slice plan's, and 5a–5d's Global Constraints apply. Also:

- **Reuse before writing.** Before adding a helper, type or function, search the workspace for
  one that does the job (`grep`, the crate's `lib.rs` exports, and the "Builds on" list), then
  reuse or extend it. A near-copy of existing logic is an **Important** review finding. Each
  report lists what it reused or extended.
- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`;
  `scripts/verify quick` before every commit, full `scripts/verify` per PR.
- **Never run cargo while an e2e test runs.** Run a test binary directly, or use another
  `CARGO_TARGET_DIR`.
- **Pinned exactly:** `agent-client-protocol = "=5.5.1"`, and `Cargo.lock` committed with it.
  No library type outside `moa_agents::acp` and `stub-acp` (tests excepted).
- **No polling** (spec U5): no timer, sleep loop or periodic check is added. The host reads the
  adapter's output as it arrives; turns end on the adapter's answer; a day change is noticed on
  the next prompt. 6a adds no `TimerKey`.
- **The adapter is always read** (#1269): no ACP handler awaits anything but an unbounded send.
- **Every product launch goes through `Spawner`.** The adapter is the only new one: at most one
  per moa account, started only by a prompt.
- **Tests:**
  - only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too;
  - never a tmux server named `moa` or `moa-work`, and never a real brain;
  - **never the real `claude` and never the real adapter:** every test with a `moa_account`
    sets that account's `acp` to `stub-acp` and its `launcher` to `fake-claude`;
    `BUILT_IN_ADAPTER` is `None` in every test build (Task 4 asserts it);
  - every wait goes through `wait_for` with a 10 s limit, never a sleep; `stub-acp` waits on
    files (`Wait`), never on time;
  - load workers in any hunt carry their own `timeout`.
- **`spike/` is frozen.** The constitution is adapted from `spike/master-chat/master/CLAUDE.md`
  by copying text out; nothing under `spike/` changes.
- **v1 never goes on `PATH`.** The flake's `packages.default` still builds only `--locked -p
  moa` (`moa` and `moactl`); `stub-acp` and `fake-claude` never land in it.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`,
  `~/private`. `stub-acp`'s scripts and transcripts are made up. The embedded constitution
  names no person, host, employer or account.
- **Log lines never hold record contents** (prompts, answers, titles, paths), except the
  adapter's stderr lines (A4).
- **Plain words** in everything the user sees: "moa", "moa's agent", "thread", never "ACP",
  "adapter", "session/update" or "JSON-RPC".

## Review Focus

1. **The adapter outruns the core.** A 5,000-chunk answer; a commit that fails mid-turn;
   writes refused for a whole turn. Expected:
   - reading never stops, and the adapter never blocks on its writes;
   - the turn ends and answers;
   - a failed commit's updates are applied after the reload, and no row is doubled;
   - during a refusal nothing is saved, and the log says how many updates weren't.

   Pinned by `a_burst_never_blocks_the_adapter` (Task 3), `refused_writes_still_end_the_turn`
   (Task 8), and `a_failed_commit_replays_moas_updates` (Task 9).
2. **Restarts around a thread.** The daemon restarts the same day; the adapter offers no resume;
   the local day changes; `moa_account` changes; the adapter dies mid-turn, then you prompt
   again. Expected:
   - same day: one `moa_threads` row, `session/resume`, no replayed rows;
   - load: no row is in the feed twice;
   - new day or account: a new thread, with the system prompt;
   - a crash: one `error` line, the waiter told, and the next prompt resumes today's thread.

   Pinned by `a_restart_the_same_day_resumes`, `without_resume_it_loads`,
   `a_new_day_starts_a_thread_with_the_brief`, `a_new_account_starts_a_new_thread` (Task 7),
   `load_never_duplicates_rows` (Task 8), and
   `a_crash_mid_turn_says_so_and_the_next_prompt_resumes` (Task 9).
3. **Whoever asked goes away.** A `--wait` client is killed mid-turn; the daemon stops
   mid-turn. Expected:
   - the turn goes on and lands in `feed`;
   - a daemon stop leaves no adapter and no process it started.

   Pinned by `a_client_gone_mid_turn_leaves_the_turn` (Task 8) and
   `stopping_the_daemon_ends_the_adapter_and_its_children` (Task 9, with `stub-acp`'s `Spawn`
   child).
4. **Permission requests before 6c.** Options in any order; no `reject_once` among them; two
   requests in one turn. Expected:
   - each is answered at once, by `kind`, never by position;
   - with no `reject_once` the turn is cancelled;
   - one `system` line each.

   Pinned by `a_permission_is_refused_by_kind` and `no_reject_once_cancels_the_turn` (Task 8),
   and `a_permission_is_refused_and_said` (Task 11).
5. **Odd text.** Prompts and answers with emoji, CJK, ZWJ sequences, RTL text, quotes and
   newlines, 100,000 characters; invalid UTF-8 from the adapter; image content; a 1 MB tool
   input. Expected:
   - exact round trips into `feed` and back to the CLI;
   - `[image]` for non-text blocks, the input cut to 2,000 chars;
   - no panic.

   Pinned by `odd_text_round_trips` and `a_huge_tool_input_is_cut` (Task 8), and
   `moa_answers_from_the_command_line` (Task 11).

---

## Task 1: the library check

**Files:** root `Cargo.toml` (workspace dependency), `crates/moa-agents/Cargo.toml` (feature
`acp`), `Cargo.lock`; new test `crates/moa-agents/tests/acp_check.rs`
(`required-features = ["acp"]`).

**Interfaces:**
- Produces: the pinned dependency and the `acp` feature (`tokio` with `rt`, `macros`,
  `io-util`, `process`, `sync`; `tokio-util` with `compat`; `agent-client-protocol`).

- [ ] **Step 1: Write the check** (in-process Client and Agent over `tokio::io::duplex`, each
  side wrapped in `ByteStreams` through `compat`):
  - `client_and_agent_agree_on_capabilities`: the agent advertises `loadSession`,
    `sessionCapabilities.resume` and `sessionCapabilities.systemPrompt`; the client reads all
    three. `session/new` with `systemPrompt {mode: append, content: [text "C"]}` and `_meta
    {"moa": 1}` reaches the agent with both, exactly.
  - `a_turns_updates_arrive_before_its_end`: the agent sends three `agent_message_chunk`s
    with `messageId "m1"`, a `tool_call`, then a `session/request_permission` with options
    `[allow_always, reject_once, allow_once]`; the client answers by kind `reject_once`; the
    agent sees that option's id; the prompt ends `end_turn`. The client's record is chunks,
    tool call, permission, then the end, in that order (`prepare_request` +
    `on_receiving_result`).
  - `resume_and_load`: `session/resume` answers with no update; `session/load` sends two
    replayed updates, then answers, and the client sees both before the answer.
  - `cancel_ends_the_turn_cancelled`: `session/cancel` during a prompt → `stopReason
    cancelled`.
  - `invalid_utf8_is_replaced_not_fatal`: a raw line holding a bad byte inside a string
    arrives with U+FFFD, and the connection lives.
- [ ] **Step 2: Run it.** `nix develop --command cargo nextest run -p moa-agents --features acp
  --test acp_check`. Expected: PASS on 5.5.1. **If any case can't be written or fails against
  the crate, stop and report BLOCKED** with what's missing: the design's fallback
  (`agent-client-protocol-schema` plus our own loop) is then the controller's decision.
- [ ] **Step 3:** `nix develop --command cargo tree -p moa-agents --features acp -i
  agent-client-protocol` shows `5.5.1` once; `cargo tree -p moa-store -e normal` shows no
  `agent-client-protocol`.
- [ ] **Step 4: Commit.** `v1: pin the ACP library, and prove it plays both roles`.

## Task 2: `stub-acp`

**Files:** new `crates/moa-testkit/src/bin/stub-acp.rs` and `src/stub.rs`;
`crates/moa-testkit/src/lib.rs` (`test_bin`, `StubStep`, helpers), `Cargo.toml` (the bin,
`moa-agents` with `acp`, the library, tokio); new test `crates/moa-testkit/tests/stub_acp.rs`.

**Interfaces:**
- Produces:
  - `pub fn test_bin(name: &str) -> PathBuf` (the `fake_claude()` lookup, generalised;
    `fake_claude()` calls it) and `pub fn stub_acp() -> PathBuf`;
  - `#[serde(tag = "do")] pub enum StubStep`: `Caps { load: bool, resume: bool,
    system_prompt: bool }` (first line only; all true when absent), `Say { text: String, id:
    Option<String> }`, `Think { text: String }`, `Tool { id: String, title: String, input:
    Option<Value> }`, `ToolDone { id: String, status: String }`, `Ask { title: String,
    options: Vec<(String, String)> }` (option id, kind), `Burst { n: u32 }`, `Stderr { line:
    String }`, `Spawn`, `Wait { file: String }`, `End { stop: Option<String> }` (default
    `end_turn`), `Hang`, `HangHard`, `Die`;
  - `pub fn write_stub_script(dir: &Path, steps: &[StubStep])` (`<dir>/.stub-acp.jsonl`) and
    `pub fn stub_log(dir: &Path) -> Vec<Value>` (`<dir>/.stub-acp.log`).
- Behaviour: the script is read from the stub's cwd (moa's agent dir). The steps up to each
  `End`, `Hang`, `HangHard` or `Die` are one prompt's turn; the k-th prompt plays the k-th
  turn, counted in `<cwd>/.stub-acp/turns` so a restarted stub goes on. Session ids are
  `stub-0001`, … (counter file). What a session sent is kept in `<cwd>/.stub-acp/<id>.jsonl`:
  `session/load` replays it then answers; `session/resume` of a known id answers with no
  update, of an unknown one with an error. `Spawn` starts `/bin/sleep 1000` and writes its pid
  to `<cwd>/.stub-acp.child`. Every request and notification received is logged as `{"method",
  "params"}`; `initialize`'s line adds `"env"` with `MOA_SESSION`, `MOA_BRAIN`,
  `CLAUDE_CONFIG_DIR` (or `null`), `CLAUDE_CODE_EXECUTABLE`, `CLAUDE_AGENT_ACP_PROTOCOL` and
  `PATH`, and `"cwd"`; each permission outcome is logged as `{"method":
  "permission.outcome", "params": …}`.

- [ ] **Step 1: Failing tests** (`stub_acp.rs`, driving the stub as a child process with the
  library's Client role):
  - `a_scripted_turn_plays_in_order` (`Say`, `Think`, `Tool`, `ToolDone`, `End`);
  - `a_permission_outcome_is_logged`;
  - `hang_ends_on_cancel_and_hang_hard_does_not`;
  - `die_exits_mid_turn`;
  - `a_second_stub_resumes_and_loads_the_first_ones_session`;
  - `the_log_records_env_cwd_and_system_prompt`;
  - `the_second_prompt_plays_the_second_turn`.
- [ ] **Step 2: Run them.** Expected: FAIL (no binary).
- [ ] **Step 3: Implement** on the library's Agent role over tokio stdin/stdout (`compat`),
  following `fake-claude`'s shape (script from cwd, everything written only inside cwd).
- [ ] **Step 4: Run them again,** and the workspace (`fake_claude()` callers unchanged).
  Expected: PASS. `nix build .#default` still lists only `moa` and `moactl` in `bin/`.
- [ ] **Step 5: Commit.** `v1: stub-acp, a scripted stand-in for moa's agent`.

## Task 3: moa's wire types, and the host

**Files:** new `crates/moa-core/src/central/{mod.rs,wire.rs}` (`pub mod central;` in
`lib.rs`); new `crates/moa-agents/src/acp/{mod.rs,convert.rs}` (`#[cfg(feature = "acp")] pub
mod acp;`); new `crates/moa-platform/src/child.rs` (moved from `moa-tmux/src/client.rs`;
`moa-platform`'s tokio gains `sync`, `process`, `io-util` and `rt`), and `sys`
(`kill_group`); `crates/moa-tmux/src/client.rs` (uses the moved helpers); new test
`crates/moa-agents/tests/acp_host.rs`.

**Interfaces:**
- Produces, in `moa_core::central::wire` (plain types, no IO):
  - `Caps { load: bool, resume: bool, system_prompt: bool }`;
  - `AdapterSpec { program: PathBuf, env: Vec<(String, String)>, unset: Vec<String>, cwd:
    PathBuf, settings: String, moactl: PathBuf }`;
  - `ThreadHow::{New { system_prompt: Option<String>, claude_md: Option<String> }, Resume {
    thread: String }, Load { thread: String }}` and `Opened::{New, Resumed, Loaded}`;
  - `AcpCmd::{Start { gen, spec }, Open { gen, how, cwd }, Prompt { gen, thread, prompt: u64,
    text }, Cancel { gen, thread }, Permission { gen, req: u64, choice: Option<String> },
    Stop { gen }}` (every `gen: u64`);
  - `AcpIn::{Started { gen, caps }, Opened { gen, result: Result<(String, Opened), String> },
    Update { gen, thread: String, update: Update }, Permission { gen, req: u64, thread:
    String, title: String, options: Vec<PermOption> }, TurnEnded { gen, prompt: u64, result:
    Result<StopReason, String> }, Exited { gen, why: String }}`;
  - `Update::{Message { id: Option<String>, text: String }, Thought { id: Option<String>,
    text: String }, Tool { id: String, title: String, kind: String, status: String, input:
    Option<String> }, ToolUpdate { id: String, title: Option<String>, status: Option<String>
    }, Plan { entries: Vec<PlanEntry> }, UserEcho, Other(&'static str)}`;
  - `PermOption { id: String, kind: PermKind }`, `PermKind::{AllowOnce, AllowAlways,
    RejectOnce, RejectAlways, Other}`, `StopReason::{EndTurn, MaxTokens, MaxTurnRequests,
    Refusal, Cancelled, Other(String)}`, `PlanEntry { content, status, priority }`.
- Produces, in `moa_agents::acp`: `pub const BUILT_IN_ADAPTER: Option<&str>`;
  `Adapter::start(gen: u64, spec: &AdapterSpec, to_core: Arc<dyn Fn(AcpIn) + Send + Sync>)
  -> Result<Adapter, String>` (writes the agent dir's files, spawns, runs `initialize`, sends
  `Started`); `Adapter::{open(&self, how: ThreadHow, cwd: &Path), prompt(&self, thread: &str,
  prompt: u64, text: &str), cancel(&self, thread: &str), answer(&self, req: u64, choice:
  Option<String>)}` (each answers through `to_core`); `async fn stop(self)`.
- Produces, in `moa_platform::child`: `Reaper` (`watch`-backed exit flag), `after_wait`,
  `kill_unless_exited`, `first_stderr_line`, moved as they are; plus `sys::kill_group(pgid:
  u32, signal: libc::c_int)`.

- [ ] **Step 1: Failing tests** (`acp_host.rs`, against `stub_acp()` in a tempdir cwd):
  - `the_adapter_starts_with_the_specs_env_and_files`: the stub's log shows the spec's env,
    `PATH` starting with `<cwd>/bin`, `cwd`; `<cwd>/bin/moactl` links to `spec.moactl`;
    `<cwd>/.claude/settings.json` equals `spec.settings`; `Started` carries the stub's caps;
    `moa_platform::spawn::launches()` grew by exactly 1.
  - `open_new_sends_the_system_prompt_or_writes_claude_md`: both `ThreadHow::New` forms;
    `CLAUDE.md` is written for one and removed for the other.
  - `updates_reach_the_core_in_order_before_the_turn_ends`.
  - `a_burst_never_blocks_the_adapter`: `Burst { n: 5000 }` while the test holds every
    `AcpIn` unread until the stub logs that it finished writing; then 5,000 updates, then
    `TurnEnded`.
  - `a_permission_waits_for_its_answer`: `Permission` reaches the core; `answer(req,
    Some(id))` reaches the stub's log.
  - `stderr_lines_reach_the_log_cut_to_500`.
  - `a_dead_adapter_says_so_once`: `Die` → exactly one `Exited`, and the prompt's
    `TurnEnded` carries `Err`.
  - `stop_ends_the_group`: after `Spawn`, `stop()` → the stub's pid and its child's pid are
    both gone.
  - `resume_and_load_report_how_they_opened`, and an unknown id's resume reports `Err`.
  - `moa-tmux`'s existing client tests pass unchanged after the move.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A4.** `convert.rs` holds every library ↔ wire conversion; nothing
  else names a library type.
- [ ] **Step 4: Run the tests again,** and the workspace. Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the ACP host, behind moa's own types`.

## Task 4: the real adapter, packaged

**Files:** `flake.nix`; `crates/moa-agents/src/acp/mod.rs` (`BUILT_IN_ADAPTER` doc);
new test in `crates/moa-agents/tests/acp_host.rs` (`a_test_build_knows_no_adapter`).

**Interfaces:**
- Produces: `packages.<system>.claude-agent-acp` (A2's override), `packages.default` built
  with `MOA_ACP_ADAPTER`, the adapter in the dev shell's `packages`.

- [ ] **Step 1: Failing test** `a_test_build_knows_no_adapter`: `BUILT_IN_ADAPTER == None`
  (it passes already; it guards the rule that no test build is given the variable).
- [ ] **Step 2: Implement A2** in `flake.nix`. `cargoExtraArgs` stays `--locked -p moa`.
- [ ] **Step 3: Check, without running the adapter:**
  - `nix build .#claude-agent-acp` builds, and `nix path-info -r .#claude-agent-acp | grep -c
    claude-code` prints `0`;
  - `nix build .#default` builds; `ls result/bin` is `moa moactl`; `strings
    result/bin/moa | grep -c claude-agent-acp-0.100.0` is at least `1`;
  - `nix develop --command sh -c 'command -v claude-agent-acp && test -z
    "${MOA_ACP_ADAPTER:-}"'` succeeds;
  - `nix flake check` passes.
- [ ] **Step 4: Commit.** `v1: package moa's agent, claude-agent-acp 0.100.0`.

## Task 5: the feed in the core and the store

**Files:** `crates/moa-core/src/central/{mod.rs,feed.rs}`, `publish.rs` (`Topic::Feed`,
`FeedRow`, `MoaRow`, `FEED_SHOWN`), `change.rs` (topics), `state.rs` (`Snapshot.{feed_tail,
feed_keys}`, `State.{feed_tail, feed_keys, central}`, `apply`); `crates/moa-store/src/
{rows.rs,snapshot.rs}` (`rows::feed`, the two loads); tests
`crates/moa-store/tests/snapshot.rs`, new `crates/moa-core/tests/central_feed.rs`, the
`moa-proto` schema snapshot.

**Interfaces:**
- Produces: `central::feed::{kind::{YOU, MOA, THOUGHT, TOOL, PLAN, SYSTEM, ERROR},
  text_body(&str) -> String, row(state, tx, now, thread, kind, message_id, body, state_col)
  -> FeedEntry}`; `central::Moa` with `Phase` (Task 7 fills it in); `State::feed_tail()`,
  `State::feed_keys(thread) -> Option<&BTreeSet<String>>`; `Topic::Feed`.

- [ ] **Step 1: Failing tests.**
  - Store: `a_snapshot_carries_the_feed_tail_and_each_accounts_newest_keys`: 150 rows over
    two threads of `acct-a` and one of `acct-b` → `feed_tail` is the newest 100 in id order;
    `feed_keys` holds the non-null `message_id`s of `acct-a`'s newer thread and `acct-b`'s
    thread only.
  - Core: `feed_rows_publish_under_feed` (keyed `#<id>`, `body` parsed to JSON);
    `the_tail_keeps_the_newest_100`; `the_state_row_says_off_at_start`; `applying_a_row_adds_
    its_key`; `a_thread_change_publishes_feed`.
  - The schema snapshot gains `feed`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A10,** trimming the tail as `trim_activity` does.
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa's feed is published, newest first`.

## Task 6: moa's launch spec, agent dir and system text

**Files:** `crates/moa-core/src/config.rs` (`AccountConfig.acp`), `state.rs` (`Deps.{acp_adapter,
agent_dir, scope_file, constitution_file}` and their test defaults under `/tmp`),
`ops/launch.rs` (`store_var`), new `central/{launch.rs,prompt.rs,constitution.md}`;
`crates/moa-platform/src/paths.rs` (`Paths::agent_dir(b)`); tests
`crates/moa-core/tests/{config.rs,central_launch.rs}`, `crates/moa-platform/tests/paths.rs`.

**Interfaces:**
- Produces: `central::launch::spec(state: &State) -> Result<(AccountName, AdapterSpec),
  OpError>`; `central::launch::settings(folders: &[PathBuf]) -> String`;
  `central::prompt::{SystemText { full, claude_md, brief }, system_text(state: &State, now:
  &Now) -> SystemText}`; `ops::launch::store_var(store, home, mode, canonical) ->
  Option<String>`; `Paths::agent_dir(&self, b: &str) -> PathBuf`.

- [ ] **Step 1: Failing tests.**
  - `the_spec_runs_the_built_in_adapter_with_the_specs_env`: exact `env` and `unset` lists.
  - `an_accounts_acp_overrides_the_built_in`.
  - `the_default_store_unsets_claude_config_dir`, and `launch_env` still gives the same
    words as before (its existing tests unchanged).
  - `no_account_no_launcher_no_adapter_say_so`: the three exact texts of A3.
  - `the_allow_list_is_exact`: two folders, one holding a space, give exactly A3's JSON.
  - `the_system_text_is_constitution_scope_and_brief` (with and without a scope file).
  - `a_constitution_file_replaces_the_embedded_one`.
  - `the_embedded_constitution_is_short_and_anonymous`: ≤ 8,192 bytes; no `/Users/`,
    `/home/`, `@`, or `.claude-` in it.
  - Config: `acp_parses_and_expands` (`~` expanded; a relative path refused with
    `launcher`'s wording).
  - Paths: `agent_dir_is_in_the_brain_dir`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A2's config key, A3 and A9.** Write `constitution.md` from the spike's
  `spike/master-chat/master/CLAUDE.md`: keep its rules, use v1's `moactl` verbs (spec §6.10),
  drop anything about its scripts, folders or accounts.
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: how moa's agent is started, and what it's told`.

## Task 7: threads

**Files:** `crates/moa-core/src/central/{mod.rs,threads.rs}`, `effect.rs` (`Input::Acp`,
`Effect::Acp`, `runs_at_once`, `is_heard`), `op.rs` (`ConsolePrompt`, `ConsoleCancel`:
variants, `METHODS`, `writes`, `method`), `core.rs` (`decide`'s branches, `Input::Reload`),
`ops/mod.rs` (the new ops go to `central`); tests new `crates/moa-core/tests/
central_threads.rs`, `skeleton.rs` samples, the `moa-proto` schema snapshot.

**Interfaces:**
- Consumes: Task 3's wire types, Task 5's feed, Task 6's `spec` and `system_text`.
- Produces: `central::send(state: &mut State, now: &Now, text: String, automatic: bool,
  waiter: Option<Waiter>) -> Vec<Effect>` with `Waiter { conn: ConnId, req: ReqId, wait: bool
  }`; `central::heard(state: &mut State, now: &Now, input: AcpIn) -> Vec<Effect>`;
  `central::reload(state: &mut State) -> Vec<Effect>`; `central::threads::{How, choose(state:
  &State, account: &AccountName, today: Date, caps: Caps) -> How}`.

- [ ] **Step 1: Failing tests.**
  - `a_first_prompt_starts_the_agent_then_a_thread`: `console.prompt` → `Acp(Start { gen: 1
    })` and the reply `moa has it`; `Started` → `Acp(Open { New { system_prompt: Some(full)
    } })`; `Opened(New)` → one `Persist [Thread, Feed(you)]`, then `Acp(Prompt)`.
  - `the_prompt_goes_only_after_its_row`: `check_order` holds; `Prompt` doesn't run at once.
  - `a_restart_the_same_day_resumes`: a snapshot with today's `acct-a` thread → `Resume`, no
    new `Thread` row.
  - `without_resume_it_loads` and `without_either_it_starts_a_new_thread`.
  - `a_new_day_starts_a_thread_with_the_brief`: an open thread from yesterday → `Open New`,
    the system prompt holding today's brief heading.
  - `a_new_account_starts_a_new_thread`.
  - `without_system_prompt_the_brief_rides_the_first_prompt`: `claude_md` is `Some`, the
    first `Prompt`'s text starts with the brief, the `you` row's body is only your text, and
    the second prompt is sent as typed.
  - `a_reopen_that_fails_starts_a_new_thread_and_says_so` (A8's exact line).
  - `a_failed_new_thread_refuses_the_prompt`.
  - `a_second_prompt_while_busy_is_refused` (while starting, and while thinking).
  - `no_account_is_refused_before_anything_starts`: no `Acp` effect at all.
  - `empty_and_oversized_prompts_are_refused`.
  - `stale_generations_are_dropped`.
  - `a_reload_resaves_an_unsaved_thread_and_forgets_an_unsent_prompt`.
  - `choose_is_exhaustive` (proptest over thread rows, days, accounts and caps: never `Use`
    of another account's or day's thread; `Resume` only with `caps.resume`).
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A6, A7 and A8** (threads and the prompt's path; turn contents come in
  Task 8).
- [ ] **Step 4: Run them again,** and the workspace. Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa keeps one thread a day`.

## Task 8: turns into the feed

**Files:** `crates/moa-core/src/central/{turn.rs,feed.rs,mod.rs}`, `core.rs`
(`Input::ClientGone` drops waiters); tests new `crates/moa-core/tests/central_turns.rs`.

**Interfaces:**
- Consumes: Task 7's `heard` and `Moa`.
- Produces: nothing new outside `central`; the `state` row's phases are live.

- [ ] **Step 1: Failing tests.**
  - `chunks_with_one_message_id_make_one_row` (and a second id makes a second row).
  - `chunks_without_ids_merge_until_a_tool_breaks_them`.
  - `a_tool_call_row_follows_its_updates` (`state` pending → in_progress → completed; key
    `tool:<id>`).
  - `a_plan_is_one_row_updated_in_place`.
  - `user_echo_and_bookkeeping_are_not_stored`.
  - `the_turn_end_answers_the_waiter_with_moas_words` (two `moa` rows joined).
  - `an_early_stop_says_why`: `moa's turn ended early: max_tokens`, as reply and `system`
    row.
  - `cancel_sends_cancel_and_the_end_is_cancelled`: open tool rows become `cancelled`.
  - `cancel_with_no_turn_says_so`.
  - `a_permission_is_refused_by_kind`: options `[allow_once, reject_always, reject_once]` →
    `Permission { choice: Some(<reject_once's id>) }` and the `system` row `moa wanted to
    <title>, and was refused: moa can't ask you yet`.
  - `no_reject_once_cancels_the_turn`: `Cancel`, then `Permission { choice: None }`, and the
    line ends `; its turn was cancelled`.
  - `an_exit_mid_turn_says_so`: the `error` row `moa's agent stopped: <why>; your next prompt
    starts it again`, the waiter's `Err`, phase `off`.
  - `a_client_gone_mid_turn_leaves_the_turn`: no `Cancel`; the rows keep coming; no reply.
  - `load_never_duplicates_rows`: while replaying, known keys and id-less updates are
    skipped, new keys are written.
  - `refused_writes_still_end_the_turn`: no `Persist`, the reply comes, one warn log with the
    count.
  - `odd_text_round_trips`: emoji, CJK, ZWJ, RTL, quotes, newlines, 100,000 characters,
    U+FFFD, exact in the `body`; an image block reads `[image]`.
  - `a_huge_tool_input_is_cut` (1 MB → 2,000 chars).
  - `the_state_row_follows_the_turn`: off → starting → ready → thinking → ready, and
    `failed` with `why` after a failed start.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A5** and the turn half of A6 and A7.
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: everything moa says lands in the feed`.

## Task 9: the daemon hosts moa

**Files:** new `crates/moa-daemon/src/acp.rs`; `core_task/mod.rs` (`Held::Acp`, the actor in
`start_inner`, its test hook), `drain.rs` (`step`, `run_now` for the at-once commands,
`release` for a held `Prompt`, the `Input::Acp` replay, stop),
`recover.rs` (`fail_held`), `run.rs` (`Deps`: `acp_adapter` from `BUILT_IN_ADAPTER`,
`agent_dir`, `scope_file`, `constitution_file`); `crates/moa-daemon/Cargo.toml`
(`moa-agents` with `acp`); tests new `crates/moa-daemon/tests/moa.rs`, `core_task/tests.rs`.

**Interfaces:**
- Consumes: Task 3's `Adapter`, Task 7's `Input::Acp`/`Effect::Acp`.
- Produces: `acp::start(to_core: impl Fn(AcpIn) + Send + Sync + 'static, failed: impl
  Fn(&'static str, String) + Send + 'static) -> UnboundedSender<AcpMsg>`.

- [ ] **Step 1: Failing tests** (in-process daemon on a `TempBrain` whose `moa.toml` has
  `[brains.default] moa_account = "acct-a"` and `[accounts.acct-a]` with `launcher` =
  `fake_claude()` and `acp` = `stub_acp()`; the script is written to `paths.agent_dir`):
  - `a_prompt_lands_in_the_feed`: `console.prompt {wait: true}` answers with the stub's
    words; the DB holds one `moa_threads` row and the `you` and `moa` rows.
  - `two_prompts_launch_one_adapter` (`spawn::launches()` grows by 1).
  - `a_restart_the_same_day_resumes`: stop and start the daemon, prompt → the stub's log shows
    `session/resume` with the first thread's id; still one `moa_threads` row.
  - `a_crash_mid_turn_says_so_and_the_next_prompt_resumes` (`Die`).
  - `a_hang_can_be_cancelled` (`Hang`, then `console.cancel` from a second client).
  - `stopping_the_daemon_ends_the_adapter_and_its_children` (`Spawn`; both pids gone within
    10 s of the stop returning).
  - `a_failed_commit_replays_moas_updates` (a store `Fault` on the commit carrying a chunk →
    after the reload the `moa` row is whole, once).
  - `the_adapter_gets_the_specs_env` (the stub's `initialize` line).
  - `a_panicking_acp_actor_is_restarted_and_says_so` (`AcpMsg::Crash` → `Exited` for the live
    generation, the core's `ActorFailed` line).
  - `twenty_verbs_launch_nothing` and `an_idle_brain_wakes_only_for_the_backup` pass
    unchanged.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A11.**
- [ ] **Step 4: Run them again,** three times. Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the daemon hosts moa's agent`.

## Task 10: `moa console`

**Files:** new `crates/moa/src/console.rs`; `crates/moa/src/main.rs` (`Command::Console`),
`crates/moa-core/src/config.rs` (`RESERVED_BRAIN_NAMES` gains `console`); tests in
`main.rs`'s `tests`, new `crates/moa/tests/console.rs` (parsing and rendering only).

**Interfaces:**
- Consumes: `Op::{ConsolePrompt, ConsoleCancel}`, `Topic::Feed`, `ctl::{Here, running,
  finish}`.
- Produces: `console::render(rows: &[FeedRow], tz: &TimeZone) -> Vec<String>`.

- [ ] **Step 1: Failing tests.**
  - `console_parses`: `moa console prompt hello there` → text `hello there`; `moa console
    prompt -- -x` → `-x`; `moa console cancel`; `moa console feed -n 5`; `--brain B` on each.
  - `every_verb_and_subcommand_is_reserved` passes with `console`.
  - `render_says_each_kind_on_one_line`: `HH:MM you  …`, `HH:MM moa  …` (first line, then
    `…` when there are more), `HH:MM ⚙ <title> · <state>`, `HH:MM · <system text>`, `HH:MM
    ✗ <error text>`, oldest first, the last `n`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement A10's CLI.** `prompt` sends `wait: true` and prints the reply's
  text; `feed` subscribes to `feed`, prints `render`'s lines and the `state` row's phase, and
  exits.
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa console prompt, cancel and feed`.

## Task 11: 6a end to end

**Files:** new `crates/moa/tests/moa_e2e.rs`; `crates/moa/tests/common/mod.rs` (a `moa_toml`
helper for the stub account, if two tests need it).

- [ ] **Step 1: Write the tests** (the real `moa` binary, autostarted, on a `TempBrain`;
  `stub-acp` as the account's `acp`; no tmux needed):
  - `moa_answers_from_the_command_line`: `moa console prompt "hello 🦀 שלום"` prints the
    stub's answer (itself odd text); `moa console feed` shows both lines; `moa sql "SELECT
    kind FROM feed ORDER BY id"` is `you`, `moa`.
  - `a_permission_is_refused_and_said`: the stub logs the `reject_once` id; `feed` shows the
    `system` line.
  - `the_thread_survives_a_daemon_restart`: prompt, `moa shutdown`, prompt → the stub logs
    `session/resume`; `moa_threads` has one row.
  - `a_cancel_from_another_terminal_ends_a_hang`: a background `moa console prompt` on a
    `Hang` turn; `moa console cancel` → the first prints `moa's turn ended early:
    cancelled`.

  Each runs 10 times under load, and each is made to fail once on purpose.
- [ ] **Step 2: Run them.** Expected: PASS.
- [ ] **Step 3: Commit.** `v1: 6a end to end`.

**Try it** (p3's body): in the dev shell, a sandbox `moa.toml` with `[brains.default]
moa_account = "acct-a"` and `[accounts.acct-a]` (`kind = "claude"`, its `store` and
`launcher`, and `acp` = the path `command -v stub-acp` gives after a `cargo build
--workspace`). Then `moa --sandbox`, and from another terminal with `MOA_ROOT` set to the
sandbox's root as in earlier try-its: `moa console prompt hello`, then `moa console feed`.
**By hand only, with your go-ahead:** set `acp` to `$(command -v claude-agent-acp)` (the dev
shell's) with a logged-in account, and `moa console prompt "what's on today?"`.

## Carried, and where each goes

- **The watchdog, respawn on crash, and a hang that ignores cancel:** 6c. Until then a hard
  hang stays until the daemon restarts.
- **Permission cards, "moa needs you":** 6c, replacing the refusal (A7's `Permission` path).
- **Queued prompts and reports, the one channel:** 6c, through `central::send`'s `automatic`.
- **The feed's paging, rendering and the console frame:** 6b, from `Topic::Feed`'s rows and
  the `state` row.
- **The real-adapter recording, its scrub tool and the replay smoke test:** 6b. It should
  also watch for adapter issue #1621 (the first prompt after a resume or load may get a stale
  result).
- **The opening brief on first presence, `client-active`:** 6d.
- **`_meta.claudeCode.options`** (plugins, model) is unused in 6a; `moactl` reaches moa through
  the agent dir's `bin/` instead.
- **`opened_by = moa`** for terminals moa opens: 6d, from `MOA_SESSION=moa`, which 6a sets.
