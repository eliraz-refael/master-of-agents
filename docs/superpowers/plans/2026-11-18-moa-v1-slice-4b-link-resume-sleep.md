# moa v1 slice 4b: the link, resume, reconcile, sleep, adopt

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After `kill-server` (or a reboot), moa's sessions come back 💤 and nothing starts on its
own. `moactl resume` brings one back under the account its conversation was recorded in, or says
plainly why it won't. Idle sessions go to sleep on their own. An agent started outside moa can be
adopted, and restarted under moa to be fully tracked.

**Architecture:**
- **The terminal link** (`moa-daemon/src/tmux.rs`) recovers on its own: it reaps a stuck control
  client, reports a panic at once, keeps its backoff step across a restart, and bounds every wait.
- **Ends.** An unexpected end (a `SessionEnd` with reason `other`, or a tab that exits with no
  end) means asleep; a clean quit means ended. The order the messages arrive in doesn't change
  the result.
- **Reconcile** (`moa_core::reconcile`, pure) runs once per connection, after the start drain:
  - one tokened query lists every pane: pid, `pane_dead`, `@moa_session`, `@moa_term`,
    `@moa_adopted`, folder and start command;
  - bindings of another generation, or whose pane is gone, are deleted, and their live sessions
    are put to sleep;
  - panes that run a conversation moa knows are bound.
- **Process reads** (`moa_platform::proc`) look at a pane's process and its children in-process:
  `proc_listchildpids` + `KERN_PROCARGS2` on macOS, `/proc` on Linux. `moa_agents::claude`
  recognises Claude from the tab's start command and the process argv and env. The core gets the
  result through `Deps::inspect`.
- **Launches** (`moa_core::ops::launch`) are shared by `open`, `resume` and `restart`: one
  `new-window -d -P`, the pane bound from the reply, then focus unless `--bg`. The default Claude
  folder runs with `CLAUDE_CONFIG_DIR` unset.
- **Sleep** (`moa_core::sleep`) is one wall-clock deadline, the earliest idle session's, plus a
  check after every input for sessions already overdue.
- **Adopt** binds a pane moa didn't start, marked untracked (`pane_bindings.tracked = 0`). It
  shows alive/dead only, never sleeps on its own, and is checked again before anything kills its
  tab.

```
 daemon start: Startup ─► drain spool ─► Input::StartDrained ─┐
 link: Connected{generation} ─► list-panes -a ×2 (Query) ─► reconcile ─► bindings kept/bound/deleted, live ─► 💤
                                                          └ Deps::inspect(pid, start) ─► moa_platform::proc ─► claude::pane_agent
 moactl resume Q ─► C11 checks ─► list-panes (no untracked tab runs it) ─► commit Live ─► new-window -d -P … --resume <conv>
                                                                         ─► reply P %N pid ─► bind ─► select-window (unless --bg)
 TimerKey::Sleep ─► sleep::run ─► commit Ended+asleep_at ─► kill-pane -t %N
```

**Tech Stack:** std, `libc` (already in `moa-platform`), `serde_json`, `insta`, `proptest`. No new
crates.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.7 (launching, C11 multi-account resume, C22 adopt and untracked panes, rotation and
  lifecycle). PR b2 amends its "Other SessionEnds set ended" sentence (Task 6, R14);
- §6.13 (sleep, and "after a reboot or crash", U7);
- §6.9 (C14: one brain per folder; C15), §6.6 (pane identity, the control client), §6.3 (pending
  ops, `ClientGone`), §6.8 (wall-clock deadlines), §6.1 (allowed launches);
- §6.15.1 ("restart = load, drain spool, one list-panes, reconcile").

Roadmap: Slice 4 items 6 (the rest), 7, 8, 9, 11, 12; Review Focus 5 and 6; P6; P18. Parity
source:
- `spike/moa-proto/moactl.py`: `resume`, `close`, `term`, `pane_agent`, `agent_from`,
  `untracked_panes`, `adopt`, `drowsy`, `runs_in_pane`, `put_to_sleep`, `sleep_cmd`;
- `spike/moa-proto/console.py`: the sleep line;
- `spike/moa-proto/picture.py`: `on_this_server`.

Lessons 1–4 (pane identity), 35 (wording), 40 (asleep is a flag), 72 (conversations survive).

**Builds on** slices 2–4a:
- the core: `handle`, `Tx` (`commit`, `ui`, `ui_each`, `activity_row`, `next_id`), `post_commit`,
  `mark_seen_into`, `quiet_timer`, `TimerKey`, `TmuxState` (`opens`, `settle`, `exited_unbound`,
  `take_exited`), `OpenEnd`, `Settled`, `OPENED_FORMAT`, `trusted_panes`, `bound_session`,
  `Topology` (`visible`, `moa_session`, `window_of`, `LIST_PANES`), `Cmd` (`check`,
  `literal_format`), `hooks::{pane_gone, end_exited, is_own}`,
  `ops::sessions::{open, opened, bind, opened_reply, end_open, lost_opens}`, `project_for`,
  `State::resolve`, `live::display_state`, `is_asleep`, `config::caller_store`;
- the daemon: `CoreHandle` (`start_inner`, the drainer, `close_conn`, `barrier`), `Stats`,
  `run.rs`'s `Deps`, the link actor (`Doing`, `Shared`, `back_off`, `lost`, `probe`,
  `bootstrap`, `clean_up`), `supervise`;
- the platform: `kill_term`, `pid_alive`, `ensure_private_dir`, `Paths::tmux_socket`;
- the agents crate: `find_transcript`, `TranscriptCheck`;
- the testkit: `TempBrain`, `Sandbox`, `wait_for`, `fake_claude()`, `FakeStep` (incl. `Wait`),
  `write_script`, `crates/moa/tests/common` (`open`, `came_up`, `moa_toml_with_fake_account`,
  `ls_row`, `ls_display`, `run_exe`, `moactl`, `tmux`), `crates/moa-daemon/tests/common`
  (`boot_with`).

## Where this plan sits

Slice 4 is two plans (`2026-11-17-moa-v1-slice-4a-hooks.md`, "Slice 4 is two plans"). This is the
second.

| Plan | Roadmap items | Done when |
|---|---|---|
| 4a: hooks and the live state | 1–5, 10, and `open` (fresh) from 6 | done (#42–#46) |
| **4b (this one)** | 11 (first), the rest of 6, 7, 8, 9, 12 | `kill-server` makes sessions come back 💤, resume follows C11, idle sessions sleep, and untracked panes can be adopted |

## PRs (roadmap P4)

Four PRs. Each branches from `main` after the previous one merged, and each gets:
- the full verify:
  - `cargo fmt --check`;
  - clippy, also `--target x86_64-unknown-linux-gnu` for `moa-platform` and `moa-tmux`;
  - `cargo nextest run --workspace` three times;
  - `nix flake check`;
- the tmux 3.4 pass on the tmux-facing crates (`moa-tmux`, `moa-daemon`, `moa`), the slice 3 way;
- `/code-review medium --fix`, with the fixes pushed;
- green CI.

Each body ends with its "Try it" (when there is one) and "Restart needed: nothing; the spike is
untouched." The b2 try-it starts by resetting the throwaway sandbox brain (R33).

| PR | Branch | Tasks | Title |
|---|---|---|---|
| b1 | `v1/slice-4b-1` | 1–3 | `v1 slice 4b (1/4): a sturdier terminal link` |
| b2 | `v1/slice-4b-2` | 4–7 | `v1 slice 4b (2/4): unexpected ends and reconcile, so sessions come back asleep` |
| b3 | `v1/slice-4b-3` | 8–11 | `v1 slice 4b (3/4): resume, focus, close, terminals and sleep` |
| b4 | `v1/slice-4b-4` | 12–15 | `v1 slice 4b (4/4): adopt, untracked tabs, restart under moa` |

This plan file is committed in PR b1. PR b1 also marks roadmap item 12's "`moactl open` makes the
folder before picking the brain" as done (fixed by #46: `ctl.rs` now routes first).

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| R1 | **Order: the link, then ends and reconcile, then the launch ops and sleep, then adopt.** Process reads (`moa_platform::proc`) come in b2 with reconcile. | The roadmap puts item 11 first. Reconcile is what makes bindings trustworthy, and resume, close and sleep act on bindings. Reconcile needs process reads to verify `spike_panes` (P18) and to rebind running agents, and adopt reuses them. |
| R2 | **Reconcile waits for the start drain.**<br>• The daemon sends `Input::StartDrained` once the start drain has run, whether its commit landed or `.draining` was kept for a retry (at once when there is no spool). The core reconciles a connection only after that.<br>• The link still connects at `Startup`.<br>• `TmuxState` remembers the last `Down`. A `StartDrained` that finds the link at `Down(NoServer)` runs `no_server` then. | Spec §6.13: "drain the spool; connect; compare". A spooled clean exit must end its session before reconcile would put it to sleep. A kept drain must not hold reconcile back; when it lands later, R14 lets its clean end win over the asleep. Connecting stays early because `tmux.ensure` (the front door) needs no panes. Without the remembered `Down`, a reboot's daemon that finds no server before its drain finishes would leave every session showing live until the user opens moa (U7). |
| R3 | **Reconcile reads its own listing.** One tokened query (`TmuxToken::Query`) per connection holds two `list-panes -a`:<br>• `PANE_INFO`: pid, `pane_dead`, tags and folder;<br>• `PANE_START`: the start command, on a line of its own per pane.<br>The topology refresh is unchanged. | It needs facts the topology doesn't keep. Roadmap 11: "`ensure` answers running before the first refresh; reconcile and focus must not assume a filled topology". tmux prints a start command on one line, with `vis`-style escapes (`\n`, `\ooo`, `\$`), so its own query keeps it apart from the folder field, and `split_start_command` decodes it exactly. |
| R4 | **A binding is kept** (`panes::holds`) only when:<br>• its `(server_pid, server_start)` is the connection's generation;<br>• its pane is listed with `pane_dead` 0;<br>• the pane's tag is the session or unset. A set tag that doesn't parse counts as another session's.<br>Anything else is deleted. When two bindings name one pane, the one the tag names is kept, else the newest `bound_at`. `Down::NoServer` deletes every binding. | Lessons 1–4: a pane id means something only with its server generation, and is checked against what runs in it. Ledger (4a T10): "reconcile keeps them unique". `NoServer` is definite: the probe found nothing at the socket. One helper keeps reconcile, adopt and `ls` from disagreeing about what "kept" means. |
| R5 | **"Put to sleep" records** `lifecycle = Ended` and `asleep_at = now`, and deletes the binding. Activity, why and `activity_at` stay, wherever it happens (reconcile, `no_server`, sleep, a lost agent).<br>Reconcile puts to sleep:<br>• every **live** session left without a binding, except a `location = Elsewhere` session, one whose launch is waiting, and one whose spike pane is on another server (R6);<br>• an **untracked** binding's session whose pane no longer runs its agent (re-inspected, R31).<br>An ended session only loses its dead binding. Reconcile writes no activity rows, only one info log with counts. | Spec §6.13: "a pane that vanished with no SessionEnd → asleep; we don't know it was closed on purpose", and "the tree looks as it was left". Roadmap 12: reconcile also ends live sessions with no binding, and panes closed with no `pane-exited`. U14: an adopted session shows dead when its agent leaves a shell tab that stays open. |
| R6 | **Rebinding.** A listed live pane that no kept binding holds is bound when:<br>(a) its tag names a known session; or<br>(b) `Deps::inspect(pid, start)` finds a Claude agent whose `MOA_SESSION`, or whose conversation id (start command first, then process argv), names a known session or conversation; or<br>(c) it is that session's `spike_panes` candidate on this server, and its agent runs the session's current conversation, or names none and its folder is the session's cwd (or one is inside the other: the spike's `related`).<br>The bound session becomes live (`asleep_at` cleared, location here) and the pane is tagged. A spike pane is "on this server" when its `tmux` column is empty or its last path component equals this server's socket name. Spike panes on this server are deleted, verified or not; ones on another server are kept, and so is their session's state. | Spec §6.13: "panes running a tracked conversation without a binding → bound". P18: a spike-launched agent's pane is its only link, and dropping it would let the fallback start a second process on a running conversation. (b) also recovers the tab of an open whose answer was lost (4a ledger, T9). The spike's `on_this_server` compares socket paths. |
| R7 | **Tracked or adopted is a property of the binding**: `pane_bindings.tracked INTEGER NOT NULL DEFAULT 1` (migration 1 edited, P6), `PaneBinding.tracked: bool`. Adopt binds with `false` and tags the pane `@moa_adopted 1`. A rebind under (b) without `MOA_SESSION`, and not a spike candidate, is untracked too. | U14: an adopted session shows alive/dead only, since no hooks reach it. It must never sleep on its own: it always looks idle while it may be working. (The spike would kill it after 6 h; spec §6.13 asks for a session whose display state *is* idle.) The marker belongs to the running process, so it goes when the pane goes, and a resume is tracked again. |
| R8 | **Process reads reach the core as `Deps::inspect: fn(pane_pid: i64, start: &[String]) -> Option<PaneAgent>`.**<br>• They read the pane's process, then its direct children, in-process.<br>• They run only at reconcile, for `ls`'s untracked list, in `adopt`, before a resume, and before a kill of an untracked tab (R31); never per event.<br>• The env read keeps 5 keys only: `CLAUDE_CONFIG_DIR`, `MOA_SESSION`, `MOA_BRAIN`, `MOA_TITLE`, `MOA_GROUP`.<br>• `PaneAgent.started_at` is the agent process's start time. | C22 (no `ps`/`pgrep`/`lsof`). A `sysctl` per process costs microseconds, so a blocking fn pointer keeps the core synchronous and testable, like `Deps::canonical`. A process's environment can hold secrets, so the rest is never kept. The start time lets adopt choose a transcript the agent itself wrote (R21). |
| R9 | **What counts as Claude** (`moa_agents::claude::pane_agent`, used on both argv lists):<br>• argv[0]'s last component is `claude`;<br>• or argv[0] is `node`/`bun` and argv[1]'s last component is `claude` or it ends in `claude-code/cli.js`;<br>• leading `K=V` words are skipped, and so is `env` with its `-u NAME` and `-i` options.<br>The conversation is `--resume X`, `--resume=X`, `-r X`, `--session-id X` or `--session-id=X`.<br>`merge(start, process)` combines the two views, in this order:<br>1. `MOA_SESSION` from the process env;<br>2. the start command's argv;<br>3. the process argv;<br>4. none.<br>Kind is Claude if either view says so. Store, title and group come from the env.<br>Codex isn't recognised until Codex lands (U8). Tests launch `fake-claude` through a symlink named `claude` (`moa_testkit::fake_claude_named_claude`). | C22 names `pane_start_command` as well as the process tree. Node's `process.title` can overwrite argv, so a retitled Claude shows plain `claude`. tmux keeps the start command as launched, so for every tab moa or the spike started it is reliable. `fake-claude` can't retitle itself, so a unit test feeds a retitled argv (`["claude"]` plus a start command with `--resume X`). The `env -u` rule reads R11's launch line. |
| R10 | **Ops that act on bindings wait for this connection's reconcile**: `resume`, `focus`, `close`, `sleep`, `adopt`, `restart` and the untracked list refuse with `moa is still looking at its tabs; try again in a moment` until it's done (`ls` then just leaves the list out). `open` and `tmux.ensure` don't wait.<br>`reconciled` is cleared by a new connection and by `TmuxState::reload()`. A reconcile whose commit is refused, or whose listing came back as an error, is asked again by the first input after writes are accepted, with one warn log for a failed listing. | Roadmap 11's "`ensure` answers before the first refresh" is then harmless: nothing that needs the panes trusts them before reconcile. A reload can drop the reconcile's changes, so its flag goes with them. A failed listing must not leave every binding op refusing until the next connection. |
| R11 | **One launch builder** (`ops/launch.rs`) for `open`, `resume` and `restart`:<br>• `new-window -d -P -F 'P #{pane_id} #{pane_pid}'`, with `-n` and `-c` through `literal_format`;<br>• `--session-id <new>` for a fresh session, `--resume <conv>` for a resume, plus `--name <title>`;<br>• the env comes from `launch_env` with `ConfigDirMode`. **`CONFIG_DIR_MODE = UnsetForDefault`**: a store that is `<home>/.claude` gets no `-e CLAUDE_CONFIG_DIR`, and `/usr/bin/env -u CLAUDE_CONFIG_DIR` before the launcher. Every other store gets `-e CLAUDE_CONFIG_DIR=<store>`, as before. | Roadmap 12: resume's `-c` must be doubled like open's. The relaunch must use `--resume` (4a's `is_own` drops a new `startup` id from a session already heard from). **Ruling (the human):** Claude's own default is that same folder, so unsetting changes nothing for Claude except that it can't key its login on the variable. "Unset explicitly, never omit": the tmux server's global env may hold the user's value. `env` execs into the agent: it is the agent's launch, not another process. |
| R12 | **Focus** sends `select-window -t %N` and `select-pane -t %N` after the commit.<br>• A session whose launch is still waiting: the focus joins that launch and answers when its pane is bound.<br>• A session that isn't running: it is resumed, then focused.<br>• `open` and `resume` now focus unless `--bg` (4a's H14 is lifted).<br>Swapping into the agent spot stays in slice 5. | Spec §6.4: "`session.focus` that waits for a pane simply replies later". `-t %N` reaches the pane without the topology. |
| R13 | **Resume (C11)**, in this order:<br>1. a running session (a binding of this generation) is focused, not launched again: `went to <title>`, or `<title> is already running` with `--bg`;<br>2. refuse: `location = Elsewhere`; a spike pane on another server; no conversation (rule 3: never "the latest in this folder");<br>3. the account is the one whose store (R32) is the current conversation's `store_dir` (rule 1), of kind Claude, with a launcher (rule 6);<br>4. the folder: of `cwd` and `start_cwd`, the one whose Claude project slot is the folder of the conversation's `transcript_path`; else `start_cwd`, else `cwd`. It must exist (`Deps::is_dir`; rule 4), and `route(config, account, folder, this brain)` must be this brain (C14, rule 6);<br>5. `Deps::transcript(store, conv)` must be `Present` (rule 2). Missing there but present in another listed account's store: the refusal names that account. A missing transcript also commits `resume_failed = true` (rule 3);<br>6. one pane query: an unbound pane whose agent runs this conversation (inspect) refuses `it's already running in tab %N; adopt it with moactl adopt %N`;<br>7. no flag that skips Claude's trust prompt is ever passed (rule 5).<br>Then one commit (R13a) and the window. | Spec §6.7 C11 rules 1–6. Claude finds a conversation by the folder it started in. `CwdChanged` only moves the shown cwd, and a conversation begun by `/resume` inside Claude can belong to the other folder; the transcript's slot tells which. Step 6 stops a second process on one transcript when the agent was started by hand after the last reconcile (Review Focus 2). `resume_failed` is committed with a refusal on purpose: resume is handled in `handle`, like `open`, not through `ops::run`'s "an `Err` applied nothing" rule. |
| R13a | **Resume's commit:**<br>• live, `asleep_at` cleared, location here, `resume_failed` false;<br>• activity idle, why cleared;<br>• `activity_at` and `last_seen_at` = now;<br>• the activity row `resumed <title>`.<br>The launch's time is kept in `Launching.launched_at` and becomes the binding's `bound_at`.<br>**A resume or restart whose window fails, or whose link goes away, puts the session back asleep** (`asleep_at = now`) without `resume_failed`; `open` keeps 4a's rule. | `resume_failed` means "the conversation can't be resumed" (C11 rule 3), not "tmux failed this once", and a 💤 must not turn into ○ because of a tab error. `bound_at` = the launch time is what R18's "heard since bound" compares with. |
| R14 | **What an end means** (the human's ruling, order-independent):<br>• **Unexpected → asleep**: `SessionEnd` with reason `other` or any reason not listed below; a pane exit with no `SessionEnd` before it (4a's `pane_gone` path, reworked); reconcile; `no_server`. Asleep writes `asleep_at = at`, `Ended`, and no activity row.<br>• **Clean quit → ended**: reasons `prompt_input_exit` and `logout`, with the "○ ended" row. A clean end for a session that is asleep moves it to `Ended`, clears `asleep_at` and writes the row, past both the "older" rule and the "already ended" rule.<br>• **Rotation**: `clear` and `resume` are history only (4a).<br>• Any other end for an already-ended session is history only.<br>`close` is `Ended` without `asleep_at`.<br>PR b2 amends spec §6.7's lifecycle sentence (Task 6). | **Ruling (the human):** U7 says sessions come back 💤 after a reboot or crash. A `kill-server`, a reboot or a crash reaches a running Claude as `other`, or as a tab that just exits, and that can happen while the daemon is up. Spec §6.13 already says "we don't know it was closed on purpose → asleep". Both states resume, so a wrong 💤 costs a click, while a wrong ○ loses "the tree as it was left". A moa kill (sleep, close, restart) makes Claude send `other`, never a clean reason, so a clean end can't undo a moa sleep. |
| R15 | **tmux commands from plain ops ride the transaction**: `Tx::cmds`.<br>• `Tx::tmux` may be called only after a successful `commit` in the same tx (debug-asserted).<br>• `post_commit` sends the commands untokened on the link's connection, after the `Persist` and `Publish`. It drops them when the tx holds no changes, or when the link isn't up.<br>• The `Input::Client` arm clears `tx.cmds` together with `tx.events` when the op returns `Err`. | `close` also runs inside `suggestion yes`, through `ops::run`, which has no effects of its own. A kill must never go out for a change the store refused: the pane would die while the DB still says live. |
| R16 | **Terminals** (`term [PATH]`, method `term.open`):<br>• a `new-window -d -P` with no command: the user's shell, no moa env;<br>• named `⌨ <title, else the folder's name, else ~>`, cut to 24 characters;<br>• tagged `@moa_term 1` and `@moa_label <name>`, the label through `literal_format`;<br>• then focused.<br>The folder is moactl's cwd, or PATH resolved against it, and `ctl.rs` refuses `no folder <path>`. `--pane` is accepted and unused. The `$` and `n` keys keep their "not yet" notices until slice 5. | Spike parity (`term`), with "tab" for "window". A `#[…]` in a folder name would otherwise style the tab's border text. "Here" for a panel row is slice 5's (`ui.here`), and both keys need it. |
| R17 | **The sleep threshold is `brains.<b>.sleep_after_hours`** in `moa.toml`: a number ≥ 0, default 6, 0 = never. It is read at daemon start, so a change needs a daemon restart until slice 9's hot reload. Slice 6's `/sleep` adds a live override. | Spec §6.13: "default 6 h, 0 = never, `/sleep` changes it live". The spike read it from `MOA_SLEEP_AFTER`, and a per-brain setting belongs in `moa.toml`. |
| R18 | **Sleep eligibility** (`sleep::why_not`, pure), all of:<br>• live, here, not archived, display state `Idle` (not quiet, done, question, waiting, failed or working);<br>• a **tracked** binding of this generation, whose pane is in the topology and tagged with this session;<br>• **heard from since bound**: `last_seen_at > binding.bound_at`;<br>• the pane isn't visible;<br>• its window isn't the cockpit (the window named `moa` in the session `moa`, as the status template tells it);<br>• no ask `pending`/`sent`/`received` from or to it;<br>• `activity_at` (else `created_at`) at least the threshold ago;<br>• the link is up and reconciled, and no launch of it is waiting.<br>**Why a tracked binding with a matching tag is spec §6.13's "verified as running that conversation":** moa started that pane with the launcher as its process, so the pane dies with the agent, and tmux never reuses a pane id within one server's life. | Spec §6.13's list, plus two safe sides. A session moa launched but never heard from (side-loading blocked by policy, a broken plugin) shows `Idle` while it may be working. Roadmap 8: in this slice only panes outside the cockpit sleep; a user can move a pane there by hand, and slice 5 parks the spot first. "Not a panel or terminal" holds by construction: only agent panes are bound. |
| R19 | **Sleep is one deadline plus a check.** `TimerKey::Sleep` is armed at the earliest future `activity_at + threshold` among live, idle, tracked, bound sessions. When it fires, **and at the end of every later input** (`sleep::run`, placed like `mark_seen_into`), every overdue session that is eligible goes to sleep in one commit. | Spec §6.13: "when it fires and the session isn't eligible (for example it's on screen), it re-evaluates on the next relevant event instead of polling". Every input can be that event, and the check is an in-memory scan. |
| R20 | **Sleep wording.**<br>• **On its own:** one activity row (who = brain) and a ui event, `💤 put N idle sessions to sleep: <names> · resume from the tree`.<br>• **`sleep Q…`:** puts any running session to sleep, tracked or not (the user asked; an untracked one only after R31's check). It answers `asleep: <names> (resumable: moactl resume ID)`, plus `<name> isn't running in moa (<state>); nothing to put to sleep` for the rest.<br>• **`sleep --idle H`:** uses R18 with H hours.<br>• **`--dry-run`:** answers `would sleep <handle>  <name>` lines, or `nothing to put to sleep`. | The spike's `sleep_cmd` and console line. |
| R21 | **Adopt** (`adopt PANE [--title] [--group] [--id]`), after one pane listing:<br>• the pane is listed and alive, not a terminal, and no binding `holds` it (R4); a stale binding naming it is freed;<br>• `Deps::inspect` finds a Claude agent;<br>• the folder is the brain's (R22), its store (R32) is a listed account's, and the route is this brain;<br>• the conversation is `--id`, else the agent's own, else `Deps::newest_conversation(store, cwd, held, since)`: the newest transcript modified at or after the agent's `started_at` that no kept binding's session holds;<br>• a conversation a kept binding's session holds is refused.<br>A known conversation reuses its session. Otherwise there's a new session: title from `--title`, then `MOA_TITLE`, then the folder's name; group from `--group`, then `MOA_GROUP`; `project_for`; and a conversation with the new reason `adopt`. The binding is untracked (`bound_at` = now), and the pane is tagged `@moa_session` and `@moa_adopted 1`. A freed stale binding's live session is put to sleep. The activity row is `adopted <title> (<account>, <folder>)`. | Spec §6.7 C22 and U14. Spike parity (`adopt`). One live pane per conversation (Review Focus 2). A freshly started `claude` that hasn't written its transcript yet would otherwise be matched to an *older* free conversation, and a restart would then resume the wrong one; it is refused instead ("ask it one thing first, or pass --id"). |
| R22 | **"The brain's folders"** (adopt and the untracked list): `config.brain_for_folder(cwd)` is this brain. A brain with no `folders` accepts any folder no other brain owns. | Spec C15: folders decide the brain. A fresh `default` brain lists no folders and must still see its own tabs. |
| R23 | **The untracked list.** When the link is up and reconciled, `session.ls` waits for one pane listing. It lists panes that are alive, not terminals, not held by a binding or tagged, carry no `MOA_SESSION`, run Claude (per inspect), and sit in the brain's folders.<br>• Text: `UNTRACKED tab %N: <account> in <cwd>  → moactl adopt %N`. The account is the listed account of its store (R32), else the store folder's name.<br>• JSON: rows `{"untracked": "%N", "account", "cwd"}` go after the sessions (spike parity). Each session row also gets `"tracked": <bool>`, and a live adopted session's text row ends `  adopted: moactl restart <handle> tracks it`.<br>• The same listing re-inspects every untracked binding: one whose agent is gone puts its session to sleep (R5, R31), in the same reply. | Spike `ls` and `untracked_panes`, with "tab" for "pane" (lesson 35). `%N` stays, because it is what `adopt` takes. U14's "dead" shows without waiting for the next connection. |
| R24 | **Restart under moa** is a new verb, `moactl restart Q` (`session.restart`), for a running adopted session.<br>• It runs resume's checks (R13 steps 3–5) and R31's check.<br>• Its one commit deletes the old binding (`Change::DeletePaneBinding`), keeps the session live with its launch waiting, and writes the activity row `restarted <title> under moa`.<br>• While a `Restart` launch of the session waits, a `SessionEnded` for it is history only.<br>• Then `kill-pane -t %N` and the resume window go out in that order on one connection.<br>• A `pane-exited` for the old pane, before or after the new window's answer, changes nothing.<br>`restart` joins `VERB_NAMES` and `RESERVED_BRAIN_NAMES`. The one-key action on the tree's row comes with slice 5. | U14: "a one-key restart under moa resumes the same conversation inside moa with full tracking". The CLI form is what that key will send. Deleting the binding first keeps the old tab's exit from being read as the session's end. An adopted agent whose hooks do reach moa would otherwise put the restarting session to sleep with its `SessionEnd(other)`. tmux destroys the old pane at `kill-pane` without waiting for its process. The two can overlap only for the old agent's shutdown work after SIGHUP, and the new one writes nothing to the transcript then, since moa sends it no prompt. So moa doesn't wait for the old process: kqueue `NOTE_EXIT` or a `pidfd` could, but it isn't worth that machinery. The PR body says so (Minor M4, declined). |
| R25 | **The link (roadmap 11):**<br>• **Reap.** On connect, before naming itself, the link reads `@moa_ctl`. If it names another control client still in `list-clients`, the link sends `kill_term` to that client's `#{client_pid}`.<br>• **Panic.** A panic while up tells the core `Down(Lost)` at once, from supervise's `failed` callback; the restart then reconnects.<br>• **Probe.** `lost()` notes "connecting" before it probes, so a panic there still reconnects.<br>• **Backoff.** The backoff step survives a restart (`AfterRestart`).<br>• **Bounds.** `CONNECT_WAIT` is 8 s, below the front door's 10 s; it is a `ConnectOptions` field so tests can shorten it. Bootstrap and clean-up commands wait 2 s each, and a timeout counts as a loss.<br>• **Wording.** A protocol error is `Down::CantLink`: `moa couldn't talk to its terminal; see moa's log`. | Each line answers a roadmap 11 item; the reasons are there. |
| R26 | **Connections that close**: `Input::ClientGone(ConnId)` (spec §6.3's `ClientGone`), sent by `CoreHandle::close_conn` for every closed connection. That includes each async hook's connection, so core inputs per hook roughly double. It drops that connection's ensure waiters, pane queries and focus waiters. A launch keeps running and only loses its reply. So does a terminal's `Asked::Term`, whose window is already on its way and still needs its tags. **`answer()` emptying the waiters before a later panic is accepted**: a panic is already reported, and the front door gives up at 10 s with a plain line. | Roadmap 11 asks for "a connection-closed signal". Tests that count `Stats::inputs` are updated knowingly. Moving the take after the effects would only shift the window. |
| R27 | **`Topology::from_lists` can't loop the link.** A proptest, `from_lists_never_panics`, feeds it arbitrary lines. There is no runtime guard. | Its parsing has no indexing that can fail. A test that proves it is cheaper than a loop breaker. |
| R28 | **The reaper uses `waitid(P_PID, pid, WEXITED \| WNOWAIT)`** (`moa_platform::wait_exited`). The order is: wait for the exit, `send(true)`, then `child.wait()` reaps. `kill_unless_exited` holds the watch's read guard across `kill_term`, so the reaper's `send(true)`, and the reap after it, wait for a kill in progress. | Roadmap 11 ("Reaper"). `WNOWAIT` alone isn't enough: a guard dropped between the check and the kill would let the reaper reap, and the pid be reused, in between. A unit test can't see this race; the PR says so. |
| R29 | **Other link items, decided:**<br>• the `moa` session is made with `detach-on-destroy on` (both front door forms);<br>• `_moa`'s shell is `/bin/sh -s`: two words, so tmux execs it without the user's login shell;<br>• `run.rs` makes the brain dir with `ensure_private_dir`;<br>• the current-window pill gets `✗` for failed;<br>• three slice 3 tests are tightened (Task 3).<br>**Carried on, with reasons**, in "Carried items" below:<br>• a stop with no live client;<br>• held notices;<br>• prompts;<br>• the too-old `kill-server` test;<br>• the clock's e2e negative case;<br>• the front door's duplicated spawn code. | Roadmap 11's list. |
| R30 | **Plain words.** User-facing text says "tab", never "pane" or "window", except the `%N` id that `adopt` takes. That includes `moactl --help`: `term` reads "A plain shell in a new tab", and `adopt` reads "Adopt an agent running in a tab". Logs carry ids and counts, never titles, folders or argv. | Lesson 35 and the Global Constraints. |
| R31 | **An untracked tab is checked again before any kill.** Before `close`, `sleep Q` or `restart` kills an untracked binding's pane, `Deps::inspect(binding.pane_pid, &[])` must find a Claude agent running one of the session's conversations, or naming none with the session's store (R32). Reconcile and `ls` also check the folder, since they have the listing. A kill has only the pid, and doesn't need the folder: the binding's pane was checked when the binding was made, and a Claude on the same store in that same tab, running no other conversation, is the agent.<br>If not:<br>• the refusal is `<title>'s agent isn't running in that tab any more`;<br>• the session is put to sleep (R14: an unexpected end) and its binding deleted;<br>• no `kill-pane` is sent.<br>Tracked bindings need no inspect. | An adopted agent usually runs inside a shell tab. When it exits, the tab stays, and a kill would take the user's shell and whatever it now runs. The binding proved the agent when it was made, not at kill time. |
| R32 | **A store is always a folder** (`State::store_of(dir: Option<&str>) -> PathBuf` = `deps.canonical(caller_store(dir, home))`): no `CLAUDE_CONFIG_DIR` means `<home>/.claude`. Accounts are matched to stores by canonical path (`State::account_of_store`). Task 4 produces it. It is used by `reconcile::agent_runs` (Task 7), resume, adopt, the untracked list, R6's spike checks, and hook rotation: a rotated conversation's `store_dir` is `store_of(envelope.store_dir)`, never `None`. | With R11's `UnsetForDefault`, every session on the default store runs and sends hooks without the variable. Without this rule its resume, its rotations and its adoption would all fail with "unknown account". It is spec 2b's rule (`caller_store`). |
| R33 | **The schema change follows P6.** Migration 1 is edited before the switch-over. A v1 DB from an earlier build (the sandbox) is throwaway, so the b2 try-it starts by resetting it. "Restart needed" stays about the spike. | P6: until the switch-over no v1 DB is kept; they're rebuilt from the spike. The sandbox from 4a's try-it is at `user_version` 1 without the new column, and wouldn't start. |
| R34 | **`fake-claude` writes a synthetic transcript**, `<store>/projects/<slot>/<id>.jsonl`, holding one line `{"type":"summary","summary":"fake"}`, when it fires its first `SessionStart` for an id, and when it starts with `--resume <id>` and no file exists yet. `<store>` is its `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`, and `<slot>` is `project_slot(cwd)`. A test checks the path against `moa_agents::claude::project_slot`. The hooks it fires report that same path as `transcript_path` (not 4a's `projects/x/…`). **It writes nothing when the store isn't under `std::env::temp_dir()` or `/tmp`**, so a run with an inherited real `HOME` can't touch a developer's Claude folder. | C11 rule 2 needs a transcript, and a real Claude has one by then. Doing it in `fake-claude` keeps every resume and restart test honest without per-test setup. Tests about a transcript in the *wrong* account write their file themselves. |

## Parity rows this slice delivers (spec §7)

- `open`, `resume`, `focus`, `close`, `term`, `adopt` (2.13): all but `archive`/`set`/`group`,
  which are done. `focus` selects the tab; the agent spot is slice 5.
- `sleep` verbs, eligibility, display 💤 (2.11); sleep on its own (2.8): deadline timers, no
  600 s loop.
- Untracked-pane detection in `ls` (2.18), in-process.
- Explicit env for children (2.1): resume and restart use open's allow-list.
- The tmux-needing suggestion action (`close`) runs (2.9).

## Global Constraints

The roadmap's, 2a's, 2b's, 3's and 4a's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **No `ps`, `pgrep` or `lsof`** (C22). Process reads are `moa_platform::proc`, in-process.
- **Zero `Spawner` launches** for reconcile, resume, focus, close, term, sleep, adopt and
  restart. Panes are tmux's children. Task 15 checks the count end to end.
- **Never kill what moa can't prove is the session's** (R14, R31):
  - `kill-pane` always has `-t %N`, taken from a binding of the current generation that reconcile
    kept;
  - an untracked binding's agent is inspected again first;
  - no kill goes out for a refused commit (R15);
  - `Cmd::check` refuses `kill-pane` without `-t %<digits>`.
- **No secrets kept from processes**: only the 5 env keys in R8, and no argv in logs.
- **Tests:**
  - no wall-clock bets: every wait goes through `wait_for` with a 10 s limit, and e2e steps are
    gated with `FakeStep::Wait` files, never sleeps;
  - tests run only under `/tmp` roots (`TempBrain`, `Sandbox`);
  - `fake-claude` is the launcher in every e2e test, and the real `claude` never runs;
  - tmux is reached only through the testkit (`Sandbox`, `TempBrain::tmux`, `common::tmux`, the
    new `TempBrain::control_client`);
  - the flake's packages build only `moa`.
- **Plain words** (R30). The user isn't a tmux user, so no step of a "Try it" asks them to think in
  tmux terms beyond one copy-paste line.
- **Public repo**: fixtures use `Lantern`, `acct-a`, `acct-b`, `~/private`.

## Review Focus

1. **The tmux server is killed or restarted under moa, and pane ids get reused** (roadmap RF 5).
   - Expected: no stale binding is trusted; sessions come back 💤 whichever message arrives
     first; nothing auto-resumes; nothing is killed by a reused id.
   - Pinned by:
     - `a_new_generation_puts_live_sessions_to_sleep` and
       `a_reused_pane_id_with_another_tag_is_not_kept` (Task 7);
     - `an_unexpected_exit_is_asleep_whichever_message_comes_first` (Task 6);
     - `kill_server_brings_sessions_back_asleep` (Task 7, e2e).
2. **Two processes on one conversation**: resume while the agent still runs (bound, or typed in a
   tab after the last reconcile), a tab whose open answer was lost, the spike's agents after a
   switch (roadmap RF 6).
   - Expected: a running session is focused, never launched again; a pane running a known
     conversation is bound or named; adopt refuses a conversation another tab runs.
   - Pinned by:
     - `resume_of_a_running_session_only_focuses` and
       `resume_refuses_when_an_untracked_tab_runs_it` (Task 9);
     - `an_untagged_pane_running_a_known_conversation_is_bound` and
       `spike_panes_are_verified_or_cleared` (Task 7);
     - `adopt_refuses_a_conversation_another_tab_runs` (Task 12).
3. **Killing the wrong thing**: an adopted agent that left its shell tab, a refused write, the
   SessionEnd of an agent moa just killed.
   - Expected: no kill without proof and a saved change; asleep stays asleep, with no second
     "○ ended" row.
   - Pinned by:
     - `close_of_an_adopted_session_whose_agent_left_kills_nothing` (Task 10);
     - `restart_refuses_when_the_adopted_agent_is_gone` (Task 14);
     - `close_with_writes_refused_kills_nothing` (Task 10), `sleep_with_writes_refused_kills_nothing`
       (Task 11) and `restart_with_writes_refused_kills_nothing` (Task 14);
     - `a_session_end_after_sleep_keeps_it_asleep` (Task 6).
4. **Resume in the wrong place**: another account's store, a cleaned-up transcript, a deleted
   folder, another brain's folder, the default store with no `CLAUDE_CONFIG_DIR`, a folder with
   `#` in its name.
   - Expected: one plain refusal each, nothing launched, and `resume_failed` only for a missing
     transcript; the default store resumes like any other.
   - Pinned by Task 9's refusal tests, `resume_doubles_hashes_in_the_folder` and
     `a_default_store_session_resumes`.
5. **Sleep killing something the user needs**: on screen, an unseen reply, a question, in an ask,
   adopted, never heard from, in the cockpit, a launch still waiting, a binding of an old
   generation.
   - Expected: none of them sleep.
   - Pinned by `sleep_eligibility_table` and the proptest `nothing_ineligible_ever_sleeps` (Task 11).

---

## Task 1: the link recovers on its own

**Files:**
- Modify: `crates/moa-daemon/src/tmux.rs` (reap, panic → Lost, `lost()` marking, backoff step,
  bounds)
- Modify: `crates/moa-daemon/src/tmux_tests.rs`
- Modify: `crates/moa-tmux/src/client.rs` (`ConnectOptions.wait`; `pub const CONNECT_WAIT` =
  8 s)
- Modify: `crates/moa-testkit/src/temp_brain.rs` (`TempBrain::control_client`): `tmux_tests.rs`
  builds its servers with `TempBrain` (`server(&tb)`)
- Modify: `crates/moa-core/src/effect.rs` (`Down::CantLink`), `crates/moa-core/src/tmux/link.rs`
  (its reply)
- Modify: `crates/moa-core/tests/tmux_link.rs`
- Modify: `docs/superpowers/plans/2026-11-15-moa-v1-roadmap.md` (item 12: mark the `open` folder
  line done by #46; point slice 4 at this plan)

**Interfaces:**
- Produces: `Down::CantLink`. `TmuxState::down` answers waiters
  `Err("moa couldn't talk to its terminal; see moa's log")`.
- Produces: `ConnectOptions { tmux, socket, env, wait: Duration }`, and
  `pub const CONNECT_WAIT: Duration = Duration::from_secs(8);` in `moa_tmux::client`.
- Produces: `pub fn control_client(&self, brain: &str) -> io::Result<std::process::Child>` on
  `TempBrain`. It starts `tmux -C attach` on that brain's server socket with the brain's
  cleared env (`TempBrain::env`), stdin and stdout piped, and stdout left unread by the caller.
- Produces, in `tmux.rs`:
  - `const STEP_WAIT: Duration = Duration::from_secs(2);`
  - `Doing::BackingOff(usize)`, which holds the next step;
  - `fn after_restart(doing: Doing) -> AfterRestart`, where
    `enum AfterRestart { StayIdle, Connect, BackOff(usize) }`;
  - `async fn reap_stale(client: &ControlClient, ours: &ClientName)`.

**What changes:**
1. **Reap**, in `bootstrap`, before `set-option -g @moa_ctl`:
   - `show-options -gqv @moa_ctl` gives `old`;
   - when `old` is non-empty and isn't ours: `list-clients -F '#{client_control_mode} #{client_pid} #{client_name}'`;
   - a line `1 <pid> <old>` gets `moa_platform::kill_term(pid)` and an info log,
     `a stuck terminal link from before was stopped`;
   - each command waits at most `STEP_WAIT`.
2. **Panic while up**: `start` wraps the `failed` callback. When `doing` is `Up`, it marks
   `Connecting` and tells the core `Down(Lost)` before calling the original `failed`. The
   restarted run follows `after_restart`; it says `Lost` only when `doing` is still `Up`.
3. **`lost()`** marks `Doing::Connecting` before `probe()`, then moves on as now.
4. **Backoff**: `back_off` stores `BackingOff(next)`. A restart with `BackingOff(n)` waits
   `BACKOFF[n]` and tries step `n`, not 0.
5. **Bounds**:
   - `attempt` passes `wait: CONNECT_WAIT`;
   - each `bootstrap` and `clean_up` command is wrapped in `tokio::time::timeout(STEP_WAIT, …)`;
   - a timeout in `bootstrap` is `CmdError::Gone` (close, then `lost`);
   - in `clean_up`, a timeout ends the loop and closes the client.
6. **`ConnectError::Protocol`** gives `Down::CantLink`, not `GaveUp`.

- [ ] **Step 1: Write the failing tests** in `tmux_tests.rs`, with `Sandbox`:
  - `a_stuck_link_from_before_is_reaped_on_connect`:
    - `tb.control_client(b)`, with its name read back through `list-clients`, and `@moa_ctl` set
      to that name;
    - start the link;
    - that child exits (`wait_for`, 10 s);
    - `list-clients` no longer shows it, and `@moa_ctl` is the link's own.
  - `a_panic_while_up_says_lost_at_once`:
    - the recorder notes the actor's run count each time it hears an input;
    - `TmuxCmd::Crash` while up;
    - the `Down(Lost)` is heard with run count 1 (before the restart), then a `Connected` with
      a higher `conn`.
  - `a_panic_while_probing_still_reconnects`:
    - a `#[cfg(test)]` flag makes the next `probe()` panic;
    - kill the control client's process;
    - a new `Connected` arrives.
  - `a_restart_during_backoff_resumes_its_step` (no timing window):
    - a `#[cfg(test)]` backoff table on `Shared` (`BACKOFF` by default) gives this test a long
      step 1 (30 s);
    - a `#[cfg(test)]` view of `doing`, and a `#[cfg(test)]` record of each backoff step when it
      is **scheduled**;
    - kill the server's control client twice, then `wait_for` (10 s) until `doing` is
      `BackingOff(1)`;
    - crash the actor;
    - `wait_for` (10 s) until the record shows the restarted run scheduled step 1, which never
      waits out the 30 s.
    - This replaces a unit test of `after_restart`, which would only restate its `match`.
  - `a_hung_server_is_given_up_on_within_the_wait`:
    - `ConnectOptions.wait` = 300 ms;
    - the server is stopped with `SIGSTOP`, and resumed by a `SIGCONT` drop guard declared
      **after** the `TempBrain`, so it drops first (the brain's own drop talks to tmux, and
      would hang against a stopped server);
    - `connect` returns `Exited` or `Protocol` within the 10 s `wait_for`, with no answer from
      tmux.
  - In `crates/moa-core/tests/tmux_link.rs`, `cant_link_says_so`: ensure waiting, then
    `Down(CantLink)` → that exact text.
  - Make `a_server_that_detaches_the_client_on_attach_is_given_up_on` print every input it saw
    in its assertion message (roadmap 11: "capture the text if it fails again").
  - Update `a_stop_queued_before_a_restart_is_answered_without_a_reconnect`
    (`tmux_tests.rs`): change 2 now sends `Down(Lost)` at the panic, so the test expects that
    `Down(Lost)` and then quiet, instead of quiet only.
- [ ] **Step 2: Run them to see them fail**: `nix develop --command cargo nextest run -p moa-daemon -p moa-core -p moa-tmux -p moa-testkit`.
- [ ] **Step 3: Implement** them, as above.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: the terminal link reaps, reports and bounds its own failures`.

## Task 2: process hygiene

**Files:**
- Modify: `crates/moa-platform/src/sys/mod.rs` (`wait_exited`); export from `lib.rs`
- Modify: `crates/moa-tmux/src/client.rs` (the reaper thread, `kill_unless_exited`; `ctl_argv`
  ends `/bin/sh -s`)
- Modify: `crates/moa/src/front.rs`:
  - `create_argv` ends `/bin/sh -s`;
  - both `moa` session forms add `; set-option -t =moa detach-on-destroy on`.
- Modify: `crates/moa-daemon/src/run.rs` (`tmux_link` uses `ensure_private_dir(&paths.brain_dir(b))`)
- Modify: `crates/moa-tmux/src/moa.tmux.tpl` (the pill's `failed` branch: `#[fg=#d57780]✗ `)
- Tests: `crates/moa-platform/tests/pid.rs`, `crates/moa-tmux/tests/conf.rs`,
  `crates/moa/tests/tmux_e2e.rs`, `crates/moa-daemon/src/run_tests.rs`, and the existing argv
  unit tests

**Interfaces:**
- Produces: `pub fn wait_exited(pid: u32) -> io::Result<()>`. It blocks until the child `pid`
  has exited and leaves it unreaped: `waitid(P_PID, pid, &mut info, WEXITED | WNOWAIT)`, retried
  on `EINTR`, with a `// SAFETY:` comment.
- The reaper thread becomes: `wait_exited(pid)`, then `exit_tx.send(true)`, then `child.wait()`.
- `kill_unless_exited` becomes `let seen = exited.borrow(); if !*seen { kill_term(pid) }`, with
  `seen` alive across `kill_term`, and a comment saying why (R28).

- [ ] **Step 1: Write the failing tests:**
  - `wait_exited_leaves_the_child_to_reap`:
    - spawn `/bin/sh -c 'exit 3'`;
    - `wait_exited(pid)` returns `Ok`;
    - `pid_alive(pid)` is still true (a zombie);
    - `child.wait()` then gives exit code 3.
  - `the_argv_starts_utf8_and_never_starts_a_server` and `create_argv_is_t1s_command`: the
    expected texts end `/bin/sh -s`; the `moa` session line ends
    `; set-option -t =moa detach-on-destroy on`, and so does `the_moa_session_is_attached_or_made_in_home`.
  - `a_user_detach_on_destroy_off_never_lands_in_moas_hidden_session` (e2e):
    - the user's `.tmux.conf` sets `set -g detach-on-destroy off`;
    - `moa` opens;
    - `show-options -t moa -v detach-on-destroy` is `on`.
  - `the_current_window_pill_shows_failed`: the generated `moa.tmux` holds
    `#{?#{==:#{@moa_state},failed},#[fg=#d57780]✗ ,`.
  - In `run_tests.rs`, `a_brain_dir_that_is_a_symlink_is_refused`: `brain_dir(b)` is a symlink →
    the daemon refuses to start with `ensure_private_dir`'s error.
  - No test sees the reaper's guard race (R28); the PR body says so.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them. Run clippy for the Linux target too
  (`--target x86_64-unknown-linux-gnu -p moa-platform -p moa-tmux`).
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: reap without a reuse window, a plain _moa shell, detach-on-destroy`.

## Task 3: closed connections, lists that can't loop, tighter tests

**Files:**
- Modify: `crates/moa-core/src/effect.rs` (`Input::ClientGone`), `crates/moa-core/src/core.rs`
- Modify: `crates/moa-core/src/tmux/link.rs` (`client_gone`; `opens` values become
  `Option<(ConnId, ReqId)>`), `crates/moa-core/src/ops/sessions.rs` (`Settled.reply` is an
  `Option`)
- Modify: `crates/moa-daemon/src/core_task/mod.rs` (`close_conn` sends `Input::ClientGone`)
- Modify: `crates/moa-core/src/tmux/topology.rs` (proptest)
- Modify: `crates/moa-tmux/tests/client.rs`, `crates/moa/tests/tmux_e2e.rs` (tightened tests)
- Modify: any 4a test that counts `Stats::inputs` across a hook connection (R26). Say which in
  the report.

**Interfaces:**
- Produces: `Input::ClientGone(ConnId)`, which the core handles with
  `TmuxState::client_gone(conn)`:
  - ensure waiters of `conn` are dropped;
  - opens of `conn` keep waiting with `None` for a reply.
  Later tasks add their own waiting lists to `client_gone`: pane queries in Task 4, focus waiters
  in Task 8.
- Produces: `Settled { id, reply: Option<(ConnId, ReqId)>, end }`. `opened` sends no `Reply` for
  `None`. Task 8 replaces `reply` with `launching`.

- [ ] **Step 1: Write the failing tests:**
  - `a_closed_connections_ensure_is_forgotten` (core, `tmux_link.rs`):
    - ensure on conn 7 while `Connecting`;
    - `ClientGone(ConnId(7))`;
    - then `Connected` → no `Reply` to conn 7.
  - `a_closed_connections_open_still_binds` (core, `open.rs`):
    - an open waits, then `ClientGone`, then `Replies { Open(id), ["P %7 4242"] }`;
    - a `PaneBinding` is persisted, and there's no `Reply`.
  - `close_conn_sends_client_gone` (`core_task` tests): after `close_conn(c)` and a `barrier()`,
    `Stats::inputs` has gone up by exactly one.
  - The proptest `from_lists_never_panics`: three `Vec<String>`s of arbitrary text (with spaces,
    `%`, `@`, `$` and digits) → `Topology::from_lists` returns.
  - `after_hook_blocks_are_skipped`: the hook also runs `set -g @after_ran 1`. The test asserts
    `@after_ran` is `1` (a hook that never ran now fails) and the `next` reply is right.
  - `a_user_tmux_conf_with_run_shell_and_hooks`: also asserts every moa index-90 hook is present
    (`pane-exited`, `pane-died`, `window-layout-changed`, `client-attached`, `client-detached`),
    and that all are gone after shutdown while `[5]` stays.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: forget closed connections; lists that can't loop the link`.

**PR b1 ends here.** Run the full verify and the tmux 3.4 pass, run `/code-review medium --fix`,
and open the PR. Its body:
- lists roadmap 11's items, each with where it was fixed or why it's carried;
- says the reaper's guard race has no test (R28).

Try it: nothing to try by hand; this PR is about how the link recovers. Restart needed: nothing;
the spike is untouched.

## Task 4: the vocabulary reconcile needs

**Files:**
- Modify: `crates/moa-core/src/tmux/cmd.rs` (allow `kill-pane`, `select-window`, `select-pane`;
  rules)
- Create: `crates/moa-core/src/tmux/panes.rs` (`PANE_INFO`, `PANE_START`, `Tag`, `PaneInfo`,
  `parse_panes`, `split_start_command`, `list_panes`, `holds`)
- Modify: `crates/moa-core/src/tmux/topology.rs`:
  - `LIST_PANES` gains the cockpit flag
    `#{&&:#{==:#{session_name},moa},#{==:#{window_name},moa}}` before `#{@moa_session}`;
  - `Topology::is_cockpit`.
- Modify: `crates/moa-core/src/effect.rs` (`TmuxToken::Query(u64)`, `Input::StartDrained`;
  `runs_at_once` covers `Query`)
- Modify: `crates/moa-core/src/tmux/link.rs` (pane queries, `drained`, `reconciled`, `last_down`)
- Modify: `crates/moa-core/src/core.rs` (`Tx::cmds`, `Tx::tmux`, `post_commit`, the Client arm)
- Modify: `crates/moa-core/src/model.rs` (`PaneBinding.tracked`, `PaneAgent`)
- Modify: `crates/moa-core/src/state.rs` (`Deps::inspect`; `binding`, `spike_panes` accessors)
- Modify: `crates/moa-store/src/sql/0001_v1.sql`, `crates/moa-store/src/rows.rs` (`tracked`)
- Modify: `crates/moa-daemon/src/core_task/mod.rs` (send `StartDrained`; a `#[cfg(test)]` input
  recorder)
- Create: `crates/moa-core/tests/common/mod.rs`. Move `loaded`, `generation`, `link_up`, `feed`,
  `send`, `changes`, `replies` and `the_open` there from `tests/open.rs`, with `open.rs` using
  them. Add `listed(st, b, info_lines, start_lines)`, which answers the pending pane query.
- Tests: `crates/moa-core/tests/panes.rs`, the `cmd.rs` unit tests,
  `crates/moa-core/tests/tmux_focus.rs`, `crates/moa-store/tests/round_trip.rs`, the
  `core_task` tests, `crates/moa-daemon/tests/tmux.rs`

**Interfaces:**
- Produces, in `moa_core::tmux::panes`:
  - `pub const PANE_INFO: &str = "I\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@moa_term}\t#{@moa_adopted}\t#{@moa_session}\t#{pane_current_path}";`
    (real tab characters; the folder is last).
  - `pub const PANE_START: &str = "S\t#{pane_id}\t#{pane_start_command}";`
  - `pub enum Tag { Unset, Session(SessionId), Other }`. A non-empty tag that isn't a session id
    is `Other`.
  - `pub struct PaneInfo { pub pane: PaneId, pub pid: i64, pub dead: bool, pub term: bool, pub adopted: bool, pub tag: Tag, pub cwd: String, pub start: Vec<String> }`.
  - `pub fn parse_panes(info: &[String], start: &[String]) -> Vec<PaneInfo>`:
    - lines that don't parse are skipped;
    - `pane_dead` `1` is dead, and an option counts as set when it's non-empty;
    - a start line that doesn't begin `S\t%` is skipped, and that pane gets an empty `start`.
  - `pub fn split_start_command(s: &str) -> Vec<String>`. It reads the line as tmux 3.4+ prints
    it: `cmd_stringify_argv` → `args_escape`, which uses `utf8_stravis` with `VIS_OCTAL`,
    `VIS_CSTYLE`, `VIS_TAB` and `VIS_NL`, so a command is never more than one line.
    - Words split on unquoted spaces; single and double quotes group.
    - Escapes decode: `\n`, `\t`, `\r`, `\b`, `\a`, `\v`, `\f`, `\0`, and octal `\ooo`. Any
      other `\X` gives `X`, so `\$` is `$` and `\\` is `\`.
    - An unbalanced quote gives an empty `Vec`.
  - `pub fn list_panes() -> Vec<Cmd>`: `list-panes -a -F PANE_INFO`, then
    `list-panes -a -F PANE_START`.
  - `pub fn holds(b: &PaneBinding, generation: Generation, info: Option<&PaneInfo>) -> bool`:
    R4's test.
- Produces: `Topology::is_cockpit(&self, w: WindowId) -> bool`: the window named `moa` in the
  session named `moa`. The flag is read from the listing, because `base-index 1` makes the
  cockpit window 1, not 0.
- Produces, in `moa_core::tmux::link`:
  - `pub(crate) enum Asked { Reconcile }`. Later tasks add `Resume` (Task 9), `Term` (Task 10),
    `Adopt` (Task 12) and `Ls` (Task 13).
  - `TmuxState::ask(&mut self, asked: Asked, cmds: Vec<Cmd>) -> Option<Effect>`: a tokened
    `Effect::Tmux` with `TmuxToken::Query(n)` while up, else `None`.
  - `TmuxState::answered(&mut self, input: &TmuxIn) -> Vec<(Asked, Result<Vec<Vec<String>>, ()>)>`.
    It runs before `input()`, like `settle`:
    - the query a `Replies { token: Query(n) }` answers, with each command's lines (`Err(())`
      when any command failed);
    - on `Down` or a new connection, every query, with `Err(())`.
  - `TmuxState::drained: bool`, set once by `StartDrained` and never reset (it's per daemon
    start).
  - `TmuxState::reconciled(&self) -> bool`, true for one connection only, and cleared by a new
    connection and by `reload()`.
  - `TmuxState::last_down: Option<Down>`.
  - `pub(crate) const NOT_READY: &str = "moa is still looking at its tabs; try again in a moment";`
  - `pub(crate) fn ready(state: &State) -> Result<u64, OpError>`: the link's `conn` when up and
    reconciled; `moa isn't open in a terminal yet; run moa first` (`open`'s `NO_TERMINAL`,
    moved here) when it isn't up; else `NOT_READY`.
- Produces: `Tx::cmds: Vec<Cmd>` and `Tx::tmux(&mut self, cmd: Cmd)` (R15).
  - `tmux` debug-asserts that `changes` is non-empty.
  - `post_commit` appends `Effect::Tmux { conn, cmds, token: None }` after the publishes when the
    link is up and the tx has changes; it drops them (with a debug log) otherwise.
  - The `Input::Client` arm clears `tx.cmds` with `tx.events` on `Err`.
- Produces: `PaneBinding.tracked: bool`. The SQL column is
  `tracked INTEGER NOT NULL DEFAULT 1`, and `rows::PANE_BINDINGS` gains `"tracked"`. Every
  existing constructor passes `tracked: true`.
- Produces, in `moa_core::model`:
  `pub struct PaneAgent { pub kind: AgentKind, pub store_dir: Option<String>, pub conversation: Option<String>, pub moa_session: Option<String>, pub moa_brain: Option<String>, pub title: Option<String>, pub group: Option<String>, pub started_at: Option<UtcMs> }`
  (Clone, Debug, PartialEq, Eq).
- Produces: `Deps::inspect: fn(i64, &[String]) -> Option<PaneAgent>`; the default is
  `|_, _| None`.
- Produces: `State::binding(&self, id: SessionId) -> Option<&PaneBinding>` and
  `State::spike_panes(&self) -> impl Iterator<Item = &SpikePane>`, both `pub(crate)`.
- Produces (R32, moved here so every task that compares stores has it):
  - `State::store_of(&self, dir: Option<&str>) -> PathBuf` =
    `deps.canonical(caller_store(dir, home))`;
  - `State::account_of_store(&self, store: &Path) -> Option<AccountName>`, comparing canonical
    paths.
- Produces: `Input::StartDrained`, which sets `drained`.
  - `CoreHandle::start_inner` sends it after `drainer.drain().await`, or right after the Startup
    commit when there's no spool.
  - A `#[cfg(test)]` recorder in `core_task` keeps the kinds of inputs stepped, for the test
    below.

**`Cmd` rules added to `check`:**
- `kill-pane`: exactly `-t %<digits>`. Anything else is refused with
  `kill-pane needs exactly one tab id` (internal text).
- `select-window` and `select-pane`: they must have `-t`.
- `new-window` with no command after its flags is allowed (a terminal).

- [ ] **Step 1: Write the failing tests:**
  - In `cmd.rs`:
    - `kill_pane_needs_one_pane_id`: `-t %3` is allowed; a bare `kill-pane`, `-t moa`,
      `-a -t %3` and `-t %3x` are refused;
    - `select_needs_a_target`;
    - `a_terminal_window_has_no_command`.
  - In `crates/moa-core/tests/panes.rs`:
    - `pane_info_parses_with_empty_options_and_spaces_in_the_folder`: a folder `~/a b`, unset
      `@moa_term`, a dead pane, a tag `junk` → `Tag::Other`;
    - `a_start_line_that_doesnt_parse_loses_only_its_pane`;
    - `split_start_command_reads_tmux_quoting`: a table of quoted words, with `'`, `"`, a space,
      `#`, `\n`, `\$`, `\\` and an octal `\001`;
    - `store_of_maps_no_variable_to_the_default_folder`: `store_of(None)` and
      `store_of(Some("<home>/.claude/"))` are the same path; `account_of_store` finds `mine`
      (store `<home>/.claude`) for both;
    - `holds_follows_r4`;
    - `a_query_is_answered_once`;
    - `a_query_is_failed_when_the_link_goes_down_or_reconnects`;
    - `ready_waits_for_reconcile`: `NO_TERMINAL` while down, `NOT_READY` while up and not
      reconciled, `NOT_READY` again after a `Reload`.
  - In `crates/moa-daemon/tests/tmux.rs` (real tmux, so the 3.4 pass checks it),
    `start_commands_parse_as_tmux_prints_them`:
    - `new-window -d` running `/bin/sh -c 'sleep 30' x 'a b' "it's" '#c' $'one\ntwo'`;
    - `list-panes -a -F PANE_START` gives one line for that pane, and `split_start_command` →
      exactly that argv, the newline included.
  - In `crates/moa-core/tests/tmux_focus.rs`, `the_cockpit_is_the_window_named_moa`.
  - `tx_cmds_follow_the_commit` (core unit test):
    - an op that commits, then calls `tx.tmux(cmd)` → `[Persist, Publish…, Tmux{token: None}]`,
      and `check_order` passes;
    - with writes refused, or when the op returns `Err`, there's no `Tmux`.
  - `bindings_round_trip_tracked`: a binding with `tracked: false` survives `apply` and
    `load_snapshot`. Update the schema snapshot.
  - `start_drained_is_sent_after_the_drain` (`core_task` tests):
    - with a seeded spool, the recorder holds every drained `Hook` before the one
      `StartDrained`;
    - with no spool, `StartDrained` comes right after `Startup`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: pane listings, queued tmux commands and the tracked binding`.

## Task 5: process reads

**Files:**
- Create: `crates/moa-platform/src/proc.rs` (export it as `pub mod proc`)
- Modify: `crates/moa-platform/src/sys/macos.rs` (`proc_listchildpids`, `KERN_PROCARGS2`,
  `proc_pidinfo` start time), `crates/moa-platform/src/sys/linux.rs` (`/proc`)
- Create: `crates/moa-agents/src/claude/agent.rs` (`pane_agent`, `merge`, `project_slot`);
  re-export them from `claude/mod.rs`
- Create: `crates/moa-daemon/src/inspect.rs` (`pub mod inspect` in `lib.rs`); wire it into
  `Deps` in `run.rs`
- Modify: `crates/moa-testkit/src/lib.rs` (`fake_claude_named_claude`)
- Modify: `crates/moa-testkit/src/bin/fake-claude.rs` (R34: the synthetic transcript)
- Tests: `crates/moa-platform/tests/proc.rs`, `crates/moa-agents/tests/agent.rs`,
  `crates/moa-daemon/tests/inspect.rs`, `crates/moa-testkit/tests/fake_claude.rs`

**Interfaces:**
- Produces, in `moa_platform::proc`:
  - `pub struct ProcArgs { pub argv: Vec<String>, pub env: Vec<(String, String)> }`;
  - `pub const ENV_KEPT: [&str; 5] = ["CLAUDE_CONFIG_DIR", "MOA_SESSION", "MOA_BRAIN", "MOA_TITLE", "MOA_GROUP"];`
  - `pub struct Reader`: one buffer sized from `KERN_ARGMAX`, reused across calls (one per
    `inspect`, not about 1 MB per process);
  - `Reader::args(&mut self, pid: u32) -> Option<ProcArgs>`: argv and the `ENV_KEPT` entries only
    (lossy UTF-8). `None` when the process is gone or not ours;
  - `pub fn children(pid: u32) -> Vec<u32>`;
  - `pub fn started(pid: u32) -> Option<SystemTime>`;
  - `pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs>`. It is pure and built on both OSes,
    for tests. The layout is: `argc` (native-endian `i32`), the exec path, NUL padding, `argc`
    argv strings, then the env strings, each NUL-terminated.
- **macOS:**
  - `children` calls `proc_listchildpids(pid, buf, bytes)` through an `extern "C"` declaration
    in `sys/macos.rs` when `libc` lacks it (libproc is in libSystem). Its return value is a
    **count of pids**, not bytes; the buffer grows once if it comes back full;
  - `args` calls `sysctl([CTL_KERN, KERN_PROCARGS2, pid])` into the reader's buffer;
  - `started` reads `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec`/`pbi_start_tvusec`.
- **Linux:**
  - `args` reads `/proc/<pid>/cmdline` and `/proc/<pid>/environ`;
  - `children` reads `/proc/<pid>/task/*/children`, falling back to a scan of `/proc/*/stat`
    when those files are missing. The ppid is parsed from the text after the **last** `)`,
    since `comm` can hold spaces and parentheses;
  - `started` is `/proc/<pid>/stat`'s start time (field 22, after the last `)`), in clock ticks
    (`sysconf(_SC_CLK_TCK)`), plus `btime` from `/proc/stat`.
- Produces, in `moa_agents::claude`:
  - `pub fn pane_agent(argv: &[String], env: &[(String, String)]) -> Option<PaneAgent>` (R9). The
    env fills `moa_session`, `moa_brain`, `store_dir`, `title` and `group`, and the `K=V` words
    before the program are read as env too;
  - `pub fn merge(start: Option<PaneAgent>, process: Option<PaneAgent>) -> Option<PaneAgent>`
    (R9's order);
  - `pub fn project_slot(cwd: &Path) -> String`: every character that isn't ASCII alphanumeric
    becomes `-`.
- Produces: `pub fn inspect_pane(pid: i64, start: &[String]) -> Option<PaneAgent>` in
  `moa_daemon::inspect`.
  - It reads `args(pid)`, then each of `children(pid)`. The first process for which `pane_agent`
    finds Claude is the process view, with `started_at` from `started` of that process.
  - The start view is `pane_agent(start, &[])`.
  - It returns `merge(start_view, process_view)`.
  - `run.rs` sets `Deps { inspect: inspect_pane, .. }`.
- Produces: `pub fn fake_claude_named_claude(dir: &Path) -> PathBuf` in `moa_testkit`. It makes
  the symlink `<dir>/bin/claude` → `fake_claude()` and returns it.

- [ ] **Step 1: Write the failing tests:**
  - In `crates/moa-platform/tests/proc.rs`:
    - `args_reads_argv_and_only_the_kept_env`:
      - spawn `/bin/sh -c 'exec sleep 30' x` with `MOA_TITLE=t`, `SECRET=s`;
      - `wait_for` until `args(pid).argv[0]` is `sleep`;
      - then `argv` = `["sleep", "30"]`, and `env` holds `MOA_TITLE=t` and no `SECRET`;
      - kill it in a drop guard.
    - `children_lists_a_shells_child`: `/bin/sh -c 'sleep 30; :'` → `children(sh)` holds the
      `sleep` pid (`wait_for` until listed).
    - `started_is_close_to_the_spawn`: within a minute of the test's `SystemTime::now()`, a
      generous bound, not a timing bet.
    - `args_of_a_gone_pid_is_none`.
  - Unit tests:
    - `procargs2_layout_parses` on hand-built bytes (argc 2, padding, env), run on both OSes;
    - on Linux, `stat_ppid_reads_after_the_last_paren` with a `comm` of `a) (b`.
  - In `crates/moa-agents/tests/agent.rs`, `claude_is_recognised`, one case each:
    - `claude --resume X` → `X`;
    - `/opt/bin/claude --session-id=Y` → `Y`;
    - `node /x/claude-code/cli.js -r Z` → `Z`;
    - `env CLAUDE_CONFIG_DIR=/s claude` → `store_dir` `/s`;
    - `/usr/bin/env -u CLAUDE_CONFIG_DIR /opt/claude --session-id Q` → `Q`, with `store_dir`
      `None`;
    - `bash`, `vim`, `codex resume X` → `None`;
    - a `MOA_SESSION` env → `moa_session`.
  - In the same file, `merge_prefers_the_start_command_for_a_retitled_agent`:
    - process argv `["claude"]` with env `MOA_SESSION=m`;
    - start command `["/opt/claude", "--resume", "X"]`;
    - → conversation `X`, `moa_session` `m`.
  - Also `project_slot_replaces_non_alphanumerics`: `/Users/x/private/a.b` →
    `-Users-x-private-a-b`.
  - In `crates/moa-daemon/tests/inspect.rs`, `inspect_finds_claude_under_a_shell`:
    - spawn `/bin/sh -c '<fake_claude_named_claude> --session-id 0000…0001; :'` with
      `FAKE_CLAUDE_SCRIPT` pointing at a `[Hang]` script and `MOA_SESSION=x`;
    - `wait_for` until `inspect_pane(sh_pid, &[])` is `Some`;
    - it gives the conversation, `moa_session: Some("x")`, and a `started_at`;
    - it is killed in a drop guard.
  - In `crates/moa-testkit/tests/fake_claude.rs`, `a_session_start_writes_a_synthetic_transcript`:
    - after a `SessionStart`, the file
      `<CLAUDE_CONFIG_DIR>/projects/<project_slot(cwd)>/<id>.jsonl` exists;
    - and `--resume <id>` with no file creates it;
    - the hook payload's `transcript_path` is that same path;
    - with no `CLAUDE_CONFIG_DIR` and a `HOME` outside the temp dir, no file is written.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them (`unsafe` only in `moa-platform/src/sys`, each block with
  `// SAFETY:`). Run clippy for the Linux target.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit**: `v1: read a pane's agent in-process (libproc, sysctl, /proc)`.

## Task 6: unexpected ends mean asleep

**Files:**
- Modify: `crates/moa-core/src/hooks.rs`:
  - `apply`'s `SessionEnded` classes (R14);
  - `pane_gone`; rename `end_exited` to `lost_agent`, which puts the session to sleep.
- Modify: `crates/moa-core/src/ops/sessions.rs` (`bind`'s exited-before-the-answer path calls
  `lost_agent`)
- Modify: `crates/moa-core/tests/hooks.rs`, `crates/moa-core/tests/open.rs`,
  `crates/moa/tests/hooks_e2e.rs`
- Modify: `docs/superpowers/specs/2026-11-15-moa-rust-design.md` §6.7 (the lifecycle sentence)

**Interfaces:**
- Produces: `pub(crate) fn end_kind(reason: &str) -> EndKind`, where
  `enum EndKind { Rotating, Clean, Unexpected }`:
  - `clear` and `resume` → `Rotating`;
  - `prompt_input_exit` and `logout` → `Clean`;
  - anything else → `Unexpected`.
- Produces: `pub(crate) fn asleep(s: &Session, at: UtcMs) -> Session` in `hooks.rs`:
  `lifecycle = Ended`, `asleep_at = Some(at)`. Reconcile, sleep and adopt use it.
- Produces: `pub(crate) fn lost_agent(state: &mut State, tx: &mut Tx, id: SessionId, also: Vec<Change>) -> bool`:
  4a's `end_exited`, except a live session becomes `asleep(s, tx.wall)` with no activity row,
  and `activity_at` stays (R5). 4a moved it so that a late `SessionEnd` would be older. R14
  makes that unneeded: an unexpected end on an ended session is history only, and a clean end
  wins over an asleep in either order.

**`apply` for `SessionEnded`, by kind** (before `older`):
- `Rotating`: history only (4a).
- `Clean`:
  - the session is asleep → `Ended`, `asleep_at = None`, `activity_at = max(at, activity_at)`,
    and the `○ ended` row;
  - else older, or already ended → history only;
  - else `Ended` and the `○ ended` row (4a).
- `Unexpected`: older, or already ended → history only; else `asleep(s, at)`, no row.

**The spec amendment** (§6.7, "Rotation and lifecycle"). It replaces "Other SessionEnds set
`ended` unless the pane is still running the agent. A pane exit with no SessionEnd sets ended too
(lesson 3)." with:

> A clean quit (SessionEnd reason `prompt_input_exit` or `logout`) sets `ended`, whichever of it
> and the pane's exit arrives first. An unexpected end (any other reason, or a pane exit with no
> SessionEnd) puts the session to sleep (asleep, U7): moa can't tell a crash, a kill or a reboot
> from a choice, and both resume. Liveness still comes from pane events, never from a hook's
> last event (lesson 3).

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/hooks.rs`. Each "both orders"
  test runs the pane-exit notification and the hook in both orders, as two cases:
  - `an_unexpected_exit_is_asleep_whichever_message_comes_first`: `SessionEnded { reason: "other" }`
    plus `moa:hook pane-exited %5` → `Asleep`, binding deleted, no `○ ended` row.
  - `a_clean_quit_ends_whichever_message_comes_first`: `prompt_input_exit` plus the pane exit →
    `Ended` with `asleep_at = None`, and exactly one `○ ended` row.
  - `a_pane_exit_with_no_end_is_asleep` (4a's `a_pane_exit_ends_its_bound_session`, updated).
  - `a_session_end_after_sleep_keeps_it_asleep`: asleep S, then `SessionEnded { reason: "other" }`
    with a newer `at` → still `Asleep`, one `events` row, no activity row.
  - `logout_and_unknown_reasons`: `logout` → ended; `"bye"` → asleep.
  
  Updated:
  - `open.rs`: the exited-before-the-answer test expects asleep;
  - `hooks_e2e.rs`: `a_dying_agent_ends_its_session` becomes `a_dying_agent_sleeps` (`asleep`).
    `fake_agents_drive_every_display_state` tolerates a transient `asleep` just before the final
    `ended`, because `Exit`'s pane exit can beat its `SessionEnd`; it says so in a comment.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them, and amend the spec sentence.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: an unexpected end puts a session to sleep; a clean quit ends it`.

## Task 7: reconcile

**Files:**
- Create: `crates/moa-core/src/reconcile.rs`
- Modify: `crates/moa-core/src/core.rs`:
  - ask on `Connected`, or on `StartDrained`;
  - run on the answer;
  - `no_server` on `Down(NoServer)` (and on `StartDrained` after one);
  - ask again after a refused commit or a failed listing.
- Create: `crates/moa-core/tests/reconcile.rs`
- Modify: `crates/moa/tests/tmux_e2e.rs` (or a new `crates/moa/tests/reconcile_e2e.rs`),
  `crates/moa-daemon/tests/hooks.rs`

**Interfaces:**
- Consumes:
  - from Task 4: `parse_panes`, `list_panes`, `holds`, `Tag`, `Asked::Reconcile`,
    `TmuxState::{ask, answered, drained, last_down}`, `Tx::tmux`, `PaneBinding.tracked`,
    `Deps::inspect`, `State::{binding, spike_panes, store_of, account_of_store}`;
  - from Task 6: `hooks::asleep`;
  - from Task 5: the testkit's `fake_claude_named_claude`, in the e2e.
- Produces:
  - `pub(crate) fn run(state: &mut State, now: &Now, panes: &[PaneInfo]) -> Vec<Effect>`;
  - `pub(crate) fn no_server(state: &mut State, now: &Now) -> Vec<Effect>`;
  - `pub(crate) fn on_this_server(spike_tmux: Option<&str>, socket: &str) -> bool`: empty or
    `None` is true; otherwise the last `/` component of each must be equal;
  - `pub(crate) fn agent_runs(state: &State, id: SessionId, agent: Option<&PaneAgent>, cwd: Option<&str>) -> bool`
    (R31's test). It is true for a Claude agent that runs one of the session's conversations,
    or that names none when both of these hold:
    - `state.store_of(agent.store_dir.as_deref())` equals `state.store_of(conversation.store_dir.as_deref())`
      for the session's current conversation (R32: no variable means `<home>/.claude`, compared
      canonically);
    - when `cwd` is `Some` (reconcile and `ls` pass the listed folder), it is related to the
      session's cwd (the spike's `related`). A kill passes `None` (R31).
    Tasks 10, 11, 13 and 14 use it.

**`run`, in order** (one `Tx`, one commit):
1. **Bindings.** For each binding, keep it when `holds` (R4), against the listing and the link's
   generation. Delete the rest, and put the binding's session to sleep when it is live (R5).
   When two kept bindings name one pane, keep one (R4) and treat the others as deleted.
2. **Untracked kept bindings.** Re-inspect each one:
   `agent_runs(state, id, inspect(pid, &info.start).as_ref(), Some(&info.cwd))`. False → delete the binding, and
   put the session to sleep when it's live (R5, R31).
3. **Rebind** (R6) every live, non-terminal pane that no kept binding holds:
   - (a) its tag is `Session(id)` of a known session that is not bound and has no waiting
     launch;
   - else `inspect(pid, &info.start)`, then (b): `moa_session` parses to a known session, or
     `conversation` is a known conversation's, and that session isn't bound and has no waiting
     launch;
   - else (c), the session's spike candidate (that pane id, on this server).
   - A rebind writes a `PaneBinding`:
     - generation, socket, pane, pid, `bound_at = now`;
     - `tracked` = `!adopted && (tag set || moa_session.is_some() || spike candidate)`.
   - It also writes the session: live, `asleep_at = None`, location here.
   - After the commit, an untagged pane gets `set-option -p -t %N @moa_session <id>` through
     `tx.tmux`.
4. **Spike panes.** Delete every `spike_panes` row on this server. Keep the others.
5. **Live and unbound** sessions: put them to sleep, except R5's three.
6. **Commit.**
   - A refused commit, or an `Err(())` listing (with the warn `listing the tabs failed`), leaves
     `reconciled` false. The next input with writes accepted asks again.
   - On success, `reconciled` is set, and one info log is written:
     `reconciled the tabs: K kept, B bound, S put to sleep`.

**`no_server`**, only when `drained`: every binding is deleted, and step 5's rule is applied:
every live session goes to sleep, bound or not, except `Elsewhere` sessions, waiting launches,
and sessions whose spike pane names another server's socket. Spike rows on this server are
deleted, since no server means no panes. It's one commit and one log line. `reconciled` stays false, since there's no
connection. `StartDrained` runs it when `last_down` is `NoServer` and the link is down (R2).

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/reconcile.rs`.
  - **Setup:** `TypicalBrain`; three live sessions A, B, C with tracked bindings at generation
    `G1 = {pid: 100, start: 1}`, panes `%1`, `%2`, `%3`; `StartDrained`; `Connected` with a
    generation per test; then `listed` with the test's lines.
  - `a_new_generation_puts_live_sessions_to_sleep`:
    - `Connected(G2)`, then an empty listing;
    - A, B and C have `lifecycle Ended` and `asleep_at = now`, and `display_state` is `Asleep`;
    - all bindings are deleted, and there's no activity row;
    - the log says `0 kept, 0 bound, 3 put to sleep`.
  - `same_generation_keeps_live_panes_and_sleeps_vanished_ones`: `G1`, listing `%1` (tag A,
    alive) and `%2` (tag B, `pane_dead 1`) → A kept; B and C asleep.
  - `a_reused_pane_id_with_another_tag_is_not_kept`: `%1` tagged with C's id → A's binding is
    deleted and A asleep; C is bound to `%1` (rebind (a)).
  - `a_junk_tag_is_another_sessions`: `%1` tagged `junk` → A's binding is deleted.
  - `an_untagged_pane_running_a_known_conversation_is_bound`:
    - no binding for D (ended, by a lost open);
    - an untagged `%9` whose `inspect` (test fn) gives `conversation = D's`, `moa_session = D`;
    - → D live, bound, `tracked: true`;
    - a `set-option -p -t %9 @moa_session <D>` comes after the `Persist`.
  - `an_agent_without_moa_session_rebinds_untracked`: the same with `moa_session: None` →
    `tracked: false`.
  - `a_retitled_agent_is_found_by_its_start_command`: `inspect` is a test fn that honours
    `start`. Its process view has no conversation, and the start line holds `--resume D` → D is
    bound.
  - `an_adopted_agent_that_exits_in_its_shell_tab_shows_asleep`: an untracked binding on a live
    `%4` whose `inspect` gives `None` → binding deleted, session asleep.
  - `a_default_folder_adopted_agent_still_runs`:
    - an untracked binding whose session's conversation has `store_dir = <home>/.claude`;
    - its `inspect` gives a Claude with `store_dir: None` and no conversation, in the session's
      folder;
    - → the binding is kept and the session stays live (N1: no variable is the default folder).
  - `spike_panes_are_verified_or_cleared`:
    - E (live, no binding) has a spike pane `%4` with tmux `/tmp/tmux-501/<socket name>`, and
      `inspect` gives E's conversation → bound, tracked, and the spike row deleted;
    - F's spike pane runs another conversation → F asleep, and the row deleted;
    - G's spike pane is on `/tmp/tmux-501/moa-other` → G untouched (live, unbound), and the row
      kept;
    - H's runs no id, in a folder inside H's cwd → bound.
  - `elsewhere_and_waiting_sessions_are_left_alone`: a live `Elsewhere` session, and a session
    whose open is waiting, stay live.
  - `reconcile_waits_for_the_drain`: `Connected` before `StartDrained` → no query; after
    `StartDrained` → one query with both `list-panes`.
  - `a_refused_reconcile_is_asked_again`: writes refused → nothing applied, and `ready` is
    `NOT_READY`; the next input after `until` sends the query again.
  - `a_failed_listing_is_asked_again`.
  - `no_server_puts_live_sessions_to_sleep`: `Down(NoServer)` after `StartDrained` → A, B and C
    asleep, bindings deleted. An unbound live session with a spike pane on this server is
    asleep too; one on another server, and an `Elsewhere` one, stay live.
  - `no_server_before_the_drain_sleeps_sessions_once_drained`: `Down(NoServer)` first → nothing;
    then `StartDrained` → A, B and C asleep.
  - The proptest `reconcile_never_keeps_a_stale_binding`:
    - random bindings across two generations, random listings (alive, dead or missing, with
      random tags);
    - after `run`, every kept binding is of the current generation, its pane is listed alive and
      tagged with its session (or untagged), and no pane has two bindings.
  
  End to end:
  - `kill_server_brings_sessions_back_asleep`, after the shape of
    `kill_server_then_moa_gives_a_new_generation`:
    - open two fake sessions (`[Hook SessionStart, Hang]`); wait for `idle`;
    - `kill-server`; run `moa` again; `came_up`;
    - both rows show `display = asleep` (`wait_for`, 10 s), and no tab runs fake-claude
      (`list-panes -a -F '#{pane_current_command}'` has no `fake-claude`/`claude`).
  - `a_spooled_clean_exit_ends_the_session` (daemon, `crates/moa-daemon/tests/hooks.rs`):
    - a seeded live session with a binding at a generation no server has;
    - the spool holds its `SessionEnd { reason: "prompt_input_exit" }`;
    - boot with a sandbox link → the session is `ended` (not `asleep`), and its binding is gone.
    - The order itself (drain, then reconcile) is pinned by the core test
      `reconcile_waits_for_the_drain` and Task 4's `start_drained_is_sent_after_the_drain`: since
      R14, a clean end ends the session whichever runs first.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`. 4a's open,
  hooks and link tests must stay green (as updated in Task 6).
- [ ] **Step 5: Commit**: `v1: reconcile the tabs at every connection; sessions come back asleep`.

**PR b2 ends here.** Verify, review and open the PR as for b1. Its body says the spec's §6.7
lifecycle sentence was amended (Task 6, the human's ruling on U7).

**Try it (PR b2):** in your own terminal, with the sandbox `moa.toml` from slice 4a's try-it.
1. **Reset the sandbox brain first** (this PR changes the schema, and the sandbox is throwaway,
   R33): `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- shutdown`, then
   `rm $HOME/.local/state/moa-sandbox/.local/share/moa/default/moa.db*`. Your `moa.toml` stays.
2. `nix run . -- --sandbox`; in its shell,
   `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- open $HOME/tmp/moa-try`, and type
   something to the new session.
3. In another terminal, stand in for a reboot:
   `tmux -S $HOME/.local/state/moa-sandbox/.local/state/moa/run/tmux-default kill-server`.
4. `nix run . -- --sandbox` again. `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- ls`
   shows the session `asleep`, and no Claude is running. (Resuming it comes in the next PR.)
5. Also try a clean quit: open another session, type `/exit` in it → `ls` shows it `ended`.
6. If step 4 shows `ended`, Claude told moa it quit cleanly when it was killed. Tell me, with
   `MOA_ROOT=… nix run . -- sql "select kind, text from events order by id desc limit 3"`.

Restart needed: nothing; the spike is untouched.

## Task 8: launches and focus

**Files:**
- Create: `crates/moa-core/src/ops/launch.rs`. Move `open`, `opened`, `bind`, `opened_reply`,
  `end_open` and `lost_opens` here from `ops/sessions.rs` (the 4a ledger: "open could be
  ops/open.rs").
- Modify: `crates/moa-core/src/tmux/link.rs` (`Launching`, `LaunchKind`; the `opens` map holds
  them)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionFocus`; `bg` now used),
  `crates/moa-core/src/core.rs` (dispatch)
- Modify: `crates/moa-core/src/hooks.rs` (`rotated` uses Task 4's `store_of`)
- Modify: `crates/moa-proto/src/cli.rs` (`Verb::Focus` → `Op::SessionFocus`)
- Modify: `crates/moa-core/tests/open.rs`, `crates/moa-core/tests/hooks.rs`,
  `crates/moa/tests/hooks_e2e.rs`; create `crates/moa-core/tests/focus.rs`

**Interfaces:**
- Produces:
  - `pub(crate) enum LaunchKind { Open, Resume, Restart }`;
  - `pub(crate) struct Launching { pub reply: Option<(ConnId, ReqId)>, pub kind: LaunchKind, pub focus: bool, pub waiters: Vec<(ConnId, ReqId)>, pub launched_at: UtcMs }`.
  It goes with `TmuxState::wait_open(id, Launching)` and
  `TmuxState::launching(&mut self, id) -> Option<&mut Launching>`.
  `Settled { id, launching: Launching, end }` replaces Task 3's `Settled { id, reply, end }`.
- Produces, in `ops/launch.rs`:
  - `pub(crate) enum Conv<'a> { Fresh(&'a str), Resume(&'a str) }`;
  - `pub(crate) struct LaunchSpec<'a> { pub id: SessionId, pub title: &'a str, pub folder: &'a str, pub store: &'a Path, pub launcher: &'a Path, pub conv: Conv<'a>, pub prompt: Option<&'a str> }`;
  - `pub(crate) fn new_window(state: &State, conn: u64, spec: &LaunchSpec) -> Effect`, with
    token `Open(spec.id)`;
  - `pub(crate) enum ConfigDirMode { Set, UnsetForDefault }` and
    `pub(crate) const CONFIG_DIR_MODE: ConfigDirMode = ConfigDirMode::UnsetForDefault;` (R11);
  - `pub(crate) fn launch_env(store: &Path, home: &Path, mode: ConfigDirMode) -> (Vec<String>, Vec<String>)`:
    the `-e` arguments, and the words that go before the launcher (empty, or
    `["/usr/bin/env", "-u", "CLAUDE_CONFIG_DIR"]`). The store is compared with
    `<home>/.claude` after `deps.canonical`;
  - `pub(crate) fn focus_cmds(pane: PaneId) -> [Cmd; 2]`: `select-window -t %N`, then
    `select-pane -t %N`.
- Consumes: `State::{store_of, account_of_store}` (Task 4).
- Produces: `Op::SessionFocus { q: String }`, method `session.focus`, a write (it may resume).
  `METHODS` and `writes()` are updated.

**Behaviour:**
- **`open` with `bg: false`** (the default) sets `focus: true` on its `Launching`. When its pane
  is bound, `focus_cmds` go after the `@moa_session` tag. `--bg` changes only that.
- **The binding's `bound_at`** is `Launching.launched_at`, the time of the launch's commit (R13a,
  R18).
- **The answer to a launch**, by kind:
  - `Open`: `opened <title> with <account> in <path>` (unchanged);
  - `Resume`: `resumed <title> in <folder>`;
  - `Restart`: `restarted <title> under moa`;
  - each with `data: {"id", "title"}`.
  - Focus waiters get `went to <title>`.
- **A failed or gone launch:**
  - `Open` ends its session, as 4a did (`resume_failed` on a tmux error);
  - `Resume` and `Restart` put it back asleep with `asleep_at = now` and no `resume_failed`
    (R13a).
- **A bind whose commit is refused** (roadmap 12): the reply is
  `moa couldn't save which tab it runs in; it may show as untracked until moa restarts`, and
  the pane isn't tagged. Reconcile's rule (b) binds it at the next connection.
- **`focus Q`**, after `ready`:
  - running (a binding of this generation; after reconcile, every binding is) → `focus_cmds`,
    reply `went to <title>`. Nothing is committed here, so the commands go out as an
    `Effect::Tmux { token: None }` of their own, before the reply, not through `tx.tmux` (R15
    governs only commands that follow a commit). The same holds for resume's `Running` answer
    (Task 9) and the terminal's tags and focus (Task 10);
  - a waiting launch → push to its `waiters` and set `focus`; the reply comes when it's bound;
  - otherwise → resume it with `focus: true` (Task 9). Until Task 9 lands, this branch answers
    `resume isn't in this build yet`.
- **`ClientGone`** also drops a connection's focus waiters.
- **Hook rotation**: `rotated` sets `store_dir = Some(state.store_of(env.store_dir.as_deref()))`,
  so a session on the default store keeps a store after `/clear`.

- [ ] **Step 1: Write the failing tests:**
  - In `open.rs`:
    - `open_focuses_its_tab_unless_bg`: after the bind, the effects end with the tag, then
      `select-window -t %7`, then `select-pane -t %7`. With `bg: true` there's no `select-*`.
    - `a_default_store_opens_with_the_variable_unset`: acct `mine` with store `<home>/.claude` →
      no `-e CLAUDE_CONFIG_DIR`, and the command starts
      `/usr/bin/env -u CLAUDE_CONFIG_DIR /opt/claude --session-id`.
    - `a_refused_bind_commit_says_the_tab_may_be_untracked`: the exact text, and no tag.
    - `the_binding_is_stamped_with_the_launch_time`.
    - `open_sends_one_new_window_with_the_env` is unchanged (acct-a's store isn't the default).
      Existing open tests that assert a whole effect list either send `bg: true` or expect the
      two `select-*` commands; say which in the report.
  - In `launch.rs` unit tests, `launch_env_modes`:
    - `Set` with a `<home>/.claude` store → `["-e", "CLAUDE_CONFIG_DIR=<home>/.claude"]` and no
      prefix;
    - `UnsetForDefault` with the same store → no `-e CLAUDE_CONFIG_DIR` and the `env -u` prefix;
    - `UnsetForDefault` with `<home>/.claude-a` → set as usual.
  - In `hooks.rs`, `a_rotation_without_the_variable_records_the_default_store`.
  - In `crates/moa-core/tests/focus.rs`:
    - `focus_selects_a_running_sessions_tab`;
    - `focus_waits_for_a_launch_and_answers_when_bound`: the open waits, `focus` is sent → no
      reply yet; then `Replies` → both the open's and the focus's replies, plus `select-*`;
    - `focus_refuses_before_reconcile` (`NOT_READY`);
    - `a_closed_focus_waiter_is_dropped`.
  - In `crates/moa/tests/hooks_e2e.rs`, `Rig::open` passes `--bg`. A focused tab is on screen,
    so it is marked seen, and a turn's end would then show `idle`, not the `done` that
    `fake_agents_drive_every_display_state` waits for.
  - In `moa-proto` tests, `focus_parses_to_session_focus`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them. The move is mechanical: no behaviour changes beyond the
  above.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: one launch path, the default store unset; focus a session's tab`.

## Task 9: resume (C11)

**Files:**
- Modify: `crates/moa-core/src/ops/launch.rs` (`resume`, `account_of`, `resume_folder`)
- Modify: `crates/moa-core/src/tmux/link.rs` (`Asked::Resume`)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionResume`), `crates/moa-core/src/core.rs`
- Modify: `crates/moa-core/src/state.rs` (`Deps::transcript`, `Deps::is_dir`)
- Modify: `crates/moa-daemon/src/run.rs` (wire `find_transcript` and `Path::is_dir`)
- Modify: `crates/moa-proto/src/cli.rs` (`Verb::Resume` → `Op::SessionResume`)
- Create: `crates/moa-core/tests/resume.rs`; modify a `crates/moa/tests/` e2e file

**Interfaces:**
- Produces: `Op::SessionResume { q: String, bg: bool }`, method `session.resume`, a write.
- Produces:
  - `Deps::transcript: fn(&Path, &str) -> TranscriptCheck`, defaulting to
    `|_, _| Present(PathBuf::new())`;
  - `Deps::is_dir: fn(&Path) -> bool`, defaulting to `|_| true`.
  The daemon passes `moa_agents::claude::find_transcript` and `std::path::Path::is_dir`.
- Produces:
  - `pub(crate) struct Checked { pub id: SessionId, pub conv: String, pub folder: String, pub store: PathBuf, pub launcher: PathBuf, pub title: String }`;
  - `pub(crate) enum Resume { Running(SessionId, PaneId), Waiting(SessionId), Check(Checked) }`;
  - `pub(crate) struct ResumeAsk { pub conn: ConnId, pub req: ReqId, pub checked: Checked, pub kind: LaunchKind, pub focus: bool }`
    and `Asked::Resume(ResumeAsk)`;
  - `pub(crate) fn resume(state: &mut State, tx: &mut Tx, now: &Now, q: &str, kind: LaunchKind) -> Result<Resume, OpError>`:
    R13 steps 1–5. For `Check`, the caller asks `Asked::Resume(ResumeAsk { .. })` with
    `list_panes()`;
  - `pub(crate) fn resumed(state: &mut State, now: &Now, ask: ResumeAsk, panes: Result<Vec<PaneInfo>, ()>) -> Vec<Effect>`:
    - step 6 refuses when an unbound pane runs the conversation;
    - a failed listing goes on, with a warn log (the launch is still guarded by steps 1–5);
    - otherwise R13a's commit, then `new_window`.
  `kind` is `Resume`, or `Restart` from Task 14, which skips step 1's running check for its own
  binding.
- Produces:
  - `pub(crate) fn account_of(state: &State, conv: &Conversation) -> Result<(AccountName, AccountConfig), OpError>`
    (R32);
  - `pub(crate) fn resume_folder(s: &Session, conv: &Conversation) -> Option<String>` (R13 step 4,
    with a core copy of `project_slot`, kept equal to `moa_agents`' by a test).

**The refusals, exact text** (`<title>` is the title else the handle; `<a>` is an account name):
| Case | Text |
|---|---|
| elsewhere | `it's running in another app right now; close it there first` |
| spike pane on another server | `it's running in another moa terminal; use it there, or close it there first` |
| no conversation | `moa doesn't know which conversation it had; start it fresh with moactl open` |
| store isn't a listed account's | `the account its conversation was recorded in isn't in moa.toml; add it there to resume` |
| not Claude / no launcher | open's texts (`ONLY_CLAUDE`; `can't find claude for <a>: set its launcher in moa.toml`) |
| folder gone | `its folder <folder> is gone` |
| another brain | `that folder belongs to the <b> brain; resume it there` |
| route `NotAvailable` | `<a> can't run sessions in that folder` |
| transcript in another account | `its conversation is in <other>'s folder, not <a>'s; moa won't resume it under the wrong account` |
| transcript missing | `its conversation isn't in <a>'s folder any more, so it can't be resumed` |
| unsafe id | `that conversation's id isn't one moa can resume` |
| an untracked tab runs it | `it's already running in tab %N; adopt it with moactl adopt %N` |

**The launch:** `LaunchSpec` with:
- `Conv::Resume(current conversation id)`;
- `folder` per R13 step 4;
- `title` = the session's title, else the folder's last component, else `agent`;
- no prompt.

The commit is R13a's, and `focus` is `!bg`.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/resume.rs`.
  - **Setup:**
    - open's TOML, plus `acct-b` (store `<home>/.claude-b`) and `mine` (store `<home>/.claude`);
    - an asleep session S with conversation `C` in `<home>/.claude-a`, and `start_cwd` =
      `<home>/work/lantern`;
    - reconciled; each launch answered with an empty listing first.
  - `resume_sends_the_resume_window`:
    - the exact `new-window` text, with `-c` the start folder and
      `--resume C --name <title>`, under acct-a's launcher and env;
    - the commit sets live, `asleep_at` cleared and `resume_failed` false;
    - the reply (after `Replies`) is `resumed <title> in <folder>`, followed by the `select-*`
      commands.
  - `resume_uses_the_recorded_store_not_the_default_account`: `open_account = "acct-b"` → still
    acct-a's launcher and store.
  - `a_default_store_session_resumes`: a conversation with `store_dir = <home>/.claude` (and one
    with `store_dir` as `store_of(None)` gives it) → `mine`'s launcher, behind the `env -u`
    prefix.
  - `resume_of_a_running_session_only_focuses`: a binding of this generation → no query and no
    `new-window`; `went to <title>` with `select-*`; with `bg`, `<title> is already running` and
    no `select-*`.
  - `resume_refuses_when_an_untracked_tab_runs_it`: the listing holds an untagged `%8` whose
    `inspect` gives conversation `C` → the last row's text, no `Persist`, no `new-window`.
  - One test per other refusal row above, each with its exact text and no `Tmux` effect. The
    `transcript missing` and `transcript in another account` rows also assert a `Persist` with
    `resume_failed = true`; the others assert no `Persist`.
  - `resume_doubles_hashes_in_the_folder`: start folder `<home>/work/a#(b)` → `-c` is
    `<home>/work/a##(b)`.
  - `resume_picks_the_folder_the_transcript_belongs_to`: `cwd` = `<home>/work/other`, with
    `transcript_path` under `projects/<slot of cwd>/` → `-c` is `cwd`.
  - `a_resume_whose_tab_fails_stays_asleep`: `Replies` with an error → `Asleep`,
    `resume_failed` false.
  - `focus_resumes_a_sleeping_session`: `focus` on S → the resume window, then
    `went to <title>` once bound.
  - `resume_refuses_before_reconcile`.
  - `the_resumed_agents_start_is_its_own`: after resume and bind, a
    `SessionStarted { source: Resume }` with conversation `C` applies (not dropped by 4a's
    `is_own`).
  
  End to end, `a_session_asleep_after_kill_server_resumes` (extends Task 7's e2e):
  - after the `kill-server` and before `resume`, the test rewrites the folder's
    `.fake-claude.jsonl` to `[Hook SessionStart {source: "resume"}, Hang]` (fake-claude reads
    its script from its cwd at each launch). The transcript exists from the first run (R34);
  - `moactl resume <id>` → `resumed …`;
  - the row shows `idle`, `ls_row.conversation` is unchanged, and one tab runs it.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: resume under the account the conversation was recorded in (C11)`.

## Task 10: close, terminals, and the close action

**Files:**
- Modify: `crates/moa-core/src/ops/sessions.rs` (`close`), `crates/moa-core/src/ops/mod.rs`
  (dispatch)
- Modify: `crates/moa-core/src/ops/launch.rs` (`term`), `crates/moa-core/src/tmux/link.rs`
  (`Asked::Term`)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionClose`, `Op::TermOpen`),
  `crates/moa-core/src/core.rs`
- Modify: `crates/moa-proto/src/cli.rs`:
  - `Close` and `Term` map to ops;
  - `parse_action` no longer special-cases `close`;
  - `term`'s help reads "A plain shell in a new tab".
- Modify: `crates/moa/src/ctl.rs` (`term`'s folder must exist)
- Tests: `crates/moa-core/tests/close.rs`, `crates/moa-core/tests/suggestions.rs`,
  `crates/moa-proto/tests/cli.rs`, a `crates/moa/tests/` e2e file

**Interfaces:**
- Produces: `Op::SessionClose { q: String }`, method `session.close`, a write, run by
  `ops::run`.
- Produces: `Op::TermOpen { path: PathBuf, title: Option<String> }`, method `term.open`, not a
  write; handled in `core.rs`.
- Produces: `Asked::Term { conn: ConnId, req: ReqId, name: String, path: String }`. Its command
  is a `new-window` with `TmuxToken::Query(n)`:
  `-d -P -F OPENED_FORMAT -n <literal_format(name)> -c <literal_format(path)> -t =moa:`, and no
  command. An `Err(())` answers `couldn't open a terminal; see moa's log`.
- Produces: `pub(crate) fn kill_checked(state: &State, id: SessionId) -> Result<Option<PaneId>, OpError>`
  (R31):
  - `Ok(Some(pane))` to kill;
  - `Ok(None)` when not running;
  - `Err` with `<title>'s agent isn't running in that tab any more` when an untracked tab's
    agent is gone.
  It calls `reconcile::agent_runs(state, id, inspect(b.pane_pid, &[]).as_ref(), None)` for an
  untracked binding: a kill has no listing, so no folder (R31). The caller then puts the session
  to sleep and deletes the binding in its own commit. Tasks 11 and 14 use it.

**`close Q`** (`ops::sessions::close`):
1. `ready`; resolve `Q`.
2. A waiting launch → `it's still opening; try again in a moment`.
3. `kill_checked`. An `Err`: commit `hooks::asleep` and `DeletePaneBinding`, then return the
   `Err` text as a `Done`, so the commit stands. That one case answers success-shaped, since
   something was saved.
4. The session: `Ended`, `asleep_at = None`, `activity_at = now`, `DeletePaneBinding` when
   bound; through `tx.ui`, the text `closed <title>`.
5. After that commit, `tx.tmux(kill-pane -t %N)` for `Some(pane)` (R15).
6. The reply is `closed` (spike parity), with `data: {"id"}`. A session that isn't running is
   still marked ended, as the spike did.

**`term [PATH]`:**
- `ctl.rs`: a missing folder → `no folder <path>`.
- The core:
  - the link must be up (`NO_TERMINAL`); `ready` isn't needed, since a terminal binds nothing;
  - the name is `⌨ <title | folder name | ~>` cut to 24 characters.
- **On the answer:** one `Effect::Tmux { token: None }` (no commit, so not `tx.tmux`) with
  `set-option -p -t %N @moa_term 1`, `set-option -p -t %N @moa_label <literal_format(name)>`,
  then the `focus_cmds`. The reply is
  `opened a terminal in <path, home as ~> (tab <name>)`.
- **`ClientGone`** keeps a waiting `Asked::Term` with no reply target (R26): its window is
  already on its way and still gets its tags.

**The `close` action:** `parse_action` maps it through `to_op`, so `suggestion yes` runs it.
`ACTION_VERBS` is unchanged.

- [ ] **Step 1: Write the failing tests:**
  - In `close.rs`:
    - `close_kills_the_tab_after_the_commit`:
      `[Persist (Session Ended, DeletePaneBinding, ui rows), Publish…, Tmux kill-pane -t %1]`;
      `check_order` passes; the reply is `closed`.
    - `close_of_a_stopped_session_just_ends_it`: no `Tmux`.
    - `close_refuses_while_opening`.
    - `close_with_writes_refused_kills_nothing`: no `Tmux`, no `Persist`.
    - `close_of_an_adopted_session_whose_agent_left_kills_nothing`: an untracked binding,
      `inspect` gives `None` → session asleep, binding deleted, no `kill-pane`, and the R31
      text.
    - `the_agents_end_after_close_is_history_only` (R14).
  - `a_suggestion_can_close_a_session` (`suggestions.rs`): `suggest … --do "close <handle>"`,
    then `yes` → `Ended` and a `kill-pane`.
  - `term_opens_a_shell_tab_and_tags_it`:
    - the exact `new-window` with no command;
    - after `Replies`, the two `set-option`s, then the `select-*` commands and the reply text;
    - a title `a#[fg=red]b` → label `a##[fg=red]b`.
  - `close_and_term_parse` (`moa-proto`): `parse_action` gives `Action::Op(SessionClose)` for
    `close x`, and `moactl --help`'s `term` line says "tab".
  
  End to end, `close_and_term_end_to_end`:
  - open a fake session, then `moactl close <id>` → `ended` and its tab gone;
  - `moactl term <root>/work` → a tab named `⌨ work` with `@moa_term 1`, and `ls` doesn't list
    it.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: close a session, open a terminal, and let suggestions close`.

## Task 11: sleep

**Files:**
- Create: `crates/moa-core/src/sleep.rs`
- Modify: `crates/moa-core/src/config.rs` (`SleepAfter`, `sleep_after_hours`)
- Modify: `crates/moa-core/src/effect.rs` (`TimerKey::Sleep`), `crates/moa-core/src/core.rs`
  (the timer, and `sleep::run` at the end of `handle`, before `status::run`)
- Modify: `crates/moa-core/src/state.rs` (`sleep_at: Option<UtcMs>`, the armed deadline)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionSleep`), `crates/moa-core/src/ops/mod.rs`
- Modify: `crates/moa-proto/src/cli.rs` (`Verb::Sleep` → op)
- Tests: `crates/moa-core/tests/sleep.rs`, `crates/moa-core/tests/config.rs`,
  `crates/moa-daemon/tests/idle.rs`, a `crates/moa/tests/` e2e file

**Interfaces:**
- Produces:
  - `pub struct SleepAfter(pub i64)`, in ms, with `0` meaning never; `Default` = 21 600 000;
    `pub fn ms(self) -> Option<i64>`;
  - `BrainConfig.sleep_after: SleepAfter`;
  - the TOML key `sleep_after_hours` (integer or float, ≥ 0, finite). Anything else is
    `sleep_after_hours in [brains.<b>] must be a number of hours, 0 or more`.
- Produces: `TimerKey::Sleep`.
- Produces, in `moa_core::sleep`:
  - `pub(crate) fn why_not(state: &State, id: SessionId, now: UtcMs, after_ms: i64) -> Option<&'static str>`:
    `None` when eligible (R18), else a fixed reason (`"on screen"`, `"not idle"`,
    `"not tracked"`, `"never heard from"`, `"in the cockpit"`, …), which tests read;
  - `pub(crate) fn deadline(state: &State, now: UtcMs) -> Option<UtcMs>` (R19);
  - `pub(crate) fn run(state: &mut State, now: &Now, fx: &mut Vec<Effect>)`:
    - it does nothing when `sleep_after.ms()` is `None`, or no session is overdue;
    - otherwise it commits the overdue eligible sessions (R20's automatic wording);
    - it puts its `Persist` before the first heard effect, then adds the publishes, the kills
      (after its commit, R15) and `SetTimer`/`CancelTimer(Sleep)` when the deadline changed;
  - `pub(crate) fn put_to_sleep(state: &State, tx: &mut Tx, id: SessionId) -> Vec<Change>`:
    `hooks::asleep` and `DeletePaneBinding`. The caller commits, then queues
    `tx.tmux(kill-pane)` for the panes.
- Produces: `Op::SessionSleep { qs: Vec<String>, idle_ms: Option<i64>, dry_run: bool }`, method
  `session.sleep`, a write, run by `ops::run`.
  - `to_op` turns `--idle H` into `idle_ms`, and refuses a negative or non-finite H with
    `--idle needs a number of hours, 0 or more`.
  - No `qs` and no `--idle` → `moactl sleep ID... | moactl sleep --idle HOURS [--dry-run]`.
  - `sleep Q` uses `kill_checked` (Task 10) for each name.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/sleep.rs`.
  - **Setup:**
    - a reconciled link;
    - session S idle, with a tracked binding to `%1` (listed, tagged, not visible, not in the
      cockpit) at `bound_at = B`;
    - `last_seen_at = B + 1s` and `activity_at = T`;
    - the default threshold.
  - `sleep_eligibility_table`: one case per R18 condition, each flipping one thing from the
    eligible base, with `why_not` naming it:
    - visible (a client on `%1`'s window);
    - `last_turn_end_at > seen_at` (done);
    - a question in its why;
    - working, and quiet;
    - failed;
    - an open ask to it;
    - an open ask from it;
    - an untracked binding;
    - never heard from (`last_seen_at == bound_at`);
    - `%1`'s window is the cockpit;
    - a binding of an old generation;
    - not in the topology;
    - a waiting launch;
    - archived;
    - elsewhere;
    - `now < T + 6h`;
    - the link not reconciled.
  - `an_idle_session_sleeps_at_its_deadline`:
    - after the `Startup` and reconcile effects there's `SetTimer { Sleep, T + 6h }`;
    - `Timer(Sleep)` at `T + 6h` gives, in order:
      1. `Persist`: S `Ended`, `asleep_at = T + 6h`, binding deleted, and one brain activity
         row `💤 put 1 idle sessions to sleep: <title> · resume from the tree`;
      2. `Tmux kill-pane -t %1`;
      3. `CancelTimer(Sleep)`.
  - `an_overdue_session_on_screen_sleeps_when_it_leaves_the_screen`: at the deadline S is
    visible → no sleep, and no timer for S. A later `TmuxIn::Notification` that switches the
    client's window → S sleeps in that input's effects.
  - `zero_means_never`: `sleep_after_hours = 0` → no `SetTimer(Sleep)` and no sleep.
  - `sleep_with_writes_refused_kills_nothing`: at the deadline with writes refused → no
    `Persist`, and no `Tmux`.
  - `sleep_by_name`:
    - `sleep <S> <ended one>` → `asleep: <S title> (resumable: moactl resume ID)` and
      `<name> isn't running in moa (ended); nothing to put to sleep`;
    - an untracked running session whose agent is there can be slept by name;
    - one whose agent is gone (`inspect` gives `None`) gets R31's text, is put to sleep, and no
      `kill-pane` is sent.
  - `sleep_idle_dry_run`: `--idle 1 --dry-run` → `would sleep <handle>  <title>`, with no
    `Persist` and no `Tmux`; with nothing eligible → `nothing to put to sleep`.
  - `sleep_by_name_refuses_while_opening`.
  - The proptest `nothing_ineligible_ever_sleeps`:
    - random sessions over the R18 conditions, random `now`, and an input sequence of
      `Timer(Sleep)` and focus notifications;
    - the oracle is written in the test from R18's list (not `why_not`);
    - every session slept was eligible by the oracle at that input, and none was visible,
      untracked, never heard from, in the cockpit, or done.
  
  In `crates/moa-core/tests/config.rs`, `sleep_after_hours_parses`: absent → 6 h; `0.5` →
  1 800 000 ms; `0` → never; `-1` and `"x"` → the error line.
  
  In `crates/moa-daemon/tests/idle.rs`, the existing idle test stays green: no sessions → no
  `Sleep` timer.
  
  End to end, `an_idle_session_goes_to_sleep_on_its_own`:
  - `sleep_after_hours = 0.0003` (about one second) in the fake account's `moa.toml`;
  - `moactl open --bg` a fake session `[Hook SessionStart, Hang]`, so no user client is on its
    window;
  - its row shows `asleep` (`wait_for`, 10 s), its tab is gone, and the activity tail has the
    `💤` line.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: idle sessions go to sleep at a deadline; sleep verbs`.

**PR b3 ends here.** Verify, review and open the PR as before.

**Try it (PR b3):** after the b2 try-it (a session shows `asleep`):
1. `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- resume <its id prefix>` → it comes back
   in a new tab, and the tab is shown. Claude shows the same conversation, and doesn't ask you to
   log in again (the default Claude folder now runs without `CLAUDE_CONFIG_DIR`).
2. `… nix run . -- close <id>` → the tab goes, and `ls` shows it `ended`. `… resume <id>` brings
   it back again.
3. `… nix run . -- term ~/tmp` → a `⌨ tmp` tab with your shell.
4. **Sleep sooner, for the try:** add `sleep_after_hours = 0.05` (3 minutes) under
   `[brains.default]` in `$HOME/.local/state/moa-sandbox/.config/moa/moa.toml`, then
   `… nix run . -- shutdown` so the next start reads it. Open a session, ask it one thing, read
   the reply, and go back to moa's first tab (`Ctrl-a [`). About 3 minutes later, `… ls` shows
   it `asleep`, and its tab is gone. Put the setting back when you're done.

Restart needed: nothing; the spike is untouched.

## Task 12: adopt

**Files:**
- Create: `crates/moa-core/src/ops/adopt.rs`
- Modify: `crates/moa-core/src/tmux/link.rs` (`Asked::Adopt`)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionAdopt`), `crates/moa-core/src/core.rs`
- Modify: `crates/moa-core/src/model.rs` (`ConversationReason::Adopt` = `"adopt"`)
- Modify: `crates/moa-core/src/state.rs` (`Deps::newest_conversation`)
- Modify: `crates/moa-agents/src/claude/mod.rs` (`newest_conversation`)
- Modify: `crates/moa-daemon/src/run.rs` (wiring)
- Modify: `crates/moa-proto/src/cli.rs` (`Adopt` → op; help reads "Adopt an agent running in a
  tab")
- Tests: `crates/moa-core/tests/adopt.rs`; create `crates/moa-agents/tests/transcripts.rs`;
  `crates/moa-proto/tests/cli.rs`

**Interfaces:**
- Produces: `pub fn newest_conversation(store: &Path, cwd: &Path, skip: &[String], since: Option<UtcMs>) -> Option<String>`
  in `moa_agents::claude`: the newest `*.jsonl` (by mtime) in
  `<store>/projects/<project_slot(cwd)>/` whose stem is a safe id, not in `skip`, and modified at
  or after `since`.
- Produces: `Deps::newest_conversation: fn(&Path, &Path, &[String], Option<UtcMs>) -> Option<String>`,
  default `|_, _, _, _| None`.
- Produces:
  - `pub(crate) struct AdoptArgs { pub pane: PaneId, pub title: Option<String>, pub group: Option<String>, pub id: Option<String> }`;
  - `Asked::Adopt { conn, req, args: AdoptArgs }`. An `Err(())` answers
    `moa's terminal went away; try again`.
- Produces: `Op::SessionAdopt { pane: String, title: Option<String>, group: Option<String>, id: Option<String> }`
  (`session.adopt`, a write). A pane argument that isn't `%<digits>` →
  `adopt needs a tab id like %5`.
- Produces: `pub(crate) fn adopt(state: &mut State, tx: &mut Tx, now: &Now, args: &AdoptArgs, panes: &[PaneInfo]) -> Result<Done, OpError>`.

**The refusals, exact text:**
| Case | Text |
|---|---|
| pane not listed, or dead | `there's no tab <pane>` |
| a terminal | `that tab is a terminal, not an agent` |
| a binding `holds` it | `that tab is already <handle>` |
| not Claude | `that tab isn't running an agent moa recognizes` |
| folder | `<cwd> isn't in this brain's folders` |
| account | `moa doesn't know the account that agent runs under; add it to moa.toml` |
| another brain | `that folder belongs to the <b> brain; adopt it there` |
| no conversation | `no conversation it wrote yet; ask it one thing first, or pass --id` |
| held | `that conversation is already running in another tab (<handle>)` |

**The success text:** `adopted <conversation> (<account>) in <cwd> as <title or -> [<group or ->]`
(the spike's), with `data: {"id", "title"}`.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/adopt.rs`.
  - **Setup:**
    - reconciled;
    - `default`'s folders = `[<home>/private]`;
    - acct-a's store is `<home>/.claude-a`, and `mine`'s is `<home>/.claude`;
    - `inspect` is a test fn keyed by pid.
  - `adopt_binds_an_agent_typed_in_a_shell`:
    - `%5` (pid 50, cwd `<home>/private/lantern`) runs `claude --resume C9`, with
      `CLAUDE_CONFIG_DIR=<home>/.claude-a`;
    - result: a new session, title `lantern`, conversation `C9` with reason `adopt`, and a
      binding with `tracked: false`;
    - tags `@moa_session` and `@moa_adopted 1` after the commit; the activity row; the reply
      text.
  - `adopt_reads_no_variable_as_the_default_store`: the same with no `CLAUDE_CONFIG_DIR` →
    account `mine`.
  - `a_default_folder_adoption_stays_live_in_ls` (N1): adopt with no variable and no
    conversation id (the newest-transcript path), then `ls` with that pane still listed and the
    same `inspect` → the session is still live, not asleep, and nothing is committed.
  - `adopt_takes_the_newest_free_transcript_since_the_agent_started`: no id in the argv →
    `newest_conversation(store, cwd, held, started_at)` is asked, with `held` holding the
    conversations of kept bindings.
  - One test per refusal row, each with its exact text and no `Persist`.
  - `adopt_refuses_a_conversation_another_tab_runs`.
  - `adopt_frees_a_stale_binding_of_the_pane`: a same-generation binding names `%5` for X, and
    the listing tags `%5` with Y's id, so `holds` is false → X is asleep and its binding
    deleted, and the adopt goes on.
  - `an_adopted_session_shows_idle_and_never_sleeps_on_its_own`: the display is `Idle`; past the
    deadline `why_not` is `"not tracked"`.
  
  In `crates/moa-agents/tests/transcripts.rs`, `newest_conversation_skips_held_and_older_ones`:
  - write three files in order and set mtimes through `File::set_modified`;
  - one held, one before `since`, and the newest free one is chosen.
  
  In `moa-proto`, `adopt_parses`, and its help says "tab".
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: adopt an agent started outside moa`.

## Task 13: the untracked list in `ls`

**Files:**
- Modify: `crates/moa-core/src/ops/sessions.rs` (`ls` gains `untracked` and `tracked`)
- Modify: `crates/moa-core/src/tmux/link.rs` (`Asked::Ls`), `crates/moa-core/src/core.rs`
  (`SessionLs` is answered here when ready, else by `ops::run` as now)
- Tests: `crates/moa-core/tests/ls_untracked.rs`

**Interfaces:**
- Produces: `Asked::Ls { conn, req, all: bool }`. An `Err(())` answers the sessions without the
  untracked list.
- Produces: `pub(crate) fn untracked(state: &State, panes: &[PaneInfo]) -> Vec<(PaneInfo, PaneAgent, String)>`.
  The third item is the account label (R23).
- Produces: `pub(crate) fn ls_with(state: &mut State, now: &Now, all: bool, panes: &[PaneInfo]) -> (Vec<Effect>, Result<Done, OpError>)`.
  It runs R23's re-inspection of untracked bindings (a commit when one is gone), then builds the
  reply from `ls` plus the list.

- [ ] **Step 1: Write the failing tests:**
  - `ls_lists_untracked_tabs`:
    - with the link up and reconciled, `ls` waits for the listing;
    - then the text ends `UNTRACKED tab %6: acct-a in <home>/private/x  → moactl adopt %6`, and
      the JSON holds `{"untracked": "%6", "account": "acct-a", "cwd": …}`;
    - these are not listed: a pane with `MOA_SESSION` in its env, a terminal, a bound pane, a
      pane under another brain's folder, and a non-agent.
  - `adopted_rows_say_so`: `"tracked": false` and the
    `adopted: moactl restart <handle> tracks it` tail; tracked rows say `"tracked": true`.
  - `an_adopted_agent_that_left_its_tab_shows_asleep_in_ls`: the untracked binding's `inspect`
    gives `None` → the reply shows it `asleep`, after a `Persist` in the same effects.
  - `ls_without_a_link_answers_at_once`, and `ls_when_the_listing_fails_answers_without_the_list`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: list untracked agents in ls`.

## Task 14: restart under moa

**Files:**
- Modify: `crates/moa-core/src/ops/launch.rs` (`restart`)
- Modify: `crates/moa-core/src/op.rs` (`Op::SessionRestart`), `crates/moa-core/src/core.rs`
- Modify: `crates/moa-core/src/hooks.rs` (a `SessionEnded` for a session with a waiting
  `Restart` launch is history only, R24)
- Modify: `crates/moa-proto/src/cli.rs` (new `Verb::Restart { id }`; `VERB_NAMES`)
- Modify: `crates/moa-core/src/config.rs` (`RESERVED_BRAIN_NAMES` gains `restart`)
- Tests: `crates/moa-core/tests/restart.rs`, `crates/moa-proto/tests/cli.rs`,
  `crates/moa/src/main.rs` (the reserved-words test)

**Interfaces:**
- Consumes: `resume(…, LaunchKind::Restart)` and `resumed` (Task 9), `kill_checked` (Task 10).
- Produces: `Op::SessionRestart { q: String }`, method `session.restart`, a write.

**`restart Q`:**
1. `ready`; resolve `Q`.
2. A tracked binding → `<title> is already tracked; nothing to restart`.
3. No binding → `<title> isn't running; resume it with moactl resume`.
4. `kill_checked` (R31). An `Err` is answered like `close`'s (asleep, binding deleted, no kill).
5. Resume's checks (R13 steps 3–5).
6. One commit:
   - `DeletePaneBinding`;
   - the session stays live, with its launch waiting (`LaunchKind::Restart`, `launched_at` =
     now);
   - activity row `restarted <title> under moa`.
7. After the commit, `tx.tmux(kill-pane -t %N)` (R15), then the resume `new_window`, in that
   order.
8. The answer is Task 8's `restarted <title> under moa`.

- [ ] **Step 1: Write the failing tests** in `crates/moa-core/tests/restart.rs`:
  - `restart_kills_then_resumes`:
    - on an adopted, bound S: one `Persist` (with `DeletePaneBinding`), then `kill-pane -t %5`,
      then the resume `new-window` (`--resume C9`), in that order;
    - after `Replies` the new binding is `tracked: true`, and the reply is
      `restarted <title> under moa`.
  - `the_old_tabs_exit_changes_nothing`: `moa:hook pane-exited %5` before the `Replies`, and in
    another case after it → S stays live, there's no ended row, and the new binding stands.
  - `the_old_agents_end_while_restarting_is_history_only`: a `SessionEnded { reason: "other" }`
    for S, newer than the commit, while the `Restart` launch waits → S stays live, with one
    `events` row.
  - `restart_refuses_when_the_adopted_agent_is_gone`: `inspect` gives `None` → R31's text, S
    asleep, no `kill-pane`, no `new-window`.
  - `restart_with_writes_refused_kills_nothing`.
  - `restart_refusals`: tracked; not running.
  
  In `moa-proto`, `restart_parses`. In `main.rs`'s tests, `restart` is reserved.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit**: `v1: restart an adopted agent under moa`.

## Task 15: end to end, and the try-it

**Files:**
- Create: `crates/moa/tests/agents_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs` (`fake_shell_agent(tb, dir, conv)`: a `moactl term`
  tab where the test types `CLAUDE_CONFIG_DIR=<root>/.claude-a <fake claude named claude> --resume <conv>`
  through `common::tmux(tb, &["send-keys", …])`, so tmux stays behind the testkit as its guard
  test requires)

- [ ] **Step 1: Write the failing tests** (all waits through `wait_for`, 10 s; fake steps gated
  with `FakeStep::Wait`):
  - `an_agent_typed_in_a_shell_can_be_adopted_and_restarted`:
    - `moactl term <root>/work/b`; `fake_shell_agent` runs conversation `0000…0042` with a
      `[Hang]` script. Its transcript is written at start (R34);
    - `moactl ls` shows `UNTRACKED tab %N`;
    - `moactl adopt %N` → `adopted …`;
    - `ls` shows it with `tracked: false`;
    - the test rewrites the folder's script to `[Hook SessionStart {source: "resume"}, Hang]`;
    - `moactl restart <handle>` → the old tab's fake-claude is gone, and a new tab runs
      `--resume 0000…0042`;
    - its `SessionStart` makes the row `idle` with `tracked: true`.
  - `an_adopted_agent_that_exits_shows_asleep`: adopt as above, then end the typed fake-claude
    (send `C-c` to its tab) → `ls` shows it `asleep`, and the shell tab is still there.
  - `resume_close_sleep_adopt_launch_nothing_in_the_daemon`: `status --json`'s launch count is
    the same before and after a resume, a focus, a close, a term, a `sleep <id>`, an adopt and
    a restart.
  - `a_resume_in_the_wrong_account_is_refused_end_to_end`:
    - the conversation's transcript file exists only under acct-b's store (the test writes it,
      and deletes the one R34 wrote under acct-a);
    - the conversation is recorded under acct-a → the acct-b refusal text, and no tab opens.
- [ ] **Step 2: Run them to see them fail** (or pass where earlier tasks did the work; say which
  in the report).
- [ ] **Step 3: Fix what they find.** A failure here is a bug in an earlier task: fix it there,
  with its own test.
- [ ] **Step 4: Run** the full verify, three times, and the tmux-facing crates on tmux 3.4.
- [ ] **Step 5: Commit**: `v1: adopt and restart end to end with fake agents`.

**PR b4 ends here.** Verify, review and open the PR. Its body:
- carries the try-it below;
- says that restart's old and new agent can overlap for the old one's shutdown, and why that's
  harmless: moa sends the new agent no prompt, so it writes nothing meanwhile (R24).

## Try it (PR b4)

In your own terminal, from this checkout, with the sandbox from the earlier try-its. Your real
brains and the spike are untouched.

1. **Start an agent by hand:** `nix run . -- --sandbox`, then
   `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- term $HOME/tmp/moa-try`. In that tab,
   start Claude, and **ask it one thing first**, so it has written its conversation. moa's
   terminal starts without `CLAUDE_CONFIG_DIR`, so a plain `claude` uses `~/.claude`. If your
   Claude folder isn't `~/.claude`, start it as `CLAUDE_CONFIG_DIR=<your folder> claude`.
2. **See it listed:** back in moa's first tab (`Ctrl-a [`), `… ls` ends with
   `UNTRACKED tab %N: … → moactl adopt %N`.
3. **Adopt it:** run that line. `… ls` shows it, marked `adopted: moactl restart <id> tracks it`.
4. **Restart under moa:** `… restart <id>`. That terminal tab is replaced by one moa started, on
   the same conversation, and its states (working, done, needs you) now show in `ls`.

**What this checks for real:**
- macOS lets moa read a running Claude's arguments in-process, and the start command still
  shows its conversation when Claude renames itself;
- a Claude started without `CLAUDE_CONFIG_DIR` is matched to your default Claude folder;
- `--resume` brings back the right conversation.

**If `ls` never lists the untracked tab:** tell me your Claude Code version, and whether you
started it with `CLAUDE_CONFIG_DIR` set.

Restart needed: nothing; the spike is untouched.

## Carried items: where each lands

| Item (source) | Where |
|---|---|
| Reap a stuck control client (roadmap 11) | Task 1 |
| A link panic reaches the core late (11) | Task 1 |
| `lost()` marks Idle before probing (11) | Task 1 |
| A stop with no live client leaves hooks naming a dead client (11) | **Carried to slice 8** (front door): the daemon has no client to run anything with, and the next connect replaces them. The front door can unhook when it finds no daemon |
| Backoff restarts at step 0 (11) | Task 1 |
| Bounds: hung server, bootstrap/clean-up, Protocol wording (11) | Task 1 |
| `ensure` answers before the first refresh (11) | R3, R10 (Task 4): nothing that needs the panes trusts them before reconcile |
| Ensure waiters of a closed connection (11) | Task 3 (R26) |
| `answer()` empties waiters before a panic (11) | Accepted (R26) |
| `from_lists` panic loop (11) | Task 3 (R27) |
| Reaper reuse window (11) | Task 2 (R28) |
| `run.rs` makes the brain dir itself (11) | Task 2 |
| Notices are held views (11) | **Carried**: notices still read no state. Revisit when one does |
| `detach-on-destroy off` (11) | Task 2 |
| `_moa`'s shell through the login shell (11) | Task 2 |
| No pill glyph for failed (11) | Task 2 |
| Prompts: `\$`, `command-prompt -b`, the template `Cmd` (11) | **Carried to slice 5**: 4b sends no `command-prompt` |
| `a_server_that_detaches…` panic text (11) | Task 1 |
| Front door too-old `kill-server` test (11) | **Carried**: needs a real tmux below 3.4 |
| `after_hook` test can't tell skipped from never sent (11) | Task 3 |
| Clock e2e has no negative case (11) | **Carried**: the core test `the_clock_runs_only_while_someone_is_attached` covers it, and an e2e negative needs a minute to pass |
| User-config e2e checks only `[5]` (11) | Task 3 |
| Front door duplication: the brain-name parse, and `make_moa_session`/`create`'s shared spawn code (11) | **Carried to slice 8** with the other front door items. Task 2 touches only the argv texts |
| The other small cleanups (11): the T23 guard, `new-session` on the allow-list, `session_named`, `Sandbox::tmux`'s PATH lookup, `write_if_changed`'s mode, `XDG_CONFIG_HOME`'s trailing `/`, `MAX_BLOCK`, the parse-error text, the front door's "no tmux" line | **Carried** to the slice that next touches each file; none is in 4b's files |
| Reconcile ends live sessions with no binding, or a vanished pane (12, 4a note) | Task 7 (R5) |
| Resume's `-c` through `literal_format` (12) | Tasks 8–9 (R11) |
| `CLAUDE_CONFIG_DIR` for `~/.claude`: unset, never omit (12) | Task 8 (R11): `UnsetForDefault`, by the human's ruling, with R32's store rule |
| A relaunch uses `--resume` (12) | Task 9 (R13), test `the_resumed_agents_start_is_its_own` |
| A failed bind commit replies "nothing was changed" (12) | Task 8 |
| `moactl open` waits forever if the store stops after its commit (12) | **Carried to slice 9** (the client's call timeout comes with re-exec and upgrades); resume shares it |
| A drain while up keeps `.draining` after a failed commit (12) | **Carried**: late, not lost; outside 4b's code |
| Replayed hooks after a failed commit have no retry limit (12) | **Carried**: outside 4b's code |
| `moactl open` makes the folder before picking the brain (12) | **Done by #46** |
| Bindings outlive ended sessions; duplicate bindings (4a ledger T5, T10) | Task 7 (R4) |
| Down after the tab exists ends the session while its agent runs (4a ledger T9) | Task 7 (rule (b) rebinds it) |
| A pane exit while connecting is dropped (4a ledger) | Task 7 (reconcile at the next connection) |
| Core ignores `bg`; open could be its own file (4a ledger T9) | Task 8 |
| `project_for` canonicalizes per open (4a ledger) | **Carried to slice 8**, item 9 (adopt uses it as is) |
| `load_config` runs `which(claude)` per `moactl` call (4a ledger) | **Carried**: outside 4b's code |
| A nested `claude --resume` passes `is_own` (4a final review) | **Carried**: it can't be told from a real `/resume`; adopt and reconcile don't change it |
| Restart's old and new agent can overlap for the old one's shutdown (review M4) | **Accepted**, R24: moa sends the new agent no prompt, so it writes nothing to the transcript while the old one shuts down; waiting for the old process (`NOTE_EXIT`, `pidfd`) isn't worth the machinery. The PR says so |
