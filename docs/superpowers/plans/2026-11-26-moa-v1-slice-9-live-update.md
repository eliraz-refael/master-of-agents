# moa v1 slice 9: live update

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new build of moa takes over without anyone noticing. When the binary changes, or
on `moa upgrade`, the daemon waits until moa is between turns and re-execs itself with the
listening socket kept open: same pid, no refused connection, no lost hook. Panels on another
build re-exec themselves in their panes. `moa.toml`, `layouts.json`, scope files and the
constitution reload while running. Each upgrade says in one console line what it needs from
you.

**Architecture:**
- **The daemon follows its own install path.** Autostart passes the path you ran (`MOA_EXE`,
  not canonicalised). The daemon watches that path's symlink chain, launches panels and
  hooks from it, and re-execs it when its build changes. A client from another install never
  stops the daemon (L1).
- **One new core module, `moa_core::upgrade`,** owns the wait for "moa is between turns". It
  takes `Input::Upgrade(Why)`, answers every parked ask wait with "no reply yet", then emits
  `Effect::Exec` once. The daemon shell winds down as for a shutdown, except that the
  listener and the brain lock stay open across the exec (`MOA_LISTEN_FDS`, `MOA_LOCK_FD`).
- **The new image adopts the inherited fds** instead of binding and locking. It runs the
  normal restart: load, drain the spool, one `list-panes`, reconcile (spec §6.15.1).
- **Config reload is a second watch,** on the config dir. moa.toml is parsed in the shell and
  handed to the core whole, or refused whole.

```
 exe chain changes ─► 500 ms settle ─► probe `<exe> build-id` ─► Input::Upgrade(Rebuilt)
 moa upgrade / stale client ─► Call::Upgrade ─┘
 core: wait until moa isn't busy ─► answer parked ask waits ─► Effect::Exec
 shell: stop accepting, stop core (flush, unhook), close sessions, stop the adapter
        ─► clear CLOEXEC on listener + lock ─► exec(MOA_EXE, daemon --brain B)
 new image: adopt listener + lock ─► load, drain, list-panes, reconcile ─► notice line
 panel: hello says another build ─► restore the terminal ─► exec(daemon's exe, own args)
```

**Tech Stack:** Rust; the existing crates. No new dependencies: `libc` (already a workspace
dependency) for `fcntl`, kqueue and inotify, inside `moa-platform`. No schema change: the
notice's build goes in `meta`.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md` §6.15 ("Live update and
restart notices", C21), §6.4 (build rule), the hooks' stable path (line ~484), U5, lesson 55.
Roadmap: "Slice 9: live update" (line 874), P10. Builds on slices 0–7 (#33–#101).

## Builds on (names to use)

- **Build id:** `moa_platform::build_id_of(&Path) -> io::Result<String>` (canonical path,
  length, mtime) and `build_id() -> &'static str` (`build_id.rs`).
- **Hello:** `Hello{client, build, caller}` → `HelloOk{brain, build, rev, matched, pid}`
  (`moa-proto/src/wire.rs`). Mismatched callers may only call
  `allowed_on_mismatch` = `Hello|Status|Upgrade|Shutdown` plus the bare hook path.
- **Client:** `ClientOptions{.., autostart, on_mismatch: OnMismatch::{Restart, Accept},
  build}`, `Client::connect` (`moa-client/src/lib.rs:343-370`), `own_exe()`, `start()`,
  `Client::upgrade`, `wait_for_exit`, `EXIT_WAIT`.
- **Daemon:** `RunOptions` (`moa-daemon/src/run.rs:52`), `bring_up` (lock at :223, socket at
  :236), `keep_up` (select loop, wind-down ~:490), `daemon_exe()` (:139), `tmux_link()`
  (:118, writes `moa_tmux`), `write_session_plugin` (:181), `Deps` (`state.rs:~185`).
- **Core:** `Input::{Startup, StartDrained}` (`core.rs`), `Moa::busy()`, `central.restarting`
  (`central/mod.rs:238, 359`), `asks::AsksMem.parked`, `Effect::Reply`, `Tx::ui_each`,
  `feed::row(.., kind::SYSTEM, ..)`, `central::card_thread`, `meta` keys (as `moa.briefed`).
- **Watch precedent:** `moa_platform::ZoneWatch` (`zone.rs`): kqueue/notify on macOS,
  inotify on a directory filtered by entry name on Linux, through `sys`.
- **Panels:** `moa_tui::run::connect` (`run.rs:375`, `Failed::Mismatch`, footer `MISMATCH`),
  `reconnect_delays`, `panel.rs` (`moa panel`), `cockpit::spawn::{spawn_cmds, respawn}`,
  `PanelLaunch{exe, env}`.
- **Config:** `load_config(&Paths)` (`run.rs:68`), `State.config` / `State::config()`,
  `ops::layout::read`, `Topic::Layouts`, `Deps.{layouts_file, scope_file,
  constitution_file}`, `prompt::system_text` (read when a new thread opens).
- **Tests:** daemon `Rig`/`Up` (`moa-daemon/tests/common/rig.rs`), `boot_with`; e2e
  `cockpit_up`, `panel_tab`, `screen_of`, `shows`, `status_json`, `server_pid`, `moactl(tb)`;
  `crates/moa/tests/ctl.rs:252` copies the binary for a second build id.

## Calls for you to check

The most important come first.

1. **The upgrade policy (roadmap 9.5, L1): the daemon follows its own install, and a client
   never kills it.**
   - **Same install, newer build** (you rebuilt, or switched the Nix profile): the client asks
     `upgrade {if_stale: true}`. The daemon sees that its own exe's build changed and
     re-execs into it (same pid).
   - **Another install** (a second checkout, or a copied binary): the daemon's exe is
     current, so it replies `upgrading: false`, and the client stops with
     `moa: brain 'default' is run by <daemon exe>; this moa is <own exe>. Use that one, or
     stop the daemon first: moa shutdown`. Two builds on one root no longer restart each
     other.
   - **A daemon from before slice 9** treats `upgrade` as `shutdown`, as today. The client
     sees a reply without `upgrading`, waits for it to exit, and starts its own, once. That
     is the only upgrade that changes the pid.
2. **The stable path (L2).** Autostart passes the path you ran (`argv[0]` resolved against
   `PATH`, symlinks kept) as `MOA_EXE`. The daemon watches it, re-execs it, and gives it to
   panels, popups, hooks and moa's `moactl`. So a Nix profile switch is followed, and the
   hooks' command string doesn't change on upgrades (spec ~484).
   - The cost: `ctl.rs`'s `a_rebuilt_client_restarts_the_daemon_on_its_build` copies the
     binary elsewhere. That is "another install" now, so the test becomes
     `another_install_is_refused_not_restarted`.
3. **No polling (U5, L3).** The exe and config watches are kqueue (macOS) and inotify
   (Linux) on each directory in the exe's symlink chain and on the config dir. After a change
   there is one 500 ms settle timer.
   - **One probe process per upgrade:** `<exe> build-id` must print the file's build id
     before the daemon winds down, so a half-copied binary never takes over. That is one
     process per new build, user-caused, like U17's exception.
   - The spec names FSEvents. kqueue gives the same events for a few files with no
     framework.
4. **Waiting for moa (L4).** The re-exec waits while moa is starting, thinking, restarting,
   or holding a permission request.
   - **A stale client** waits up to 30 s, then says `moa is finishing a turn; it upgrades
     right after. Try again in a moment.`
   - **Before the exec,** every `moactl ask` or `reply` that is waiting gets A13's
     `no reply yet from Kite; `moactl reply 12` waits again`, so no asker is cut off.
5. **Restart notices (§6.15.5, L5).** One line, only when the build changed since the last
   start (`meta.daemon.build`), with the most demanding text winning:
   - `moa upgraded: re-attach your terminal` (an attach-time tmux option changed, and a
     tmux server was running);
   - `moa upgraded: N running sessions use the old hooks; they get the new ones when
     resumed` (the session plugin changed, and N > 1 Claude sessions were live at the
     reconcile); for one, `moa upgraded: 1 running session uses the old hooks; it gets the
     new ones when resumed`; with none, the next line;
   - `moa upgraded: the daemon restarted itself` (new pid: the old daemon stopped, crashed,
     or predates slice 9);
   - `moa upgraded: nothing to do` (re-exec, same pid).

   It's a `system` line in moa's current thread (`card_thread`), and an activity row
   always, so it survives panels that are still reconnecting.
6. **Hot reload (§6.15.4, L6):**
   - **`moa.toml`:** a parse error, or a file that drops the running brain, is refused
     whole. moa keeps the last good config and says `moa.toml has an error, so moa kept the
     last one: <error>`. A good one replaces it, rewrites the tmux conf, and re-sources it
     through the link.
   - **`layouts.json`:** re-read and published.
   - **Scope and constitution:** they apply from moa's next thread, never mid-thread. The
     line says `moa's instructions changed; they apply from its next thread (/new starts
     one)`.
   - **The theme has no file;** it follows the terminal, so there's nothing to reload.
7. **An exec that fails after the wind-down exits the daemon (L7).** The next client starts
   a fresh one (notice: "restarted itself"). The probe makes this rare.
8. **Accepted connections drop at the exec (L8).** In-flight calls are answered first, as at
   shutdown. Panels reconnect within 250 ms, and new connections wait in the listener's
   backlog. Nothing is refused.
9. **`Patch`'s base rev (roadmap 9.5b) stays carried.** `subscribe.since` doesn't land in
   this slice, and every reconnect takes a full snapshot, so a base rev has nothing to check
   yet.

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged. Each gets:
- per-task reviews;
- one `/code-review medium --fix` over its branch;
- `scripts/verify`;
- green CI.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-9-1` | 1–3 | `v1 slice 9 (1): the daemon re-execs itself` |
| p2 | `v1/slice-9-2` | 4–5 | `v1 slice 9 (2): panels follow, restart notices` |
| p3 | `v1/slice-9-3` | 6–7 | `v1 slice 9 (3): config reload, end to end` |

p1 also carries this plan. The branch `v1/slice-9` holds the plan until p1 starts.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| L1 | **Upgrade policy.** Daemon side, `Call::Upgrade { if_stale: bool }` (`#[serde(default)]`, so an old client's bare `upgrade` means `if_stale: false`): stale = `build_id_of(MOA_EXE) != build_id()`. `!if_stale \|\| stale` → `Input::Upgrade(Why::Asked)`, reply `{"upgrading": true, "waiting_for_moa": <busy>}`. Otherwise reply `{"upgrading": false, "exe": <MOA_EXE>}`. Client side, `OnMismatch::Restart` runs: matched → ok; `upgrading: true` → wait for the old image to hang up, reconnect through `connect_soon` (50 ms steps), all within `UPGRADE_WAIT = 30 s` (`ClientOptions.upgrade_wait`), and tell an upgrade in place from a restart by pid; then `HelloOk.build` must be the client's own build, else the L1 refusal (another install); a wait that runs out while the daemon waits for moa gives the L4 text; `upgrading: false` → the L1 refusal (`code::BUILD_MISMATCH`); no `upgrading` field → today's stop, `wait_for_exit`, start, once. `OnMismatch::Accept` is unchanged. *Amended (Task 2 review):* a daemon from before slice 9 refuses unknown params, so `if_stale: false` is sent as `{}` (which it takes as a stop), and an `INVALID_PARAMS` answer to `upgrade {if_stale: true}` means a daemon from before slice 9, still running: the client treats it like the reply without `upgrading`. | Roadmap 9.5: settle the policy with re-exec. |
| L2 | **The stable path.** `moa_platform::invoked_exe() -> PathBuf`: `argv[0]` containing `/` → made absolute against the cwd, symlinks kept; otherwise the first executable `PATH` entry of that name; else `current_exe()`. Autostart runs `invoked_exe()` with `MOA_EXE` set to it. `daemon_exe()` returns `MOA_EXE` when it is absolute and exists, else today's canonical `current_exe`. `HelloOk` gains `exe: Option<String>` (MOA_EXE). | Spec ~484; roadmap 9.5's Nix point. |
| L3 | **Watching.** `moa_platform::PathWatch::new(entries: &[(dir, name)]) -> io::Result<PathWatch>`, `async changed()` (cancel-safe, coalescing), like `ZoneWatch`. macOS uses kqueue `EVFILT_VNODE` on each dir (`NOTE_WRITE`) and each existing entry (`NOTE_WRITE\|NOTE_DELETE\|NOTE_RENAME\|NOTE_ATTRIB\|NOTE_EXTEND`), re-armed after an event. Linux uses inotify on each dir (`IN_CLOSE_WRITE\|IN_MOVED_TO\|IN_CREATE\|IN_DELETE\|IN_ATTRIB`), filtered by name. `exe_chain(path) -> Vec<(dir, name)>` lists every link in the symlink chain plus the final file, and the watch is rebuilt from it after each change. After a change: a 500 ms settle timer, then the build check, then the probe (`<exe> build-id`, 5 s limit, its output must equal `build_id_of(exe)`). A failed probe logs `upgrade skipped: <exe> isn't a whole moa yet` and waits for the next change. | U5; spec §6.15.2. |
| L4 | **The wait.** `moa_core::upgrade`: `Input::Upgrade(Why::{Rebuilt, Asked})` sets `state.upgrade = Some(Want{why, since})`; a second one changes nothing. `upgrade::run` at the end of every input: wanted, not yet sent, `!central.busy()`, `central.restarting.is_none()` → reply A13's no-reply-yet text to every parked ask/reply wait and drop them, then `Effect::Exec` once. `status` gains `"upgrade": "waiting for moa's turn" \| null`. | §6.15.2 "waits until moa is between turns". |
| L5 | **The exec.** `Effect::Exec` reaches the shell through `CoreHandle::exec_wanted()` (a `watch`), as another `keep_up` select branch. The wind-down is shutdown's: stop accepting without closing the listener, `core.stop()` and `core.ended()` (flush, link `clean_up`), close and wait for sessions, adapter stopped. Then `RunOptions.exec` hands off: `Exec::Real` clears `FD_CLOEXEC` on the listener and lock fds and calls `moa_platform::reexec(exe, ["daemon", "--brain", B], Inherit{listener, lock, from_build})`, which sets `MOA_LISTEN_FDS=<fd>`, `MOA_LOCK_FD=<fd>`, `MOA_REEXEC_FROM=<old build>` and `exec`s. It returns only an error, which is logged, and the daemon exits 1. `Exec::Test(tx)` sends the owned listener and lock to the test instead. | §6.15.2; daemon tests can't exec their own process. |
| L6 | **Adopting.** `moa daemon` reads and removes the three variables before the tokio runtime starts (`moa_platform::take_inherited() -> Option<Inherited>`, with the unsafe `from_raw_fd` and `remove_var` under `// SAFETY:` and single-threaded). Then `RunOptions.inherited: Option<Inherited>`. `bring_up` with one re-checks `try_lock` on the adopted lock fd (same open file, so it succeeds), sets CLOEXEC again, takes the listener (`set_nonblocking`, `UnixListener::from_std`), and never removes or binds the socket path. | §6.15.2 "same pid; no connection is refused". |
| L7 | **Notices.** The shell computes `UpgradeFacts{ reexec: bool, plugin_changed: bool, reattach: bool }` at bring-up: `write_session_plugin` returns whether it changed anything, and `conf::needs_reattach(old, new)` checks `moa_tmux` lines setting `default-terminal`, `terminal-overrides` or `terminal-features`. It passes them in `CoreOptions`. At `StartDrained`'s reconcile answer, if `meta.daemon.build != build_id()`, the core writes the L5 notice once, with N = live Claude sessions (the singular form for one; none falls through to the next text; re-attach is skipped when the link found no tmux server), and saves `meta.daemon.build` in the same commit. A refused commit retries at the next input. | §6.15.5. |
| L8 | **Config reload.** The shell watches the config dir for `moa.toml`, `layouts.json`, `<brain>.md` and `constitution.md`. moa.toml: `load_config` in the shell, then `Input::Config(Result<Config, Unread>)` (`Unread::{Error(String), Gone}`); the core refuses an `Error`, or a config without its brain, with the L6 text (`CoreEvent::Error`), and otherwise replaces `State.config` and emits `Effect::TmuxConf` when `[ui]` changed (the shell rewrites `moa_tmux` and sends `TmuxCmd::Source`, which re-runs `set_up`'s `source-file`). A changed `moa_account`, or that account's `launcher`/`acp`, reaches moa's running agent at its next start; the reload doesn't restart it (`/account` does that). A removed moa.toml (`Unread::Gone`) keeps the last config, with its own line. layouts.json: `Input::LayoutsChanged` → `ops::layout::read`, publish `Topic::Layouts` (a bad file keeps the last, with an error line). Scope or constitution: `Input::InstructionsChanged` → the L6 line through `Tx::ui_each`. | §6.15.4. |

## Global Constraints

The roadmap's constraints apply, as do every earlier slice plan's. Also:

- **`unsafe` only in `moa-platform`,** each block with a `// SAFETY:` comment (roadmap
  conventions). `fcntl`, kqueue, inotify, `from_raw_fd` and `remove_var` live there. The
  `exec` itself is `std::os::unix::process::CommandExt::exec`.
- **No new dependencies.** `libc` only, in `moa-platform`. `moa-core`, `moa-proto` and
  `moa-view` stay free of IO crates (`scripts/io-crates`).
- **No polling (U5).** Watches are kernel events. Timers are armed only after an event: the
  500 ms settle, the probe's 5 s limit, and a client's 30 s upgrade wait.
- **Old and new speak to each other.** Every new wire field is `#[serde(default)]` or an
  `Option`: `Upgrade.if_stale`, `HelloOk.exe`, and the upgrade reply's fields. The schema
  snapshot is updated in the same commit.
- **Reuse before writing.** One watch type (`PathWatch`, which `ZoneWatch`'s Linux path may
  share); one wind-down (shutdown's, with an exec branch); one no-reply-yet text (A13's);
  one notice writer. A near-copy is an **Important** finding.
- **The core stays pure:** time is `Now`; the core never reads files or the exe. The shell
  passes facts in.
- **Building and verifying:**
  - cargo only through `nix develop --command cargo …`, with `cargo nextest run`;
  - `scripts/verify quick` before every commit, and the full `scripts/verify` once per PR;
  - never run cargo while an e2e test runs;
  - test counts are quoted from `target/verify/nextest.log`, never from memory.
- **Tests:**
  - only under `/tmp` roots;
  - never the real `claude`, the real adapter, a tmux server named `moa`/`moa-work`, a real
    brain, `launchctl` or `systemctl`;
  - every wait goes through `wait_for` with a 10 s limit;
  - daemon tests use `Exec::Test` (they can never exec the test process);
  - a binary is replaced by writing a copy beside it and renaming it over, never by writing
    into a running binary (macOS kills a process whose signed binary changes in place).
- **Public repo:** no machine paths in fixtures; use `/tmp` roots and `~/private`.
- **Plain words** in everything the user sees: "upgrade", "restart", "build". Never
  "exec", "fd", "CLOEXEC" or "kqueue".

## Review Focus

1. **A binary copied in place, half-written:**
   - **The input:** `cp` writes the stable path in two steps; the watch fires mid-copy.
   - **Expected:** the probe fails, nothing winds down, and the next change (the copy's end)
     upgrades.
   - **Pinned by:** `a_half_written_binary_is_skipped` (Task 2).
2. **Two rebuilds in a row, or a rebuild while moa thinks:**
   - **Expected:** one exec, into the latest build, after the turn.
   - **Pinned by:** `rebuilds_while_moa_thinks_exec_once` (Task 2).
3. **Two installs on one root:**
   - **The input:** calls alternate from two exe paths.
   - **Expected:** the daemon's pid never changes, and the other install's calls are
     refused with L1's text.
   - **Pinned by:** `two_installs_never_restart_each_other` (Task 3).
4. **A hook or `moactl` call during the exec window:**
   - **Expected:** it is answered by the new image. Nothing is refused, and no event is lost.
   - **Pinned by:** `a_hook_during_the_handoff_is_kept` (Task 2) and the e2e (Task 7).
5. **moa.toml saved half-edited:**
   - **Expected:** one error line, the last good config kept, and the next good save
     applies.
   - **Pinned by:** `a_broken_moa_toml_keeps_the_last` (Task 6).

---

## Task 1: the platform pieces

**Files:**
- **Modify:** `crates/moa-platform/src/{lib.rs, sys.rs, build_id.rs, zone.rs}`.
- **Create:** `crates/moa-platform/src/{exe.rs, watch.rs, inherit.rs}`.
- **Tests:** unit tests in each, and `crates/moa-platform/tests/watch.rs`.

**Interfaces:**
- Produces:
  - `invoked_exe() -> PathBuf` and `exe_chain(&Path) -> Vec<(PathBuf, OsString)>` (`exe.rs`);
  - `PathWatch::new(&[(PathBuf, OsString)]) -> io::Result<PathWatch>` and `async fn
    changed(&mut self) -> io::Result<()>` (`watch.rs`);
  - `Inherited{ listener: std::os::unix::net::UnixListener, lock: File, from_build: String }`,
    `take_inherited() -> io::Result<Option<Inherited>>`, `keep_open(fd: BorrowedFd) ->
    io::Result<()>` (clears CLOEXEC), and `reexec(exe: &Path, args: &[&str], listener:
    BorrowedFd, lock: BorrowedFd, from_build: &str) -> io::Error` (`inherit.rs`).

- [ ] **Step 1: Failing tests:**
  - **`invoked_exe` (unit, `argv[0]` and `PATH` passed in):** `./bin/moa` against cwd `/tmp/x`
    gives `/tmp/x/bin/moa`; `moa` with a `PATH` holding a symlink `moa` gives the symlink's
    path, not its target.
  - **`exe_chain`:** `a → b → file` gives three entries, and a plain file gives one.
  - **`PathWatch`** (`watch.rs`, temp dirs):
    - `a_rename_over_fires`;
    - `a_symlink_retarget_fires` (a link two levels up the chain);
    - `an_in_place_write_fires`;
    - `an_unrelated_entry_doesnt_fire` (waits 300 ms, then asserts pending);
    - `changes_coalesce`.
  - **`inherit`:**
    - `take_inherited` is `None` with no variables;
    - a round trip in a child process (fork via `Command` running the test binary with a
      filter) gets the same listener: a connection made before the handoff is accepted after
      it, and the lock is still held (a second `try_lock` from another file fails).
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** L2's `invoked_exe` and L3's watch. Move `ZoneWatch`'s Linux
  inotify into `PathWatch` if it reads cleanly, and keep `ZoneWatch`'s API. Then L5/L6's
  `inherit`.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass, on both platforms
  through `clippy-linux`.
- [ ] **Step 5: Commit.** `v1: watch paths, find our exe, hand fds across an exec`.

## Task 2: the daemon re-execs itself

**Files:**
- **Modify:**
  - `crates/moa/src/{daemon.rs, main.rs, ctl.rs}` (`moa upgrade`, hidden `moa build-id`);
  - `crates/moa-daemon/src/{run.rs, session.rs, core_task/mod.rs}`;
  - `crates/moa-core/src/{core.rs, effect.rs, state.rs, op.rs, ops/status*}`;
  - `crates/moa-proto/src/{wire.rs, cli.rs}` and the schema snapshot.
- **Create:** `crates/moa-core/src/upgrade.rs`, `crates/moa-daemon/tests/upgrade.rs`.

**Interfaces:**
- Consumes: Task 1's watch and inherit.
- Produces:
  - `Input::Upgrade(Why)`, `Why::{Rebuilt, Asked}`, `Effect::Exec`;
  - `RunOptions.{exec: Exec, inherited: Option<Inherited>, exe: PathBuf}`;
  - `Exec::{Real, Test(mpsc::Sender<Inherited>)}`;
  - `Call::Upgrade{if_stale}`, and the reply `UpgradeOk{upgrading: bool, waiting_for_moa:
    bool, exe: Option<String>}`;
  - `HelloOk.exe`.

- [ ] **Step 1: Failing tests** (`tests/upgrade.rs`, the `Rig` with `Exec::Test`; "restart"
  means `Rig::start` with the received `Inherited`):
  - `moa_upgrade_hands_over_the_socket`: a client connected before the handoff gets EOF; a
    client that connects during it (before the new start) is answered after it; same root,
    no `remove_file` (the socket inode is unchanged).
  - `the_upgrade_waits_for_moas_turn`: moa thinking (stub `Hang`) means no handoff, and
    `status.upgrade` is `"waiting for moa's turn"`. The turn ends, then the handoff.
  - `parked_asks_hear_no_reply_yet_first`: a parked `ask` and `reply` both get A13's
    no-reply-yet text before the handoff.
  - `a_rebuild_upgrades`: the rig's `exe` is a temp copy; renaming a newer copy over it
    leads to a handoff within the settle time.
  - `a_half_written_binary_is_skipped` (Review Focus 1): an empty file renamed over gives no
    handoff and the log line; a whole copy after it gives one.
  - `rebuilds_while_moa_thinks_exec_once` (Review Focus 2).
  - `a_hook_during_the_handoff_is_kept` (Review Focus 4): a hook sent between the old
    image's last accept and the new start is recorded once by the new one.
  - `upgrade_if_stale_on_a_current_daemon_says_no`: `{upgrading: false, exe}` and no
    handoff.
  - `moas_thread_resumes_after_the_handoff`: before it, moa has today's thread. After it,
    the stub adapter was stopped, and the next prompt resumes the same thread
    (`session/resume` with its id; spec §6.15.2's "the thread resumes").
  - `server.rs`'s `a_mismatched_build_may_only_hello_status_upgrade_shutdown_and_hook`
    still passes with the new `Upgrade` params; `upgrade_stops_cleanly` becomes
    `upgrade_hands_over_cleanly`.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement:**
  - L2 on the daemon side (`MOA_EXE` into `RunOptions.exe`; `daemon_exe()`; `HelloOk.exe`);
  - L4 (`moa_core::upgrade`);
  - L5 and L6;
  - the exe watch (L3) in `keep_up`, with the settle timer and the probe;
  - `moa upgrade`, which asks `upgrade {if_stale: false}`, waits for the same pid with the
    new build, and prints `moa upgraded (<old> → <new>)`, or L4's text if moa is still
    thinking after 30 s;
  - hidden `moa build-id`, which prints `build_id()`.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: the daemon re-execs itself between moa's turns`.

## Task 3: clients follow the policy

**Files:**
- **Modify:** `crates/moa-client/src/lib.rs` (`connect`, `own_exe` → `invoked_exe`,
  `start` sets `MOA_EXE`), `crates/moa/src/{front.rs, ctl.rs}`.
- **Tests:** `crates/moa/tests/{client.rs, ctl.rs}`.

**Interfaces:**
- Consumes: Task 2's `Call::Upgrade{if_stale}`, `UpgradeOk` and `HelloOk.exe`.

- [ ] **Step 1: Failing tests:**
  - `client.rs`:
    - `a_stale_daemon_upgrades_in_place` (same pid, new build);
    - `another_install_is_refused_not_restarted` (L1's text; the pid unchanged);
    - `an_old_daemon_is_restarted_once` (a reply without `upgrading`: today's path);
    - `a_busy_moa_makes_the_client_wait_then_say_so` (L4's text after `UPGRADE_WAIT`, with
      the wait shortened for the test through `ClientOptions`).
  - `ctl.rs`:
    - `a_rebuilt_client_restarts_the_daemon_on_its_build` becomes
      `another_install_is_refused_not_restarted`;
    - `two_installs_never_restart_each_other` (Review Focus 3): ten alternating calls, one
      pid.
  - Autostart starts the daemon with `MOA_EXE` equal to the path the test ran, symlink
    kept: `autostart_passes_the_path_you_ran`.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** L1's client side and L2's autostart.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: a client never stops another install's daemon`.

## Task 4: panels follow the build

**Files:**
- **Modify:** `crates/moa-tui/src/run.rs` (`connect`, the `Link::Mismatch` path),
  `crates/moa/src/panel.rs`.
- **Tests:** `crates/moa-tui` unit tests, and `crates/moa/tests/cockpit_e2e.rs`.

**Interfaces:**
- Consumes: `HelloOk.{build, exe}`.
- Produces: `moa_tui::run::Follow` (the decision), `MOA_PANEL_FOLLOWED`.

- [ ] **Step 1: Failing tests:**
  - **Unit, `follow(own_build, hello, followed_env) -> Follow::{Stay, Exec(exe), Mismatch}`:**
    - matched → `Stay`;
    - a different build with `exe` → `Exec`;
    - no `exe` (a daemon from before slice 9) → `Mismatch`;
    - `followed_env == hello.build` and still different → `Mismatch` (no loop).
  - **e2e, `a_panel_follows_the_daemon_in_its_pane`:**
    - the cockpit runs from a temp copy;
    - the daemon is upgraded (Task 2);
    - the projects panel's pane id and pid are unchanged, and its connection hello now
      matches (its `panel.log` says `followed <old> → <new>`);
    - no `MISMATCH` footer.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement:** on `Exec(exe)`, restore the terminal (leave the alternate screen
  and raw mode, as at exit), then `exec(exe, own argv[1..])` with `MOA_PANEL_FOLLOWED=<daemon
  build>`. An exec error falls back to the `MISMATCH` footer. A `--once` popup follows the
  same way.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: panels follow the daemon's build`.

## Task 5: restart notices

**Files:**
- **Modify:**
  - `crates/moa-daemon/src/run.rs` (`UpgradeFacts`, `write_session_plugin` returns
    changed);
  - `crates/moa-tmux/src/conf.rs` (`needs_reattach`);
  - `crates/moa-core/src/{core.rs, upgrade.rs, reconcile.rs}`.
- **Tests:** `crates/moa-core/tests/upgrade.rs`, `crates/moa-tmux` unit tests,
  `crates/moa-daemon/tests/upgrade.rs`.

**Interfaces:**
- Produces: `UpgradeFacts`, `CoreOptions.facts`, `upgrade::NOTICE_*` texts, and
  `meta.daemon.build`.

- [ ] **Step 1: Failing tests:**
  - Core, one per text and the precedence:
    - `a_reexec_says_nothing_to_do`;
    - `a_new_pid_says_restarted_itself`;
    - `a_changed_plugin_counts_running_sessions` (3 live sessions, exact text);
    - `an_attach_option_says_reattach` (it beats the others);
    - `the_same_build_says_nothing_at_all` (no line, no meta write);
    - `no_thread_still_leaves_the_activity_row`;
    - `a_refused_commit_says_it_next_time`.
  - `conf`: `needs_reattach` is true for a changed `terminal-features` line and false for a
    changed key binding.
  - Daemon: after an `Exec::Test` handoff with a new `build_id` stand-in, the feed has
    `moa upgraded: nothing to do` once.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** L7.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: every upgrade says what it needs`.

## Task 6: config hot reload

**Files:**
- **Modify:**
  - `crates/moa-daemon/src/{run.rs, tmux.rs}` (`TmuxCmd::Source`);
  - `crates/moa-core/src/{core.rs, state.rs, effect.rs, ops/layout.rs, upgrade.rs or a new
    config_reload.rs}`.
- **Tests:** `crates/moa-core/tests/config_reload.rs`, `crates/moa-daemon/tests/config.rs`.

**Interfaces:**
- Produces:
  - `Input::{Config(Result<Config, Unread>), LayoutsChanged, InstructionsChanged}`;
  - `Effect::TmuxConf`;
  - `TmuxCmd::Source`.

- [ ] **Step 1: Failing tests:**
  - Core:
    - `a_good_moa_toml_replaces_the_config` (an account added is usable by the next `open`);
    - `a_broken_moa_toml_keeps_the_last` (Review Focus 5: the exact text, the config
      unchanged, then a good one applies);
    - `a_moa_toml_without_this_brain_is_refused`;
    - `layouts_reload_and_publish`;
    - `instructions_apply_from_the_next_thread` (the open thread's system text is
      unchanged; `/new`'s has the new constitution).
  - **Audit first:** list every value copied out of `Config` at start (in the report), and
    test that each follows a reload or is named in an amendment as start-only.
  - Daemon:
    - `editing_moa_toml_reloads` (`[ui]` changed → `moa_tmux` rewritten, and `TmuxCmd::Source`
      sent once);
    - `saving_twice_reloads_once` (the settle).
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** L8.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: moa.toml, layouts and instructions reload while running`.

## Task 7: end to end

**Files:** `crates/moa/tests/upgrade_e2e.rs`, plus `crates/moa/tests/common` helpers
(`install_copy(tb) -> PathBuf`, `rebuild(path)`: write a copy beside it, then rename).

- [ ] **Step 1: Failing tests** (the real binary in a tmux sandbox, the real clock):
  - **`a_rebuild_updates_everything_in_place`** (the roadmap's "done when"):
    - the cockpit is started through a temp install; a fake-claude session fires a hook
      every 100 ms; a loop runs `<install>/moa status` against the brain (*amended:* `moactl` has no `status` verb);
    - `rebuild` the install;
    - **expected:**
      - `status` shows the same pid and the new build;
      - every `moa status` in the loop succeeded;
      - every hook the session fired is in its events, none missing;
      - each panel's build matches, in the same pane and pid;
      - the console shows `moa upgraded: nothing to do`.
  - **`moa_upgrade_says_so`:** `moa upgrade` prints `moa upgraded (<old> → <new>)`.
  - **`a_running_session_hears_about_new_hooks`:** after a rebuild that changes the plugin
    the console shows the `1 running session uses …` line. *Amended (as built):* the environment survives the re-exec, so no env-gated hook could differ between builds; the test writes an older plugin text to disk before the session starts, and the new build's start rewrites it through the real path.
- [ ] **Step 2: Run them.** They fail where the wiring is missing.
- [ ] **Step 3: Fix what fails.** Each test waits on something visible. Make each fail once
  on purpose.
- [ ] **Step 4: Run them again,** 20 times under load. They pass every time.
- [ ] **Step 5: Commit.** `v1: a rebuild updates the cockpit in place`.

## Try it (p3's body)

In a sandbox (`MOA_ROOT=/tmp/…`):
1. Start the cockpit with `target/debug/moa`, open a session, and run `cargo build` after any
   change. Within a second or two the console says `moa upgraded: nothing to do`. `moa
   status` shows the same pid, and the panels redraw on their own.
2. Break `moa.toml` (an unclosed quote) and save. The console says it kept the last one. Fix
   it and save again.

**Restart needed:** nothing for the spike, which stays as it is until slice 10's switch.
A v1 sandbox daemon that predates slice 9 can't re-exec, so the first `moa` in that sandbox
after this lands restarts it by itself ("the daemon restarted itself"). After that, sandbox
upgrades happen in place.
