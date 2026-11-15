# moa v1 slice 2b: daemon, protocol, suggestions, CLI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put slice 2a's core behind a per-brain daemon and a Unix socket, so that
`moactl task|note|project|suggest|suggestion|brief|attention|find` work against a copy of
the user's brain, autostarting the daemon, with no tmux and no process launched per verb.

**Architecture:**
- `moa-proto` (new) holds the wire types: newline JSON-RPC 2.0 messages, `hello` with the
  build match, `subscribe`/`patch`/`event`, error codes, a JSON Schema. It also holds the
  verb grammar `cli::Verb` (P11), which the CLI and suggestion validation share.
- `moa-daemon` (new) runs one core task (`handle` from 2a, inside `catch_unwind`), the
  store actor, a wall-clock scheduler, and one task per client connection. The core task
  runs inputs in drains and commits each drain's changes as one batch, with one batch in
  flight; nothing a drain does outside the core happens before its commit (2a call S4).
- `moa-client` (new) connects, says hello, and autostarts the daemon under a start lock.
- The `moa` binary dispatches `moactl` on `argv[0]`, and gains `moa daemon`, `moa sql`,
  `moa service`, `moa status` and `moa shutdown`.

```
 moactl task add … ──► moa-client ──hello/op──► socket (0700 dir, peer uid) ──► session task
        ▲    autostart: start lock → Spawner(moa daemon --notify-ready) → "ready"     │ Input::Client
        └──────────────────── reply (after commit) ◄── core task ◄──────────────────┘
                                                        │ drain's batch ─► StoreHandle::commit ─► Ok | Failed | Stopped
                                                        │ SetTimer ─► scheduler (WallAlarm)
                                                        └ Model ─► watch ─► subscribers (patch)
```

**Tech Stack:** `tokio`, `tokio-util` (`codec`, `rt`), `futures-util`, `tracing`,
`tracing-subscriber`, `clap` (in `moa-proto`), `schemars` 1 (new), `shell-words`, `insta`,
`assert_cmd`.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`: §6.1 (allowed launches),
§6.3, §6.4 (protocol), §6.5 (`moa sql`, file locations), §6.7 (hook connect), §6.9,
§6.10 (verbs, caller identity), §6.12 (suggestions), §11.2 (anti-goal tests). Roadmap:
Slice 2 items 6–10 and its "done when" and "try it"; P4, P10, P11, P13. Parity: R1 §2.9,
§2.13; `spike/moa-proto/moactl.py`.

**Builds on** `2026-11-15-moa-v1-slice-2a-core.md`: `handle`, `State`, `Input`
(incl. `Reload`), `Op`, `Done`, `Change`, `Model`, `Topic`, `CoreEvent`, `StoreHandle`
(`commit`, `CommitError`, `Fault`), `Paths` (incl. `check_v1_allowed`,
`ensure_private_dir`), `Spawner`, `WallTimer`, `ZoneWatch`, `Config::from_file_text`,
`caller_store`, `TypicalBrain`. Read 2a's calls S1–S19 first; S4 (group commit) and S17
(the `MOA_ROOT` gate) shape this plan most.

## PRs (roadmap P4)

Three PRs, each its own branch from `main` after the previous one merged, each with
`/code-review medium --fix`, green CI, and "Restart needed: nothing; the spike is
untouched".

| PR | Branch | Tasks | Title |
|---|---|---|---|
| b1 | `v1/slice-2b-1` | 1–3 | `v1 slice 2b (1/3): protocol, grammar, suggestions` |
| b2 | `v1/slice-2b-2` | 4–6 | `v1 slice 2b (2/3): the daemon` |
| b3 | `v1/slice-2b-3` | 7–11 | `v1 slice 2b (3/3): client, CLI, sql, service, end to end` |

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| D1 | **`upgrade` in slice 2 is a clean stop.** A `moactl` whose build differs from the daemon's sends `upgrade`, reads to EOF, waits (≤ 10 s) until it can take the daemon's lock, which proves the old process is gone, and then autostarts its own build. Slice 9 replaces the stop with re-exec | Rebuilding v1 during dogfood must not need a manual restart. The state is all in SQLite, so a stop loses nothing. Build ids are hashes with no order, so there's no "newer" to compare (P10) |
| D2 | **Autostart waits for one line, not for the socket.** Under `run/<brain>.start` (an exclusive file lock) the client retries the connect once, then spawns `moa daemon --brain B --notify-ready` through `Spawner`. The daemon's first stdout line is `ready`, `already running` or `error: <one line>`. Then it points stdout at `/dev/null`. The client: `ready` → connect; `already running` or EOF → connect once more; `error:` → `couldn't start moa: <line> (log: <stderr log>)`. The wait is capped at 10 s | Event-driven (no connect loop); two racing clients or a launchd copy start one daemon; the real reason reaches the user; a dropped pipe can't hurt the daemon later |
| D3 | **The daemon lock is `std::fs::File::try_lock` on `run/<brain>.lock`.** "Already running" exits **0** | flock without `unsafe`. Exit 0 lets launchd's `KeepAlive {SuccessfulExit = false}` leave a second copy alone |
| D4 | **`subscribe` always answers a full snapshot**; `since` is accepted and ignored until a client reconnects with one (slice 5) | YAGNI; the snapshot is small |
| D5 | **Hook notifications are accepted before or without `hello`, never answered, and dropped** (debug log) until slice 4 ingests them | Spec §6.7: the hook connects, writes one line and exits within a 200 ms budget, with no round trip |
| D6 | **`cli::Verb` has every verb and flag of R1 §2.13.** Verbs built in later slices parse, and the CLI answers `moactl <verb> isn't in this build yet` without starting a daemon | One grammar from day one (P11), and agents get a clear answer |
| D7 | **Long flags may be abbreviated** (clap `infer_long_args`), as argparse allowed | Agents' existing `moactl` habits keep working |
| D8 | **The core gets the grammar and path canonicalisation through `Deps`** (`parse_action`, `canonical`: fn pointers the daemon passes in; the core can't depend on proto or do IO). `close` is a valid action; "yes" reports `close isn't in this build yet` for it | P11's one grammar, with the dependency rule kept, the same way P20 passes the transcript check. A path in a suggestion action is canonicalised the same way as one typed at the CLI |
| D9 | **At startup, suggestions whose actions no longer parse are flagged, never dropped**, and "yes" on a flagged one is refused in one line | Roadmap item 8; the imported spike data includes an action the spike's own parser rejected |
| D10 | **Service**: label `moa.daemon.<brain>`; plist with `RunAtLoad`, `KeepAlive {SuccessfulExit = false}`, `ProcessType = Adaptive`; systemd unit with `Restart=on-failure`. **The file names a stable `moa` path**, chosen in this order: `--exe PATH`; else `<home>/.nix-profile/bin/moa` when it exists (the profile link, unresolved); else `current_exe()`. The result is refused when it's under `/nix/store/`. On macOS `current_exe()` is the path `moa` was run by, symlinks unresolved; on Linux it's `/proc/self/exe`, always resolved, so there a Nix install is found by the profile link or `--exe`. **Under `MOA_ROOT` the file is written but never registered** | Spec §6.7: never a Nix store path, which disappears after an upgrade and GC. Tests and the sandbox must never register a real launchd or systemd job |
| D11 | **`moa status` and `moa shutdown`** are small subcommands | The tests, the try-it, and stopping a sandbox need them |
| D12 | **The daemon reads its zone with `TimeZone::try_system()`** (which honours `TZ`) and stops with `can't tell your time zone; set TZ` if that fails | Deterministic tests; never a silent UTC |
| D13 | **The Nix package ships `bin/moactl -> moa`, plus `apps.moactl`** | The try-it can run `moactl` without v1 on `PATH` (Global Constraints) |
| D14 | **The offline tools refuse a brain whose daemon holds the lock**, and `adopt-spike --into` creates the brain's folder (`0700`) when the target is exactly `paths.db(<brain>)` | Roadmap Global Constraints ("the daemon is the only writer"); the try-it's first step must work on a fresh root |
| D15 | **A panic inside `handle` is caught** (`catch_unwind`): that input fails with `moa hit a bug answering that; it reloaded`. The drain stops there: the inputs before it are committed, and the ones after it wait for the next drain. Then the core reloads from disk and subscribers get a fresh snapshot. A panic while loading (`Startup`, `Reload`) stops the daemon instead of looping. The scheduler and the accept loop run under `supervise` | `handle` is pure and returns its changes only when it finishes, so catching is safe and a half-done input never reaches disk. Restarting the core task would lose its channels and held replies |
| D16 | **Lines are capped at 8 MiB**; a longer one gets an error and the connection is closed | The hook's stdin cap (spec §6.7); a buggy client can't exhaust memory |
| D17 | **The autostarted daemon gets a clean environment**, `env_clear()` then:<br>• from the client's `Paths`: `HOME` = `paths.home()`, plus `MOA_ROOT` = the same when sandboxed;<br>• from the parent: `PATH`, `TMPDIR`, `USER`, `LANG`, `LC_*`, `MOA_LOG`; `TZ` only when sandboxed; `MOA_REAL_HOME` only when *not* sandboxed.<br>Its cwd is the state dir | An agent's `MOA_SESSION`, `CLAUDE_CONFIG_DIR`, `TMUX` or `TZ` must not become the brain's (lesson 5; from slice 3 the daemon starts the tmux server) |
| D18 | **`moactl`'s brain comes from `MOA_BRAIN` or routing only**; there's no global `--brain` flag on the verbs (`moa status|shutdown|daemon|sql|service` keep theirs) | `setup --brain` (slice 8) means "send this account to a brain"; one flag with two meanings is a trap |
| D19 | **`status` is a `CoreHandle` query, not an `Op`** | One `status` method on the wire, and the schema shows it once |
| D20 | **2a's S17 applies here**: `moa daemon`, autostart and `moa service install|uninstall` refuse without `MOA_ROOT` unless `MOA_REAL_HOME=1`, until slice 10 | The spike is live and in daily use on this machine |

## Parity rows slice 2 delivers (spec §7)

Delivered by 2a and 2b together (later slices finish the rows marked "part"):
- Brain = own daemon, DB and scope (2.1): the daemon per brain; its tmux server is slice 3
  (part).
- Robustness: logged errors (2.8): tracing to `logs/<brain>.log`, actor errors as events
  (part; the console side is slice 5).
- Due dates as data, relative input (2.10): `--due` (part; nagging is slice 6).
- Every moactl verb and flag (2.13): the whole grammar; the verbs of slices 4, 7 and 8 say
  "not in this build yet" (part).
- Session name resolution (2.13).
- `brief`, `attention`, `find`, `project`, `note`, `task …` (2.13).
- `ui` events (2.18).
- Routing: only / brain / except (2.14), through `route()` in the CLI (the hook path is
  slice 4).
- Suggestions: suggest, validate, yes/no/later, ls (2.9): the data side; cards and the
  signal to moa are slice 6 (part).
- Session data ops `set`, `group …`, `archive`, `ls` (2.13): their data columns (the tmux
  columns are slice 4).

## Global Constraints

The roadmap's and 2a's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **Tests never touch real user state.**
  - Every daemon in a test runs under a `MOA_ROOT` short root in `/tmp`: in-process through
    `Paths::with_root`, or as a child process through `moa_testkit::TempBrain`, whose
    environment is cleared and then set to `MOA_ROOT`, `HOME` (both the root), `TZ` and
    `PATH` only.
  - No test runs `launchctl` or `systemctl` (D10), and no test uses a real brain.
  - `TempBrain` stops every daemon it started when it drops.
- **Manual runs don't either** (D20): without `MOA_ROOT`, the daemon, autostart and service
  commands refuse.
- **Zero launches per verb.** The only product launches in this slice are the autostart
  (one per daemon start) and service registration, both through `Spawner`.
- **One plain line per error**, a non-zero exit, no tmux words. Clap usage errors exit 2 as
  argparse did.
- **No record contents in logs** (2a call S15). `logs/<brain>.log` isn't rotated before the
  switch-over; rotation is on the roadmap's after-switch list.
- **New dependencies** (workspace, exact version resolved when added): `schemars 1` in
  Task 1, `tokio-util` (codec, rt) and `tracing` in Task 4, `futures-util` (sink) and
  `tracing-subscriber` (fmt) in Task 6.

## Review Focus

1. **Starting and stopping under use**: two `moactl`s racing to start the daemon, a stale
   socket file from a crash, a second `moa daemon`, a rebuilt binary while a daemon runs, a
   daemon that can't start, a fresh root with no `run/` yet.
   - Expected: one daemon; every client gets its answer or the real reason; the rebuilt
     client restarts the daemon on its own build; nothing hangs.
   - Pinned by `racing_clients_start_one_daemon`, `a_rebuilt_client_restarts_the_daemon_on_its_build`
     (Task 8), `a_fresh_root_autostarts`, `a_daemon_that_cannot_start_says_why` (Task 7),
     `a_stale_socket_file_is_replaced` and `a_second_daemon_says_already_running` (Task 6).
2. **Agents' existing invocations**: conversation-id prefixes, abbreviated long flags,
   `--json` anywhere, multi-word titles without quotes, `""` to clear, the imported
   suggestion actions.
   - Expected: what worked against the spike works here.
   - Pinned by `spike_style_invocations_parse` (Task 2) and `imported_suggestion_two_runs`
     (Task 3).
3. **Brain isolation through the CLI**: an account sent to `work` except under
   `~/private`, run from both kinds of folder, including the default account with no
   `CLAUDE_CONFIG_DIR`.
   - Expected: nothing lands in the other brain's DB, and a project in another brain's
     folder is refused.
   - Pinned by `work_never_lands_in_default` (Task 11).
4. **A misbehaving client**: garbage, an oversized line, a request before `hello`, a lone
   hook line, a slow subscriber.
   - Expected: an error reply or a fresh snapshot, never a crash, and other clients are
     unaffected.
   - Pinned by Task 6's server tests.
5. **A failure inside the daemon**: a read while a write is uncommitted, writes queued
   behind a failed commit, a commit that keeps failing, the store thread dying, a panic in
   `handle`.
   - Expected: no client ever sees state the DB doesn't have; every waiting request gets
     one line; no hot loop; the daemon keeps serving.
   - Pinned by Tasks 4–5's tests.

---

### Task 1: `moa-proto` wire types

**Files:**
- Create: `crates/moa-proto/Cargo.toml`, `src/lib.rs`, `src/wire.rs`, `src/patch.rs`,
  `tests/wire.rs`, `tests/schema.rs` (+ `tests/snapshots/`)
- Modify:
  - `crates/moa-core/Cargo.toml`: optional `schemars`, feature `schema`.
  - `crates/moa-core/src/{op.rs, publish.rs, state.rs}`:
    `#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]` on `Op` and its param
    structs, `Done`, `Topic`, `CoreEvent` and `CallerInfo`.
  - `Cargo.toml`: workspace `schemars`.
  - `.github/workflows/ci.yml`.

**Interfaces:**
- Produces (`moa_proto`, depending on `moa-core` with `schema`, plus `serde`, `serde_json`,
  `schemars`):
  ```rust
  pub use moa_core::publish::{Topic, CoreEvent, Model};
  pub const MAX_LINE: usize = 8 << 20;
  pub enum Id { Num(i64), Str(String) }                                       // serde untagged
  pub enum Call { Hello(Hello), Status, Shutdown, Upgrade, Subscribe(Subscribe), Op(moa_core::op::Op) }
  pub enum Incoming { Request { id: Id, call: Call }, Hook(serde_json::Value) }  // "hook" is a notification
  pub struct Hello { pub client: String, pub build: String, pub caller: CallerInfo }
  pub struct HelloOk { pub brain: String, pub build: String, pub rev: u64, pub matched: bool }
  pub struct StatusOk { pub brain: String, pub build: String, pub pid: u32, pub launches: u64,
                        pub sessions: BTreeMap<String, usize>, pub caller: String }
  pub struct Subscribe { pub topics: Vec<Topic>, pub since: Option<u64> }
  pub struct SnapshotMsg { pub rev: u64, pub topics: BTreeMap<Topic, Vec<serde_json::Value>> }
  pub struct Patch { pub rev: u64, pub upsert: BTreeMap<Topic, Vec<serde_json::Value>>, pub delete: BTreeMap<Topic, Vec<String>> }
  pub struct RpcError { pub code: i64, pub message: String }                  // message: one human line
  pub mod code { pub const PARSE: i64 = -32700; pub const INVALID_REQUEST: i64 = -32600; pub const METHOD_NOT_FOUND: i64 = -32601;
                 pub const INVALID_PARAMS: i64 = -32602; pub const FAILED: i64 = -32000; pub const BUILD_MISMATCH: i64 = -32001;
                 pub const HELLO_FIRST: i64 = -32002; pub const TOO_LONG: i64 = -32003; }
  pub fn decode_incoming(line: &str) -> Result<Incoming, (Option<Id>, RpcError)>;
  pub fn allowed_on_mismatch(call: &Call) -> bool;                            // Hello, Status, Upgrade, Shutdown
  pub fn encode_result(id: &Id, result: &impl Serialize) -> String;          // one line, no trailing '\n'
  pub fn encode_error(id: Option<&Id>, e: &RpcError) -> String;
  pub fn encode_notice(method: &str, params: &impl Serialize) -> String;     // "patch", "snapshot", "event"
  pub enum ServerLine { Result { id: Id, value: serde_json::Value }, Error { id: Option<Id>, error: RpcError }, Notice { method: String, params: serde_json::Value } }
  pub fn decode_server_line(line: &str) -> Result<ServerLine, RpcError>;     // the client side
  pub fn snapshot_of(model: &Model, topics: &[Topic]) -> SnapshotMsg;
  impl Patch { pub fn between(old: &Model, new: &Model, topics: &[Topic]) -> Option<Patch> }   // None when nothing differs
  ```
  - Method names: `hello`, `status`, `shutdown`, `upgrade`, `subscribe`, `hook`, plus every
    entry of `Op::METHODS`.
  - For an `Op` variant with no fields (`brief`, `attention`, `suggestion.ls`), missing,
    `null` and `{}` params are all accepted.
  - Errors from `decode_incoming`:
    - not JSON → `PARSE`, `that isn't JSON`, id None;
    - a JSON array → `INVALID_REQUEST`, `batches aren't supported`;
    - `jsonrpc` missing or not `"2.0"` → `INVALID_REQUEST`, `that isn't a JSON-RPC 2.0 message`;
    - a request with an unknown method → `METHOD_NOT_FOUND`, `moa doesn't know the method <m>`;
    - params that don't fit → `INVALID_PARAMS`, `the parameters for <m> don't fit: <serde's first line>`.

    An unknown notification method is ignored (additive, spec §6.4).

- [ ] **Step 1: Write the failing tests**
  - `wire.rs`:
    - `a_task_add_request_decodes`:
      `{"jsonrpc":"2.0","id":7,"method":"task.add","params":{"project":"Lantern","title":"x",…}}`
      → `Request { id: Num(7), call: Op(TaskAdd(..)) }`.
    - `unit_ops_take_any_empty_params`: `brief` with no params, `"params": null` and
      `"params": {}` → `Op(Brief)` each.
    - `the_error_table`: each bad input above gives its code, its exact message and its id.
    - `string_ids_round_trip`: an id of `"a"` comes back as `"a"`.
    - `hook_is_a_notification`: `{"jsonrpc":"2.0","method":"hook","params":{…}}` → `Hook`.
    - `mismatch_allows_only_the_four`: `allowed_on_mismatch` is true for Hello, Status,
      Upgrade and Shutdown only.
    - `patch_between_models`: a changed row lands in `upsert` (with its `"key"`), a removed
      key in `delete`, an untouched topic is absent, equal models give `None`, and
      `rev` = the new model's.
    - `encoded_lines_have_no_newline`: a `Done` whose text has `\n` encodes to one line.
  - `schema.rs`, `protocol_schema` (it also asserts the text never contains `debug.panic`,
    which Task 5 adds with `schemars(skip)`): one JSON object with `schemars::schema_for!` of `Call`,
    `HelloOk`, `StatusOk`, `SnapshotMsg`, `Patch` and `CoreEvent`, as an
    `insta::assert_json_snapshot!`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement.** The envelope is parsed first as a `serde_json::Value`. A control
  method goes to its params type; any other method is deserialised as `Op` from
  `{"method": m, "params": p}`, after checking `Op::METHODS` so an unknown method is told
  apart from bad params.
- [ ] **Step 4: Extend the CI purity step** (2a Task 9) with the same two-line pattern for
  `moa-proto`, grepping for `^(tokio|rusqlite) `.
- [ ] **Step 5: Run them and see them pass**, and review the schema snapshot.
- [ ] **Step 6: Commit** `git commit -m "v1: moa-proto wire types, errors, patches and protocol schema"`

---

### Task 2: The verb grammar, `cli::Verb`

**Files:**
- Create: `crates/moa-proto/src/cli.rs`, `crates/moa-proto/tests/cli.rs`
- Modify: `crates/moa-proto/Cargo.toml` (`clap`, `shell-words`), `crates/moa-core/src/op.rs`
  (`pub enum Action { Op(Op), NotYet(&'static str) }`)

**Interfaces:**
- Produces (`moa_proto::cli`):
  ```rust
  #[derive(clap::Parser)] #[command(name = "moactl", infer_long_args = true)]
  pub struct Moactl { #[arg(long, global = true)] pub json: bool, #[command(subcommand)] pub verb: Verb }
  #[derive(clap::Subcommand)] pub enum Verb { /* every verb of R1 §2.13, below */ }
  pub struct VerbCtx { pub cwd: Option<PathBuf>, pub home: PathBuf }
  pub enum VerbError { NotYet(&'static str), Usage(String) }   // NotYet → "moactl <verb> isn't in this build yet"
  impl Verb { pub fn name(&self) -> &'static str; pub fn to_op(&self, ctx: &VerbCtx) -> Result<Op, VerbError>; }
  pub const VERB_NAMES: &[&str];                                    // for `moa <verb>` dispatch
  pub fn parse_action(a: &SuggestionAction, home: &Path) -> Result<Action, String>;
  ```
- The grammar, flag for flag from the spike's argparse (`moactl.py:main`), with no global
  `--brain` (D18):

  | Verb | Args and flags | `to_op` |
  |---|---|---|
  | `ls` | `--all` | `SessionLs { all }` |
  | `find Q` | | `SessionFind` |
  | `set ID` | `--title`, `--group` | `SessionSet` |
  | `archive ID…` (1+) | | `SessionArchive` |
  | `group OP ARGS…` | OP ∈ add, rename, rm | `GroupAdd`/`GroupRename`/`GroupRm`; a wrong arg count → Usage `moactl group add PATH \| group rename OLD NEW \| group rm PATH` |
  | `note TEXT` | `--project`, `--kind` (default `fact`) | `NoteAdd` |
  | `project NAME` | `--add PATH`, `--summary` | `ProjectAdd` with the path made absolute (below), else `ProjectShow` |
  | `task OP ARGS…` | OP ∈ add, set, ls; `--today --not-today --status --blocked-on --title --who {you,agent,other} --due --session --note --all --priority {p0,p1,p2,""}` | `TaskLs { project: args.first(), all }`; `TaskAdd { project: args[0], title: args[1..].join(" ") }`, with fewer than 2 args → Usage `moactl task add PROJECT TITLE… [--who you\|agent\|other] [--due DATE] [--session ID] [--status S]`; `TaskSet { id: args[0] }`, with a missing or non-numeric id → Usage `moactl task set ID [--status S] [--title T] [--who W] [--due D] [--priority P] [--session ID] [--note N]` |
  | `suggest TEXT` | `--do ARGS` (repeatable) | `Suggest { text, actions }` (Task 3) |
  | `suggestion OP [ID]` | OP ∈ yes, no, later, ls | `SuggestionAnswer`/`SuggestionLs` (Task 3) |
  | `brief`, `attention` | | `Brief`, `Attention` |
  | `open PATH` | `--account --title --group --prompt --bg` | NotYet |
  | `resume ID`, `focus Q`, `close ID` | `--bg` on resume | NotYet |
  | `sleep [ID…]` | `--dry-run`, `--idle HOURS` (f64) | NotYet |
  | `term [PATH]` | `--title`, `--pane` | NotYet |
  | `adopt PANE` | `--title --group --id` | NotYet |
  | `goto PANEL`, `layout OP ARGS…` (list, show, apply, set) | | NotYet |
  | `peers`, `ask TO TEXT [--timeout 540]`, `reply ID [--timeout 540]`, `asks` | | NotYet |
  | `setup [OP] [NAME] [FOLDERS…]` | OP ∈ list, install, only, remove, skip; `--brain`, `--except F…` | NotYet |
  | `import-herdr` | | NotYet |

  `open` has no default `--account`: the brain's `open_account` will supply it in slice 4.
- **Paths in args**: `~` and `~/x` expand with `ctx.home`; a relative path is joined to
  `ctx.cwd` and cleaned lexically (`.` dropped, `..` pops). With `cwd: None` (a suggestion
  action) a relative path is `Usage("use a full path in a suggestion")`.
- `parse_action`, in this order (the spike's order, `moactl.py:410`):
  1. A verb outside `ACTION_VERBS` → `a suggestion can run moactl archive, close, group,
     note, project, set, task`.
  2. `Moactl::try_parse_from(["moactl", verb, args…])`. A clap error gives the first line of
     its rendered text, minus the `error: ` prefix.
  3. `close` → `Ok(Action::NotYet("close"))`.
  4. Otherwise `to_op(&VerbCtx { cwd: None, home })` → `Ok(Action::Op(op))`.

- [ ] **Step 1: Write the failing tests** (`tests/cli.rs`)
  - `spike_style_invocations_parse`: each argv → the `Op` (full struct literals in the test):

    | argv | Op |
    |---|---|
    | `task add Lantern Order more boards --due fri --priority p1` | `TaskAdd { project: "Lantern", title: "Order more boards", due: Some("fri"), priority: Some("p1"), .. }` |
    | `task set 12 --stat done` (abbreviated) | `TaskSet { id: 12, status: Some("done"), .. }` |
    | `task set 3 --priority "" --due ""` | `priority: Some("")`, `due: Some("")` |
    | `--json ls --all` and `ls --all --json` | `SessionLs { all: true }`, with `json` true both times |
    | `set 3f9a1c52 --title "New name"` | `SessionSet { q: "3f9a1c52", title: Some("New name"), group: None }` |
    | `project Harbour --add ./harbour` (cwd `/h/private/lantern`) | `ProjectAdd { path: "/h/private/lantern/harbour", .. }` |
    | `project Shore --add ~/private` (home `/h`) | `ProjectAdd { path: "/h/private", .. }` |
    | `suggest "Close it?" --do "task set 1 --status done" --do "note x"` | `Suggest { actions: ["task set 1 --status done", "note x"] }` |
    | `group rename Private Home` | `GroupRename` |
  - `later_verbs_say_not_yet`: `open ~/x`, `peers`, `layout list`, `setup` → `NotYet` with
    the exact text.
  - `usage_errors_are_the_spikes`: `task set x`, `task add Lantern`, `group rename A` → their
    Usage lines.
  - `actions_parse_with_the_same_grammar`:
    - `task set 1 --status done` → `Op(TaskSet{..})`;
    - `task 1 --status done` → `Err` starting `invalid value '1'`;
    - `rm -rf x` and `open ~/x` → the whitelist line;
    - `close 3f9a1c52` → `NotYet("close")`;
    - `project X --add rel` → `use a full path in a suggestion`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `cli.rs`**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: cli::Verb, one grammar for moactl and suggestion actions"`

---

### Task 3: Suggestions in the core

**Files:**
- Create: `crates/moa-core/src/ops/suggestions.rs`, `crates/moa-core/tests/suggestions.rs`
  (+ snapshots)
- Modify: `crates/moa-core/src/{op.rs, state.rs, core.rs, ops/mod.rs, ops/projects.rs}`,
  `crates/moa-core/Cargo.toml` (`shell-words`; dev: `moa-proto`),
  `crates/moa-testkit/src/brain.rs` (`state_with`)

**Interfaces:**
- Consumes: `parse_action` (Task 2) through `Deps`; 2a's `Tx`, `activity::suggests`,
  `handle_checked`, `TypicalBrain`.
- Produces:
  - `pub struct Deps { pub parse_action: Option<fn(&SuggestionAction, &Path) -> Result<Action, String>>, pub canonical: fn(&Path) -> PathBuf, pub home: PathBuf }`.
    - `Deps::default()` has no grammar and the identity as `canonical`. With it, `suggest`
      with actions is refused with `suggestions need the moactl grammar`, and `Startup`
      skips re-validation, so 2a's tests, which have no grammar, never flag anything.
    - `State::with_deps(self, deps) -> State`, applied before `Startup`.
    - `project add` now canonicalises its path with `deps.canonical` before checking it, so
      a path typed at the CLI and one in a suggestion action go through the same step.
  - `moa_testkit::brain::TypicalBrain::state_with(&self, deps: Deps) -> State`.
  - New `Op`s:
    ```rust
    #[serde(rename = "suggest")]           Suggest { text: String, actions: Vec<String> },
    #[serde(rename = "suggestion.answer")] SuggestionAnswer { id: Option<i64>, answer: Answer },   // Answer: Yes | No | Later
    #[serde(rename = "suggestion.ls")]     SuggestionLs,
    ```
  - Behaviour (spike `suggest`/`suggestion`, R1 §2.9, spec §6.12):
    - **suggest**:
      - Each action is split with `shell_words` into `{verb, args}` and checked with
        `parse_action`. The first failure → `--do '<action>': <reason>` (a split error's
        reason is its message), and nothing is applied.
      - An open suggestion with the same text → text `already suggested (#<id>)`, no change.
      - Otherwise insert `{status: open, actions, actions_raw: None, created_at: now}` and
        activity `suggests(id, text)`. Text `suggestion #<id>: the user sees it with yes /
        later / no buttons` (it's said to moa); data the row.
    - **suggestion ls**: open and later, by id.
      - Line: `#{id} [{status}] {text}`.
      - Then `  (yes runs: {a}; {b})`, each action shown as `shell_words::join([verb] + args)`.
      - Or, for a flagged suggestion, `  (yes can't run its actions: {actions_invalid})`.
    - **suggestion yes|no|later**:
      - No id, or no such suggestion → `moactl suggestion yes|no|later ID`.
      - Not open or later → `suggestion #<id> is already <status>`.
      - **yes** on a flagged suggestion → `suggestion #<id> has actions moa can't run; answer
        no or later`.
      - **yes** runs each action in order, **in-process**, through the same `ops::run` with
        the same `Tx`. A failure is recorded as `<joined action>: <error line>` and the next
        action still runs; `NotYet(v)` is recorded as `<joined action>: <v> isn't in this
        build yet`.
      - Then: status (yes → accepted, no → rejected, later → later), `resolved_at = now`,
        `result` (`done`, or the failures joined with `; `).
      - ui event: `{status} suggestion #{id}: {text}`, plus ` (failed: {…})` when anything
        failed.
      - Text: `done`, `rejected` or `later`, plus `; but {failures}`.
      - Yes without actions only records it; the signal to moa is slice 6.
    - **At `Startup`** (not `Reload`, which only replaces the state; 2a Task 9): every open or later suggestion that isn't
      flagged and has actions is re-checked with `parse_action`. A failing one gets
      `actions_invalid = "an action doesn't parse as moactl"` (committed, so it persists),
      plus a `Log` naming its id.

- [ ] **Step 1: Write the failing tests** (`tests/suggestions.rs`, on
  `TypicalBrain::state_with(Deps { parse_action: Some(moa_proto::cli::parse_action), canonical: |p| p.to_path_buf(), home })`)
  - `startup_flags_the_action_the_spike_could_not_run`:
    - #1 (`task 1 --status done`) gets `actions_invalid` set, and the `Startup` effects hold
      that change in a `Persist`.
    - #2 and #4 stay valid.
    - `yes 1` → `suggestion #1 has actions moa can't run; answer no or later`.
  - `imported_suggestion_two_runs`: `yes 2` → `done`; S1's title is `New name` (its action
    names S1 by conversation prefix); #2 is accepted, with `resolved_at` set.
  - `suggest_validates_and_refuses_duplicates`:
    - `suggest "Start the board?" --do "task set 1 --status active"` → `suggestion #5: the
      user sees it with yes / later / no buttons`, with activity `suggests #5: Start the
      board?`;
    - the same text again → `already suggested (#5)`, no `Persist`;
    - `--do "rm -rf x"` → `--do 'rm -rf x': a suggestion can run moactl archive, close,
      group, note, project, set, task`;
    - `--do "task 1 --status done"` → starts with `--do 'task 1 --status done': invalid
      value '1'`.
  - `yes_runs_every_action_in_one_transaction`:
    - Suggest with `--do "task set 1 --status active"`, `--do "note 'board ordered'
      --project Lantern"`, `--do "task set 99 --status done"` and `--do "close 3f9a1c52"`.
    - `yes` → `done; but task set 99 --status done: no task #99; close 3f9a1c52: close
      isn't in this build yet`.
    - Task 1 is active and the note exists.
    - The effects are **one** `Persist` (task, note, activity rows, suggestion, ui event),
      then `Publish`, then `Reply`.
  - `an_action_path_is_canonicalised_like_a_typed_one`: `Deps.canonical` maps
    `<home>/link` → `<home>/private`; a suggestion `--do "project Shore --add <home>/link"`,
    then `yes` → the project's path is `<home>/private`.
  - `no_and_later_and_already_answered`.
  - `suggestion_ls_matches_the_spike_layout` (insta: #1 flagged, #2 with `(yes runs: set
    3f9a1c52 --title 'New name')`, #4 later).
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: suggestions: validated by the moactl grammar, yes runs actions in-process"`
- [ ] **Step 6: Open PR b1** (Tasks 1–3), with `/code-review medium --fix`. Start Task 4
  from `main` once it merges.

---

### Task 4: `moa-daemon`: the core task with group commit, the scheduler, supervision

**Files:**
- Create: `crates/moa-daemon/Cargo.toml`, `src/lib.rs`, `src/clock.rs`, `src/core_task.rs`,
  `src/scheduler.rs`, `src/supervise.rs`, `src/testing.rs`, `tests/core_task.rs`,
  `tests/idle.rs`
- Modify:
  - `Cargo.toml`: workspace `tokio-util = { version = "0.7", features = ["codec", "rt"] }`
    and `tracing = "0.1"`.
  - `crates/moa-core/src/state.rs`: `State::status(&self, &CallerInfo) -> StatusData`.

**Interfaces:**
- Consumes: `handle`, `check_order`, `Effect::runs_at_once`, `Input`, `TimerKey`,
  `start_of_next_day` (2a); `StoreHandle`, `CommitError`, `Fault` (2a Task 11);
  `WallTimer`, `spawn::launches` (2a Tasks 2–3); `Deps` (Task 3).
- Produces:
  ```rust
  // clock.rs
  pub trait Clock: Send + Sync + 'static { fn wall(&self) -> UtcMs; fn mono(&self) -> Duration; }
  pub struct SystemClock;                         // SystemTime + an Instant taken at start
  // testing.rs (always compiled; small)
  pub struct FakeTime;                            // Arc-shared wall time
  impl FakeTime { pub fn new(at: UtcMs) -> Arc<FakeTime>; pub fn advance(&self, by: Duration); pub fn alarm(self: &Arc<Self>) -> FakeAlarm; pub fn armed(&self) -> Option<UtcMs>; }
  impl Clock for FakeTime;  pub struct FakeAlarm; impl WallTimer for FakeAlarm;   // fires when advance() passes the deadline
  // core_task.rs
  pub const DRAIN_CAP: usize = 64;
  pub struct CoreOptions { pub brain: BrainName, pub config: Config, pub deps: Deps, pub tz: TimeZone, pub clock: Arc<dyn Clock>,
                           pub alarm: Box<dyn WallTimer>, pub db: PathBuf, pub backups: PathBuf, pub events_capacity: usize /* 256 */ }
  pub struct StatusData { pub brain: String, pub sessions: BTreeMap<String, usize>, pub caller: String }   // in moa-core
  pub struct Stats { pub inputs: AtomicU64, pub drains: AtomicU64, pub commits: AtomicU64, pub timer_fires: AtomicU64,
                     pub commit_failures: AtomicU64, pub reloads: AtomicU64 }
  pub struct CoreHandle { /* Clone */ }
  impl CoreHandle {
      pub async fn start(opts: CoreOptions) -> Result<CoreHandle, DaemonError>;   // Store::open, StoreHandle::spawn, snapshot, Startup
      pub fn open_conn(&self) -> (ConnId, mpsc::UnboundedReceiver<(ReqId, Result<Done, OpError>)>);
      pub fn close_conn(&self, conn: ConnId);
      pub fn send(&self, conn: ConnId, req: ReqId, caller: CallerInfo, op: Op);
      pub async fn status(&self, caller: CallerInfo) -> StatusData;   // an inbox item, answered in drain order
      pub fn zone_changed(&self, tz: TimeZone);
      pub fn model(&self) -> watch::Receiver<Arc<Model>>;
      pub fn events(&self) -> broadcast::Receiver<CoreEvent>;    // capacity: events_capacity
      pub fn generation(&self) -> watch::Receiver<u64>;          // bumps on every reload
      pub fn stats(&self) -> Arc<Stats>;
      pub async fn barrier(&self);                               // an inbox item; answers after its drain's commit and a StoreHandle::sync()
      pub async fn stop(&self);                                  // an inbox item; its drain commits and releases first, then the loop ends
      #[doc(hidden)] pub async fn start_with_store(opts: CoreOptions, store: Store) -> Result<CoreHandle, DaemonError>;  // tests
      #[doc(hidden)] pub fn store_fault(&self, f: Fault);        // tests
      #[doc(hidden)] pub fn panic_next_handle(&self);            // tests (Task 5): the next input panics inside catch_unwind
  }
  // supervise.rs
  pub async fn supervise<F, Fut>(name: &'static str, make: F, failed: impl Fn(&'static str, String) + Send + 'static)
      where F: FnMut() -> Fut + Send + 'static, Fut: Future<Output = ()> + Send + 'static;
  pub enum DaemonError { /* Task 6 adds the run-time variants */ Store(StoreError), Io(io::Error) }
  ```
- **The core task loop (2a call S4, the happy path).** Repeat:
  1. Wait for an inbox item: client inputs, `status`, `barrier`, `stop`, zone changes,
     scheduler fires. The drain first takes the items in the local carry-over queue (a
     `VecDeque`, Task 5), in order, then what else is already waiting with `try_recv`, up
     to `DRAIN_CAP` in all. That's one drain; `stats.drains += 1`.
  2. For each input, in order:
     - build `Now::at(clock.wall(), clock.mono(), tz.clone())`;
     - run `handle` (Task 5 wraps it in `catch_unwind`). In debug builds, assert
       `check_order`. `stats.inputs += 1`.
     - From its effects:
       - `Persist` changes are appended to the drain's batch;
       - the `runs_at_once` effects run now;
       - `Backup { day }` is started on a spawned task, and its result is logged as
         counts;
       - every other effect is appended to the drain's held list.
     - A `status` item computes `State::status` and holds its answer like a reply.
  3. If the batch is non-empty: `stats.commits += 1`, and await `store.commit(batch)`.
     On `Ok`, release the held list in order. With no batch, release it at once.
  4. Releasing:
     - `Publish(Model)` → `watch`;
     - `Publish(Event)` → `broadcast`;
     - `Reply` → that connection's channel (dropped silently if it's gone);
     - `barrier` → `store.sync()`, then answer;
     - `stop` → end the loop after this drain. A `stop` ends the drain like a panic does
       (the items after it are carried over). Every item still carried over or in the
       inbox then gets `moa is stopping; try again`, and the per-connection senders drop,
       so no client waits for the process to exit.

  Inputs that arrive during the commit wait in the inbox and make up the next drain.
  That's where batching comes from.
- **The scheduler** keeps `BTreeMap<TimerKey, UtcMs>`, arms the `WallTimer` at the
  earliest, and on each `fired()` sends `Timer(key)` for every key whose deadline ≤
  `clock.wall()` (`stats.timer_fires += 1` each). It runs under `supervise`.
- **`supervise`** restarts `make()` after a panic (`JoinError::is_panic`), calling `failed`
  with the panic message's first line, which sends `ActorFailed`. The restart delay is 1 s,
  doubling to 30 s. A normal return isn't restarted.

- [ ] **Step 1: Write the failing tests**
  - `core_task.rs` (`#[tokio::test]`, `TypicalBrain`, `FakeTime` at `TYPICAL_NOW`; every
    wait is wrapped in a 5 s `timeout`):
    - `the_reply_waits_for_the_commit`: send `group add Work`. When the reply arrives, a
      separate read-only connection already sees the group, and `model()` already has it.
    - `a_read_during_a_pending_commit_waits_for_it`:
      1. `store_fault(PauseNextCommit(rx))`.
      2. Send `task add Lantern Solder` on connection A. Wait until `stats.commits` has
         moved by one (Startup may already have committed).
      3. Send `task ls` on connection B. B has no reply yet.
      4. Release `rx`. A's reply arrives, then B's, and B's text lists `Solder`.
    - `writes_arriving_during_a_commit_share_the_next_one`: `PauseNextCommit`; one
      `group add A`, then (while paused) 10 more `group add`s. Release → all 11 answer `Ok`,
      and `stats.commits` moved by exactly 2.
    - `a_drain_without_writes_answers_at_once`: with `PauseNextCommit` set but no write
      sent, `brief` and `task ls` answer (nothing is in flight).
    - `stop_waits_for_the_batch_in_flight`: `PauseNextCommit`; send `group add Work`; call
      `stop()`. Release → the `group add` reply is `Ok` and arrives before `stop()`
      returns, and the group is on disk.
    - `a_zone_change_moves_the_backup_deadline`: `zone_changed(UTC0)` → `FakeTime::armed()`
      is the next UTC midnight.
  - In `supervise.rs`, `a_panicking_actor_restarts_and_is_reported`: a `make` that panics on
    its first run → `failed("x", "boom")` is called once, and the second run completes
    (tokio's time paused for the 1 s delay).
  - `idle.rs`, `an_idle_brain_wakes_only_for_the_backup`:
    1. `FakeTime` at `TYPICAL_NOW`; start the core; no clients.
    2. `advance(1 h)` 24 times, each followed by `barrier().await`.
    3. Not counting the barriers: `stats.inputs == 2` (Startup and the midnight
       `Timer(Backup)`), and `stats.timer_fires == 1`.
    4. `backups/` has `daily-2026-11-15.db` (at startup) and `daily-2026-11-16.db`.
    5. `moa_platform::spawn::launches()` is unchanged.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: daemon core task: group commit with one batch in flight, wall scheduler, supervise"`

---

### Task 5: The core task's failures: failed commits, a dead store, panics

**Files:**
- Modify:
  - `crates/moa-daemon/src/core_task.rs`.
  - `crates/moa-core/src/op.rs`:
    `#[doc(hidden)] #[cfg_attr(feature = "schema", schemars(skip))] Op::DebugPanic`, kept
    out of `Op::METHODS` so the wire can't reach it.
- Test: `crates/moa-daemon/tests/failures.rs`

**Interfaces:**
- Consumes: Task 4's loop; `Input::Reload { snapshot, refuse_writes }` and `RefuseWrites`
  (2a Task 9); `CommitError` (2a Task 11).
- Produces these rules in the loop (2a call S4):
  - **A failed commit** (`CommitError::Failed(e)`):
    1. Every held `Reply` of that drain fails with `moa couldn't save that (<e>); nothing
       was changed, try again`.
       - That covers writes and reads, since a read may have seen the failed state.
       - Held `Publish` and external effects are dropped.
       - Held `status` items are computed again after the reload, and held `barrier`
         items still answer, so neither shows the failed state.
    2. `stats.commit_failures += 1`, `failures_in_row += 1`, and
       `Error { "moa couldn't save a change; it reloaded from disk" }` is published.
    3. **Reload:**
       - `store.snapshot()`;
       - `handle(Input::Reload { snapshot, refuse_writes })`, which publishes the whole
         model;
       - `generation += 1`, `stats.reloads += 1`.

       `refuse_writes` is `None` while `failures_in_row < 3`. From the third failure in a
       row it's `Some(RefuseWrites { until: now + min(1 s × 2^(failures_in_row − 3),
       30 s), reason: e })`.
    4. A successful commit resets `failures_in_row = 0`.
       - Refusal ends by itself: the core compares `until` with `Now` (2a Task 9), so the
         first write after it probes the store.
       - No input and no reload is needed to lift it.
    5. The next drain runs against the reloaded state.
  - **A dead store** (`CommitError::Stopped` from `commit`, `snapshot` or `sync`):
    1. Every held `Reply` of the drain fails with `moa couldn't save that: the store
       stopped`.
    2. Held `Publish` and external effects are dropped, as for a failed commit.
       Reopen `Store::open` and spawn a new `StoreHandle`. If `Store::open` fails: log
       it, publish `the store can't be opened: <line>` once, and stop the daemon with that
       line (exit 1).
    3. Reload as above (it doesn't count toward `failures_in_row`), then send
       `ActorFailed { actor: "the store", … }`.
  - **A panic in `handle`**: each call runs inside `catch_unwind(AssertUnwindSafe(…))`.
    - The panicking input's own reply is `moa hit a bug answering that; it reloaded`.
    - `handle` returns its effects only when it finishes, so **a panicking input's partial
      changes never reach the batch**. Its `Tx` buffer is dropped with the unwound call.
    - **The drain stops at the panic:**
      - the inputs before it are still committed (their changes are already in the batch)
        and released as usual;
      - the inputs after it go into the local carry-over queue, in order (with any
        `stop` or `barrier` among them), which the next drain empties before it calls
        `try_recv`.
    - After that commit, reload (as above, not counted as a failure), so the panicking
      input's partial in-memory changes are gone. `Error { "moa hit a bug and reloaded" }`
      is published.
    - **A panic while handling `Startup` or `Reload`** isn't retried:
      - log it and publish the error once;
      - stop the daemon with `moa hit a bug loading the brain; see the log` (exit 1).

      launchd, or the next autostart, starts it again with its own delay.

- [ ] **Step 1: Write the failing tests** (`tests/failures.rs`; same setup as Task 4; 5 s
  timeouts)
  - `a_failed_commit_fails_the_reply_and_reloads`:
    - `FailNextCommit`; `group add Work` → `Err` starting `moa couldn't save that (`.
    - An `Error` event is published, and `generation` moved on.
    - Then `group rename Work X` → `no group Work` (the in-memory state was reloaded), and
      `brief` still answers.
  - `writes_queued_behind_a_failed_commit_fail_too`:
    1. `PauseNextCommit(rx)`; send `group add Primer` (drain 1, now paused).
    2. Queue `FailNextCommit`, then send `task add Lantern Solder` and `task ls`. Wait
       until both are in the inbox (`stats.inputs` unchanged, so they're queued, not run).
    3. Release `rx`.
       - Drain 1 commits (`Primer` answers `Ok`).
       - Drain 2 holds both queued inputs. Its commit fails, so both get the retry line,
         the read too.
    4. Then `task set 6 --status done` → `no task #6` (it runs against the reloaded state),
       and `task ls` shows no `Solder`.
  - `a_commit_that_keeps_failing_backs_off_and_refuses_writes`:
    1. Start with `start_with_store`, passing a `Store` with `PRAGMA query_only = 1`, and
       `Deps` **with** the grammar (so `Startup` itself writes and fails).
    2. Startup's flagging commit is failure 1. Two `group add`s fail with the retry line
       (failures 2 and 3).
    3. The third `group add` gets `moa can't save right now (<reason>); nothing was
       changed`, and `stats.commits` didn't move.
    4. Reads keep answering, and `stats.reloads == 3`.
    5. `FakeTime::advance(1 s)` → the next `group add` is tried (`stats.commits` moves) and
       fails, and the next refusal lasts 2 s.
  - `the_store_thread_is_restarted`:
    - `store_fault(Crash)`, then immediately `group add Work`.
    - Within 5 s it either fails with `moa couldn't save that: the store stopped` or
      succeeds; it never hangs.
    - A following `group add Other` succeeds, and an `Error` event says `the store stopped
      and was restarted`.
  - `a_panicking_handle_is_reported_and_the_core_keeps_serving`: send `Op::DebugPanic` →
    `moa hit a bug answering that; it reloaded`; an `Error` event; `generation` moved on;
    `brief` still answers.
  - `a_panic_in_the_backup_timer_keeps_it_armed`: with a `#[doc(hidden)]
    CoreHandle::panic_next_handle()` hook (the wrapper panics inside `catch_unwind` before
    calling `handle` for the next input), advance `FakeTime` to midnight so
    `Timer(Backup)` is that input → it panics, the core reloads, and `FakeTime::armed()` is
    the following midnight.
  - `a_panic_mid_drain_keeps_the_earlier_writes`:
    1. `PauseNextCommit(rx)`; send `group add Primer`.
    2. Queue `group add Kept`, `DebugPanic` and `group add After`.
    3. Release.
       - `Kept` answers `Ok` and is on disk.
       - `DebugPanic` gets the bug line.
       - `After` answers `Ok` too, in a later drain, after the reload.
    4. The model has `Kept` and `After`, and nothing else changed.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: core task failures: failed commits reload with backoff, a dead store restarts, panics are caught"`

---

### Task 6: The daemon process: lock, socket, client sessions, logs, `moa daemon`

**Files:**
- Create: `crates/moa-daemon/src/run.rs`, `src/server.rs`, `src/session.rs`,
  `src/logging.rs`, `tests/server.rs`
- Modify:
  - `crates/moa/src/main.rs`.
  - `crates/moa/Cargo.toml`: `moa-daemon`, `moa-proto`, `moa-platform`, `tokio`.
  - `Cargo.toml`: workspace `futures-util`, `tracing-subscriber`.
  - `crates/moa-platform/src/{lib.rs, sys/*}`:
    `pub fn stdout_to_null() -> io::Result<()>` (`dup2` of `/dev/null` onto fd 1, in
    `sys`).

**Interfaces:**
- Consumes: Task 4's `CoreHandle`; Task 1's wire functions; `ensure_private_dir`,
  `check_v1_allowed`, `Config::from_file_text` (2a).
- Produces:
  ```rust
  pub enum Ready { Stdout, Channel(oneshot::Sender<PathBuf>), Nothing }
  pub struct RunOptions { pub paths: Paths, pub brain: BrainName, pub config: Config, pub tz: TimeZone, pub clock: Arc<dyn Clock>,
                          pub alarm: Box<dyn WallTimer>, pub zone: Option<ZoneWatch>, pub ready: Ready, pub events_capacity: usize,
                          #[doc(hidden)] pub uid_override: Option<u32> }   // tests: pretend to be another uid
  pub async fn run(opts: RunOptions) -> Result<(), DaemonError>;
  pub enum DaemonError { AlreadyRunning(BrainName), NoSuchBrain(BrainName, PathBuf), Paths(PathsError), Store(StoreError), Io(io::Error) }
  pub fn peer_allowed(peer_uid: u32, my_uid: u32) -> bool;
  pub async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send>(stream: S, core: CoreHandle, brain: BrainName, stop: CancellationToken);
  ```
  - `DaemonError` texts:
    - `the {b} brain's moa is already running`;
    - `there's no brain named {b} in {config file path}`;
    - otherwise the inner error's line (`UnsafeDir` already carries its `chmod 700` hint).
  - **`run`, in order:**
    1. The brain must be in the config (`NoSuchBrain`).
    2. `ensure_private_dir` for `run/` (or the `$TMPDIR` fallback's directory) and `logs/`.
    3. `File::try_lock` on `lock(b)`: `WouldBlock` → `AlreadyRunning`.
    4. Holding the lock, remove whatever is at `socket(b)` and bind it.
    5. Start logging, then `CoreHandle::start`. Log the config's `skipped_tables` as one
       warning.
    6. Signal `Ready`. With `Stdout`, print `ready\n`, flush, then `stdout_to_null()`. A
       failure before this point prints `error: <its line>\n` instead (or
       `already running\n`), then returns.
    7. Accept until stopped (the accept loop runs under `supervise`). A connection whose
       `peer_allowed(peer_cred().uid(), uid_override.unwrap_or(getuid))` is false is closed
       and logged.
    8. SIGTERM/SIGINT/`shutdown`/`upgrade` → stop accepting, `core.stop()`, remove the
       socket file, return `Ok`. SIGHUP is logged and ignored.
  - **`serve`** (one connection; `LinesCodec::new_with_max_length(MAX_LINE)`):
    - A `hook` notification is accepted before or without `hello`, never answered, and
      dropped (D5).
    - Any request before `hello` → `HELLO_FIRST` `say hello first`.
    - `hello` → `HelloOk { matched: build == moa_platform::build_id() }`. When not matched,
      only `allowed_on_mismatch` calls run; the rest get `BUILD_MISMATCH` `this moa is a
      different build; run the command again to restart it`.
    - `status` → `core.status(caller)`, plus `build`, `pid` and `spawn::launches()`.
    - `shutdown`/`upgrade` reply `{}` and then stop the daemon.
    - `subscribe` → a `SnapshotMsg` result (D4). Then:
      - whenever `model()` changes, a `patch` notice (`Patch::between`);
      - each event, an `event` notice;
      - a broadcast `Lagged`, or a `generation()` change (a reload), sends a `snapshot`
        notice instead.
    - An `Op` → `core.send`, and its reply is encoded when it arrives.
    - A line over `MAX_LINE` → `TOO_LONG` `that message is too long`, then close.
  - **Logging**: `tracing-subscriber` fmt, no ANSI, appending to `paths.log(b)`; the level
    from `MOA_LOG` (default `info`). Lines name ids and methods, never record text (S15).
  - **`moa daemon [--brain B] [--notify-ready]`** (`--brain` defaults to `default`):
    - `Paths::from_env()`, then `check_v1_allowed()` (D20).
    - `Config::from_file_text(paths.read_config()?, home, moa_platform::canonical)`.
    - `TimeZone::try_system()` (D12); `WallAlarm::new()`; `ZoneWatch::system()` (on
      failure, log and run without it); a zone change → `core.zone_changed(TimeZone::try_system())`.
    - `Deps { parse_action: Some(moa_proto::cli::parse_action), canonical: moa_platform::canonical, home }`;
      `SystemClock`.
    - Any error before `ready` is printed (on stdout as `error: …` under `--notify-ready`,
      else on stderr) and exits 1. `AlreadyRunning` exits 0 (D3).

- [ ] **Step 1: Write the failing tests** (`tests/server.rs`)
  - Each test runs an in-process `run` on a `/tmp` short root. The DB is put in place with
    `moa_store::backup::vacuum_into(TypicalBrain::store().conn(), &paths.db("default"))`,
    with `FakeTime` and `Ready::Channel`.
  - A tiny raw client writes and reads JSON lines over a std `UnixStream`.
  - `hello_then_ops_over_the_socket`: hello → `matched: true`; `task.ls` → its text has
    `## Lantern`.
  - `the_first_message_must_be_hello`.
  - `a_hook_line_alone_is_accepted`: connect, write one `hook` line, close. A new
    connection's hello and `status` work.
  - `a_mismatched_build_may_only_hello_status_upgrade_shutdown_and_hook`:
    - hello with build `"x"` → `matched: false`;
    - `task.ls` → `BUILD_MISMATCH`;
    - `status` works;
    - a `hook` notice gets no reply, and the next `status` still works.
  - `subscribe_sends_a_snapshot_then_patches`: subscribe `[tasks]` → 5 rows. A second
    connection runs `task.add` → the first gets a `patch` with 1 upsert and `rev + 1`.
  - `a_lagging_subscriber_gets_a_fresh_snapshot`:
    - `serve` runs over `tokio::io::duplex(1024)` with a core started with
      `events_capacity: 4`.
    - The test subscribes and stops reading while another connection makes 10
      `group.add` calls.
    - When it reads again, a `snapshot` notice arrives.
    - (A real socket's kernel buffer would absorb the events and hide the lag.)
  - `garbage_and_oversized_lines_get_an_error_not_a_crash`:
    - `not json` → `PARSE`;
    - a 9 MiB line → `TOO_LONG` and the connection closes;
    - a new connection still works.
  - `another_uid_is_turned_away`: `uid_override: Some(my_uid + 1)` → a connection is
    closed with no reply. The unit test `peer_allowed(501, 501) && !peer_allowed(502, 501)`
    sits beside it.
  - `a_second_daemon_says_already_running`: a second `run` on the same root →
    `AlreadyRunning`.
  - `a_stale_socket_file_is_replaced`: a regular file at the socket path before `run` → it
    starts and answers.
  - `the_socket_dir_is_private`: the created `run/` is `0700`. A pre-made `run/` at `0777` →
    `Paths(UnsafeDir)`, `…is open to other users; moa won't use it (chmod 700 …)`.
  - `shutdown_stops_cleanly`: `shutdown` → `{}`, `run` returns `Ok`, the socket file is
    gone. Same for `upgrade`.
  - `the_log_names_no_record_contents`: after `task.add` with the title `SECRET-TITLE-7`,
    the log file doesn't contain it.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa daemon: lock, private socket, hello-free hooks, build match, subscribe, logs"`
- [ ] **Step 6: Open PR b2** (Tasks 4–6), with `/code-review medium --fix`. Start Task 7
  from `main` once it merges.

---

### Task 7: `moa-client`, autostart, and `TempBrain`

**Files:**
- Create: `crates/moa-client/Cargo.toml`, `crates/moa-client/src/lib.rs`,
  `crates/moa-testkit/src/temp_brain.rs`, `crates/moa/tests/client.rs`
- Modify: `crates/moa-testkit/{Cargo.toml, src/lib.rs}` (`moa-platform`, `serde_json`),
  `crates/moa/Cargo.toml` (`moa-client`)

**Interfaces:**
- Produces:
  ```rust
  // moa_client
  pub enum OnMismatch { Restart, Accept }
  pub struct ClientOptions { pub paths: Paths, pub brain: BrainName, pub client: &'static str, pub caller: CallerInfo,
                             pub autostart: Option<PathBuf>, pub on_mismatch: OnMismatch }
  pub struct Client { /* Framed<UnixStream, LinesCodec>, next id, HelloOk */ }
  impl Client {
      pub async fn connect(opts: ClientOptions) -> Result<Client, ClientError>;
      pub fn hello(&self) -> &HelloOk;
      pub async fn call(&mut self, op: Op) -> Result<Done, ClientError>;
      pub async fn status(&mut self) -> Result<StatusOk, ClientError>;
      pub async fn shutdown(&mut self) -> Result<(), ClientError>;
      pub async fn subscribe(&mut self, topics: &[Topic]) -> Result<SnapshotMsg, ClientError>;
      pub async fn next_notice(&mut self) -> Result<(String, serde_json::Value), ClientError>;
  }
  pub enum ClientError { NotRunning(BrainName), Start(String), Paths(PathsError), Rpc(RpcError), Io(io::Error), Closed }
  pub enum ReadyLine { Ready, TryAgain, Failed(String) }
  pub fn read_ready(first_line: Option<&str>) -> ReadyLine;     // "ready" | "already running" or EOF | "error: …"
  pub fn daemon_env(parent: impl Iterator<Item = (OsString, OsString)>, paths: &Paths) -> Vec<(OsString, OsString)>;   // D17
  // moa_testkit::temp_brain
  pub struct TempBrain { pub dir: TempDir }
  impl TempBrain {
      pub fn new(moa_toml: Option<&str>) -> TempBrain;   // a /tmp short root; writes <root>/.config/moa/moa.toml when given
      pub fn root(&self) -> &Path;  pub fn paths(&self) -> Paths;  pub fn db(&self, brain: &str) -> PathBuf;
      pub fn env(&self) -> Vec<(OsString, OsString)>;   // MOA_ROOT, HOME (= root), TZ (TYPICAL_TZ), PATH
      pub fn adopt_typical(&self, brain: &str);         // Typical fixture under <root>/spike-home, imported into db(brain)
  }
  impl Drop for TempBrain { /* below */ }
  ```
  - `ClientError` texts:
    - `the {b} brain's moa isn't running`;
    - `couldn't start moa: {line} (log: {stderr log path})`;
    - `Paths`: the inner line (the `MOA_ROOT` gate, or a directory open to others);
    - the `RpcError` message;
    - `moa closed the connection`.
  - **connect** (D1, D2):
    1. Connect. On `NotFound`/`ConnectionRefused` with `autostart: Some(exe)`:
       `paths.check_v1_allowed()`, `ensure_private_dir` for `run/` and `logs/`, take
       `File::lock` on `start_lock(b)`, and connect once more.
    2. If that fails too, spawn through `Spawner`: `exe daemon --brain b --notify-ready`,
       with `env_clear().envs(daemon_env(…))`, `current_dir(paths.state_dir())`,
       `process_group(0)`, stdin null, stdout piped, stderr appended to `stderr_log(b)`.
    3. `read_ready` on its first stdout line, within 10 s: `Ready` → connect; `TryAgain` →
       connect once more; `Failed(line)` → `Start(line)`. Then drop the start lock.
    4. Send `hello`. When it's not matched and `OnMismatch::Restart`:
       1. send `upgrade` and read to EOF;
       2. block (≤ 10 s, on a helper thread) until `File::lock(lock(b))` succeeds, then
          release it;
       3. autostart as above.

    `exe` is `fs::canonicalize(current_exe())`, so a `moactl` symlink never ends up as the
    daemon's `argv[0]`.
  - **`daemon_env`** (D17) sets `HOME` from `paths.home()`, and `MOA_ROOT` to the same when
    `paths.is_sandboxed()`. It never copies either from the parent. From the parent it keeps
    `PATH`, `TMPDIR`, `USER`, `LANG`, `LC_*` and `MOA_LOG`; `TZ` only when sandboxed; and
    `MOA_REAL_HOME` only when not sandboxed. So an in-process client in a test (whose own
    environment has the developer's real `HOME`) still starts its daemon in the sandbox.
  - **`TempBrain::drop`**, for each `run/*.sock`:
    1. Over a std `UnixStream`, write a hello line, then a shutdown line (`shutdown` is
       allowed on a mismatched build).
    2. Wait up to 3 s (`moa_testkit::wait_for`) until `lock(b)` can be `try_lock`ed.
    3. If it can't be, `kill` the pid that `status` reported.

    Errors are ignored.

- [ ] **Step 1: Write the failing tests**
  - Unit tests in `moa-client`:
    - `the_ready_line`: `Some("ready")` → `Ready`; `Some("already running")` and `None` →
      `TryAgain`; `Some("error: moa.toml: …")` → `Failed("moa.toml: …")`.
    - `the_daemon_gets_a_clean_env`. The parent env has `MOA_SESSION`,
      `CLAUDE_CONFIG_DIR`, `TMUX`, `TMUX_PANE`, `TZ`, `MOA_REAL_HOME=1`, `HOME=/real`,
      `PATH` and `LC_ALL`.
      - With `Paths::with_root("/r")`: exactly `HOME=/r`, `MOA_ROOT=/r`, `TZ`, `PATH` and
        `LC_ALL`.
      - With unsandboxed paths whose home is `/home/u`: exactly `HOME=/home/u`,
        `MOA_REAL_HOME=1`, `PATH` and `LC_ALL`.
  - `crates/moa/tests/client.rs` (using `cargo_bin("moa")` as the autostart exe, and a
    `TempBrain`):
    - `a_fresh_root_autostarts`: a root with no `.local/` at all. `Client::connect` with
      autostart → `hello` matched; `run/` ends up `0700`; `status().launches == 0` (the
      daemon itself launched nothing).
    - `a_daemon_that_cannot_start_says_why`: a `moa.toml` with an error →
      `ClientError::Start` whose text is `couldn't start moa: moa.toml: … (log: …)`.
    - `no_autostart_means_not_running`: `autostart: None` on a fresh root → `the default
      brain's moa isn't running`.
    - `temp_brain_stops_its_daemons`: drop a `TempBrain` whose daemon is up → afterwards
      that brain's lock is free.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-client with autostart (clean env, ready line, upgrade wait); TempBrain"`

---

### Task 8: The CLI front: `moactl`, `moa status|shutdown`, the offline tools' guard, the flake

**Files:**
- Create: `crates/moa/src/ctl.rs`, `crates/moa/tests/ctl.rs`
- Modify: `crates/moa/src/main.rs`, `crates/moa/src/adopt.rs` (D14), `flake.nix` (D13)

**Interfaces:**
- Consumes: Task 7's `Client`; Task 2's `Moactl`; `route`, `caller_store`,
  `Config::from_file_text` (2a).
- Produces:
  - **`main.rs` dispatch**:
    - The basename of `argv[0]` is `moactl` → `Moactl::parse()`.
    - Else `argv[1]` is in `VERB_NAMES` → `Moactl::parse_from(["moactl", argv[1..]…])`.
    - Else the `moa` subcommands: `adopt-spike`, `export-spike`, `daemon`, `sql` (Task 9),
      `service` (Task 10), `status [--brain]`, `shutdown [--brain]`.
    - A bare `moa` keeps its slice-0 behaviour.
  - **`ctl::run(m: Moactl) -> ExitCode`**:
    1. `to_op(VerbCtx { cwd, home })`. `NotYet` and `Usage` print their line and exit 1,
       with no connect.
    2. Read the config with `Config::from_file_text(…, canonical)`, the same call the
       daemon makes.
    3. Pick the brain (D18):
       - `MOA_BRAIN` if set;
       - else `route(config, account_for_store(canonical(caller_store(CLAUDE_CONFIG_DIR, home))),
         canonical(cwd), None)`. `NotAvailable` → `moa isn't available here`.
       - A brain missing from the config → `there's no brain named {b} in {config file path}`.
    4. `CallerInfo` from `MOA_SESSION`, `CLAUDE_CODE_SESSION_ID` and the cwd.
    5. `Client::connect(autostart, Restart)`.
    6. `call` → print `text` (with `--json`, `data` as one line of JSON); exit 0. `Err` →
       its line on stderr, exit 1.
  - **`moa status [--brain B]`** prints three lines:
    - `moa {brain}: running, build {build}, pid {pid}, {launches} launches`;
    - `you are: {caller}`;
    - `sessions: {word} {n}, …` in display-state order, zeros left out, or `sessions: none`.

    It doesn't autostart; not running → the `NotRunning` line, exit 1.
  - **`moa shutdown [--brain B]`** doesn't autostart either; not running → the same line,
    exit 0.
  - **D14**:
    - `adopt-spike --into` and `export-spike --from` refuse a path that is `paths.db(b)`
      for a brain `b` whose `lock(b)` is held: `the {b} brain's moa is running; stop it
      first with: moa shutdown --brain {b}`.
    - `adopt-spike --into paths.db(b)` creates `brain_dir(b)` with mode `0700` when it's
      missing. Any other missing folder is still refused as before.
  - **Flake**: `packages.default` gets `postInstall = "ln -s moa $out/bin/moactl";`, and add
    `apps.moactl = { type = "app"; program = "${self.packages.${system}.default}/bin/moactl"; }`.

- [ ] **Step 1: Write the failing tests** (`crates/moa/tests/ctl.rs`)
  - Every command is `assert_cmd` with `.env_clear().envs(tb.env())` and a `current_dir`
    under the root. `moactl` is a symlink to `cargo_bin("moa")` made in the root.
  - `moactl_autostarts_and_answers`: on `TempBrain::adopt_typical("default")`, `moactl
    brief` → exit 0, the stdout starts with `# Brief (`, and `moa status` shows a pid.
    Then a second `moactl task ls` gives the same pid.
  - `moa_verb_equals_moactl`: `moa task ls` and `moactl task ls` print the same text.
  - `racing_clients_start_one_daemon`: 4 `moactl brief` at once on a fresh `TempBrain` →
    all exit 0; afterwards `moa status` shows one pid, and the `run/` dir has one socket.
  - `without_moa_root_nothing_starts`: `.env_clear()` with only `HOME=<root>` and `PATH`.
    - `moactl brief` → exit 1, `moa v1 isn't switched over yet; set MOA_ROOT to try it (see
      the try-it)`.
    - `moa daemon` → the same.
    - No `.local/` was created under `<root>`.
  - `errors_are_one_line_and_exit_1`: `moactl task set 999 --status done` → stderr exactly
    `no task #999\n`, exit 1.
  - `later_verbs_say_so_without_a_daemon`: `moactl open ~/x` → `moactl open isn't in this
    build yet`, exit 1, and no socket exists.
  - `json_prints_data`: `moactl task ls --json` parses as a JSON array of 3 open tasks.
  - `a_rebuilt_client_restarts_the_daemon_on_its_build`:
    1. Copy `cargo_bin("moa")` to `<root>/other/moa` (another path gives another build id,
       P10) and link `<root>/other/moactl` to it.
    2. Start with the original's `moactl brief`, then run the copy's `moactl brief` → exit 0.
    3. The copy's `moa status` shows a new pid and the copy's build id.
  - `the_caller_reaches_the_daemon`: with `MOA_SESSION=<S1's moa id>`, `moa status` prints
    `you are: 3f9a1c52`. With nothing set and the cwd a folder named `lantern` under the
    root, it prints `you are: unknown in lantern`.
  - `moa_daemon_reports_already_running_and_exits_0`: with a daemon up, `moa daemon` prints
    `the default brain's moa is already running`, exit 0.
  - `offline_tools_refuse_a_running_brain`: daemon up; `moa export-spike --from
    <db(default)> --into <root>/x.db` → exit 1 with the D14 line.
  - `adopt_spike_makes_the_brain_folder`: on a fresh root, `moa adopt-spike --from <fixture>
    --into <root>/.local/share/moa/default/moa.db` → exit 0, and that folder is `0700`.
  - `shutdown_stops_it`: `moa shutdown` → exit 0, the socket is gone; again → exit 0.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**, and `nix build .#default && ls -l result/bin/`
  shows `moactl -> moa`.
- [ ] **Step 5: Commit** `git commit -m "v1: moactl front with routing, moa status/shutdown, offline tools' guard, moactl in the package"`

---

### Task 9: `moa sql`

**Files:**
- Create: `crates/moa/src/sql.rs`, `crates/moa/tests/sql.rs`
- Modify: `crates/moa/src/main.rs`

**Interfaces:**
- Produces `moa sql [--brain B] [SQL]` (spec §6.5), in-process (no `sqlite3`):
  - Opens `paths.db(b)` with `Store::open_read_only`, then `PRAGMA query_only = ON`. It only
    reads, so it isn't gated by `MOA_ROOT`.
  - With `SQL`, it runs that one statement; otherwise one statement per stdin line, with a
    `moa> ` prompt when stdin is a terminal.
  - Output: a header of column names, then one row per line, tab-separated, NULL as empty.
  - An error is one line on stderr. The one-shot form exits 1; the stdin form continues.
  - A missing DB → `the {b} brain has no database yet`.
  - It works while the daemon runs (WAL readers don't block the writer).
- [ ] **Step 1: Write the failing tests**
  - `select_prints_a_header_and_rows`: on `adopt_typical`, `moa sql "select count(*) from
    projects"` prints `count(*)\n3\n`.
  - `writes_are_refused_and_change_nothing`: `moa sql "delete from tasks"` → exit 1, stderr
    contains `readonly`, and there are still 5 tasks.
  - `stdin_runs_each_line`: two `select`s → both results; a bad line in between → its error,
    and the next line still runs.
  - `a_missing_brain_says_so`.
  - `works_while_the_daemon_runs`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `sql.rs`**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa sql, a read-only SQL shell on a brain"`

---

### Task 10: Service registration

**Files:**
- Create: `crates/moa-platform/src/service.rs`, `crates/moa-platform/tests/service.rs`
  (+ snapshots), `crates/moa/tests/service.rs`
- Modify: `crates/moa-platform/src/lib.rs`, `crates/moa/src/main.rs`

**Interfaces:**
- Produces (`moa_platform::service`):
  ```rust
  pub fn label(brain: &str) -> String;                                         // "moa.daemon.<brain>"
  pub fn stable_exe(given: Option<&Path>, home: &Path) -> Result<PathBuf, ServiceError>;   // D10's order; refuses /nix/store/
  pub fn launchd_plist(exe: &Path, brain: &str, paths: &Paths) -> String;
  pub fn systemd_unit(exe: &Path, brain: &str, paths: &Paths) -> String;
  pub struct ServiceOutcome { pub file: PathBuf, pub registered: bool, pub message: String }
  pub fn install(paths: &Paths, brain: &str, exe: &Path) -> Result<ServiceOutcome, ServiceError>;
  pub fn uninstall(paths: &Paths, brain: &str) -> Result<ServiceOutcome, ServiceError>;
  pub enum ServiceError { NixStore, NotAllowed(PathsError), Io(io::Error) }
  ```
  - `NixStore` text: `that moa is a Nix store path that changes on upgrade; run it from your
    profile (~/.nix-profile/bin/moa) or pass --exe`.
  - **plist**: `Label`; `ProgramArguments` = `[exe, "daemon", "--brain", brain]`;
    `RunAtLoad true`; `KeepAlive { SuccessfulExit false }`; `ProcessType Adaptive`;
    `StandardErrorPath` = `stderr_log(brain)`. XML-escape `& < >` in every string.
  - **unit**: `[Unit] Description=moa daemon for the <brain> brain`; `[Service]
    ExecStart="<exe>" daemon --brain <brain>` (the path quoted, `"` and `\` escaped),
    `Restart=on-failure`; `[Install] WantedBy=default.target`.
  - **install**:
    - Starts with `paths.check_v1_allowed()` (D20).
    - The file goes to `launch_agents_dir()/<label>.plist` (macOS) or
      `systemd_user_dir()/moa-<brain>.service` (Linux), written atomically (temp file and
      rename, mode `0644`). Same content already there → `already installed`.
    - Registration, through `Spawner`, **only when not sandboxed**: macOS `launchctl bootout
      gui/<uid>/<label>` (its error ignored), then `launchctl bootstrap gui/<uid> <file>`;
      Linux `systemctl --user daemon-reload`, then `systemctl --user enable --now
      moa-<brain>.service`.
    - When sandboxed: `registered: false`, message `sandbox: wrote <file>; not registered
      with launchd` (or `systemd`).
  - **uninstall**: the reverse (`bootout`, or `disable --now`, only when not sandboxed), then
    remove the file. A missing file → `not installed`.
  - **CLI**: `moa service install|uninstall [--brain B] [--exe PATH]`. The brain must be in
    the config; `exe` = `stable_exe(--exe, paths.home())`; prints the message; exit 0.
- [ ] **Step 1: Write the failing tests**
  - `the_plist_and_unit_text` (insta): exe `/opt/moa bin/moa&co`, brain `work`, root `/r`.
  - `the_stable_exe_rule`:
    - with `home` a tempdir holding `.nix-profile/bin/moa` (a symlink into a fake
      `nix/store` dir), `stable_exe(None, home)` → `<home>/.nix-profile/bin/moa`,
      unresolved;
    - with no profile link, `stable_exe(None, home)` → `current_exe()`.
  - `a_nix_store_path_is_refused`: `stable_exe(Some("/nix/store/abc-moa/bin/moa"), home)` →
    the `NixStore` text exactly. `stable_exe(Some("/r/.nix-profile/bin/moa"), home)` → that path,
    unresolved.
  - `a_sandboxed_install_writes_but_never_registers`: on `Paths::with_root(tmp)`,
    `install` → the file exists with the plist (or unit) text, `registered == false`, the
    message says `sandbox`, and `spawn::launches()` is unchanged. A second install →
    `already installed`. `uninstall` removes the file.
  - `crates/moa/tests/service.rs`:
    - `moa service install --brain work` on a `TempBrain` with a `work` brain → exit 0, the
      file is under the root.
    - `--brain nope` → `there's no brain named nope in <config path>`, exit 1.
    - Without `MOA_ROOT` → the D20 line, exit 1.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa service install/uninstall (stable exe path; never registered in a sandbox)"`

---

### Task 11: End to end: brain isolation, zero launches, the user's brain copy

**Files:**
- Create: `crates/moa/tests/isolation.rs`, `crates/moa-daemon/tests/flow.rs`,
  `crates/moa/tests/local_verbs.rs`
- Modify: `crates/moa-daemon/Cargo.toml` (dev: `moa-testkit`, `moa-client`)

**Interfaces:**
- Consumes everything above; produces no new API.

- [ ] **Step 1: Write the tests**
  - `isolation.rs`, `work_never_lands_in_default` (spec §11.2). A `TempBrain` with:
    ```toml
    [brains.default]
    folders = ["~/private"]
    [brains.work]
    folders = ["~/work"]
    [accounts.acct-a]
    kind = "claude"
    store = "~/.claude-acct-a"
    brain = "work"
    except = ["~/private"]
    [accounts.main]
    kind = "claude"
    store = "~/.claude"
    brain = "work"
    except = ["~/private"]
    ```
    - Make `~/private/lantern`, `~/work/tidepool`, `~/.claude-acct-a` and `~/.claude`.
    - With `CLAUDE_CONFIG_DIR=<root>/.claude-acct-a`, from `~/work/tidepool`:
      `moactl project Tidepool --add .`, `moactl note "WORK-NOTE-1" --project Tidepool`,
      `moactl task add Tidepool WORK-TASK-1` → all exit 0 (the work daemon autostarts).
    - **With no `CLAUDE_CONFIG_DIR`** (the default account, `~/.claude`), from
      `~/work/tidepool`: `moactl note "WORK-NOTE-2" --project Tidepool` → exit 0.
    - From `~/private/lantern` (an `except` folder): `moactl project Lantern --add .` →
      exit 0 (the default daemon).
    - With `MOA_BRAIN=default` from `~/work/tidepool`: `moactl project Other --add .` → exit
      1, `… isn't in this brain's folders; this brain doesn't track it`.
    - Then `moa shutdown --brain work` and `moa shutdown --brain default`.
    - Read both DBs:
      - default has only the project Lantern, no notes and no tasks, and no `WORK-` token
        in any text column;
      - work has Tidepool, both notes and the task.
  - `crates/moa-daemon/tests/flow.rs`, `twenty_verbs_launch_nothing`:
    1. An in-process daemon on a `TempBrain` with `adopt_typical("default")`.
    2. Read `launches()` after `Ready`.
    3. Through `moa_client::Client` (no autostart), run these 20, each answering `Ok` except
       the noted refusal:
       1. `brief`
       2. `attention`
       3. `ls`
       4. `ls --all`
       5. `find lantern`
       6. `project Lantern`
       7. `task ls`
       8. `task add Lantern Solder --due fri`
       9. `task set 6 --status active`
       10. `task set 6 --today`
       11. `note "volts" --project Lantern`
       12. `group add Work/Infra`
       13. `set b84e07d1 --group Work/Infra`
       14. `set b84e07d1 --title "Tide tables"`
       15. `group rename Work/Infra Work/Platform`
       16. `suggest "Finish it?" --do "task set 6 --status done"`
       17. `suggestion ls`
       18. `suggestion yes 5`
       19. `archive 6a2f8e40`
       20. `group rm Work/Platform` (refused: S2 is still there)
    4. `launches()` is unchanged, and `status.launches == 0`.
  - Also in `crates/moa/tests/ctl.rs`, `cli_flow_launches_nothing_after_start`: after the
    first `moactl brief` autostarts the daemon, run 10 more `moactl` data verbs; `moa
    status` shows `0 launches`.
  - `local_verbs.rs` (**local-only**). It returns at once unless `MOA_LOCAL_BRAIN_COPY` is
    set, and also reads `MOA_LOCAL_BRAIN_TZ` and `MOA_LOCAL_BRAIN_ACCOUNTS`, as slice 1's
    local test does. It goes the try-it's way:
    1. `moa adopt-spike --from <copy> --into <TempBrain db(default)>`, with no folder made
       beforehand.
    2. Reads: `brief`, `attention`, `ls`, `ls --all`, `task ls`, `task ls --all`,
       `suggestion ls`, `find a`, and `project <the first project's name>` (from `moa sql
       "select name from projects order by id limit 1"`).
    3. Writes, on the copy:
       1. `task add <it> moa-local-check --due +1`;
       2. `task set <its id from --json> --status done`;
       3. `note moa-local-check --project <it>`;
       4. `suggest moa-local-check`, then `suggestion no <its id>`;
       5. `group add moa-local-check`, then `group rm moa-local-check`.
    4. Through `moa_client`, subscribe to every topic and keep the last snapshot. Then
       `moa shutdown`.
    5. Load the DB into a fresh `State` (`Store::load_snapshot` + `Startup`).
       `snapshot_of(state.model(), all)` equals the kept snapshot, ignoring `rev`.
    6. It asserts every verb exited 0, and prints only each verb's exit code and line
       count, never the output.
- [ ] **Step 2: Run them and see them pass** (they exercise finished code; a failure is a
  bug in an earlier task: fix it there, with a test)

  Run: `nix develop --command cargo nextest run --workspace`, clippy with `-D warnings`,
  `fmt --check`, and `nix flake check -L`.
- [ ] **Step 3: The private brain, on a copy** (the implementer, **from the main checkout**:
  the spike's `brain.db` is gitignored and isn't in a worktree)

```bash
python3 -c "import sqlite3; s=sqlite3.connect('file:spike/master-chat/brain.db?mode=ro',uri=True); d=sqlite3.connect('/tmp/moa-private-copy.db'); s.backup(d)"
MOA_LOCAL_BRAIN_COPY=/tmp/moa-private-copy.db MOA_LOCAL_BRAIN_TZ=<your zone> MOA_LOCAL_BRAIN_ACCOUNTS=<pairs> \
  nix develop --command cargo nextest run -p moa --test local_verbs --no-capture
```

  Expected: every verb exits 0 and the reload matches. Delete the copy afterwards. Type the
  account pairs on the command line only (as in slice 1).
- [ ] **Step 4: The work brain** (the user runs this): the same command with the work
  brain's path, in their own terminal. Ask them to report whether it passed.
- [ ] **Step 5: Commit** `git commit -m "v1: end-to-end: brain isolation, zero launches per verb, local brain check"`
- [ ] **Step 6: Open PR b3** (Tasks 7–11), with `/code-review medium --fix`. The
  description lists calls D1–D20 in a line each, gives the try-it below, and says "Restart
  needed: nothing; the spike is untouched". Then give the user the try-it.

## Done when (the roadmap's slice 2)

- `moactl task|note|project|suggest|suggestion|brief|attention|find` work against a copy of
  the private brain imported with slice 1, through the daemon, with no tmux. Its writes
  survive a reload unchanged (`local_verbs`, plus the try-it).
- A scripted flow of 20 verbs makes zero `Spawner` launches after the daemon is up
  (`twenty_verbs_launch_nothing`).
- Brain isolation holds through the CLI (`work_never_lands_in_default`), and CI is green.

**Try it** (on a copy of the private brain, in your own terminal, from the main checkout):

```bash
python3 -c "import sqlite3; s=sqlite3.connect('file:spike/master-chat/brain.db?mode=ro',uri=True); d=sqlite3.connect('/tmp/moa-copy.db'); s.backup(d)"
export MOA_ROOT=/tmp/moa-try
nix run . -- adopt-spike --from /tmp/moa-copy.db --into /tmp/moa-try/.local/share/moa/default/moa.db [--account NAME=STORE…]
nix run .#moactl -- brief          # autostarts the sandbox daemon
nix run .#moactl -- task ls
nix run . -- shutdown              # stop it; rm -rf /tmp/moa-try /tmp/moa-copy.db when done
```

Restart needed: nothing; the spike is untouched.
