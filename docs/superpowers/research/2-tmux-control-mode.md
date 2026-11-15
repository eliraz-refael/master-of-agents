# 2. tmux control mode as moa's only link to tmux

Research for the moa daemon (Rust). Goal: talk to tmux with **no polling and no
process per action**. Installed here: `tmux 3.8` (nix). Sources used:

- `man tmux` (3.8): CONTROL MODE, HOOKS, `refresh-client`, `attach-session`.
- tmux source at tag `3.8` (`control.c`, `control-notify.c`, `monitor.c`,
  `cmd-queue.c`, `cmd-display-message.c`, `cmd-wait-for.c`, `hooks.c`,
  `cmd-find.c`), plus `git show <tag>:file` to compare 3.2 → 3.7c, and `CHANGES`.
- tmux wiki: `Control-Mode.md`, `Events.md` (3.8 events/monitors).
- iTerm2 `sources/tmux/TmuxGateway.m`, `TmuxController.m` (the main control-mode client).
- Rust crates: `tmux_interface`, `libtmux` (libtmux-rs) and its `docs/findings.md`,
  `tmuxctl`, `par-term-tmux`.
- **16 experiments** run against a private server (`tmux -L moaresearch -f /dev/null`,
  never the user's server). The harness and scripts are in
  `scratchpad/exp/` (`ctl.py`, `e1.py` … `e16.py`). Results marked **[verified 3.8]**
  were seen on this machine. Results marked **[source]** come from reading the code.

---

## 0. TL;DR recommendation

1. **One long-lived `tmux -C` child per tmux server**, started once when the daemon
   starts or reconnects. It's the only process moa ever launches for tmux. Its stdin and
   stdout are pipes, so use a single `-C` (`-CC` is for terminals: it sends a DCS
   prefix and uses the tty fd).
   ```
   tmux -L <sock> -C attach-session -f no-output,ignore-size,no-detach-on-destroy -t '=_moa'
   ```
   `_moa` is a small, moa-owned session that holds one placeholder pane. The client
   doesn't sit in the user's session (see §4 for why).
2. **Commands:** one tmux command per line, pipelined, matched in FIFO order against
   `%begin … %end/%error` blocks whose **flags field is 1**. Skip blocks with flags 0.
   Multi-step "transactions" go on one `;`-joined line followed by a sentinel line (§5).
   Measured: **0.06 ms** per round trip and **2.7 ms** for a 27-command layout switch.
   The same switch as 27 processes takes **271 ms** (§5).
3. **Events:** use the built-in notifications. `%window-pane-changed`,
   `%session-window-changed`, `%client-session-changed`, `%sessions-changed` and
   `%unlinked-window-*` are server-wide. Use a **hook mailbox** for everything else:
   `set-hook -g 'pane-died[90]' "display-message -c <ctl> 'moa:pane-died #{hook_pane}'"`
   makes the daemon receive `%message moa:pane-died %5` immediately, with no process.
   Optionally add one 1 Hz whole-server format subscription as a reconciliation net
   (§2). It runs inside tmux and costs about 1 ms of server time per second at 41 panes.
4. **Keys:** bind keys to `display-message -c <ctl-client> 'moa:key …'`. The daemon gets
   `%message moa:key g %12 $3 /dev/ttys004` **0.8 ms** (median) after the keypress, with
   no process. You can make the bindings static (written in the config file and kept
   across daemon restarts) by routing through `run -C` and a `@moa_ctl` server option
   (§8). **[verified 3.8]**
5. **Minimum tmux 3.4.** `display-message` to a control client (`%message`) arrived
   in 3.4, and moa's event mechanism depends on it. Recommend and test on **3.8**:
   notifications are no longer injected inside `%begin/%end`, replies are bounded,
   and there are empty panes and JSON layouts (§10).

---

## 1. The protocol

### 1.1 Framing

- The client writes **one command line per `\n`**. An **empty line detaches**, so never
  send `""`. Commands are parsed by the normal tmux parser, so quoting rules apply.
- Every *command* (not every line) produces one block:
  ```
  %begin <unix-seconds> <cmd-number> <flags>
  …output lines…
  %end   <unix-seconds> <cmd-number> <flags>      (or %error … on failure)
  ```
  - `cmd-number` is a **server-global counter** (`static u_int number` in `cmdq_next`).
    It isn't per client and you can't choose it, so you can't use it as your request
    id. The `%end` always repeats `%begin`'s time and number.
  - `flags`: **1** means "this command came from this control client's stdin"
    (`CMDQ_STATE_CONTROL`). **0** means a server-originated block in this client's
    queue: the initial attach command, or an `after-*` hook triggered by your command.
    iTerm2 treats flags 0 as an "auto response" and doesn't pop its command queue for
    it. Do the same. **[verified 3.8]**
    ```
    split-window -d -t work -P -F '#{pane_id}' 'sleep 1000'
    %begin 1794736073 341 1
    %4
    %end 1794736073 341 1
    %begin 1794736073 342 0        <- after-split-window hook output, not ours
    from-after-hook
    %end 1794736073 342 0
    ```
- `a ; b` on one line gives **two blocks**. If `a` fails, **the rest of the group is
  dropped** and you get one `%error` block only **[verified 3.8]**:
  ```
  kill-pane -t %99 ; display -p after-failure
  %begin 1794736020 324 1
  can't find pane: %99
  %error 1794736020 324 1
  ```
  So you can't count blocks per `;` element. iTerm2 joins command lists with `"; "`
  and counts. That's a latent desync on error, so don't copy it.
- Parse errors are reported as a normal block:
  `%begin … / parse error: unknown command: bogus-command / %error …`.
- **Command output isn't escaped.** `display -p`, `capture-pane -p` and `show-buffer`
  lines arrive raw (tabs, backslashes and UTF-8 included). Only `%output` is escaped.
  A pane that prints a line like `%end 1794… 335 1` could fake a guard. Match the
  closing guard on the **exact `(time, number, flags)`** of the open `%begin`. Always use
  `-F` formats whose lines can't begin with `%` (for example a `P ` prefix:
  `list-panes -a -F 'P #{pane_id} …'`).
- **Notifications inside blocks:** tmux 3.8 defers notifications generated while a
  block is open until after `%end` (CHANGES 3.8, issue 5458; `guard_depth` in
  `control.c`). **Before 3.8, a notification can appear between `%begin` and `%end`.**
  The parser must accept a `%<known-notification>` line inside a block and dispatch
  it as a notification. This is unambiguous only if your command output never starts
  with `%` (see the previous point).
- `%message` (from `display-message -c <ctl>`) is written with `control_write`, not
  through the deferral path. It can only land inside one of *our* blocks if our queue
  is stalled by a waiting command (§5.4), so never send waiting commands.

### 1.2 Notifications (3.8) and what each gives moa

Scope legend: **S** = only about the control client's attached session or its
windows; **G** = server-wide.

| Line (exact format from source) | Scope | Use for moa |
|---|---|---|
| `%output %<pane> <escaped-bytes>` | S | Pane bytes. Suppressed by `no-output` (§3). |
| `%extended-output %<pane> <age-ms> … : <bytes>` | S | Replaces `%output` when `pause-after` is set. Ignore fields between age and `:`. |
| `%pause %<pane>` / `%continue %<pane>` | S | Flow control (§3). |
| `%layout-change @<win> <layout> <visible-layout> <flags>` | S (windows linked to the attached session) | Layout after any split, resize, swap or kill. With the `new-layouts` flag (3.8) the layouts are JSON (example below). |
| `%window-add @w` / `%window-close @w` / `%window-renamed @w name` | S | Window lifecycle in our session. |
| `%unlinked-window-add/close/renamed @w [name]` | G (all other sessions) | Window lifecycle elsewhere. This is how a ctl in `_moa` sees agent windows come and go. **[verified]** |
| `%window-pane-changed @w %p` | **G** | Active pane of *any* window changed (select-pane, mouse click, killed active pane). **[verified with ctl in another session]** |
| `%session-window-changed $s @w` | **G** | Current window of any session changed. **[verified]** |
| `%session-changed $s name` | self | *Our* client switched session (also sent once on attach). |
| `%client-session-changed <client> $s name` | G | *Another* client (the user's terminal) switched session. |
| `%client-detached <client>` | G | A client detached (3.2+). |
| `%sessions-changed` | G | A session was created or destroyed. No detail, so re-list (`ls -F`). |
| `%session-renamed $s name` | G | The man page says `%session-renamed name`, but the **source emits `$id name`**. |
| `%pane-mode-changed %p` | G | Copy mode, choose mode and so on entered or left. |
| `%paste-buffer-changed name` / `%paste-buffer-deleted name` | G | 3.4+ (named correctly from 3.4). Useful as a fallback mailbox (§8). |
| `%subscription-changed name $s @w idx %p … : value` | per subscription | Format subscriptions (§2). Session scope prints `$s - - - : value`. |
| `%message text` | targeted | Output of `display-message -c <this client>` (3.4+). **This is moa's event bus** (§7, §8). |
| `%config-error text` | — | Config file errors (3.4+). |
| `%exit [reason]` | — | Client is exiting (detached, session killed, "too far behind"). Reconnect. |

There is **no** `%pane-exited` or `%pane-died` notification. Pane death only shows up
indirectly (`%layout-change`, `%window-close`) unless you add hooks (§7).

Example lines **[verified 3.8]**:
```
%window-pane-changed @0 %3
%session-window-changed $0 @2
%unlinked-window-add @2
%client-session-changed /dev/ttys033 $0 work
%layout-change @0 b653,200x60,0,0[200x30,0,0,0,200x29,0,31,1] b653,200x60,0,0[200x30,0,0,0,200x29,0,31,1] *
%layout-change @0 {"V":2,"L":{"t":"h","w":120,"h":40,"x":0,"y":0,"c":[{"t":"p","w":60,"h":40,"x":0,"y":0,"a":true,"i":0,"I":"%0"},{"t":"p","w":59,"h":40,"x":61,"y":0,"i":1,"I":"%2"}]}} …
%extended-output %0 0 : héllo ✓ \134 back\011tab
%message moa:key x pane=%1 sess=work client=/dev/ttys045
```

---

## 2. `refresh-client -B` format subscriptions

```
refresh-client -B 'name:what:format'     # add or replace
refresh-client -B 'name'                 # remove
```
`what` can be empty (the attached session), `%N`, `%*` (all panes **in the attached
session**), `@N`, or `@*` (all windows in the attached session).

**Semantics [source `monitor.c` + verified]:**
- These are **polled inside the tmux server**: one libevent timer per client, every 1 s.
  Each tick expands every subscribed format and sends `%subscription-changed` only
  when a value differs from the last one. The first value is always sent on the
  first tick (`MONITOR_NOTIFY_INITIAL`). Re-adding a name resets it.
- Latency is **0–1 s**. Nothing is pushed on change. A killed pane showed up about
  0.6 s after the kill, on the next tick. **[verified]**
- Panes that disappear are swept silently, with **no removal notification**.
- **`%N` stops reporting once the pane is dead.** `monitor_check_pane` returns early
  when `wp->fd == -1`, so `remain-on-exit` panes never report `pane_dead=1` through a
  single-pane subscription. `%*` does report it. **[verified]**
- Scope trap: `%N`, `%*`, `@N` and `@*` are limited to the client's attached session
  (`if (wl->session != s) continue`). A ctl in `_moa` sees nothing through them.
- **Cross-session trick:** a *session-scope* subscription whose format loops over the
  whole server **[verified]**:
  ```
  refresh-client -B 'all::#{S:#{W:#{P:#{pane_id}=#{pane_dead}:#{pane_current_command}:#{@moa_session} }}}'
  %subscription-changed all $1 - - - : %0=0:coreutils: %1=1:: %3=0:sleep:abc …
  ```
  One string covers every pane. The daemon diffs it. At 41 panes the same format
  costs about 1 ms per evaluation (measured via `display -p` round trip, so ≈1 ms of
  server CPU per second).

**Can it replace polling?** It replaces *moa's* polling: no processes, no command
traffic, and tmux does the work in-process. It doesn't give instant events. Use it as:
- the source for things with no event at all: `pane_current_command`, `pane_title`,
  `pane_current_path`, and 3.8's `pane_last_output_time` and `pane_output_generation`;
- a reconciliation and safety net behind the instant hook mailbox (§7).

`pane_dead`, `window_active` and pane focus all have instant paths (hooks and
notifications), so prefer those.

3.8 also adds **server-side monitors**: `set-hook -B 'name:what:format' [-T]`. They
fire a hook when a format changes or becomes true, at the same 1 Hz, and they don't
need a control client. They're 3.8-only and not needed.

---

## 3. Output flow control: not streaming every agent

Facts **[source `control.c` + verified]**:
- `%output` is only produced for panes in windows **linked to the control client's
  attached session** (`control_write_output`:
  `winlink_find_by_window(&c->session->windows, …)`).
- `no-output` client flag: no `%output` at all. It **overrides** `-A %N:on`
  (`control_pane_offset` returns NULL for `CLIENT_CONTROL_NOOUTPUT`). Verified that
  `-A %1:on` after `no-output` delivers nothing.
- `refresh-client -A '%N:off|on|pause|continue'` works per pane, but **new panes stream
  by default**. A pane created after you turned the others off sent
  `%extended-output %2 0 : new-pane-output` immediately **[verified]**, so a
  per-pane allow-list always races.
- **Crash on older versions:** `-A %N:off` on a pane with queued output **kills the
  tmux server on 3.2a–3.6b**. 3.7 fixed it (libtmux-rs `findings.md`, "Muting a
  control-mode pane kills the server before tmux 3.7", reproduced on every run by a
  flood fixture). If you ever mute below 3.7, use `pause` instead of `off`.
- `pause-after=<s>`: when a pane falls `s` seconds behind, it's paused (`%pause %N`)
  and its queued data **is discarded, not buffered**. After
  `refresh-client -A '%N:continue'` (`%continue %N`), output resumes from *now*.
  Verified: text typed while paused never arrived, so use `capture-pane` to resync.
  Output lines become `%extended-output %N <age-ms> : …`.
- Without `pause-after`, a client more than **300 s** behind on output is disconnected
  ("too far behind", `CONTROL_MAXIMUM_AGE`). 3.8 also disconnects a client with more
  than **64 MiB** of unread replies.

**Recommendation for moa:**
- The main ctl attaches with **`-f no-output,ignore-size,no-detach-on-destroy`** to
  `_moa`. That's two independent guarantees of zero output traffic.
- For snapshots, run `capture-pane -p -e -t %N -S -<n>` on demand over the same
  channel. It's a single command and no process. Remember the output is raw text (§1.1).
- For agent "is it busy or idle" state without content, there are three options, all
  event-driven: `monitor-silence <s>` (window option) plus the `alert-silence` hook
  routed to the mailbox; the 3.8 `pane-activity` hook (fires per output burst, so it's
  too chatty to forward); or `pane_last_output_time` in the 1 Hz subscription. The
  agent's own hooks reporting to the daemon are likely better than any screen signal.
- If live streaming of one pane is ever needed (a "watch" view), use a **second,
  on-demand control client without `no-output`**, attached to a throwaway session
  that links only that agent's window (`link-window`). Output is filtered per
  session, so only that window's panes stream. Use `pause-after` for back-pressure.
  Avoid `pipe-pane`, because it spawns a process.

---

## 4. Adopting a live server; coexisting with the user's terminal

**Yes, a control client can attach to an existing server and session with running
panes.** It's an ordinary client. Attaching starts, kills and resizes nothing.
**[verified]**: `tmux -C attach -t work` on a server with running `sh` and `sleep`
panes returned `%begin … 0 / %end … 0 / %session-changed $0 work`, and the panes kept
running. `list-panes -a` over the channel showed their pids. Killing the `tmux -C`
process (SIGKILL) just removes the client. It's gone from `list-clients` within 50 ms,
and every pane survives.

**Sizing [verified 3.8]**, with the user's terminal client at 100x30 and
`window-size latest`:
- A control client attached to the **same** session without `refresh-client -C` doesn't
  affect window size (stayed 100x29). This is documented: "control mode clients do not
  affect the size of other clients" until `-C`.
- After `refresh-client -C 200x60` the window grew to 200x60. After `-C 40x10` it
  **shrank the user's window to 40x10**.
- After `refresh-client -f ignore-size` the window snapped back to 100x29.
- **Rule:** always attach with `ignore-size` and never send `refresh-client -C`. With
  that, `window-size` and `aggressive-resize` are irrelevant to the ctl.

**Other interference to know about:**
- Connecting fires the user's `client-attached` and `client-session-changed` hooks.
  The current prototype's config runs `run-shell -b 'touch …'` on those, so that's
  one process per daemon connect. Harmless, but the new design should remove those
  hooks.
- The ctl shows up in `list-clients`, `choose-client` and `#{session_attached}` (2 if
  it shares a session). moa's own UI should filter with `#{client_control_mode}`. A
  user's `detach-client -a` detaches the daemon, so reconnect on `%exit` or EOF.
- **Message routing:** client-less contexts (hooks, `display-message` without `-c`)
  pick the "best client" of the most recently active session (`cmd_find_current_client`,
  `cmd_find_best_client`). Right after `_moa` is created, before the user has typed
  anything, that can be the ctl, and a user hook's `display-message` then went to the
  daemon as `%message` **[verified]**. Once the user has typed, their client wins.
  Daemon command traffic didn't steal it (4 combinations tested). It's minor, but the
  daemon should log unexpected `%message` lines rather than drop them silently.
- Why a **dedicated `_moa` session** rather than the user's session: it keeps the ctl
  out of the user's best-client and attached-count calculations and out of their
  session's `%output`. The cost is that `%layout-change` and `%*`/`@*` subscriptions
  only cover `_moa`. Get the same information from the server-wide notifications, the
  `window-layout-changed` hook to the mailbox, and the session-scope `#{S:…}`
  subscription. If moa owns the console session the user views, attaching the ctl
  there (still `no-output,ignore-size`) is also acceptable and gives native
  `%layout-change` for it.
- `kill-session` on the session the ctl sits in sends `%exit` and kills the client
  (default `detach-on-destroy`) **[verified]**. The `no-detach-on-destroy` flag (3.2+)
  moves it to another session instead. Either way, handle `%session-changed`.
- **Target syntax trap:** `-t _moa` silently matched an existing `_moa2` after `_moa`
  had gone (prefix matching) **[verified]**. Use `=_moa` for sessions, `=_moa:` when a
  window or pane target is expected (`split-window -t =agent0` fails with
  "can't find pane", while `=agent0:` works), or better, `$id`, `@id` and `%id`.
- **Server version:** `tmux -V` reports the *client* binary. A running server may be
  older (for example after a nix upgrade), and the client and server must speak the
  same protocol. Read `#{version}` from the server on connect.
- **Server identity:** ids restart at `$0 @0 %0` with each server, and the socket path
  (and inode) is reused. Identify a server generation as `(#{pid}, #{start_time})`
  (libtmux-rs finding; both formats exist since 3.2):
  `display -p '#{pid} #{start_time} #{version} #{socket_path}'` gives
  `11326 1794736216 3.8 /private/tmp/tmux-501/moaresearch`.

---

## 5. Running multi-step commands

### 5.1 Ordering and atomicity [source]
- Each client has a FIFO command queue. Lines are parsed and appended in order, and
  each line's commands form one *group*. `control_read_callback` consumes all
  complete lines available in one read, then `cmdq_next(c)` drains that client's
  queue until it's empty or an item WAITs. A batch written with one `write()` is
  therefore usually executed back to back, without user keystrokes interleaving and
  with one redraw at the end. That's not a documented guarantee, but it follows from
  the event loop.
- Responses come back in exactly the order of the lines, one block per executed
  command.

### 5.2 Correlation scheme for the daemon
- Keep a `VecDeque<Pending>` and push one entry per **line** sent.
- Reader state machine: `Idle` → on `%begin T N F` → `InBlock{T,N,F,lines}` → on the
  exact `%end T N F` or `%error T N F` → complete.
  If `F == 0`, discard (server-originated). If `F == 1`, pop the front pending entry.
  - **Simple mode:** exactly one command per line, so one block per line. Errors don't
    stop later lines (they're separate groups).
  - **Transaction mode** (abort on first error): send `a ; b ; c` as one line, then a
    sentinel line `display -p 'moa-seq 42'`. Collect flag-1 blocks until the sentinel's
    block. Fewer blocks than commands, with the last one `%error`, means the group
    aborted at that step. **[verified that the group stops and the next line still runs]**
- Use the reply of `-P -F '#{pane_id}'` to learn new ids. **A command after
  `split-window` in the same line does not target the new pane**:
  `split-window … ; set -p @moa_session abc` put the option on the *ctl's* current pane
  **[verified]**. That's two round trips (0.06 ms each), or `-e` for the environment.

### 5.3 Latency [verified, this Mac, `e5.py`, `e10.py`]
| Operation | Control mode | Process per command |
|---|---|---|
| `display -p` round trip | **0.057 ms** median (p95 0.088) | **9.8 ms** median (p95 12.4) |
| `list-panes -a -F …` (6 panes / 41 panes) | 0.15 ms / 1.8 ms | — |
| 27-command layout switch (swap, resize, select, `set -p`) | **2.7 ms** pipelined; 2.8 ms as one `;` line | **271 ms** (27 × `tmux …`) |
| Create 10 sessions + 30 splits (40 real processes spawned by tmux) | 53 ms total | — |

### 5.4 Commands that must never go on the main ctl
Any command that returns `CMD_RETURN_WAIT` writes its `%end` **immediately** and then
**stalls the ctl's whole queue**. Its later output shows up as unframed lines or extra
flag-1 blocks:
- `command-prompt` and `confirm-before` **block by default since 3.3**. Verified: a
  `display -p ping` sent while a daemon-issued prompt was open only answered after the
  user pressed Enter, and the prompt's callback output arrived inside a flag-1 block
  that would have been matched to the wrong request. **Use `-b`.** With `-b` the queue
  stays live and the callback arrives as a plain `%message`.
- `wait-for` (all forms, including 3.8 `wait-for -E`). Verified that `-E -v` prints the
  payload lines after `%end`, outside any block. libtmux-rs measured the same on 3.7d.
- `run-shell` without `-C` (it's a process anyway), `if-shell` with a shell condition,
  `display-popup` (runs a process), and `source-file` of files that contain these.

### 5.5 Errors
`%error` blocks carry tmux's message text (`can't find pane: %99`,
`unknown buffer: …`, `parse error: …`). Log the full request line on error. Watch for
`%exit` and stdout EOF: fail all pending requests, reconnect, re-snapshot, and
reinstall hooks and bindings.

---

## 6. Pane identity

- Within one server lifetime, `%N` ids are **never reused**. `next_window_pane_id++` is
  a monotonic counter, and the same holds for windows and sessions. Ids restart only
  with a new server.
- Pane id and **pane user options travel with the pane** through `break-pane`,
  `join-pane`, `swap-pane` and `respawn-pane -k` **[verified]**:
  ```
  init        a:@0 %2 @=S1
  break-pane  hidden:@2 %2 @=S1
  join-pane   a:@0 %2 @=S1
  swap-pane   hidden:@1 %2 @=S1
  respawn -k  hidden:@1 %2 @=S1
  ```
- **Recommended identity:**
  - moa's durable store holds `moa_session_id` (UUID) and the agent's own resume id.
    That's the only thing that survives a tmux server restart, after which the agents
    are gone anyway and get resumed.
  - Back-reference in tmux: `set -p -t %N @moa_session <uuid>` right after creation.
    Optionally also `-e MOA_SESSION=<uuid>` on `split-window` or `new-window`, which
    puts it in the process environment and in `#{pane_start_command}`.
  - Live key: `(server pid, server start_time, %pane_id)`. `pane_pid` is useful for
    sanity checks (pids can be reused after the process dies; `respawn` changes it).
  - Daemon restart rebuild with one command:
    `list-panes -a -F 'P #{pane_id} #{@moa_session} #{pane_pid} #{pane_dead} #{session_id} #{window_id}'`.
- Don't use `pane_current_command` as identity. It reports the OS process name, which
  for nix's coreutils `sleep` showed `coreutils`, and an empty 3.8 `-E` pane showed
  `zsh`. **[verified]**

---

## 7. Pane death, remain-on-exit, focus: events without processes

### 7.1 Death [verified 3.8]
- `remain-on-exit on` (also `failed`, 3.7 `key`, 3.8 `failed-key`) keeps the dead pane.
  Formats: `pane_dead`, `pane_dead_status`, `pane_dead_signal` (the subscription showed
  `%1:1::kill`), and `pane_dead_time`. `respawn-pane -k -t %N 'cmd'` restarts the agent
  in place and keeps the id and options.
- Hooks: **`pane-died`** fires when the pane is kept (remain-on-exit). **`pane-exited`**
  fires when it closes. In the test exactly one fired in each case.
- Hooks only run tmux commands, so to reach the daemon a hook must use a command that
  **expands formats itself**. In 3.8, hook command strings are *not* pre-expanded:
  `set-buffer -b "moa:died:#{hook_pane}"` produced a buffer literally named
  `moa:died:#{hook_pane}` **[verified]**. Working options:
  ```
  set-hook -g 'pane-died[90]'   "display-message -c client-3575 'moa:pane-died #{hook_pane}'"
  set-hook -g 'pane-exited[90]' "display-message -c client-3575 'moa:pane-exited #{hook_pane}'"
  # -> %message moa:pane-exited %2       (arrives in the same event-loop turn) [verified]
  ```
  The 3.2+ equivalent that doesn't depend on display-message's formats is
  `run -C "set-buffer -b 'moa:died:#{hook_pane}' ."`, which gives
  `%paste-buffer-changed moa:died:%1`. The daemon must then `delete-buffer` it.
  Named (non-automatic) buffers aren't used by a bare `paste-buffer`, but they do show
  briefly in `choose-buffer`.
- Why `display-message -c` works from a hook even though hooks have no client:
  `cmdq_fire_command` sets `item->client = cmd_find_client(item, NULL, 1)` when it's
  NULL (present in 3.4 through 3.8). Since the ctl itself is always attached, there's
  always a client. **[source + verified]**
- **Don't clobber user hooks.** Hooks are array options, so use a fixed index
  (`pane-died[90]`; numeric keys work on all 3.x, named keys like `[moa]` only on 3.8
  **[verified]**). Reinstall on every connect (idempotent) and `set-hook -gu
  'pane-died[90]'` on clean shutdown. Since 3.2 pane hooks are pane options and
  window hooks are window options. `-g` sets the global default, which is what moa
  wants.

### 7.2 "The user switched to pane X": event-only
Maintain `client → session`, `session → current window` and `window → active pane` from:
- `%window-pane-changed @w %p`: G, for select-pane, mouse and the active pane being
  killed;
- `%session-window-changed $s @w`: G, for select-window, next and prev;
- `%client-session-changed <client> $s name`: G, for switch-client on the user's
  terminal;
- seeded once at connect from `list-clients -F`, `list-sessions -F` and
  `list-windows -a -F`.

The focused pane of terminal client C is
`active_pane[current_window[session_of[C]]]`. No hook needed. Optional extras:
`pane-focus-in` (needs `focus-events on`, and also fires on terminal focus regain) and
`client-active` (3.3+, which client is most recent), both routed to the mailbox.

### 7.3 Hooks vs notifications
Use the notifications for anything they cover, because they arrive without
installing anything. Use hooks with `display-message -c <ctl>` for pane-died,
pane-exited, window-layout-changed (for windows outside the ctl's session),
alert-silence, alert-bell, and the 3.8 extras (`pane-command-finished` with OSC 133,
`window-zoomed`, `pane-resized` …). Never `run-shell` without `-C`.

---

## 8. Key bindings that notify the daemon without a process

**Mechanism [verified 3.8, `e4.py`, `e11.py`, `e16.py`]:**
```
bind -n C-a switch-client -T moa
bind -T moa g display-message -c client-6307 'moa:key g #{pane_id} #{session_id} #{client_name}'
```
Pressing `C-a g` in the user's terminal gives the daemon:
```
%message moa:key x pane=%1 sess=work client=/dev/ttys045
```
- Latency, from the pty write to the daemon reading the line: **0.78 ms median, 2.2 ms
  max** (30 presses). No process.
- `#{client_name}` is the pressing terminal: display-message uses `-c` only for
  formats when that client is in the target's session, and otherwise falls back to
  the best client of the target session. So the daemon knows which terminal to answer
  (for example with `display-menu -c`).
- The ctl's own name is `client-<pid of the tmux -C process>` when its stdin isn't a
  tty. Read it at connect with `display -p '#{client_name}'`.

**Static bindings that survive daemon restarts (recommended):**
```
# in moa's tmux config, written once:
bind -T moa g run -C "display-message -c '#{@moa_ctl}' 'moa:key g ##{pane_id} ##{session_id} ##{client_name}'"
# daemon on every connect:
set -gF @moa_ctl '#{client_name}'
# -> %message moa:key F6 %0 $0 /dev/ttys039          (median 1.3 ms) [verified]
```
`run -C` expands `#{@moa_ctl}` and turns `##{…}` into `#{…}` for display-message to
expand. When the daemon is down, `-c` names no client and the key is a **silent no-op**
(display-message has `CMD_CLIENT_CANFAIL`) **[verified]**. moa could add an `if -F`
that shows "moa is not running" instead.

**Alternatives considered:**
| Approach | Verdict |
|---|---|
| `display-message -c <ctl>` → `%message` (3.4+) | **Use.** Instant, carries a payload, no side effects. |
| `run -C 'set-buffer -b "moa:key:g:#{pane_id}" .'` → `%paste-buffer-changed` | Works and is instant, but pollutes the buffer list and needs a delete. Only a fallback. |
| `set -gF @moa_key …` + subscription | Up to 1 s latency (`%subscription-changed k … : z …` 1 s later). No. |
| 3.8 `set-hook -E @moa-key` + `wait-for -E -v` on a 2nd ctl | Works, but each wait is one-shot (can miss events between re-arms), prints unframed lines, blocks that client's queue, and is 3.8-only. No. |
| `send-keys -t <moa panel pane> …` | Works on any version, but needs a moa process pane to read it. Not needed. |
| `run-shell moamenu …` (today) | One process per key at 5–10 ms each. This is what we're replacing. |

**Menus and prompts without processes:**
- Open menus **from key bindings**, so they run in the terminal's context. Make each
  item `display-message -c <ctl> 'moa:menu <item>'`. Verified: F5 → menu → `n` gives
  `%message moa:menu new …`. A `display-menu -c <tty>` sent *by the ctl* didn't react
  to keys in 3.8 (menus now belong to a window, and the default `-t` was the ctl's
  `_moa` window), so pass `-t <user's pane>` as well if the daemon opens one.
- Text input: daemon sends `command-prompt -b -t <tty> -p 'Agent name:'
  "display-message -c <ctl> -l 'moa:prompt %%'"` and receives
  `%message moa:prompt fix bug "x" ✓` **[verified; `-l` stops format expansion of the
  user's text]**. `%message` text goes through `utf8_stravisx(VIS_OCTAL|VIS_CSTYLE)`,
  so control characters arrive C or octal escaped. Unescape them.
- `display-popup` always runs a process. The picker popup (`python3 picker.py`) needs a
  different design (a long-lived console pane, `choose-tree`, or a menu).
- `send-keys -K -c <client> <key>` (3.4+, fixed in 3.8) simulates a real keypress
  through the key tables. That's useful for integration tests of the bindings
  **[verified]**.

---

## 9. Rust crates, or write our own

| Crate | What it is | Fit |
|---|---|---|
| `tmux_interface` 0.4.0 (MIT, since 2019, 175k downloads) | Mainly a typed builder for **CLI invocations, one process per command**. Has a `control_mode` module with a line-level `Response` enum feature-gated per tmux version, but `%output` values are `String`, which is lossy for split UTF-8. | Grammar reference only. |
| `libtmux` (libtmux-rs) 0.1.2 (MIT/Apache, Aug 2026, 20 releases, "pin an exact version") | Async (tokio) typed client and object model. CLI dispatch by default plus an optional `Server::over_control_mode` handle (routed mode needs tmux 3.7+). Fuzzed control-line and block parsers, compat lanes 3.3a–3.8, Linux-first ("macOS best-effort"). Excellent `docs/findings.md`. | Too big and unstable to depend on. **Read its findings and fuzz seeds.** |
| `tmuxctl` 0.1.0 | Control-mode client with typed notifications and reply correlation. **Archived**, one release. | No. |
| `par-term-tmux` | Control-mode integration for the par-term emulator (iTerm2-style). | Tied to its app. |

**Recommendation: write our own (~400–600 lines)**, because the protocol is small and
the edge cases are ours:
- Read **bytes**: `tokio::io::BufReader::read_until(b'\n')`. Don't use `lines()`, because
  output isn't guaranteed UTF-8 and a multi-byte character can be split across two
  `%output` lines (`control_append_data` cuts at arbitrary byte limits). Decode only
  the notification fields you need. Bound line length and block size (libtmux-rs uses
  8 MiB per line and 64 MiB per block) and drop the connection if exceeded.
- `%output` and `%extended-output` unescape: only bytes `< 0x20` and `\` are escaped,
  as `\ooo` (always 3 octal digits). Bytes `>= 0x7f` and DEL pass raw. Feed the bytes
  into a per-pane byte stream, then a VT parser (for example `vt100` or
  `alacritty_terminal`) if moa ever renders.
- State machine and FIFO matching as in §5.2. Notifications go to a typed enum. Keep
  unknown `%foo` lines as `Unknown(String)` for forward compatibility (3.8 added
  fields after `:` precisely so they can be extended). For `%subscription-changed`
  and `%extended-output`, split at the first `" : "` after the fixed fields.
- Writer: one task owns stdin. Commands are built with an escaping helper (quote with
  `'…'` and escape `'` as `'\''`, or use `"…"`; reject `\n` in arguments).
- Test against a real tmux on a private `-L` socket in CI (needs tmux ≥ 3.4 on the
  runner) and fuzz the parser with `cargo-fuzz`, as libtmux-rs does.

---

## 10. Pitfalls by version and the minimum version

Feature timeline (CHANGES + `git show <tag>:file`):
| Version | Relevant additions and fixes |
|---|---|
| 3.0 | Pane options (`set -p`). `refresh-client -F no-output`. |
| 3.2 | **Subscriptions `-B`**, **`pause-after` / `%extended-output` / `-A`**, client flags on `attach -f` (`ignore-size`, `no-output`, `read-only`), `%client-detached`, **`run -C`**, pane hooks become pane options, `no-detach-on-destroy`. |
| 3.3 | `client-active` hook. **`command-prompt` and `confirm-before` block by default** (use `-b`). |
| 3.4 | **`display-message` → `%message` for control clients**, `%paste-buffer-changed` name fixed and `%paste-buffer-deleted` added, `%config-error`, `send-keys -K`. |
| 3.5 | `refresh-client -r` (OSC 10/11 reports). |
| 3.7 | **`-A %N:off` no longer crashes the server**. Control client hang on exit after toggling `no-output` fixed. `remain-on-exit key`. |
| 3.8 | **Notifications deferred out of `%begin/%end`**, replies bounded (64 MiB), stuck clients can't block server exit, offsets reset on respawn, events and monitors (`set-hook -E/-B`, `wait-for -E`), named hook keys, JSON layouts (`new-layouts` flag), `new-window -E` empty panes, `pane_last_output_time`, `pane_output_generation`. |

Known pitfalls (✔ = reproduced here):
1. `-A %N:off` with queued output crashes the server below 3.7 (libtmux-rs). moa
   doesn't need `off` (it uses `no-output`).
2. Notifications inside a command block below 3.8. The parser must tolerate them (§1.1).
3. ✔ Raw command output can fake guards. Match exact `(t, n, flags)` and use `-F`
   lines that don't start with `%`.
4. ✔ Flag-0 blocks from after-hooks in our queue.
5. ✔ `;` groups abort on the first error, so block counts vary (iTerm2's approach is
   fragile).
6. ✔ Blocking commands (`command-prompt` without `-b`, `wait-for`) stall the queue
   and emit misattributed or unframed output.
7. ✔ `%N` subscriptions go silent for dead panes. Scope is limited to the attached
   session.
8. ✔ The control client's size takes over windows after `refresh-client -C`. Always
   use `ignore-size`.
9. ✔ Session-name prefix matching (`_moa` matched `_moa2`). Use `=name`, `=name:` or ids.
10. ✔ Hook commands aren't format-expanded. Use display-message, `run -C` or `set -F`.
11. ✔ A command after `split-window` on the same line targets the old current pane.
12. ✔ `kill-session` of the ctl's session sends `%exit` (use `no-detach-on-destroy`
    and reconnect logic).
13. Server/client version skew after a binary upgrade. Read `#{version}`.
14. Ids and socket inode reused across server restarts. Use `(pid, start_time)`.
15. `refresh-client -l` (clipboard) can kill the server below 3.6a (libtmux-rs). Don't use it.
16. `list-keys -T <table>` with exactly one binding prints nothing on 3.7–3.7c
    (libtmux-rs). Only matters if moa reads bindings back.
17. macOS pty input queue: libtmux-rs reports only about 1 KB per burst arrives intact
    when the reader is busy. Long prompts typed into agents with `send-keys` may
    truncate. Prefer `set-buffer` plus `paste-buffer -p -d -t %N` (bracketed paste)
    and **verify** with Claude Code before relying on it.
18. ✔ `pane_current_command` shows wrapper or OS names (`coreutils`). Not an identity.

**Minimum version: require tmux ≥ 3.4. Recommend and test on 3.8.**
- 3.4 is the first version where hooks and keys can push to the daemon instantly
  (`%message`). Below that, moa needs the clunkier mailbox or panel workarounds.
- Distribution check (from memory, verify): Ubuntu 24.04 ships 3.4, Debian 12 ships
  3.3a, Homebrew and nix ship current. On 3.4–3.7 the parser must handle notifications
  inside blocks, and moa must never use `-A off`.
- The daemon checks `#{version}` at connect and refuses below 3.4, with a clear message.

---

## 11. Sketch: daemon ↔ tmux

```
daemon start / reconnect
  spawn: tmux -L <sock> -C attach -f no-output,ignore-size,no-detach-on-destroy -t '=_moa'
         (create _moa first if missing: new-session -d -s _moa -x 10 -y 3 '<cheap long-lived cmd>'
          — on 3.8 'new-window -E' makes a process-less pane)
  → display -p 'V #{version} #{pid} #{start_time} #{client_name}'   (refuse < 3.4)
  → set -gF @moa_ctl '#{client_name}'
  → set-hook -g 'pane-died[90]'  "display-message -c '…' 'moa:pane-died #{hook_pane}'"   (+ pane-exited, window-layout-changed, alert-silence …)
  → list-sessions / list-windows -a / list-panes -a / list-clients  with 'P …' -F formats  → model
  → (optional) refresh-client -B 'all::#{S:#{W:#{P:…}}}'           → 1 Hz reconciliation
run loop
  stdout: %message moa:key|menu|prompt|pane-died … → actions
          %window-pane-changed / %session-window-changed / %client-session-changed → focus model
          %unlinked-window-* / %sessions-changed / %window-* → topology model
          %begin/%end(flags 1) → resolve the next pending request; flags 0 → drop
          %exit / EOF → fail pending, back off, reconnect
  stdin:  one command per line, pipelined; transactions = 'a ; b ; c' + sentinel line
```
Process launches: **1 per connect**, **0 per action**.
