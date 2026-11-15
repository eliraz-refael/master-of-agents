# moa v1 slice 2a: core, store actor, platform

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The brain, in-process: the pure core that answers the data verbs, the store actor
that persists what the core decides, and the OS pieces both need (paths, the wall-clock
alarm, the spawn counter). No socket and no daemon yet; slice 2b puts this behind them.

**Architecture:**
- `moa-platform` (new) holds the OS glue: `Paths` (with `MOA_ROOT`), private directories,
  the build id, the counting `Spawner`, `WallAlarm` (kqueue / timerfd) and `ZoneWatch`.
  It's the only crate with `unsafe`.
- `moa-core` grows the config, the routing function, `Now`, due dates, the live display
  model, and the core itself: `handle(&mut State, Input, Now) -> Vec<Effect>`. Each verb
  handler collects typed `Change`s and hands them to its transaction (`Tx::commit`), which
  checks them all, then applies them to the in-memory `State`. The same `Change` values
  later reach SQLite, so memory and disk are changed by one value.
- `moa-store` grows `load_snapshot`, `apply(&[Change])`, and `StoreHandle`: one OS thread
  that runs each commit it's given as one transaction and answers it. The batching is the
  core task's: it groups a whole drain of inputs into one commit (call S4).

```
 Input ──► handle(&mut State, Input, Now) ──► [Persist(changes) …, Publish(model), Reply(done)]
                │ ops::* → Tx::commit(changes)               │
                │ (check all, then apply to State)           ▼
                ▼                                 StoreHandle (OS thread): BEGIN; apply; COMMIT
          Model (topics as JSON rows, rev)               └─► Ok | Failed(reason) | Stopped
```

**Tech Stack:** Rust 2024; `tokio` (platform: `AsyncFd`; store: `sync` only), `libc`,
`toml` (new), `jiff` (now with its `serde` feature), `insta` (new), `proptest`, `rusqlite`
0.41.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`: §6.3 (core), §6.5 (store,
paths C27, backups), §6.7 (live-state table), §6.8 (timers), §6.9 (brains, routing),
§6.10 (verbs, name resolution, due dates), §11.1–11.3. Roadmap: Slice 2 items 1–5 and 8,
P4, P10, P13, P14. Parity source for output text: `spike/moa-proto/moactl.py` and
`picture.py` (R1 §2.4, §2.13).

**Slice 2b** (`2026-11-15-moa-v1-slice-2b-daemon.md`) builds on this: protocol, verb
grammar, suggestions, daemon, client, `moa sql`, service. The roadmap's slice-2 "done
when" and "try it" belong to 2b.

## PRs (roadmap P4)

The slice is too big for one PR, so it ships as four. Each PR is its own branch from `main`,
cut after the previous one merged. Each gets `/code-review medium --fix`, its fixes pushed,
and green CI, and its description says "Restart needed: nothing; the spike is untouched".

| PR | Branch | Tasks | Title |
|---|---|---|---|
| a1 | `v1/slice-2a-1` | 1–3 | `v1 slice 2a (1/4): store hardening, platform` |
| a2 | `v1/slice-2a-2` | 4–7 | `v1 slice 2a (2/4): config, routing, time, live model` |
| a3 | `v1/slice-2a-3` | 8–11 | `v1 slice 2a (3/4): core skeleton, store actor` |
| a4 | `v1/slice-2a-4` | 12–14 | `v1 slice 2a (4/4): data verbs` |

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| S1 | **Slice 2 is two plans.** 2a = everything that computes (platform, config, route, core, store actor, the data verbs except suggestions). 2b = everything that talks (protocol, grammar, suggestions, daemon, client, CLI, `moa sql`, service) | 2a is fully testable through `handle` and the store actor with no socket; 2b is IO. Suggestions sit in 2b because validating an action needs the verb grammar (P11), which is 2b's |
| S2 | **`Snapshot`, `Topic`, `CoreEvent` and `Op` are defined in `moa-core`**; `moa_store` re-exports `Snapshot`, `moa_proto` re-exports `Topic` | `Input::Startup(Snapshot)`, `Effect::Publish` and `Input::Client(op)` are core types, and the core can't depend on store or proto |
| S3 | **The core renders the spike's text** into every reply (`Done { text, data }`); the CLI prints `text`, or `data` with `--json` | The text needs the brain's clock and zone (ages, "today"), and the console (slice 6) shows the same text. Spec §6.2 already puts brief/attention text in the core |
| S4 | **Group commit, one batch in flight; nothing leaves the daemon before its data is on disk.** The core task (2b) takes everything waiting in its inbox, up to 64 inputs (a *drain*), and runs `handle` on each in order. The core updates its model at once. The drain's `Persist` changes become **one** batch, and every other effect is held, in order; that includes `Publish` and `Reply`, and from later slices `Tmux`, `Acp` and `Spawn`. Only `SetTimer`, `CancelTimer` and `Log` run at once; the executor also starts `Backup` at once (2b Task 4), since it copies only committed data, so a failed or panicked drain never loses the day's backup.<br>• **A drain with changes:** the batch goes to the store as one transaction, and the core waits for it. On success the held effects are released in order, then the next drain starts; inputs that arrive meanwhile queue up, which is how batches grow under load.<br>• **A drain with no changes:** its effects are released at once. Nothing uncommitted exists, because the previous batch was awaited.<br>• **A failed commit:** every held `Reply` of that drain fails with one retry line. That's writes **and** reads, since a read in the drain may have seen the failed state. Held `Publish` and external effects are dropped. Then the core reloads (`Input::Reload`, which replaces the state and re-derives the timers) and publishes the whole model.<br>• **Failures in a row:** after 3, the reload carries a deadline (1 s, doubling to 30 s) before which every write is refused with `moa can't save right now (<reason>); nothing was changed`. The core compares the deadline with `Now`, so the refusal ends by itself, and the next write after it probes the store; no reload just to lift it.<br>So there's no `StoreCommitted` input, no sequence numbers, and nothing to fence | Batching, as the roadmap's item 5 asks, with the spec's ordering rule kept for reads too. Slices 3–6 inherit the hold rule for their side effects. The cost: a write's reply waits for one commit, and a drain doesn't overlap a commit. At one user's scale, with SQLite WAL commits taking milliseconds, that's fine. The pipelined design (seqs, epochs, high-water marks) had three bookkeeping bugs in one review round |
| S5 | **Inputs and effects arrive with their first user.** 2a has `Startup`, `Reload`, `Client`, `Timer`, `ZoneChanged`, `ActorFailed`; effects `Persist`, `SetTimer`, `CancelTimer`, `Publish`, `Reply`, `Backup`, `Log`. `Hook`, `Tmux*`, `Acp`, `ClientGone`, `ConfigChanged`, `Spawn` and `pending_ops` come in slices 3–9. `Reload`, `ZoneChanged` and `Backup` are additions to spec §6.3 | An input nobody sends or a table nobody fills is untested code (P2). A time-zone change must reach the core (§6.8), and the daily backup must reach the store actor |
| S6 | **No `DelayQueue` in slice 2**; the scheduler (2b) runs wall-clock deadlines only. The roadmap's "read-only connection for find/history" waits for its first user too: `find` searches the in-memory model, as the spike's `candidates()` searched the sessions table | Slice 2's only timer is the daily backup. Short timers start with slice 3's reconnect backoff |
| S7 | **The core allocates integer ids** (`NextIds`, max + 1 per table, loaded with the snapshot) | It must answer "task #12 added" before the commit. The daemon is the only writer, so max + 1 is what SQLite would pick |
| S8 | **Sessions are shown by a handle**: the first 8 characters of the current conversation id (else of the moa id). Name lookup accepts a prefix of the moa id or of any conversation id | Imported moa ids are UUIDv7: sessions started within the same ~65 s share their first 8 characters. Conversation prefixes are what the user, moa's notes and imported suggestions already use |
| S9 | **`route()` returns `Route::{Brain(BrainName), NotAvailable}`**; the config expands `~` when it's parsed, and callers canonicalise both the cwd and (through `Config::from_file_text`) the configured folders | The roadmap's `Route` name; the core does no IO; symlinked folders must match their targets |
| S10 | **A missing `moa.toml` means one brain, `default`**, with no folders and no accounts. A brain other than `default` must be in the file. Unknown keys inside `[brains.*]`, `[accounts.*]` and `[setup]` are errors; an unknown top-level table is skipped and reported as a warning | The first runs have no config. A typo in a routing key must not silently send work somewhere else, but a table a newer build added (slice 8 writes `moa.toml`) must not stop an older build |
| S11 | **Paths are fixed under `HOME`** (or `MOA_ROOT`); `XDG_*` variables are not read. A socket path over 103 bytes falls back to `$TMPDIR/moa-$UID/<fnv8>-<brain>.sock`, where `fnv8` hashes the intended path | The spike's forwarders (slice 10) look for `~/.local/state/moa/v1-live` at that exact path. The hash keeps two long sandbox roots from sharing a socket |
| S12 | **User-facing strings that named the user in the spike say "you" or "the user"** | Public repo; the product is for other people too |
| S13 | **Small fixes over the spike's verbs**, each one line:<br>• `set` with no flag is refused;<br>• `group rename` of a group nobody has is refused;<br>• `note --project X` with an unknown X saves the note without a project and says so (the spike dropped the project silently);<br>• `--due` words are case-insensitive;<br>• empty task titles are refused;<br>• `--status` is validated | Spec goal 5 (fix the spike's mistakes); none changes a valid call's output |
| S14 | **Unknown enum text in the DB loads as the import's default for that column**, with a note, and **this build refuses to edit that row**: `task #3 has a value this build doesn't know; update moa` | A newer build's value (additive rule) must not stop an older build from starting, and a full-row upsert must not erase it |
| S15 | **Logs and `Log` effects never carry record contents** (titles, text, names, paths): ids, tables and fixed sentences only | The work brain's data stays out of anything that might be pasted (P8's spirit) |
| S16 | **`run/` and `logs/` are made by whoever needs them first** (client or daemon) through one function, `ensure_private_dir`: mode `0700`, refusing a directory another user owns or others can open | A client that creates `run/` with default permissions would make the daemon refuse it on the very first autostart |
| S17 | **Until slice 10, nothing uses the real home**: `moa daemon`, autostart and `moa service` refuse without `MOA_ROOT`, unless `MOA_REAL_HOME=1` (`Paths::check_v1_allowed`). Slice 10 removes the gate | The spike is live and in daily use on this machine; a manual `cargo run -- daemon` would otherwise create a v1 brain next to it |
| S18 | **An unset `CLAUDE_CONFIG_DIR` means `~/.claude`** (`config::caller_store`), as the spike's `track.sh` treats it | Claude's default account runs with it unset; without this, its sessions would route to `default` whatever the config says |
| S19 | **The CLI and the daemon read `moa.toml` through one function**, `Config::from_file_text(text, home, canonical)` | Both must canonicalise the same way, or `/tmp` vs `/private/tmp` (macOS) misroutes |

## Global Constraints

The roadmap's Global Constraints apply. The ones that matter most here:

- **cargo runs only inside the dev shell**: every command is `nix develop --command cargo
  …`, with `cargo nextest run`. `nix flake check -L` proves the Nix build.
- **Tests never touch real user state.** No real brains, no real `~/.claude*`, no real tmux
  server (slice 2a needs none). Paths come from `Paths::with_root(<tempdir>)`; nothing
  reads `HOME` in a test. Test roots are short dirs under `/tmp`
  (`tempfile::Builder::new().prefix("moa").tempdir_in("/tmp")`), so socket paths never
  need the `$TMPDIR` fallback.
- **The core is pure**: `moa-core` never depends on tokio, rusqlite or libc (CI checks it
  from Task 9), reads no clock, and does no IO. Time arrives as `Now`.
- **`unsafe` only in `moa-platform`'s `sys` module**, each block with a `// SAFETY:`
  comment, and `#[allow(unsafe_code)]` on that module only.
- **Every product launch goes through `moa_platform::spawn::Spawner`**, which counts.
- **Instants** are `UtcMs`; **calendar dates** (`due`, `today`, `last_activity_on`) are local
  `jiff::civil::Date`s in the brain's zone (`Now.tz`), never UTC dates.
- **Output text** follows the spike's `moactl` exactly where a format is given below.
  Multi-line outputs are pinned with `insta` snapshots, reviewed once and committed.
- **New dependencies** are added to `[workspace.dependencies]` by the task that first needs
  them, with the exact version resolved at that time: `libc 0.2` and `tokio 1` (Tasks 2–3),
  `toml` (Task 4), `insta` with `json` (Task 7), `jiff`'s `serde` feature (Task 8).
- **Fixtures are made up**: Lantern, Tidepool, Driftwood, `acct-a`, `acct-b`, `~/private`.

## Review Focus

1. **Paths in routing and folders** (roadmap Review Focus 4): `~`, symlinked folders,
   trailing slashes, `.` segments, empty entries, `~/private2` next to `~/private`, an
   unset `CLAUDE_CONFIG_DIR`.
   - Expected: whole-component matching; an empty entry matches nothing; a symlinked
     configured folder routes like its target; unset means `~/.claude`.
   - Pinned by the route proptests, `a_symlinked_folder_routes_like_its_target` (Task 5),
     `caller_store_defaults_to_dot_claude` (Task 4), and
     `project_add_checks_the_folder_by_whole_components` (Task 13).
2. **The in-memory model drifting from the DB.** A `Change` applied one way in `State` and
   another way in SQL would show the user something the DB doesn't have.
   - Expected: after any sequence of verbs, reloading the DB gives the same model.
   - Pinned by `verbs_through_the_store_match_a_reload` (Task 14).
3. **"Today" near midnight and across DST.** At 23:30 local the UTC date is already
   tomorrow.
   - Expected: `--due today`, `--today`, "done today" and the brief's date use the brain's
     local day.
   - Pinned by `now_takes_the_local_day_not_the_utc_one` (Task 6) and
     `due_today_at_half_past_eleven_is_today` (Task 13).
4. **Ambiguous session names in imported data**: moa ids that share 8 characters, archived
   sessions, titles that differ only in case, a session with no conversation row.
   - Expected: the spike's rules (unique prefix, then substring, exact title wins, archived
     only by prefix); an ambiguous name lists up to 8 and changes nothing.
   - Pinned by Task 12's resolution tests.
5. **Odd files and failures where the store expects its own**: another app's SQLite file at
   the brain's path, a dangling `daily-*.db` link, a backup removed while pruning, a
   directory named like a backup, a commit that fails halfway, a store thread that dies.
   - Expected: nothing foreign is written; pruning keeps going and reports what it couldn't
     remove; a dangling link is not "already backed up"; a failed commit leaves nothing on
     disk; a dead store thread answers `Stopped`.
   - Pinned by Task 1 and `a_commit_is_one_transaction` (Task 11).

---

### Task 1: Store hardening (slice 1's review carry-overs)

Fixed before anything schedules pruning or the daily `VACUUM INTO`, and before a daemon
writes while an export reads.

**Files:**
- Modify: `crates/moa-store/src/lib.rs` (`open_with`, `open_read_only`, a shared brain check)
- Modify: `crates/moa-store/src/backup.rs` (`daily_backup`, `prune_backups`)
- Modify: `crates/moa-store/src/export/mod.rs` (`write_all`: one read transaction)
- Modify: `crates/moa-store/src/import/mod.rs` (`SrcRow::text_cow`), `crates/moa-store/src/import/sessions.rs` (`Ev` keeps the row only)
- Modify: `crates/moa/src/adopt.rs` (`export` drops its copy of export's exists checks)
- Test: `crates/moa-store/tests/open.rs`, `crates/moa-store/tests/backup.rs`,
  `crates/moa-store/tests/round_trip.rs`, `crates/moa/tests/adopt.rs`

**Interfaces:**
- Produces:
  - `Store::open` / `open_with`: when `db` exists and is non-empty, it is first opened
    `SQLITE_OPEN_READ_ONLY` and checked; only a brain gets the PRAGMAs and migrations.
    A file is **not a brain** (`StoreError::NotABrain`) when:
    - SQLite says it isn't a database; or
    - `user_version` is 0 and any table exists; or
    - `user_version` > 0 and there is no `sessions` table.

    An empty or missing file is a new brain, as before.
  - `fn brain_version(conn: &Connection) -> Result<Option<usize>, StoreError>` (crate-private;
    `None` = a new, empty file), used by both `open_with` and `open_read_only`.
  - `pub struct Pruned { pub removed: Vec<PathBuf>, pub failed: Vec<(PathBuf, io::ErrorKind)> }`
  - `pub fn prune_backups(dir: &Path) -> io::Result<Pruned>`: a file that is already gone
    (`NotFound`) is skipped silently; any other removal error goes in `failed` and pruning
    continues. Only `read_dir` failing is an `Err`.
  - `daily_backup`: "already backed up" means `symlink_metadata(dest)` is a **regular file**.
    A symlink at that name (dangling or not) is removed (the link, never its target) and the
    backup is written. Anything else there (a directory) is `Err(AlreadyExists)`.
    The backup is written to `daily-<day>.db.tmp` (a leftover `.tmp` and its sidecars are
    removed first) and then renamed, so a process killed mid-`VACUUM INTO` never leaves a
    partial file that counts as the day's backup. A leftover `-wal`, `-shm` or `-journal`
    beside `daily-<day>.db` itself is still refused before the rename (the existing
    `daily_backup_reports_a_leftover_sidecar` keeps passing): a fresh file renamed next to
    a stale journal would have that journal rolled into it on its first open.
  - `export_spike` reads its source inside one read transaction
    (`src.conn().unchecked_transaction()`, held until the `Exporter` is done).
  - `SrcRow::text_cow(&self, col) -> Option<Cow<'_, str>>`; `Ev` drops its `text` field and
    reads `row.text_cow("text")`, so each event's text is held once.

- [ ] **Step 1: Write the failing tests**
  - `open.rs`:
    - `a_foreign_database_is_refused_before_anything_is_written`: a rollback-journal SQLite
      file with `CREATE TABLE x(a)` and `user_version` 0. `Store::open` → `NotABrain`. The
      file's SHA-256 is unchanged, no `-wal`/`-shm` exists beside it, and
      `PRAGMA journal_mode` on a fresh read-only open is still `delete`.
    - `another_apps_versioned_database_is_refused`: `user_version` 7 plus table `x` →
      `NotABrain`, bytes unchanged. The same file through `open_read_only` → `NotABrain`.
    - `a_file_that_isnt_sqlite_is_refused_untouched`: `b"hello\n"` → `NotABrain`, bytes
      unchanged.
    - `an_empty_file_becomes_a_brain`: a 0-byte file → `Ok`, `user_version` =
      `SCHEMA_VERSION`.
    - The existing `a_db_from_a_newer_build_opens_without_migrating` must still pass.
  - `backup.rs`:
    - `prune_keeps_going_past_a_file_it_cannot_remove`:
      - `daily-2026-01-01.db` … `daily-2026-01-20.db`, with the first a **directory**.
      - Expected kept: Jan 4, 11, 13 and 14–20 (10 dates).
      - Expected deleted: Jan 1, 2, 3, 5, 6, 7, 8, 9, 10, 12.
      - So `failed == [daily-2026-01-01.db]`, `removed` holds the other 9, and the 10 kept
        files remain.
    - `a_dangling_daily_link_is_not_a_backup`: `daily-2026-11-15.db` → a symlink to a
      missing path. `daily_backup(…, 2026-11-15)` → `Some(path)`; `path` is now a regular
      file whose `projects` count matches the store; the link's old target still doesn't
      exist.
    - `a_daily_link_to_a_real_file_is_replaced`: the link points at `other.db` (contents
      `b"keep"`). After `daily_backup`, `other.db` still reads `b"keep"` and the daily file
      is a regular file.
    - `a_leftover_tmp_is_not_a_backup`: only `daily-2026-11-15.db.tmp` (garbage bytes)
      exists. `daily_backup(…, 2026-11-15)` → `Some(path)`; `daily-2026-11-15.db` is a
      valid copy and no `.tmp` remains. `prune_backups` never counts a `.tmp` file.
  - unit test in `backup.rs`, `a_backup_someone_else_removed_is_not_an_error`: call the
    removal step (a crate-private `remove_dated(dir, &[Date]) -> Pruned`) with one date whose
    file doesn't exist → `removed` and `failed` both leave it out.
  - `round_trip.rs`, `export_reads_one_snapshot`:
    1. Import Typical into `v1.db`; note the event count and max event id.
    2. Open it with `Store::open_read_only`. Register
       `conn.progress_handler(1000, Some(handler))`, where `handler` fires **once**: through
       a second, read-write `Connection` it inserts a project, a task and an event.
    3. `export_spike` → the exported DB has the pre-write counts for projects, tasks and
       events, and `meta.cursor` equals the old max event id.
    4. The source DB does have the new rows (the write happened).
  - `crates/moa/tests/adopt.rs`:
    - `export_spike_refuses_an_existing_target`: `--into` an existing file → exit 1,
      `moa.db already exists; moa never overwrites a brain`, the file unchanged. (It passes
      today; it pins the message once the CLI's own exists check is gone.)
    - `export_spike_refuses_a_sidecar_name`: `--into x.db-wal` → the CLI's existing sidecar
      sentence, not `already exists`.
- [ ] **Step 2: Run them and see the new ones fail**

  Run: `nix develop --command cargo nextest run -p moa-store -p moa`
  Expected: the new store tests FAIL (foreign files open, prune aborts, the link reads as
  backed up, the export sees the new rows).
- [ ] **Step 3: Implement** to the interfaces above.
  - In `adopt.rs`, `export()` keeps its `is_sidecar_name` check (export's own
    `ExportError::Exists` would say "already exists", which is false there). It drops the
    exists loop and relies on `ExportError::Exists`, which `export_err` already maps to the
    same sentence.
  - `adopt()` keeps `refuse_existing`: `Store::open` would happily open an existing brain.
- [ ] **Step 4: Run them and see them pass**, the whole workspace included.

  Run: `nix develop --command cargo nextest run --workspace`
  Expected: PASS
- [ ] **Step 5: Commit** `git commit -m "v1: store refuses foreign files before writing; prune and daily backup survive odd files; export reads one snapshot"`

---

### Task 2: `moa-platform`: paths, private dirs, the v1 gate, the build id, the spawn counter

**Files:**
- Create: `crates/moa-platform/Cargo.toml`, `src/lib.rs`, `src/paths.rs`,
  `src/build_id.rs`, `src/spawn.rs`, `tests/paths.rs`, `tests/spawn.rs`
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `libc = "0.2"`)

**Interfaces:**
- Produces (`moa_platform`; brain names are `&str` here, because the platform doesn't depend
  on `moa-core`):
  - `pub struct Paths { home: PathBuf, sandboxed: bool, real_home_ok: bool, tmpdir: PathBuf, uid: u32 }`
  - `Paths::from_env() -> Result<Paths, PathsError>`: reads `MOA_ROOT`, `HOME`, `TMPDIR`,
    `MOA_REAL_HOME` (`"1"` sets `real_home_ok`) and the uid, and calls `from_vars`.
  - `Paths::from_vars(moa_root: Option<OsString>, home: Option<OsString>, tmpdir: Option<OsString>, uid: u32, real_home_ok: bool) -> Result<Paths, PathsError>`:
    `MOA_ROOT` wins and sets `sandboxed`; it must be absolute. `TMPDIR` defaults to `/tmp`.
  - `Paths::with_root(root: &Path) -> Paths` (sandboxed; for tests and `--sandbox`).
  - Accessors, all derived from `home` (C27):

    | Method | Path |
    |---|---|
    | `config_dir()`, `config_file()` | `home/.config/moa`, `…/moa.toml` |
    | `data_dir()`, `brain_dir(b)`, `db(b)`, `backups(b)` | `home/.local/share/moa`, `…/<b>`, `…/<b>/moa.db`, `…/<b>/backups` |
    | `state_dir()`, `run_dir()` | `home/.local/state/moa`, `…/run` |
    | `lock(b)`, `start_lock(b)` | `run/<b>.lock`, `run/<b>.start` |
    | `socket(b) -> Result<PathBuf, PathsError>` | `run/<b>.sock`, else the fallback (call S11) |
    | `spool(b)`, `log_dir()`, `log(b)`, `stderr_log(b)` | `state/spool/<b>.jsonl`, `state/logs`, `…/<b>.log`, `…/<b>.stderr.log` |
    | `launch_agents_dir()`, `systemd_user_dir()` | `home/Library/LaunchAgents`, `home/.config/systemd/user` |

  - `Paths::home()`, `Paths::is_sandboxed()`, `Paths::read_config(&self) -> io::Result<Option<String>>`
    (`None` when the file is missing).
  - `Paths::check_v1_allowed(&self) -> Result<(), PathsError>` (call S17): `Ok` when
    sandboxed or `real_home_ok`, else `NotSwitchedOver`.
  - `pub fn ensure_private_dir(dir: &Path) -> Result<(), PathsError>` (call S16). It
    creates `dir` (and missing parents) with mode `0700`, then checks `dir` with
    `symlink_metadata` (never following a link, since the socket fallback lives in a shared
    `$TMPDIR`):
    - a symlink → `UnsafeDir(dir, "is a symlink")`;
    - owned by another uid → `UnsafeDir(dir, "isn't yours")`;
    - any group or other permission bit set → `UnsafeDir(dir, "is open to other users")`.
  - `pub enum PathsError { NoHome, RootNotAbsolute, SocketTooLong, NotSwitchedOver, UnsafeDir(PathBuf, &'static str), Io(io::Error) }`,
    displayed exactly as:

    | Variant | Text |
    |---|---|
    | `NoHome` | `can't tell your home folder: HOME isn't set` |
    | `RootNotAbsolute` | `MOA_ROOT must be a full path` |
    | `SocketTooLong` | `the socket path is too long even under TMPDIR; set a shorter TMPDIR` |
    | `NotSwitchedOver` | `moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)` |
    | `UnsafeDir` | `{dir} {reason}; moa won't use it (chmod 700 {dir})` (no `chmod` hint for "isn't yours" or "is a symlink") |

  - `pub fn canonical(p: &Path) -> PathBuf`: `fs::canonicalize`, or `p` unchanged when that
    fails.
  - `pub fn fnv1a64(bytes: &[u8]) -> u64`
  - `pub fn build_id_of(exe: &Path) -> io::Result<String>` (P10): FNV-1a-64 over
    `canonical path bytes ‖ 0x00 ‖ file length (u64 LE) ‖ mtime in ns since the epoch
    (u128 LE)`, as 16 lowercase hex digits. `pub fn build_id() -> &'static str`: the same
    for `std::env::current_exe()`, computed once (`OnceLock`); `"unknown"` if that fails.
  - `pub mod spawn`: `pub struct Spawner;` with
    `pub fn spawn(&self, cmd: &mut std::process::Command) -> io::Result<std::process::Child>`,
    which adds 1 to a process-wide `AtomicU64` **before** spawning, and
    `pub fn launches() -> u64`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/paths.rs
fn p(root: &str) -> Paths { Paths::with_root(Path::new(root)) }
fn vars(root: Option<&str>, home: Option<&str>, real: bool) -> Result<Paths, PathsError> {
    Paths::from_vars(root.map(Into::into), home.map(Into::into), None, 501, real)
}
#[test] fn moa_root_moves_every_path() {
    let x = p("/r");
    assert_eq!(x.config_file(), Path::new("/r/.config/moa/moa.toml"));
    assert_eq!(x.db("default"), Path::new("/r/.local/share/moa/default/moa.db"));
    assert_eq!(x.backups("work"), Path::new("/r/.local/share/moa/work/backups"));
    assert_eq!(x.lock("work"), Path::new("/r/.local/state/moa/run/work.lock"));
    assert_eq!(x.socket("work").unwrap(), Path::new("/r/.local/state/moa/run/work.sock"));
    assert_eq!(x.spool("work"), Path::new("/r/.local/state/moa/spool/work.jsonl"));
    assert_eq!(x.log("work"), Path::new("/r/.local/state/moa/logs/work.log"));
    assert_eq!(x.stderr_log("work"), Path::new("/r/.local/state/moa/logs/work.stderr.log"));
    assert_eq!(x.launch_agents_dir(), Path::new("/r/Library/LaunchAgents"));
    assert_eq!(x.systemd_user_dir(), Path::new("/r/.config/systemd/user"));
    assert!(x.is_sandboxed());
}
#[test] fn moa_root_wins_over_home() {
    assert_eq!(vars(Some("/r"), Some("/home/u"), false).unwrap().home(), Path::new("/r"));
    let y = vars(None, Some("/home/u"), false).unwrap();
    assert_eq!(y.home(), Path::new("/home/u")); assert!(!y.is_sandboxed());
}
#[test] fn bad_roots_and_no_home_are_refused() {
    assert_eq!(vars(Some("rel"), None, false).unwrap_err().to_string(), "MOA_ROOT must be a full path");
    assert_eq!(vars(None, None, false).unwrap_err().to_string(), "can't tell your home folder: HOME isn't set");
}
#[test] fn the_real_home_is_off_limits_until_the_switch() {
    assert!(vars(Some("/r"), Some("/home/u"), false).unwrap().check_v1_allowed().is_ok());
    assert_eq!(vars(None, Some("/home/u"), false).unwrap().check_v1_allowed().unwrap_err().to_string(),
               "moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)");
    assert!(vars(None, Some("/home/u"), true).unwrap().check_v1_allowed().is_ok());   // MOA_REAL_HOME=1
}
#[test] fn private_dirs_are_made_0700_and_open_ones_refused() {
    // ensure_private_dir(tmp/a/b) → both exist, b's mode is 0700
    // a pre-made tmp/c with mode 0755 → Err whose text is "<tmp>/c is open to other users; moa won't use it (chmod 700 <tmp>/c)"
    // tmp/d a symlink to a 0700 dir of ours → Err "<tmp>/d is a symlink; moa won't use it"
}
#[test] fn a_long_socket_path_falls_back_to_tmpdir() {
    let long = format!("/{}", "a".repeat(100));
    let x = Paths::from_vars(Some(long.clone().into()), None, Some("/t".into()), 501, false).unwrap();
    let s = x.socket("default").unwrap();
    assert!(s.starts_with("/t/moa-501/") && s.to_str().unwrap().ends_with("-default.sock"));
    assert!(s.as_os_str().len() <= 103);
    let other = Paths::from_vars(Some(format!("{long}b").into()), None, Some("/t".into()), 501, false).unwrap();
    assert_ne!(other.socket("default").unwrap(), s);                       // roots don't share
}
#[test] fn the_limit_is_103_bytes() { /* a root that makes run/<b>.sock exactly 103 bytes keeps it; 104 falls back */ }
#[test] fn fnv_known_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
}
#[test] fn build_id_follows_path_length_and_mtime() {
    // copy a file to two names: ids differ; append a byte: id changes; same file twice: equal;
    // always 16 lowercase hex digits
}
// tests/spawn.rs
#[test] fn spawner_counts_every_launch() {
    let before = spawn::launches();
    Spawner.spawn(&mut std::process::Command::new("true")).unwrap().wait().unwrap();
    let _ = Spawner.spawn(&mut std::process::Command::new("/no/such/program"));   // a failed launch still counts
    assert_eq!(spawn::launches(), before + 2);
}
```

- [ ] **Step 2: Run them and see them fail**

  Run: `nix develop --command cargo nextest run -p moa-platform`
  Expected: compile errors (the crate is new).
- [ ] **Step 3: Implement.** The fallback name is
  `<tmpdir>/moa-<uid>/<fnv1a64(intended path) low 32 bits as 8 hex>-<brain>.sock`.
  `Paths` never creates directories; `ensure_private_dir` does.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-platform paths (MOA_ROOT, v1 gate, private dirs, socket fallback), build id, counting spawner"`

---

### Task 3: `moa-platform`: the wall-clock alarm and the time-zone watch

**Files:**
- Create: `crates/moa-platform/src/alarm.rs`, `src/zone.rs`, `src/sys/mod.rs`,
  `src/sys/macos.rs`, `src/sys/linux.rs`, `tests/alarm.rs`, `tests/zone.rs`
- Modify:
  - `Cargo.toml`: workspace `tokio = { version = "1", features = ["rt", "rt-multi-thread", "net", "time", "sync", "macros", "io-util", "signal"] }`.
  - `crates/moa-platform/Cargo.toml`: `tokio`, `libc`.
  - `src/lib.rs`: `#[allow(unsafe_code)] mod sys;`.
  - `rust-toolchain.toml`: `targets = ["x86_64-unknown-linux-gnu"]`, so the Linux code is
    checked on the Mac.

**Interfaces:**
- Produces:
  - `pub trait WallTimer: Send { fn arm(&mut self, at: SystemTime) -> io::Result<()>; fn disarm(&mut self) -> io::Result<()>; fn fired(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>>; }`
    - `arm` replaces any earlier deadline. A deadline in the past fires at once.
    - `fired` resolves once per expiry and is cancel-safe (a dropped future loses nothing).
  - `pub struct WallAlarm` (implements `WallTimer`); `WallAlarm::new() -> io::Result<WallAlarm>`
    must be called inside a tokio runtime.
    - macOS: one `kqueue` fd in `AsyncFd`, one `EVFILT_TIMER` (ident 1, `EV_ADD | EV_ONESHOT`,
      `fflags = NOTE_ABSOLUTE | NOTE_MACH_CONTINUOUS_TIME | NOTE_USECONDS`, `data` = the
      deadline in µs since the epoch). Define `NOTE_MACH_CONTINUOUS_TIME = 0x80` locally if
      `libc` lacks it. Disarm is `EV_DELETE`. When the fd is readable, drain with a
      zero-timeout `kevent`.
    - Linux: `timerfd_create(CLOCK_REALTIME, TFD_NONBLOCK | TFD_CLOEXEC)` +
      `timerfd_settime(TFD_TIMER_ABSTIME)`; read the 8-byte count to clear it.
  - `pub struct ZoneWatch`; `ZoneWatch::system() -> io::Result<ZoneWatch>`;
    `pub async fn changed(&mut self) -> io::Result<()>` (one or more changes since the last
    call).
    - macOS: `notify_register_file_descriptor("com.apple.system.timezone", …)` (declare the
      `extern "C"` functions from libSystem: `notify_register_file_descriptor`,
      `notify_cancel`, `notify_post`). The fd is set `O_NONBLOCK` and wrapped in `AsyncFd`;
      read 4-byte tokens until `WouldBlock`. `notify_cancel` on drop.
    - Linux: `inotify` on `/etc` for `localtime` (`IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE
      | IN_DELETE | IN_ATTRIB`), because the file is a symlink that gets replaced. Only
      events naming `localtime` count.
  - For tests only (`#[doc(hidden)]`): on macOS `ZoneWatch::notify_name(name: &str)` and
    `zone::post_for_test(name: &str)` (wraps `notify_post`); on Linux
    `ZoneWatch::file(dir: &Path, name: &str)`.

- [ ] **Step 1: Write the failing tests** (`#[tokio::test]`, real time, generous bounds so
  they don't flake on a busy CI host)

```rust
#[tokio::test] async fn fires_at_an_absolute_deadline() {
    let mut a = WallAlarm::new().unwrap();
    let t0 = Instant::now();
    a.arm(SystemTime::now() + ms(300)).unwrap();
    a.fired().await.unwrap();
    assert!(t0.elapsed() >= ms(250) && t0.elapsed() < ms(2000));   // a wrong NOTE_* unit fails here
}
#[tokio::test] async fn re_arming_earlier_wins() { /* arm +5 s, then +200 ms → fires in < 2 s */ }
#[tokio::test] async fn re_arming_later_wins() { /* arm +200 ms, then +1 s → not fired at 900 ms, fired by 3 s */ }
#[tokio::test] async fn a_past_deadline_fires_at_once() { /* arm now - 60 s → fires in < 500 ms */ }
#[tokio::test] async fn disarm_stops_it() { /* arm +200 ms, disarm; timeout(600 ms, fired()) elapses */ }
#[tokio::test] async fn fires_once_per_arm() { /* after one firing, timeout(300 ms, fired()) elapses */ }
// tests/zone.rs
#[cfg(target_os = "macos")] #[tokio::test] async fn zone_watch_sees_a_post() {
    // name = format!("moa.test.{}.zone", std::process::id()) (never the system's own name)
    // ZoneWatch::notify_name(name); post_for_test(name) → changed() resolves within 2 s
}
#[cfg(target_os = "linux")] #[tokio::test] async fn zone_watch_sees_the_file_replaced() {
    // ZoneWatch::file(tmp, "localtime"); write tmp/localtime.new and rename it over tmp/localtime
    // → changed() resolves within 2 s; before that, creating tmp/other doesn't resolve it (300 ms)
}
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement** `alarm.rs`, `zone.rs` and `sys/*`. Keep every `unsafe` block to
  one syscall with its `// SAFETY:` comment.
- [ ] **Step 4: Run them and see them pass on this Mac, and check the Linux code here**

  Run: `nix develop --command cargo nextest run -p moa-platform` and
  `nix develop --command cargo check -p moa-platform --target x86_64-unknown-linux-gnu`
  Expected: PASS, and the Linux check compiles. (`nix flake check -L` builds only this
  Mac's system; `sys/linux.rs` first *runs* in the `linux` CI job.)
- [ ] **Step 5: Commit** `git commit -m "v1: wall-clock alarm (kqueue continuous / timerfd) and time-zone watch"`
- [ ] **Step 6: Open PR a1** (Tasks 1–3) as described under "PRs", and run
  `/code-review medium --fix`. Start Task 4 from `main` once it merges.

---

### Task 4: `moa-core::config` and `AccountMap::from_config`

**Files:**
- Create: `crates/moa-core/src/config.rs`, `crates/moa-core/tests/config.rs`
- Modify: `crates/moa-core/src/lib.rs`, `crates/moa-core/Cargo.toml` (`toml`),
  `Cargo.toml` (workspace `toml`), `crates/moa-store/src/accounts.rs`,
  `crates/moa-store/tests/accounts.rs`

**Interfaces:**
- Produces (`moa_core::config`):
  - `pub struct BrainName(String)`: 1–32 characters of `[a-z0-9_-]`, starting with a letter
    or digit. `BrainName::parse(&str) -> Result<BrainName, ConfigError>`,
    `BrainName::default_brain()` (= `"default"`), `as_str()`, `Display`, serde as a string.
  - `pub struct AccountName(String)`: 1–64 characters of `[A-Za-z0-9._-]`, starting with a
    letter or digit. Same methods (no default).
  - `pub struct Config { pub brains: BTreeMap<BrainName, BrainConfig>, pub accounts: BTreeMap<AccountName, AccountConfig>, pub setup: SetupConfig, pub skipped_tables: Vec<String> }` (`Default`, `Clone`, `PartialEq`, `Debug`)
  - `pub struct BrainConfig { pub folders: Vec<PathBuf>, pub moa_account: Option<AccountName>, pub open_account: Option<AccountName>, pub spike_db: Option<PathBuf> }`
  - `pub struct AccountConfig { pub kind: AgentKind, pub store: PathBuf, pub launcher: Option<PathBuf>, pub brain: Option<BrainName>, pub except: Vec<PathBuf>, pub only: Vec<PathBuf> }`
  - `pub struct SetupConfig { pub skipped: Vec<AccountName> }`
  - `Config::parse(text: &str, home: &Path) -> Result<Config, ConfigError>`:
    1. serde, with `deny_unknown_fields` on `BrainConfig`, `AccountConfig` and
       `SetupConfig`. Unknown top-level tables are collected into `skipped_tables`, for the
       caller to log as a warning (call S10).
    2. `~`/`~/x` expansion of every path.
    3. Validation.

    **Empty folder entries (`""`) are kept**; `route` ignores them (spike semantics).
  - `Config::canonicalize_with(&mut self, f: impl Fn(&Path) -> PathBuf)`: rewrites every
    folder, `except`, `only` and `store` path (empty entries stay empty).
  - `Config::from_file_text(text: Option<&str>, home: &Path, canonical: impl Fn(&Path) -> PathBuf) -> Result<Config, ConfigError>`:
    `None` → `Config::default()`, else `parse` then `canonicalize_with`. The one loader the
    CLI and the daemon both call (call S19).
  - `Config::brain(&self, b: &BrainName) -> Option<BrainConfig>`: the configured one, or an
    empty one for `default` when it isn't configured (call S10).
  - `Config::account_for_store(&self, store: &Path) -> Option<&AccountName>`
  - `pub fn caller_store(claude_config_dir: Option<&OsStr>, home: &Path) -> PathBuf`
    (call S18): the value, or `<home>/.claude` when it's unset or empty. Slice 4's hook
    reuses it.
  - `pub struct ConfigError(pub String)`: one line, always starting with `moa.toml`.
- `moa_store::accounts::AccountMap::from_config(config: &Config, home: &Path) -> AccountMap`:
  one explicit entry per configured account (name → store).

- [ ] **Step 1: Write the failing tests** (`crates/moa-core/tests/config.rs`)
  - `the_spec_example_parses`: the `moa.toml` of spec §6.9, home `/h`.
    - `brains.default.folders == ["/h/private", "/h/.config"]`, `moa_account == acct-b`;
      `brains.work.folders == ["/h/work"]`.
    - `accounts.acct-a`: store `/h/.claude-acct-a`, brain `work`,
      `except == ["/h/private", "/h/.config"]`; `acct-b.only == []`.
  - `no_file_means_one_default_brain`: `from_file_text(None, …)` equals `Config::default()`;
    its `brain(&default)` is `Some` with no folders; `brain("work")` is `None`.
  - `an_unknown_top_level_table_is_skipped`: `[layouts]\nx = 1` plus a valid brain → `Ok`,
    `skipped_tables == ["layouts"]`.
  - `errors_are_one_plain_line`: each input gives exactly this text, with no newline:

    | Input | Error |
    |---|---|
    | `[brains.Work]` | `moa.toml: "Work" isn't a brain name (use lowercase letters, digits, - and _)` |
    | `[brains.default]` + `folder = []` | starts with `moa.toml line 2:` |
    | acct-a with `except` but no `brain` | `moa.toml: acct-a has except folders but no brain to send the rest to` |
    | acct-a with `brain` and `only` | `moa.toml: acct-a can't both go to a brain and be limited to folders` |
    | acct-a `brain = "work"`, no `[brains.work]` | `moa.toml: acct-a sends its sessions to the work brain, which isn't in moa.toml` |
    | `brains.default.moa_account = "acct-z"`, no such account | `moa.toml: the default brain's moa_account acct-z isn't in [accounts]` |
    | `folders = ["src"]` | `moa.toml: the folder src in brains.default must start with / or ~` |
    | `folders = ["~bob/x"]` | `moa.toml: ~bob/x: only ~ or ~/… is understood` |
    | `kind = "gemini"` | `moa.toml: acct-a's kind must be claude or codex` |
  - `canonicalize_with_rewrites_every_path`: map `/h/link` → `/real`; folders, except, only
    and store are all rewritten.
  - `account_for_store_matches_the_store`.
  - `caller_store_defaults_to_dot_claude`: `None` → `/h/.claude`; `Some("")` →
    `/h/.claude`; `Some("/x")` → `/x`.
  - In `crates/moa-store/tests/accounts.rs`, `from_config_names_the_configured_stores`:
    `resolve("acct-a").store == Some("/h/.claude-acct-a")`, and
    `resolve("/h/.claude-acct-a").name == "acct-a"`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement.** The line number comes from the toml error's span. Other
  validation errors have no line. Parse the top level as a `toml::Table` first: take
  `brains`, `accounts` and `setup`, and list the other keys in `skipped_tables`.
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa.toml config types, validation, one loader, AccountMap::from_config"`

---

### Task 5: `moa-core::route`

**Files:**
- Create: `crates/moa-core/src/route.rs`, `crates/moa-core/tests/route_fs.rs`
- Modify: `crates/moa-core/src/lib.rs`, `crates/moa-core/Cargo.toml` (dev: `moa-platform`)

**Interfaces:**
- Consumes: Task 4's `Config`, `BrainName`, `AccountName`.
- Produces:
  - `pub enum Route { Brain(BrainName), NotAvailable }` (`Clone`, `PartialEq`, `Debug`)
  - `pub fn route(config: &Config, account: Option<&AccountName>, cwd: &Path, opener: Option<&BrainName>) -> Route`
    (spec §6.9; lesson 34). Let `home = opener` or `default`.
    1. The account is configured with `brain = B`: `cwd` under one of its `except` folders →
       `home`; otherwise → `B`.
    2. Else the account has a non-empty `only`: `cwd` under one of them → `home`; otherwise
       → `NotAvailable` (an `only` of just `""` matches nothing, as `MOA_ONLY_UNDER=":"` did).
    3. Else (no account, unknown account, no limits) → `home`.

    "A session's brain is fixed when it's first seen" is the caller's rule (slice 4), not
    `route`'s.
  - `pub fn is_under(path: &Path, folder: &Path) -> bool`: `false` for an empty `folder` or a
    relative `path`; otherwise `path.starts_with(folder)` (whole components; `Path` already
    ignores trailing slashes and interior `.`).
  - `pub fn is_under_any(path: &Path, folders: &[PathBuf]) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
// in route.rs. cfg() = the spec §6.9 example with home /h
#[test] fn the_spec_table() {
    let c = cfg(); let a = acct("acct-a"); let b = acct("acct-b");
    assert_eq!(route(&c, Some(&a), p("/h/work/x"), None), Route::Brain(brain("work")));
    assert_eq!(route(&c, Some(&a), p("/h/private/lantern"), None), Route::Brain(brain("default")));
    assert_eq!(route(&c, Some(&a), p("/h/private/lantern"), Some(&brain("work"))), Route::Brain(brain("default")));  // except never goes to the account's own brain (final-review ruling)
    assert_eq!(route(&c, Some(&b), p("/anywhere"), None), Route::Brain(brain("default")));                  // no `only` is no limit (an explicit `only = []` is a config error)
    assert_eq!(route(&c, None, p("/h/work/x"), None), Route::Brain(brain("default")));
}
proptest! {
    #[test] fn an_except_folder_routes_to_the_opener(seg in "[a-z]{1,8}", tail in "(/[a-z]{1,8}){0,3}", opener in prop::option::of("[a-z]{1,8}")) { /* brain = work, except = [/h/<seg>] */ }
    #[test] fn only_outside_its_folders_is_not_available(seg in "[a-z]{1,8}", other in "[a-z]{1,8}") { /* only = [/h/<seg>]; cwd /h/<other>x → NotAvailable */ }
    #[test] fn empty_entries_never_match(path in "(/[a-z]{1,6}){0,4}") { prop_assert!(!is_under(Path::new(&path), Path::new(""))); }
    #[test] fn a_lookalike_prefix_is_not_under(seg in "[a-z]{1,8}", extra in "[a-z0-9]{1,4}") {
        prop_assert!(!is_under(&PathBuf::from(format!("/h/{seg}{extra}")), &PathBuf::from(format!("/h/{seg}"))));
    }
    #[test] fn trailing_slashes_and_dots_dont_matter(seg in "[a-z]{1,8}") {
        prop_assert!(is_under(&PathBuf::from(format!("/h/{seg}/./x/")), &PathBuf::from(format!("/h/{seg}/"))));
    }
}
#[test] fn only_with_just_an_empty_entry_is_not_available() { /* only = [""] → NotAvailable everywhere */ }
#[test] fn a_relative_cwd_is_under_nothing() { assert!(!is_under(Path::new("private/x"), Path::new("/h/private"))); }
```

  `tests/route_fs.rs`, `a_symlinked_folder_routes_like_its_target`:
  - In a tempdir, `real/` and `link -> real`. The config says `except = ["<tmp>/link"]` and
    is loaded with `Config::from_file_text(…, moa_platform::canonical)`.
  - With `cwd = canonicalize(<tmp>/link/sub)`, the route is the opener's brain.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `route.rs`**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: routing function with whole-component matching"`

---

### Task 6: `Now`, day boundaries, ages and due dates

**Files:**
- Modify: `crates/moa-core/src/time.rs`
- Create: `crates/moa-core/src/due.rs`
- Modify: `crates/moa-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Now { pub wall: UtcMs, pub mono: Duration, pub local_day: jiff::civil::Date, pub tz: jiff::tz::TimeZone }` (`Clone`, `Debug`) (P14)
  - `Now::at(wall: UtcMs, mono: Duration, tz: TimeZone) -> Now`: `local_day` is the date of
    `wall` in `tz`.
  - `pub fn start_of_next_day(day: Date, tz: &TimeZone) -> UtcMs`: the first instant of
    `day + 1` in `tz` (jiff's `start_of_day`, so a midnight that doesn't exist gives the
    first one that does).
  - `pub fn days_ago(now: &Now, days: i64) -> UtcMs`: the same local wall time `days`
    earlier (for the spike's `datetime('now','localtime','-N day')`).
  - `pub fn age(from: Option<UtcMs>, now: UtcMs) -> String`: `?` for `None`; `{m}m` under
    an hour, `{h}h` under a day, else `{d}d`, with integer division; a future time is `0m`.
  - `pub fn local_text(t: UtcMs, tz: &TimeZone) -> String` (= `utc_to_spike_local`, the
    `YYYY-MM-DD HH:MM:SS` the spike printed).
  - `moa_core::due`: `pub enum Due { Clear, On(Date) }`,
    `pub fn parse_due(text: &str, today: Date) -> Result<Due, DueError>`:
    - `""` → `Clear`; `today` / `tomorrow` (any case) → today / today + 1;
    - `+N` (digits only) → today + N;
    - a weekday or its 3-letter form (any case) → the next one, never today (1–7 days
      ahead);
    - `YYYY-MM-DD` → that date.
    - `DueError` displays as
      `--due: '<text>' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)`.

- [ ] **Step 1: Write the failing tests**

```rust
fn ny() -> TimeZone { TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap() }
#[test] fn now_takes_the_local_day_not_the_utc_one() {
    let n = Now::at(utc("2026-11-16T04:30:00Z"), Duration::ZERO, ny());     // 23:30 on the 15th in New York
    assert_eq!(n.local_day, date(2026, 11, 15));
}
#[test] fn start_of_next_day_skips_a_missing_midnight() {
    let brt = TimeZone::posix("BRT3BRST,M10.3.0/0,M2.3.0/0").unwrap();     // DST starts at 00:00
    assert_eq!(start_of_next_day(date(2026, 10, 17), &brt), utc("2026-10-18T03:00:00Z"));
    assert_eq!(start_of_next_day(date(2026, 11, 15), &ny()), utc("2026-11-16T05:00:00Z"));
}
#[test] fn ages_like_the_spike() {
    let now = utc("2026-11-15T12:00:00Z");
    assert_eq!(age(Some(UtcMs(now.0 - 59_000)), now), "0m");
    assert_eq!(age(Some(UtcMs(now.0 - 61 * 60_000)), now), "1h");
    assert_eq!(age(Some(UtcMs(now.0 - 25 * 3_600_000)), now), "1d");
    assert_eq!(age(Some(UtcMs(now.0 + 5_000)), now), "0m");
    assert_eq!(age(None, now), "?");
}
// due.rs; today = 2026-11-15, a Sunday
#[test] fn due_words() {
    let t = date(2026, 11, 15);
    for (s, want) in [("", Due::Clear), ("today", on(15)), ("Tomorrow", on(16)), ("+0", on(15)), ("+3", on(18)),
                      ("fri", on(20)), ("Friday", on(20)), ("MON", on(16)), ("sun", on(22)), ("2026-12-01", Due::On(date(2026, 12, 1)))] {
        assert_eq!(parse_due(s, t).unwrap(), want, "{s}");
    }
    assert_eq!(parse_due("next week", t).unwrap_err().to_string(),
               "--due: 'next week' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)");
    for bad in ["+x", "-1", "2026-13-01", "fr", "+"] { assert!(parse_due(bad, t).is_err(), "{bad}"); }
}
proptest! {
    #[test] fn plus_n_is_n_days_on(n in 0i64..3650, d in 0i32..3000) { /* parse_due(&format!("+{n}"), t) == On(t + n days) */ }
    #[test] fn a_weekday_is_one_to_seven_days_ahead(d in 0i32..3000, w in 0usize..7) { /* the result has weekday w and is 1..=7 days after t */ }
    #[test] fn never_panics(s in ".*", d in 0i32..3000) { let _ = parse_due(&s, base + d.days()); }
}
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: Now, day boundaries, ages and due-date parsing"`

---

### Task 7: `moa_core::live`, the display-state model from rows

**Files:**
- Create: `crates/moa-core/src/live.rs`
- Modify: `crates/moa-core/src/lib.rs`, `crates/moa-core/Cargo.toml` (dev: `insta`),
  `Cargo.toml` (workspace `insta = { version = "1", features = ["json"] }`)
- Modify: `crates/moa-store/src/import/live.rs`: delete its own `is_space`,
  `is_line_break`, `strip`, `last_line`, `is_idle_ping` and `asks`. `activity_why` stays
  in the store and calls the core's.

**Interfaces:**
- Produces:
  - `pub enum DisplayState { Waiting, Question, Failed, Done, Working, Idle, Asleep, Ended, Archived }`:
    the declared order is the urgency order. `ALL`, serde as lowercase words, and
    `glyph()`: `⚠ ? ✗ ✓ ◐ ● 💤 ○` and `""` for archived.
  - `pub type Why = String;`
  - `pub fn display_state(s: &Session, now: UtcMs) -> (DisplayState, Why)`: `now` is unused
    until slice 4 adds "quiet". The rules, first match wins:

    | Condition | State | Why |
    |---|---|---|
    | `archived_at` set | Archived | `""` |
    | ended, `asleep_at` set | Asleep | `""` |
    | ended | Ended | `""` |
    | live, `needs_permission` | Waiting | why on one line, or `needs you` |
    | live, `needs_answer` | Question | why on one line, or `asks you` |
    | live, `failed` | Failed | why on one line |
    | live, `working` | Working | why on one line |
    | live, idle, `asks(why)` | Question | `last_line(why)` trimmed of ` *_` |
    | live, idle, `last_turn_end_at > seen_at` (`seen_at` set) | Done | `last_line(why)` trimmed of ` *_#` |
    | live, idle | Idle | the same |

    Any why that starts with `<` becomes `""`. Working comes before the `?` test because a
    working session's why is the user's own prompt (the spike checks `working` first too).
  - `pub fn sort_key(s: &Session, state: DisplayState) -> (DisplayState, std::cmp::Reverse<i64>)`:
    state, then `activity_at` newest first (`None` last).
  - `pub fn spike_state(s: &Session) -> &'static str`: `archived` / `elsewhere` / `ended` /
    `working` / `waiting` (needs_permission or needs_answer) / `idle` (idle or failed).
    It's the state word the spike's output shows, and the export's mapping.
  - `pub fn is_asleep(s: &Session) -> bool` (ended, `asleep_at` set, not archived)
  - Moved from the store, now `pub`, unchanged: `strip(&str) -> &str` (Python's `strip()`
    whitespace), `last_line(&str) -> &str`, `asks(&str) -> bool`,
    `is_idle_ping(Option<&str>) -> bool`.

- [ ] **Step 1: Write the failing tests** (in `live.rs`)
  - `the_table`: one minimal `Session` per row above → its state and why, written out as
    literal expectations. Examples: why `"Ready to merge?**"` on an idle session → Question
    with why `Ready to merge?`; a working session whose why is `can you fix it?` → Working.
  - `archived_beats_everything`: an archived session with live/needs_permission → Archived.
  - `unseen_turn_is_done_and_no_seen_is_idle`: `last_turn_end_at` 10, `seen_at` 5 → Done;
    `seen_at` None → Idle.
  - `markup_lines_are_blanked`: why `<command-name>/clear` → `""`.
  - `sorts_by_state_then_recency`.
  - `spike_state_table`.
  - proptest `every_combination_has_one_consistent_state`, over every lifecycle × activity ×
    location × `archived_at`/`asleep_at`/`seen_at`/`last_turn_end_at` presence × a why from
    `["", "ok", "why?", "<x>"]`:
    - Archived ⇔ `archived_at` set;
    - a live state (Waiting…Idle) ⇒ lifecycle live and not archived;
    - Asleep ⇔ `is_asleep(s)`;
    - Waiting ⇔ live, not archived, `needs_permission`.
  - The import's tests stay green (they now run the moved functions).
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass** (`-p moa-core -p moa-store`)
- [ ] **Step 5: Commit** `git commit -m "v1: live display-state model in moa-core (one implementation)"`
- [ ] **Step 6: Open PR a2** (Tasks 4–7), with `/code-review medium --fix`. Start Task 8
  from `main` once it merges.

---

### Task 8: Core data types: `Change`, `Snapshot`, `State`, `Model`

**Files:**
- Create in `crates/moa-core/src/`: `change.rs`, `state.rs`, `publish.rs`
- Modify:
  - `crates/moa-core/src/lib.rs`;
  - `src/model.rs`: `Serialize, Deserialize` on every row struct, plus
    `pub fn normalize_group_path(raw: &str) -> String`;
  - `crates/moa-core/Cargo.toml`: `jiff` with `serde`;
  - `crates/moa-store/src/import/rules.rs`: `group_path` wraps `normalize_group_path`.
- Test: `crates/moa-core/tests/state.rs`

**Interfaces:**
- Produces:
  - `moa_core::change::Change`, one upsert per table (by primary key), plus deletes:
    ```rust
    pub enum Change {
        Project(Project), Task(Task), Note(Note), Suggestion(Suggestion), Session(Session),
        Conversation(Conversation), PaneBinding(PaneBinding), SpikePane(SpikePane),
        Event(Event), Activity(ActivityRow), Group(Group), Ask(Ask), Thread(MoaThread),
        Feed(FeedEntry), Nag(Nag), Meta { key: String, value: Option<String> },   // None deletes
        DeletePaneBinding(SessionId), DeleteSpikePane(SessionId), DeleteGroup(String),
    }
    impl Change {
        pub fn topics(&self) -> &'static [Topic];
        pub fn row(&self) -> Option<(&'static str, String)>;   // (table, key) of the row it edits, e.g. ("tasks", "#3")
    }
    ```
    `topics`:
    - Session → `[Sessions, Status]`;
    - Conversation → `[Sessions]` (the handle);
    - Project → `[Projects, Sessions]` (session rows show the project name);
    - Activity → `[Activity]`; Event → none;
    - every other variant → its own topic, where it has one.
  - `moa_core::state`:
    - `pub struct Snapshot { pub projects, tasks, notes, suggestions, sessions, conversations, groups, asks, pane_bindings, spike_panes, threads: Vec<MoaThread>, nags: Vec<Nag>, activity_tail: Vec<ActivityRow>, meta: Vec<(String, String)>, next_ids: NextIds, unknown: Vec<LoadNote> }`
      (each a `Vec` of its row struct; `Default`)
    - `pub struct NextIds { pub project: i64, pub task: i64, pub note: i64, pub suggestion: i64, pub ask: i64, pub event: i64, pub activity: i64, pub feed: i64 }`
    - `pub struct LoadNote { pub table: &'static str, pub key: String, pub reason: &'static str }`:
      a row loaded with a default for text this build doesn't know (call S14). `key` is
      what `Change::row()` gives for that row: `#<id>`, the session's moa id, or the group's
      path.
    - `pub struct State`, with:
      - `State::new(brain: BrainName, config: Config) -> State` (empty until `Startup`);
      - read accessors for tests and views: `sessions()`, `session(id)`, `tasks()`,
        `projects()`, `groups()`, `notes()`, `suggestions()`, `conversations_of(id)`,
        `model() -> Arc<Model>`, `brain()`, `config()`;
      - `State::load(&mut self, snap: Snapshot)` (crate-private): replace every row, keep
        `rev`;
      - `State::apply(&mut self, c: &Change)` (crate-private): the in-memory twin of the
        store's SQL;
      - `State::publish(&mut self, topics: &BTreeSet<Topic>) -> Option<Arc<Model>>`
        (crate-private): rebuild those topics, `rev += 1`. `None` when the set is empty.
    - `pub struct CallerInfo { pub moa_session: Option<String>, pub claude_session: Option<String>, pub cwd: Option<PathBuf> }` (serde)
    - `pub enum Caller { Moa, Session(SessionId), Unknown { folder: String } }`, with
      `Display`: `moa`, the session's handle, or `unknown in <folder>`.
    - `State::caller(&self, c: &CallerInfo) -> Caller`, in this order:
      1. `moa_session == "moa"` → `Moa`.
      2. `moa_session` is a known session id → that session.
      3. `claude_session` is a known conversation id → its session.
      4. Otherwise `Unknown { folder: basename of cwd }`, or `an agent outside moa` when
         there's no cwd.
    - `pub fn handle_of(&self, id: SessionId) -> String` (call S8).
  - `moa_core::publish`:
    - `pub enum Topic { Brain, Sessions, Groups, Projects, Tasks, Suggestions, Asks, Activity, Status }`
      (serde lowercase)
    - `pub struct Model { pub rev: u64, pub topics: BTreeMap<Topic, Arc<BTreeMap<String, serde_json::Value>>> }`
    - `pub enum CoreEvent { Error { message: String }, Ui { text: String } }` (serde
      `tag = "kind"`, lowercase)
    - Row keys: sessions → moa id, groups → path, `status` → `"counts"`, `brain` → `"brain"`,
      the rest → integer id as text. Every row's JSON object gets a `"key"` field.
    - A `sessions` row is the `Session` row plus `handle`, `conversation`, `project` (name),
      `display_state` and `why`.
    - `status` is `{ "<state word>": n, … }`, one count per display state except ended and
      archived (slice 3's status line reads it).
    - `activity` is the newest 300 rows.

- [ ] **Step 1: Write the failing tests** (`tests/state.rs`, with a small hand-built
  `Snapshot`: two projects, three sessions with conversations, two tasks, one group)
  - `load_then_publish_builds_every_topic`: after `load` and publishing every topic, rev
    is 1, and each topic has its rows keyed as above, each with `"key"`.
  - `apply_then_publish_shows_the_change`: apply `Change::Task(<task 1, status done>)`,
    then publish `Change::topics()` → `tasks` has the new status, rev 2, and no other topic
    was rebuilt (the same `Arc`s).
  - `archiving_a_session_updates_the_status_counts`: a `Change::Session` with
    `archived_at` set → the `status` topic loses that session's count.
  - `load_keeps_rev_moving`: rev 5, then `load` another snapshot and publish → rev 6.
  - `every_change_names_its_row`: `row()` for a task is `("tasks", "#3")`, for a session
    `("sessions", <moa id>)`, and for `Meta` `None`.
  - `the_caller_is_found_by_moa_session_then_conversation`: the four cases above.
  - `handles_use_the_current_conversation`: a session with two conversations shows the
    later one's first 8 characters; with none, its moa id's.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: core data types: Change, Snapshot, State, Model"`

---

### Task 9: `handle`, `Input`, `Effect`, the transaction, activity texts

**Files:**
- Create in `crates/moa-core/src/`: `effect.rs`, `activity.rs`, `op.rs`, `core.rs`
  (`handle` and `Tx`), `ops/mod.rs`, `ops/groups.rs`
- Modify: `crates/moa-core/src/lib.rs` (re-export `State`, `Input`, `Effect`, `handle`,
  `Now`), `.github/workflows/ci.yml` (the `linux` job; Step 5)
- Test: `crates/moa-core/tests/skeleton.rs`, `crates/moa-store/tests/activity_wording.rs`

**Interfaces:**
- Consumes: Task 8's types; `start_of_next_day` (Task 6).
- Produces:
  - `moa_core::op`:
    - `pub struct ConnId(pub u64)`, `pub struct ReqId(pub u64)`
    - `pub enum Op` (serde `tag = "method", content = "params"`; each variant renamed to its
      method name; `pub const METHODS: &'static [&'static str]`). This task adds
      `#[serde(rename = "group.add")] GroupAdd { path: String }`. Tasks 12–14 add the rest.
    - `pub struct Done { pub text: String, pub data: serde_json::Value }`
    - `pub struct OpError(pub String)`: one human line (the ambiguous-name list in Task 12
      is the one multi-line case, as in the spike).
  - `moa_core::effect`:
    ```rust
    pub enum Input { Startup(Box<Snapshot>), Reload { snapshot: Box<Snapshot>, refuse_writes: Option<RefuseWrites> },
                     Client { conn: ConnId, req: ReqId, caller: CallerInfo, op: Op },
                     Timer(TimerKey), ZoneChanged, ActorFailed { actor: &'static str, message: String } }
    pub struct RefuseWrites { pub until: UtcMs, pub reason: String }   // call S4
    pub enum TimerKey { Backup }
    pub enum Effect { Persist(Vec<Change>), SetTimer { key: TimerKey, at: UtcMs }, CancelTimer(TimerKey),
                      Publish(Publish), Reply { conn: ConnId, req: ReqId, result: Result<Done, OpError> },
                      Backup { day: Date }, Log(LogLine) }
    impl Effect { pub fn runs_at_once(&self) -> bool }   // SetTimer, CancelTimer, Log (call S4)
    pub enum Publish { Model(Arc<Model>), Event(CoreEvent) }
    pub struct LogLine { pub level: LogLevel, pub message: String }   // contents-free (S15)
    pub fn check_order(effects: &[Effect]) -> Result<(), &'static str>  // every Persist before any Publish or Reply
    pub fn handle_checked(state: &mut State, input: Input, now: Now) -> Vec<Effect>  // handle, then panic if check_order fails
    pub fn reply_of(effects: &[Effect]) -> Option<&Result<Done, OpError>>
    ```
  - `pub fn handle(state: &mut State, input: Input, now: Now) -> Vec<Effect>`:
    - `Startup` → `load` the snapshot, then `Backup { day: now.local_day }`,
      `SetTimer { Backup, start_of_next_day(now.local_day, &now.tz) }`, a `Log` per
      `unknown` note (table and key only), and a model publish.
    - `Reload` → `load` and keep `refuse_writes`; a model publish of every topic, and the
      same derived timers as `Startup` (`SetTimer { Backup, start_of_next_day(…) }`), so a
      timer lost with a failed or panicked drain is set again. No re-validation and no
      backup (call S4).
    - `Timer(Backup)` → `Backup { day }` + the next `SetTimer`.
    - `ZoneChanged` → a new `SetTimer` from `now.tz`. The executor has already updated the
      zone in `Now`.
    - `ActorFailed` → `Publish(Event(Error { "<actor> stopped and was restarted" }))` +
      `Log`.
    - `Client` → `ops::run(state, &mut tx, caller, op, &now)`. The effects are
      `[Persist(tx.changes)]` if any, then `Publish(Model)` if a topic changed, any
      `Publish(Event)`, then `Reply`.
  - `Tx` (crate-private):
    - `commit(&mut self, state: &mut State, changes: Vec<Change>) -> Result<(), OpError>`.
      It checks **every** change first, then applies all of them to `state` and records
      them. The checks:
      - `refuse_writes` is set and `now.wall < until` (call S4) →
        `OpError("moa can't save right now (<reason>); nothing was changed")`. At or after
        `until` the check passes, so the next write probes the store;
      - a change whose `row()` is in the snapshot's `unknown` notes →
        `OpError("<table word> <key> has a value this build doesn't know; update moa")`
        (`task #3`, `session <handle>`, …; call S14).
    - `ui(&mut self, state, now, text) -> Result<(), OpError>`: commits a `ui` `Event` row
      plus its activity row (`who = you`, the same text), as the spike's `log_ui` and
      `a_event` trigger did, and queues `CoreEvent::Ui`.
    - `next_id(Table) -> i64`.
    - **Rule for every op handler: validate the input, build the changes, then one
      `tx.commit`.** A handler that returns `Err` has applied nothing.
  - `moa_core::activity`: the spike's trigger texts, as functions returning `String`:

    | fn | Text |
    |---|---|
    | `new_project(name, status)` | `new project {name} ({status})` |
    | `project_status(name, old, new)` | `project {name}: {old} → {new}` |
    | `new_task(title, project: Option<&str>)` | `new task "{title}" in {project}`, or without ` in …` |
    | `task_status(title, old, new)` | `task "{title}": {old} → {new}` |
    | `added_to_today(title)` | `added to today: "{title}"` |
    | `noted(kind, text)` | `noted ({kind}): {first 100 chars of text}` |
    | `suggests(id, text)` | `suggests #{id}: {text}` |
    | `linked(handle, project)` | `linked session {handle} → {project}` |
  - `ops::groups::add`: `group add PATH`.
    - `normalize_group_path`; empty → `a group needs a name`.
    - An existing group → no change; else commit `Group { path, collapsed: false,
      created_at: now }`.
    - Then `ui("new group {path}")` either way, as the spike logged it.
    - Reply `ok`, data `{"path": …}`.

- [ ] **Step 1: Write the failing tests**
  - `crates/moa-core/tests/skeleton.rs` (a small hand-built `Snapshot`; Task 11 brings the
    real fixture):
    - `startup_arms_the_backup_and_publishes`: `handle_checked(Startup)` at 2026-11-15 12:00
      EST → contains `Backup { day: 2026-11-15 }`, `SetTimer { Backup, 2026-11-16T05:00Z }`
      and `Publish(Model)`.
    - `reload_only_replaces_the_state`: `Reload` → exactly one `Publish(Model)`, rev one
      higher than before, and `Startup`'s `SetTimer { Backup, … }`; no `Backup` or
      `Persist`.
    - `the_backup_timer_backs_up_and_re_arms`.
    - `a_zone_change_re_arms_with_the_new_zone`: `ZoneChanged` with `now.tz = UTC0` →
      `SetTimer` at the next UTC midnight.
    - `group_add_persists_then_publishes_then_replies`: the effects are exactly, in order:
      1. `Persist` holding the `Group`, the `ui` `Event` and the `Activity` row;
      2. `Publish(Model)`, whose `groups` topic has the new path;
      3. `Publish(Event(Ui))`;
      4. `Reply(Ok("ok"))`.

      The event and activity ids are the snapshot's `next_ids`, and both advance.
    - `adding_an_existing_group_changes_nothing_but_logs`.
    - `an_empty_group_name_is_refused_and_nothing_is_applied`: `group add "//"` →
      `Err("a group needs a name")`, no `Persist`, and the model rev unchanged.
    - `refused_writes_change_nothing_until_the_deadline`: after
      `Reload { refuse_writes: Some(RefuseWrites { until: now + 2 s, reason: "disk full" }) }`:
      - `group add X` at `now` → `Err("moa can't save right now (disk full); nothing was
        changed")`, and no `Persist`;
      - `group add X` at `now + 2 s` → `Ok`, with a `Persist`; no other input came between.
    - `a_row_with_an_unknown_value_is_not_edited`: a snapshot with
      `unknown = [("groups", "Private")]` → `Tx::commit` of that group's change is refused
      with `group Private has a value this build doesn't know; update moa`. (Tasks 12–13
      repeat this through real verbs.)
    - `check_order_rejects_a_publish_before_a_persist` (a hand-made effect list).
    - `only_timers_and_logs_run_at_once`: `runs_at_once` is true for `SetTimer`,
      `CancelTimer` and `Log` only.
    - `actor_failure_is_published`.
  - `crates/moa-store/tests/activity_wording.rs`, `activity_texts_match_the_spike_triggers`:
    - In a spike-schema DB (`SCHEMA_SQL` + `TRIGGERS_SQL`), run the inserts and updates
      that fire each trigger: project insert and status change, task insert with and
      without a project, task status change, task `today` set, note insert with 150 chars
      of text, suggestion insert, session project link.
    - Each `activity.text` equals the matching `moa_core::activity` function's output for
      the same values.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Keep the core pure.** Add a step to the `linux` job in
  `.github/workflows/ci.yml`, after clippy:

  ```yaml
      - name: moa-core stays free of IO crates
        run: |
          out=$(cargo tree -p moa-core -e normal --prefix none)
          ! grep -E '^(tokio|rusqlite|libc) ' <<<"$out"
  ```

  - The `$(…)` assignment fails the step if `cargo tree` itself fails.
  - It's a CI step rather than a nextest test because the Nix build sandbox can't run
    `cargo tree`.
  - Check it locally with the same two lines inside `nix develop --command bash -c '…'`:
    exit 0.
- [ ] **Step 6: Commit** `git commit -m "v1: the core: handle, Input, Effect, Tx, activity texts, ordering rule"`

---

### Task 10: Store rows, `load_snapshot`, `apply`

**Files:**
- Create: `crates/moa-store/src/rows.rs` (row ↔ struct), `src/snapshot.rs`,
  `tests/snapshot.rs`
- Modify: `crates/moa-store/src/lib.rs` (`pub use moa_core::state::Snapshot;`)

**Interfaces:**
- Consumes: `Change`, `Snapshot`, `NextIds`, `LoadNote` (Task 8).
- Produces:
  - `Store::load_snapshot(&self) -> Result<Snapshot, StoreError>`:
    - every row of every table except `events` and `feed`;
    - `activity_tail` = the newest 300 by id;
    - `next_ids` = `coalesce(max(id), 0) + 1` per table;
    - an unknown enum value loads as the import's default for that column (G3), and is
      listed in `unknown` (call S14).
  - `Store::apply(&mut self, changes: &[Change]) -> Result<(), StoreError>`: one
    transaction; `INSERT … ON CONFLICT(<pk>) DO UPDATE SET <every column>` per upsert.

- [ ] **Step 1: Write the failing tests** (`tests/snapshot.rs`, on the imported Typical
  fixture, as slice 1's tests build it)
  - `typical_snapshot_has_every_row`:
    - projects 3, sessions 9, tasks 5, notes 4, suggestions 4, asks 3, groups 2,
      conversations 9, spike_panes 2;
    - `next_ids.task == 6`;
    - `activity_tail` holds the newest rows, at most 300; `unknown` is empty.
  - `every_change_round_trips`: for each `Change` variant, one sample row → `apply` →
    `load_snapshot` contains an equal row. The three deletes and `Meta { value: None }`
    remove theirs.
  - `unknown_enum_text_loads_with_a_note`: raw SQL sets task 1's status to `someday` → it
    loads as `todo`, and `unknown == [LoadNote { table: "tasks", key: "#1", … }]`.
  - `a_bad_batch_changes_nothing`: `apply([a project, a task whose project_id is 999])` →
    `Err` (foreign key), and neither row is there.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: store snapshot and apply for every table"`

---

### Task 11: The store actor, daily backups, the testkit's typical brain

**Files:**
- Create: `crates/moa-store/src/actor.rs`, `tests/actor.rs`,
  `crates/moa-testkit/src/brain.rs`, `crates/moa-testkit/tests/brain.rs`
- Modify: `crates/moa-store/Cargo.toml` (`tokio` with `sync` only),
  `crates/moa-testkit/Cargo.toml` (`moa-core`, `moa-agents`, `jiff`),
  `crates/moa-testkit/src/lib.rs`

**Interfaces:**
- Consumes: `load_snapshot`, `apply` (Task 10); `daily_backup`, `prune_backups`, `Pruned`
  (Task 1); `handle_checked`, `reply_of` (Task 9).
- Produces:
  - `moa_store::actor`:
    ```rust
    pub enum CommitError { Failed(String), Stopped }   // Failed: one line; Stopped: the thread is gone
    pub struct BackupDone { pub written: Option<PathBuf>, pub pruned: Pruned }
    pub struct StoreHandle { /* std::sync::mpsc::Sender<Cmd> */ }   // Clone
    impl StoreHandle {
        pub fn spawn(store: Store, backups: PathBuf) -> StoreHandle;
        pub async fn commit(&self, changes: Vec<Change>) -> Result<(), CommitError>;   // one transaction
        pub async fn snapshot(&self) -> Result<Snapshot, CommitError>;
        pub async fn backup(&self, day: Date) -> Result<BackupDone, CommitError>;
        pub async fn sync(&self);                                    // a no-op that answers in queue order
        #[doc(hidden)] pub fn fault(&self, f: Fault);                // tests only
    }
    #[doc(hidden)] pub enum Fault { FailNextCommit, PauseNextCommit(std::sync::mpsc::Receiver<()>), Crash }
    ```
    - The thread runs its commands one at a time, in queue order. Each `commit` is one
      transaction: all of its changes or none.
    - Every command answers on its own oneshot. **When the thread dies, its senders drop,**
      so a waiting or later call returns `CommitError::Stopped`, never hangs. That is 2b's
      death signal.
    - The thread does no batching of its own: the core sends one batch per drain and
      waits for it (call S4).
    - `Fault` (tests only):
      - `FailNextCommit` makes the next commit fail as if SQLite refused it.
      - `PauseNextCommit(rx)` blocks before the next commit until `rx` receives.
      - `Crash` panics the thread.

      A fault is a queued command too, so it applies to the next commit *after* it.
  - `moa_testkit::brain`:
    ```rust
    pub const TYPICAL_TZ: &str = "EST5EDT,M3.2.0,M11.1.0";
    pub const TYPICAL_NOW: &str = "2026-11-15T17:00:00Z";        // Sunday 12:00 in TYPICAL_TZ
    pub struct TypicalBrain { pub dir: TempDir, pub home: PathBuf, pub db: PathBuf }
    impl TypicalBrain {
        pub fn new() -> TypicalBrain;           // SpikeFixture Typical in <dir>/home, imported with Derived ids, brain "default"
        pub fn store(&self) -> Store;           // Store::open(db)
        pub fn snapshot(&self) -> Snapshot;
        pub fn config(&self) -> Config;         // brains.default.folders = [<home>/private]
        pub fn now(&self) -> Now;               // Now::at(TYPICAL_NOW, 0, TYPICAL_TZ)
        pub fn state(&self) -> State;           // State::new(default, config) + handle(Startup)
    }
    pub fn run(state: &mut State, op: Op, now: &Now) -> Result<Done, OpError>;   // handle_checked + reply_of
    ```

- [ ] **Step 1: Write the failing tests**
  - `actor.rs` (`#[tokio::test]`):
    - `a_commit_is_one_transaction`: `commit([a project, a task whose project_id is 999])`
      → `Failed(_)` (foreign key), and neither row is on disk.
    - `an_injected_failure_answers_failed_and_the_next_commit_works`: `FailNextCommit`, then
      `commit([a project, a task in it])` → `Failed(_)`, nothing on disk. The next `commit` of
      the same changes → `Ok`. (The fault answers before applying anything, so atomicity is
      `a_commit_is_one_transaction`'s to prove.)
    - `commands_answer_in_queue_order`: `PauseNextCommit(rx)`, a `commit`, then a
      `snapshot()`. Release `rx`: the snapshot has the committed row.
    - `a_dead_thread_answers_stopped`: `Crash` → the next `commit` and `snapshot` return
      `Stopped` within 2 s.
    - `backup_writes_today_once_and_prunes`: 20 old `daily-*.db` files exist. `backup(day)`
      → `written == Some(daily-<day>.db)`, and `pruned.removed.len()` equals what
      `backups_to_delete` picks over the 21 dates. A second `backup(day)` → `written ==
      None`.
  - `crates/moa-testkit/tests/brain.rs`, `typical_brain_starts`: `state().sessions().len()
    == 9`, and `run(group add X)` is `Ok`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: the store actor (one transaction per commit, Stopped on death), daily backup + prune; testkit TypicalBrain"`
- [ ] **Step 6: Open PR a3** (Tasks 8–11), with `/code-review medium --fix`. Start Task 12
  from `main` once it merges.

---

### Task 12: Sessions and groups: names, `ls`, `set`, `group rename|rm`, `archive`

**Files:**
- Create: `crates/moa-core/src/names.rs`, `src/ops/sessions.rs`,
  `crates/moa-core/tests/sessions.rs` (+ `tests/snapshots/`)
- Modify: `src/op.rs`, `src/ops/mod.rs`, `src/ops/groups.rs`;
  `crates/moa-core/Cargo.toml` (dev: `moa-testkit`; this dev-dependency cycle works for
  `tests/*.rs` only, as in slice 1)

**Interfaces:**
- Consumes: `TypicalBrain`, `run` (Task 11); `display_state`, `spike_state`, `is_asleep`
  (Task 7); `age`, `local_text`, `days_ago` (Task 6); `Tx::commit`, `Tx::ui` (Task 9).
- Produces:
  - `State::resolve(&self, q: &str) -> Result<SessionId, OpError>` and
    `State::candidates(&self, q: &str) -> Vec<SessionId>` (spec §6.10, R1 §2.13). Case is
    ignored throughout.
    1. **Prefix**: sessions whose moa id (hyphenated) or any conversation id starts with
       `q`, archived ones included.
    2. If there are none, **substring**: non-archived sessions whose title, project name or
       `group_path` contains `q`, newest `last_seen_at` first (`None` last). If any title
       equals `q`, only those are kept.
    3. `resolve`: one → it. None → `no session matches '<q>'`. Several →
       `<n> sessions match '<q>'; be more specific:` plus up to 8 lines of
       `  {handle}  {title or -}  ({group or project or -}, {spike_state})`.
  - New `Op`s:

    | Method | Variant | Text (spike format) | data |
    |---|---|---|---|
    | `session.ls` | `SessionLs { all: bool }` | per session: `{handle}  {alive:<9} {account:<12} {group:<22} {title:<34} {cwd}  last {last}` | `[{id, conversation, alive, state, account, group, title, cwd, last}]` |
    | `session.set` | `SessionSet { q, title: Option<String>, group: Option<String> }` | `ok` | the session row |
    | `session.archive` | `SessionArchive { qs: Vec<String> }` | `archived {n}` | `{"archived": [ids]}` |
    | `group.rename` | `GroupRename { old, new }` | `ok` | `{"old", "new"}` |
    | `group.rm` | `GroupRm { path }` | `ok` | `{"path"}` |

    - **ls**: shows sessions that are working, idle, waiting or elsewhere, or whose
      `last_seen_at` is within 2 days (`days_ago(now, 2)`), or that are asleep; `all` shows
      every session. Newest `last_seen_at` first.
      - `alive` = `asleep` or `spike_state`. The live-pane check and the untracked list
        come in slice 4.
      - `group` = `group_path`, else the project name, else `-`.
      - `title` = title, else the first 40 characters of `last_prompt`.
      - `cwd` and `last` (`local_text`) show `-` when missing.
    - **set**: refuses no flags (`set needs --title or --group`) and `--title ""`
      (`--title can't be empty`).
      - A title sets `title_locked = true`.
      - A group is normalised; its row is created if missing.
      - ui event: `session {new title or old title or handle}`, then ` → {group}` when a
        group was given, else ` renamed`.
    - **archive**: resolves every name before changing anything. Sets `archived_at = now`;
      ui `archived {title or handle}` per session.
    - **group rename**: renames the subtree (the path itself and `old/…`) in sessions and
      groups, and ensures `new` exists. ui `group {old} → {new}`. Refused when no group or
      session has `old`: `no group {old}`.
    - **group rm**: refused while a non-archived session is in the subtree:
      `{path} still has {n} session(s); move or archive them first`. Otherwise it deletes
      the subtree's groups; ui `removed group {path}`.

- [ ] **Step 1: Write the failing tests** (`tests/sessions.rs`, on `TypicalBrain`; `S1`…`S9`
  are `moa_testkit::ids`; `h(S)` is the first 8 characters of the conversation id)

```rust
#[test] fn names_resolve_like_the_spike() {
    let (b, mut st) = typical();
    assert_eq!(st.resolve(&S1_WORKING[..8]).unwrap(), sid(&st, S1_WORKING));            // conversation prefix
    assert_eq!(st.resolve(&S1_WORKING[..6].to_uppercase()).unwrap(), sid(&st, S1_WORKING));
    let moa_id = sid(&st, S1_WORKING).to_string();
    assert_eq!(st.resolve(&moa_id[..13]).unwrap(), sid(&st, S1_WORKING));               // moa id prefix, hyphen included
    assert_eq!(st.resolve("dimmer").unwrap(), sid(&st, S1_WORKING));                    // title substring
    assert_eq!(st.resolve("Beach scan").unwrap_err().0, "no session matches 'Beach scan'");  // archived: not by name…
    assert_eq!(st.resolve(&S6_ARCHIVED[..8]).unwrap(), sid(&st, S6_ARCHIVED));          // …but by prefix
    let e = st.resolve("lantern").unwrap_err().0;                                       // project name: S1 S3 S9 S5
    assert!(e.starts_with("4 sessions match 'lantern'; be more specific:\n"));
    assert_eq!(e.lines().skip(1).map(|l| l.split_whitespace().next().unwrap()).collect::<Vec<_>>(),
               [&S1_WORKING[..8], &S3_WAITING[..8], &S9_WAITING_IDLE[..8], &S5_ASLEEP[..8]]);
}
#[test] fn an_exact_title_wins() { /* set S9's title to "Fix the flicker again"; resolve("fix the flicker") == S3 */ }
#[test] fn moa_ids_that_share_eight_characters_are_ambiguous_but_handles_are_not() {
    // hand-built snapshot: two sessions with ids 0195f3a2-…-0001 and -0002, conversations with different v4 ids
    // resolve("0195f3a2") lists both; resolve(<each conversation prefix>) finds each; handle_of() differs
}
#[test] fn ls_matches_the_spike_layout() { insta::assert_snapshot!(run(&mut st, SessionLs { all: false }).text); }   // and all: true
#[test] fn set_title_and_group() {
    // set S2 --group Work/Infra --title "Tide tables" → ok; group row exists; title_locked;
    // the ui event text is "session Tide tables → Work/Infra"
}
#[test] fn set_with_no_flag_is_refused() { /* "set needs --title or --group"; no Persist */ }
#[test] fn a_session_with_an_unknown_value_is_not_edited() {
    // snapshot with unknown = [("sessions", <S2's moa id>)]: set S2 --title x → "session b84e07d1 has a value this build doesn't know; update moa"
}
#[test] fn archive_resolves_everything_first() { /* archive [h(S4), "nope"] → Err("no session matches 'nope'"); S4 unchanged */ }
#[test] fn group_rename_moves_the_subtree() {
    // groups Private, Private/Lantern; S1 in Private/Lantern; rename Private → Home
    // → groups Home, Home/Lantern; S1 in Home/Lantern; "Private2"-style lookalikes untouched
}
#[test] fn group_rm_refuses_while_sessions_remain() {
    // rm Private → "Private still has 1 session(s); move or archive them first"
    // archive S1, then rm Private → ok, both groups gone
}
```

- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement** `names.rs`, `ops/sessions.rs` and the group ops.
- [ ] **Step 4: Run them and see them pass**, and review and commit the snapshots
  (`nix develop --command cargo insta review`).
- [ ] **Step 5: Commit** `git commit -m "v1: session names, ls, set, archive, group rename/rm in the core"`

---

### Task 13: Tasks, notes, projects

**Files:**
- Create: `crates/moa-core/src/ops/tasks.rs`, `src/ops/projects.rs`,
  `crates/moa-core/tests/tasks.rs`, `crates/moa-core/tests/projects.rs`
- Modify: `src/op.rs`, `src/ops/mod.rs`

**Interfaces:**
- Consumes: `parse_due` (Task 6), `resolve` (Task 12), `activity::*` and `Tx` (Task 9),
  `is_under_any` (Task 5).
- Produces new `Op`s. Every field is the user's raw text, so the core gives the one-line
  errors:
  ```rust
  #[serde(rename = "task.ls")]  TaskLs { project: Option<String>, all: bool },
  #[serde(rename = "task.add")] TaskAdd(TaskAdd),
  #[serde(rename = "task.set")] TaskSet(TaskSet),
  #[serde(rename = "note.add")] NoteAdd { text: String, project: Option<String>, kind: String },
  #[serde(rename = "project.show")] ProjectShow { name: String },
  #[serde(rename = "project.add")]  ProjectAdd { name: String, path: PathBuf, summary: Option<String> },
  pub struct TaskAdd { pub project: String, pub title: String, pub today: bool, pub status: Option<String>, pub blocked_on: Option<String>,
                       pub who: Option<String>, pub due: Option<String>, pub session: Option<String>, pub note: Option<String>, pub priority: Option<String> }
  pub struct TaskSet { pub id: i64, pub status: Option<String>, pub title: Option<String>, pub who: Option<String>, pub due: Option<String>,
                       pub priority: Option<String>, pub session: Option<String>, pub note: Option<String>, pub blocked_on: Option<String>,
                       pub today: bool, pub not_today: bool }
  ```
  For `TaskSet`, `Some("")` clears `due`, `priority`, `session`, `note` and `blocked_on`.
- Behaviour (spike `moactl.py`, with call S13's fixes):
  - **task ls** (`all` keeps done and dropped):
    - Order: project name in byte order, with no-project first, then task id. A
      `## {project or -}` heading starts each project.
    - Line: `- #{id} [{status}] {title}`, then `  ({extras})` when there are any, joined by
      `, `, in this order: `P0`/`P1`/`P2`, `today` (when `today == now.local_day`),
      `who={who}`, `due={due}`, `on={blocked_on}`, `session={handle}`.
    - `project` filters by exact name, ignoring case. Nothing to show → `no open tasks`.
    - data: the task rows plus `project` names.
  - **task add**:
    - The project is found by exact name, ignoring case, else
      `no project '<P>' (moactl project NAME to check)`.
    - An empty title → `a task needs a title`.
    - `status` = the given one, else `blocked` when `blocked_on` is set, else `todo`. It's
      validated: `--status: '<s>' isn't one of idea, todo, active, waiting, blocked, done,
      dropped`.
    - `who` defaults to `you`. `who` and `priority` are validated against their enums, and
      `priority ""` is none.
    - `due` goes through `parse_due(now.local_day)`; `today` = `now.local_day` when asked;
      `session` through `resolve`.
    - `created_at = updated_at = now`, and `done_at = now` when the status is done.
    - Activity `new_task`. Text `task #{id} added`; data the row.
  - **task set**:
    - `no task #{id}`; `--title can't be empty`.
    - A non-empty `blocked_on` also sets the status to `blocked` unless `--status` was given.
    - `today` wins over `not_today`.
    - Every given field is updated, then `updated_at = now` if anything changed.
      `done_at = now` when the status becomes done, `None` when it leaves done.
    - Activity: `task_status(<title before this call>, old, new)` on a status change;
      `added_to_today(<title after this call>)` when `today` becomes a new non-null date.
    - Text `ok`; data the row.
  - **note**:
    - `kind` defaults to `fact`, and any kind is kept.
    - The project is found by exact name, ignoring case. An unknown one gives
      `noted (no project '<P>'; saved without one)`. Otherwise text `noted`.
    - Activity `noted`.
  - **project show**:
    - Lookup: exact name, ignoring case, else the lowest-id project whose name contains
      `name`; none → `no project matches '<name>'`.
    - Text, missing values shown as `-`:
      ```
      {name} [{status}] {path}  last activity {last_activity_on}
      {summary or ''}
        task #{id} {title} [{status}]{ blocked on X}            (open tasks, by id)
        session {handle} {label} {spike_state} {age}  last prompt: {first 80 chars}   (10 newest by last_seen_at)
        note ({kind}) {first 200 chars}                         (10 newest by id)
      ```
      `label` = `{title or first 50 chars of last_prompt or handle} [{group or project or -}]`.
    - data `{project, tasks, sessions, notes}`.
  - **project add** (`path` is absolute; 2b makes it so, and canonical):
    - Refused unless `path` is under one of this brain's folders (`is_under_any`):
      `{path} isn't in this brain's folders; this brain doesn't track it`. A brain with no
      folders gives `the {brain} brain has no folders yet; add them to moa.toml`.
    - A duplicate name (ignoring case) or path → `there's already a project named '<name>'
      or for {path}`.
    - Inserts `status active`, `last_activity_on = now.local_day`, `created_at` and
      `updated_at = now`.
    - Links every session whose `cwd` is under `path` (whole components) and that has no
      project: activity `linked(handle, name)` for each.
    - Activity `new_project(name, active)`; ui `new project {name} ({path})`.
    - Text `project {name} added for {path}`, plus `; {n} session linked` or
      `; {n} sessions linked` when `n > 0`. data `{project, linked: [ids]}`.

- [ ] **Step 1: Write the failing tests** (on `TypicalBrain`; today is 2026-11-15)
  - `tasks.rs`:
    - `task_ls_matches_the_spike_layout`: an insta snapshot. It must show `## -` first,
      then `## Lantern` with `- #1 [todo] Order the dimmer board  (P1, today, who=you,
      due=2026-11-16, session=3f9a1c52)`.
    - `task_add_with_relative_due_and_session`:
      `TaskAdd { project: "lantern", title: "Solder the board", due: "fri", session: "dimmer", priority: "p0", .. }`
      → `task #6 added`. The row has due 2026-11-20, who you, status todo and session S1,
      and activity `new task "Solder the board" in Lantern`.
    - `task_add_refusals`: unknown project, empty title, `--status someday`, `--due next
      week`, an ambiguous `--session lantern`. Each gives its exact line, and none applies
      anything.
    - `task_set_fields_and_clears`:
      - `TaskSet { id: 1, due: "", priority: "", .. }` clears both;
      - `{ id: 1, status: "done" }` sets `done_at`, with activity `task "Order the dimmer
        board": todo → done`;
      - `{ id: 1, status: "active" }` clears `done_at`;
      - `{ id: 1, blocked_on: "parts" }` → status blocked.
    - `task_set_title_and_today_wording`: `{ id: 3, title: "Survey again", today: true }` →
      activity has `added to today: "Survey again"`.
    - `due_today_at_half_past_eleven_is_today`: `now` = 2026-11-16T04:30Z (23:30 on the 15th
      in New York); `TaskAdd { due: "today", today: true }` → due and today are both
      2026-11-15.
    - `no_task_and_empty_title`: `no task #99`; `--title can't be empty`.
    - `a_task_with_an_unknown_value_is_not_edited`: a snapshot whose `unknown` holds
      `("tasks", "#3")` → `task set 3 --status done` → `task #3 has a value this build
      doesn't know; update moa`.
  - `projects.rs`:
    - `project_show_matches_the_spike_layout` (insta, `Lantern`); `project_show_by_substring`
      (`tide` → Tidepool); `no project matches 'nope'`.
    - `project_add_links_sessions_and_logs`. Every Typical session's cwd is
      `<home>/private/lantern`.
      - Add `Harbour` at `<home>/private/lantern/harbour` → exactly
        `project Harbour added for <that path>` (nothing is under it).
      - On a fresh brain, add `Shore` at `<home>/private` → `…; 3 sessions linked`. Those are
        S2, S7 and S8, the sessions with no project. Each gets a
        `linked session <handle> → Shore` activity row; S1 keeps Lantern.
      - Either add also writes the ui event `new project <name> (<path>)` and the activity
        `new project <name> (active)`.
    - `project_add_checks_the_folder_by_whole_components`: `<home>/private2/x` → refused;
      `<home>/private` itself → accepted.
    - `project_add_refuses_duplicates`: name `lantern` at a new path → refused; a new name at
      `<home>/private/lantern` (Lantern's path) → refused.
    - `note_with_unknown_project_is_kept`:
      `NoteAdd { text: "lamp is 12 V", project: "nope", kind: "fact" }` → the text above; a
      note row with `project_id` None; activity `noted (fact): lamp is 12 V`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement** `ops/tasks.rs` and `ops/projects.rs`.
- [ ] **Step 4: Run them and see them pass**, and review the snapshots.
- [ ] **Step 5: Commit** `git commit -m "v1: task ls/add/set with due dates, notes, project show/add with back-linking"`

---

### Task 14: `brief`, `attention`, `find`, and the model-equals-disk check

**Files:**
- Create: `crates/moa-core/src/ops/read.rs`, `crates/moa-core/tests/read.rs`,
  `crates/moa-store/tests/core_and_store.rs`
- Modify: `src/op.rs`, `src/ops/mod.rs`

**Interfaces:**
- Produces new `Op`s: `#[serde(rename = "brief")] Brief`,
  `#[serde(rename = "attention")] Attention`,
  `#[serde(rename = "session.find")] SessionFind { q: String }`.
  - Their data: brief and attention give `{"text": …}`; find gives
    `[{id, conversation, title, group, state, age, cwd}]`.
  - **find**: `candidates(q)` (Task 12), the first 15. Line:
    `{handle}  {label}  {spike_state}  {age(last_seen_at)}  {cwd or -}`. No match prints
    nothing.
  - **attention**: the lines in this order, at most 20 (the brief passes 8); none at all →
    `nothing needs you right now`. Sessions are picked by `spike_state` (Task 7), as the
    spike picked them by its one `state` column, so archived and elsewhere sessions never
    appear.
    1. `spike_state == "waiting"`, oldest `last_seen_at` first:
       `- ⚠ {label} is blocked on you (permission/question) for {age}  → moactl resume {handle}`.
    2. `spike_state == "idle"`, seen within the last day (`days_ago(now, 1)`), oldest
       first: `- ● {label} finished its turn {age} ago; your move  → moactl resume {handle}`.
    3. Open tasks whose `today` is today: `- ▸ today: {project or -}: {title} [{status}]`.
    4. Blocked tasks: `- ⧗ blocked: {project or -}: {title} on {blocked_on or ?}`.
    5. Projects with open tasks (todo, active, waiting) whose `last_activity_on` is set and
       more than 7 days ago, oldest first (a project with no date is skipped, as SQL's
       `NULL < x` skipped it): `- ◷ {name}: {n} open task(s), untouched since {date}`.
    6. Open suggestions by id: `- ? suggestion #{id}: {text}`.
  - **brief**, joined by `\n`:
    ```
    # Brief ({now local, "%a %d %b %H:%M"}). More: moactl attention | project NAME | ls | find Q
    ## Today: {"; "-joined "#{id} {project or -}: {title} [{status}]" for tasks with today == today, or "nothing set yet"}
    ## Needs attention
    {attention, at most 8, or "- nothing"}
    ## Active this week: {projects with last_activity_on in the last 7 days, newest first, ", "-joined, or "none"}
    ## Live sessions
    {up to 12 of working/idle/waiting/elsewhere spike states, newest last_seen_at first: "- {handle} {label} {spike_state} {age}", or "- none"}
    ## Asleep (idle, window closed to save memory; moactl resume ID): {up to 12 "{handle} {label}", newest asleep_at first, ", "-joined, " …" when more}   (only when any are asleep)
    ## What you know about the user (latest notes)
    {10 newest preference/routine/fact notes: "- ({kind}) {first 200 chars}"}
    ## Last thread ({local date of its created_at}): {first 600 chars}   (the newest thread-summary note, when there is one)
    ```

- [ ] **Step 1: Write the failing tests**
  - `read.rs` (on `TypicalBrain` at `TYPICAL_NOW`):
    - `brief_matches_the_spike_layout` and `attention_matches_the_spike_layout` (insta). The
      brief's first line is `# Brief (Sun 15 Nov 12:00). More: moactl attention | project
      NAME | ls | find Q`.
    - `attention_names_the_waiting_session_first`: the first line starts with `- ⚠ Fix the
      flicker [Lantern] is blocked on you`. S9 (the idle ping) isn't in a `⚠` line.
    - `archived_and_elsewhere_sessions_need_no_attention`: archive S3 → no `⚠` line.
    - `an_empty_brain_has_nothing_to_say`: `attention` → `nothing needs you right now`;
      `brief` has `## Today: nothing set yet`, `- nothing`, `## Active this week: none` and
      `- none`.
    - `find_lists_matches_with_state_and_age`: `find lantern` → 4 lines, the first starting
      `3f9a1c52  Wire up the dimmer [Private/Lantern]  working  `.
  - `crates/moa-store/tests/core_and_store.rs`, `verbs_through_the_store_match_a_reload`
    (Review Focus 2):
    1. Start from `TypicalBrain`, with a `State` plus a `StoreHandle` on the same DB.
    2. Run about 15 verbs through `handle_checked`: group add/rename/rm, set, archive, task
       add/set (with due, today, done), note (with and without a project), project add with
       linking.
    3. After each call, `commit` its `Persist` changes and await `Ok`.
    4. Load a fresh snapshot into a new `State`: its `model()` topics equal the live
       state's, ignoring `rev`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement `ops/read.rs`**
- [ ] **Step 4: Run everything**

  Run: `nix develop --command cargo nextest run --workspace`, then
  `nix develop --command cargo clippy --workspace --all-targets -- -D warnings`,
  `nix develop --command cargo fmt --all --check`, and `nix flake check -L`.
  Expected: all PASS.
- [ ] **Step 5: Commit** `git commit -m "v1: brief, attention, find; the in-memory model matches the DB after any verb"`
- [ ] **Step 6: Open PR a4** (Tasks 12–14), with `/code-review medium --fix`. The
  description lists calls S1–S19 in one line each, and says "Restart needed: nothing; the
  spike is untouched".

## Done when

- CI is green, including the `cargo tree` purity step.
- Every data verb except the suggestions answers through `handle` against the imported
  Typical brain, with the spike's text (snapshots committed). The ordering rule holds in
  every core test (`handle_checked`).
- `verbs_through_the_store_match_a_reload` passes: memory and disk agree after any verb.
- A failed commit writes nothing (`a_commit_is_one_transaction`), and a dead store
  thread answers `Stopped`, never silence.
- The alarm fires at absolute deadlines on macOS and Linux CI. Route proptests and the
  store-hardening tests pass.

**Try it:** there's nothing to run by hand yet; the daemon and `moactl` come in 2b. To see
the text, open the committed snapshots in `crates/moa-core/tests/snapshots/`:
`read__brief_matches_the_spike_layout.snap`, `read__attention_matches_the_spike_layout.snap`,
`sessions__ls_matches_the_spike_layout.snap`, `tasks__task_ls_matches_the_spike_layout.snap`
and `projects__project_show_matches_the_spike_layout.snap`. Restart needed: nothing; the
spike is untouched.
