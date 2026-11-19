# moa v1 slice 5b: layouts, the cockpit and the agent spot

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When you first attach, moa's first tab becomes the **cockpit**: the panels of the
current layout around an **agent spot**. `Ctrl-a 1–4` switch layouts by moving the panels that
already run, never starting new ones once each is there. `Ctrl-a c t p g` go to a panel.
Entering a session swaps it into the spot, and the session that was there goes back to its own
tab. `Ctrl-a r` repairs. Sleep and close leave the spot showing "no agent here yet" instead of a
hole. The panels behave as parts of moa: they survive moa restarts and follow the view moa gives
them. They come back on Repair and give the terminal back on every signal.

**Architecture:**
- **`moa_core::layout`** (pure) holds the spike's `layouts.json` format: parse with the spike's
  messages, validate, the presets, the merge.
- **`moa_core::cockpit`** (pure) is the engine. From a `Scene` (which panes exist, where they
  are and what they are, read from tmux's lists and the panel tags) and a layout, it plans:
  1. which panels to start (only when no running panel can be reused);
  2. one chained tmux command line that moves panes into place, parks the rest in the hidden
     `_moa` session, resizes, and tags.

  A small simulator checks the plan's invariants under proptest. The real tmux e2e checks the
  geometry.
- **The core** runs the steps: `layout.*`, `goto`, `ui.select_view`, the keys, Repair, the lazy
  cockpit on the first attach, and the spot-aware `focus`/`sleep`/`close`.
- **The panels** follow their row in the `panels` topic. moa's assignment of a view (the pane
  tag `@moa_panel`) wins over a panel's own memory.

```
 Ctrl-a 2 ─► moa:key layout-2 ─► core: Scene::read(topology, state) ─► cockpit::plan(scene, focus layout)
   ├─ spawn: [empty-slot]? ─► new-window -d -t =_moa: … moa panel empty-slot ─► -P reply ─► plan again
   └─ build: one chain "swap-pane … ; break-pane … ; join-pane … ; resize-pane … ; set -p …"
            ─► tags (@moa_panel, @moa_label, frame styles) ─► panels topic (views) ─► select
 focus Q ─► spot? swap-pane -d -s %agent -t %spot ─► old occupant: park (empty slot) | own tab by title
 sleep/close Q in the spot ─► swap an empty slot in first ─► kill-pane
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
  - `ops::launch::{focus, focus_cmds, term, term_key, Tell, told}`; `sleep::{why_not, kill_tab, put_to_sleep}`;
  - `tmux::{cmd::Cmd, link::{TmuxState, Asked, notice, set_pane_option, unset_pane_option}, topology::Topology, mailbox::{KeyAction, HookMsg, Mailbox}}`;
  - `publish::{Topic, PanelRow}`; `Effect::Tmux`; `Tx::tmux`.
- The client: `moa_client::Live`; `moa_tui::run::{Loop, first_ops, claim_pane}`; `moa_view::panel::Panel::{set_view, view}`.
- `moa_tmux::{client::ControlClient, conf::moa_tmux}`.
- `moa_proto::{cli::{Verb, LayoutOp}, wire::StatusOk}`.
- The testkit (`TempBrain`, `Terminal`, `wait_for`, `fake_claude`).
- `crates/moa/tests/common`: `open`, `came_up`, `tmux`, `status_json`, `panel_tab`, `screen_of`, `click`, `moactl`.
- `crates/moa-core/tests/common`: `loaded`, `link_up`, `send`, `send_on`, `feed`, `changes`, `replies`.

## Calls for you to check

The most important come first. Each is a rule below (L-number) with its reason.

1. **moa's first tab becomes the cockpit the first time you attach** (L9). Today that tab is a
   plain shell. When the cockpit is built there, that shell moves to its own tab, `⌨ shell`,
   still running. moa never closes a program you started.
2. **The console's place says "the console isn't in this build yet"** until slice 6 builds the
   console. The default layout, `cockpit`, has the console in the middle (spike parity), so the
   middle of the cockpit is that one line until then (L9).
3. **Panels are reused, not multiplied** (L5). When a layout needs a view no running panel
   shows, moa changes the view of a panel the new layout doesn't need: in `triage`, the tree
   panel becomes the status panel. A new panel starts only when there's none to spare. Across
   the four presets that's at most four panels, plus the empty spot. Each keeps running for
   moa's whole life.
4. **Ctrl-c in a cockpit panel cancels a prompt, but never closes the panel** (L14). With
   nothing to cancel it says `this panel is part of moa's layout; Ctrl-a r redraws the layout`.
   A panel you start yourself with `moa panel` still quits on Ctrl-c.
5. **A panel that stops stays visible as a pane** reading `panel stopped · Ctrl-a r brings it
   back`. It isn't restarted by itself: a panel that crashes on start would otherwise restart
   in a loop. It's logged. Repair restarts it in place (L12).
6. **One layouts file for every brain, with the current layout kept per brain** (L2). `layout
   set` writes `~/.config/moa/layouts.json` (the spike's file and format). Edits you make by
   hand apply at the next layout command; no restart is needed. The spike kept `current` in
   the file, which every brain would share; v1 keeps it in each brain and leaves the file's
   `current` alone.
7. **Switching layouts rebuilds the cockpit in place.** Panes you resized by hand go back to the
   layout's sizes, as in the spike. The window stays the same tab with the same number, so your
   terminal doesn't jump to another tab during a switch (L6).
8. **An agent that ends while it's in the spot is replaced by the empty spot** right away
   ("no agent here yet"), so the cockpit never has a hole (L11).
9. **moa's assignment of a view wins over a panel's memory** (L8, 5a's M6). If moa gives a
   panel another view while the panel is disconnected or moa restarts, the panel shows moa's
   choice when it reconnects.
10. **A panel or `moactl` never starts moa in a folder that's gone** (L15). A test that removed
    its folder could leave a moa daemon and a tmux server running behind it; those are the
    stray `/tmp/moaXXXX` servers. Now it says `moa's folder <root> is gone`.

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

**Done already** (committed with this plan): the spec edits that record Calls 4–6 and 9 (§6.13).
§6.4 already lists the `layouts` topic and the `layout.*`, `goto` and `ui.select_view` methods.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| L1 | **`moa_core::layout`, the spike's format and words exactly:**<br>• `Node` is `{"split":"h"\|"v","children":[≥2 nodes],"size"?}` or `{"panel":LEAF,"size"?}`;<br>• `size` is a positive integer (cells) or `"N%"` with N from 1 to 99;<br>• leaves are `console tree plan status today activity projects picture agent`, at most one `console` and one `agent` per layout;<br>• the presets are the spike's `DEFAULTS` (cockpit, focus, classic, triage), copied exactly;<br>• the file's order is kept; presets missing from the file come first (the spike's merge), so a deleted preset comes back;<br>• the file's entries stay raw JSON and are parsed when used: a broken entry is listed with its error and refused on `apply`, while the others still work.<br>The spike's error texts are used with their paths (`layout.children[1]: unknown panel 'tre' (panels: console, tree, …)`). | Spec §6.13 ("keep the spike's format and file", "validated: known names, ≤ 1 console, ≤ 1 agent", presets "merged in, deleted presets come back"). The spike accepted `"0%"`, which tmux refuses; 1–99 says so up front. Parsing per entry keeps one bad hand edit from taking away every layout. |
| L2 | **Where layouts live:**<br>• `layouts.json` in `Paths::config_dir()` (`~/.config/moa/`, the sandbox's own in the sandbox);<br>• the core reads it through `Deps.read_text` when it starts and again at every `layout.*` op, `goto` or layout key: a user action, never a poll;<br>• `layout.set` writes it through a new `Effect::WriteFile { path, text }` that the daemon runs before the effects after it (temporary file plus rename; on failure, a log line and an error event `couldn't save layouts.json: <error>`). The write keeps the file's other entries and its `current` key as they were, pretty-printed with 2 spaces like the spike's `json.dump(indent=2)`;<br>• the current layout per brain is the `meta` value `layout.current`, defaulting to `cockpit`; a name no longer in the file falls back to `cockpit`. | Spec: `layouts.json` is shared config (§6.5 "Shared config"). Hot reload is slice 9; reading on each layout action gets hand edits in with no watcher. Brains share the file, so a per-file `current` would let one brain switch another's layout. |
| L3 | **Ops and verbs** (spec §6.4):<br>• `layout.list` → lines `▸ cockpit: tree · console · plan` (`▸` for current, two spaces otherwise), then `(config: <path, ~ for home>)`;<br>• `layout.show {name}` → the entry's JSON, pretty;<br>• `layout.set {name, layout}` → `saved layout <name>; apply it with: moa layout apply <name>`;<br>• `layout.apply {name?}` → `layout <name> applied`; with no name, the current one;<br>• `goto {panel}` → the leaf word;<br>• `ui.select_view {pane, view}` → `{pane, view}`.<br>`moactl layout list\|show NAME\|set NAME JSON…\|apply [NAME]` and `moactl goto PANEL` stop saying "not yet"; `set` joins the remaining words with spaces, as the spike did. Writes: `layout.set`, `layout.apply` (the meta value). New topic `layouts`: rows `{key: name, name, leaves: [words], current: bool, error: string?}`. | Parity (R1 §2.2) with "moa" for "moactl" in the hint, since `moa` is the command users type. The topic lets the console (slice 6) and help (5c) show layouts without asking. |
| L4 | **tmux commands that move panes** join `Cmd`'s allow-list, each with a checked shape:<br>• `swap-pane -d -s %A -t %B`;<br>• `join-pane -d -h\|-v -s %A -t %B`;<br>• `break-pane -d -s %A -t =moa:\|=_moa: [-n NAME]`;<br>• `move-window -s %A -t =moa:\|=_moa:`;<br>• `resize-pane -t %A -x\|-y N\|N%`;<br>• `respawn-pane -k -t %A …`;<br>• `rename-window -t %A NAME`.<br>Pane ids only, never names, so a prefix can't match a stranger (research 2 §9).<br>**`Cmd::chain(Vec<Cmd>) -> Cmd`** sends `a ; b ; …` as one line, checked part by part, with the existing sentinel after it. Its result is `Err` with the first error, and tmux skips the rest of the line after a failure. | Spec §6.6: "`a ; b` plus a sentinel for transactions"; "mutations batched into one write". Slice 3's T14 left this to its first consumer. A failed `join-pane` must not be followed by moves that assume it worked. |
| L5 | **Which pane plays each leaf** (`cockpit::assign`), in order:<br>1. **`agent`:** the spot's occupant (an agent, or the empty slot); else a spare `empty-slot` panel; else a spare panel of another view, reassigned to `empty-slot`; else a new one.<br>2. **Each panel leaf, by layout order:** a panel already showing that view (one in the cockpit first, then one parked).<br>3. **The leaves still without a pane:** a spare panel from the cockpit or parked, cockpit first, reassigned. A spare panel is one this layout doesn't use: in the cockpit or parked, any view.<br>4. **The rest:** new panels, started parked, before the build.<br>**Panels** are panes tagged `@moa_panel=<view>` whose pane isn't dead. A dead one is left for Repair. | Spec §6.13: "views it needs that aren't on screen come from parked panel panes … only the very first use of a view (or Repair after a crash) spawns a panel". Reassigning is a published row change, not a launch. Preferring an exact match keeps each panel's scroll and selection. |
| L6 | **The build, in place** (`cockpit::build`), as one chain:<br>1. **The keeper.** K = the first leaf's pane. If K isn't in the cockpit window, `swap-pane -d -s K -t C`, where C is a cockpit pane (an untagged one first, so a stranger leaves the cockpit straight away).<br>2. **Clear out.** Every other cockpit pane goes home: a panel → `break-pane -d -s P -t =_moa:`; a bound agent → `break-pane -d -s P -t =moa: -n <title>`; anything else → `break-pane -d -s P -t =moa: -n '⌨ shell'` plus `@moa_term 1` and `@moa_label '⌨ shell'`.<br>3. **The swapped-out pane goes home too.** C now sits where K was. If C landed in the wrong session, it moves: `move-window -s C -t =moa:\|=_moa:` when it's alone in its window, `break-pane` otherwise.<br>4. **Join breadth first.** For each split, join each later child's first pane after the previous child's first pane (`join-pane -d -h\|-v -s X -t Y`), then build each child inside its own cell.<br>5. **Resize** each sized child's first pane (`resize-pane -x\|-y`), parents before children, the spike's order.<br>6. **Tag** (L10), then `select-window`/`select-pane` the console's pane, or else the first leaf's, but only when the user asked (a key, `layout.apply`, `goto`), never for a lazy build or a refill.<br>The cockpit window keeps its id, index and name. No pane is killed. | Spec: "`layout apply` **moves** existing panes (swap/join/break) and resizes, never restarts them"; "leftover agents go back to their own windows, named by title". The spike built a new window from `sleep` placeholders. Those are launches, and the user's terminal jumped while the old window closed. Joining real panes needs no placeholders. Splitting only inside each child's cell is what makes nesting come out right (the spike split cells along the axis first for the same reason). |
| L7 | **Spawning a panel:** `new-window -d -P -F '#{pane_id}' -t =_moa: -n <view> -e MOA_BRAIN=<brain> [-e MOA_ROOT=<root>] <moa exe> panel <view> --brain <brain>`. The answer then sets `@moa_panel <view>`, `remain-on-exit on` and `remain-on-exit-format 'panel stopped · Ctrl-a r brings it back'`.<br>`Deps.panel: PanelLaunch { exe, env }` comes from the daemon: its own `current_exe`, and `MOA_ROOT` only when sandboxed.<br>`State.panel_starts: u32` counts each new or respawned panel. `status` reports it as `panel_starts`. | Lesson 5 (env explicitly, per pane). A panel started with its view never flashes the picture first (5a carry). Counting panel starts makes "a layout switch starts nothing" checkable: the daemon's `Spawner` count doesn't see processes tmux starts. |
| L8 | **Who owns a panel's view** (5a's M6): `ui.panel` resolves the view as the pane's `@moa_panel` tag (moa's assignment, read from tmux, which survives a moa restart), else the view asked for, else `pane_views`, else `picture`. The reply gains `managed: bool` (the pane has the tag). `ui.select_view {pane, view}` sets the tag, the registry and `pane_views`, re-tags the frame (L10), and publishes `panels`. A panel follows its own row (`key` from its `ui.panel` reply): when the row's view changes, it calls `Panel::set_view`. When the tmux server changes, registry entries lose their pane (`pane: None`), as `pane_views` is already cleared. | A reassignment made while a panel was away must not be undone by the panel's stale memory. The tag lives with the pane in tmux, so it is the one record both a reconnect and a restarted daemon can read. A row change can't be missed the way a one-off message can (5a's V3). |
| L9 | **The lazy cockpit [C28]:** on `client-attached`, and on a link-up while a user is attached, the core builds the current layout if the cockpit window holds no tagged panel. A flag stops a second attach from building twice. A stranger pane there (today's login shell) goes home as `⌨ shell` (L6 step 2). Nothing is selected. Until slice 6 the `console` leaf is an ordinary panel with the console view, which reads `the console isn't in this build yet`. | Spec: "the daemon builds the cockpit when a user first attaches, not at login". Never kill what the user started. |
| L10 | **Tags and frames** (the spike's `tag`, spec "Frames"):<br>• **A panel pane:** `@moa_panel <view>`, `@moa_label <View::label>`. If `View::framed`, also `@moa_framed 1`, `window-style fg=#4c566a` and `window-active-style fg=#88c0d0`, all pane-level; otherwise these three are unset.<br>• **An agent in the spot:** `@moa_panel agent`, `@moa_label '● <title>'`, framed options unset.<br>• **An agent leaving the spot:** `@moa_panel` and `@moa_label` unset, and its window renamed to its title (cut to 24 characters, the spike's cut).<br>Every tag is a `set-option -p` inside the build's chain. | Lessons 11–12: the frame colour follows focus through tmux's own pane styles, with no events. |
| L11 | **The agent spot:**<br>• **`focus Q`** of a live session whose pane isn't the spot, when the layout has a spot: `swap-pane -d -s P -t S`, tag P, and `select-window`/`select-pane` P.<br>• **The old occupant:** an empty slot is parked (`move-window -s S -t =_moa:` when it's alone in P's old window, else `break-pane`); an agent keeps P's old window, renamed to its title.<br>• **With no spot**, or when P is already the spot: 4b's `focus_cmds`.<br>• **Everything that shows a session** goes through this (`focus`, an `open` or `resume` once its tab is up, the tree's Enter): `show(state, pane)`.<br>• **Taking a session out of the spot** (`kill_tab` for sleep, close or restart): first an empty slot is swapped in, from a spare or a new one (then the kill waits for the start's answer).<br>• **When the spot's pane exits** by itself, the core re-applies the current layout, which puts an empty slot there.<br>• **`$` with no `ui.here`** opens in the spot agent's folder, else home. | Spec: "`focus Q` resumes if needed and swaps the session into the agent spot (the previous occupant returns to its window; an empty slot is parked)"; "Sleeping: in the agent spot → park an empty slot there first". The spike's `here_dir` fell back to the spot's agent. Re-applying keeps the layout whole. |
| L12 | **Supervision and Repair:**<br>• A `pane-died` for a tagged pane logs `panel <view> in %N stopped` (info, once per death); the pane stays, showing L7's format.<br>• **Repair** (`Ctrl-a r`, prefix `C`, the menu) respawns every dead tagged pane in place (`respawn-pane -k -t %N <the panel command>`, counted in `panel_starts`), then re-applies the current layout and says `repaired: <n> panels restarted` (or `layout <name> applied` when none was dead).<br>• **`goto PANEL`** selects that panel in the cockpit. If the panel is missing and the current layout has it, it re-applies first. If the layout lacks it: `the <layout> layout has no <panel> panel (moa layout list)`. `agent` also finds the empty slot. | Spec §6.13 ("a dead panel is logged and respawned on Repair", "`goto PANEL` selects a panel, re-applying the layout if it's missing"); spike `goto`. |
| L13 | **Keys:** `c t p g` → `goto console\|tree\|plan\|agent`; `1 2 3 4` → `layout.apply cockpit\|focus\|classic\|triage`; `r` and prefix `C` → Repair. Each answers by a notice to the pressing client (`Tell::Notice`). `Find`, `FindOther`, `New` and `Help` keep their not-yet notices (5c). The core takes every key in one dispatch (`keys::run`) before the link's not-yet fallback. | Spec §6.13 key table. 5a's `core.rs` comment: "as more keys land, they belong in one dispatch here". |
| L14 | **The panel process** (5a carries):<br>• **Ctrl-c:** cancels an open prompt or y/N everywhere. With nothing open it quits only an unmanaged panel; a managed one (L8's `managed`) shows L14's line from Call 4.<br>• **SIGTERM, SIGHUP and SIGINT** restore the terminal and exit 0.<br>• **`FocusGained`** redraws and re-arms the deadline (M7).<br>• **Logging:** `panel.log` in the brain's log folder gets one line per start, give-up, signal and panic, each with the pane and view.<br>• **No view asked and not yet linked:** nothing is drawn (no picture flash). | 5a's final review, recommendations 3 and 5, and M7. tmux sends SIGHUP to a pane it kills. A managed panel quitting leaves a dead pane in the layout. |
| L15 | **No stray servers:** `moa_client`'s autostart, and so the panel's first connect and every `moactl`, refuses when `Paths`'s root folder doesn't exist: `moa's folder <root> is gone`. Task 2 hunts the other sources of the stray `/tmp/moaXXXX` servers. Each source it finds gets a fix and a test that would have caught it. | 5a's Task 14 carry: a test failing between `panel_tab` and the panel's first connect let that connect start a daemon under a deleted root. A user's real root always exists. |
| L16 | **Topology learns the tags:** `LIST_PANES` gains `#{pane_dead}`, `#{window_panes}` and `#{@moa_panel}`. Renames are applied from the notification (`%window-renamed @w name`, `%unlinked-window-renamed`, `%session-renamed`) without a full listing, which removes 4b's "listing per tab rename" cost. `Topology::session_named` becomes `#[cfg(test)]`, and `Sandbox::tmux` looks tmux up once. | The scene needs tags, liveness and whether a pane is alone in its window. 4b and slice 3 carried the rename cost and the two cleanups to this slice. |

## Parity rows this part delivers (spec §7)

"Layout tree, validation, presets, merge"; "`layout apply/list/show/set`"; "`goto`, `focus`,
agent spot swap"; "Panel tagging, labels, frame colours"; "Repair, cockpit split refused" (the
refusal is already in `moa.tmux`). The picker and help are 5c's.

## Global Constraints

The roadmap's, every earlier slice plan's, and 5a's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **Never run cargo while an e2e test runs.** Relinking `target/debug/moa` breaks the running
  tests. Run a test binary directly, or use another `CARGO_TARGET_DIR`.
- **Zero launches for a layout switch, a `goto`, a `focus` and a Repair with nothing dead,**
  once the panels exist:
  - neither the daemon's `Spawner` count nor `panel_starts` changes;
  - every panel's `pane_pid` is the same before and after.
- **No pane is ever killed by a layout change or a focus**, except an agent that sleep, close or
  restart is taking down, and only after an empty slot replaced it in the spot.
- **tmux targets are pane ids** (`%N`) or the exact sessions `=moa:` and `=_moa:`, never a bare
  name.
- **The core stays pure:** file reads only through `Deps` functions, writes only through
  `Effect::WriteFile`.
- **Tests:**
  - only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too;
  - never a tmux server named `moa` or `moa-work`, never a real brain;
  - `fake-claude` is the only agent;
  - every wait goes through `wait_for` with a 10 s limit, never a sleep.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`, `~/private`.
- **Plain words** in every message: "tab", "folder", "panel", "layout", never "pane" or
  "window" (lesson 35).

## Review Focus

1. **A layout switch or focus with agents and strangers in odd places.** The cases: an agent in
   the spot; a user's split shell inside an agent's tab; a stranger pane in the cockpit; a dead
   panel. Expected: nothing is killed, every agent ends in its own tab named by its title,
   strangers become `⌨ shell` tabs, and the cockpit holds exactly the layout's leaves. Pinned by
   `no_switch_kills_or_strands_a_pane` (Task 5, proptest over the simulator) and
   `a_layout_switch_moves_panes_and_starts_nothing` (Task 12).
2. **tmux refuses one command mid-switch** (a pane vanished between the listing and the chain).
   Expected: the chain stops there, the reply says
   `layout <name> couldn't be applied: <tmux's words>; Ctrl-a r tries again`, and the next apply
   repairs it. Pinned by `a_chain_stops_at_its_first_error` (Task 4) and
   `a_failed_build_says_so_and_the_next_apply_mends_it` (Task 7).
3. **moa restarts, or a panel reconnects, after its view changed.** Expected: the panel shows
   moa's assignment, not its old view. Pinned by `the_tag_wins_over_the_view_a_panel_asks_for`
   (Task 8) and `a_reassigned_panel_keeps_moas_view_after_moa_restarts` (Task 12).
4. **Two users attach at once, or attach while moa is still linking.** Expected: one cockpit,
   built once, with no duplicate panels. Pinned by `two_attaches_build_the_cockpit_once`
   (Task 7).
5. **tmux 3.4.** The cases: `remain-on-exit-format`, `resize-pane -x N%`, `join-pane` across
   sessions. Expected: the same behaviour as 3.8. Pinned by the tmux 3.4 pass over Task 4's
   real-tmux tests and Task 12's e2e. If `remain-on-exit-format` is missing on 3.4, the
   option is skipped there (detected from the server version the link already reads), and a
   dead panel shows tmux's own "Pane is dead" line.

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

- [ ] **Step 1: Move the items, unchanged.** No other change in this commit: `git diff -M --stat`
  shows the move, and the bodies are identical.
- [ ] **Step 2: Run** `nix develop --command cargo nextest run --workspace` → all pass, the same
  count as before.
- [ ] **Step 3: Commit:** `v1: the terminal code moves to ops/term.rs`.

## Task 2: no stray test servers

**Files:**
- Modify: `crates/moa-client/src/lib.rs` (the autostart check), and the testkit and test helpers
  where the hunt points.
- Test: `crates/moa/tests/client.rs`, plus the guard named in Step 3.

**Interfaces:**
- Produces: the autostart refusal `moa's folder <root> is gone` (a `ClientError` variant that
  shows that text).

- [ ] **Step 1: Write the failing test**
  `autostart_refuses_a_root_that_is_gone` (`client.rs`):
  1. Build `Paths` under a `/tmp` folder, then remove the folder.
  2. `Client::connect` with `autostart: Some(exe)` → `Err`, whose text is
     `moa's folder <root> is gone`.
  3. `spawn::launches()` hasn't changed, and the folder still doesn't exist.
- [ ] **Step 2: Run it to see it fail:**
  `nix develop --command cargo nextest run -p moa --test client autostart_refuses` → it fails:
  a daemon starts and recreates the folder.
- [ ] **Step 3: Hunt the other sources.**
  - Record the tmux sockets and `moa daemon` processes under `/tmp/moa*` before and after a
    full `cargo nextest run --workspace`, and attribute each new leftover to a test (its root's
    name, its log). Record the method and the findings in the report.
  - Fix each source, at the root cause (a helper that starts a server outside `TempBrain`, a
    `Drop` that runs before a child's first connect, …).
  - Add a guard test that fails if the source comes back. If the hunt finds no other source,
    the guard is `a_full_e2e_file_leaves_no_server_behind`: it runs `panel_e2e`'s helpers on a
    `TempBrain`, drops it, and finds no socket and no daemon under that root.
  - **Never touch servers outside the test's own roots.** The 8 stray servers already on this
    machine are not this task's to clean up.
- [ ] **Step 4: Implement the check** in the autostart path: the root folder
  (`Paths::root()`, or whatever names `MOA_ROOT`/the home) must exist before anything is
  spawned or created.
- [ ] **Step 5: Run the tests to see them pass**, then the whole workspace twice, recording the
  leftovers after each run: none.
- [ ] **Step 6: Commit:** `v1: moa never starts in a folder that's gone; no stray test servers`.

## Task 3: layouts

**Files:**
- Create: `crates/moa-core/src/layout.rs`, `crates/moa-core/src/ops/layout.rs`
- Modify:
  - `crates/moa-core/src/{lib.rs, op.rs, state.rs, effect.rs, publish.rs, change.rs}`;
  - `crates/moa-core/Cargo.toml` (`indexmap` with `serde`);
  - `crates/moa-daemon/src/{run.rs, core_task/*}` (`Deps.read_text`, running `Effect::WriteFile`);
  - `crates/moa-proto/src/cli.rs` (`Verb::Layout` and `Verb::Goto` become ops; `goto` lands in
    Task 7, so here it's `Layout` only, and `Goto` stays in the not-yet list until then);
  - `crates/moa-proto/tests/snapshots/` (the schema).
- Test: `crates/moa-core/tests/layout.rs` (new), `crates/moa-core/tests/skeleton.rs`,
  `crates/moa-proto/tests/cli.rs`, `crates/moa-daemon/tests/…` (the write).

**Interfaces:**
- Produces, in `moa_core::layout`:
  - `pub enum Axis { H, V }`; `pub enum Size { Cells(u16), Percent(u8) }` with
    `fn arg(&self) -> String` (`"26%"` or `"40"`).
  - `pub enum Leaf { Panel(View), Agent }`, parsing and printing the nine words.
  - `pub enum Node { Split { axis: Axis, children: Vec<Node>, size: Option<Size> }, Leaf { leaf: Leaf, size: Option<Size> } }`.
  - `pub fn parse(v: &serde_json::Value) -> Result<Node, LayoutError>`; `LayoutError` displays
    the spike's texts with paths (`layout`, `layout.children[2]`).
  - `pub fn leaves(n: &Node) -> Vec<Leaf>`.
  - `pub struct Layouts { pub current_in_file: Option<String>, pub entries: IndexMap<String, serde_json::Value> }`,
    with:
    - `pub fn load(text: Option<&str>) -> Result<Layouts, LayoutError>` (with the merge);
    - `fn node(&self, name: &str) -> Result<Node, LayoutError>`;
    - `fn names(&self) -> impl Iterator<Item = &str>`;
    - `fn file_text_with(&self, name: &str, layout: &serde_json::Value) -> String`, which writes
      only the file's own entries plus this one, never the merged presets.
  - `pub fn presets() -> Layouts`; `pub const PRESET_KEYS: [&str; 4] = ["cockpit", "focus", "classic", "triage"]`.
- Produces in the core:
  - `Op::LayoutList`, `Op::LayoutShow { name: String }`,
    `Op::LayoutSet { name: String, layout: serde_json::Value }` (methods `layout.list`,
    `layout.show`, `layout.set`), in `METHODS`; `LayoutSet` is in `WRITES`;
  - `Effect::WriteFile { path: PathBuf, text: String }`;
  - `Deps.read_text: fn(&Path) -> io::Result<Option<String>>`, `Deps.layouts_file: PathBuf`;
  - `State.layouts: Layouts`;
  - `fn current_layout(&State) -> String` (meta `layout.current`, else `cockpit`; a missing name
    falls back to `cockpit`);
  - `Topic::Layouts` with `LayoutRow { name, leaves: Vec<String>, current: bool, error: Option<String> }`.
- `layout.apply` and the meta write come in Task 7.

- [ ] **Step 1: Write the failing tests** in `layout.rs`:
  - `presets_are_the_spikes`: `presets()` parses to exactly the spike's four trees (sizes
    `26%`, `30%`, `24%`, `45%`, `42%`, `40%`, `28%`), in the order cockpit, focus, classic,
    triage.
  - `a_file_keeps_its_order_and_gets_missing_presets_first`: the file `{"layouts":{"mine":…,"cockpit":…}}`
    → names `focus, classic, triage, mine, cockpit`.
  - `validation_says_what_the_spike_said`, one case each:
    - `{"panel":"tre"}` → `layout: unknown panel 'tre' (panels: console, tree, plan, status, today, activity, projects, picture, agent)`;
    - a split with one child → `layout: a split needs at least 2 children`;
    - `{"split":"x",…}` → `layout: needs "panel", or "split": "h" (side by side) / "v" (stacked)`;
    - two consoles → `layout: at most one console and one agent panel`;
    - `"size":"0%"` and `"size":0` → `layout.children[0]: size must be a number of cells or like "30%"`.
  - `a_broken_entry_doesnt_take_the_others_down`: one bad entry; `names()` lists it, and
    `node("bad")` is an `Err` while `node("cockpit")` is `Ok`.
  - `layout_list_marks_the_current_one`: after meta `layout.current = focus`, the reply text
    starts `  cockpit: tree · console · plan\n▸ focus: tree · plan · agent · console`, then the
    two other presets in the same form, and ends `(config: ~/.config/moa/layouts.json)`.
  - `layout_set_validates_writes_and_publishes`: `LayoutSet { name: "mine", layout: <valid> }` →
    effects, in order:
    1. `WriteFile` with the file text (2-space JSON holding only `mine`, plus the file's earlier
       entries and its `current` if it had them);
    2. a `Publish` of `layouts`;
    3. the `Reply` `saved layout mine; apply it with: moa layout apply mine`.

    An invalid layout is refused with the validation text, and nothing is written.
  - `layout_ops_read_the_file_again`: `Deps.read_text` returns a new text between two
    `LayoutList`s → the second lists the new entry.
  - `cli.rs`: `layout_verbs_parse`:
    - `layout list`, `layout show cockpit`, and `layout set mine {"panel": "tree"}` (the
      words after the name joined with spaces) all parse;
    - `layout show` with no name → `usage: moa layout show NAME`.
- [ ] **Step 2: Run them to see them fail:** `nix develop --command cargo nextest run -p moa-core --test layout` → compile errors.
- [ ] **Step 3: Implement** the interfaces. The daemon:
  - reads `layouts.json` at start and passes the text to the core;
  - runs `WriteFile` as a temporary file in the same folder plus a rename, logging on failure
    and sending L2's error event;
  - adds a test, in the daemon's tests, that a `WriteFile` replaces the file atomically.

  Update the schema snapshot (`cargo insta review`); only the new methods, topic and effect
  should change.
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: layouts: the spike's file, presets, list, show and set`.

## Task 4: tmux commands that move panes

**Files:**
- Modify:
  - `crates/moa-core/src/tmux/cmd.rs` (allow-list, shapes, `chain`);
  - `crates/moa-tmux/src/client.rs` (a chain gets the sentinel; its results fold into one);
  - `crates/moa-core/src/tmux/topology.rs` (L16);
  - `crates/moa-testkit/src/sandbox.rs` (tmux looked up once);
  - `crates/moa-tmux/src/conf.rs` (5a's mouse-line alignment: one space, `conf.rs:98`).
- Test: `crates/moa-core/tests/tmux_vocab.rs`, `crates/moa-tmux/tests/client.rs`,
  `crates/moa-core/tests/tmux_focus.rs`.

**Interfaces:**
- Produces:
  - `Cmd::check` accepts L4's seven shapes and refuses every other form of them, with the
    existing `Refused` texts' style;
  - `pub fn chain(cmds: Vec<Cmd>) -> Cmd`: one line, ` ; `-joined; `nests()` is true; each part
    is checked; an empty chain is refused;
  - Topology:
    - `pub fn panel_of(&self, p: PaneId) -> Option<&str>` (the raw tag);
    - `pub fn is_dead(&self, p: PaneId) -> bool`;
    - `pub fn alone(&self, p: PaneId) -> bool`;
    - `pub fn session_of(&self, p: PaneId) -> Option<&str>` (the session's name);
    - `pub fn cockpit(&self) -> Option<WindowId>`;
    - `pub fn panes_in(&self, w: WindowId) -> impl Iterator<Item = PaneId>`.

- [ ] **Step 1: Write the failing tests:**
  - `the_pane_moving_commands_take_only_their_safe_shapes` (`tmux_vocab.rs`):
    - each L4 shape passes;
    - each of these is refused: `swap-pane` without `-d`, a `join-pane` target by name
      (`-t moa`), `break-pane -t moa:` (not `=moa:`), `move-window -t _moa2:`, `resize-pane`
      without `-x`/`-y`, `respawn-pane` without `-k`.
  - `a_chain_is_one_line_and_checks_every_part`: `chain([select-pane -t %1, swap-pane -d -s %1 -t %2]).line()`
    is `select-pane -t %1 ; swap-pane -d -s %1 -t %2`; a chain holding a refused part is refused.
  - `a_chain_stops_at_its_first_error` (`client.rs`, real tmux): send the chain of three:
    1. `set-option -p -t %0 @a 1`
    2. `swap-pane -d -s %0 -t %999`
    3. `set-option -p -t %0 @b 1`

    The result is one `Err` naming the missing pane, `@a` is set and `@b` isn't, and the next
    command's reply is its own (the sentinel kept the replies in step).
  - `renames_apply_without_a_listing` (`tmux_focus.rs`): `%window-renamed @2 moa` on a window in
    session `moa` → `Applied::Changed`, and `is_cockpit(@2)`; renaming it away clears it; neither
    returns `Applied::Refresh`.
  - `the_listing_reads_tags_liveness_and_company`: a `LIST_PANES` line with `@moa_panel tree`,
    dead `1` and window panes `3` → `panel_of`, `is_dead` and `alone` answer from it.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** `%window-renamed` carries the new name. A window's session comes
  from the pane lines, so add the window → session map the cockpit flag needs. `%session-renamed`
  changes which windows count as the cockpit, by the session's new name.
- [ ] **Step 4: Run them to see them pass**: the workspace, and `moa-tmux` and `moa-core` on
  tmux 3.4.
- [ ] **Step 5: Commit:** `v1: tmux commands that move panes, chains, and tags in the topology`.

## Task 5: the cockpit engine

**Files:**
- Create: `crates/moa-core/src/cockpit/{mod.rs, scene.rs, assign.rs, build.rs, sim.rs}`
  (`sim.rs` is `#[cfg(any(test, feature = "testkit"))]`, following how the core exposes test
  helpers today).
- Test: `crates/moa-core/tests/cockpit.rs` (new).

**Interfaces:**
- Produces:
  - `pub enum Role { Panel(View), Spot(Option<SessionId>), Agent(SessionId), Other }`. `Spot(None)`
    is an empty slot in the cockpit, `Spot(Some)` an agent tagged `agent`; a panel tagged
    `empty-slot` outside the cockpit is `Panel(EmptySlot)`.
  - `pub struct PaneAt { pub pane: PaneId, pub window: WindowId, pub place: Place, pub alone: bool, pub dead: bool, pub role: Role }`,
    with `pub enum Place { Cockpit, Moa, Hidden, Elsewhere }`.
  - `pub struct Scene { pub panes: Vec<PaneAt> }`, built by `Scene::read(topology: &Topology, bound: impl Fn(PaneId) -> Option<SessionId>) -> Scene`.
  - `pub struct Plan { pub spawn: Vec<View>, pub build: Option<Build> }`. `build` is `None` while
    `spawn` isn't empty: start those first, then plan again.
  - `pub struct Build { pub chain: Cmd, pub views: Vec<(PaneId, View)>, pub spot: Option<PaneId>, pub first: PaneId, pub console: Option<PaneId> }`.
  - `pub fn plan(scene: &Scene, layout: &Node, title: impl Fn(SessionId) -> String) -> Plan`
    (L5, L6, L10). Selecting the result is the caller's.
  - `pub fn swap_in(scene: &Scene, pane: PaneId, title: …) -> SwapIn`, with
    `pub enum SwapIn { Swap(Cmd), Select(Vec<Cmd>), NeedSlot }` (L11's swap and old-occupant
    handling).
  - `pub fn vacate(scene: &Scene, spot: PaneId) -> Vacate`, with
    `pub enum Vacate { Swap(Cmd), NeedSlot }`: the slot swapped in before a kill.
  - `sim::World::from(&Scene)`, `World::run(&Cmd)` (swap, join, break, move-window and
    set-option over a membership model: which pane is in which window and session; no
    geometry), and `World::cockpit_leaves()`.

- [ ] **Step 1: Write the failing tests** (`cockpit.rs`):
  - `the_first_cockpit_spawns_its_three_panels`: a scene with only the login shell in the
    cockpit → `spawn == [Tree, Console, Plan]`, and `build` is `None`.
  - `with_panels_parked_the_cockpit_builds_in_one_chain`: tree, console and plan parked in
    `_moa`, plus the shell in the cockpit. The plan has no spawn, and the chain's commands are,
    in order:
    1. swap the tree with the shell;
    2. `move-window -s <shell> -t =moa:`;
    3. `rename-window`, then the shell's two `set-option`s (`@moa_term`, `@moa_label`);
    4. `join-pane -d -h -s <console> -t <tree>`, then `join-pane -d -h -s <plan> -t <console>`;
    5. `resize-pane -t <tree> -x 26%`, then `resize-pane -t <plan> -x 30%`;
    6. the tags (L10).
  - `the_focus_layout_nests`: the `focus` preset from parked panels. The joins, in order:
    1. `-h` the agent spot's pane after the tree;
    2. `-v` the plan under the tree;
    3. `-v` the console under the spot.

    Then the resizes: `-x 24%` on the tree, `-y 45%` on the plan, `-y 30%` on the console.
  - `a_switch_reuses_panels_before_starting_one`: from a built `cockpit` to `triage`. The tree
    panel is reassigned (`views == [(tree's pane, Status)]`), and nothing is spawned.
  - `the_spot_agent_goes_home_when_the_layout_has_no_spot`: from `focus` with an agent in the
    spot to `cockpit` → `break-pane -d -s <agent> -t =moa: -n Lantern`, and the agent's tags
    are unset.
  - `a_dead_panel_is_not_reused`: a dead `tree` panel in `_moa` → the tree comes from a spare,
    or is spawned; the dead pane is untouched.
  - `swap_in_parks_the_empty_slot`: the empty slot is in the spot, and
    agent `%9` is alone in its tab → `swap-pane -d -s %9 -t <slot>`, `move-window -s <slot> -t =_moa:`,
    and the tags. If agent `%9` shares its tab, the second command is `break-pane`.
  - `swap_in_sends_the_old_occupant_home_by_title`: agent B in the spot, and A focused → the swap,
    then `rename-window -t <B> Kite` and B's tags unset.
  - `vacate_needs_a_slot_when_none_is_spare`.
  - The proptest `no_switch_kills_or_strands_a_pane`. Scenes are random: 0–3 agents in their
    own tabs, one or none in the spot, 0–2 strangers in the cockpit, 0–5 panels in random
    places, some dead. The layout is a random preset. Once the spawned panels are added as
    parked, the plan is run in `World`, and then:
    1. no pane is lost;
    2. the cockpit's panes are exactly one per leaf;
    3. every agent outside the cockpit is in session `moa`;
    4. every other panel is in `_moa`;
    5. every stranger is in session `moa`;
    6. no dead pane moved.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** L5 and L6, with the scene from Task 4's topology. `title` cuts to 24
  characters.
- [ ] **Step 4: Run them to see them pass** (the proptest at its default case count, plus one
  10 000-case run recorded in the report).
- [ ] **Step 5: Commit:** `v1: the cockpit engine: assign, build, swap_in, vacate`.

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
  - `Asked::Spawn { views: Vec<View>, then: Then }`, with
    `pub(crate) enum Then { Apply { name: String, tell: Option<Tell>, select: bool }, Show { pane: PaneId, tell: Option<Tell> }, Vacate { id: SessionId, pane: PaneId, why: Vacating } }`;
  - `pub(crate) enum Vacating { Sleep, Close, Restart }`: why a session leaves the spot (Task 10
    uses it);
  - `pub(crate) fn spawn_cmds(state: &State, views: &[View]) -> Vec<Cmd>` (L7, one `new-window`
    each);
  - on the answer, the tags and options, and then the `then` step run again with a fresh scene
    (the new panes added to the topology from the answers);
  - `State.panel_starts: u32`.

- [ ] **Step 1: Write the failing tests** (`cockpit_run.rs`, with `link_up`):
  - `spawning_runs_moa_panel_with_its_view_and_brain`: one `Tmux` effect with
    `new-window -d -P -F #{pane_id} -t =_moa: -n tree -e MOA_BRAIN=default -e MOA_ROOT=/r /x/moa panel tree --brain default`.
    The answer `%12` → `set-option -p -t %12 @moa_panel tree`, `remain-on-exit on` and the
    format, and `panel_starts == 1`.
  - `outside_the_sandbox_no_moa_root_is_passed`: with no `MOA_ROOT` in `Deps.panel.env`, there
    is no `-e MOA_ROOT`.
  - `a_spawn_whose_answer_never_comes_starts_nothing_else`: the link goes down → the `then`
    step is dropped, with one log line.
  - `status_reports_panel_starts`.
  - `crates/moa-daemon/tests/tmux.rs`: `a_spawned_panel_runs_in_the_hidden_session` (real
    tmux, with a stand-in `exe` script that sleeps): the pane is in `_moa`, its window is named
    `tree`, and it carries `@moa_panel tree`. On tmux 3.4 and 3.8, `remain-on-exit-format` is
    set or, per Review Focus 5, skipped.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** The daemon fills `Deps.panel` from `std::env::current_exe()` and
  its `Paths` (`MOA_ROOT` only when sandboxed).
- [ ] **Step 4: Run them to see them pass**, plus the tmux 3.4 pass on `moa-daemon`.
- [ ] **Step 5: Commit:** `v1: moa starts its own panels, parked, with their views`.

## Task 7: layout switching, goto, Repair and the lazy cockpit

**Files:**
- Create: `crates/moa-core/src/keys.rs` (the one key dispatch, L13)
- Modify:
  - `crates/moa-core/src/{core.rs, op.rs, ops/layout.rs, cockpit/mod.rs, tmux/link.rs, tmux/mailbox.rs}`;
  - `crates/moa-proto/src/cli.rs` (`Verb::Goto` becomes `Op::Goto`; drop it from the not-yet list).
- Test: `crates/moa-core/tests/cockpit_run.rs`, `crates/moa-core/tests/tmux_keys.rs`,
  `crates/moa-proto/tests/cli.rs`.

**Interfaces:**
- Produces:
  - `Op::LayoutApply { name: Option<String> }` (`layout.apply`, a write) and
    `Op::Goto { panel: Leaf }` (`goto`);
  - `pub(crate) fn apply(state, tell: Option<Tell>, name: Option<&str>, select: bool) -> Vec<Effect>`;
  - `pub(crate) fn repair(state, tell) -> Vec<Effect>`;
  - `pub(crate) fn ensure_cockpit(state) -> Vec<Effect>`;
  - `keys::run(state, key: KeyAction, pane, client) -> Option<Vec<Effect>>` (`None` → the link's
    not-yet notice);
  - `Asked::Build { name: String, tell: Option<Tell>, select: bool }`, the chain's answer;
  - `State.cockpit: CockpitState { building: bool, spot: Option<PaneId> }`;
  - the meta write `layout.current` in the commit of an apply that succeeded;
  - the `ui` event `layout → <name>` (the spike's `log_ui`).

- [ ] **Step 1: Write the failing tests:**
  - `layout_apply_builds_and_remembers`: with the panels parked → one chained `Tmux` (no
    spawn). Its answer `Ok` → the meta `layout.current`, the `layouts` and `panels` topics
    published, the `ui` event, and the reply `layout focus applied`.
  - `a_failed_build_says_so_and_the_next_apply_mends_it`: the chain's answer is
    `Err("can't find pane %4")` → the reply is `layout focus couldn't be applied: can't find pane %4; Ctrl-a r tries again`,
    and the meta is unchanged. A second apply, from a fresh listing, succeeds.
  - `the_layout_keys_apply_the_presets`: `moa:key layout-2 %3 $1 /dev/ttys1` → the apply of
    `focus`, answered by a notice to `/dev/ttys1`.
  - `goto_selects_or_reapplies`: `goto tree` with the tree in the cockpit → `select-window` and
    `select-pane` on its pane. With the tree missing → the apply, then the select. `goto status`
    in `cockpit` → `the cockpit layout has no status panel (moa layout list)`. `goto agent`
    finds the empty slot.
  - `repair_respawns_dead_panels_in_place`: a dead tagged `%7` (`plan`) →
    `respawn-pane -k -t %7 /x/moa panel plan --brain default` (with the `-e`s), then the apply,
    and the notice `repaired: 1 panel restarted`. `panel_starts` goes up by one.
  - `a_dead_panel_is_logged_once`: `moa:hook pane-died %7` twice for a tagged pane → one log line,
    `panel plan in %7 stopped`.
  - `the_first_attach_builds_the_cockpit`: `moa:hook client-attached /dev/ttys1` with nothing
    tagged in the cockpit → spawns, then the build, with no `select-*`. A second
    `client-attached` while it's building → nothing.
  - `two_attaches_build_the_cockpit_once`: two `client-attached` hooks before the spawn answer →
    one spawn and one build.
  - `a_built_cockpit_isnt_rebuilt_on_attach`.
  - `link_up_with_a_user_attached_builds_it`.
  - `tmux_keys.rs`: `every_key_gets_its_notice` now covers only `Find`, `FindOther`, `New` and
    `Help`; `the_cockpit_keys_act` covers `c t p g 1 2 3 4 r`.
  - `cli.rs`: `goto_parses` (`goto tree`; `goto nope` → a usage error naming the nine words).
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** The chain's answer comes back through `Asked::Build { name, tell, select }`.
  `Tx::tmux` isn't used for the build, because the build waits for its answer.
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: layout switching, goto, Repair, and the cockpit on first attach`.

## Task 8: panels follow moa's view

**Files:**
- Modify:
  - `crates/moa-core/src/{panel.rs, core.rs, op.rs}` (`Op::UiSelectView`; the tag rule; the
    generation clear);
  - `crates/moa-view/src/panel/mod.rs` (`Panel::follow(&Data, key)`);
  - `crates/moa-tui/src/run.rs` (keep the key from the reply; call `follow` on a `panels` change).
- Test: `crates/moa-core/tests/panels.rs`, `crates/moa-view/tests/panel.rs`,
  `crates/moa-tui/tests/run.rs`.

**Interfaces:**
- Produces:
  - `Op::UiSelectView { pane: String, view: View }` (`ui.select_view`, not a write);
  - the `ui.panel` reply `{key, view, managed}`;
  - `Panel::follow(&mut self, data: &Data, key: &str) -> bool` (true when the view changed);
  - `Panel::managed(&self) -> bool`, set from the reply.

- [ ] **Step 1: Write the failing tests:**
  - `the_tag_wins_over_the_view_a_panel_asks_for` (`panels.rs`): `%3` is tagged `status` in the
    topology, and conn 7 registers `%3` with `view: tree` → `{view: status, managed: true}`. An
    untagged `%4` with `view: tree` → `{tree, managed: false}`.
  - `select_view_retags_and_publishes`: `UiSelectView { pane: "%3", view: Plan }` →
    `set-option -p -t %3 @moa_panel plan`, the label and the frame options, then `panels`
    publishing `c7` as `plan`, then the reply.
  - `a_new_tmux_server_clears_the_panes_of_registered_panels`.
  - `panel.rs` (moa-view): `a_panel_follows_its_row`: a `panels` row for its key with `plan` →
    `follow` returns true, and `view() == Plan`. A row for another key → nothing changes.
  - `run.rs`: `a_reconnect_sends_the_view_moa_last_gave`. After a `follow` to `plan`, the
    reconnect's first ops carry `view: plan`; the tag still decides.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: panels follow the view moa gives them`.

## Task 9: the agent spot

**Files:**
- Modify: `crates/moa-core/src/ops/{launch.rs, term.rs}` (`show`, `focus`, the `$` fallback),
  `crates/moa-view/src/views/{mod.rs, empty_slot.rs}` (the empty-slot view).
- Test: `crates/moa-core/tests/cockpit_run.rs`, `crates/moa-core/tests/panels.rs`,
  `crates/moa-view/tests/…` (snapshot), `crates/moa-tui/tests/render.rs`.

**Interfaces:**
- Produces:
  - `pub(crate) fn show(state: &mut State, pane: PaneId, tell: Option<Tell>) -> Vec<Effect>` (L11),
    used by `focus`, by the open and resume answers (in place of `focus_cmds`), and by the
    tree's Enter (through `focus`);
  - the reply text `brought it into the cockpit` (a swap) or `switched to its tab`;
  - `views::empty_slot::build(width) -> Vec<Line>`, three dim lines (the spike's
    `EMPTY_SLOT`): `no agent here yet`, a blank line, `press Enter on a session in the tree`,
    `to bring it into this spot`; unframed, no keys.

- [ ] **Step 1: Write the failing tests:**
  - `focus_swaps_into_the_spot`: the `focus` layout is built, and Lantern is live in its own
    tab `%9` → `swap-pane -d -s %9 -t <slot>`, the parking of the slot, the tags,
    `select-window -t %9`, `select-pane -t %9`, and the reply `brought it into the cockpit`.
    `panel_starts` doesn't change.
  - `focus_with_no_spot_selects_its_tab`: in `cockpit` → 4b's commands, and
    `switched to its tab`.
  - `focusing_the_spots_agent_only_selects_it`.
  - `a_resumed_session_lands_in_the_spot`: a resume's open answer in `focus` → the swap in
    place of `focus_cmds`.
  - `the_terminal_key_falls_back_to_the_spots_folder` (`panels.rs`), replacing 5a's "home until
    5b" test: a panel with no `ui.here`, and Lantern in the spot with cwd `/r/work/a` →
    `-c /r/work/a`. With an empty spot → home.
  - The empty-slot model snapshot at 80 columns, and its `TestBackend` text at 40 and 80.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Remove `the agent spot isn't in this build yet` (the `EmptySlot`
  arm of `not_yet`, and the `g` key's, which Task 7 already routes).
- [ ] **Step 4: Run them to see them pass**, then the workspace.
- [ ] **Step 5: Commit:** `v1: focus swaps a session into the agent spot`.

## Task 10: sleep, close and restart leave an empty spot

**Files:**
- Modify: `crates/moa-core/src/{sleep.rs, ops/sessions.rs, ops/launch.rs (restart), cockpit/mod.rs, core.rs (the spot's pane exiting)}`
- Test: `crates/moa-core/tests/cockpit_run.rs`, `crates/moa-core/tests/sleep.rs` (or wherever 4b's
  sleep tests live), `crates/moa/tests/sleep_e2e.rs`.

**Interfaces:**
- Produces:
  - `kill_tab` becomes spot-aware. For the spot's pane it emits `vacate`'s swap before the
    `kill-pane`, in one chain. With `NeedSlot`, it spawns first: `Then::Vacate { id, pane, why }`,
    and the kill and the commit's meaning (asleep, closed, restarting) wait for the answer;
  - `why_not` drops `in the cockpit` for the spot: the spot can sleep, but `on screen` still
    holds;
  - `pane-exited`/`pane-died` of `State.cockpit.spot` re-applies the current layout, with no
    select.

- [ ] **Step 1: Write the failing tests:**
  - `sleeping_the_spots_agent_parks_a_slot_first`: Lantern in the spot, not visible, idle past
    the threshold, a spare empty slot parked → one chain,
    `swap-pane -d -s <slot> -t %9 ; kill-pane -t %9`; `asleep_at` is set; `panel_starts`
    is unchanged.
  - `sleep_q_on_the_spot_with_no_spare_starts_a_slot_then_sleeps`: the spawn, then, on its
    answer, the chain and the commit. If the link goes before the answer, nothing is killed
    and the reply says so.
  - `closing_the_spots_agent_leaves_the_empty_slot` (`x` / `session.close`).
  - `a_spot_whose_agent_ends_refills`: `moa:hook pane-exited %9` for the spot → the apply,
    which builds with a spare empty slot.
  - `a_late_start_from_the_killed_tab_stays_history_across_a_reconnect_to_the_same_server`
    (5a carry): the kill, then the link drops and comes back to the same generation, then a
    `SessionStart` from the killed tab → the session stays asleep.
  - `sleep_e2e.rs`: `the_spot_sleeps_and_shows_no_agent_here_yet` (real tmux, `focus` layout,
    a fake agent in the spot, threshold 0 through `sleep Q`). The spot's pane shows
    `no agent here yet` within 10 s, the agent's process is gone, and `ls` shows the session
    asleep.
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
    `this panel is part of moa's layout; Ctrl-a r redraws the layout`;
  - the log lines `panel <view> in <pane|->: started`, `…: gave up reaching moa`,
    `…: stopped by <signal>`, `…: panicked: <message>`.

- [ ] **Step 1: Write the failing tests:**
  - `ctrl_c_cancels_a_prompt_first` (moa-view);
  - `ctrl_c_on_a_bare_managed_panel_stays_and_says_so`;
  - `ctrl_c_quits_a_panel_you_started` (unmanaged).
  - `run.rs`: `focus_gained_redraws`; `nothing_is_drawn_before_the_first_link_when_no_view_was_asked`.
  - `panel_e2e.rs`: `sigterm_and_sighup_give_the_terminal_back`: a panel in a sandbox tab. Send
    `kill -TERM <pid>` (and, in a second run, `-HUP`). The tab's terminal is no longer in the
    alternate screen, its mouse reporting is off (`#{mouse_any_flag}` is 0), and `panel.log`
    has `stopped by SIGTERM`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Signals come through `tokio::signal::unix` in the loop's `select!`,
  and they end the loop like Ctrl-c does. The panic hook appends its line before `exit(101)`.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: panels cancel, log, and give the terminal back on every signal`.

## Task 12: the cockpit end to end

**Files:**
- Create: `crates/moa/tests/cockpit_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs` (`panes_of(tb, target) -> Vec<PaneRow>`, holding the
  pane id, pid, tag, left, top, width, height, and window name).

**Interfaces:**
- Consumes everything above. A helper `cockpit_up(tb) -> Terminal` attaches a terminal and
  waits for the cockpit to hold its three tagged panes.

- [ ] **Step 1: Write the tests** (real tmux, a real terminal, real `moa panel` processes, and
  `fake-claude` agents):
  - `the_first_attach_builds_the_cockpit`:
    - the cockpit window holds tree, console and plan, side by side, at about 26% / 44% / 30%
      of the width (±2 columns);
    - each shows its frame title (`▤ sessions`, `◷ big picture`) by `screen_of`;
    - the login shell is in its own tab `⌨ shell`, still running;
    - `panel_starts == 3`.
  - `a_layout_switch_moves_panes_and_starts_nothing`:
    1. Warm up: press `Ctrl-a 1 2 3 4` once each.
    2. Record `panes_of` and the `launches` and `panel_starts` counts.
    3. Press `Ctrl-a 2 4 1 3` and wait for each layout's leaves to show.
    4. Check: the counts are unchanged, and every panel pane keeps its pid.
    5. For each preset, check the geometry: the leaves' order and split direction from the
       panes' left, top, width and height.
  - `focus_swaps_and_the_old_agent_goes_home`: in `focus`, open Lantern and Kite. Enter on
    Lantern in the tree → Lantern's pane is in the spot. Enter on Kite → Kite is in the spot,
    and Lantern's pane is alone in a tab named `Lantern`. Both agents keep their pids.
  - `repair_brings_a_killed_panel_back`:
    1. `kill -9` the plan panel's process.
    2. Its pane reads `panel stopped · Ctrl-a r brings it back`, and the daemon's log has
       `panel plan in %N stopped`.
    3. Press `Ctrl-a r` → the plan panel draws again in the same place.
    4. `panel_starts` went up by exactly one.
  - `frame_colours_follow_focus`: the tree's and plan's panes have `window-style` `fg=#4c566a`
    and `window-active-style` `fg=#88c0d0`. After `select-pane` onto the tree, the tree's frame
    cell in the terminal is drawn in `#88c0d0` and the plan's in `#4c566a` (by `cell_of`).
  - `a_reassigned_panel_keeps_moas_view_after_moa_restarts`:
    1. `triage` reassigns the tree panel to status.
    2. `moa shutdown`.
    3. Run any `moactl` (it starts moa again).
    4. The panel reconnects and still shows `◉ status`, with the same pid.
  - `goto_keys_select_their_panels`: `Ctrl-a p` makes the plan pane active; `Ctrl-a g` in
    `focus` makes the spot active.
- [ ] **Step 2: Run them**; tests that pass straight away are fine. Make each one fail once on
  purpose (break the rule it pins) and note how in the report.
- [ ] **Step 3: Run each new test 10 times under load** (the test binary run directly, with no
  cargo running), and on tmux 3.4.
- [ ] **Step 4: Commit:** `v1: the cockpit end to end in the sandbox`.

## Carried, and where each goes

| Item (from) | Where |
|---|---|
| Focus swaps into the agent spot; sleep parks an empty slot (4b) | Tasks 9, 10 |
| `is_cockpit` costs a listing per rename (4b) | Task 4 (L16) |
| `a ; b` plus sentinel transactions (slice 3 T14) | Task 4 |
| `Topology::session_named`, `Sandbox::tmux`'s PATH lookup (slice 3) | Task 4 |
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
| Setup snags from the 5a try-it: the sandbox starts with no `moa.toml`, moa's own tabs count as an unlisted `~/.claude`, and the sandbox's `~` misleads | **Slice 8** (setup); the 5b try-it gives the working `moa.toml` |
