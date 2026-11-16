# moa v1 slice 3: the tmux layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `moa --sandbox` opens a throwaway brain in the user's terminal on its own tmux
server. moa's key table, menu and two-line status line work there. Every moa key and menu
item reaches the daemon with no process launched, the status line shows the brain's live
counts, and a killed or restarted tmux server is noticed.

**Architecture:**
- `moa-core::tmux` (pure) holds tmux's vocabulary:
  - ids, `Notification`, the `moa:` mailbox, and `Cmd` with its quoting and allow-list;
  - the list formats and their parsers, and the focus `Topology`;
  - the link state machine, `seen` marking, and the status line.
  The core decides every tmux command it sends.
- `moa-tmux` (new) holds:
  - the byte parser (`Parser`: bytes → `Frame`s);
  - `ControlClient`, one `tmux -C` child whose replies are matched FIFO and whose replies
    and notifications come back as one ordered `Event` stream;
  - the generated config.
- `moa-daemon` gains a tmux actor:
  - it probes the server's socket, connects the control client and bootstraps it (sources
    moa's config, sets `@moa_ctl`, installs the hooks);
  - it forwards events to the core and sends the core's `Effect::Tmux` batches after each
  commit;
  - it reconnects with a short backoff.
- The `moa` binary gains the front door, `moa [BRAIN]` and `moa --sandbox`. It creates the
  tmux server itself, from the user's terminal and with the user's environment (T1), then
  asks the daemon to connect (`tmux.ensure`) and execs `tmux attach`.

```
 user's terminal ── moa --sandbox ──(1) hello + tmux.ensure ──► daemon ── no server ──► {running:false}
        │            (2) tmux -S <root>/…/tmux-default -f tmux.conf new-session _moa ; new-session moa   (user's env)
        │            (3) tmux.ensure ──► daemon: probe ► Spawner(tmux -N … -C new-session -A -s _moa …) ► source moa.tmux,
        │                                 @moa_ctl, hooks ► Connected ► core: list-* ► Topology ► {running:true}
        └── exec tmux attach -t =moa
 C-a c ─► tmux key table ─► run -C "display-message -c '#{@moa_ctl}' 'moa:key console %3 $0 /dev/ttys1'"
        ─► %message ─► Parser ─► Event ─► core: Mailbox::Key ─► Effect::Tmux[display-message -c … -d 4000 -l "moa: …"]
 core commit ─► status::sync ─► set-option -g @moa_status …   (status line redraws on its own)
```

**Tech Stack:** `tokio` (`process`, `io-util`), `rustix` (testkit only: `pty`, `termios`,
`fs`), `insta`, `proptest`. There are no new crates outside the workspace's lock except
rustix's features.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.1 (allowed launches), §6.3 (`Input`/`Effect`), §6.6 (the tmux layer, C7, C18);
- §6.8 (timers), §6.13 (keys and menu), §6.14 (colours), §6.15.4 (re-applied on every
  connect), §7 row 1 (no nesting).

Roadmap: the Slice 3 section, its "carried from slice 2b" notes, and Review Focus 5.
Research: R2 (`docs/superpowers/research/2-tmux-control-mode.md`), and R1 §2.1, §2.5 and
§2.6. Parity source: `spike/moa-proto/tmux.conf`, `menu.tmux`, `moamenu`, `moa`, and
`picture.py:counts_text`.

**Builds on** slices 2a and 2b:
- the core: `handle`, `State`, `Input`, `Effect`, `check_order`, `TimerKey`, `Op`, `Done`,
  `StatusData`, `live::display_state`;
- the daemon: `CoreHandle` and the scheduler (`SetTimer` on a `WallAlarm`);
- the client and platform: `moa-client` (autostart, `daemon_env`), `Paths` (incl.
  `socket`'s fallback and `check_v1_allowed`), `Spawner`;
- the testkit: `TempBrain` and `Sandbox`.

Read 2b's calls D1–D20. D17 (the daemon's env) and D20 (the `MOA_ROOT` gate) shape this
plan most.

## PRs (roadmap P4)

Four PRs. Each branches from `main` after the previous one merged. Each gets
`/code-review medium --fix`, green CI, and "Restart needed: nothing; the spike is
untouched".

| PR | Branch | Tasks | Title |
|---|---|---|---|
| c1 | `v1/slice-3-1` | 1–4 | `v1 slice 3 (1/4): tmux vocabulary, parser, control client` |
| c2 | `v1/slice-3-2` | 5–8 | `v1 slice 3 (2/4): the tmux link in the core` |
| c3 | `v1/slice-3-3` | 9–10 | `v1 slice 3 (3/4): generated config, the daemon's terminal link` |
| c4 | `v1/slice-3-4` | 11–12 | `v1 slice 3 (4/4): front door, end to end` |

This plan file is committed in PR c1.

## Calls this plan makes

Facts marked **[3.4+3.8]** were checked against throwaway tmux 3.4 and 3.8 servers while
writing this plan (see the appendix).

| # | Call | Why |
|---|---|---|
| T1 | **The front door creates the tmux server. The daemon never does**: every tmux process the daemon runs has `-N` ("don't start a server") **[3.4+3.8]**. The front door runs `tmux <sock> -f <tmux.conf> new-session -d -s _moa -x 10 -y 3 /bin/sh ; new-session -d -s moa -n moa -c <home>`. Its environment is the user's, minus `MOA_*`, `CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_CONFIG_DIR`, `TMUX` and `TMUX_PANE`, plus `MOA_ROOT` when sandboxed. `TERM` and `SSH_AUTH_SOCK` pass through. `MOA_ROOT` is the one moa variable in a server's environment, and only in a sandbox: spec §6.6 keeps the server's environment free of moa, but a sandbox shell's `moactl` must reach the sandbox and not the real home. Slice 4's per-pane `-e` sets it for agents. | This answers the roadmap's carried question about the daemon's PATH and environment. Whoever starts a tmux server gives every pane its environment. The daemon may have been started by an agent's `moactl` (D17) or by launchd with a minimal PATH. The user's terminal is the right parent, as in the spike (R1 §2.1). Spec §6.6 says "the daemon starts the server"; this changes who, not what (one exec, with the generated config). It also fits C28: nothing exists on the tmux side until someone opens moa. |
| T2 | **Sandboxed brains use `-S <run>/tmux-<b>`** (falling back like `Paths::socket`). Real brains keep `-L moa` / `-L moa-<b>`. | P13: `MOA_ROOT` moves every moa path under the root. Tests and the try-it can then never reach the user's live `-L moa`/`-L moa-work` servers, and parallel tests never collide. The roadmap's try-it "socket `moa-sandbox`" becomes a path inside `~/.local/state/moa-sandbox`. |
| T3 | **The control client is `tmux -N <sock> -C new-session -A -s _moa -f no-output,ignore-size,no-detach-on-destroy -x 10 -y 3 /bin/sh`**, launched through `Spawner`. Its environment is cleared, then given `HOME`, `USER`, `LOGNAME`, `PATH`, `LANG`, `LC_*` and `TMUX_TMPDIR` from the daemon's. | `-A` creates `_moa` when it's missing (on a server that never had one) and attaches when it's there; the flags apply either way **[3.4+3.8]**. `/bin/sh` exists everywhere, NixOS included. |
| T4 | **The daemon finds a server by connecting to its socket in-process**, at start and on `tmux.ensure`. "Not found" or "refused" means there is none, and it then waits for the next `tmux.ensure`. It never retries on its own. When its control client drops but the server is still there, it reconnects after 0.1, 0.2, 0.4, 0.8, 1.6 and 3.2 s, then gives up until the next `tmux.ensure`. The backoff lives in the actor (tokio sleep), not the scheduler. While connected or connecting, the actor answers a `Connect` by reporting its state again, never with a second control client. **Every connection has a number** (`conn`). `Connected`, `Effect::Tmux` and `TmuxIn::Replies` carry it, and both sides drop anything from another connection. | No polling, and launches happen only per connect (spec §6.1). The backoff is bounded and monotonic. |
| T5 | **`tmux.ensure {tmux?}`** is the front door's handshake (an `Op`, not an agent verb). It answers `{running: bool}` once the daemon is connected and a `moa` session exists, or `running: false` when there's no server. `tmux` is the absolute tmux path the front door found. The core accepts only an absolute path whose file name is `tmux` (else `that isn't a tmux program`). The daemon keeps it for its control client and forgets it after a failed spawn (`Down(NoTmux)`). The front door waits at most 10 s for each answer. It's the first reply the core holds back (`TmuxState`'s waiters). | A launchd daemon has no tmux on its PATH. The front door must not attach before the daemon is listening to the server. |
| T6 | **The generated config is two files** in the brain dir:<br>• `tmux.conf`, the `-f` file: `source-file -q ~/.tmux.conf`, then `source-file <moa.tmux>`;<br>• `moa.tmux`: moa's options, the status line, the key table and the menu, with no hooks and no waiting commands.<br>The daemon writes both at start when their text changed, and sources `moa.tmux` over the control client on every connect. | Spec §6.15.4: bindings and options are re-applied on every connect. The user's own file may hold `run-shell` (a waiting command, R2 §5.4), so it's never sourced over the control client. The user's settings load first and moa's override them, as in the spike. |
| T7 | **How the server reaches the daemon.**<br>• **Hooks** name the control client literally (`display-message -c client-123 'moa:hook pane-exited #{hook_pane}'`) and are reinstalled at index 90 on every connect.<br>• **Keys and menu items** are static and go through `run -C "display-message -c '#{@moa_ctl}' '…'"`. A bare `-c '#{@moa_ctl}'` isn't expanded and silently sends nothing **[3.4+3.8]**.<br>• **When the daemon goes:** a `client-detached[90]` hook unsets `@moa_ctl` when the control client goes, even after `kill -9` **[3.4+3.8]**, so keys say "moa isn't running: run moa".<br>• Installed now: `pane-exited`, `pane-died`, `window-layout-changed` (zoom fires it **[3.4+3.8]**), `client-attached`, `client-detached`. `alert-bell` and `client-active` wait for a consumer. | Literal names are the only form a hook expands. The `if -F '#{@moa_ctl}'` guard covers a clean stop, and the hook covers a crash. A hook with no consumer is dead code. |
| T8 | **Mailbox grammar.** Hooks name the control client literally (T7); keys and menu items name it through `@moa_ctl`.<br>• `moa:key ACTION PANE SESSION CLIENT`:<br>&nbsp;&nbsp;– ACTION is a word (`console`, `layout-2`…);<br>&nbsp;&nbsp;– PANE is `%N`, or `-` from a menu (menus have no pane, **[3.4+3.8]**; the core takes the client's active pane);<br>&nbsp;&nbsp;– CLIENT is the rest of the line.<br>• `moa:hook NAME ARG`.<br>• `moa:prompt TOKEN TEXT`.<br>Prompt templates are `display-message -c CTL -l "moa:prompt T %%%"`, double-quoted, because `%%%` escapes for double quotes. Single quotes lose `'` **[3.4+3.8]**. | One vocabulary for keys and menu items, so slices 4 and 5 replace a notice with an action in one place. |
| T9 | **The focus model.**<br>• **Seeded and refreshed** with three list commands (`list-sessions`, `list-panes -a`, `list-clients`), each line starting with a letter.<br>• **Updated in place** by focus notifications (`%window-pane-changed`, `%session-window-changed`, `%client-session-changed`, `%client-detached`).<br>• **Structural changes trigger one refresh**: window add or close, `%sessions-changed`, and the layout, pane-exited, pane-died and client-attached hooks. Refreshes are coalesced: one in flight, at most one more after it. | Spec §6.6 keeps focus by notifications. A re-list costs 0.15–1.8 ms (R2 §5.3) and replaces hand-maintaining tmux's tree for the rare structural changes. |
| T10 | **What waits for the commit and what doesn't.**<br>• **A view of saved state** (an untokened `Effect::Tmux`: status writes, notices) waits for the drain's commit, as replies do. `check_order` treats it as heard, and `fail_held` drops it when the commit fails.<br>• **Link plumbing reveals nothing about saved state**, so it runs at once (`runs_at_once`): `TmuxConnect`, and a tokened `Effect::Tmux` (the refresh lists, the `moa` session create).<br>• **`Reload`**, after a failed commit or a panic, resets the link's bookkeeping: the refresh in flight, the cockpit attempt, and what was written. It then refreshes when `Up`, and asks to connect again when `Connecting`. | The status line is a view and must never show state the DB doesn't have. But plumbing held behind a failed commit would be dropped, leaving the link waiting for a reply that never comes (the waiters hang). A panic mid-`handle` can leave the bookkeeping half-changed. |
| T11 | **Only pane bindings of the current generation, whose pane exists, are trusted** for `@moa_state` and `seen`. Slice 4 creates and reconciles bindings; this slice's tests seed them. | Review Focus 5: no stale binding is ever trusted. |
| T12 | **Status:**<br>• `@moa_status` is the spike's `counts_text` plus `failed` (`✗`, red) between `question` and `done`.<br>• `@moa_state` is set per window (its most urgent trusted pane) and unset when that pane goes.<br>• Both are written as one batch, only what changed.<br>• `@moa_clock` is the local `HH:MM`. It's written at connect and at every minute boundary (`TimerKey::Clock`, a `WallAlarm` deadline), only while a non-control client is attached.<br>• The date stays `%a %d %b` in the format. Setting an option redraws the status line **[3.4+3.8]**, so the date is fresh at every write. | Spec C18 and the idle rule of §6.8: no wake-ups unless someone is looking. Spec §7 says the status view adds failed. |
| T13 | **Keys in this slice.**<br>• Every daemon key and menu item answers with a 4 s notice, `moa: <thing> isn't in this build yet`, in the spike's `moamenu` style.<br>• The pure-tmux keys work fully: arrows and `h j k l`, `z`, `[ ]`, `a`, `\|`, `-`, `m`, and prefix `Space`/`m`. | The done-when is "every key reaches the daemon". Slices 4–5 swap each notice for its action. |
| T14 | **No transaction mode (`a ; b` plus a sentinel) yet**: one command per line. | Slice 5's layouts are its first consumer, and it's built there. |
| T15 | **The parser is fuzzed with proptest, not cargo-fuzz**: arbitrary bytes, the same stream split at every boundary, and adversarial well-formed streams (fake `%end` lines, notifications inside blocks). | cargo-fuzz needs nightly and libFuzzer, against P12's single toolchain pin and the Nix build. Proptest runs in every CI job. |
| T16 | **`Cmd` is the only way to send a tmux command, and it carries an allow-list.**<br>• **Allowed names** (canonical only; slices extend the list): `display-message`, `set-option`, `set-hook`, `show-options`, `list-sessions`, `list-panes`, `list-clients`, `new-session`, `source-file`, `command-prompt`.<br>• **Extra rules:** `command-prompt` needs `-b`, and `source-file` only takes a path ending in `moa.tmux`.<br>• **Refused** commands are never written.<br>• **Quoting:**<br>&nbsp;&nbsp;– an argument of only `[A-Za-z0-9_@%:=.,/+-]` goes bare;<br>&nbsp;&nbsp;– otherwise single-quoted pieces, with `'` written `\'` and a newline or CR as a `"\n"`/`"\r"` piece.<br>&nbsp;&nbsp;– Double quotes expand a leading `~` **[3.4+3.8]**. | Roadmap item 2's "never sends waiting commands" is enforced at the one choke point, not by grepping a log, because a command log would carry titles (S15). The quoting round-trips every tested string on both versions, except the 3.4 quirk below. |
| T17 | **tmux 3.4's `$` quirk is documented, not fought**: on 3.4, a `$` followed by an ASCII letter, `_`, `{` or any non-ASCII character is stored and printed as `\$`, whatever the quoting **[3.4]** (the non-ASCII case was found by Task 4's proptest). The prompt parser turns `\$` back into `$`. **`%message` text is otherwise taken verbatim**, with no `\ooo` unescaping: 3.4 escapes control characters, while 3.8 turns them into `_` **[3.4+3.8]**, so unescaping on 3.8 would turn a typed `\101` into `A`. moa's own messages never hold control characters. | No encoding avoids the quirk. moa writes no `$` text in this slice (notices are fixed, counts and the clock have none). It's cosmetic in titles later. |
| T18 | **The front door:**<br>• `moa [BRAIN]` and `moa --sandbox` (root `~/.local/state/moa-sandbox`).<br>• Inside this brain's own server it switches the client (`switch-client -t =moa`).<br>• Inside any other tmux it refuses with the spike's line: `the <b> moa is running; open it from a new terminal tab: moa <b>` (exit 0).<br>• Brain names that are `moactl` verbs or `moa` subcommands are refused in `moa.toml` (`RESERVED_BRAIN_NAMES`; a proto test checks every verb is listed). | Spec §7 row 1. `moa brief` already means `moactl brief`, so a brain can't be called `brief`. |
| T19 | **Tests drive a real terminal on a pseudo-terminal** (`moa_testkit::Terminal`, rustix, no `unsafe`), never `send-keys -K`. `tmux attach` works on a pty that isn't the controlling terminal, UTF-8 included **[3.4+3.8]**. | `send-keys -K` was fixed only in 3.8, and CI's Linux job runs 3.4. A real key goes through the root table, the moa table and the binding, as the user's does. |
| T20 | **D17's allow-list gains `TMUX_TMPDIR`.** | With `-L` names (real brains, slice 10), the daemon and the front door must compute the same socket path. |
| T22 | **`Notification`, `Mailbox` and `Cmd` live in `moa_core::tmux`, not `moa_tmux`** (roadmap names table, spec §6.2). `moa_tmux` re-exports them. | The core decides every tmux command and reads every notification, and it can't depend on `moa-tmux`, which has tokio (the `cargo tree` check in CI). Task 1 updates the roadmap's table. |
| T23 | **Tests reach tmux only through the testkit**: `Sandbox::tmux` (`-L` plus its own `TMUX_TMPDIR`), `TempBrain::tmux` (`-S <tmux_socket>`), and `Terminal::spawn`. All three clear the environment first. A testkit test fails if any other test file contains `Command::new("tmux")`. | The implementer agents run inside the user's live tmux (`TMUX` points at `-L moa`). A tmux command that forgot its socket, such as a test's `kill-server`, would hit the user's server. |
| T21 | **Until slice 10, nothing in v1 touches a real (`-L`) tmux server.**<br>• The daemon's tmux link stays off unless sandboxed: it logs `the terminal link waits for the switch-over`, and `status` shows it off. With no link, the core task answers `TmuxConnect` itself with `Down(Disabled)`, so `tmux.ensure` says `moa v1 isn't switched over yet; try moa --sandbox` instead of waiting forever.<br>• The front door refuses unless sandboxed: `moa v1 isn't switched over yet; try moa --sandbox`.<br>• This holds even with `MOA_REAL_HOME=1`. | The spike runs on `-L moa`/`-L moa-work` every day. A v1 daemon that sourced `moa.tmux` there would replace its key bindings. Slice 10 item 7 lifts this with the rest of the gate. |

## Parity rows this slice delivers (spec §7)

- `moa [BRAIN]`, refuse nesting (2.1): the front door, switching inside its own server
  (part; `--account` is slice 8).
- Explicit env for children, unset inherited `MOA_*` (2.1): the server's environment, T1
  (part; per-pane `-e` comes with launches in slice 4).
- Status line (2.5): pills, the urgent-only window list, counts, clock and bell.
- Ctrl-a key table, menu, prefix keys (2.6): all bindings; daemon actions are notices for
  now (part; help is slice 5).
- `moamenu` 4 s messages (2.6): `display-message -c <client> -d 4000`.

## Global Constraints

The roadmap's, 2a's and 2b's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`, tests with `cargo nextest run`.
- **Never touch the user's tmux servers.**
  - Every test uses a `Sandbox` (`-L` name under its own `TMUX_TMPDIR`) or a `TempBrain`
    (`-S` inside its root).
  - Manual runs use `moa --sandbox` only (T2, T21).
  - No test or manual step names `-L moa` or `-L moa-work`.
  - Tests run tmux only through the testkit's helpers, with the environment cleared (T23).
- **Never send a waiting command** (R2 §5.4) or `refresh-client`, and never an empty line.
  `Cmd::check` enforces it (T16).
- **Output can't fake a reply.**
  - Every `-F` format moa uses starts each line with a letter.
  - moa never runs a command whose output is user text (`capture-pane`, `show-buffer`)
    without a later slice's review of the parser rule in Task 3.
- **Zero launches** per key, menu item, status write, focus change or clock tick. The
  daemon launches one control client per connect, plus at most 6 backoff retries. The front
  door launches the server, and execs `tmux attach` or `switch-client`.
- **No tmux words in what the user reads**, except naming tmux when it must be installed or
  upgraded. Notices start with `moa: `.
- **No record contents in logs.**
  - tmux command lines are never logged.
  - A failed command logs its name and tmux's first error line; moa targets by id, so
    that line names ids.
- **Tests that need tmux are never skipped.** A missing tmux fails with the testkit's line.
  `Terminal` runs its command with `LANG=C.UTF-8`, and attach with `-u`.

## Review Focus

1. **The tmux server is killed or restarted under moa, and pane ids are reused.**
   - Expected:
     - the daemon notices at once (its control client gets `%exit`/EOF);
     - it never starts a server;
     - the next `moa` creates one and the daemon sees a new generation;
     - a binding from the old generation is never trusted.
   - Pinned by:
     - `a_new_generation_is_counted_once` and `a_binding_from_another_generation_is_not_trusted`
       (Tasks 5–6);
     - `the_daemon_never_starts_a_server` (Task 10);
     - `kill_server_then_moa_gives_a_new_generation` (Task 12).
2. **The daemon stops, crashes or is restarted while the user presses keys.**
   - Expected:
     - keys say "moa isn't running: run moa";
     - once it's back, keys work with nothing re-sourced by hand;
     - a user's `detach-client -a` only costs a reconnect.
   - Pinned by:
     - `keys_say_moa_isnt_running_without_a_ctl` and `the_ctl_option_is_cleared_when_the_ctl_dies`
       (Task 9);
     - `the_daemon_reconnects_after_its_client_is_detached` (Task 10);
     - `keys_say_moa_isnt_running_when_the_daemon_is_gone` (Task 12).
3. **Text with `'`, `"`, `\`, `$`, `~`, `#`, `;`, `{`, newlines or wide characters going into
   tmux commands, and typed into prompts.**
   - Expected: it never breaks a command or runs one, and it arrives as written (T17's
     quirk aside).
   - Pinned by `quote_round_trips_through_tmux` and `prompt_text_round_trips` (Task 4, on
     both CI tmux versions).
4. **Output that looks like protocol, or a 3.4 notification inside a reply.**
   - Expected: replies stay matched to their commands, and nothing is misread as a key.
   - Pinned by Task 3's proptests and `blocks_match_on_time_number_and_flags`, and by
     `an_error_reply_does_not_shift_the_rest` (Task 4).
5. **The user's own tmux config and hooks.**
   - Cases: a `~/.tmux.conf` with `run-shell`; user hooks at other indexes; user options.
   - Expected:
     - the user's options still apply where moa doesn't set them;
     - user hooks survive;
     - moa's hooks and `@moa_ctl` go on a clean stop;
     - the daemon never stalls.
   - Pinned by `user_hooks_at_other_indexes_survive` (Task 9),
     `shutdown_removes_hooks_and_the_ctl_option` (Task 10) and
     `a_user_tmux_conf_with_run_shell_and_hooks` (Task 12).

---

### Task 1: Split the core task, and note this plan's moves in the roadmap

**Files:**
- Move: `crates/moa-daemon/src/core_task.rs` →
  - `core_task/mod.rs`: `DaemonError`, `CoreOptions`, `Stats`, `ConnReply`, `Item`, `Held`,
    `CoreHandle`, `sync_scheduler`, `reply_to`, `lock`;
  - `core_task/drain.rs`: `Core`, `Flow`, `Stepped`, `run`, `drain`, `step`, `run_now`,
    `start_backup`, `wait_for_backups`, `release`, `Drop`, `take_drain`, and the `BUG_*`
    and `UNEXPECTED` lines;
  - `core_task/recover.rs`: `recover`, `backoff`, `reload`, `reopen_store`, `fatal`,
    `error_event`, `fail_held`, `cannot_open`.
- Modify: `docs/superpowers/plans/2026-11-15-moa-v1-roadmap.md`.

**Interfaces:** none change. `moa_daemon::core_task::*` keeps every public path.

- [ ] **Step 1: Move the code**, with no behaviour change: items keep their bodies and doc
  comments, and visibility widens only to `pub(super)` where a sibling needs it.
- [ ] **Step 2: Run** `cargo nextest run -p moa-daemon` and clippy. Everything passes
  unchanged, and no test file changes.
- [ ] **Step 3: Update the roadmap's Slice 3 section** with one line each. Item 1's
  `cargo-fuzz` becomes proptest (T15). Item 2's transactions move to slice 5 (T14). Item 3:
  the front door creates the server and the daemon runs tmux with `-N` (T1). The
  `alert-bell` and `client-active` hooks go to their consumers (T7). Replace the "Decide the
  daemon's PATH" carried note with "decided: T1". Add to slice 10 item 7: "and the tmux
  link's sandbox-only gate (slice 3, T21)". Change Slice 3's try-it socket to "a socket
  inside the sandbox root (T2)". In the "Shared names" table, move `Notification`, `Cmd`
  and `Mailbox` to `moa_core::tmux`, leaving `ControlClient` in `moa_tmux` (T22).
- [ ] **Step 4: Commit** `git commit -m "v1: split the core task into drain and recovery; roadmap notes for slice 3"`

---

### Task 2: `moa_core::tmux`, the vocabulary

**Files:**
- Create: `crates/moa-core/src/tmux/{mod.rs, ids.rs, notify.rs, mailbox.rs, cmd.rs,
  topology.rs}`, `crates/moa-core/tests/tmux_vocab.rs`
- Modify: `crates/moa-core/src/lib.rs` (`pub mod tmux`)

**Interfaces:**
- Produces:
  ```rust
  // ids.rs — Display and FromStr use tmux's spelling; FromStr needs the exact prefix, then a u32
  #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)] pub struct PaneId(pub u32);        // "%3"
  #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)] pub struct WindowId(pub u32);      // "@2"
  #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)] pub struct TmuxSessionId(pub u32); // "$0"
  #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)] pub struct ClientName(pub String);      // "/dev/ttys1", "client-123"
  #[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)] pub struct Generation { pub pid: i64, pub start: i64 }
  // notify.rs — what the parser (moa-tmux) produces from a '%' line
  pub enum Notification {
      WindowPaneChanged { window: WindowId, pane: PaneId },
      SessionWindowChanged { session: TmuxSessionId, window: WindowId },
      ClientSessionChanged { client: ClientName, session: TmuxSessionId },
      ClientDetached { client: ClientName },
      SessionChanged { session: TmuxSessionId },   // the control client's own
      SessionsChanged,
      WindowAdd { window: WindowId },              // %window-add and %unlinked-window-add
      WindowClose { window: WindowId },            // %window-close and %unlinked-window-close
      LayoutChange { window: WindowId },
      Message(String),                             // %message, verbatim (T17)
      Exit,
      Other(&'static str),                         // known, unused here: its name ("window-renamed", "output", …)
      Unknown(String),                             // an unknown %name (the name only)
  }
  // mailbox.rs
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum KeyAction { Console, Tree, Plan, Agent, Find, FindOther, New, Terminal, Layout(u8), Repair, Help }
  impl KeyAction {
      pub const ALL: [KeyAction; 14];              // Layout(1..=4)
      pub fn word(self) -> String;                 // console tree plan agent find find-other new terminal layout-1..4 repair help
      pub fn from_word(w: &str) -> Option<KeyAction>;
      pub fn key(self) -> &'static str;            // c t p g / s n $ 1 2 3 4 r ?
      pub fn label(self) -> &'static str;          // menu text (below)
      pub fn not_yet(self) -> &'static str;        // the notice until its slice lands (below)
  }
  pub enum HookMsg { PaneExited(PaneId), PaneDied(PaneId), Layout(WindowId), ClientAttached(ClientName) }
  pub enum Mailbox {
      Key { action: KeyAction, pane: Option<PaneId>, session: TmuxSessionId, client: ClientName },
      Hook(HookMsg),
      Prompt { token: u64, text: String },
  }
  pub enum MailboxError { NotMoa, Bad }
  impl Mailbox { pub fn parse(message: &str) -> Result<Mailbox, MailboxError>; }
  // cmd.rs
  #[derive(Clone, PartialEq, Eq, Debug)] pub struct Cmd { /* name: &'static str, args: Vec<String> */ }
  impl Cmd {
      pub fn new(name: &'static str) -> Cmd;
      pub fn arg(self, a: impl Into<String>) -> Cmd;
      pub fn args<S: Into<String>>(self, a: impl IntoIterator<Item = S>) -> Cmd;
      pub fn name(&self) -> &str;
      pub fn argv(&self) -> &[String];
      pub fn line(&self) -> String;                // name + quote(arg)…, space-joined; never holds '\n' or '\r'
      pub fn check(&self) -> Result<(), Refused>;  // T16
  }
  pub struct Refused(pub &'static str);            // "that isn't a command moa sends" | "command-prompt needs -b" | "moa only sources its own config"
  pub fn quote(s: &str) -> String;                 // T16
  pub const ALLOWED: &[&str];
  // topology.rs
  pub const LIST_SESSIONS: &str = "S #{session_id} #{session_name}";
  pub const LIST_PANES: &str = "P #{pane_id} #{window_id} #{session_id} #{pane_active} #{window_active} #{window_zoomed_flag} #{@moa_session}";
  pub const LIST_CLIENTS: &str = "C #{client_control_mode} #{session_id} #{client_name}";
  pub fn refresh_cmds() -> Vec<Cmd>;               // list-sessions -F, list-panes -a -F, list-clients -F (that order)
  pub struct Window { pub panes: BTreeSet<PaneId>, pub active: Option<PaneId>, pub zoomed: bool }
  #[derive(Clone, Default, Debug)] pub struct Topology { /* sessions, current window per session, windows, user clients, @moa_session per pane */ }
  pub enum Applied { Changed, Same, Refresh }
  impl Topology {
      pub fn from_lists(sessions: &[String], panes: &[String], clients: &[String]) -> Topology;  // bad lines are skipped
      pub fn apply(&mut self, n: &Notification) -> Applied;
      pub fn visible(&self) -> BTreeSet<PaneId>;
      pub fn users_attached(&self) -> bool;
      pub fn session_named(&self, name: &str) -> Option<TmuxSessionId>;
      pub fn has_pane(&self, p: PaneId) -> bool;
      pub fn window_of(&self, p: PaneId) -> Option<WindowId>;
      pub fn client_pane(&self, c: &ClientName) -> Option<PaneId>;
  }
  ```
- **Labels and notices** (`label` / `not_yet`):

  | Action | Label | Notice |
  |---|---|---|
  | Console | `Console` | `the console isn't in this build yet` |
  | Tree | `Sessions tree` | `the sessions tree isn't in this build yet` |
  | Plan | `Big picture` | `the big picture isn't in this build yet` |
  | Agent | `Agent spot` | `the agent spot isn't in this build yet` |
  | Find | `Find a session…` | `finding a session isn't in this build yet` |
  | FindOther | `Find a session, full window…` | (same as Find) |
  | New | `New session…` | `new sessions aren't in this build yet` |
  | Terminal | `Terminal here` | `terminals aren't in this build yet` |
  | Layout 1–4 | `Layout: cockpit`, `Layout: focus (agent)`, `Layout: classic`, `Layout: triage (status)` | `layouts aren't in this build yet` |
  | Repair | `Repair panels` | `repair isn't in this build yet` |
  | Help | `Keys and help` | `help isn't in this build yet` |
- **Mailbox lines** (after `moa:`; anything else is `NotMoa`, and a malformed `moa:` line is
  `Bad`):
  - `key WORD PANE SESSION CLIENT…`: PANE is `%N` or `-`; CLIENT is the rest of the line.
  - `hook pane-exited %N`, `hook pane-died %N`, `hook layout @N`, `hook client-attached CLIENT…`.
  - `prompt TOKEN TEXT…`: TEXT may be empty; `\$` becomes `$` (T17).
- **`Topology` rules:**
  - `list-panes` rows give each window's panes, its active pane and its zoom. Its
    `window_active` rows give each session's current window.
  - `@moa_session` (the last field) is parsed as a `SessionId`. Empty, missing or anything
    else is `None`: tmux prints an unset option as nothing.
  - `list-clients` rows with `client_control_mode` 1 are left out.
  - `visible()`: for each user client → its session → that session's current window → all
    its panes, or only the active pane when the window is zoomed.
  - `apply`:
    - `WindowPaneChanged`, `SessionWindowChanged` and `ClientDetached` change it in place.
    - A `ClientSessionChanged` for a known client changes it in place; for an unknown client
      it's `Refresh`.
    - `WindowAdd`, `WindowClose`, `SessionsChanged` and `LayoutChange` are `Refresh`.
    - Anything else is `Same`.

- [ ] **Step 1: Write the failing tests** (`tests/tmux_vocab.rs`)
  - `ids_print_and_parse_like_tmux`: `"%3".parse::<PaneId>() == Ok(PaneId(3))`;
    `"@3".parse::<PaneId>()` is an error; `TmuxSessionId(0).to_string() == "$0"`.
  - `quote_leaves_safe_words_bare_and_quotes_the_rest`:
    - `quote("=moa:moa") == "=moa:moa"`;
    - `quote("it's") == r"'it'\''s'"`;
    - `quote("a\nb") == r#"'a'"\n"'b'"#`;
    - `quote("") == "''"`;
    - `quote("~/x") == "'~/x'"`;
    - `quote("#{pane_id}") == "'#{pane_id}'"`.
  - `a_line_never_holds_a_newline` (proptest over any `String` arguments).
  - `the_allow_list_refuses_waiting_and_unknown_commands`:
    - `wait-for`, `run-shell`, `display-popup`, `confirm-before`, `if-shell` and the alias
      `set` → `Err`;
    - `command-prompt` without `-b` → `Err`, with `-b` → `Ok`;
    - `source-file /x/tmux.conf` → `Err`; `source-file /x/moa.tmux` → `Ok`.
  - `mailbox_lines_parse`:
    - `moa:key console %3 $0 /dev/ttys1` → `Key{Console, Some(%3), $0, "/dev/ttys1"}`;
    - `moa:key layout-2 - $1 client 7` → `pane: None`, `client == "client 7"`;
    - `moa:hook pane-died %4`;
    - `moa:prompt 7 it's "a" \$HOME` → `Prompt{7, "it's \"a\" $HOME"}`;
    - `moa:prompt 8 ` → empty text;
    - `hello` → `NotMoa`; `moa:key nope %1 $0 c` → `Bad`.
  - `every_action_has_a_word_key_label_and_notice`: `from_word(word(a)) == Some(a)` for
    every action; keys are distinct; no notice has a tmux word ("tmux", "pane", "window",
    "session").
  - `topology_from_lists_and_visible`, built from the lines the plan's appendix shows:
    - two windows in `$0`, the current one `@2` with `%2 %3` and active `%3`;
    - one user client on `$0`, one control client on `$1`;
    - `visible() == {%2, %3}`; zoomed → `{%3}`;
    - with no user client → `{}`.
  - `topology_apply`:
    - `SessionWindowChanged{$0, @0}` makes `@0`'s panes visible (`Changed`);
    - `ClientDetached` of the user → `visible()` empty;
    - `WindowAdd` → `Refresh`;
    - an unknown client's `ClientSessionChanged` → `Refresh`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-core tmux vocabulary: ids, notifications, mailbox, quoted commands, topology"`

---

### Task 3: `moa-tmux`: the byte parser

**Files:**
- Create: `crates/moa-tmux/{Cargo.toml, src/lib.rs, src/parse.rs}`, `tests/parse.rs`. The
  two captures are `const` strings inside `tests/parse.rs`, because the flake's source
  filter (`flake.nix` ~38) drops `.txt` files.
- Cargo: `moa-core`, `tracing`; dev: `proptest`.

**Interfaces:**
- Produces:
  ```rust
  pub const MAX_LINE: usize = 8 << 20;
  pub const MAX_BLOCK: usize = 64 << 20;
  pub enum Frame {
      Reply { flags: u32, error: bool, lines: Vec<String> },   // lines decoded as lossy UTF-8
      Notification(Notification),
      Stray,                                                   // a non-'%' line outside any block
  }
  #[derive(Debug, thiserror::Error)] pub enum ParseError { #[error("a line from tmux is too long")] LineTooLong, #[error("a reply from tmux is too long")] BlockTooLong }
  pub struct Parser { /* buffer, open block (time, number, flags, lines, bytes) */ }
  impl Parser {
      pub fn new() -> Parser;
      pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, ParseError>;
  }
  pub fn notification(line: &str) -> Notification;   // a line starting with '%'; %message text verbatim (T17)
  ```
- **Rules** (R2 §1.1):
  - Outside a block:
    - `%begin T N F` opens a block;
    - any other `%` line is a notification;
    - other lines are `Stray`.
  - Inside a block:
    - only `%end`/`%error` with the exact same `T N F` closes it;
    - a line whose first word is a known notification name (the R2 §1.2 table, minus
      `%begin`/`%end`/`%error`) is a notification, for pre-3.8 servers;
    - every other line is content, including `%end` lines with other numbers.

- [ ] **Step 1: Write the failing tests** (`tests/parse.rs`)
  - `blocks_match_on_time_number_and_flags`:
    - content lines `%end 1 2 1` and `%end T N 0` stay content inside block `T N 1`;
    - the exact `%end T N 1` closes it.
  - `flag_zero_blocks_keep_their_flags`; `an_error_block_is_an_error`.
  - `a_notification_inside_a_block_is_a_notification`: `%begin…`, `x`,
    `%unlinked-window-add @3`, `y`, `%end…` → a `Notification(WindowAdd{@3})`, then a
    `Reply` with lines `["x", "y"]`.
  - `notifications_parse`:
    - `%window-pane-changed @0 %3`;
    - `%session-window-changed $0 @2`;
    - `%unlinked-window-close @2`;
    - `%client-session-changed /dev/ttys033 $0 work`;
    - `%client-detached /dev/ttys038`;
    - `%layout-change @0 b653,200x60,0,0 b653,200x60,0,0 *`;
    - `%message moa:key x %1 $0 /dev/ttys045`;
    - `%exit` and `%exit too far behind` → `Exit`;
    - `%window-renamed @1 bash` → `Other("window-renamed")`;
    - `%zap 1` → `Unknown("zap")`.
  - `message_text_is_verbatim`: `%message a\026b \101` → `"a\\026b \\101"`.
  - `the_captures_parse_to_their_frames`: the `V3_4` and `V3_8` constants (built from the
    appendix transcript) give their frames, in order: each block a `Reply` with its lines
    and flags, each `%` line its `Notification`.
  - `split_anywhere_gives_the_same_frames` (proptest): a capture pushed in random chunk
    sizes gives the same frames as one push.
  - `any_bytes_never_panic` (proptest, arbitrary chunks of arbitrary bytes, including `%`,
    `\n` and invalid UTF-8).
  - `a_huge_line_is_an_error`: 9 MiB with no newline → `LineTooLong`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: moa-tmux: control-mode byte parser, fuzzed with proptest"`

---

### Task 4: `ControlClient`, the testkit's `Terminal`, and real-tmux tests

**Files:**
- Create: `crates/moa-tmux/src/client.rs`, `crates/moa-tmux/tests/client.rs`,
  `crates/moa-testkit/src/terminal.rs`
- Modify:
  - `crates/moa-tmux/Cargo.toml`: `moa-platform`; `tokio` (`process`, `io-util`, `sync`,
    `rt`, `macros`, `time`); dev: `moa-testkit`, `tokio` (`rt-multi-thread`).
  - `crates/moa-testkit/{Cargo.toml, src/lib.rs, src/sandbox.rs}`: `rustix` (`pty`,
    `termios`, `fs`); `Sandbox::without_server`, `Sandbox::socket_args`, and
    `tests/tmux_only_through_the_testkit.rs` (T23).
  - `crates/moa-platform/src/{lib.rs, paths.rs}`: `pub fn which(name: &str, path: Option<&OsStr>) -> Option<PathBuf>`
    (the first executable `dir/name` in `PATH`'s absolute entries).

**Interfaces:**
- Produces:
  ```rust
  // moa_tmux::client
  pub const INFO_FORMAT: &str = "V #{version} #{pid} #{start_time} #{client_name}";
  pub struct ConnectOptions { pub tmux: PathBuf, pub socket: Vec<OsString>, pub env: Vec<(OsString, OsString)> }
  pub struct ServerInfo { pub version: String, pub generation: Generation, pub client: ClientName }
  #[derive(Debug, thiserror::Error)]
  pub enum ConnectError { Spawn(io::Error), Exited(String) /* tmux's first stderr line */, TooOld(String), Protocol(&'static str) }
  #[derive(Clone, Debug, PartialEq, thiserror::Error)]
  pub enum CmdError { #[error("{0}")] Tmux(String) /* the %error text */, #[error("{0}")] Refused(&'static str),
                      #[error("the terminal server connection closed")] Gone }
  pub enum Event { Notification(Notification), Replies { tag: u64, results: Vec<Result<Vec<String>, CmdError>> } }
  pub type Events = mpsc::UnboundedReceiver<Event>;   // None once the client has exited
  pub struct ControlClient { /* writer task sender, child pid */ }
  impl ControlClient {
      pub async fn connect(opts: &ConnectOptions) -> Result<(ControlClient, Events, ServerInfo), ConnectError>;
      pub async fn run(&self, cmd: &Cmd) -> Result<Vec<String>, CmdError>;   // awaited: bootstrap and tests
      pub fn send(&self, cmds: Vec<Cmd>, tag: Option<u64>);                  // one write; tagged results arrive as Event::Replies
      pub async fn close(self);                                               // close stdin, wait ≤ 1 s, then kill
      #[doc(hidden)] pub fn child_pid(&self) -> u32;
  }
  pub fn ctl_argv(socket: &[OsString]) -> Vec<OsString>;   // T3, after the tmux path
  pub fn version_ok(v: &str) -> bool;                       // ≥ 3.4: "3.4" "3.4a" "3.8" "next-3.9" "master" ok; "3.3a" "3.2" "2.9a" not
  // moa_testkit
  pub struct Terminal { /* pty master, child, output buffer and its reader thread */ }
  impl Terminal {
      pub fn spawn(program: &Path, args: &[OsString], env: &[(OsString, OsString)], cols: u16, rows: u16) -> io::Result<Terminal>;
      // env_clear(), then `env`, then TERM=xterm-256color and LANG=C.UTF-8 when `env` lacks them; stdin/stdout/stderr on the pty
      pub fn press(&self, bytes: &[u8]);
      pub fn mark(&self);                                          // later waits see only output after this
      pub fn wait_for_text(&self, text: &str, limit: Duration) -> bool;
      pub fn tty(&self) -> String;                                 // the pty's path: the tmux client's name
  }                                                                // Drop kills the child
  impl Sandbox {
      pub fn without_server() -> io::Result<Sandbox>;
      pub fn socket_args(&self) -> Vec<OsString>; // ["-L", name]; `env()` already carries its TMUX_TMPDIR
  }
  ```
- **`connect`:**
  1. Spawn `tmux` + `ctl_argv` through `Spawner` (std `Command`): `env_clear().envs(env)`,
     stdin, stdout and stderr piped. Reap the child on a thread, as autostart does.
  2. Write `display-message -p INFO_FORMAT` at once.
  3. Read frames, skipping flag-0 blocks, until the first flag-1 reply. EOF first →
     `Exited(first stderr line)`.
  4. Parse `ServerInfo`. If `!version_ok` → `close`, then `TooOld(version)`.
- **The writer** is one task.
  - It pushes each command's pending entry before writing its line, so the reader's FIFO pop
    always matches.
  - A `Cmd` that fails `check()` is never written: its result is `Err(Refused)` in place.
  - Untagged batches' errors are logged as `tmux refused <name>: <first line>`.
- **The reader** sends `Event::Replies` at the moment a batch's last block completes, on the
  same channel as notifications, so the stream keeps tmux's order.
  - On EOF, every pending entry gets `Gone` and the channel closes.

- [ ] **Step 1: Write the failing tests** (`tests/client.rs`; real tmux through `Sandbox`;
  `tmux` from `moa_platform::which`)
  - `connect_reads_version_pid_start_and_name`:
    - `info.generation.pid` equals the sandbox's `display -p '#{pid}'`;
    - `version_ok(&info.version)`;
    - the client name starts with `client-`.
  - `connect_never_starts_a_server`: with `Sandbox::without_server()`, `connect` →
    `Err(Exited(_))`, and `tmux ls` still fails afterwards.
  - `connect_creates_the_hidden_session_once`: after two connects, `list-sessions` shows
    exactly one `_moa`.
  - `pipelined_replies_match_in_order`: 50 `display-message -p "R {i}"` in one tagged
    `send` → `Replies{tag, results}` with `results[i] == Ok(["R {i}"])`.
  - `an_error_reply_does_not_shift_the_rest`: `[ok, list-panes -t %999, ok]` →
    `[Ok, Err(Tmux(t)), Ok]`, where `t` contains `can't find`.
  - `after_hook_blocks_are_skipped`:
    - the sandbox sets `after-new-session[91]` to `display-message -p from-hook`;
    - `run(new-session -d -s w)`, then `run(display-message -p next)` == `["next"]`.
  - `refused_commands_are_never_written`: `run(Cmd::new("wait-for"))` → `Err(Refused(_))`,
    and the next command still answers.
  - `the_events_end_when_the_server_is_killed`:
    - `kill-server` → the events channel ends within 2 s;
    - then `run(…)` → `Err(Gone)`.
  - `quote_round_trips_through_tmux` (proptest, 64 cases):
    - the strategy: printable characters plus `' " \ $ ~ # ; { } \n \t`, and `✓ 中 💤`;
    - `run(set-option -g @q <s>)`, then `display-message -p '#{@q}'`;
    - the reply's lines joined with `\n` equal `s`, with T17's `$` quirk applied to the
      expected value when the version starts with `3.4`.
  - `prompt_text_round_trips`:
    - a `Terminal` runs `tmux -u <sock> attach -t =sandbox`;
    - send `command-prompt -b -t <tty> -p x <template>`, where the template is
      `display-message -c <ctl> -l "moa:prompt 7 %%%"`, built with `Cmd`;
    - type `it's "a" #{x} ; b \ ✓ $HOME` and Enter;
    - the `Message` event parses to `Prompt{7, that text}`.
  - `version_ok_table` (a unit test in `client.rs`).
  - `tmux_only_through_the_testkit` (in `moa-testkit/tests`):
    - scan every `crates/*/tests/**/*.rs` file;
    - none contains `Command::new("tmux")` or `Command::new(tmux`;
    - the testkit's own `src/` is exempt (T23).
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**, three times. Then run `nix flake check` once:
  the pty must work in the Nix build. If the darwin sandbox refuses `/dev/ptmx`, stop and
  report; don't skip the test.
- [ ] **Step 5: Commit** `git commit -m "v1: moa-tmux control client (FIFO replies, ordered events, allow-list), pty Terminal for tests"`
- [ ] **Step 6: Open PR c1** (Tasks 1–4) with `/code-review medium --fix`. Start Task 5
  from `main` once it merges.

---

### Task 5: The link in the core: connect, ensure, refresh, the `moa` session, generations

**Files:**
- Create: `crates/moa-core/src/tmux/link.rs`, `crates/moa-core/tests/tmux_link.rs`
- Modify:
  - `crates/moa-core/src/{effect.rs, core.rs, op.rs (incl. `Op::METHODS`), state.rs, ops/mod.rs}`;
  - `crates/moa-core/tests/skeleton.rs` (~448): a sample for the new op;
  - `crates/moa-proto/src/wire.rs` (`StatusOk`), the schema snapshot;
  - `crates/moa-daemon/src/core_task/{drain.rs, recover.rs}`:
    - `fail_held` drops a held `Tmux` (T10);
    - until Task 10 wires the actor, the core task answers `TmuxConnect` with
      `Down(Disabled)` and drops `Tmux` effects with a debug log.

**Interfaces:**
- Produces:
  ```rust
  // effect.rs
  pub enum Input { …, Tmux(TmuxIn) }
  pub enum TmuxIn {
      Connected { conn: u64, generation: Generation, version: String, ctl: ClientName },
      Down(Down),
      Notification(Notification),
      Replies { conn: u64, token: TmuxToken, results: Vec<Result<Vec<String>, String>> },
  }
  pub enum Down { Lost, NoServer, GaveUp, TooOld(String), NoTmux, Disabled }
  #[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum TmuxToken { Refresh, Cockpit }
  pub enum TimerKey { Backup, Clock }
  pub enum Effect { …, Tmux { conn: u64, cmds: Vec<Cmd>, token: Option<TmuxToken> }, TmuxConnect { tmux: Option<PathBuf> } }
  // Effect::runs_at_once is also true for TmuxConnect and a tokened Tmux (T10); check_order counts only an untokened Tmux as heard
  // op.rs
  #[serde(rename = "tmux.ensure")] TmuxEnsure { #[serde(default)] tmux: Option<PathBuf> },   // writes() == false
  // answered Done { text: "", data: json!({"running": bool}) }
  // state.rs
  pub struct StatusData { …, pub tmux: Option<TmuxLinkData>, pub tmux_restarts: u32 }
  #[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]   // + the `schema` cfg_attr, like CallerInfo
  pub struct TmuxLinkData { pub version: String, pub pid: i64, pub start: i64 }
  impl State { pub fn tmux(&self) -> &TmuxState; }
  // tmux/link.rs
  pub enum Link { Down, Connecting, Up { conn: u64, generation: Generation, version: String, ctl: ClientName } }
  pub struct TmuxState { pub link: Link, pub topology: Topology, pub restarts: u32, /* waiters, refresh, last generation, tmux path, written, clock */ }
  ```
  `StatusOk` gains `#[serde(default)] tmux: Option<TmuxLinkData>` and
  `#[serde(default)] tmux_restarts: u32`.
- **Transitions** (every effect goes through `handle_checked`'s order rule):
  - Every `Tmux` effect carries `Up.conn`. `Replies` from another `conn`, or while not
    `Up`, are dropped.
  - **`Startup`:** `link = Connecting`; emit `TmuxConnect { tmux: None }`, after Startup's
    `Persist` (it runs at once, so its place doesn't matter to `check_order`).
  - **`TmuxEnsure { tmux }`** (`core.rs` takes it before `ops::run`; `ops::run`'s arm
    answers `moa can't answer that here`):
    1. a `tmux` that isn't an absolute path ending in `/tmux` → `Err("that isn't a tmux program")`;
    2. remember `tmux` when given;
    3. if `Up` and `session_named("moa")` → reply `{running: true}`;
    4. if `Up` without `moa` → record the waiter, clear the cockpit attempt, and refresh;
    5. if `Connecting` → record the waiter;
    6. if `Down` → record the waiter, then `Connecting` plus `TmuxConnect { tmux }`.
  - **`Connected`:**
    - `link = Up` with its `conn`;
    - if a last generation exists and differs, then `restarts += 1` and
      `Log(Info, "the terminal server restarted")`;
    - remember the generation;
    - forget what was written (Task 7);
    - emit `Tmux { refresh_cmds(), Some(Refresh) }`.
  - **`Replies { Refresh, … }`:**
    - with link `Up` and its `conn` only;
    - if any of the three lists failed, the reply is "no information": the topology stays
      as it was, `Log(Warn, "listing the terminal server failed")`, and nothing is created
      or answered (the next structural event or ensure refreshes again);
    - else `topology = from_lists(…)`;
    - another refresh if one was asked meanwhile;
    - no `moa` session → `Tmux { [new-session -d -s moa -n moa], Some(Cockpit) }`, once
      per missing spell;
    - otherwise answer every waiter `{running: true}`.
  - **`Replies { Cockpit, [Err(e)] }`:** the waiters get
    `moa couldn't open its window; see the log`, and `Log(Warn, "creating the moa session failed: <e first line>")`.
  - **`Notification(n)`:**
    - `Message` is Task 8's;
    - `Exit` is ignored (the actor reports `Down`);
    - otherwise `topology.apply(n)`; `Refresh` → a refresh (coalesced, T9).
  - **`Down(d)`:**
    - `Lost` → `link = Connecting`, because the actor is reconnecting. The waiters stay.
    - Everything else → `link = Down`.
    - Either way:
      - empty the topology;
      - forget what was written, the refresh in flight and the cockpit attempt;
      - cancel the clock (Task 7).
    - The waiters are answered:
      - `NoServer` → `{running: false}`;
      - `GaveUp` → `moa lost its terminal server and couldn't reconnect; see the log`;
      - `TooOld(v)` → `moa needs tmux 3.4 or newer; this one is {v}`;
      - `NoTmux` → `moa can't find tmux; install tmux 3.4 or newer`;
      - `Disabled` → `moa v1 isn't switched over yet; try moa --sandbox`.
  - **`Reload`:**
    - keeps the link, the topology, the waiters, the generation and the restart count
      (`State::load` doesn't touch them);
    - resets the refresh in flight, the cockpit attempt and what was written (T10);
    - then refreshes when `Up`, and emits `TmuxConnect` when `Connecting`.
  - **`State::status`** fills `tmux` when `Up`, and `tmux_restarts`.

- [ ] **Step 1: Write the failing tests** (`tests/tmux_link.rs`; build states with the 2a
  helpers and run every input through `handle_checked`)
  - `startup_asks_to_connect`.
  - `ensure_waits_for_the_link_and_the_moa_session`:
    - ensure while `Connecting` → no reply;
    - `Connected` → the refresh effect;
    - `Replies(Refresh)` with `S $0 moa`, `S $1 _moa` → `Reply {running: true}`.
  - `ensure_without_a_server_says_not_running`:
    - `Down(NoServer)` → `{running: false}`;
    - a later ensure with a path → `TmuxConnect { tmux: Some(path) }`.
  - `a_missing_moa_session_is_created_once`:
    - a refresh reply without `moa` → exactly one `Cockpit` effect;
    - a second reply before `SessionsChanged` → none;
    - `SessionsChanged` → a refresh → with `moa` present the waiter gets its reply.
  - `a_failed_cockpit_create_answers_the_waiters`.
  - `refreshes_coalesce`: three structural notifications during one refresh → exactly one
    more refresh after its reply.
  - `too_old_and_missing_tmux_answer_in_one_line`: the exact lines above.
  - `lost_keeps_the_waiters_and_connects_once`: `Down(Lost)` → `Connecting`; an ensure then
    records a waiter and emits no `TmuxConnect`.
  - `a_new_generation_is_counted_once`:
    - `Connected g1`, `Down(NoServer)`, `Connected g2` → `restarts == 1`, with the log
      line;
    - `Down(Lost)`, then `Connected g2` again → still 1.
  - `replies_from_an_old_connection_are_dropped`: `Connected{conn 1}`, `Down(Lost)`,
    `Connected{conn 2}`, then a `Replies{conn 1, Refresh, …}` → no change, no effect.
  - `a_failed_list_keeps_the_topology`: a refresh whose `list-panes` failed → topology
    unchanged, no `Cockpit`, no reply, one warning.
  - `ensure_after_a_failed_cockpit_create_tries_again`.
  - `a_disabled_link_answers_ensure`: `Down(Disabled)` → the waiter gets the
    not-switched-over line.
  - `a_bad_tmux_path_is_refused`: `tmux: "rm"` and `"/bin/sh"` → `Err`.
  - `reload_keeps_the_link_and_restarts_its_bookkeeping`:
    - a refresh is in flight, and then `Reload`;
    - → a new refresh is emitted;
    - its reply answers the ensure waiter.
  - `effect_ordering`:
    - `check_order([Tmux{token: None, …}, Persist(…)])` is `Err`;
    - `Tmux{token: Some(Refresh)}` and `TmuxConnect` run at once.
  - `status_shows_the_link`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**, plus the proto schema snapshot (review it, then
  accept).
- [ ] **Step 5: Commit** `git commit -m "v1: the tmux link in the core: connect, tmux.ensure, refresh, the moa session, generations"`

---

### Task 6: The focus model and `seen`

**Files:**
- Create: `crates/moa-core/src/tmux/focus.rs`, `crates/moa-core/tests/tmux_focus.rs`
- Modify: `crates/moa-core/src/{core.rs, tmux/link.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub(crate) fn trusted_panes(state: &State) -> BTreeMap<PaneId, SessionId>;  // T11
  pub(crate) fn mark_seen(state: &mut State, tx: &mut Tx, now: &Now);         // commits through Tx, like every op
  ```
- **Trust:** a pane binding is trusted when it's in the current generation
  (`server_pid`/`server_start` == `Up.generation`) and `topology.has_pane(pane_id)`.
  (`pane_id` is stored as `%N`.)
- **`mark_seen`** applies `display_state`'s own rule (`live.rs` ~165): for every trusted pane
  in `visible()` whose session has `last_turn_end_at > seen_at`, both set, it sets
  `seen_at = now`.
  - One `tx.commit` per session, so `commit`'s guards apply.
  - A session row this build doesn't fully know (`is_unknown`) is skipped.
  - While writes are refused (`refuse_writes`), nothing is marked: it's skipped, not an
    error, and isn't retried until the next focus change.
  - A session with no `seen_at` is already `Idle`, so it isn't touched.
- After every `Input::Tmux`:
  - the `Tx`'s changes become the input's `Persist`, which comes first;
  - `Topic::Sessions` is published with them.
  - Slice 4 calls `mark_seen` after hook events too.

- [ ] **Step 1: Write the failing tests** (`tests/tmux_focus.rs`). The base state: session S
  is live and `Done` (`last_turn_end_at` 10, `seen_at` 5), with a binding `(g, %3)`.
  - `a_visible_done_session_becomes_seen`:
    - `Connected g`;
    - a refresh that puts `%3` in `$0`'s current window, with a user client on `$0`;
    - → `Persist([Session{seen_at: now}])` before the `Publish`, and S's display state is
      `Idle`.
  - `zoom_hides_the_other_panes`: the window is zoomed on `%4` → no change.
  - `a_binding_from_another_generation_is_not_trusted`: binding `(1, 1)`, `Connected (2, 2)`,
    `%3` visible → no change.
  - `focus_notifications_move_visibility`:
    - `%3` starts in a background window;
    - `SessionWindowChanged` to it → seen;
    - the same for `WindowPaneChanged` into a zoomed window.
  - `control_clients_make_nothing_visible`.
  - `a_detached_user_sees_nothing`.
  - `seen_is_written_once`: the same focus again → no `Persist`.
  - `seen_respects_the_write_guards`:
    - an S loaded with an unknown value → no change;
    - with `refuse_writes` active → no change and no error.
  - `structural_changes_refresh`: `WindowAdd`, `WindowClose`, `SessionsChanged`, and the
    hook messages for pane-exited, pane-died, layout and client-attached → one refresh.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: focus model: visibility from tmux events, seen for trusted panes"`

---

### Task 7: The status line and the minute clock

**Files:**
- Create: `crates/moa-core/src/tmux/status.rs`, `crates/moa-core/tests/tmux_status.rs`
- Modify: `crates/moa-core/src/{core.rs, tmux/link.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub fn counts_text(state: &State) -> String;
  pub fn clock_text(now: &Now) -> String;                         // local "HH:MM"
  pub fn next_minute(at: UtcMs) -> UtcMs;                         // the next whole UTC minute, strictly after `at`
  pub(crate) fn sync(state: &mut State) -> Option<Effect>;        // Tmux{cmds, None}, or None when nothing changed or not Up
  pub(crate) fn clock(state: &mut State, now: &Now) -> Vec<Effect>;
  ```
- **`counts_text`:**
  - the live sessions' display states, in urgency order;
  - only states with a non-zero count;
  - each piece is `#[fg=<colour>]<glyph> <n>`, joined by two spaces;
  - the colours: waiting `#ebcb8b`, question `#d08770`, failed `#d57780`, done `#a3be8c`,
    working `#88c0d0`, idle `#8c9ab3`;
  - asleep, ended and archived aren't counted.
- **`sync`** compares against what was written last:
  - `set-option -g @moa_status <text>`;
  - `set-option -w -t @W @moa_state <word>`, where the word is the most urgent display state
    of `@W`'s trusted panes;
  - `set-option -w -u -t @W @moa_state` for windows written before that no longer have one
    and still exist; windows that are gone are just forgotten.
  - It runs at the end of every `handle` while `Up`.
- **`clock`** runs at the end of every `handle` while `Up`:
  - Users attached and the clock not armed → `set-option -g @moa_clock <HH:MM>`, then
    `SetTimer { Clock, next_minute(now) }`.
  - No users and armed → `CancelTimer(Clock)`.
  - `Timer(Clock)` → write and re-arm, or disarm when nobody is attached.
  - `ZoneChanged` while armed → write and re-arm.
  - `Down` while armed → `CancelTimer(Clock)`.

- [ ] **Step 1: Write the failing tests** (`tests/tmux_status.rs`)
  - `counts_text_matches_the_spike`:
    - waiting 2, question 1, failed 1, done 3, working 4, idle 19 →
      `#[fg=#ebcb8b]⚠ 2  #[fg=#d08770]? 1  #[fg=#d57780]✗ 1  #[fg=#a3be8c]✓ 3  #[fg=#88c0d0]◐ 4  #[fg=#8c9ab3]● 19`;
    - zeros are left out;
    - no live sessions → `""`.
  - `the_status_is_written_once_per_change`:
    - after connecting, exactly one `@moa_status` write;
    - an input that changes nothing → no `Tmux` effect;
    - `session.archive` of a live session → a new `@moa_status`, after its `Persist`.
  - `window_states_follow_trusted_panes_and_are_unset_when_gone`.
  - `the_clock_runs_only_while_someone_is_attached`:
    - no users → no `Clock` timer;
    - a user attaches at 14:05:30 local → `@moa_clock 14:05` and a timer at 14:06:00;
    - the timer fires → `14:06` and a timer at 14:07:00;
    - the user detaches → `CancelTimer(Clock)`.
  - `an_idle_brain_with_nobody_attached_wakes_only_for_backups`: a simulated day, connected
    with no users → the only `SetTimer`s are `Backup`.
  - `the_clock_follows_a_zone_change`: from `EST5EDT…` to `PST8PDT…`, the text changes and
    the timer stays at the next minute.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: status line counts, per-window state and a minute clock while someone looks"`

---

### Task 8: Keys and the mailbox in the core

**Files:**
- Create: `crates/moa-core/tests/tmux_keys.rs`
- Modify: `crates/moa-core/src/tmux/link.rs` (the `Message` arm)

**Interfaces:**
- Produces:
  ```rust
  pub fn notice(client: &ClientName, text: &str) -> Cmd;   // display-message -c <client> -d 4000 -l "moa: <text>"
  ```
- **What the core does with a `Message(text)`**, by what `Mailbox::parse` returns:
  - `Key` → `Tmux { [notice(client, action.not_yet())], None }`.
  - `Hook(_)` → a refresh.
  - `Prompt` → `Log(Info, "a prompt answer arrived that nothing waits for")`.
  - `NotMoa` → `Log(Warn, "tmux sent a message moa didn't ask for")`.
  - `Bad` → `Log(Warn, "a moa message didn't parse")`.
  - No log line carries the message text.

- [ ] **Step 1: Write the failing tests**
  - `every_key_gets_its_notice`: for each `KeyAction::ALL`, exactly one `Tmux` effect whose
    command's argv is `["-c", client, "-d", "4000", "-l", "moa: " + not_yet]`.
  - `a_menu_item_without_a_pane_still_answers`.
  - `hook_messages_refresh`.
  - `messages_moa_did_not_send_are_logged_without_their_text`: a message containing
    `SECRET-7` → a log line that doesn't contain it.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: keys and menu items answer with a notice; hook messages refresh"`
- [ ] **Step 6: Open PR c2** (Tasks 5–8) with `/code-review medium --fix`.

---

### Task 9: The generated config and the tmux paths

**Files:**
- Create: `crates/moa-tmux/src/conf.rs`, `crates/moa-tmux/tests/conf.rs` (+ `snapshots/`)
- Modify:
  - `crates/moa-platform/src/paths.rs`: `TmuxSocket`, `tmux_socket`, `tmux_conf`,
    `moa_tmux`;
  - `crates/moa-platform/tests/paths.rs`.

**Interfaces:**
- Produces:
  ```rust
  // moa_platform::paths
  #[derive(Clone, PartialEq, Eq, Debug)] pub enum TmuxSocket { Name(String), Path(PathBuf) }
  impl TmuxSocket {
      pub fn args(&self) -> [OsString; 2];                              // ["-L", name] | ["-S", path]
      pub fn path(&self, tmux_tmpdir: Option<&Path>, uid: u32) -> PathBuf;  // Name: <tmux_tmpdir or /tmp>/tmux-<uid>/<name>
  }
  impl Paths {
      pub fn tmux_socket(&self, b: &str) -> Result<TmuxSocket, PathsError>;
      // sandboxed: Path(run/tmux-<b>), with socket()'s length rule and fallback (<hash>-tmux-<b>);
      // real: Name("moa") for default, Name("moa-<b>") otherwise
      pub fn tmux_conf(&self, b: &str) -> PathBuf;   // brain_dir(b)/tmux.conf
      pub fn moa_tmux(&self, b: &str) -> PathBuf;    // brain_dir(b)/moa.tmux
  }
  // moa_tmux::conf
  pub fn moa_tmux(brain: &str) -> String;
  pub fn tmux_conf(moa_tmux: &Path) -> String;       // "source-file -q ~/.tmux.conf\nsource-file <quoted path>\n"
  pub fn write_if_changed(path: &Path, text: &str) -> io::Result<bool>;   // temp file + rename, 0600; false when equal
  pub fn hooks(ctl: &ClientName) -> Vec<Cmd>;        // T7, index 90
  pub fn unhooks() -> Vec<Cmd>;                      // set-hook -gu '<name>[90]' for each
  ```
- **`moa_tmux(brain)`** ports `spike/moa-proto/tmux.conf` and `menu.tmux` section by
  section, with these changes only:
  - no `source-file ~/.tmux.conf` (it's in `tmux.conf`, T6);
  - no `set-hook` lines: the spike's `@moa_wake` hooks go, and moa's hooks are set at
    connect (T7);
  - `status-interval 0`, and status-right's `%H:%M` becomes `#{@moa_clock}`;
  - `set-titles-string "moa · #W"` for `default`, `"moa <b> · #W"` otherwise;
  - `terminal-features[20..24]`: the spike's ghostty line, then `xterm-kitty:RGB`,
    `alacritty:RGB`, `wezterm:RGB`, `foot*:RGB`;
  - **daemon keys** (each `KeyAction`) bind to
    `if -F '#{@moa_ctl}' { run -C "display-message -c '#{@moa_ctl}' 'moa:key <word> ##{pane_id} ##{session_id} ##{client_name}'" } { display-message -d 4000 "moa isn't running: run moa" }`;
  - **the menu** is inlined: the spike's items, with every daemon item using the guarded
    command with `-` as the pane. It's bound to `moa Space`, `moa C-a` and the prefix's
    `Space`; prefix `C` is the guarded `repair`;
  - **the pure keys** are kept verbatim: arrows (`-r`) and `h j k l`, `z`, `[ ]`, `a`,
    `\|`, `-`, `m`, prefix `m`, and `repeat-time 700`.
- **`hooks(ctl)`** sets these at index 90:
  - `pane-exited`, `pane-died`: `display-message -c <ctl> 'moa:hook <name> #{hook_pane}'`;
  - `window-layout-changed`: the same with `layout #{hook_window}`;
  - `client-attached`: the same with `client-attached #{hook_client}`;
  - `client-detached`: `if -F '#{==:#{hook_client},#{@moa_ctl}}' { set-option -gu @moa_ctl }`.

- [ ] **Step 1: Write the failing tests**
  - `the_generated_config_is_stable`: insta snapshots of `moa_tmux("default")`,
    `moa_tmux("work")` and `tmux_conf(…)`, reviewed line by line against the spike files.
  - `it_sources_cleanly_on_this_tmux`:
    - a `Sandbox` runs `source-file` on the written `moa.tmux` → exit 0, no output;
    - `show-options -gv status-interval` == `0`;
    - `list-keys -T moa` names every `KeyAction::key()` and every pure key.
    - (CI runs it on 3.4 and on 3.8.)
  - `every_daemon_key_reaches_the_mailbox`:
    - a `Sandbox` with the config, and a `ControlClient` with `@moa_ctl` set;
    - a `Terminal` attached; press `C-a <key>` for each action;
    - each gives a `Message` that parses to `Key{action, Some(pane), …}`;
    - `C-a Space`, then `c` → `Key{Console, None, …}`.
  - `keys_say_moa_isnt_running_without_a_ctl`: `@moa_ctl` unset, press `C-a c` → the
    terminal shows `moa isn't running: run moa`.
  - `the_ctl_option_is_cleared_when_the_ctl_dies`: `hooks` installed, then `kill -9` of
    `child_pid()` → `show-options -gv @moa_ctl` is empty within 2 s.
  - `user_hooks_at_other_indexes_survive`: a user `pane-exited[5]`; `hooks`, then
    `unhooks` → `[5]` is still there, `[90]` is gone.
  - `every_hook_reaches_the_mailbox`, the literal-name form for each hook:
    - a window whose command exits → `hook pane-exited`;
    - the same with `remain-on-exit` → `hook pane-died`;
    - a split → `hook layout`;
    - attaching a `Terminal` → `hook client-attached`.
    - (The appendix verified `client-attached` only through `run -C`; if the literal form
      fails, use `run -C` for that hook and note it.)
  - Paths:
    - `tmux_socket_is_inside_the_root_when_sandboxed`;
    - `real_brains_keep_the_spike_names` (`moa`, `moa-work`);
    - `a_long_sandbox_socket_falls_back`;
    - `name_sockets_follow_tmux_tmpdir`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**
- [ ] **Step 5: Commit** `git commit -m "v1: generated tmux config (status line, key table, menu), hooks, tmux paths"`

---

### Task 10: The daemon's tmux actor

**Files:**
- Create: `crates/moa-daemon/src/tmux.rs`, `crates/moa-daemon/tests/tmux.rs`
- Modify:
  - `crates/moa-daemon/src/{lib.rs, run.rs, core_task/mod.rs, core_task/drain.rs}`;
  - `crates/moa-daemon/Cargo.toml` (`moa-tmux`);
  - `crates/moa-client/src/lib.rs` (`daemon_env` + `TMUX_TMPDIR`, T20);
  - `crates/moa-testkit/src/temp_brain.rs`:
    - `pub fn tmux(&self, brain: &str, args: &[&str]) -> io::Result<String>`, run like
      `Sandbox::tmux`: `env_clear`, then `-S <tmux_socket(brain)>`, with `PATH`, `HOME`
      (the root), `TERM` and `LANG`. It's the only way tests reach a TempBrain's server
      (T23).
    - `env()` gains `TERM=xterm-256color`.
    - On drop, `tmux(b, ["kill-server"])` for every brain whose socket exists.

**Interfaces:**
- Produces:
  ```rust
  pub struct TmuxLink { pub socket: TmuxSocket, pub moa_tmux: PathBuf, pub env: Vec<(OsString, OsString)> }
  // CoreOptions gains `pub tmux: Option<TmuxLink>`; None → tmux effects are dropped (the existing core tests)
  pub fn tmux_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)>;  // T3's list
  impl CoreHandle { pub fn tmux_input(&self, input: TmuxIn); }
  pub(crate) enum TmuxCmd { Connect { tmux: Option<PathBuf> }, Send { conn: u64, cmds: Vec<Cmd>, token: Option<TmuxToken> }, Stop(oneshot::Sender<()>) }
  pub(crate) fn start(link: TmuxLink, to_core: impl Fn(TmuxIn) + Send + Sync + 'static,
                      failed: impl Fn(&'static str, String) + Send + 'static) -> mpsc::UnboundedSender<TmuxCmd>;
  ```
- **The actor:**
  - Runs under `supervise` as `"the terminal link"`. A restart sends `Down(Lost)` and
    reconnects.
  - **`Connect`** while connected or in backoff: report the current state again (`Connected`
    with the same `conn`, or nothing while in backoff), and never a second control client.
    Otherwise:
    1. Find tmux: the given path, else the remembered one, else `which("tmux", PATH)`.
       None → `Down(NoTmux)`.
    2. Probe `socket.path(TMUX_TMPDIR, uid)` with a `UnixStream` connect (T4). Not found or
       refused → `Down(NoServer)`.
    3. `ControlClient::connect`:
       - `TooOld(v)` → `Down(TooOld(v))`;
       - `Spawn` → forget the remembered path, then `Down(NoTmux)`;
       - `Exited` → the backoff below.
    4. `conn += 1`.
    5. `run(source-file <moa_tmux>)`, logging a failure's first line.
    6. `run(set-option -g @moa_ctl <ctl>)`, then `send(hooks(ctl))`.
    7. `Connected { conn, … }`.
    `Gone` from any `run` in steps 5–6 takes the `Lost` path below.
  - **Events:**
    - `Notification` → `TmuxIn::Notification`, except `Exit`;
    - `Replies{tag}` → `TmuxIn::Replies` with its `conn` and the tag's token, each
      `CmdError` turned into its line (`to_string()`);
    - the channel ends → `Down(Lost)`, then:
      - probe; none → `Down(NoServer)`;
      - else up to 6 connects at 0.1·2^k s;
      - then `Connected` or `Down(GaveUp)`.
  - **`Send`** for another `conn`, or while disconnected, is dropped, with a debug log.
  - **`Stop`:**
    1. `run` each of `unhooks()`, then `run(set-option -gu @moa_ctl)`;
    2. `close()`;
    3. answer.
- **The core task:**
  - An untokened `Effect::Tmux` is held (`Held::Tmux`) and released to the actor after the
    commit, in order. `fail_held` drops it.
  - `TmuxConnect` and tokened `Tmux` go to the actor at once (T10).
  - With no `TmuxLink`, `TmuxConnect` is answered at once with `Down(Disabled)`, through
    the inbox.
  - At the end of `Core::run`, after `wait_for_backups`, it sends `Stop` and waits up to 2 s.
- **`run.rs`:**
  - In `bring_up`: when `paths.is_sandboxed()`, `write_if_changed` both config files and
    build the `TmuxLink`.
  - Otherwise, log `the terminal link waits for the switch-over` with no link (T21).

- [ ] **Step 1: Write the failing tests** (`tests/tmux.rs`)
  - The setup: an in-process `run` on a `/tmp` root, with a `TypicalBrain` DB and
    `Ready::Channel`.
  - The server, when a test wants one, is created by the test exactly as T1's command does,
    on `paths.tmux_socket("default")`.
  - `without_a_server_ensure_says_not_running_and_launches_nothing`: `tmux.ensure` →
    `{running: false}`, and `status.launches` is unchanged.
  - `the_daemon_never_starts_a_server`: after start and an ensure, no socket file exists.
  - `the_daemon_adopts_a_running_server`:
    - create the server, then ensure → `{running: true}`;
    - `show-hooks -g` lists the five `[90]` hooks;
    - `@moa_ctl` equals the status's control client;
    - `@moa_status` equals `counts_text` for the Typical brain.
  - `shutdown_removes_hooks_and_the_ctl_option`.
  - `the_daemon_reconnects_after_its_client_is_detached`:
    - `detach-client -s _moa` → `status` is connected again within 2 s;
    - `tmux_restarts == 0`, and launches went up by 1.
  - `real_brains_stay_off_tmux`:
    - `Paths::from_vars(None, home, …, real_home_ok: true)`;
    - → `status.tmux` is `None`, no launch, and the log line is there;
    - `tmux.ensure` answers the not-switched-over line.
  - `a_failed_commit_keeps_the_link_working` (the ordering itself is pinned by Task 5's
    `reload_keeps_the_link_and_restarts_its_bookkeeping`):
    - adopt a server, set `@moa_status` to `junk` with `TempBrain::tmux`, and queue a
      `Fault` failing the next commit;
    - a `session.archive` → its error line;
    - then, within 2 s, `@moa_status` is `counts_text` again (the reload rewrote it) and a
      `tmux.ensure` answers `{running: true}`.
  - `a_second_connect_never_starts_a_second_client`: two `tmux.ensure` while the actor backs
    off → launches go up by at most the backoff's count, and one control client in
    `list-clients`.
  - `daemon_env_passes_tmux_tmpdir` (in `moa-client`).
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**, three times.
- [ ] **Step 5: Commit** `git commit -m "v1: the daemon's terminal link: probe, connect, bootstrap, reconnect, clean stop"`
- [ ] **Step 6: Open PR c3** (Tasks 9–10) with `/code-review medium --fix`.

---

### Task 11: The front door: `moa [BRAIN]` and `moa --sandbox`

**Files:**
- Create: `crates/moa/src/front.rs`, `crates/moa/tests/front.rs`
- Modify:
  - `crates/moa/src/main.rs`: the `Cli` gains `brain: Option<String>` and
    `#[arg(long)] sandbox: bool`, with `args_conflicts_with_subcommands`. The front door
    replaces the `None` arm's "moa v1 is still being built" line, and the test in
    `crates/moa/tests/cli.rs` that expects it becomes `moa_without_the_sandbox_refuses`;
  - `crates/moa/src/ctl.rs`: `moa status` prints `terminal: connected` or
    `terminal: not connected`. The tmux version and server pid are in `--json` only (no tmux
    words);
  - `crates/moa-core/src/config.rs`: `RESERVED_BRAIN_NAMES`, and a refusal for them;
  - `crates/moa-proto/tests/cli.rs`;
  - `crates/moa-platform/src/paths.rs`: `sandbox_root`.

**Interfaces:**
- Produces:
  ```rust
  pub struct FrontArgs { pub brain: Option<String>, pub sandbox: bool }
  pub fn front(args: &FrontArgs) -> ExitCode;
  pub fn front_env(parent: impl Iterator<Item = (OsString, OsString)>, paths: &Paths) -> Vec<(OsString, OsString)>;  // T1
  #[derive(PartialEq, Eq, Debug)] pub enum Nesting { Outside, Ours, Other }
  pub fn nesting(tmux_var: Option<&OsStr>, ours: &Path) -> Nesting;   // $TMUX's socket (before the first ',') vs ours, both canonical
  pub fn create_argv(socket: &TmuxSocket, conf: &Path, home: &Path) -> Vec<OsString>;  // T1's command; `home` is the
                                                                                      // user's $HOME (the shell's folder), not the root
  // moa_core::config
  pub const RESERVED_BRAIN_NAMES: &[&str];   // the moa subcommands + every moactl verb
  // moa_platform::paths
  pub fn sandbox_root(home: &Path) -> PathBuf;   // <home>/.local/state/moa-sandbox
  ```
- **`front`, in order:**
  1. `--sandbox` → `Paths::from_vars(Some(sandbox_root(HOME)), …)`, after `ensure_private_dir`
     on the root.
  2. Not sandboxed → `moa v1 isn't switched over yet; try moa --sandbox`, exit 1 (T21).
  3. The brain (default `default`) must parse and be in the config.
  4. `which("tmux", PATH)`, or else `moa needs tmux 3.4 or newer; install it and run moa again`,
     exit 1.
  5. `nesting`:
     - `Other` → print `the <b> moa is running; open it from a new terminal tab: moa<space+b>`
       (no space for `default`), exit 0.
  6. Connect with autostart (`OnMismatch::Restart`), then
     `call(Op::TmuxEnsure { tmux: Some(path) })`. Each call waits at most 10 s, or else
     `moa didn't answer within 10 s; see its log (<log path>)`, exit 1.
  7. `running: false`:
     - spawn `create_argv` through `Spawner`, with `env_clear().envs(front_env(…))`, and
       wait for it;
     - a non-zero exit with no server afterwards →
       `moa couldn't start its terminal server: <first stderr line>`, exit 1;
     - then ensure again, which must say `running: true`.
  8. `exec` `tmux <socket args> attach -t =moa`, or `switch-client -t =moa` when `Ours`,
     with `front_env`.
- **Config:** a brain in `RESERVED_BRAIN_NAMES` →
  `moa.toml: a brain can't be called <name>; moa uses that word for a command`.

- [ ] **Step 1: Write the failing tests**
  - Unit tests:
    - `front_env_drops_agent_and_moa_variables`: `MOA_SESSION`, `MOA_BRAIN`, `CLAUDECODE`,
      `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CONFIG_DIR`, `TMUX` and `TMUX_PANE` are gone;
      `PATH`, `HOME` and `SSH_AUTH_SOCK` stay; `MOA_ROOT` is set when sandboxed.
    - `nesting_compares_socket_paths`:
      - canonicalise each path's parent folder, because the socket may not exist yet;
      - the same path is `Ours`, another path is `Other`, and no `TMUX` is `Outside`;
      - on macOS only (`#[cfg(target_os = "macos")]`), `/tmp/x` vs `/private/tmp/x` is
        `Ours`.
    - `create_argv_is_t1s_command`.
  - `every_verb_and_subcommand_is_reserved`: a proto test over `VERB_NAMES`, and a `moa`
    test over clap's subcommand names.
  - `a_brain_named_like_a_verb_is_refused`: a config unit test.
  - Real tmux, with `TempBrain` + `Terminal` running the `moa` binary
    (`env!("CARGO_BIN_EXE_moa")`, `TempBrain::env()`). Every tmux query goes through
    `TempBrain::tmux` (T23).
    - `moa_starts_the_server_and_attaches`:
      - the terminal's env also carries `MOA_SESSION=x`, `CLAUDECODE=1` and
        `CLAUDE_CONFIG_DIR=/x`, so the check below can fail;
      - the terminal shows the status line within 5 s;
      - `list-clients` has one user client on `moa`;
      - `show-environment -g` has `MOA_ROOT` and none of those three, nor `TMUX`;
      - `status.tmux` is `Some`.
    - `a_second_moa_attaches_to_the_same_server`: two terminals, one server pid.
    - `moa_inside_its_own_server_switches_instead_of_nesting`:
      - type `<moa path>\r` into the cockpit shell;
      - → still exactly one user client, and no `tmux` client inside a pane (`list-clients`
        count unchanged).
    - `moa_inside_another_tmux_refuses`: `assert_cmd` with
      `TMUX=/tmp/elsewhere/default,1,0` → stdout is the spike's line, exit 0, and no socket
      created.
    - `moa_without_the_sandbox_refuses`: no `MOA_ROOT`, with and without
      `MOA_REAL_HOME=1` → exit 1 with the line, and no socket and no daemon.
    - `moa_names_a_missing_brain`.
    - `status_says_whether_the_terminal_is_connected`.
- [ ] **Step 2: Run them and see them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run them and see them pass**, three times.
- [ ] **Step 5: Commit** `git commit -m "v1: front door: moa [BRAIN] and moa --sandbox create the server, connect, attach"`

---

### Task 12: End to end, and the try-it

**Files:**
- Create: `crates/moa/tests/tmux_e2e.rs`

All tests use `TempBrain` with the Typical brain adopted, and a `Terminal` running `moa`.

- [ ] **Step 1: Write the tests** (they should pass with Tasks 1–11 in place; any that
  doesn't is a bug in the task that owns it)
  - `keys_and_menu_reach_the_daemon_with_zero_launches` (done-when 1):
    1. Record `launches` from `moa status --json` once the status line shows.
    2. Press each daemon key; each notice appears (`wait_for_text`, 2 s each).
    3. `C-a Space`, then `t` → the tree's notice.
    4. Pure keys:
       - with a second window made by the test, `C-a ]` and `C-a [` change `#{window_index}`;
       - `C-a |` in the cockpit → `the cockpit is a layout`.
    5. `launches` is unchanged.
  - `the_status_line_follows_the_brain`:
    - `@moa_status` equals `counts_text` for the Typical brain;
    - `moactl archive <a live idle session>` → `@moa_status`'s idle count drops by one
      within 2 s.
  - `the_clock_shows_while_attached`: `@moa_clock` matches `^\d\d:\d\d$`.
  - `kill_server_then_moa_gives_a_new_generation` (done-when 2):
    - `kill-server` → status `terminal: not connected` within 2 s;
    - a new `moa` terminal → connected, with a new pid and `tmux_restarts == 1`.
  - `keys_say_moa_isnt_running_when_the_daemon_is_gone`:
    - `moa shutdown` → `C-a c` shows `moa isn't running: run moa`;
    - `moa` again → `C-a c` shows the console notice;
    - then `kill -9` of the daemon's pid → `C-a c` shows the not-running line again.
  - `a_user_tmux_conf_with_run_shell_and_hooks` (Review Focus 5):
    - `<root>/.tmux.conf` has `set -g @user_opt 1`, `run-shell 'sleep 1'` and a
      `pane-exited[5]` hook;
    - `moa` comes up and `C-a c` answers within 2 s;
    - `@user_opt` is 1, `[5]` and `[90]` are both there;
    - after `moa shutdown`, `[5]` stays and `[90]` is gone.
- [ ] **Step 2: Run the whole verify**:
  - `cargo fmt --check`;
  - clippy `-D warnings`, including `--target x86_64-unknown-linux-gnu -p moa-platform`;
  - `cargo nextest run --workspace` three times;
  - `nix flake check` once.
- [ ] **Step 3: Commit** `git commit -m "v1: slice 3 end to end: keys, menu, status, generations, daemon down"`
- [ ] **Step 4: Open PR c4** (Tasks 11–12) with `/code-review medium --fix`. The PR body ends
  with the try-it and `Restart needed: nothing; the spike is untouched (v1 never contacts
  the moa or moa-work tmux servers, T21)`.

**Done when:**
- In a sandbox, every moa key and menu item reaches the daemon and gets its notice. The
  pure-tmux keys work. The status line shows the brain's counts and follows a change. Over
  all of that the daemon launches nothing (`keys_and_menu_reach_the_daemon_with_zero_launches`).
- `kill-server` followed by `moa` gives a new generation, and `moa status` shows it
  (`kill_server_then_moa_gives_a_new_generation`).
- With the daemon stopped or killed, keys say `moa isn't running: run moa`; once it's back,
  they work.

**Try it** (in your own terminal, from this checkout; v1 isn't on `PATH`):
```
nix run . -- --sandbox
```
- **What you should see:** a throwaway, empty moa opens in the terminal.
  - Ctrl-a lists the keys on the status line.
  - Ctrl-a c says `moa: the console isn't in this build yet`.
  - Ctrl-a Space opens the menu.
  - The clock ticks each minute.
- **To leave:** close the tab. Running `nix run . -- --sandbox` again re-attaches.
- **To see real counts in the status line:**
  1. Stop the sandbox's moa.
  2. Delete the sandbox's empty brain. `adopt-spike` never overwrites one, and the first
     run created it.
  3. Load a copy of your private brain into it.
  4. Open it again.

  (These use `$HOME`, not `~`, so they work in fish too.)
  ```
  MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- shutdown
  rm -f $HOME/.local/state/moa-sandbox/.local/share/moa/default/moa.db*
  MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- adopt-spike --from <copy> --into $HOME/.local/state/moa-sandbox/.local/share/moa/default/moa.db
  nix run . -- --sandbox
  ```
- **To remove it all:**
  ```
  MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- shutdown
  tmux -S $HOME/.local/state/moa-sandbox/.local/state/moa/run/tmux-default kill-server
  rm -rf $HOME/.local/state/moa-sandbox
  ```

Restart needed: nothing; the spike is untouched.

---

## Appendix: what was checked on tmux 3.4 and 3.8 (2026-11-16)

These runs used throwaway servers (`-L moaplan-<pid>` under a temp `TMUX_TMPDIR`), never the
user's. Each line holds on both versions unless marked.

- `tmux -N … new-session` with no server: `error connecting to …`, exit 1, and no server is
  started. `tmux -N … -C new-session -A -s _moa …` with no server: `no server running on …`.
- `-C new-session -A -s _moa -f no-output,ignore-size,no-detach-on-destroy` creates `_moa`
  when it's missing and attaches when it's there. `client_flags` show
  `control-mode,ignore-size,no-detach-on-destroy,no-output`, and no `%output` ever arrived.
- A key bound to `run -C "display-message -c '#{@moa_ctl}' 'moa:key x ##{pane_id} ##{session_id} ##{client_name}'"`
  gives `%message moa:key x %0 $0 /dev/ttys038`.
  - With `display-message -l`, the `##{…}` stay literal (so keys don't use `-l`).
  - Without `run -C`, `-c '#{@moa_ctl}'` sends nothing.
- In a `display-menu` item, `#{pane_id}` expands to empty, while `#{session_id}` and
  `#{client_name}` work.
- Hooks with the literal control-client name deliver `moa:hook pane-exited %2`,
  `moa:hook pane-died %3` (with `remain-on-exit`) and `moa:hook alert-bell @4`.
  - `window-layout-changed` fires on split, zoom and unzoom.
  - `client-attached` via `run -C` works.
  - A `client-detached[90]` hook with `if -F '#{==:#{hook_client},#{@moa_ctl}}' { set -gu @moa_ctl }`
    clears it after `kill -9` of the control client.
- `set -g @moa_status …` redraws the status line on its own. `display-message -c <tty> -d 4000 '…'`
  from the control client shows on the user's terminal.
- `command-prompt -b -t <tty> … "display-message -c <ctl> -l \"moa:prompt 7 %%%\""` returns
  `it's "a" #{x} ; b \ …` intact.
  - With single quotes around `%%%`, `'` is lost or the message never comes.
  - Escape sends nothing.
  - A Ctrl-V-quoted control character arrives octal-escaped (`\026`) on 3.4 and as `_` on
    3.8.
- Focus notifications: `select-window` → `%session-window-changed $0 @2`; `select-pane` →
  `%window-pane-changed @2 %3`; killing the active pane → `%window-pane-changed @2 %2`.
  A new window in another session → `%unlinked-window-add`. A detach →
  `%client-detached /dev/ttys038`. `kill-server` → `%exit`, then EOF, with exit 0.
- Quoting: single-quoted pieces (`'it'\''s'`, `'a'"\n"'b'`) round-trip 29 of 29 hard
  strings on 3.8 and 28 of 29 on 3.4.
  - Double quotes expand a leading `~`.
  - On 3.4 only, a `$` followed by an ASCII letter, `_`, `{` or a non-ASCII character is
    stored as `\$`, however it's quoted (T17).
- `tmux attach` on a plain pty (not the controlling terminal) works, with
  `client_utf8 1` under `-u` and `LANG=C.UTF-8`. Keys typed into the pty reach the bindings.

Transcript (3.4), trimmed, for the `V3_4` constant in `tests/parse.rs`:
- The `P` lines are rewritten in `LIST_PANES`'s field order; the experiment printed
  `pane session window …`. Their empty last field (`@moa_session` unset) is a trailing
  space in real output.
- The 3.8 one is the same, except that the initial `%window-add @1` is absent and a
  `%window-renamed @1 tmux` follows `%session-changed`.
```
%begin 1794852795 275 0
%end 1794852795 275 0
%window-add @1
%sessions-changed
%session-changed $1 _moa
%begin 1794852795 281 1
V 3.4 14229 1794852795 client-14231
%end 1794852795 281 1
%client-session-changed /dev/ttys038 $0 moa
%message moa:key g %0 $0 /dev/ttys038
%begin 1794852799 332 1
P %1 @1 $1 1 1 0
P %0 @0 $0 1 1 0
P %2 @2 $0 0 1 1
P %3 @2 $0 1 1 1
%end 1794852799 332 1
%window-pane-changed @2 %2
%begin 1794852800 339 1
after-1
%end 1794852800 339 1
%unlinked-window-add @3
%unlinked-window-renamed @3 w3
%exit
```
