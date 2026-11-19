# moa v1 slice 5b: layouts, the cockpit and the agent spot

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When you first attach, moa's first tab becomes the **cockpit**: the panels of the
current layout around an **agent spot**.
- `Ctrl-a 1–4` switch layouts by moving the panels that already run, never starting new ones
  once each is there.
- `Ctrl-a c t p g` go to a panel.
- Entering a session swaps it into the spot, and the session that was there goes back to its
  own tab.
- `Ctrl-a r` repairs.
- Sleep and close leave the spot showing "no agent here yet" instead of a hole.

The panels behave as parts of moa. They survive moa restarts, follow the view moa gives them,
come back on Repair, and give the terminal back on every signal.

**Architecture:**
- **`moa_core::layout`** (pure) holds the spike's `layouts.json` format: parse with the spike's
  messages, validate, the presets, and the merge.
- **`moa_core::cockpit`** (pure) is the engine. From a `Scene` and a layout, it plans which
  panels to start (only when no running panel can be reused), then one chained tmux command line
  that moves panes into place, parks the rest in the hidden `_moa` session, resizes, and tags. A
  `Scene` is what a fresh tmux listing says: which panes exist, where they are, what they are.
  A simulator in the tests checks the plan's invariants under proptest. The real tmux e2e
  checks the geometry.
- **The core** runs one cockpit operation at a time, each planned from a fresh listing. These
  are `layout.*`, `goto`, `ui.select_view`, the keys, Repair, the lazy cockpit on the first
  attach, and the spot-aware `focus`/`sleep`/`close`.
- **The panels** follow their row in the `panels` topic. moa's assignment of a view (the pane
  tag `@moa_view`) wins over a panel's own memory.

```
 Ctrl-a 2 ─► moa:key layout-2 ─► core: cockpit queue (one in flight) ─► Asked::Scene: list-sessions/panes/clients
   └─ answer ─► Scene ─► cockpit::plan(scene, focus layout)
        ├─ spawn: [empty-slot]? ─► new-window -d -c ~ -t =_moa: … moa panel empty-slot ─► tag each started pane ─► listing again
        └─ build: one chain "swap-pane … ; break-pane … ; join-pane … ; resize-pane … ; set -p …"
                 ─► answer ─► panels topic (views) ─► next queued operation
 focus Q   ─► listing ─► spot? swap-pane -d -s %agent -t %spot ─► old occupant: parked (empty slot) | own tab by title
 sleep/close Q in the spot ─► a spare slot? swap it in, kill-pane : respawn-pane -k the spot as the empty slot
```

**Tech Stack:** Rust; the existing crates (`moa-core`, `moa-daemon`, `moa-tmux`, `moa-proto`,
`moa-view`, `moa-tui`, `moa`, `moa-testkit`). New dependency: `indexmap` (with `serde`) in
`moa-core`, to keep `layouts.json`'s order. `tokio`'s `signal` feature in `moa-tui`.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.13: panels, layouts, the lazy cockpit [C28], `goto`, `focus`, frames, keys, sleep;
- §6.6: the allow-listed `Cmd`, "`a ; b` plus a sentinel for transactions", mutations batched
  into one write;
- §6.4: the `layouts` topic and the `layout.*`, `goto` and `ui.select_view` methods;
- §7: rows "Layout tree…", "`layout apply/list/show/set`", "`goto`, `focus`, agent spot swap",
  "Panel tagging, labels, frame colours", "Repair, cockpit split refused";
- §8: lessons 4, 11, 12 and 14.

Roadmap: slice 5 item 4, item 6 (supervision, Repair), and the 5b row of the 5a plan's split
table. Parity source (read only, never run): `spike/moa-proto/layout.py` (all of it),
`moactl.py` (`here_dir`, `put_to_sleep`, `goto`/`layout` in `main`), R1 §2.2.

**Builds on** 5a and before:
- From `moa_core`:
  - `panel::{View, PanelReg, register, here, gone}`; `State.{panels, pane_views, here_of, panel_in, killed}`;
  - `ops::launch::{focus, focus_cmds}`; `ops::term::{term, term_key, Tell, told}` (after Task 1);
  - `sleep::{why_not, kill_tab, put_to_sleep}`;
  - `tmux::{cmd::{Cmd, literal_format}, link::{TmuxState, Asked, Answer, notice, set_pane_option, unset_pane_option}, topology::{Topology, refresh_cmds, LIST_PANES}, mailbox::{KeyAction, HookMsg, Mailbox}}`;
  - `publish::{Topic, PanelRow}`; `Effect::Tmux`; `Tx::tmux`; `post_commit`.
- The client: `moa_client::Live`; `moa_tui::run::{Loop, first_ops, claim_pane}`; `moa_view::panel::Panel::{set_view, view}`.
- `moa_tmux::{client::ControlClient, conf::moa_tmux}`.
- `moa_proto::{cli::{Verb, LayoutOp}, wire::StatusOk}`.
- The testkit: `TempBrain`, `Terminal`, `wait_for`, `fake_claude`.
- `crates/moa/tests/common`: `open`, `came_up`, `tmux`, `status_json`, `panel_tab`, `screen_of`, `cell_of`, `click`, `moactl`.
- `crates/moa-core/tests/common`: `loaded`, `link_up`, `send`, `send_on`, `feed`, `changes`, `replies`.

## Calls for you to check

The most important come first. Each is a rule below (L-number) with its reason.

1. **moa's first tab becomes the cockpit the first time you attach** (L9). It also happens the
   first time after the terminal server restarts, for example after a reboot. That tab is a
   plain shell today. When the cockpit is built there, the shell moves to its own tab,
   `⌨ shell`, still running. moa never closes a program you started.
2. **The console's place says "the console isn't in this build yet"** until slice 6 builds the
   console. The default layout, `cockpit`, has the console in the middle (spike parity), so the
   middle of the cockpit is that one line until then (L9).
3. **Panels are reused, not multiplied** (L5). When a layout needs a view no running panel
   shows, moa changes the view of a panel the new layout doesn't need. In `triage`, for
   example, the tree panel becomes the status panel. A new panel starts only when there's none
   to spare. Across the four presets that's at most four panels, the empty spot included, each
   kept running for moa's whole life.
4. **Ctrl-c in a cockpit panel cancels a prompt, but never closes the panel** (L14). With
   nothing to cancel it says `this panel is part of moa's layout; Ctrl-a r repairs the layout`.
   A panel you start yourself with `moa panel` still quits on Ctrl-c.
5. **A panel that stops stays where it was**, reading `panel stopped · Ctrl-a r brings it back`
   (L12). It isn't restarted by itself, so a panel that crashes on start can't restart in a
   loop. It's logged. Switching layouts moves it like any panel. Repair restarts it in place.
6. **One layouts file for every brain, with the current layout kept per brain** (L2). `layout
   set` writes `~/.config/moa/layouts.json`, the spike's file and format. Edits you make by hand
   apply at the next layout command; no restart is needed. If the file has a typing mistake,
   moa uses the built-in layouts, says what's wrong, and won't save over your file until it's
   fixed. The spike kept `current` in the file, which every brain would share. v1 keeps it in
   each brain and leaves the file's `current` alone.
7. **Switching layouts rebuilds the cockpit in place.** Panes you resized by hand go back to the
   layout's sizes, as in the spike. The cockpit stays the same tab with the same number, so your
   terminal doesn't jump to another tab during a switch (L6).
8. **An agent that ends while it's in the spot is replaced by the empty spot** right away ("no
   agent here yet"), so the cockpit never has a hole. Putting the spot's agent to sleep or
   closing it does the same (L11).
9. **moa's assignment of a view wins over a panel's memory** (L8, 5a's M6). If moa gives a panel
   another view while the panel is disconnected, or while moa restarts, the panel shows moa's
   choice when it reconnects.
10. **moa says `moa's folder <root> is gone`** rather than quietly starting itself in a folder
    that no longer exists (L15).

## PRs (roadmap P4)

Six PRs, each about 1500 lines of diff at most. Each branches from `main` after the previous one
merged. Each gets:
- the full verify: `cargo fmt --check`; clippy, also with `--target x86_64-unknown-linux-gnu` for
  `moa-platform` and `moa-tmux`; `cargo nextest run --workspace` three times; `nix flake check`;
- the tmux 3.4 pass on the tmux-facing crates it touched;
- `/code-review medium --fix`, with the fixes pushed;
- green CI.

Bodies end with the try-it, when there is one. No "Restart needed" line.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-5b-1` | 1–2 | `v1 slice 5b (1/6): the terminal code in its own file, and no stray test servers` |
| p2 | `v1/slice-5b-2` | 3–4 | `v1 slice 5b (2/6): layouts, and the tmux commands that move panes` |
| p3 | `v1/slice-5b-3` | 5–6 | `v1 slice 5b (3/6): the cockpit engine, and moa starting its panels` |
| p4 | `v1/slice-5b-4` | 7–8 | `v1 slice 5b (4/6): layout switching, goto, Repair, and panels that follow moa` |
| p5 | `v1/slice-5b-5` | 9–10 | `v1 slice 5b (5/6): the agent spot` |
| p6 | `v1/slice-5b-6` | 11–12 | `v1 slice 5b (6/6): panels as parts of moa, end to end` |

p3 is the one most likely to run long. If it passes about 1500 lines, Task 6 moves to the head
of p4.

**Done already** (committed with this plan): the spec edits for Calls 4–6 and 9 (§6.13). §6.4
already lists the `layouts` topic and the `layout.*`, `goto` and `ui.select_view` methods. This
plan was reviewed before implementation; the review checked every tmux claim on real 3.4 and
3.8 servers.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| L1 | **`moa_core::layout`, the spike's format and texts:**<br>• `Node` is `{"split":"h"\|"v","children":[≥2 nodes],"size"?}` or `{"panel":LEAF,"size"?}`;<br>• `size` is a positive integer (cells; not `30.0`, not a boolean) or `"N%"` with N from 1 to 99;<br>• leaves, in the spike's `PANELS` order, are `console tree today activity projects plan status picture agent`, at most one `console` and one `agent` per layout;<br>• the presets are the spike's `DEFAULTS` (cockpit, focus, classic, triage), copied exactly;<br>• the file's order is kept, and presets missing from the file come first (the spike's merge), so a deleted preset comes back;<br>• the file's entries stay raw JSON and are parsed when used: a broken entry is listed with its error and refused on `apply`, while the others still work.<br>Errors are the spike's texts with their paths (`layout.children[1]: unknown panel 'tre' (panels: console, tree, today, activity, projects, plan, status, picture, agent)`). | Spec §6.13 ("keep the spike's format and file", "validated: known names, ≤ 1 console, ≤ 1 agent", presets "merged in, deleted presets come back"). The spike accepted `"0%"`, which tmux refuses, so 1–99 says that up front. Parsing per entry keeps one bad hand edit from taking away every layout. |
| L2 | **Where layouts live:**<br>• `layouts.json` lives in `Paths::config_dir()` (`~/.config/moa/`; the sandbox's own in the sandbox).<br>• The core reads it through `Deps.read_text` when it starts, and again at every `layout.*` op, `goto`, layout key, Repair and lazy build. That's a user action, never a poll.<br>• **A file that isn't valid JSON** gives the presets alone, plus a file error. `layout list` and `apply` say `layouts.json has an error: <serde's text with line and column>`, and the `layouts` topic carries it. Keys and the lazy cockpit keep working with the presets.<br>• `layout.set` refuses such a file: `fix layouts.json first (<error>); nothing was saved`.<br>• **Otherwise `layout.set`** writes the file through `Effect::WriteFile { path, text, req }`. The daemon runs it before the effects after it: a temporary file in the same folder, then a rename. On failure, the daemon skips that op's `Publish` and `Reply` and refuses the request (`code::FAILED`, `couldn't save layouts.json: <error>`). The write keeps every top-level key and entry it doesn't change (`current` included), pretty-printed with 2 spaces like the spike's `json.dump(indent=2)`. Only layout ops publish `layouts`, and each reads the file first, so after a failed write (whose publish the daemon skipped) the next one shows the file as it really is. Accepted: a client that subscribes in between can see the unsaved entry until then.<br>• The current layout per brain is the `meta` value `layout.current`, defaulting to `cockpit`. A name no longer in the file, or a broken entry, falls back to `cockpit`. | Spec: `layouts.json` is shared config (§6.5 "Shared config"). Hot reload is slice 9; reading on each layout action gets hand edits in with no watcher. Brains share the file, so a per-file `current` would let one brain switch another's layout. A hand-edited file must never be lost to a `layout set`. |
| L3 | **Ops and verbs** (spec §6.4):<br>• `layout.list` → lines `▸ cockpit: tree · console · plan` (`▸` for the current one, a space otherwise, then a space), then `(config: <path, ~ for home>)`;<br>• `layout.show {name}` → the entry's JSON, pretty;<br>• `layout.set {name, layout}` → `saved layout <name>; apply it with: moa layout apply <name>`;<br>• `layout.apply {name?}` → `layout <name> applied`; with no name, the current one;<br>• `goto {panel}` → the leaf word;<br>• `ui.select_view {pane, view}` → `{pane, view}`.<br>`moactl layout list\|show NAME\|set NAME JSON…\|apply [NAME]` and `moactl goto PANEL` stop saying "not yet". `set` joins the remaining words with spaces, as the spike did. Writes: `layout.set` and `layout.apply` (the meta value). New topic `layouts`: rows `{key: name, name, leaves: [words], current: bool, error: string?}`, plus a row `{key: "file", error}` while the file doesn't parse. | Parity (R1 §2.2). The hint says "moa" where the spike said "moactl", since `moa` is the command users type. The topic lets the console (slice 6) and help (5c) show layouts without asking. |
| L4 | **tmux commands that move panes** join `Cmd`'s allow-list, each with a checked shape:<br>• `swap-pane -d -s %A -t %B`;<br>• `join-pane -d -h\|-v -s %A -t %B`;<br>• `break-pane -d -s %A -t =moa:\|=_moa: [-n NAME]`;<br>• `resize-pane -t %A -x\|-y N\|N%`;<br>• `respawn-pane -k -t %A [-c DIR] [-e K=V]… CMD…`;<br>• `rename-window -t %A NAME`.<br>Targets are pane ids only, never names, so a prefix can't match a stranger (research 2, item 9). `break-pane` also moves a pane that is alone in its window (verified on 3.4 and 3.8), so `move-window` isn't needed.<br>**`Cmd::chain(Vec<Cmd>) -> Cmd`** sends `a ; b ; …` as one line, checked part by part, with the existing sentinel after it. tmux runs the commands in order and skips the rest of the line after a failure. Control mode sends one block per command that ran (verified). The chain's result is the first error's text, or `Ok`. | Spec §6.6: "`a ; b` plus a sentinel for transactions"; "mutations batched into one write". Slice 3's T14 left this to its first consumer. A failed `join-pane` must not be followed by moves that assume it worked. |
| L5 | **Which pane plays each leaf** (`cockpit::assign`). A *panel* is a pane tagged `@moa_view=<view>`. Unassigned means no leaf has taken it yet. In order:<br>1. **`agent`:** the cockpit's spot occupant (an agent tagged `agent`, or an `empty-slot` panel, even a dead one); else an unassigned live `empty-slot` panel.<br>2. **Each panel leaf, in layout order:** an unassigned panel showing exactly that view: a live one first (in the cockpit, then parked), then a dead one. A **dead** panel counts here, and only here: it keeps its own leaf, moves like any pane, and is never reassigned.<br>3. **Every leaf still empty** (`agent` included) takes any unassigned live panel, cockpit first, reassigned to the leaf's view (`empty-slot` for `agent`).<br>4. **The rest** are new panels, started parked before the build. | Spec §6.13: "views it needs that aren't on screen come from parked panel panes … only the very first use of a view (or Repair after a crash) spawns a panel". Reassigning is a published row change, not a launch. Preferring an exact match keeps each panel's scroll and selection. A stopped panel staying in its place means only Repair starts a process for it (Call 5). |
| L6 | **The build, in place** (`cockpit::build`), as one chain from a fresh scene (L17):<br>1. **The keeper.** K is the first leaf's pane. If K isn't in the cockpit window, `swap-pane -d -s K -t C`, where C is a cockpit pane, an untagged one first.<br>2. **Clear out what the layout doesn't use.** Every cockpit pane that isn't assigned goes home with `break-pane -d -s P -t … [-n …]`: a panel goes to `=_moa:`; an agent to `=moa:`, named by its title; anything else to `=moa:`, named `⌨ shell`, plus `@moa_term 1` and `@moa_label '⌨ shell'`. **An agent** is a pane with a binding *or* a `@moa_session` tag: right after a restart, or between a sleep's commit and its vacate, the binding can be missing while the tag isn't. A pane carrying `@moa_session` never gets `@moa_term`. A pane tagged `@moa_view agent` with neither is "anything else", and its `@moa_view` is unset as it goes home.<br>3. **The swapped-out pane goes home too.** C now sits where K was. If that's the wrong session for C, it gets step 2's `break-pane`.<br>4. **Join breadth first.** For each split, join each later child's first pane after the previous child's first pane (`join-pane -d -h\|-v -s X -t Y`), then build each child inside its own cell. Panes already in the cockpit join the same way; tmux moves a pane within one window too (verified).<br>5. **Resize, children first** (the spike's `build`): after a split's children are built, `resize-pane -x\|-y <size>` on the first pane of each sized child. A percentage is a share of the window (verified), so the order doesn't change the result.<br>6. **Tag** (L10).<br>7. **Select** a pane only when the user asked: the `goto` leaf's pane, else the console's, else the first leaf's. A key and `layout.apply` ask; a lazy build, a refill and Repair's apply don't.<br>**When the cockpit window is missing** (session `=moa` exists, but no window there is named `moa`): step 1 becomes `break-pane -d -s K -t =moa: -n moa`, which makes K's window the cockpit (verified). When `=moa` itself is gone: nothing is done, and the reply is `moa isn't open in a terminal yet; run moa first`.<br>The cockpit window keeps its id, index and name. No pane is killed. Names: `break-pane -n` takes a name literally; `rename-window` and `@moa_label` expand `#`, so those go through `literal_format` (verified). | Spec: "`layout apply` **moves** existing panes (swap/join/break) and resizes, never restarts them"; "leftover agents go back to their own windows, named by title". The spike built a new window from `sleep` placeholders: those are launches, and the user's terminal jumped while the old window closed. Joining real panes needs no placeholders, and joining only inside each child's cell is what makes nesting come out right; on real tmux it gives the spike's geometry for all four presets (verified). Leaving kept panes in the window spares the spot's agent a detour through a tab of its own. |
| L7 | **Spawning a panel:** `new-window -d -P -F '#{pane_id}' -c <home> -t =_moa: -n <view> -e MOA_BRAIN=<brain> [-e MOA_ROOT=<root>] <moa exe> panel <view> --brain <brain>`, one command per panel, each answered on its own (L18). For every pane that started, the answer sets these, all `-p`: `@moa_view <view>`, `@moa_label <label>`, `remain-on-exit on`, and `remain-on-exit-format 'panel stopped · Ctrl-a r brings it back'` (3.4 has it, verified). The tag is recorded in the topology at once (as `tag_sent` does for `@moa_session`).<br>`Deps.panel: PanelLaunch { exe, env }` comes from the daemon: its own `current_exe`, and `MOA_ROOT` only when sandboxed. `State.panel_starts: u32` counts each new or respawned panel; `status` reports it as `panel_starts`.<br>A plan re-runs at most once after its spawns. If the second plan still needs a spawn, the reply is `couldn't start the <view> panel; see panel.log`. | Lesson 5: env goes explicitly, per pane. `-c <home>` matters because `moa panel` refuses a folder that's gone, and `_moa`'s folder is wherever moa first ran. A panel started with its view never flashes the picture first (5a carry). Counting panel starts makes "a layout switch starts nothing" checkable: the daemon's `Spawner` count doesn't see processes tmux starts. |
| L8 | **Who owns a panel's view** (5a's M6):<br>• **`ui.panel`** resolves the view as the pane's `@moa_view` tag (moa's assignment, read from tmux, so it survives a moa restart), else the view asked for, else `pane_views`, else `picture`.<br>• **`managed`** (the pane has the tag) is in the reply and in the panel's `PanelRow`.<br>• **At every listing** the link takes, each registered panel's view and `managed` are resolved again from the tags. `panels` is published when either changed, so a panel that registered before moa read its tag (after a restart, or right after its spawn) is corrected.<br>• **`ui.select_view {pane, view}`** sets the tag, the registry and `pane_views`, re-tags the frame (L10), and publishes `panels`.<br>• **A panel follows its own row** (its `key` from the `ui.panel` reply). When the row's view or `managed` changes, it calls `Panel::follow`.<br>• **When the tmux server changes**, registry entries lose their pane (`pane: None`), as `pane_views` already does.<br>• **The tag is `@moa_view`, not the spike's `@moa_panel`**, so a server adopted from the spike (slice 10) never mistakes the spike's Python panels for v1 panels. The agent in the spot carries `@moa_view agent`. | A reassignment made while a panel was away must not be undone by the panel's stale memory. The tag lives with the pane in tmux, so both a reconnect and a restarted daemon can read it. A row change can't be missed the way a one-off message can (5a's V3). |
| L9 | **The lazy cockpit [C28]:** on `client-attached`, and on a link-up, the core queues a lazy build (L17). In its listing, if a non-control client is attached (`client_control_mode` 0) and the cockpit holds no tagged pane, it builds the current layout, falling back to `cockpit` when that entry is broken. Otherwise it does nothing. A stranger pane there (today's login shell) goes home as `⌨ shell` (L6 step 2). Nothing is selected. Until slice 6 the `console` leaf is an ordinary panel with the console view, which reads `the console isn't in this build yet`. | Spec: "the daemon builds the cockpit when a user first attaches, not at login". `client-attached` also fires for control clients (verified), and the front door creates session `moa` and attaches in one step. Only a fresh listing knows both. Never kill what the user started. |
| L10 | **Tags and frames** (the spike's `tag`; spec "Frames"):<br>• **A panel pane:** `@moa_view <view>` and `@moa_label <View::label>`. If `View::framed`, also `@moa_framed 1`, `window-style fg=#4c566a` and `window-active-style fg=#88c0d0`, all pane-level; otherwise these three are unset.<br>• **An agent in the spot:** `@moa_view agent` and `@moa_label '● <title>'`; the framed options are unset.<br>• **An agent leaving the spot:** `@moa_view` and `@moa_label` are unset, and its window takes its title. Titles are cut to 24 characters, the spike's cut.<br>Every tag is a `set-option -p` inside the operation's chain. | Lessons 11–12: the frame colour follows focus through tmux's own pane styles, with no events. |
| L11 | **The agent spot**, each operation one chain from a fresh scene:<br>• **`focus Q`** of a live session whose pane P isn't the spot, when the layout has a spot S: `swap-pane -d -s P -t S`, tag P, then `select-window`/`select-pane` P.<br>• **The old occupant:** an empty slot goes to `_moa` (`break-pane -d -s S -t =_moa:`). An agent stays in P's old window, which takes its title.<br>• **With no spot, or when P already is the spot:** 4b's `focus_cmds`, with 4b's reply.<br>• **Everything that shows a session goes through this:** `focus`, an `open` or `resume` once its tab is up, and the tree's Enter, all via `show(state, pane, tell)`.<br>• **Taking a session out of the spot** (`kill_tab` for sleep, close or restart): if an unassigned `empty-slot` panel is parked, `swap-pane -d -s <slot> -t S ; kill-pane -t S`; else `respawn-pane -k -t S -c <home> -e … <moa> panel empty-slot --brain …`, then `set-option -p -u -t S @moa_session`, the slot's tags, `remain-on-exit on` and its format (an agent's pane never had them, and without them a crashed slot would vanish, verified). The second is the spike's way: one command, no wait (respawning fires no exit hook and keeps pane options, verified), and it counts in `panel_starts`. `kill_tab` stays synchronous.<br>• **When an agent's pane in the spot exits by itself** (judged from its `@moa_view agent` tag, read before `pane_gone` deletes the binding), the core re-applies the current layout, which puts an empty slot there. A dead *panel* in the spot (the empty slot included) is left for Repair.<br>• **`$` with no `ui.here`** opens in the spot agent's folder, else home. | Spec: "`focus Q` resumes if needed and swaps the session into the agent spot (the previous occupant returns to its window; an empty slot is parked)"; "Sleeping: in the agent spot → park an empty slot there first". Every sleep, close and restart path saves first and kills after (`sleep.rs`, `ops/sessions.rs`, `ops/launch.rs`). An in-place respawn keeps that order with no wait in between. Re-applying only for an agent's exit keeps a crashing empty slot from looping. The spike's `here_dir` fell back to the spot's agent. |
| L12 | **Supervision and Repair:**<br>• A `pane-died` for a tagged panel logs `panel <view> in %N stopped` (info, once per death), and the pane stays, showing L7's format.<br>• **Repair** (`Ctrl-a r`, prefix `C`, the menu) respawns every dead pane tagged with a *panel* view (never `agent`) in place: `respawn-pane -k -t %N -c <home> -e … <the panel command>`, counted in `panel_starts`. It then re-applies the current layout from a listing taken after the respawns (L17), and says `repaired: <n> panels restarted`, or `layout <name> applied` when none was dead.<br>• **`goto PANEL`** selects that panel in the cockpit. If the panel is missing and the current layout has it, the layout is re-applied first, then that panel is selected. If the layout lacks it: `the <layout> layout has no <panel> panel (moa layout list)`. `agent` also finds the empty slot. | Spec §6.13 ("a dead panel is logged and respawned on Repair", "`goto PANEL` selects a panel, re-applying the layout if it's missing"); spike `goto`. A user's own `remain-on-exit on` can leave a dead agent in the spot; Repair isn't the place to restart an agent. |
| L13 | **Keys:**<br>• `c t p g` → `goto console\|tree\|plan\|agent`;<br>• `1 2 3 4` → `layout.apply cockpit\|focus\|classic\|triage`;<br>• `r` and prefix `C` → Repair.<br>Each answers by a notice to the client that pressed it (`Tell::Notice`). `Find`, `FindOther`, `New` and `Help` keep their not-yet notices (5c). The core takes every key in one dispatch (`keys::run`), ahead of the link's not-yet fallback. | Spec §6.13 key table. 5a's `core.rs` comment: "as more keys land, they belong in one dispatch here". |
| L14 | **The panel process** (5a carries):<br>• **Ctrl-c** cancels an open prompt or y/N everywhere. With nothing open, it quits only an unmanaged panel; a managed one (L8) shows Call 4's line.<br>• **SIGTERM, SIGHUP and SIGINT** restore the terminal and exit 0.<br>• **`FocusGained`** redraws and re-arms the deadline (M7).<br>• **`panel.log`** in the brain's log folder gets one line per start, give-up, signal and panic, each with the pane and the view.<br>• **With no view asked and no link yet**, nothing is drawn (no picture flash). | 5a's final review (recommendations 3 and 5, and M7). tmux sends SIGHUP to a pane it kills. A managed panel that quit would leave a dead pane in the layout. |
| L15 | **No stray servers:** `moa_client`'s autostart, and with it the panel's first connect and every `moactl`, refuses when `Paths`'s root folder doesn't exist: `moa's folder <root> is gone`. Task 2 hunts the other sources of the stray `/tmp/moaXXXX` servers. Each source it finds gets a fix, plus a test that would have caught it. | 5a's Task 14 carry: a test failing between `panel_tab` and the panel's first connect let that connect start a daemon under a deleted root. A user's real root always exists. |
| L16 | **The topology learns the tags:** `LIST_PANES` gains `#{pane_dead}` and `#{@moa_view}`, and `LIST_CLIENTS` already has `client_control_mode`. Renames are applied straight from the notification (`%unlinked-window-renamed @w name` for windows outside `_moa`, `%window-renamed` inside it, `%session-renamed`), without a full listing. That removes 4b's "listing per tab rename" cost. `Sandbox::tmux` looks tmux up once. (`Topology::session_named` stays public: the integration tests use it.) | The scene needs tags and liveness. 4b and slice 3 carried the rename cost and the cleanup to this slice. |
| L17 | **One cockpit operation at a time, each from its own listing:**<br>• **One queue, `State.cockpit`.** These all go through it:<br>&nbsp;&nbsp;– builds: apply, goto's re-apply, Repair, the lazy build, the refill;<br>&nbsp;&nbsp;– swaps (`show`) and `goto`'s select;<br>&nbsp;&nbsp;– `ui.select_view`;<br>&nbsp;&nbsp;– vacates (`kill_tab` of an agent, below).<br>• **A newer `Build` replaces a waiting `Build`**, and only that. The replaced request is answered `replaced by a later layout request`. Repair, Lazy and Refill are never replaced.<br>• **Each operation starts with a fresh listing:** it asks `refresh_cmds()` (sessions, panes and clients) as `Asked::Scene { op }`. That answer rebuilds the topology exactly as `listed()` does (keeping the tags sent but not yet listed), then runs `resolve_panels` (L8). The operation plans in that answer, sends its chain as `Asked::Chain { op }`, and only when the chain's answer arrives does the next operation start.<br>• **A chain's `Ok` records its tag changes in the topology at once**, as `tag_sent` does, so the topology knows which pane is the spot before the next refresh.<br>• **An answer for an operation that isn't the one in flight is ignored.**<br>• **`Input::Reload`** (after a panic) resets the queue. Waiting `Vacate`s are queued again, since their commits are already saved. Every other waiting request is answered `moa lost its terminal; try again`.<br>• **tmux runs one client's commands in order**, so a listing sent after a chain sees that chain's result. It costs one round trip, about 1 ms.<br>• **If the link goes down**, every waiting operation except `Vacate`s is dropped: its request is answered `moa lost its terminal; try again`, and a key's with nothing. `Vacate`s run when the link comes back to the same server. On another server their panes are gone anyway.<br>• **`kill_tab` of a bound agent** goes through the queue as `Vacate { pane }` whenever a cockpit exists. A vacate whose fresh scene shows the pane in the spot uses L11's chain; one elsewhere, a plain `kill-pane`; one whose pane is gone, nothing. With no cockpit, `kill_tab` stays as it is today. | The topology catches up only through refreshes, which arrive after the chain that caused them, and `%layout-change` never comes for the cockpit (it's outside `_moa`). A plan from a stale scene can move a whole window that it believes holds one pane, dragging an agent into `_moa`. Two keys pressed quickly, or a Repair whose respawned panes still read as dead, hit exactly that. |
| L18 | **Answers keep tmux's words, per command:** the new `Asked` variants (`Scene`, `Spawn`, `Chain`) get `Vec<Result<Vec<String>, String>>`, one entry per `Cmd` sent (a chain is one `Cmd`, so one entry), with tmux's error text. A failed spawn never hides the panels the other commands started: each started pane is tagged. A chain's error reaches the user in plain words:<br>• `layout <name> couldn't be applied: <tmux's words>; Ctrl-a r tries again`;<br>• tmux's `no space for a new pane` and `pane too small` become `the <name> layout doesn't fit this window; make the terminal bigger`. | `Answer` (`link.rs:186`) drops the error text today. An untagged `moa panel` would run in `_moa` for ever, never reused. |

## Parity rows this part delivers (spec §7)

"Layout tree, validation, presets, merge"; "`layout apply/list/show/set`"; "`goto`, `focus`,
agent spot swap"; "Panel tagging, labels, frame colours"; "Repair, cockpit split refused" (the
refusal is already in `moa.tmux`). The picker and help are 5c's.

## Global Constraints

The roadmap's, every earlier slice plan's, and 5a's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **Never run cargo while an e2e test runs.** Relinking `target/debug/moa` breaks the running
  tests. Run a test binary directly, or use another `CARGO_TARGET_DIR`.
- **Zero launches for a layout switch, a `goto`, a `focus` and a Repair with nothing dead**,
  once the panels exist:
  - neither the daemon's `Spawner` count nor `panel_starts` changes;
  - every panel's `pane_pid` is the same before and after.
- **No pane is ever killed by a layout change or a focus.** The one exception is an agent that
  sleep, close or restart is taking down, and only in the same chain that puts the empty slot
  in its place.
- **tmux targets are pane ids** (`%N`) or the exact sessions `=moa:` and `=_moa:`, never a bare
  name.
- **The core stays pure:** file reads only through `Deps` functions, writes only through
  `Effect::WriteFile`.
- **Ops that save nothing send their own `Effect::Tmux`.** `post_commit` drops tmux commands
  when nothing was saved (`core.rs:463-477`). This applies to `ui.select_view`, the swap and
  `goto`.
- **Tests:**
  - only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too;
  - never a tmux server named `moa` or `moa-work`, never a real brain;
  - `fake-claude` is the only agent;
  - every wait goes through `wait_for` with a 10 s limit, never a sleep.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`, `~/private`.
- **Plain words** in every message: "tab", "folder", "panel", "layout", never "pane" or
  "window" (lesson 35).

## Review Focus

1. **A switch, focus or vacate with agents and strangers in odd places.** The cases: an agent
   in the spot; a user's split shell inside an agent's tab; a stranger pane in the cockpit; a
   dead panel; an agent tagged `agent` outside the cockpit (left by a failed chain). Expected:
   - nothing is killed;
   - every agent ends in its own tab, named by its title;
   - strangers never enter `_moa`;
   - the cockpit holds exactly the layout's leaves.

   Pinned by `no_sequence_kills_or_strands_a_pane` (Task 5, a proptest over sequences of plan,
   swap-in and vacate) and `a_layout_switch_moves_panes_and_starts_nothing` (Task 12).
2. **Keys faster than tmux, or tmux refusing mid-switch** (two layout keys at once; a pane gone
   between the listing and the chain). Expected:
   - one operation at a time, each from a fresh listing;
   - a refusal stops the chain and is said in plain words;
   - the next apply mends the cockpit.

   Pinned by `two_layout_keys_run_one_after_the_other` and
   `a_failed_build_says_so_and_the_next_apply_mends_it` (Task 7), and
   `a_chain_stops_at_its_first_error` (Task 4).
3. **moa restarts, or a panel connects before moa read its tag.** Expected: the panel ends up
   with moa's view and `managed`. Pinned by
   `a_listing_corrects_a_panel_that_registered_too_early` (Task 8) and
   `moas_view_wins_after_moa_restarts` (Task 12, with the panel's memory and the tag made to
   differ).
4. **First attach: two users at once, a control client, or the attach arriving before moa
   knows session `moa`.** Expected: one cockpit, built once, only for a real user. Pinned by
   `two_attaches_build_the_cockpit_once` and `a_control_client_attaching_builds_nothing`
   (Task 7), and `the_first_attach_builds_the_cockpit` (Task 12).
5. **A broken `layouts.json` or a missing cockpit window.** Expected:
   - the presets still work, the error is said, and the file is never overwritten;
   - a missing cockpit is rebuilt in session `moa`.

   Pinned by `a_broken_file_keeps_the_presets_and_is_never_overwritten` (Task 3) and
   `a_missing_cockpit_window_is_made_again` (Task 7).

---

## Task 1: the terminal code in its own file

**Files:**
- Create: `crates/moa-core/src/ops/term.rs`
- Modify: `crates/moa-core/src/ops/launch.rs` (remove the moved code), `crates/moa-core/src/ops/mod.rs`,
  and every `use` of the moved items.

**Interfaces:**
- Moves: `Tell`, `told`, `term_key`, `here_answered`, `term`, `term_opened` and their private
  helpers. Callers reach them as `ops::term::…`.
- Unchanged: `focus_cmds` and `focus` stay in `launch.rs`.

- [ ] **Step 1: Move the items, unchanged.** No other change in this commit. `git diff -M --stat`
  shows the move, and the bodies are identical.
- [ ] **Step 2: Run** `nix develop --command cargo nextest run --workspace` → all pass, with the
  same count as before.
- [ ] **Step 3: Commit:** `v1: the terminal code moves to ops/term.rs`.

## Task 2: no stray test servers

**Files:**
- Modify: `crates/moa-client/src/lib.rs` (the autostart check), plus the testkit and test
  helpers where the hunt points.
- Test: `crates/moa/tests/client.rs`, plus the guard named in Step 3.

**Interfaces:**
- Produces: the autostart refusal `moa's folder <root> is gone` (a `ClientError` variant that
  shows that text).

- [ ] **Step 1: Write the failing test** `autostart_refuses_a_root_that_is_gone` (`client.rs`):
  1. Build `Paths` under a `/tmp` folder, then remove the folder.
  2. `Client::connect` with `autostart: Some(exe)` → `Err`, whose text is
     `moa's folder <root> is gone`.
  3. `spawn::launches()` hasn't changed, and the folder still doesn't exist.
- [ ] **Step 2: Run it to see it fail:**
  `nix develop --command cargo nextest run -p moa --test client autostart_refuses` → a daemon
  starts and recreates the folder.
- [ ] **Step 3: Hunt the other sources.**
  - Record the tmux sockets and `moa daemon` processes under `/tmp/moa*` before and after a full
    `cargo nextest run --workspace`.
  - Attribute each new leftover to a test, by its root's name and its log.
  - Fix each source at its root cause (for example a helper that starts a server outside
    `TempBrain`, or a `Drop` that runs before a child's first connect).
  - For each source, add a guard test that fails if it comes back. If the hunt finds no other
    source, the guard is `a_full_e2e_file_leaves_no_server_behind`: it runs `panel_e2e`'s
    helpers on a `TempBrain`, drops it, and finds no socket and no daemon under that root.
  - Record the method and findings in the report.
  - **Never touch servers outside the test's own roots.** The 8 stray servers already on this
    machine are not this task's to clean up.
- [ ] **Step 4: Implement the check.** In the autostart path, the root folder must exist before
  anything is spawned or created.
- [ ] **Step 5: Run the tests to see them pass.** Then run the whole workspace twice, recording
  the leftovers after each run: there should be none.
- [ ] **Step 6: Commit:** `v1: moa never starts in a folder that's gone; no stray test servers`.

## Task 3: layouts

**Files:**
- Create: `crates/moa-core/src/layout.rs`, `crates/moa-core/src/ops/layout.rs`
- Modify:
  - `crates/moa-core/src/{lib.rs, op.rs, state.rs, effect.rs, publish.rs, change.rs}`;
  - `crates/moa-core/Cargo.toml` (`indexmap` with `serde`);
  - `crates/moa-daemon/src/{run.rs, core_task/*}` (`Deps.read_text`, running `Effect::WriteFile`);
  - `crates/moa-proto/src/cli.rs`: `Verb::Layout`'s `list`, `show` and `set` become ops;
    `apply` and `Goto` stay in the not-yet list until Task 7;
  - `crates/moa-proto/tests/snapshots/` (the schema).
- Test: `crates/moa-core/tests/layout.rs` (new), `crates/moa-core/tests/skeleton.rs`,
  `crates/moa-proto/tests/cli.rs`, and a daemon test for the write.

**Interfaces:**
- Produces, in `moa_core::layout`:
  - `pub enum Axis { H, V }`;
  - `pub enum Size { Cells(u16), Percent(u8) }` with `fn arg(&self) -> String` (`"26%"` or
    `"40"`);
  - `pub enum Leaf { Panel(View), Agent }`, parsing and printing the nine words;
    `pub const LEAVES: [&str; 9]` holds them in L1's order;
  - `pub enum Node { Split { axis: Axis, children: Vec<Node>, size: Option<Size> }, Leaf { leaf: Leaf, size: Option<Size> } }`;
  - `pub fn parse(v: &serde_json::Value) -> Result<Node, LayoutError>`; `LayoutError` displays
    the spike's texts with paths (`layout`, `layout.children[2]`);
  - `pub fn leaves(n: &Node) -> Vec<Leaf>`;
  - `pub struct Layouts { pub file: serde_json::Map<String, serde_json::Value>, pub entries: IndexMap<String, serde_json::Value>, pub file_error: Option<String> }`
    (`file` is every top-level key of the file as read):
    - `pub fn load(text: Option<&str>) -> Layouts`: the merge, and the presets alone with
      `file_error` when the text isn't JSON;
    - `fn node(&self, name: &str) -> Result<Node, LayoutError>`;
    - `fn names(&self) -> impl Iterator<Item = &str>`;
    - `fn file_text_with(&self, name: &str, layout: &serde_json::Value) -> Result<String, LayoutError>`,
      an `Err` while `file_error` is set; it writes the file's own keys and entries plus this
      one, never the merged presets;
  - `pub fn presets() -> Layouts`; `pub const PRESET_KEYS: [&str; 4] = ["cockpit", "focus", "classic", "triage"]`.
- Produces in the core:
  - `Op::LayoutList`, `Op::LayoutShow { name: String }`,
    `Op::LayoutSet { name: String, layout: serde_json::Value }` (methods `layout.list`,
    `layout.show`, `layout.set`), in `METHODS`; `LayoutSet` is in `WRITES`;
  - `Effect::WriteFile { path: PathBuf, text: String, req: Option<(ConnId, ReqId)> }` (L2);
  - `Deps.read_text: fn(&Path) -> io::Result<Option<String>>`, `Deps.layouts_file: PathBuf`;
  - `State.layouts: Layouts`;
  - `fn current_layout(&State) -> String` (L2's fallbacks);
  - `Topic::Layouts`, with `LayoutRow { name, leaves: Vec<String>, current: bool, error: Option<String> }`
    and the `file` row.
- `layout.apply` and the meta write come in Task 7.

- [ ] **Step 1: Write the failing tests** in `layout.rs`:
  - `presets_are_the_spikes`: `presets()` parses to exactly the spike's four trees (sizes `26%`,
    `30%`, `24%`, `45%`, `42%`, `40%` and `28%`), in the order cockpit, focus, classic, triage.
  - `a_file_keeps_its_order_and_gets_missing_presets_first`: the file
    `{"layouts":{"mine":…,"cockpit":…}}` → names `focus, classic, triage, mine, cockpit`.
  - `validation_says_what_the_spike_said`, one case each:
    - `{"panel":"tre"}` → `layout: unknown panel 'tre' (panels: console, tree, today, activity, projects, plan, status, picture, agent)`;
    - a split with one child → `layout: a split needs at least 2 children`;
    - `{"split":"x",…}` → `layout: needs "panel", or "split": "h" (side by side) / "v" (stacked)`;
    - two consoles → `layout: at most one console and one agent panel`;
    - `"size":"0%"`, `"size":0`, `"size":30.0` and `"size":true` in the first child →
      `layout.children[0]: size must be a number of cells or like "30%"`.
  - `a_broken_entry_doesnt_take_the_others_down`: with one bad entry, `names()` lists it,
    `node("bad")` is an `Err` and `node("cockpit")` is `Ok`.
  - `a_broken_file_keeps_the_presets_and_is_never_overwritten`: the text `{"layouts": {` gives
    the four presets and a `file_error` naming line 1. Then:
    - `LayoutList` lists the presets and the line `layouts.json has an error: …`;
    - `LayoutSet` is refused with `fix layouts.json first (…); nothing was saved`, and there is
      no `WriteFile`.
  - `layout_list_marks_the_current_one`: after the meta `layout.current = focus`, the reply text
    starts with `  cockpit: tree · console · plan`, then `▸ focus: tree · plan · agent · console`.
    The two other presets follow in the same form, and it ends
    `(config: ~/.config/moa/layouts.json)`.
  - `layout_set_validates_writes_and_publishes`: `LayoutSet { name: "mine", layout: <valid> }`
    → the effects, in order:
    1. `WriteFile`, whose text is 2-space JSON with the file's other keys and entries as they
       were, plus `mine`;
    2. a `Publish` of `layouts`;
    3. the `Reply` `saved layout mine; apply it with: moa layout apply mine`.

    An invalid layout is refused with the validation text, and nothing is written.
  - `layout_ops_read_the_file_again`: when `Deps.read_text` returns a new text between two
    `LayoutList`s, the second lists the new entry.
  - Daemon: `a_failed_write_refuses_the_request`: an unwritable folder → the request gets
    `couldn't save layouts.json: …`, with no `saved` reply and no publish. In the same file,
    `a_write_replaces_the_file_atomically`.
  - `cli.rs`: `layout_verbs_parse`:
    - `layout list`, `layout show cockpit`, and `layout set mine {"panel": "tree"}` (the words
      after the name joined with spaces) all parse;
    - `layout show` → `usage: moa layout show NAME`.
- [ ] **Step 2: Run them to see them fail:**
  `nix develop --command cargo nextest run -p moa-core --test layout` → compile errors.
- [ ] **Step 3: Implement** the interfaces. The daemon reads `layouts.json` at start and passes
  the text to the core. It runs `WriteFile` by L2. Update the schema snapshot
  (`cargo insta review`); only the new methods, the topic and the effect may change.
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: layouts: the spike's file, presets, list, show and set`.

## Task 4: tmux commands that move panes

**Files:**
- Modify:
  - `crates/moa-core/src/tmux/cmd.rs` (allow-list, shapes, `chain`);
  - `crates/moa-core/src/tmux/link.rs` (the per-command answer type for new `Asked` variants, L18);
  - `crates/moa-tmux/src/client.rs` (a chain gets the sentinel and answers once: `Ok`, or its
    first error's text);
  - `crates/moa-core/src/tmux/topology.rs` (L16);
  - `crates/moa-testkit/src/sandbox.rs` (tmux looked up once);
  - `crates/moa-tmux/src/conf.rs` (5a's mouse-line alignment: one space, `conf.rs:98`).
- Test: `crates/moa-core/tests/tmux_vocab.rs`, `crates/moa-tmux/tests/client.rs`,
  `crates/moa-core/tests/tmux_focus.rs`.

**Interfaces:**
- Produces:
  - `Cmd::check` accepts L4's six shapes and refuses every other form of them, in the style of
    the existing `Refused` texts;
  - `pub fn chain(cmds: Vec<Cmd>) -> Cmd`: one line, ` ; `-joined; `nests()` is true; each part
    is checked; an empty chain is refused. `Cmd::parts(&self) -> &[Cmd]` gives a chain's parts
    (a plain command is its own one part), so the tests' simulator can run them;
  - `pub(crate) type Answers = Vec<Result<Vec<String>, String>>`, the per-command form L18 uses;
  - Topology:
    - `pub fn view_of(&self, p: PaneId) -> Option<&str>` (the raw tag);
    - `pub fn is_dead(&self, p: PaneId) -> bool`;
    - `pub fn session_name_of(&self, p: PaneId) -> Option<&str>`;
    - `pub fn cockpit(&self) -> Option<WindowId>`;
    - `pub fn panes_in(&self, w: WindowId) -> impl Iterator<Item = PaneId>`;
    - `pub fn users_attached(&self) -> bool`, which already exists and counts non-control
      clients only.

- [ ] **Step 1: Write the failing tests:**
  - `the_pane_moving_commands_take_only_their_safe_shapes` (`tmux_vocab.rs`):
    - each L4 shape passes;
    - each of these is refused: `swap-pane` without `-d`; a `join-pane` target by name
      (`-t moa`); `break-pane -t moa:` (not `=moa:`); `break-pane -t _moa2:`; `resize-pane`
      without `-x`/`-y`; `respawn-pane` without `-k`; `move-window` (not allowed at all).
  - `a_chain_is_one_line_and_checks_every_part`:
    `chain([select-pane -t %1, swap-pane -d -s %1 -t %2]).line()` is
    `select-pane -t %1 ; swap-pane -d -s %1 -t %2`; a chain holding a refused part is refused.
  - `a_chain_stops_at_its_first_error` (`client.rs`, real tmux): send this chain:
    1. `set-option -p -t %0 @a 1`
    2. `swap-pane -d -s %0 -t %999`
    3. `set-option -p -t %0 @b 1`

    The result is `Err` with tmux's text naming `%999`. `@a` is set and `@b` isn't, and the next
    command's reply is its own.
  - `renames_apply_without_a_listing` (`tmux_focus.rs`): `%unlinked-window-renamed @2 moa` on a
    window in session `moa` → `Applied::Changed` and `is_cockpit(@2)`. Renaming it away clears
    it. Neither returns `Applied::Refresh`. `%session-renamed` of `moa` clears the cockpit flag.
  - `the_listing_reads_tags_and_liveness`: a `LIST_PANES` line with `@moa_view tree` and dead
    `1` → `view_of` and `is_dead` answer from it.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** A window's session comes from the pane lines. Keep the window → session
  map that the cockpit flag needs.
- [ ] **Step 4: Run them to see them pass**: the workspace, then `moa-tmux` and `moa-core` on
  tmux 3.4.
- [ ] **Step 5: Commit:** `v1: tmux commands that move panes, chains, and tags in the topology`.

## Task 5: the cockpit engine

**Files:**
- Create: `crates/moa-core/src/cockpit/{mod.rs, scene.rs, assign.rs, build.rs}`
- Test: `crates/moa-core/tests/cockpit.rs` (new) and `crates/moa-core/tests/cockpit/sim.rs` (the
  simulator; it reads only `Cmd`'s public `parts()`, `name()` and `argv()`).

**Interfaces:**
- Produces:
  - `pub enum Role { Panel(View), Spot(Option<SessionId>), Agent(SessionId), Other }`:
    - an *agent* is a pane with a binding or a `@moa_session` tag (L6 step 2); its session id
      comes from whichever it has;
    - `Spot` is the cockpit's spot occupant: `Spot(Some)` an agent tagged `@moa_view agent`,
      `Spot(None)` an `empty-slot` panel there;
    - an `empty-slot` panel outside the cockpit is `Panel(EmptySlot)`;
    - an agent anywhere else, tagged `agent` or not, is `Agent`;
    - a pane tagged `@moa_view agent` that isn't an agent is `Other`.
  - `pub struct PaneAt { pub pane: PaneId, pub window: WindowId, pub place: Place, pub dead: bool, pub role: Role }`,
    with `pub enum Place { Cockpit, Moa, Hidden, Elsewhere }`.
  - `pub struct Scene { pub panes: Vec<PaneAt>, pub moa_session: bool, pub cockpit: Option<WindowId>, pub users: bool }`,
    built by `Scene::read(topology: &Topology, bound: impl Fn(PaneId) -> Option<SessionId>) -> Scene`
    from a topology that L17's fresh listing just rebuilt. A pane is an agent when `bound` gives
    a session, else when its `@moa_session` tag (`Topology::moa_session`) does (L6 step 2).
  - `pub struct Plan { pub spawn: Vec<View>, pub build: Option<Build> }`. `build` is `None`
    while `spawn` isn't empty, and `None` with an empty `spawn` when there's no session `moa`.
  - `pub struct Build { pub chain: Cmd, pub views: Vec<(PaneId, View)>, pub leaves: Vec<(Leaf, PaneId)>, pub spot: Option<PaneId> }`.
  - `pub fn plan(scene: &Scene, layout: &Node, title: impl Fn(SessionId) -> String, home: &Path) -> Plan`,
    implementing L5, L6 and L10. Selecting, by L6 step 7, is the caller's job, from `leaves`.
  - `pub fn swap_in(scene: &Scene, pane: PaneId, title: …) -> Option<Cmd>`: L11's swap and the
    old occupant's handling, as one chain. `None` means no spot, or the pane already is the
    spot.
  - `pub fn vacate(scene: &Scene, spot: PaneId, respawn: &[String]) -> Cmd`: L11's two forms,
    a spare slot swapped in or an in-place respawn, each followed by its kill or untag.
  - In the tests: `sim::World::from(&Scene)`; `World::run(&Cmd)`, which models `swap-pane`,
    `join-pane` (also within one window), `break-pane` (also of a pane alone in its window,
    which moves the window), `respawn-pane -k`, `kill-pane` and `set-option -p` over membership
    (which pane is in which window and session; no geometry); and `World::cockpit_leaves()`.

- [ ] **Step 1: Write the failing tests** (`cockpit.rs`):
  - `the_first_cockpit_spawns_its_three_panels`: a scene with only the login shell in the
    cockpit → `spawn == [Tree, Console, Plan]` and `build == None`.
  - `with_panels_parked_the_cockpit_builds_in_one_chain`: tree, console and plan parked in
    `_moa`, and the shell in the cockpit. The chain's commands, in order:
    1. swap the tree with the shell;
    2. `break-pane -d -s <shell> -t =moa: -n '⌨ shell'`, then the shell's two `set-option`s;
    3. `join-pane -d -h -s <console> -t <tree>`, then `join-pane -d -h -s <plan> -t <console>`;
    4. `resize-pane -t <tree> -x 26%`, then `resize-pane -t <plan> -x 30%`;
    5. the tags (L10).
  - `the_focus_layout_nests`: the `focus` preset from parked panels.
    - The joins, in order:
      1. `-h` the spot's pane after the tree;
      2. `-v` the plan under the tree;
      3. `-v` the console under the spot.
    - The resizes, children first: `-y 45%` on the plan, then `-y 30%` on the console, then
      `-x 24%` on the tree.
  - `a_switch_reuses_panels_before_starting_one`: from a built `cockpit` to `triage` → the tree
    panel is reassigned (`views == [(tree's pane, Status)]`), with no spawn.
  - `kept_panes_stay_in_the_window`: a re-apply of the same layout with an agent in the spot →
    no `break-pane` of the agent; the agent's pane is joined within the window.
  - `the_spot_agent_goes_home_when_the_layout_has_no_spot`: from `focus` with Lantern in the
    spot to `cockpit` → `break-pane -d -s <agent> -t =moa: -n Lantern`, and the agent's tags
    unset.
  - `a_dead_panel_keeps_its_leaf`: a dead `plan` panel in the cockpit, switching to `classic` →
    the dead pane is assigned to `plan` and joined in its place; nothing is spawned for `plan`;
    the dead panel is never in `views`.
  - `a_title_with_a_hash_is_literal`: the title `fix #12` → `break-pane … -n 'fix #12'`, while
    `rename-window` and `@moa_label` get `fix ##12`.
  - `no_moa_session_builds_nothing`; `a_missing_cockpit_is_made_from_the_first_leaf` (step 1
    becomes `break-pane -d -s <tree> -t =moa: -n moa`).
  - `swap_in_parks_the_empty_slot`: the empty slot in the spot, and agent `%9` in its tab
    (alone, or sharing it with a split shell) → `swap-pane -d -s %9 -t <slot>`, then
    `break-pane -d -s <slot> -t =_moa:`, then the tags.
  - `swap_in_sends_the_old_occupant_home_by_title`: Kite in the spot, Lantern focused → the
    swap, then `rename-window -t <Kite's pane> Kite` and Kite's tags unset.
  - `vacate_swaps_a_spare_or_respawns_in_place`: with a parked `empty-slot` →
    `swap-pane -d -s <slot> -t %9 ; kill-pane -t %9`. With none →
    `respawn-pane -k -t %9 -c <home> -e … <moa> panel empty-slot --brain default`, then
    `set-option -p -u -t %9 @moa_session`, then the slot's tags, `remain-on-exit on` and its
    format.
  - `an_unbound_agent_with_its_tag_goes_home_by_title`: Lantern in the spot, its binding
    missing but its `@moa_session` tag present, switching to `cockpit` →
    `break-pane … -n Lantern`, with no `@moa_term` on it. An untagged, unbound pane tagged
    `@moa_view agent` goes home as `⌨ shell`, and its `@moa_view` is unset.
  - The proptest `no_sequence_kills_or_strands_a_pane`.
    - **Scenes are random:**
      - 0–3 agents in their own tabs, 0–2 of them with a split shell in the tab;
      - one agent, or none, in the spot;
      - 0–1 agents tagged `agent` outside the cockpit;
      - 0–2 strangers in the cockpit;
      - 0–5 panels in random places, some dead.
    - **Sequences are random:** 1–6 steps of `plan(random preset)`, `swap_in(random live agent)`
      and `vacate(spot)`. Each step's spawns are added as parked panes, and its chain runs in
      `World`.
    - **After each step:**
      1. no pane is lost, except the agent a `vacate` kills;
      2. after a `plan`, the cockpit holds exactly one pane per leaf;
      3. every agent outside the cockpit is in session `moa`;
      4. every panel outside the cockpit is in `_moa`;
      5. no stranger is ever in `_moa`;
      6. no dead pane is reassigned, killed or respawned.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** L5, L6, L10 and L11's chains. `title` cuts to 24 characters.
- [ ] **Step 4: Run them to see them pass**: the proptest at its default case count, plus one
  10 000-case run recorded in the report.
- [ ] **Step 5: Commit:** `v1: the cockpit engine: assign, build, swap in, vacate`.

## Task 6: moa starts its panels

**Files:**
- Modify:
  - `crates/moa-core/src/{state.rs, tmux/link.rs (Asked::Spawn), cockpit/mod.rs}`;
  - `crates/moa-daemon/src/{run.rs (Deps.panel), session.rs (status)}`;
  - `crates/moa-proto/src/wire.rs` (`StatusOk.panel_starts`, `#[serde(default)]`).
- Test: `crates/moa-core/tests/cockpit_run.rs` (new), `crates/moa-daemon/tests/tmux.rs`.

**Interfaces:**
- Produces:
  - `pub struct PanelLaunch { pub exe: PathBuf, pub env: Vec<(String, String)> }` in `Deps.panel`;
  - `pub(crate) fn panel_argv(state: &State, view: View) -> Vec<String>`
    (`<exe> panel <view> --brain <brain>`), used by spawn, Repair and `vacate`;
  - `Asked::Spawn { views: Vec<View>, op: CockpitOp }`, answered per command (L18);
  - `pub(crate) fn spawn_cmds(state: &State, views: &[View]) -> Vec<Cmd>` (L7);
  - `State.panel_starts: u32`.
  - On the answer, every started pane is tagged (L7) and recorded in the topology. The
    operation then plans again from a fresh listing (L17), at most once (L7).

- [ ] **Step 1: Write the failing tests** (`cockpit_run.rs`, with `link_up`):
  - `spawning_runs_moa_panel_with_its_view_and_brain` → one `Tmux` effect with
    `new-window -d -P -F #{pane_id} -c /h -t =_moa: -n tree -e MOA_BRAIN=default -e MOA_ROOT=/r /x/moa panel tree --brain default`.
    The answer `%12` → `set-option -p -t %12 @moa_view tree`, the label, `remain-on-exit on`
    and the format. `panel_starts == 1`.
  - `outside_the_sandbox_no_moa_root_is_passed`.
  - `one_failed_spawn_still_tags_the_others`: three spawns, the second answering
    `Err("create window failed")` → the first and third panes are tagged, `panel_starts == 2`,
    and the operation re-plans once. If it still needs the second, the reply is
    `couldn't start the console panel; see panel.log`.
  - `a_spawn_whose_answer_never_comes_starts_nothing_else`: the link goes down, so the operation
    is dropped (L17), with one log line.
  - `status_reports_panel_starts`.
  - In `crates/moa-daemon/tests/tmux.rs`, `a_spawned_panel_runs_in_the_hidden_session` (real
    tmux, with a stand-in `exe` script that sleeps): the pane is in `_moa`, its window is named
    `tree`, its folder is `-c`'s, and it carries `@moa_view tree`. When the script exits, the
    pane stays and shows `panel stopped · Ctrl-a r brings it back`, on 3.4 and 3.8.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** The daemon fills `Deps.panel` from `std::env::current_exe()` and
  its `Paths` (`MOA_ROOT` only when sandboxed).
- [ ] **Step 4: Run them to see them pass**, plus the tmux 3.4 pass on `moa-daemon`.
- [ ] **Step 5: Commit:** `v1: moa starts its own panels, parked, with their views`.

## Task 7: layout switching, goto, Repair and the lazy cockpit

**Files:**
- Create: `crates/moa-core/src/keys.rs` (the one key dispatch, L13),
  `crates/moa-core/src/cockpit/run.rs` (the queue, L17)
- Modify:
  - `crates/moa-core/src/{core.rs, op.rs, ops/layout.rs, tmux/link.rs, tmux/mailbox.rs}`;
  - `crates/moa-proto/src/cli.rs`: `layout apply` and `Verb::Goto` become ops; both leave the
    not-yet list.
- Test: `crates/moa-core/tests/cockpit_run.rs`, `crates/moa-core/tests/tmux_keys.rs`,
  `crates/moa-proto/tests/cli.rs`.

**Interfaces:**
- Produces:
  - `Op::LayoutApply { name: Option<String> }` (`layout.apply`, a write) and
    `Op::Goto { panel: Leaf }` (`goto`);
  - `pub(crate) enum CockpitOp { Build { name: Option<String>, tell: Option<Tell>, select: Select }, Repair { tell: Option<Tell> }, Lazy, Refill, Show { pane: PaneId, tell: Option<Tell> }, SelectView { pane: PaneId, view: View, tell: Option<Tell> }, Vacate { pane: PaneId } }`,
    with `pub(crate) enum Select { No, Default, Leaf(Leaf) }`. A `Build` with `Select::Leaf`
    is `goto`'s: it replies with the leaf word, not `layout <name> applied`;
  - `pub(crate) fn queue(state: &mut State, op: CockpitOp) -> Vec<Effect>`, which starts the
    operation when nothing is in flight;
  - `Asked::Scene { seq: u64 }` and `Asked::Chain { seq: u64 }`. Each operation gets the next
    `seq` when it starts, and an answer whose `seq` isn't the one in flight is ignored (L17);
  - `keys::run(state, key: KeyAction, pane, client) -> Option<Vec<Effect>>`; `None` falls
    through to the link's not-yet notice;
  - `State.cockpit: CockpitRun { seq: u64, in_flight: Option<(u64, CockpitOp)>, waiting: VecDeque<CockpitOp> }`.
    The spot is read from each scene, never stored. `in_flight` is cleared on every outcome:
    an answer, an error, the link going down, or a reload. An in-flight `Vacate` cleared by a
    lost link or a reload goes back to the head of `waiting`.
  - **"A cockpit exists"** (L17, Task 10) means the topology has a window named `moa` in session
    `moa` (`Topology::cockpit()` is `Some`).
  - `SelectView` runs like every queued operation: a listing, then its chain (the `set-option`s
    for the tag, the label and the frame), then on `Ok` the registry, `pane_views`, the
    `panels` publish and the reply `{pane, view}`. A pane that isn't a panel in the listing →
    `%3 isn't a moa panel`.
  - The meta write `layout.current` happens in the commit after a build's chain succeeds.
  - The `ui` event `layout → <name>` (the spike's `log_ui`).

- [ ] **Step 1: Write the failing tests:**
  - `layout_apply_lists_then_builds_then_remembers`:
    - an `Asked::Scene` listing comes first;
    - its answer (panels parked) → one chained `Tmux`, with no spawn;
    - the chain's `Ok` → the meta `layout.current`, the `layouts` and `panels` topics
      published, the `ui` event, and the reply `layout focus applied`.
  - `two_layout_keys_run_one_after_the_other`: `layout-2` and `layout-4` before the first
    listing's answer.
    - One operation is in flight.
    - The second's listing is asked only after the first chain's answer.
    - The second chain is planned from that listing.
    - A third key, while one runs and one waits, replaces the waiting build, whose request is answered
      `replaced by a later layout request`. A waiting Repair is never replaced.
  - `a_reload_unwedges_the_cockpit_queue`: a `Build` in flight, a waiting `Vacate` and a
    waiting `Build`, then `Input::Reload`:
    - the waiting `Build`'s request gets `moa lost its terminal; try again`;
    - the `Vacate` runs;
    - a layout key after that runs;
    - the old operation's late answer is ignored.
  - `vacates_survive_a_link_drop`: a `Vacate` waiting, then the link drops and comes back to
    the same server → the vacate runs. When it comes back to another server, the vacate is
    dropped.
  - `a_failed_build_says_so_and_the_next_apply_mends_it`: the chain answers
    `Err("can't find pane: %4")` → the reply
    `layout focus couldn't be applied: can't find pane: %4; Ctrl-a r tries again`, with the
    meta unchanged and `in_flight` cleared. A second apply, from its own listing, succeeds.
  - `a_layout_too_big_for_the_window_is_said_plainly`: `Err("size or position no space for a new pane")`
    → `the focus layout doesn't fit this window; make the terminal bigger`.
  - `the_layout_keys_apply_the_presets`: `moa:key layout-2 %3 $1 /dev/ttys1` → the apply of
    `focus`, answered by a notice to `/dev/ttys1`.
  - `goto_selects_or_reapplies`:
    - `goto tree` with the tree in the cockpit → `select-window` and `select-pane` on its pane,
      with no build;
    - with the tree missing → the build, then the select of the tree's pane;
    - `goto status` in `cockpit` → `the cockpit layout has no status panel (moa layout list)`;
    - `goto agent` finds the empty slot.
  - `repair_respawns_dead_panels_then_applies_from_a_new_listing`: a dead tagged `%7` (`plan`)
    and a dead `%8` tagged `agent` →
    - `respawn-pane -k -t %7 -c /h -e … /x/moa panel plan --brain default`, and nothing for `%8`;
    - then a listing, in which `%7` reads alive, then the build, with no spawn;
    - then the notice `repaired: 1 panel restarted`.

    `panel_starts` goes up by exactly one.
  - `a_dead_panel_is_logged_once`: `moa:hook pane-died %7` twice for a tagged panel → one log
    line, `panel plan in %7 stopped`.
  - `the_first_attach_builds_the_cockpit`: `moa:hook client-attached /dev/ttys1` → a listing.
    Its answer (a user client, nothing tagged in the cockpit) → the spawns, then the build,
    with no `select-*`.
  - `two_attaches_build_the_cockpit_once`: two `client-attached` hooks → one lazy build in
    flight, and the second, from its own listing, finds the cockpit built and does nothing.
  - `a_control_client_attaching_builds_nothing`: the listing shows only a control-mode client
    → nothing.
  - `an_attach_before_moa_knows_session_moa_still_builds`: the hook arrives while the
    topology has no `moa` session, and the listing it asks for has one → the build.
  - `a_missing_cockpit_window_is_made_again`: the listing shows session `moa` with no window
    named `moa` → the chain starts `break-pane -d -s <first> -t =moa: -n moa`. With no session
    `moa`, `layout apply` replies `moa isn't open in a terminal yet; run moa first`.
  - `link_up_with_a_user_attached_builds_it`; `a_lost_link_drops_all_but_the_vacates` (L17's replies; an in-flight `Vacate` is
    queued again).
  - In `tmux_keys.rs`, `every_key_gets_its_notice` now covers only `Find`, `FindOther`, `New`
    and `Help`. `the_cockpit_keys_act` covers `c t p g 1 2 3 4 r`.
  - In `cli.rs`: `goto_parses` (`goto tree`; `goto nope` → a usage error naming the nine words)
    and `layout_apply_parses`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** A build sends its own `Effect::Tmux` (`Tx::tmux` isn't used: the
  build waits for its answer). The meta write is a commit of its own when the chain's answer
  is `Ok`.
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: layout switching, goto, Repair, and the cockpit on first attach`.

## Task 8: panels follow moa's view

**Files:**
- Modify:
  - `crates/moa-core/src/{panel.rs, core.rs, op.rs, publish.rs, tmux/link.rs (the listing hook)}`;
  - `crates/moa-view/src/panel/mod.rs` (`Panel::follow`);
  - `crates/moa-tui/src/run.rs` (keep the key from the reply; call `follow` on a `panels` change).
- Test: `crates/moa-core/tests/panels.rs`, `crates/moa-view/tests/panel.rs`,
  `crates/moa-tui/tests/run.rs`.

**Interfaces:**
- Produces:
  - `Op::UiSelectView { pane: String, view: View }` (`ui.select_view`, not a write). It queues
    `CockpitOp::SelectView` (L17, Task 7), so it can't race a build's retagging of the same
    pane;
  - the `ui.panel` reply `{key, view, managed}`; `PanelRow` gains `managed: bool`;
  - `pub(crate) fn resolve_panels(state: &mut State) -> Option<Effect>`, run after every listing;
  - `Panel::follow(&mut self, data: &Data, key: &str) -> bool` (true when the view changed);
    `Panel::managed(&self) -> bool`, read from the row.

- [ ] **Step 1: Write the failing tests:**
  - `the_tag_wins_over_the_view_a_panel_asks_for` (`panels.rs`): `%3` is tagged `status` in the
    topology, and conn 7 registers `%3` with `view: tree` → `{view: status, managed: true}`. An
    untagged `%4` registering with `view: tree` → `{tree, managed: false}`.
  - `a_listing_corrects_a_panel_that_registered_too_early`: `ui.panel {%3, tree}` before
    `link_up` → `managed: false`. The first listing shows `%3` tagged `status` → `panels`
    publishes `c7` as `{view: status, managed: true}`.
  - `select_view_retags_and_publishes`: `UiSelectView { pane: "%3", view: Plan }` → a listing.
    Its answer → the chain with `set-option -p -t %3 @moa_view plan`, the label and the frame
    options. The chain's `Ok` → `panels` publishing `c7` as `plan`, then the reply. On an
    untagged `%4` → `%4 isn't a moa panel`.
  - `chain_ok_records_its_tags_at_once`: right after a `Show` chain's `Ok`, before any refresh,
    `Topology::view_of(%9) == Some("agent")`, so a sleep then queues a vacate of the spot.
  - `a_new_tmux_server_clears_the_panes_of_registered_panels`.
  - In moa-view's `panel.rs`, `a_panel_follows_its_row`: a `panels` row for its key with `plan`
    and `managed: true` → `follow` returns true, `view() == Plan` and `managed()`. A row for
    another key changes nothing.
  - In `run.rs`, `a_reconnect_sends_the_view_it_last_followed`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: panels follow the view moa gives them`.

## Task 9: the agent spot

**Files:**
- Modify:
  - `crates/moa-core/src/ops/launch.rs` (`show`; `focus` and the open and resume answers use it);
  - `crates/moa-core/src/ops/term.rs` (the `$` fallback);
  - `crates/moa-view/src/views/{mod.rs, empty_slot.rs}` (the empty-slot view).
- Test: `crates/moa-core/tests/cockpit_run.rs`, `crates/moa-core/tests/panels.rs`, a moa-view
  snapshot, `crates/moa-tui/tests/render.rs`.

**Interfaces:**
- Produces:
  - `pub(crate) fn show(state: &mut State, pane: PaneId, tell: Option<Tell>) -> Vec<Effect>`,
    which queues `CockpitOp::Show` (L11, L17). For `focus`, its reply is
    `brought it into the cockpit` after a swap. Without a spot, it's 4b's `focus_cmds` and 4b's
    reply (`went to <title>`, `data {id, title}`), unchanged. An `open` or `resume` queues
    `Show` with `tell: None`, and its caller and waiters are answered as today
    (`Launching.reply` and `waiters`);
  - `views::empty_slot::build(width) -> Vec<Line>`: the spike's `EMPTY_SLOT` text, all dim and
    unframed, with no keys. The lines are `no agent here yet`, a blank line,
    `press Enter on a session in the tree` and `to bring it into this spot`.

- [ ] **Step 1: Write the failing tests:**
  - `focus_swaps_into_the_spot`: the `focus` layout is built, and Lantern is live in its tab
    `%9` → a listing, then one chain:
    1. `swap-pane -d -s %9 -t <slot>`;
    2. `break-pane -d -s <slot> -t =_moa:`;
    3. the tags;
    4. `select-window -t %9`, then `select-pane -t %9`.

    The reply is `brought it into the cockpit`, and `panel_starts` is unchanged.
  - `focus_with_no_spot_selects_its_tab`: in `cockpit` → 4b's commands and 4b's reply.
  - `focusing_the_spots_agent_only_selects_it`.
  - `a_resumed_session_lands_in_the_spot`: a resume's open answer, in `focus` → the swap, in
    place of `focus_cmds`.
  - `the_terminal_key_falls_back_to_the_spots_folder` (`panels.rs`), replacing 5a's "home until
    5b" test: a panel with no `ui.here`, and Lantern in the spot with cwd `/r/work/a` →
    `-c /r/work/a`. With the empty slot in the spot → home.
  - The empty-slot model snapshot at 80 columns, and its `TestBackend` text at 40 and 80.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Remove `the agent spot isn't in this build yet` (the `EmptySlot`
  arm of `not_yet`).
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: focus swaps a session into the agent spot`.

## Task 10: sleep, close and restart leave an empty spot

**Files:**
- Modify: `crates/moa-core/src/{sleep.rs, ops/sessions.rs, ops/launch.rs (restart), cockpit/run.rs, core.rs (the spot's agent exiting)}`
- Test: `crates/moa-core/tests/cockpit_run.rs`, 4b's sleep tests, `crates/moa/tests/sleep_e2e.rs`.

**Interfaces:**
- Produces:
  - `kill_tab` stays synchronous. While a cockpit exists, it queues `CockpitOp::Vacate { pane }`
    in place of the plain `kill-pane` (L17). From a fresh listing, the vacate does one of three
    things:
    - the pane is in the spot → L11's chain;
    - the pane is elsewhere → a plain `kill-pane`;
    - the pane is gone → nothing.

    The commit (asleep, closed, restarting) is unchanged and comes first, as today.
  - `why_not` drops `in the cockpit`: the spot can sleep, and `on screen` still holds.
  - A `pane-exited` of a pane the topology has tagged `@moa_view agent` queues
    `CockpitOp::Refill` (a build of the current layout, with no select). The tag is read before
    `pane_gone` deletes the binding.

- [ ] **Step 1: Write the failing tests:**
  - `sleeping_the_spots_agent_with_a_spare_slot_swaps_it_in`: Lantern in the spot, not
    visible, idle past the threshold, and a spare `empty-slot` parked → the commit with
    `asleep_at`, then a listing, then
    `swap-pane -d -s <slot> -t %9 ; kill-pane -t %9`. `panel_starts` is unchanged.
  - `sleeping_the_spots_agent_with_no_spare_respawns_the_spot`: the same with no spare →
    `respawn-pane -k -t %9 -c /h -e … /x/moa panel empty-slot --brain default ; set-option -p -u -t %9 @moa_session ; …tags ; set-option -p -t %9 remain-on-exit on ; …its format`.
    `panel_starts` goes up by one, and the session is asleep.
  - `closing_the_spots_agent_leaves_the_empty_slot` (`x`, `session.close`).
  - `restarting_the_spots_agent_leaves_the_empty_slot`.
  - `a_spot_whose_agent_ends_refills`: `moa:hook pane-exited %9`, the spot's bound agent →
    `Refill` → a listing, then the build.
  - `closing_an_agent_outside_the_spot_kills_it_after_a_listing`: with a cockpit, closing Kite
    in its own tab → the commit, a listing, then `kill-pane -t <Kite>`. With no cockpit, it's
    4b's synchronous `kill-pane`, so 4b's sleep and close tests (which have no cockpit) keep
    passing unchanged.
  - `sleep_right_after_a_show_vacates_the_spot`: a `Show` chain's `Ok`, then at once a sleep of
    that session → the vacate's spot chain, never a bare `kill-pane`.
  - `a_vacate_whose_pane_left_the_spot_just_kills_it`: a `Vacate` queued behind a build that
    sent the pane home → a plain `kill-pane -t %9`. With the pane gone → nothing.
  - `a_dead_empty_slot_in_the_spot_is_left_for_repair`: `pane-died` of the `empty-slot` in the
    spot → the log line, and no build.
  - `a_late_start_from_the_killed_tab_stays_history_across_a_reconnect_to_the_same_server`
    (5a carry): the kill; then the link drops and comes back to the same generation; then a
    `SessionStart` from the killed tab → the session stays asleep.
  - In `sleep_e2e.rs`, `the_spot_sleeps_and_shows_no_agent_here_yet` (real tmux, the `focus`
    layout, a fake agent in the spot, `sleep Q`). Within 10 s, the spot's pane shows
    `no agent here yet`, the agent's process is gone, and `ls` shows the session asleep.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to see them pass**, plus the tmux 3.4 pass on `moa` (the sleep e2e).
- [ ] **Step 5: Commit:** `v1: sleep, close and restart leave the agent spot whole`.

## Task 11: panels as parts of moa

**Files:**
- Modify:
  - `crates/moa-tui/src/run.rs` (signals, `FocusGained`, no flash, Ctrl-c);
  - `crates/moa-tui/Cargo.toml` (`tokio` `signal`);
  - `crates/moa-view/src/panel/mod.rs` (Ctrl-c: cancel first; the managed line);
  - `crates/moa/src/panel.rs` (logging to `panel.log`).
- Test: `crates/moa-view/tests/panel.rs`, `crates/moa-tui/tests/run.rs`,
  `crates/moa/tests/panel_e2e.rs`.

**Interfaces:**
- Produces:
  - `Panel::interrupt(&mut self) -> Option<Out>`: cancels a prompt or y/N (`cancelled`); else
    `Some(Out::Quit)` when unmanaged; else `None`, with the footer line
    `this panel is part of moa's layout; Ctrl-a r repairs the layout`;
  - the log lines `panel <view> in <pane|->: started`, `…: gave up reaching moa`,
    `…: stopped by <signal>` and `…: panicked: <message>`.

- [ ] **Step 1: Write the failing tests:**
  - in moa-view: `ctrl_c_cancels_a_prompt_first`, `ctrl_c_on_a_bare_managed_panel_stays_and_says_so`
    and `ctrl_c_quits_a_panel_you_started` (unmanaged);
  - in `run.rs`: `focus_gained_redraws` and
    `nothing_is_drawn_before_the_first_link_when_no_view_was_asked`;
  - in `panel_e2e.rs`, `sigterm_and_sighup_give_the_terminal_back`: a panel in a sandbox tab
    whose pane has `remain-on-exit on`, so it can be read after the exit.
    1. Send `kill -TERM <pid>`; a second run sends `-HUP`.
    2. The pane's `#{alternate_on}` and `#{mouse_any_flag}` are 0.
    3. `panel.log` has `stopped by SIGTERM`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Signals come through `tokio::signal::unix` in the loop's `select!`
  and end the loop as Ctrl-c does. The panic hook appends its line before `exit(101)`.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: panels cancel, log, and give the terminal back on every signal`.

## Task 12: the cockpit end to end

**Files:**
- Create: `crates/moa/tests/cockpit_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs`: `panes_of(tb, target) -> Vec<PaneRow>`, with the
  pane id, pid, tag, left, top, width, height and window name.

**Interfaces:**
- Consumes everything above. A helper `cockpit_up(tb) -> Terminal` attaches a terminal and waits
  for the cockpit to hold its three tagged panes.

- [ ] **Step 1: Write the tests** (real tmux, a real terminal, real `moa panel` processes,
  `fake-claude` agents):
  - `the_first_attach_builds_the_cockpit`:
    - the cockpit window holds tree, console and plan side by side;
    - the tree is about 26% of the width and the plan about 30% (±2 columns), and the console
      takes the rest, borders included;
    - each shows its frame title (`▤ sessions`, `◷ big picture`) by `screen_of`;
    - the login shell is in its own tab, `⌨ shell`, still running;
    - `panel_starts == 3`.
  - `a_layout_switch_moves_panes_and_starts_nothing`:
    1. Warm up: press `Ctrl-a 1 2 3 4` once each.
    2. Record `panes_of` and the `launches` and `panel_starts` counts.
    3. Press `Ctrl-a 2 4 1 3` back to back, then wait for `3`'s leaves to show.
    4. The counts are unchanged, and every panel pane keeps its pid.
    5. For each preset, the geometry is right: the order and split direction of the leaves,
       from the panes' left, top, width and height.
  - `focus_swaps_and_the_old_agent_goes_home`: in `focus`, open Lantern and Kite.
    1. Enter on Lantern in the tree → Lantern's pane is in the spot.
    2. Enter on Kite → Kite is in the spot, and Lantern's pane is alone in a tab named
       `Lantern`.
    3. Both agents keep their pids.
  - `repair_brings_a_killed_panel_back`:
    1. `kill -9` the plan panel's process.
    2. Its pane reads `panel stopped · Ctrl-a r brings it back`, and the daemon's log has
       `panel plan in %N stopped`.
    3. `Ctrl-a 2` still shows it, dead, in the plan's place.
    4. `Ctrl-a r` → the plan panel draws again in the same place. `panel_starts` went up by
       exactly one.
  - `frame_colours_follow_focus`: the tree's and the plan's panes have `window-style`
    `fg=#4c566a` and `window-active-style` `fg=#88c0d0`. After `select-pane` onto the tree, the
    tree's frame cell in the terminal is drawn in `#88c0d0` and the plan's in `#4c566a`
    (`cell_of`).
  - `moas_view_wins_after_moa_restarts`:
    1. `moa shutdown`.
    2. With moa down, set the tree panel's `@moa_view` to `status` with tmux, so the panel's
       memory (`tree`) and the tag differ.
    3. Run any `moactl`, which starts moa again.
    4. The panel reconnects and shows `◉ status`, with the same pid.
    5. Ctrl-c in it leaves it running and shows Call 4's line.
  - `goto_keys_select_their_panels`: `Ctrl-a p` makes the plan pane active; `Ctrl-a g` in `focus`
    makes the spot active.
- [ ] **Step 2: Run them.** A test that passes straight away is fine. Make each one fail once on
  purpose, by breaking the rule it pins, and note how in the report.
- [ ] **Step 3: Run each new test 10 times under load** (the test binary run directly, with no
  cargo running), and on tmux 3.4.
- [ ] **Step 4: Commit:** `v1: the cockpit end to end in the sandbox`.

## Carried, and where each goes

| Item (from) | Where |
|---|---|
| Focus swaps into the agent spot; sleep parks an empty slot (4b) | Tasks 9, 10 |
| `is_cockpit` costs a listing per rename (4b) | Task 4 (L16) |
| `a ; b` plus sentinel transactions (slice 3 T14) | Task 4 |
| `Sandbox::tmux`'s PATH lookup (slice 3) | Task 4 |
| `Topology::session_named` (slice 3) | Stays: the integration tests use it |
| `$` falling back to the spot's folder (5a) | Task 9 |
| `ui.select_view`; who owns a reconnecting panel's view (5a M6) | Task 8 |
| Panel supervision and logging; SIGTERM/SIGHUP; `FocusGained` (5a M7); the picture flash (5a) | Tasks 7, 11 |
| Ctrl-c in a cockpit panel (5a final review) | Task 11 |
| Terminal code out of `launch.rs` (5a M5) | Task 1 |
| Old pane ids in `state.panels` after a server change (5a) | Task 8 |
| The killed-tab test for a reconnect to the same server (5a) | Task 10 |
| The mouse line's alignment in the tmux file (5a) | Task 4 |
| Stray test servers and daemons (5a) | Task 2 |
| The `n` key, the first `command-prompt`, prompt quoting, a keymap table | **5c** |
| The spike's `@moa_panel` tags on an adopted server: v1 uses `@moa_view`, so they never mix; the switch-over retags or replaces the spike's panels | **Slice 10** |
| Setup snags from the 5a try-it: the sandbox starts with no `moa.toml`, moa's own tabs count as an unlisted `~/.claude`, and the sandbox's `~` misleads | **Slice 8** (setup); the 5b try-it gives the working `moa.toml` |
