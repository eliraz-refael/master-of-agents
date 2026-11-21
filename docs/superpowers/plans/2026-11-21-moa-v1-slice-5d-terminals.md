# moa v1 slice 5d: terminals

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every terminal opened inside moa is recorded: who opened it, its folder, the session it
belongs to, when it opened, when you last touched it, and what's running in it. A terminals view
(`Ctrl-a T`) lists them all, with the ones untouched for 2 hours marked. You can close, name,
add a note to, or put a timeout on any of them, from the view or with `moa term …`.

**Architecture:**
- **A `terminals` table** in the brain, one row per terminal opened, kept after the terminal
  closes as history. Open rows live in `State.terminals`.
- **Registration runs from the listing moa already makes.** It doesn't run on a timer. The
  listing's fourth query lists every pane with its current program, the default shell and its
  folder. `moa_core::terminals::listed` (new) diffs that against the open rows: new shells are
  registered, gone ones closed, and changed programs and folders updated.
- **Last touched** follows `tmux::focus::mark_seen`'s pattern. At the end of every tmux input,
  the terminal each attached user is looking at gets stamped. A row is written only when a
  terminal stops or starts being looked at.
- **Close** sends `Ctrl-c` (a new, strictly shaped `send-keys`), waits 3 seconds, then
  `kill-pane`. **Timeouts** and those 3-second waits share one wall-clock timer,
  `TimerKey::Terminals`, built like sleep's.
- **The view** is a popup like the picker (`moa panel terms --once`). It reuses the picker's row
  layout, `Panel::confirm`, the `Ask` prompts, and `text::ago`.

```
 moa term / Ctrl-a $ / Ctrl-a | - / plain tmux keys
        └─► tmux listing (events only) ─► terminals::listed ─► Change::Terminal ─► terms topic ─► view / term.ls
 attached client looks at %N ─► terminals::touched (end of each tmux input) ─► touched_at
 term.close %N ─► running? send-keys -t %N C-c, closing[%N]=now+3s ─► TimerKey::Terminals ─► kill-pane -t %N
 term.timeout %N 3h ─► timeout_at ─► TimerKey::Terminals ─► close, said by moa
```

**Tech Stack:** Rust; the existing crates. `jiff` (already a workspace dependency) parses
durations like `3h` or `90m`. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-21-moa-slice-5d-terminals-design.md`, which the user
approved. It builds on the main spec `docs/superpowers/specs/2026-11-15-moa-rust-design.md`
("Plain terminals": "a shell window `⌨ name` in "here", not a session in the brain, listed by
the picker"; §6.5 store; §6.8 deadlines derived from data, no timers table).

Roadmap: "Slice 5d: terminals". Backlog: `docs/backlog.md`, "Terminals".

**Builds on** 5c and before. Each item below is named so tasks reuse it and never copy it:
- **Store:**
  - one migration, `crates/moa-store/src/sql/0001_v1.sql`, edited in place (roadmap P6;
    there's no work brain yet);
  - the `pane_bindings` table and its rows code as the template (`rows.rs` `PANE_BINDINGS`,
    `pane_binding()`, the `Change::PaneBinding` arm, `upsert`), and `asks` as the
    integer-id template (`ASKS`, `Table::Ask`, `NextIds`).
- **Model:** `moa_core::model` (`int_ids!`, `str_enum!`, `PaneBinding`),
  `moa_core::change::Change` (`topics`, `row`), and `moa_core::state` (`Snapshot`, `NextIds`,
  `Table`, `load`, `apply`, `next_id`).
- **tmux:**
  - `tmux/topology.rs`: `refresh_cmds`, `LIST_TERMS`, `terms_listed`, `term_label`,
    `term_name`, `terms`, `is_live_term`, `moa_session`, `view_of`, `window_of`, `panes_in`,
    `client_pane`, `visible`, `session_name_of`, and the `HIDDEN` constant;
  - `TmuxState::term_sent`, `set_pane_option`, `unset_pane_option`;
  - `tmux/cmd.rs`: `ALLOWED`, `Cmd::check`, `literal_format`, `one_line`, `is_pane_id`.
- **Core:**
  - `core.rs::handle`, with `sleep::run` at its end, `mark_seen_into`, and the `link_listed`
    branch;
  - `sleep.rs`: `deadline`, `run`, `kill_pane`;
  - `tx.ui`, `tx.ui_by(.., ActivityWho::Brain)`, `Tx::tmux`, `finish_op`;
  - `link::ready`, `ops::term::{Tell, told, term_opened, show, with_tilde, TERM_GONE}`,
    `ops::launch::{focus_cmds, title_of}`, `folders::tilde`.
- **Config:** `config.rs`: `UiConfig`, `RawUi`, and `SleepAfter::parse`, the one hours parser.
- **CLI:** `crates/moa-proto/src/cli.rs`: `Verb::Term`, with `Verb::Layout` as the
  sub-command shape, `to_op`, and `VERB_NAMES`; `crates/moa/src/ctl.rs`.
- **Views:**
  - `moa_view`: `views::build`, `views/picker.rs` (`row_line`, `fit`, `fit_right`, `cut_spans`,
    `term_title`), `Item::Term`, `Panel::{confirm, ask, say, replied}`, `panel/ask.rs` `Ask`,
    `keys.rs` (`Action`, `rows`, `actions`, `hint`), `views/help.rs` `VIEWS`, and
    `text::{ago, next_ago_change, ellipsize, short_path, fit, fit_right, width}`;
  - `moa_core::panel::View`, `keymap::{Binding, Does, ctrl_a}`, `conf.rs`
    (`Popups::command`, `menu_command`, `moa_tmux`, `moa.tmux.tpl`),
    `moa_tui::run::topics`, and the `--once` gate in `crates/moa/src/panel.rs`.
- **Tests:**
  - core: `crates/moa-core/tests/common/mod.rs` and `tests/term.rs` (`lists`, `refreshed`,
    `listed_with`, `terms`), `tests/sleep.rs` (`at`, `World`/`Rig`, `sleep_timers`), and
    `tests/tmux_vocab.rs`;
  - real tmux without a daemon: `crates/moa-daemon/tests/tmux.rs`
    `the_terminals_query_lists_only_terminals`;
  - e2e: `crates/moa/tests/common/mod.rs` (`ctrl_a`, `panes_of`, `moactl_ok`, `popup_closes`,
    `screen_of`, `popups`), and the `Rig` helpers local to `picker_e2e.rs:30-225`.

The full map with line numbers is at
`/private/tmp/claude-501/-Users-elirazk-private-master-of-agents/48ca7e5d-bd75-4449-ad3d-18cf4c8865a1/scratchpad/recon-5d.md`.
Briefs quote what a task needs from it.

## Calls for you to check

The most important come first.

1. **What counts as a terminal.** Any pane in moa's tmux that isn't an agent, a panel, the agent
   spot, or a pane moa is in the middle of opening. That covers:
   - `moa term` and `Ctrl-a $` tabs;
   - the shells from `Ctrl-a |` and `-` (today these aren't even marked as terminals, so the
     picker misses them);
   - anything opened with plain tmux keys.

   If you start an agent in a terminal and moa adopts it, it stops being a terminal: its record
   closes, and moa never closes it as one. It keeps its terminal mark, which is what stops moa
   from ever putting your shell to sleep (T3).
2. **"Who opened it" has three values: `you`, `moa` and `found`.** In 5d everything you open is
   `you`, including `moa term` typed in a shell. `found` means it was already open when moa
   first looked: at startup, or when 5d first runs. `moa` is kept for slice 6, when moa itself
   opens terminals at your request (T3).
3. **Closing:**
   - moa checks what's running at the moment you close, because tmux doesn't tell moa when a
     program starts;
   - a terminal that's only at its shell prompt closes at once;
   - one running a program first gets `Ctrl-c`, then closes 3 seconds later, whether the
     program stopped or not;
   - in the view, `x` always asks first: `close <name>? y/N`, or
     `close <name>, running <program>? y/N` (T8, T9).
4. **"Running a program" means** the terminal's current program isn't the shell it started
   with. So a `bash` started inside a `fish` terminal counts as running (T2).
5. **A timeout is set as a duration** (`3h`, `90m`, `2h30m`) and cleared with `off`. When it
   runs out, moa closes the terminal as in call 3, and the activity log says
   `moa closed <name>: its timeout ran out` (T8).
6. **`moa term` keeps `moa term [FOLDER]`.** The new words are `moa term ls`, `close`, `name`,
   `note` and `timeout`. A folder actually named `ls` is opened with `moa term ./ls`. A
   terminal is named by its number from `moa term ls` (`7`), its tmux id (`%12`), or its exact
   name (T5).
7. **The terminals view is a popup on `Ctrl-a T`**, and it's in the menu as `Terminals…`.
   - **Rows:** name, folder, what's running, when you last touched it, how long it's been
     open, a timeout if set, and the note.
   - **Order:** most recently touched first. The one you're on reads `now`.
   - **Idle:** terminals untouched for 2 hours or more are dim and say `idle`.
   - **Keys:** Enter jumps, `x` closes, `r` names, `n` adds a note, `t` sets a timeout.
   - **No search box:** the picker (`Ctrl-a /`) already finds terminals by name, and letters
     here are commands (T9).
8. **The 2-hour mark is `terminal_idle_hours` under `[ui]` in `moa.toml`.** It defaults to 2, and
   `0` turns the mark off (T9).
9. **Naming a terminal renames its tab too,** but only when the terminal is alone in its tab.
   A split beside an agent never renames the agent's tab (T5).

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged, and gets:
- the full verify: `cargo fmt --check`; clippy, also with `--target x86_64-unknown-linux-gnu`
  for `moa-platform` and `moa-tmux`; `cargo nextest run --workspace` three times; and
  `nix flake check`;
- the tmux 3.4 pass on the tmux-facing crates it touched;
- `/code-review medium --fix`, with the fixes verified and pushed;
- green CI.

Bodies end with the try-it, when there is one. No "Restart needed" line.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-5d-1` | 1–3 | `v1 slice 5d (1): every terminal recorded` |
| p2 | `v1/slice-5d-2` | 4–6 | `v1 slice 5d (2): last touched, and moa term ls, name, note` |
| p3 | `v1/slice-5d-3` | 7–8 | `v1 slice 5d (3): closing terminals, and timeouts` |
| p4 | `v1/slice-5d-4` | 9–10 | `v1 slice 5d (4): the terminals view, end to end` |

p1 also carries this plan and the design doc.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| T1 | **The `terminals` table** (edit `0001_v1.sql` in place, per P6): `id INTEGER PRIMARY KEY, server_pid INTEGER NOT NULL, server_start INTEGER NOT NULL, pane_id TEXT NOT NULL, opened_by TEXT NOT NULL, cwd TEXT NOT NULL, session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL, opened_at INTEGER NOT NULL, touched_at INTEGER, name TEXT NOT NULL, note TEXT, timeout_at INTEGER, shell TEXT NOT NULL, command TEXT NOT NULL, command_since INTEGER NOT NULL, closed_at INTEGER`, `STRICT`, with `CREATE UNIQUE INDEX terminals_open ON terminals(server_pid, server_start, pane_id) WHERE closed_at IS NULL`, so the store itself refuses two open records for one pane. The model gets a `TermId` (`int_ids!`), `OpenedBy` (`str_enum!`: `you`, `moa`, `found`), and `Terminal`, a struct with those fields, where `session_id` is `Option<SessionId>` and the times are `UtcMs`. It also gets `Change::Terminal(Terminal)`, which upserts by `id`, with topics `[Topic::Terms]` and row `("terminals", "#<id>")`, and `Table::Terminal` in `NextIds`. `Store::load_snapshot` loads only open rows (`closed_at IS NULL`); `Src::rows` gains an optional `filter: Option<&str>` (a WHERE clause written in code, never from data) for that. `next_ids` reads `max(id)` over all rows. Export: `NOT_EXPORTED` gains `("terminals", "all", "terminals are not exported")`. | It copies `pane_bindings` (tmux reuses `%N` across servers, so the generation is part of the key) and `asks` (an integer id, so a terminal's history row stays after it closes). Loading open rows only keeps memory flat as history grows. |
| T2 | **The fourth refresh query lists every pane, tab-separated:** `T\t#{pane_id}\t#{pane_pid}\t#{@moa_term}\t#{pane_current_command}\t#{pane_current_path}\t#{@moa_label}`, with no `-f`. The label is last and the line is split with `splitn(7, '\t')`. A folder holding a tab or a newline corrupts its own line and is accepted as rare. A pane listed twice (a window linked into two sessions: only sheets do that) keeps no pane fields, the same rule `terms_listed` applies. `Topology` keeps, per pane, `pid`, `command` and `cwd`, beside the existing term label, and says whether this list parsed at all (`panes_listed: bool`); `rebuild` already keeps the other lists when this one fails.<br>**The shell's name** is read once, when a terminal is registered (T3): `Deps.process_name: fn(u32) -> Option<String>` (new) returns `#{pane_pid}`'s name the way tmux's own `pane_current_command` reads a name on that OS (macOS: `proc_pidinfo` `PROC_PIDTBSDINFO`'s name; Linux: argv0 from `/proc/<pid>/cmdline`), with a leading `-` dropped and the base name taken. It's stored as the record's `shell`.<br>**Running** = the listed `command` isn't the record's `shell`. It's display only: tmux sends nothing when a program starts or stops in a pane (checked on 3.4 and 3.8), so a listing can be stale. **Closing never trusts it** (T8). | One listing gives registration everything it needs, so nothing polls. Comparing with `#{b:default-shell}` fails on macOS, where `/bin/sh` (the test shell) runs as `bash` (plan review I1); the shell process's own name is what `pane_current_command` reports when nothing else runs. A nix `sleep` lists as `coreutils`, so no test asserts a program's name, only running or not. |
| T3 | **Registration**, `moa_core::terminals::listed(state, now, first: bool) -> Vec<Change>`. It runs only once the link is **reconciled** (`state.tmux().reconciled()`), because a `@moa_term` tag changes how reconcile and adopt bind an agent (`reconcile.rs:354,374`, `adopt.rs:209`). It's called:<br>&nbsp;&nbsp;– in `core.rs`'s `link_listed` branch, next to `panel::listed`, when reconciled;<br>&nbsp;&nbsp;– once more, against the topology already in memory, when this connection's reconcile finishes. That's the run with `first = true`, and it needs no new query.<br>It skips any listing whose pane list didn't parse (`panes_listed == false`).<br>• **A terminal pane:** in a session other than `_moa`, with no `@moa_session`, no `@moa_view`, not `state.panel_in`, not dead, and not bound to a session. That's `terminals::is_terminal_pane`, the one place this rule lives.<br>• **Registering:** a terminal pane with no open record (matched by generation and `pane_id`) gets one. `opened_by` is `found` if and only if `first` is true, and `you` otherwise. `cwd` is the listed folder. `session_id` is the session bound to another pane in the same tab, if exactly one. `name` is `term_name` of its `@moa_label`, or `shell`. `shell` is `Deps.process_name(pid)`, or the listed `command` when that read fails. `command` and `command_since` are the listed program and `now`. A pane without `@moa_term` is tagged `@moa_term 1` and `@moa_label ⌨ shell` (through `set_pane_option` and `term_sent`), so the picker sees it. As a result, `moa adopt %N` on such a pane with no agent says "that tab is a terminal, not an agent", which is expected.<br>• **Not yet:** while any launch waits (`state.tmux().waiting_opens().next().is_some()`), or an `Asked::Term`, `Asked::Spawn` or `Asked::Adopt` query is out, **new untagged** panes aren't registered from this listing; the next listing does it. tmux answers in order, and a launched agent is tagged by `opened` before a later listing shows it.<br>• **Updating:** a changed program sets `command` and `command_since = now`. A changed folder sets `cwd`.<br>• **Closing a record:** an open record whose pane isn't listed, or is no longer a terminal pane (bound to a session now, for example), gets `closed_at = now`, and `timeout_at` is cleared. A new server generation closes every open record of the old one. **A pane that became an agent keeps its `@moa_term`:** that tag is what keeps sleep from killing your shell under an adopted agent (`sleep.rs:56,93`).<br>• **`term_opened(state, now, …)`** commits its record through a `Tx` (`opened_by = you`, `name` = the name it was given, uncut, without `⌨ `). The tags, focus and reply go out after the commit's effects, and still go out if the commit was refused, as today. The next listing then finds the record.<br>• **The `terms` topic** is still built from `topology.terms()`, so the picker never loses a terminal because a commit failed. Each row is enriched from the open record when there is one, and its `name` is the record's name when there is one, else the label. | One place decides what's a terminal, from data moa already has. `found` is honest about terminals moa didn't see open. `moa` waits for slice 6 (see call 2). |
| T4 | **The splits are marked:** `Ctrl-a |` and `-`, in `moa.tmux.tpl` and in `conf.rs::menu_command`'s `split` closure, add `; set -p @moa_term 1` after `@moa_label`. **A drift test**, carried from 5c, loads the generated `moa.tmux` into a throwaway tmux on 3.4 and 3.8. It checks with `list-keys -T moa` that the `|`, `-`, `m`, `z`, `[`, `]` and `a` bindings run what `keymap::ctrl_a`'s `Does` says, and that the menu's copies match the key bindings. | Splits were invisible to moa. The two literal copies are where drift would hide. |
| T5 | **`term.*` ops:**<br>• **The target** is `"7"` or `"#7"` (a record id), `"%12"` (a pane), or an exact name, case-insensitive. A lookup that finds nothing says `no terminal <t>`. Two names that match say `2 terminals are named <t>; use its number`. The resolver is `terminals::find(state, target: &str) -> Result<&Terminal, OpError>`, the one lookup every `term.*` op uses.<br>• **`term.ls`** (writes nothing) first asks one fresh listing (`state.tmux.refresh()`) and answers when it lands, in the `ops::sessions::start_ls`/`Asked::Ls` shape, so "what's running" is current when you ask. That's one listing per request, not per terminal. It replies with the rows as `data`, and as text one line per terminal: `#<id>  <name>  <~folder>  <program> (<since ago>)` or `shell`, then `touched <ago>  open <ago>  [idle]  [closes in <until>]  [— <note>]`. The idle mark uses `terminal_idle_hours`. With none open, it says `no terminals open`. With the link down, it answers from the records and starts with `moa isn't connected to tmux right now; this list may be out of date`.<br>• **`term.name {target, name}`**: `one_line`, trimmed, 1–40 characters (`a terminal's name is 1 to 40 characters`). It sets the record's `name` and the pane's `@moa_label` (`⌨ <name>`, through `literal_format`). When the pane is alone in its tab, it also renames the tab: `rename-window -t %N -- ⌨ <name>`, cut to `WINDOW_NAME_CUT`, exactly as `ops/term.rs:166-170` names a new terminal's tab. Reply: `named it <name>`.<br>• **`term.note {target, note}`**: trimmed, and an empty note clears it. At most 200 characters, cut with `…`. Reply: `noted` or `note cleared`.<br>• **Idle:** `[ui] terminal_idle_hours` (default 2; `0` turns the mark off) is parsed by `SleepAfter::parse`'s logic, generalised into `config::hours_ms(v: &toml::Value) -> Option<i64>`, which both use. It's read as `UiConfig.terminal_idle_ms: i64`. Error: `terminal_idle_hours in [ui] must be a number of hours, 0 or more`. Idle means not being looked at, and the last touch (or the open, when never touched) at least that long ago; the one function is `terminals::is_idle(last: UtcMs, active: bool, idle_ms: i64, now: UtcMs) -> bool`, with `last = touched_at.unwrap_or(opened_at)`, so the core and the view both call it with plain values.<br>• **One unit formatter:** `moa_core::time::span(ms: i64) -> String` (`now`, `5m`, `3h`, `2d`) is the one place the units live. The existing `time::age` and `moa_view::text::ago` are rewritten to call it (they format the same units twice today), and the new `time::until(at, now) -> String` calls it too.<br>• **CLI:** `Verb::Term { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String>, #[arg(long)] title, #[arg(long)] pane }`. `--pane` stays: it's parsed and ignored, as today (`moa-proto/tests/cli.rs:486`). When the first word is `ls`, `close`, `name`, `note` or `timeout`, it's a sub-command; otherwise it's `term [FOLDER]` as today. `name` and `note` take the rest of the words joined by spaces, so a note may start with `-`. The usage line lists the sub-commands. | A number from `term ls` is easy to type; `%N` is what tmux and the panels use. Keeping `moa term FOLDER` keeps the spec's "term [PATH]". Renaming an agent's tab from a split would mislabel the agent. |
| T6 | **Last touched,** `terminals::touched(state, now) -> Vec<Change>`, runs at the end of every tmux input, next to `mark_seen_into`. A new `Topology::looked_at() -> BTreeSet<PaneId>` gives the active pane of the window each attached user client looks at; control clients are already excluded. `State.looked_at` (in memory) keeps the previous set. Each open record whose pane **enters or leaves** that set gets `touched_at = now`; nothing else is written. A published row's `active: bool` says it's being looked at now, and the view shows `now`. | That's one write when you switch to a terminal and one when you leave it, never one per event. Typing while you're on it counts, because it's on screen. |
| T7 | **`send-keys`** joins `ALLOWED` in exactly one shape: `send-keys -t %N C-c`, built only by `Cmd::interrupt(pane: PaneId) -> Cmd`. Anything else is refused with `send-keys sends only Ctrl-c to one pane`. `tmux_vocab.rs`'s `ruled` list gains it. | moa may type nothing into a terminal except the one interrupt that closing needs. |
| T8 | **Close and timeout:**<br>• **Every close decides on a fresh answer,** never on the listing (tmux doesn't announce program starts, T2). `term.close {target}` (after `link::ready`) asks one tokened query, `display-message -p -t %N '#{pane_current_command}'`, as a new `Asked::TermClose { tell, id }`, in the `Asked::Ls` shape. The decision is made in the answer handler:<br>&nbsp;&nbsp;– a pane that's gone, or no longer `is_terminal_pane`, says `that terminal is gone`;<br>&nbsp;&nbsp;– a shell at its prompt (the answer equals the record's `shell`) gets `kill-pane -t %N` (`sleep::kill_pane`) and the reply `closed <name>`;<br>&nbsp;&nbsp;– a running one gets `Cmd::interrupt`, then `kill-pane` 3 seconds later, with the reply `stopping <program> in <name>, then closing it`.<br>&nbsp;&nbsp;Either way, one activity line `closed terminal <name>` goes through `tx.ui`.<br>• **`State.closing: BTreeMap<TermId, Closing { generation, pane, kill_at: Option<UtcMs> }>`** holds every close moa has started, the shell-only ones too, until a listing no longer shows the pane. A second close of a terminal already in it answers `already closing <name>` and sends nothing. `closing` is cleared when the server changes (`another_server`), when the link is lost, and on `Input::Reload`.<br>• **A Ctrl-c in copy mode only leaves copy mode.** The kill 3 seconds later still happens; that's accepted. **A daemon restart during those 3 seconds** leaves the terminal open with its program interrupted, and you close it again; that's accepted too.<br>• **`TimerKey::Terminals`**, one wall-clock key. `terminals::deadline(state, now) -> Option<UtcMs>` is the minimum of the `kill_at` times and the open records' `timeout_at`, **keeping only times after `now`**, as `sleep::deadline` does. Due work that can't run yet is retried at the end of the next input, never by arming a past time. `terminals::run(state, now, fired, fx)` is called beside `sleep::run` in `handle()`, with `State.terminals_at` re-armed only when the deadline changes, exactly as `sleep_at`. `Input::Reload` resets it.<br>• **The guard before any act** (in `run`, for a kill or a timeout): `link::ready(state)` is `Ok`, the record's or entry's generation equals `state.tmux().generation()`, and `is_terminal_pane` still holds for the pane. Otherwise nothing is sent. A timeout whose generation is gone just waits for the listing that closes its record.<br>• **When it fires:** a due `kill_at` sends `kill-pane` and keeps the entry (with `kill_at` cleared) until the listing drops the pane. A due `timeout_at` commits `timeout_at = None` together with its activity line (`tx.ui_by(.., ActivityWho::Brain)`, text `moa closed <name>: its timeout ran out`), then closes exactly as `term.close` does, starting with the fresh query. While writes are refused, nothing is done, as `sleep_due` does.<br>• **`term.timeout {target, after_ms: Option<i64>}`**: `timeout_at = now + after_ms`, or none. Reply: `closes in <until>` (Task 5's `time::until`) or `no timeout`. `terminals::parse_after` parses `3h`, `90m`, `2h30m`, `45s` and `7d` with `jiff` (`Span` → total ms through `SpanTotal::days_are_24_hours()`, from 1 s to 7 days, else `a timeout is like 3h or 90m, up to 7 days`), and `off` clears it. The CLI and the view both call it. | Deadlines are derived from data, as spec §6.8 says, so a timeout survives a restart, and one that ran out while moa was down fires at once on start. The 3-second wait needs no new timer kind. |
| T9 | **The view, `View::Terms`** (word `terms`, label `⌨ terminals`), `views/terms.rs`:<br>• **`TermRow` grows in Task 3** (with `active` set from Task 4), all `#[serde(default)]`: `id: i64, folder: String, command: String, running: bool, since: i64, opened_at: i64, touched_at: Option<i64>, active: bool, opened_by: String, session: Option<String>` (the session's title, via `title_of`), `note: Option<String>, timeout_at: Option<i64>`. `name` stays. It's published from the open records joined with the topology. The picker keeps reading only `name`.<br>• **Lines:** line 0, dim: `<n> terminals · <k> idle`, or `no terminals open`. Then one row per terminal: ` ⌨ <name> <folder> <what> <touched> <open> <closes> <note>`, where `name` is `clamp(w/5, 8, 24)` wide, `folder` (`short_path`) is `clamp(w/5, 0, 28)` wide and absent under 70 columns, `what` is `<program> <since ago>` (e.g. `node 2h`), or `shell`, plus `· <session>` when it belongs to one, in 22 columns, `touched` is `now` or `ago` in 4 right-aligned, `open` is `ago` in 4 right-aligned, `closes` is `⏱<until>` when a timeout is set, and the note is dim and fills the rest. Idle rows (the same rule as `terminals::is_idle`, which the view calls through `moa_core`) use the dim tone and add the word `idle` after `open`. `changes_at` is the soonest of the rows' `next_ago_change` and, for a timeout, the time its `until` text next changes (the same unit steps, counted down).<br>• **Order:** active first, then by `touched_at` (or `opened_at` when never touched), newest first.<br>• **Keys:** Enter → `term.show`, and an `Ok` closes the popup (`Out::Close`, as the picker); `x` → `term.close`, **always** through `Panel::confirm`: `close <name>, running <program>? y/N` when the row says running, else `close <name>? y/N` (both end in `QUESTION_END`, so a narrow footer keeps `y/N`); the core still makes the real decision on its fresh query (T8); `q`, Esc and Ctrl-c close the view, as in help; `r` → `Ask::TermName` prefilled with the name; `n` → `Ask::TermNote` prefilled with the note; `t` → `Ask::TermTimeout` prefilled `2h`. Prompts are answered with the same ops. A timeout answer is parsed by Task 8's `terminals::parse_after`, so the CLI and the view share it. Hint: `↵ jump · x close · r name · n note · t timeout · esc close`.<br>• **"closes in"** uses Task 5's `moa_core::time::until`; the view adds no formatter of its own.<br>• **Setting:** Task 5's `terminal_idle_ms` is published as `BrainRow.terminal_idle_ms` (`#[serde(default)]`, 2 h).<br>• **Opening it:** `Does::Popup(View::Terms)` on `T`, with the menu item `Terminals…` placed after `Terminal here` and help text `every terminal: what's running, when you last touched it; close, name, note, timeout`. `Popups::command` handles `Terms` with title `#[fg=#88c0d0 bold] ⌨ terminals ` at 80% × 70%. `moa panel terms --once` is allowed, and `moa_tui::run::topics(Terms, once)` is `terms, brain`. When a terms panel registers, the core asks one listing (`state.tmux.refresh()`), so "what's running" is current when you open it. Help's `VIEWS` gains `Terms`. The `moa.tmux` snapshots change by the binding and the menu item. | A popup is the cheapest form that's "its own place", the same as the picker. A typing filter would take the letter keys, and the picker already finds terminals by name. Idle is computed in the view from data it has, so it needs no timer in the core. |

## Global Constraints

The roadmap's, every earlier slice plan's, and 5a's, 5b's and 5c's Global Constraints apply.
Also:

- **Reuse before writing.** Before adding a helper, type or function, search the workspace for
  one that already does the job (`grep`, the crate's `lib.rs` exports, and the "Builds on" list
  above), then reuse or extend it. A near-copy of existing logic is an **Important** review
  finding. Each report lists what it reused or extended.
- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **Never run cargo while an e2e test runs.** Run a test binary directly, or use another
  `CARGO_TARGET_DIR`.
- **No ongoing cost per terminal:** no timer, poll, query or process exists because a terminal
  is open. Registration, program and folder come only from listings that events already cause,
  plus one listing when you ask (`moa term ls`, or opening the view) and one query per close.
  The only timer is `TimerKey::Terminals`, armed only for a closing or a timeout, and never at
  a time already past.
- **Never act on a pane id from another server:** every `send-keys` or `kill-pane` a terminal
  gets is checked against the link's server generation and `is_terminal_pane` at the moment
  it's sent (T8).
- **moa never closes a terminal you didn't ask it to,** except a timeout you set. Nothing in
  5d closes a terminal for being idle.
- **moa never reads terminal output** (no `capture-pane` of a terminal).
- **`send-keys` only as `Cmd::interrupt`** (T7).
- **tmux targets are pane ids** (`%N`) or the exact sessions `=moa:` and `=_moa:`. Never a name.
- **The core stays pure:** file reads only through `Deps`, writes only through
  `Effect::WriteFile`.
- **Tests:**
  - only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too;
  - never a tmux server named `moa` or `moa-work`, and never a real brain;
  - `fake-claude` is the only agent;
  - every wait goes through `wait_for` with a 10 s limit, never a sleep;
  - a test without a daemon that waits for a stopped pane nudges with `run-shell true`;
  - load workers in any hunt carry their own `timeout`.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`,
  `~/private`.
- **Plain words** everywhere: "tab", "folder", "terminal", never "pane" or "window".

## Review Focus

1. **A terminal that becomes an agent.** You run `claude` in a terminal and moa adopts it, or
   moa opens an agent tab and lists it before tagging it. Expected:
   - the record closes, or is never made;
   - an agent moa launched whose tag was lost is still recovered by reconcile as tracked
     (registration waits for reconcile);
   - a terminal's `@moa_term` stays when an agent is adopted in it;
   - moa never sends `Ctrl-c` or `kill-pane` to it through `term.*`, even when a close or a
     timeout was already under way.

   Pinned by `an_adopted_terminal_stops_being_one`, `a_launch_in_flight_holds_back_new_panes`,
   `registration_waits_for_reconcile` (Task 3), and `a_close_skips_a_pane_that_became_an_agent`
   (Task 8).
2. **Restarts.** The daemon restarts with terminals open, or the tmux server dies and a new one
   starts (a reboot). Expected:
   - no duplicate records;
   - terminals moa had are kept with their names, notes and timeouts;
   - the old server's records close;
   - a timeout that ran out while moa was down fires once the link is up and reconciled, on
     the same server;
   - on a new server, nothing is ever sent to a pane id an old record named.

   Pinned by `a_daemon_restart_keeps_each_terminal_once`, `a_new_server_closes_the_old_records`
   (Task 3), `a_timeout_that_passed_while_down_fires_once_reconciled`, and
   `an_old_servers_timeout_never_touches_the_new_server` (Task 8).
3. **Closing awkward terminals:** a program that ignores `Ctrl-c`; the terminal you're looking
   at; one that's already gone; two closes in a row; a close during a link outage. Expected:
   - it's gone within about 3 seconds;
   - a gone one says `that terminal is gone`;
   - a second close never sends a second `kill-pane`;
   - a close with the link down is refused in plain words.

   Pinned by `a_program_started_after_the_listing_is_still_interrupted`,
   `a_running_terminal_is_interrupted_then_killed`, `closing_twice_kills_once` (shell-only and
   running), `closing_a_gone_terminal_says_so` (Task 8), and
   `the_view_closes_a_running_terminal` (Task 10, a `trap '' INT; sleep 1000` program).
4. **Odd names and folders:** names with `#`, `;`, quotes, emoji, CJK, ZWJ or RTL text; 40 or
   41 characters; a folder holding spaces or `#`; 60 terminals in the view at widths 40–200.
   Expected:
   - exact round trips into `@moa_label` and the tab name;
   - no line wider than the panel;
   - no panic.

   Pinned by `naming_round_trips_odd_names` (Task 5, real tmux 3.4 and 3.8) and
   `terms_view_never_panics_or_overflows` (Task 9, proptest).
5. **No cost for being open.** With 30 terminals open and ordinary activity going on, moa sends
   nothing for them and arms no timer. Pinned by `thirty_idle_terminals_cost_nothing` (Task 8:
   after a listing, an hour of `Timer(Clock)` inputs, `WindowPaneChanged` among non-terminal
   panes, and an unchanged listing produce no `SetTimer{Terminals}`, no tmux command for a
   terminal, and no `Persist` of a terminal). Also `a_blocked_timeout_never_arms_a_past_timer`
   (Task 8: writes refused, a timeout due → at most one `SetTimer` per input, never at a time
   `<= now`).

---

## Task 1: the terminals table

**Files:** `crates/moa-store/src/sql/0001_v1.sql`, `rows.rs`, `snapshot.rs`,
`export/mod.rs`; `crates/moa-core/src/model.rs`, `change.rs`, `state.rs`, `core.rs`
(`unknown_message`); tests `crates/moa-store/tests/{open,snapshot,round_trip}.rs` and
`crates/moa-core/tests/` (model round trip).

**Interfaces:**
- Produces: `model::{TermId, OpenedBy, Terminal}`, `Change::Terminal(Terminal)`,
  `Table::Terminal`, `State.terminals: BTreeMap<TermId, Terminal>` (open rows only), and
  `state.next_id(Table::Terminal)`.

- [ ] **Step 1: Failing tests.**
  - `open.rs`: the exact table list gains `terminals`.
  - `snapshot.rs`, in "one row for every table": a terminal row loads.
  - New: `the_store_refuses_two_open_records_for_one_pane` (the partial unique index), while
    a closed record and a new open one for the same pane are fine.
  - New: `a_closed_terminal_is_not_loaded_but_its_id_is_not_reused`. Write ids 1 (open) and
    2 (closed); a fresh load has only id 1, and `next_ids` gives 3.
  - `round_trip.rs`: a `Change::Terminal` written and read back is equal, including `None`
    fields and `opened_by = found`.
  - Core: `OpenedBy` round-trips `you`/`moa`/`found`, and an unknown value is kept as a load
    note (the `Flags::closed` pattern).
- [ ] **Step 2: Run them.** `nix develop --command cargo nextest run -p moa-store -p moa-core`.
  Expected: FAIL (no table, no types).
- [ ] **Step 3: Implement T1** by copying `pane_bindings` (rows, generation fields, upsert) and
  `asks` (integer id, `NextIds`). `Change::topics` returns `[Topic::Terms]`. Nothing publishes
  from records yet; that's Task 3.
- [ ] **Step 4: Run the tests again.** Expected: PASS, and the whole workspace still passes.
- [ ] **Step 5: Commit.** `v1: a terminals table`.

## Task 2: the listing sees every pane's program and folder

**Files:** `crates/moa-core/src/tmux/topology.rs`, `tmux/link.rs` (`rebuild`), `state.rs`
(`Deps.process_name`); `crates/moa-platform/src/proc.rs`; `crates/moa-daemon/src/run.rs`;
`crates/moa-core/tests/term.rs`, `tests/tmux_vocab.rs`; `crates/moa-daemon/tests/tmux.rs`;
`crates/moa-tmux/src/moa.tmux.tpl`, `conf.rs`; `crates/moa-tmux/tests/conf.rs` and its
snapshots.

**Interfaces:**
- Produces: `Topology::{pid_of(&PaneId) -> Option<u32>, command_of(&PaneId) -> Option<&str>,
  cwd_of(&PaneId) -> Option<&str>, panes_listed(&self) -> bool}`; `LIST_TERMS` in T2's
  shape; and `Deps.process_name: fn(u32) -> Option<String>`, backed by a new
  `moa_platform::proc::name(pid: u32) -> Option<String>` beside `proc::started`, and set in
  `crates/moa-daemon/src/run.rs` where the other `Deps` fns are. `terms()`, `term_label` and
  `term_name` are unchanged for callers.

- [ ] **Step 1: Failing tests.**
  - `term.rs`: the existing `lists(..)`/`listed_with(..)` helpers move to T2's line format.
  - New: `the_listing_keeps_each_panes_pid_program_and_folder`: a `T` line for `%5` with pid
    `4242`, `node` and `/tmp/a b/#x`; `pid_of(%5) == Some(4242)`, `command_of(%5) ==
    Some("node")`, and `cwd_of == "/tmp/a b/#x"`.
  - New: `a_label_with_tabs_is_kept`.
  - New: `a_pane_listed_twice_keeps_no_pane_fields`.
  - New: `a_failed_pane_list_says_so` (`panes_listed == false`, the other lists kept).
  - Every existing terms test keeps passing.
  - `moa-platform`: `process_name_matches_tmux` — for this test process's own pid, the name is
    the executable's base name.
  - `moa-daemon/tests/tmux.rs`: `the_terminals_query_lists_only_terminals` becomes
    `the_pane_query_lists_programs_and_folders`. On real tmux, with tmux 3.4 put first on
    `PATH` as in earlier slices, and again with the default 3.8: a fresh shell's
    `command_of` equals `process_name(pid_of)` (this is the test that catches macOS's
    `/bin/sh` running as `bash`); a pane running a long `sleep` doesn't (assert only that
    they differ, never the name `sleep`, since a nix `sleep` lists as `coreutils`); and a
    folder with a space reads exactly.
  - `crates/moa-tmux/tests/conf.rs`: new `the_split_keys_mark_terminals` and the T4 drift
    test `the_plain_keys_do_what_the_table_says`, both on a throwaway tmux.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T2 and T4.** The query loses `-f` and gains the fields; `terms_listed`
  keeps its rule for `terms()` and stores the new per-pane fields for every pane.
  `tmux_vocab.rs`'s four-query test changes only by the query's text.
- [ ] **Step 4: Run the tests again**, including the daemon tmux test with the 3.4 tmux.
  Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the listing knows each pane's program and folder, and splits are terminals`.

## Task 3: registration

**Files:** new `crates/moa-core/src/terminals.rs` (with `pub mod terminals;` in `lib.rs`);
`core.rs` (the `link_listed` branch, and the `first` flag from the link's connect);
`ops/term.rs` (`term_opened`); `publish.rs` (`terms_rows` from records); new test
`crates/moa-core/tests/terminals.rs`.

**Interfaces:**
- Consumes: Task 1's model, and Task 2's `Topology` accessors.
- Produces: `terminals::listed(state: &mut State, now: UtcMs, first: bool) -> Vec<Change>`;
  `terminals::open_for(state: &State, pane: &PaneId) -> Option<&Terminal>`;
  `terminals::is_terminal_pane(state: &State, pane: &PaneId) -> bool`, the T3 rule in one
  place; and `TermRow` with T9's fields (`active` is false until Task 4).

- [ ] **Step 1: Failing tests** (`tests/terminals.rs`, using `tests/common` and `term.rs`'s
  listing helpers, moved into `common` so neither file copies them):
  - `registration_waits_for_reconcile`: listings before reconcile register and tag nothing;
    an untagged, unbound agent pane moa launched is still bound tracked by reconcile; then
    reconcile's end runs `listed` once with `first = true`.
  - `the_first_run_finds_open_terminals`: two plain panes and one agent → two `found`
    records (the shell read through `Deps.process_name`), and the agent has none.
  - `a_split_made_after_the_first_run_is_yours`: a pane that already carries `@moa_term` from
    the split key → `you`.
  - `a_new_shell_is_registered_as_yours`: after the first run, a new untagged pane gets a
    `you` record, and `@moa_term`/`@moa_label ⌨ shell` are sent.
  - `a_launch_in_flight_holds_back_new_panes`: with a launch waiting, a new untagged pane
    gets no record and no tag; the next listing registers it.
  - `a_failed_pane_list_changes_nothing`.
  - `a_terminal_moa_opened_is_registered_with_its_name`: `term.open` → `term_opened` writes
    `you` with its name, and the next listing makes no second record.
  - `a_split_belongs_to_its_agents_session`.
  - `a_changed_program_updates_since`.
  - `a_gone_terminal_is_closed`.
  - `an_adopted_terminal_stops_being_one`: a record's pane becomes bound to a session →
    `closed_at` set, and no `@moa_term` unset is sent.
  - `panels_spot_and_hidden_panes_are_never_terminals`.
  - `the_picker_keeps_a_terminal_whose_record_failed`: writes refused → the `terms` row is
    still published from the topology.
  - `a_daemon_restart_keeps_each_terminal_once`: a load with open records, then a first
    listing of the same generation → no new records, names kept.
  - `a_new_server_closes_the_old_records`.
  - `the_terms_topic_is_built_from_records`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T3.** Writes go through one `Tx` commit per listing that changed
  something, never one per pane. The tags for newly found panes are `Tx::tmux` commands, sent
  only if the commit saved.
- [ ] **Step 4: Run the tests again,** and the whole workspace (`cockpit`, `term`, `picker`
  tests included). Expected: PASS.
- [ ] **Step 5: Commit.** `v1: every terminal is registered when moa sees it`.

## Task 4: last touched

**Files:** `crates/moa-core/src/tmux/topology.rs` (`looked_at`), `terminals.rs` (`touched`),
`state.rs` (`looked_at`), `core.rs` (the call beside `mark_seen_into`), `publish.rs`
(`active`); tests in `tests/terminals.rs`.

**Interfaces:**
- Produces: `Topology::looked_at(&self) -> BTreeSet<PaneId>`, and
  `terminals::touched(state: &mut State, now: UtcMs) -> Vec<Change>`.

- [ ] **Step 1: Failing tests.**
  - `switching_to_a_terminal_stamps_it`.
  - `leaving_a_terminal_stamps_it_again`.
  - `staying_on_a_terminal_writes_nothing` (ten `WindowPaneChanged` inputs for other windows
    → no `Change::Terminal`).
  - `a_detach_stamps_the_terminal_you_were_on`.
  - `two_clients_on_two_terminals_stamp_both`.
  - `a_control_client_touches_nothing`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T6,** following `tmux::focus::mark_seen`.
- [ ] **Step 4: Run the tests again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa knows when you last touched each terminal`.

## Task 5: `moa term ls`, `name`, `note`

**Files:** `crates/moa-core/src/op.rs` (`TermLs`, `TermName`, `TermNote`, plus `METHODS`,
`writes`, `method`), `ops/term.rs`, `terminals.rs` (`find`, `is_idle`), `config.rs`
(`hours_ms`, `terminal_idle_hours`), `time.rs` (`until`), `ops/mod.rs`, `core.rs`
dispatch; `crates/moa-proto/src/cli.rs`; `crates/moa/src/ctl.rs`; tests
`crates/moa-core/tests/{terminals,skeleton,config}.rs`, `crates/moa-proto/tests/` (CLI and
the schema snapshot), `crates/moa-daemon/tests/tmux.rs` (`naming_round_trips_odd_names`).

**Interfaces:**
- Produces: `terminals::find(state: &State, target: &str) -> Result<&Terminal, OpError>`;
  `terminals::is_idle(t: &Terminal, active: bool, idle_ms: i64, now: UtcMs) -> bool`;
  `config::hours_ms(v: &toml::Value) -> Option<i64>` and `UiConfig.terminal_idle_ms`;
  `time::until(at: UtcMs, now: UtcMs) -> String`; `Op::TermLs`,
  `Op::TermName { target: String, name: String }`, and
  `Op::TermNote { target: String, note: String }` (methods `term.ls`, `term.name`,
  `term.note`); and `Verb::Term { args: Vec<String>, title: Option<String> }`.

- [ ] **Step 1: Failing tests.**
  - `find_takes_a_number_a_pane_or_a_name`.
  - `find_refuses_unknown_and_ambiguous` (the exact texts from T5).
  - `ls_lists_open_terminals_in_text_and_data`, including `idle` and `closes in` (a fixture
    sets `timeout_at` directly, since `term.timeout` comes in Task 8).
  - `ls_with_none_says_so`.
  - `is_idle_counts_from_last_touch_or_open_and_never_while_looked_at`, with `0` meaning
    never.
  - `terminal_idle_hours_parses`: 0 included, the error text exact, and `sleep_after_hours`
    still parses the same through `hours_ms`.
  - `age_ago_and_until_share_one_formatter` (`time::span`; the existing `age` and `ago`
    tests still pass unchanged).
  - `ls_asks_a_fresh_listing_first`.
  - `ls_with_the_link_down_says_so`.
  - `ls_shows_how_long_a_program_has_run` (`node (2h)`).
  - `naming_sets_the_record_label_and_tab`.
  - `naming_a_split_leaves_the_agents_tab`.
  - `a_name_is_1_to_40_characters`.
  - `a_note_is_cut_at_200_and_empty_clears`.
  - CLI: `term_keeps_its_folder_form`, `term_ls_and_friends_parse`, and `term_dot_ls_is_a_folder`.
  - `skeleton.rs` samples for the three ops.
  - The schema snapshot updated.
  - Real tmux 3.4 and 3.8: `naming_round_trips_odd_names`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T5.** `rename-window` and `set-option -p` are already allowed; reuse
  `set_pane_option` and `term_sent`.
- [ ] **Step 4: Run the tests again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa term ls, name and note`.

## Task 6: what the picker shows for a terminal

**Files:** `crates/moa-view/src/views/picker.rs`, `panel/mod.rs` (`here`); tests
`crates/moa-view/tests/picker.rs` and its snapshots.

**Interfaces:**
- Consumes: Task 3's richer `TermRow`.

- [ ] **Step 1: Failing tests.**
  - `a_terminal_row_shows_its_folder_and_program`: the picker's `why` column shows `~/folder ·
    node`, replacing 5c's name-only row (5c call 5: moa didn't know the folder then; now it
    does).
  - `ctrl_a_dollar_from_a_terminal_row_uses_its_folder` (`Panel::here`).
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement**, reusing `short_path`.
- [ ] **Step 4: Run the tests again.** Expected: PASS, with the snapshots reviewed.
- [ ] **Step 5: Commit.** `v1: the picker shows a terminal's folder and program`.

## Task 7: `send-keys`, only Ctrl-c

**Files:** `crates/moa-core/src/tmux/cmd.rs`; `crates/moa-core/tests/tmux_vocab.rs`.

**Interfaces:**
- Produces: `Cmd::interrupt(pane: PaneId) -> Cmd`.

- [ ] **Step 1: Failing tests.**
  - `interrupt_is_send_keys_c_c_to_one_pane`.
  - `send_keys_refuses_any_other_shape`: other keys, `-l`, two targets, a session target, an
    extra argument. The text is `send-keys sends only Ctrl-c to one pane`.
  - `ruled` gains `send-keys`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T7.**
- [ ] **Step 4: Run the tests again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa may send one pane a Ctrl-c, and nothing else`.

## Task 8: close and timeout

**Files:** `crates/moa-core/src/effect.rs` (`TimerKey::Terminals`), `terminals.rs`
(`deadline`, `run`, `parse_after`, `close`), `state.rs` (`closing`, `terminals_at`),
`core.rs` (`handle`, `Input::Timer`, `Input::Reload`), `op.rs` (`TermClose`, `TermTimeout`),
`ops/term.rs`; `crates/moa-proto/src/cli.rs`; tests `crates/moa-core/tests/terminals.rs`,
which uses `tests/sleep.rs`'s time helpers, moved into `common`.

**Interfaces:**
- Consumes: Task 7's `Cmd::interrupt`, Task 5's `find`, and `sleep::kill_pane`.
- Produces: `Asked::TermClose { tell: Tell, id: TermId }`, `State.closing:
  BTreeMap<TermId, Closing>`, `TimerKey::Terminals`, `Op::TermClose { target: String }`,
  `Op::TermTimeout { target: String, after_ms: Option<i64> }`, and
  `terminals::parse_after(text: &str) -> Result<Option<i64>, String>`.

- [ ] **Step 1: Failing tests.**
  - `a_close_asks_tmux_first`: `term.close` sends one tokened query and nothing else.
  - `a_shell_terminal_closes_at_once` (the answer equals the record's `shell`).
  - `a_program_started_after_the_listing_is_still_interrupted`: the record says shell, the
    fresh answer says a program → `Cmd::interrupt`, not `kill-pane`.
  - `a_running_terminal_is_interrupted_then_killed`: `Cmd::interrupt`, then
    `SetTimer{Terminals, now+3000}`, then at that wall `kill-pane`.
  - `closing_twice_kills_once`, for a shell-only close and for a running one:
    `already closing <name>`.
  - `closing_a_gone_terminal_says_so`.
  - `closing_with_the_link_down_is_refused`.
  - `a_close_skips_a_pane_that_became_an_agent`: bound during the 3 s → no `kill-pane`.
  - `closing_is_cleared_by_a_new_server_a_lost_link_and_reload`.
  - `a_timeout_closes_and_says_so`: the activity text is exact, it's by `moa`, and
    `timeout_at` is cleared in the same commit.
  - `a_timeout_can_be_cleared`.
  - `a_timeout_that_passed_while_down_fires_once_reconciled`.
  - `an_old_servers_timeout_never_touches_the_new_server`: due timeouts at start, a new
    server's first listing → the old records close, and no `send-keys` or `kill-pane` is
    sent.
  - `writes_refused_close_nothing`.
  - `a_blocked_timeout_never_arms_a_past_timer`.
  - `thirty_idle_terminals_cost_nothing` (as Review Focus 5 describes).
  - `parse_after_reads_3h_90m_2h30m_45s_and_off`, and refuses `0s`, `8d`, `soon` and `-1h`
    with the T8 text.
  - The CLI's `term close`/`timeout`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T8,** with `sleep.rs`'s `deadline`/`run` shape. Don't copy them: if a
  piece is shared word for word (re-arming only when the deadline changes), move it to one
  helper both use.
- [ ] **Step 4: Run the tests again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: closing terminals, gently, and timeouts`.

## Task 9: the terminals view

**Files:** `crates/moa-core/src/panel.rs` (`View::Terms`), `keymap.rs`, `publish.rs`
(`BrainRow.terminal_idle_ms`); `crates/moa-view/src/views/{mod.rs,terms.rs,help.rs}`,
`keys.rs`, `panel/{mod.rs,ask.rs}`, `fixture.rs`; `crates/moa-tmux/src/conf.rs` and its
snapshots; `crates/moa-tui/src/run.rs` (`topics`); `crates/moa/src/panel.rs` (`--once`);
tests in `crates/moa-view/tests/` (`terms.rs`, `never_panic.rs`, snapshots),
`crates/moa-tui/tests/render.rs` (widths 40, 60, 80, 120, 200), and the `keymap` tests.

**Interfaces:**
- Consumes: Tasks 3–5 and 8's ops and rows, `terminals::{is_idle, parse_after}`, and
  `time::until`.
- Produces: `View::Terms` and `views::terms::lines(data, ctx, width) -> Vec<Line>`.

- [ ] **Step 1: Failing tests.**
  - `terms_rows_order_and_idle_mark`.
  - `terms_header_counts_idle`.
  - `none_open_says_so`.
  - `enter_shows_x_always_confirms_q_and_esc_close`: both confirm texts, and they keep `y/N`
    in a 30-column footer.
  - `a_timeout_countdown_redraws_on_time` (`changes_at`).
  - `registering_a_terms_panel_asks_one_listing` (core).
  - `r_n_t_prompt_and_send_their_ops` (prefills included).
  - `a_bad_timeout_answer_stays_in_the_footer`.
  - `the_idle_setting_reaches_the_view` (`BrainRow.terminal_idle_ms`).
  - `terms_view_never_panics_or_overflows` (proptest: 0–60 rows, odd names, widths 20–200).
  - Render snapshots at the five widths.
  - The `keymap` tests: `T` is in the table once, and has help.
  - `moa.tmux` snapshots: the `T` popup binding and `Terminals…`.
  - `--once` accepts `terms`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement T9,** reusing the picker's column helpers and `Panel::confirm`/`ask`.
- [ ] **Step 4: Run the tests again.** Expected: PASS, with the snapshots reviewed.
- [ ] **Step 5: Commit.** `v1: the terminals view`.

## Task 10: 5d end to end

**Files:** new `crates/moa/tests/terms_e2e.rs`; `crates/moa/tests/common/mod.rs` (the `Rig`
helpers move here from `picker_e2e.rs`, which then uses them from `common`).

- [ ] **Step 1: Write the tests** (real tmux, a real `moa`, `fake-claude`):
  - `every_kind_of_terminal_is_recorded`: `moa term`, `Ctrl-a $`, `Ctrl-a |`, and a plain
    `tmux new-window` → `moa term ls` lists four, with the right owner. The split names its
    agent's session.
  - `the_view_lists_names_and_jumps`: `Ctrl-a T`, select a row, Enter → that terminal's tab
    is current, and the popup closed (`popup_closes`).
  - `the_view_closes_a_running_terminal`: a terminal running `trap '' INT; sleep 1000`; `x`,
    `y` → the pane is gone within 10 s.
  - `naming_from_the_view_renames_the_tab`.
  - `a_timeout_closes_a_terminal`: `moa term timeout <n> 2s` → gone, and the activity line
    reads `moa closed …`.
  - `idle_terminals_are_marked`: `terminal_idle_hours = 0.0005` → `idle` shows within 10 s.
  - `the_split_shows_in_the_picker`.

  Each runs 10 times under load on tmux 3.8 and 3.4, and each is made to fail once on purpose.
- [ ] **Step 2: Run them.** Expected: PASS. CI's Ubuntu tmux 3.4 job is the real check.
- [ ] **Step 3: Commit.** `v1: 5d end to end`.

## Carried, and where each goes

- **The sweep** (an agent that looks at terminals every few hours and asks you): slice 6 or 7,
  through these `term.*` ops. `term` joins `ACTION_VERBS` then.
- **`opened_by = moa`**: slice 6, from moa's own connection.
- **Autonomy** (moa acting without asking): slice 6 design.
- **Pruning closed records:** the retention decision (Q1).
- **5c's carries not taken here** stay in the backlog: m-1, M3, M6, `run-shell -b true`, the
  reconcile×queue test, input.rs's doc minors, and crossterm upstream. M8 goes to slice 8.
