# moa v1 slice 5a: the panels and their views

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `moa panel tree` (and `plan`, `status`, `today`, `projects`, `activity`, `picture`)
shows the brain live in a rounded frame, in the spike's colours, with spinners and flashes. Its
keys and clicks act on sessions, groups, tasks and suggestions, and `Ctrl-a $` opens a terminal
in the folder the panel points at. Nothing polls, and nothing launches except what `moactl`
already may: a panel's first connect starts moa's daemon when none runs (V13).

**Architecture:**
- **`moa-view`** (new, pure): a typed mirror of the published topics (`Data`); one builder per
  view that lays the rows out for a width (`Vec<Line>`, each line carrying the item it stands
  for); the scroll and selection model (lesson 20); the keymap and `Action`s; the footer editor;
  and `Panel`, the state machine that turns keys, clicks and replies into ops to send.
- **`moa-tui`** (new): the palette, the frame, drawing a `Screen` into a ratatui buffer, and the
  event loop: terminal input, the daemon's pushes, an animation tick only while something on
  screen moves, and one deadline for the next text that changes with time.
- **The core** keeps a registry of connected panels (`ui.panel`, `ui.here`) and publishes it as
  the `panels` topic, plus a `brain` topic (folders, accounts, folds). Tree folds are saved
  (`group.fold`). The `$` key opens a terminal in "here".
- **`moa-client`** gets `Live`: one subscribed connection driven by its own task, so a reply that
  takes a while (a focus waiting for its tab) never freezes the panel.

```
 moa panel tree ─► Client::connect ─► hello matched? ─► live(TOPICS, [ui.panel {pane, view}, ui.here {folder}])
   loop: terminal key/click/paste/resize ─► Panel::key|mouse ─► Out::Send(op) ─► Live::send ─► reply ─► footer
         snapshot/patch ─► Data ─► Panel::screen(size) ─► moa_tui::draw ─► terminal
         animation tick (100 ms) only while Screen.animate · one deadline at Screen.wake_at
         Closed ─► backoff ─► connect again (same first ops, new key, Data replaced)
 Ctrl-a $ ─► moa:key terminal %N ─► here_of(%N) | session cwd | #{pane_current_path} | ~ ─► new-window
```

**Tech Stack:** ratatui 0.30 + crossterm 0.29 (`event-stream`), `unicode-width`,
`unicode-segmentation`, `textwrap` (with `unicode-width`), `insta`, `proptest`, tokio. New crates
`moa-view` and `moa-tui` (roadmap's crate table, slice 5).

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.13 (panels, views, common keys, mouse, `ui.here`, frames), §6.14 (colours), §6.2 (`moa-view`,
  `moa-tui`), §6.4 (topics `brain`, methods `ui.here`, `ui.select_view`), §6.5 (`groups`, `meta`);
- §7 rows "Panel common chrome…", "`@moa_here` publishing", "Animation", "tree/plan/status/today,
  projects, activity, picture views", "Robustness: degrade the entry, not the frame";
- §8 lessons 11–12 (frames), 16–21 (rendering, widths, scroll, mouse), 70 (alive).

Roadmap: slice 5 items 1, 2, 3, and from 6 `ui.here`. Parity source (read only, never run):
`spike/moa-proto/picture.py` (views, `follow`, `row_at`, `page`, `ask`, `tree_key`, `main`),
`chrome.py` (palette, `xterm256`, `frame`), `layout.py` (`LABELS`, `FRAMED`), `moactl.py`
(`here_dir`), `tmux.conf`, `help.txt`. R1 §2.3, §2.4.

**Builds on** slices 2–4b: `State` (incl. `meta`), `Tx` (`commit`), `post_commit`, `finish_op`,
`Effect`, `Change::{Meta, topics}`, `check_order`/`handle_checked`, `publish::{Topic, Model,
word}`, `live::{DisplayState, display_state, is_asleep}`, `model::*`, `config::{Config,
AccountConfig}`, `time::{Now, start_of_next_day}`, `tmux::{Cmd, literal_format,
mailbox::{Mailbox, KeyAction}, ids::PaneId, link::{Asked, notice, set_pane_option}}`,
`ops::launch::{term, term_opened, focus_cmds}` (4b R16), `Deps::is_dir`; `moa_proto::{SnapshotMsg,
Patch, Subscribe, allowed_on_mismatch}`; `moa_client::{Client, ClientOptions, OnMismatch,
own_exe}` and its private `Wire`; `moa/src/ctl.rs` (`Here`, `pick_brain`, `runtime`);
`moa_platform::{Paths, TmuxSocket, Paths::tmux_socket}`; `moa_tmux::conf::moa_tmux`; the testkit
(`TempBrain`, `Terminal`, `wait_for`, `fake_claude`, `write_script`, `FakeStep`),
`crates/moa/tests/common` (`open`, `came_up`, `tmux`, `status_json`, `moa_toml_with_fake_account`,
`moactl`, `tab_shows`), `crates/moa-core/tests/common` (`loaded`, `link_up`, `send`, `changes`,
`replies`), `crates/moa-core/tests/skeleton.rs` (op samples, `WRITES`).

## Slice 5 is three plans

| Plan | Roadmap items | Carried in | Done when |
|---|---|---|---|
| **5a (this one): the panels and their views** | 1, 2, 3; `ui.here` from 6 | 4b: the `$` key, `ui.here`, the tree's restart key | Every view's snapshots pass at widths 40–200 with 💤, CJK and emoji. In the sandbox, `moa panel <view>` follows the brain, and its keys and a click act (e2e by `capture-pane`). `Ctrl-a $` opens where the panel points. Keys, clicks, scrolls and `$` make zero `Spawner` launches. |
| 5b: layouts, the cockpit and the agent spot | 4; 6 (supervision, Repair); keys `c t p g`, `1–4`, `r`; `goto`; `layout list/show/set/apply`; `ui.select_view` | 4b: focus swaps into the agent spot; sleep parks an empty slot first (also `sleep Q` on the spot's session, which today empties it); `is_cockpit` costs a full listing per tab rename (4b ledger, its Task 11). Slice 3: T14's `a ; b` + sentinel transactions; the small cleanups in files it touches (`Topology::session_named`, `Sandbox::tmux`'s PATH lookup). 5a: the agent-spot fallback for `$` | A layout switch and `focus` make zero `Spawner` launches once the panels are parked; sandbox e2e of a layout switch; a killed panel comes back on Repair and is logged; frame colours follow focus |
| 5c: the picker, help, and new sessions | 5; the `n` key | Slice 3 "Prompts" (`\$` only on 3.4 with the version in the core, `command-prompt -b`, the double-quoted template `Cmd`): 5c sends the first `command-prompt`. Help's "hold Shift to select text" | Sandbox e2e of the picker in both forms and of help; opening the full-window picker makes zero launches, the popup one per open |

**Why three:** P1. 5b's layout engine reassigns views through 5a's registry and moves the
panels 5a makes. 5c's full-window picker is a parked panel shown as a linked tab, so it needs 5b's
parking. Each part's plan is written just before it starts.

## Calls for you to check

Most important first. Each is a rule below (V-number) with its reason.

1. **Three plans for slice 5, not two** (V1): views and panels now; layouts and the agent spot
   next; the picker, help and the `n` key last.
2. **moa turns the tmux mouse on** (`set -g mouse on` in its own config), so clicks and the wheel
   reach panels for everyone. moa's config loads after yours, so this **overrides a `mouse off`
   in your own tmux config**. The opt-out is `mouse = false` under `[ui]` in `moa.toml`: moa then
   leaves the mouse to your config. It takes effect when moa's terminal next starts (a running
   one keeps the mouse it has). With the mouse on, selecting text needs Shift (V11).
3. **The daemon tells a panel its view through a published list** (`panels`), not a message sent
   to that panel alone. In 5a a panel asks for its view (`moa panel tree`); 5b's layouts change it
   in the same list. A slow panel can miss a one-off message but never a list update. A panel
   registers again on every reconnect (V3, V13).
4. **Folds are saved in the brain, but never as groups.** The fold state is one saved list of
   folded tree paths, so folding `Loose` or a project's node doesn't turn it into a permanent
   group. The `groups.collapsed` column stays unused. Expanding a project stays per panel, as in
   the spike (V5).
5. **New tree key `R`: restart an adopted session under moa** (4b left the key to slice 5; the
   spike had none). It asks y/N first, since it closes that tab (V7).
6. **`n` and `N` check the folder before asking more.** Right after the folder prompt, a folder
   that doesn't exist (`no folder ~/x`) or doesn't start with `~` or `/` stops the prompts and
   sends nothing. The spike's tree created missing folders through `moactl open`; a typo in a
   footer prompt shouldn't leave folders behind. Whether the folder belongs to another brain
   depends on the account too, so moa itself answers that, after the prompts, in the words
   `moactl open` uses (V7).
7. **Ages and dates refresh at exact moments** (the next minute an age changes, local midnight),
   not on a ticking timer. The spike redrew every 300 ms (V10).
8. **A panel that loses moa retries for about a minute, then waits for a key.** After its first
   connect it never starts moa by itself, so `moa shutdown` stays shut down (V13).
9. **A panel on a different build than moa says so and stays up**; it doesn't restart moa under
   the other panels (that is `moactl`'s rule). Slice 9 makes panels restart themselves (V13).
10. **Colours inside tmux are always exact 24-bit**; tmux converts them for a terminal that lacks
    them. The spike's 256-colour mapping is used only outside tmux without `COLORTERM=truecolor`.
    The theme file and its hot reload wait for slice 9 (V12).
11. **Emoji in titles are simplified for alignment:** `⚠️` shows as `⚠`, `👍🏽` as `👍`, a family
    `👨‍👩‍👧` as `👨`, a flag `🇮🇱` as `IL`. tmux and the panel count those sequences differently, and the frame would
    break (V9).
12. **Your call: clicks never surprise.** The row under the mouse pointer gets a soft highlight
    (the palette's `surface` grey, under the row's own colours), so you see what a click will act
    on before you click; the click then acts on that row. Moving the mouse redraws a panel only
    when the pointer reaches another row. The highlight clears when you press a key, when the
    panel loses focus, or when that row goes away. Terminals don't say when the pointer leaves a
    panel, so **a highlight can linger on a panel the pointer left** until one of those happens;
    on a panel you never clicked into (so it never gets a key or loses focus), it can stay
    indefinitely, until its row changes or the pointer comes back.
    If tmux 3.4 turns out not to pass mouse movement to panels, the fallback is your third choice:
    the first click on a pane that isn't focused only focuses it (V11, V22).
13. **v1's new states get colours:** failed is a red ✗, quiet a cyan ◌ (V16).
14. **Every key and every y/N acts on the row you saw selected**, by identity. The spike used the
    row's position, so an update arriving between your look and your key could shift it (V8).
15. **Small ones:** Ctrl-c quits any panel and `q` only the picture (spike); the activity list
    scrolls with j/k too (the spike needed the wheel); control characters in titles show as
    spaces; prompts say "folder:" where the spike said "path:"; `+` at the top level starts empty
    (the spike prefilled `Private/`); `n` on a row outside the tree opens with the brain's default
    account and `N` asks which, where the spike had one fixed account per key; the picture redraws
    10 times a second while any session works or waits, even when its shown tab has nothing
    moving, because its brand spins (the spike did the same) (V15, V17, V9, V7, V21).

## PRs (roadmap P4)

Six PRs, each about 1500 lines of diff at most. Each branches from `main` after the previous one
merged, and each gets the full verify (`cargo fmt --check`; clippy, also
`--target x86_64-unknown-linux-gnu` for `moa-tmux`; `cargo nextest run --workspace` three times;
`nix flake check`), the tmux 3.4 pass on the tmux-facing crates it touched, `/code-review medium
--fix` with the fixes pushed, and green CI. Bodies end with the try-it (when there is one). No
"Restart needed" line: the user dropped it (2026-11-18); a body names a restart only if a PR
touches the spike.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-5a-1` | 1–3 | `v1 slice 5a (1/6): panels in the core, the $ key, saved folds` |
| p2 | `v1/slice-5a-2` | 4–5 | `v1 slice 5a (2/6): moa-view's data, text, lists and keys` |
| p3 | `v1/slice-5a-3` | 6–7 | `v1 slice 5a (3/6): the panel, the tree and the big picture` |
| p4 | `v1/slice-5a-4` | 8–10 | `v1 slice 5a (4/6): status, today, projects, activity, the picture` |
| p5 | `v1/slice-5a-5` | 11 | `v1 slice 5a (5/6): every view drawn, at every width` |
| p6 | `v1/slice-5a-6` | 12–14 | `v1 slice 5a (6/6): moa panel, live, end to end` |

p3 is the one most likely to run long. If it passes ~1500 lines, Task 7 (the plan and V21's keys)
moves to the head of p4, and p3's title drops "and the big picture".

**Done already** (committed with this plan, so no PR does it): the roadmap's slice 5 points to the
three plans; its shared-names table has `View`, `PanelReg` (`moa_core::panel`, 5a), `Data`, `Line`,
`Panel`, `Screen` (`moa_view`, 5a) and `Live` (`moa_client`, 5a); and slice 6's item 3 says "the
button bar is a runtime list moa sets (and the user can override), not a fixed set; the user's
call, 2026-11-18."

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| V1 | **Slice 5 is three plans** (table above). | P1: 5b's signatures depend on 5a's registry and panels, 5c's on 5b's parking. Each is already several PRs. |
| V2 | **`moa-view` holds everything that isn't drawing or IO:**<br>• `Data`, the typed mirror of the topics;<br>• one builder per view, `build(data, ctx, ui, width) -> Vec<Line>`, where `width` is the body's (`geometry.body.w`, at least 10); each `Line` carries its `Item`, so another client can lay the same items out its own way;<br>• `ListState` (scroll and selection), the keymap, the footer `Editor`;<br>• `Panel`, which turns keys, clicks and replies into `Out`s, and builds the `Screen` to draw.<br>`moa-tui` maps tones to colours, draws, and runs the loop. `moa-view` depends on `moa-core` and pure crates only (a CI `cargo tree` step, like `moa-core`'s). | Spec §6.2: `moa-view` = view-models shared by every client (rows, `Action` enum, default keymap), no IO. Roadmap 5.1: "pure, with insta snapshots of the models". The spike's rows depend on the width (`build(…, max(10, iw))`, columns at 34, 44, 72 and 76, wrapping), and the scroll model counts lines. |
| V3 | **Panels register with the daemon:**<br>• `ui.panel {pane?, view?}` → `{key, view}`. The view is the one asked for, else the one this pane last had in this daemon's life, else `picture` (the spike's no-flag panel);<br>• the registry is in the core's memory (`State.panels`, by `ConnId`), dropped on `ClientGone`; registering a pane another connection holds moves it to the newest;<br>• it is published as the topic `panels`: rows `{key: "c<conn>", pane, view}`;<br>• `ui.here {folder?}` records the panel's selected folder; the panel sends it when it changes and on every (re)connect (V13). It is not published.<br>`ui.select_view` (reassigning) comes with 5b's `goto`. | Spec §6.13: "it connects with its pane id; the daemon assigns its view and can reassign it"; "panels publish their selected row's folder (`ui.here`), which `term` uses for here (no tmux option writes)". Events are a bounded broadcast and a lagging client misses them; patches of the model can't be missed, so a reassign rides the model. |
| V4 | **Two new topics and two small row changes:**<br>• `brain`: one row `{key: "brain", name, folders, accounts, open_account, folded}`: folders as absolute paths; accounts = listed accounts with a launcher; `folded` = V5's list;<br>• `panels` (V3);<br>• session rows gain `"adopted": bool` (an untracked binding);<br>• `DisplayState` gains `Deserialize`. | Spec §6.4 lists `brain`. `N` "choose the account: replaces the spike's two hardcoded accounts" (§6.13) needs the list; whether an account may open in a given folder is the core's `route` (account, folder, `except`, `only`), so the core answers that. `n` prefills the brain's first folder (V7). The `R` key needs `adopted` (U14, 4b R24). Clients read display states, so they decode them. |
| V5 | **Folds are saved as one `meta` value**, `tree.folded`: the sorted JSON list of folded tree paths (a real group, a project's node or `Loose`). `group.fold {path, collapsed}` adds or removes one path in one `Change::Meta`; it never writes `groups`. The list is published as `brain.folded`, so `Change::Meta` touches `Topic::Brain`. `group.rename` moves the folded paths at and under the old name to the new one, and `group.rm` drops them, in the same commit. `groups.collapsed` stays unused. No activity row; the reply text is empty. Project expansion stays in the panel. | A fold should outlive a panel restart (slice 9 re-execs panels) and agree across tree panels. Tree nodes also come from project names and `Loose`; writing `groups` rows for them would make them real, permanent groups (shown empty, offered as `n`'s group, left behind by a project rename). One value covers every kind of node. Folding isn't news for moa's activity feed. |
| V6 | **`Ctrl-a $` opens a terminal "here"**, by the key's pane:<br>• a panel's pane: its `ui.here`;<br>• a bound agent's pane: its session's `cwd` (none: asked as any other pane);<br>• any other pane: one `display-message -p -t %N '#{pane_current_path}'` on the control connection (`Asked::Here`);<br>• nothing, an error answer (the pane went), a relative path, or a folder `Deps::is_dir` denies: the home folder.<br>Then 4b's terminal (R16), told by notice instead of a reply. If the link goes before an answer, nothing opens and one fixed log line says so (as `term_opened` does). A panel with no `here` uses home until 5b adds the agent spot's folder. | The spike's `here_dir`. 4b (R16) handed `$` and `ui.here` to slice 5. The query is a command on the open connection, not a launch. |
| V7 | **Tree keys** (spec §6.13, spike `tree_key`), each sending one op:<br>• Enter/click: a session → `session.focus`; a group node → `group.fold`;<br>• `n`: footer prompts `new session in <group \| top level> · folder:`, `title:`, `account (acct-a / acct-b):`, then `session.open {path, account, title?, group?}`. Prefill: the newest session in the selected group or below it (folder, account), else the selected session's, else the brain's first folder and `open_account`, else `~/`. The folder must start with `~` or `/` (`a folder starts with ~ or /`); it must exist (`no folder <path>`); both checked right after the folder prompt. Another brain's folder is refused by the core after the prompts (`that folder belongs to the <b> brain; open it there`, as for `moactl open`). An empty account answer cancels;<br>• `N` on a session row: V21's (the spike's tree passed `N` through to it);<br>• `+` `new group:` (prefill `<group>/`, empty at the top level) → `group.add`; `m` `move <title> to group:` → `session.set {group}`; `r` → `session.set {title}` or `group.rename {old, new = parent/new}`; `D` (a group) → `group.rm`; `a` → `session.archive`;<br>• `x` → `close <title>? y/N` → `session.close`;<br>• **`R`** (an adopted session only) → `restart <title> under moa? y/N` → `session.restart`.<br>Esc in a prompt says `cancelled`. Every op names a session by its full moa id. | Parity, with "folder" for "path". 4b R24: "the one-key action on the tree's row comes with slice 5"; it closes a tab, hence the confirm. The early folder checks are Call 6; the core still checks everything again. |
| V8 | **Keys act on identity.** After every rebuild the selection moves to the line with the same `Item`; when that item is gone, to the nearest selectable line. A y/N confirm stores its op when it is asked, so `y` sends exactly that. | The spike kept a row index, so a patch between looking and pressing could aim `x` at another session. |
| V9 | **Text is cleaned and measured by display width.** Every string from data goes through `text::clean`: control characters (including `\n`, `\t`, ESC) become a space; U+FE0E and U+FE0F are dropped; a grapheme holding a ZWJ (U+200D), a skin-tone modifier (U+1F3FB–1F3FF), or a tag character (U+E0020–E007F) becomes its first code point; a regional indicator becomes its ASCII capital (a flag `🇮🇱` reads `IL`), since terminals disagree on a lone indicator's width. Widths then use `unicode-width` over grapheme clusters, which tmux agrees with for every cleaned grapheme; a cut never splits a wide character (it pads with a space). | Lesson 18, spec "display width by grapheme" and "a tested glyph set". Titles, prompts and why texts come from agents' payloads: an ESC must never reach the terminal. tmux counts per code point, so a `⚠️` or `👨‍👩‍👧` would take a different number of cells there than ratatui assumed, shifting the rest of the line and breaking the frame. |
| V10 | **When a panel wakes:** a push, an input (a mouse move redraws only when the hovered row changes, V11), a resize; a 100 ms animation tick only while `Screen.animate` (a *visible* line animates: spinner, ⚠/! blink, ✦ flash; or the picture's brand spins, V21); and one deadline, the earliest moment a visible text changes (an age label's next step, local midnight, a flash's end). | Spec §6.13 ("redraw only on daemon pushes, input, resize, or the animation interval while something animated is visible"), U5. Without the deadline "5m" stays "5m" and "today" outlives midnight; deadlines are allowed ("timers fire only at exact deadlines"). |
| V11 | **Mouse, with a hover mark (the user's call, 2026-11-18):** the panel enables `?1003h` (every mouse event, motion included) and `?1006h` (SGR), focus reports `?1004h`, and bracketed paste `?2004h`.<br>• **Hover:** the selectable row under the pointer gets a soft mark: background `surface` (`#3b4252`), its own text colours kept. The selection bar (`bright` on `sel`) wins on the selected row. Hover is kept by `Item`, like the selection.<br>• **A motion event redraws only when the hovered row changes** (another row, or none); motion along one row, over headings after a heading, or outside the body does nothing.<br>• **A left click acts on the row under the pointer** (the hovered one): it selects it and acts, as Enter. Clicks on headings and outside the body are ignored.<br>• **The mark clears** on any key press, on focus-out (`?1004h`; moa's `moa.tmux` already sets `focus-events on`, so nothing new there), and when the hovered item leaves the list. Terminals send no "pointer left" event, so a mark can stay on a panel the pointer left until one of those happens.<br>• The wheel moves the view ±3 and drags the selection into it; the hover is then the row now under the last pointer position.<br>`moa.tmux` sets `mouse on` unless `moa.toml` has `[ui] mouse = false` (`UiConfig { mouse: bool }`, default true; anything but a boolean is `mouse in [ui] must be true or false`). | Spec §6.13 Mouse, lessons 20–21. The user found the spike's click on an unfocused panel surprising: it acted at once on whatever was under the pointer. Showing what a click will act on keeps herdr's single click without surprises. Focus-follows-pointer was rejected: it would pull the keyboard from a Claude tab whenever the mouse crossed a panel, and couldn't follow onto Claude tabs. Motion events are input, not polling: they come only while the mouse moves over a panel, and redraw only on a row change. Lesson 11 (focus reports are unreliable for moved panes) is why focus-out is only one of three ways the mark clears. tmux forwards mouse events only to a pane that asked, and only with `mouse on`; without it the wheel reaches agents as arrow keys. The spike left the mouse to the user's config, and moa's file loads after the user's, hence the opt-out (Call 2). |
| V22 | **Fallback, only if Task 2's probe (Step 1b), or Task 14's hover test, finds that tmux 3.4 or 3.8 doesn't forward motion or focus events to a pane that asked:** the first click on a pane that isn't the active one only selects it. `moa.tmux` binds `bind -n MouseDown1Pane if -Ft= '#{pane_active}' { select-pane -t=; send -M } { select-pane -t= }`, written only when the mouse setting is on (with `[ui] mouse = false` nothing is bound). It replaces any `MouseDown1Pane` binding from the user's own tmux config, since moa's file loads after it; the PR and the try-it say so. The panel drops `?1003h`/`?1004h` back to `?1000h`, and the hover mark goes.<br>**Tests under V22:** `moa_tmux_binds_the_first_click_to_focus` (Task 13, `conf.rs`: the line with mouse on, absent with it off) and `a_first_click_on_another_pane_only_selects_it` (Task 14: a click on the unfocused panel makes it active and sends no op; a second click acts). **Dropped under V22:** Task 6's `tree_hovered`, `motion_along_one_row_doesnt_redraw`, `a_key_clears_the_hover_mark`, `focus_out_clears_the_hover_mark`, `a_hovered_item_that_leaves_the_list_clears_the_mark`, `the_selection_wins_over_hover`; Task 11's `a_hovered_row_has_the_surface_background`; Task 14's `hover_marks_the_row_under_the_pointer_and_focus_out_clears_it`. `a_click_acts_on_the_row_under_the_pointer` stays, without its `Move`. **Changed under V22:** Task 13's `INIT` and `the_panel_asks_for_the_mouse_focus_and_paste` (`?1000h`, `?1006h`, `?2004h`; no `?1003h`, `?1004h`); the loop's `FocusLost` → `Panel::blur` wiring and `mouse_of`'s `Move` go, with `Panel::blur` and `Mouse::Move`. The implementer switches by this rule and says so in the PR. | The user's third choice. It works without motion or focus reports, since tmux decides it from `pane_active`. It applies to every pane, which matches how most apps treat a first click. |
| V12 | **Colours:** spec §6.14's tokens. Inside tmux (`TMUX` set) or with `COLORTERM` `truecolor`/`24bit`: 24-bit. Otherwise the nearest xterm-256 index by the spike's `xterm256` (cube or grey ramp). The frame is drawn in the default colour (`Color::Reset`) and body text in `text`. Tone map: head and group blue bold; dim; warn yellow; ask orange; ok green; fail red; work blue; quiet cyan; brain magenta; you yellow; p0 red bold; late `dark` on red bold; today `dark` on yellow bold; soon blue; selection `bright` on `sel`. The theme file and its hot reload: slice 9. | Lessons 11–12 (tmux colours the frame per pane), 16–17. tmux converts 24-bit for clients without RGB, so a panel in tmux never needs the fallback. Roadmap slice 9 item 3 lists the theme for hot reload. |
| V13 | **The connection** (`moa_client::Live`): a task owns the connection; `send(op)` returns a tag at once; replies come back by tag, pushes in order.<br>• **Every connect and reconnect** first checks `hello().matched` (a mismatched daemon allows only `hello`, `status`, `upgrade`, `shutdown`), then calls `live(TOPICS, [UiPanel { pane, view: current }, UiHere { folder: current }])`, takes its new key from the reply, and replaces `Data` from the snapshot.<br>• A loss: reconnect after 0.25 s, doubling to 8 s, about 60 s in all, then wait for a key.<br>• The first connect autostarts the daemon like `moactl`; reconnects never do.<br>• A build mismatch: the panel stays, says `this panel is from another build of moa; close it and run moa panel again`, and sends nothing. | Spec §6.2 (`moa-client`: reconnect + resync). The registry lives in the daemon's memory, by connection, and connection numbers restart with the daemon, so a reconnected panel must register again and must never read an old key. A focus that waits for a tab replies late, and the panel must keep drawing. A bounded backoff is the control client's precedent (slice 3), not a poll. Restarting a daemon other panels use is 2b's D1 policy, carried to slice 9. |
| V14 | **A panel claims its pane** (`TMUX_PANE`) only when the socket in `TMUX` is this brain's (`Paths::tmux_socket`, a name resolved under `TMUX_TMPDIR` or `/tmp` and the uid), comparing canonical paths. | Lesson 4: `TMUX_PANE` from another server names someone else's pane. Taking `TmuxSocket` now keeps the signature when real brains (named sockets) open up in slice 10. |
| V15 | **`moa panel [VIEW] [--brain B]`:** the brain is `--brain`, else `MOA_BRAIN`, else where the folder routes (`ctl::pick_brain` with `writes = false`: a panel isn't an agent's call); outside the sandbox it refuses like every verb until the switch-over. `panel` is a reserved brain name. Ctrl-c quits any panel; `q` only the picture. A view this build doesn't draw shows the keys' wording: `the console isn't in this build yet`, `finding a session isn't in this build yet` (picker), `the agent spot isn't in this build yet` (empty-slot), `help isn't in this build yet`, `setup isn't in this build yet`. | The spike's `--panel NAME`. Spec §6.13's view list. A dead panel is 5b's Repair. |
| V16 | **v1's states in the views:** the status panel groups by `DisplayState` order (waiting, question, failed, done, working, quiet, idle), headed `needs you`, `asks you`, `failed`, `done`, `working`, `quiet`, `idle`. Failed is ✗ in `fail` (red), quiet ◌ in `quiet` (cyan), elsewhere ◌ dim. The tree's "working" count counts `Working` only. Account names lose a leading `claude-` before their 6-column cut, as in the spike. | Spec §7: "status view: adds failed and quiet". Red is the palette's error colour, cyan its secondary accent. Spike-imported account names start with `claude-`. |
| V17 | **A list with nothing to select** (activity) scrolls with j/k, PgUp/PgDn, g/G. | Constraint 2, keyboard-first; the spike needed the wheel there. |
| V18 | **Rows the panel can't read** (a newer daemon, a bad value) are skipped and counted; the footer says `<n> rows from moa couldn't be shown` until the count is 0. No view panics for any data or size (proptest). | Spec §7: "degrade the entry, not the frame". |
| V19 | **Snapshots:** in `moa-view`, one model snapshot per view at 80 columns, one line per `Line` (tone, `*` for bold, text, chip in `⟨⟩`, item kind). In `moa-tui`, `TestBackend` text snapshots of every view at widths 40, 60, 80, 120 and 200, 16 rows high. Colours are checked by cell assertions, not snapshots. The fixture is `moa_view::fixture::lantern` (Task 4). | Roadmap 5.1 and 5.3. Text snapshots read well in a PR; style dumps don't. |
| V20 | **Plain words.** Prompts, hints and messages say "tab" and "folder"; account names appear only as the brain's own list, never written into the code. The hints are the spike's, per view (Task 5). | Lesson 35, Global Constraints. |
| V21 | **Session-row keys in every view** (spike `main`): on a session row outside the tree, `a` archives and `x` asks `close <title>? y/N` (not in the big picture, where `x` only drops tasks); on any row with a project (session, task, project), `n` opens a session in that project's folder with the default account and `N` asks the account first, with V7's folder checks; a row with none says `this row has no project`. In the tree its own `n` wins, and `N` on a session row is this one (on a group, nothing), as in the spike, whose tree passed `N` through. The picture's brand `moa <spinner>` spins while any session is working or waiting, or an activity row is under 4 s old, as the spike's did. | Spike parity (`picture.py` 921–948, and its picture hint). The brand is always on screen in the picture, so ticking for it is "something animated is visible". |

## Parity rows this part delivers (spec §7)

Panel common chrome, rows, keys, mouse, scroll model, footer editor (2.3); `@moa_here` as
`ui.here` (2.3); animation (2.3), with the interval only while visible; the tree, plan, status,
today, projects, activity and picture views and their keys (2.3); robustness for panels. The
layout rows (2.2) are 5b's; the picker (2.7) and help (2.6) are 5c's.

## Global Constraints

The roadmap's and every earlier slice plan's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …` (every `cargo` command in this plan,
  `cargo insta review` and `cargo tree` included); tests with `cargo nextest run`.
- **`moa-view` stays free of IO crates:** no `tokio`, `rusqlite`, `libc`, `crossterm` or
  `ratatui` in `cargo tree -p moa-view -e normal --prefix none --target all` (a CI step, Task 4).
- **Zero `Spawner` launches** for a panel's keys, clicks, scrolls, prompts and ops, and for
  `Ctrl-a $`. A panel never runs `tmux`; `ui.here` replaces the spike's option write. The one
  launch a panel may cause is its **first** connect starting moa's daemon when none runs, through
  the counted `Spawner`, as `moactl` does; later reconnects never start it (V13).
- **No polling** (V10, V13): a panel wakes only for the reasons in V10, plus the bounded
  reconnect.
- **Colours exactly spec §6.14's** (V12): 24-bit inside tmux and with `COLORTERM`
  `truecolor`/`24bit`; the nearest 256-colour values only outside tmux without it. Nothing
  changes `TERM` (lesson 16).
- **Tests:** only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too; never a tmux
  server named `moa` or `moa-work`; never a real brain; `fake-claude` is the only agent; every
  wait through `wait_for` with a 10 s limit, never a sleep.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`, `~/private`.

## Review Focus

1. **The list changes between looking and pressing, or a click lands on a panel you weren't
   in** (a patch arrives after you selected a row, or while a y/N waits). Expected: the key acts
   on the row you saw; `y` closes the session the question named; a click acts on the row the
   hover mark showed. Pinned by `the_selection_follows_its_item_when_rows_change` (Task 5),
   `x_closes_the_session_it_named_even_after_the_list_moved` and
   `a_click_acts_on_the_row_under_the_pointer` (Task 6).
2. **Text from agents:** ESC sequences, newlines and tabs in titles, prompts and why texts; wide
   glyphs and emoji sequences (💤, CJK, `⚠️`, `👍🏽`, `👨‍👩‍👧`) at a cut. Expected: no control
   character reaches the terminal, columns stay aligned, no wide glyph is split. Pinned by
   `clean_replaces_control_characters`, `clean_simplifies_emoji_sequences`,
   `fit_never_splits_a_wide_char` (Task 4), the proptest
   `views_never_panic_or_leak_control_characters` (Task 10) and the width snapshots (Task 11).
3. **Tiny and resized panes** (1–12 columns, 0–4 rows, a resize mid-prompt). Expected: no panic,
   nothing drawn outside the pane, the prompt survives. Pinned by
   `tiny_sizes_draw_without_panicking` (Task 11) and the proptest's sizes 0..250 × 0..60 (Task 10).
4. **moa goes away, restarts, or is another build while a panel is open.** Expected: the panel
   says so in plain words, keeps its last picture, reconnects within the bounded backoff,
   registers again with its view and here, never restarts moa itself, never spins. Pinned by
   `live_says_closed_when_the_daemon_stops` (Task 12), `reconnect_gives_up_after_about_a_minute`,
   `first_ops_register_and_restore_here` and `a_different_build_is_said_not_restarted` (Task 13),
   and `a_panel_comes_back_after_moa_restarts` (Task 14).
5. **Time passes with nothing happening.** Expected: ages, "today", due chips and the ✦ flash
   change at the right moment, and nothing animates when nothing moving is on screen. Pinned by
   `text_changes_at_exact_deadlines` and `nothing_animates_off_screen` (Task 10).

---

## Task 1: panels in the core

**Files:**
- Create: `crates/moa-core/src/panel.rs` (`View`, `PanelReg`)
- Modify: `crates/moa-core/src/{lib.rs, op.rs, state.rs, core.rs, publish.rs, live.rs, change.rs}`
- Modify: `crates/moa-core/src/ops/mod.rs` (`Op::UiPanel` and `Op::UiHere` join the arm of ops
  `handle` answers itself: `moa can't answer that here`)
- Modify: `crates/moa-proto/tests/snapshots/` (the protocol schema snapshot)
- Test: `crates/moa-core/tests/panels.rs` (new; a `send_on(b, st, conn, req, op)` helper in
  `tests/common/mod.rs`), `crates/moa-core/tests/skeleton.rs` (samples for `UiPanel` and
  `UiHere`, neither in `WRITES`), `crates/moa-proto/tests/wire.rs`

**Interfaces:**
- Produces, in `moa_core::panel`:
  - `pub enum View { Console, Tree, Plan, Status, Today, Activity, Projects, Picture, Picker, EmptySlot, Help, Setup }`
    with `ALL`, `word() -> &'static str` (`"empty-slot"` for `EmptySlot`, lowercase otherwise),
    `FromStr`, `Serialize`/`Deserialize` as the word, `JsonSchema` under `schema`;
  - `pub fn label(self) -> &'static str`: the spike's `LABELS` (`◆ moa`, `▤ sessions`,
    `◷ big picture`, `☀ today`, `≡ activity`, `▦ projects`, `◉ status`, `◫ picture`,
    `○ agent spot`), the word for the rest;
  - `pub fn framed(self) -> bool`: the spike's `FRAMED` (console, tree, plan, today, activity,
    projects, status);
  - `pub struct PanelReg { pub pane: Option<PaneId>, pub view: View, pub here: Option<String> }`.
- Produces, in `moa_core::op`: `Op::UiPanel { pane: Option<String>, view: Option<View> }`
  (`ui.panel`) and `Op::UiHere { folder: Option<String> }` (`ui.here`), neither a write, both in
  `METHODS`. `ConnId` derives `PartialOrd, Ord`.
- Produces, in `moa_core::publish`: `Topic::Brain`, `Topic::Panels` (in `ALL`);
  `pub struct BrainRow { pub name: String, pub folders: Vec<String>, pub accounts: Vec<String>, pub open_account: Option<String>, pub folded: Vec<String> }`,
  `pub struct PanelRow { pub pane: Option<String>, pub view: View }` (all `Serialize +
  Deserialize`); `folded` is empty until Task 3. Session rows gain `"adopted"`, so
  `Change::PaneBinding` and `Change::DeletePaneBinding` now also touch `Topic::Sessions`.
- Produces, on `State`: `pub(crate) panels: BTreeMap<ConnId, PanelReg>`,
  `pub(crate) pane_views: BTreeMap<PaneId, View>`, `pub fn here_of(&self, pane: PaneId) -> Option<&str>`.
- `DisplayState` derives `Deserialize` (lowercase).

`ui.panel` and `ui.here` are handled in `core.rs` (they need the `ConnId`), not `ops::run`. Their
effects: a `Publish` of the `panels` topic when it changed, then the `Reply`; never a `Persist`.
Refusals: `a panel's tab id looks like %5` for a bad pane; `say which panel this is first
(ui.panel)` for `ui.here` before `ui.panel`. `ClientGone` removes the connection's panel and
publishes `panels`.

- [ ] **Step 1: Write the failing tests** in `panels.rs` (every input through `handle_checked`):
  - `a_panel_registers_and_is_published`: `UiPanel { pane: "%3", view: Tree }` on conn 7 → one
    `Publish` whose `panels` topic is `{"c7": {"key": "c7", "pane": "%3", "view": "tree"}}`, then
    `Reply` `data == {"key": "c7", "view": "tree"}`; no `Persist`.
  - `a_panel_without_a_view_gets_its_panes_last_one_else_picture`: conn 7 registers `%3` as plan,
    `ClientGone(7)`, conn 8 registers `%3` with no view → `plan`; `%4` with no view → `picture`.
  - `a_second_panel_on_a_pane_replaces_the_first`: conns 7 and 8 both claim `%3` → only `c8`.
  - `a_closed_connection_drops_its_panel`: `ClientGone(7)` → `panels` publishes without `c7`.
  - `ui_here_is_kept_per_panel`: `UiHere { folder: "/r/work/a" }` on conn 7 →
    `here_of(%3) == Some("/r/work/a")`, no `Publish`; on an unregistered conn → the refusal.
  - `a_bad_pane_is_refused`: pane `"5"` → `a panel's tab id looks like %5`.
  - `brain_topic_lists_folders_and_accounts`: brain `default` (folders `~/private`); accounts
    `acct-a` and `acct-b` (launchers), `acct-c` (no launcher), `open_account = "acct-a"` →
    `folders == ["<home>/private"]`, `accounts == ["acct-a", "acct-b"]`.
  - `session_rows_say_adopted`: an untracked binding → `"adopted": true`; tracked or none →
    false; deleting the binding publishes the row again with `false`.
  
  In `wire.rs`: `ui_panel_decodes` (`{"view":"tree","pane":"%3"}`) and
  `an_unknown_view_is_invalid_params` (`{"view":"nope"}` → `code::INVALID_PARAMS`). In `live.rs`'s
  unit tests: `display_state_words_round_trip` over `DisplayState::ALL`.
- [ ] **Step 2: Run them to see them fail:** `nix develop --command cargo nextest run -p moa-core --test panels` → compile errors (no `View`, no `UiPanel`).
- [ ] **Step 3: Implement** the interfaces above, and update the schema snapshot
  (`nix develop --command cargo insta review` on `moa-proto`'s `protocol_schema`: only the new
  methods, topics and `View` change).
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit:** `v1: panels register with the daemon; brain and panels topics`.

## Task 2: the `$` key

**Files:**
- Modify: `crates/moa-core/src/ops/launch.rs` (`term` takes a `Tell`; `term_key`),
  `crates/moa-core/src/tmux/link.rs` (`Asked::Here`), `crates/moa-core/src/core.rs` (the key and
  the answer)
- Modify: `crates/moa/tests/tmux_e2e.rs` (`keys_and_menu_reach_the_daemon_with_zero_launches`
  skips `KeyAction::Terminal`: it opens a tab now; Task 14 tests it; and Step 1b's probe)
- Test: `crates/moa-core/tests/panels.rs`, `crates/moa-core/tests/tmux_keys.rs`

**Interfaces:**
- Produces: `pub(crate) enum Tell { Reply(ConnId, ReqId), Notice(ClientName) }`; `term(state,
  tell: Tell, path, title)` and `term_opened` answer through it (a notice uses
  `link::notice(client, text)`). For a key, the success is a notice too, with the same text
  `term_opened` replies today and the spike's `moamenu` showed for 4 s:
  `opened a terminal in <folder, home as ~> (tab <name>)`.
- Produces: `pub(crate) fn term_key(state: &mut State, pane: Option<PaneId>, client: ClientName) -> Vec<Effect>` (V6).
  `Asked::Here { client: ClientName }`, whose one command is
  `display-message -p -t %N '#{pane_current_path}'`; its answer runs `term` with the folder.

- [ ] **Step 1: Write the failing tests** in `panels.rs`, with the link up (`link_up`) and conn 7
  a panel on `%3`:
  - `the_terminal_key_opens_in_the_panels_here`: `UiHere "/r/work/a"`, then the message
    `moa:key terminal %3 $1 /dev/ttys1` → one tokened `Tmux` whose command is
    `new-window -d -P -F … -n '⌨ a' -c /r/work/a -t =moa:`; after its `Replies`, the two
    `set-option`s, the focus commands and the notice to `/dev/ttys1` reading
    `opened a terminal in /r/work/a (tab ⌨ a)`; no `Reply`, and no effect but `Tmux`.
  - `the_terminal_key_on_an_agent_tab_uses_its_folder`: `%4` bound to a session with cwd
    `/r/work/b` → `-c /r/work/b`.
  - `the_terminal_key_on_a_shell_asks_tmux_where_it_is`: `%5` unknown → `display-message -p -t
    %5 '#{pane_current_path}'`; `Replies [Ok(["/r/work/c"])]` → `new-window … -c /r/work/c`;
    `Replies [Ok([""])]` → `-c <home>`; `Replies [Err("can't find pane")]` → `-c <home>`.
  - `the_terminal_key_avoids_a_missing_folder`: `Deps::is_dir` false → `-c <home>`.
  - `a_terminal_key_whose_answer_never_comes_opens_nothing`: the link goes `Down(Lost)` after the
    `display-message` → no `new-window`, and one info log with a fixed sentence.
  - In `tmux_keys.rs`: `the_terminal_key_is_no_longer_a_notice` (no `not_yet` text for it), and
    the existing `every_key_gets_its_notice` skips `KeyAction::Terminal` (it loops
    `KeyAction::ALL`).
- [ ] **Step 1b: Probe tmux for V11, before any hover code exists.** In `tmux_e2e.rs`,
  `tmux_forwards_motion_and_focus_to_a_pane_that_asked`: a sandbox with `set -g mouse on` (sent by
  the test) and an attached `Terminal`; a tab split in two whose first pane (the probe) runs
  `printf '\033[?1003h\033[?1006h\033[?1004h'; stty raw -echo; dd bs=1 of=<root>/seen`, so its
  input is read raw, byte by byte, with nothing held for a newline. Steps:
  1. the probe active, then `select-pane` to the other half → `seen` holds `ESC [ O` (focus-out);
  2. with the other half active, the `Terminal`'s output since its mark holds `\e[?1003h` (tmux
     turned all-motion reporting on for the terminal, because the visible probe asked);
  3. an SGR motion on the `Terminal` at the probe's own position (x = `#{pane_left}`+3,
     y = `#{pane_top}`+2, from `display-message -p -t <probe>`) → `seen` holds `[<35;` while the
     probe is still not the active pane;
  4. an SGR left press and release there → `seen` holds `[<0;`, and the probe is now active.
  Run it on tmux 3.8 and 3.4. If it fails
  on either, V22 replaces V11 for the rest of this plan: record that in the progress ledger and
  the PR body. It needs no product code, so it passes or fails at once.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them. The key is caught in `core.rs`'s `Input::Tmux` arm before the
  link's notice, for `KeyAction::Terminal` only.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`, and the tmux
  3.4 pass on `moa` (which also runs Step 1b's probe on 3.4).
- [ ] **Step 5: Commit:** `v1: Ctrl-a $ opens a terminal where you are`.

## Task 3: saved folds

**Files:**
- Modify: `crates/moa-core/src/ops/groups.rs` (`fold`), `crates/moa-core/src/op.rs`
  (`Op::GroupFold`), `crates/moa-core/src/ops/mod.rs`, `crates/moa-core/src/change.rs`
  (`Meta` → `Topic::Brain`), `crates/moa-core/src/publish.rs` (`brain.folded`, `FOLDED_KEY`,
  `folded`)
- Modify: `crates/moa-proto/tests/snapshots/` (schema)
- Test: `crates/moa-core/tests/panels.rs`, `crates/moa-core/tests/skeleton.rs` (a sample, and
  `group.fold` in `WRITES`), `crates/moa-core/src/state/tests.rs` (`changes_name_their_topics`:
  `Meta` → `[Brain]`)

**Interfaces:**
- Produces: `Op::GroupFold { path: String, collapsed: bool }` (`group.fold`, a write, through
  `ops::run`): `pub(crate) fn fold(state: &mut State, tx: &mut Tx, path: &str, collapsed: bool) -> Result<Done, OpError>`;
  `Done { text: "", data: {"path", "collapsed"} }`; an empty path → `a group needs a name`.
- Produces, in `moa_core::publish` (public, so tests and `ops/groups.rs` reach them):
  `pub const FOLDED_KEY: &str = "tree.folded"` and `pub fn folded(state: &State) -> Vec<String>` (a value that doesn't parse reads as empty, and
  the next fold rewrites it).

- [ ] **Step 1: Write the failing tests** in `panels.rs`:
  - `a_fold_is_one_meta_value`: `GroupFold { "Private/Lantern", true }` →
    `Persist [Meta { key: "tree.folded", value: Some("[\"Private/Lantern\"]") }]` only (no
    `Group`, `Activity` or `Event`), and the `brain` row's `folded` lists it; `false` → `"[]"`.
  - `folding_loose_leaves_groups_unchanged`: `GroupFold { "Loose", true }` → no `Change::Group`,
    and `groups` still empty.
  - `folds_are_sorted_and_kept_once`: folding `B`, `A`, `A` → `["A","B"]`.
  - `a_bad_folded_value_reads_as_empty`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them, and the schema snapshot.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`.
- [ ] **Step 5: Commit:** `v1: tree folds are saved as one list`.

**PR p1 ends here.** Verify, review and open the PR. No try-it yet; the body says `Ctrl-a $` now
opens a terminal in the sandbox's home.

## Task 4: `moa-view`: the data mirror, text, the fixture

**Files:**
- Create: `crates/moa-view/{Cargo.toml, src/lib.rs, src/data.rs, src/text.rs, src/ctx.rs, src/fixture.rs}`
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `unicode-width`, `unicode-segmentation`,
  `textwrap` with `unicode-width`, `ratatui = "0.30"`, `crossterm = { version = "0.29", features = ["event-stream"] }`)
- Modify: `.github/workflows/ci.yml` (a `moa-view stays free of IO crates` step, grep
  `^(tokio|rusqlite|libc|crossterm|ratatui) `)
- Test: `crates/moa-view/tests/{data.rs, text.rs}`

**Interfaces:**
- `moa-view` depends on `moa-core`, `serde`, `serde_json`, `jiff`, `uuid`, `unicode-width`,
  `unicode-segmentation`, `textwrap`; feature `fixture` exposes `fixture`. Its dev-dependencies:
  `insta`, `proptest`, and itself with the fixture
  (`moa-view = { path = ".", features = ["fixture"] }`), so its own tests reach `lantern()`.
- `moa_view::data`:
  - `pub struct SessionRow { #[serde(flatten)] pub session: Session, pub handle: String, pub project: Option<String>, pub display_state: DisplayState, pub why: String, #[serde(default)] pub adopted: bool }`;
  - `pub struct Data { pub sessions: BTreeMap<SessionId, SessionRow>, pub groups: BTreeMap<String, Group>, pub projects: BTreeMap<ProjectId, Project>, pub tasks: BTreeMap<TaskId, Task>, pub suggestions: BTreeMap<SuggestionId, Suggestion>, pub activity: BTreeMap<ActivityId, ActivityRow>, pub brain: Option<BrainRow>, pub panels: BTreeMap<String, PanelRow>, /* private: unreadable keys */ }`;
  - `impl Data { pub fn replace(&mut self, topic: Topic, rows: Vec<Value>); pub fn upsert(&mut self, topic: Topic, rows: Vec<Value>); pub fn delete(&mut self, topic: Topic, keys: &[String]); pub fn unreadable(&self) -> usize }`
    (a row that doesn't decode is left out and its key counted until a later row with that key
    decodes, or it is deleted);
  - `pub const TOPICS: &[Topic]`: sessions, groups, projects, tasks, suggestions, activity,
    brain, panels.
- `moa_view::ctx`:
  `pub struct Ctx { pub now: UtcMs, pub today: Date, pub tz: TimeZone, pub tick: u64, pub home: PathBuf, pub is_dir: fn(&Path) -> bool }`
  and `Ctx::at(now, tz, tick, home, is_dir)`. `tick` is monotonic milliseconds / 100.
- `moa_view::text`:
  - `clean(&str) -> String` (V9); `width(&str) -> usize`; `cut(&str, w) -> String`;
    `fit(&str, w) -> String` (cut, then pad to exactly `w`); `fit_right(&str, w) -> String`;
    `wrap(&str, w) -> Vec<String>` (never empty; long words broken);
  - `ago(from: Option<UtcMs>, now: UtcMs) -> String`: `""` for none, `now` under 60 s (and the
    future), then `{m}m`, `{h}h`, `{d}d` (the spike's picture `ago`);
    `next_ago_change(from: UtcMs, now: UtcMs) -> UtcMs`: the first moment `ago` reads differently;
  - `days_left(due: Date, today: Date) -> i64`; `when(due: Date, today: Date) -> String`:
    `{n}d overdue`, `today`, `tomorrow`, the weekday (`Tue`) under 7 days, else `3 Dec`;
  - `short_path(path: &str, home: &Path) -> String`: home as `~`, and the last two components
    when there are more than three;
  - `short_account(&str) -> &str`: without a leading `claude-` (V16);
  - `pub const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]` (the spike's
    spinner; frame `SPIN[tick % 10]`);
  - `pub const CHROME: &[(&str, usize)]`: every glyph moa draws itself with its width (V9).
- `moa_view::fixture::lantern() -> (Data, Ctx)`, built by feeding JSON rows through
  `Data::upsert`, so it also tests decoding. Every later test relies on it, so it is exactly this.
  `Ctx`: now Fri 13 Nov 2026 10:00 in `EST5EDT,M3.2.0,M11.1.0`, `tick` 3, home `/h`, `is_dir`
  true. Project folders are `/h/private/lantern`, `/h/private/kite`, `/h/private/harbor`.
  - **Sessions**, `last_seen_at` newest first in this order (1 is a minute ago, each next one an
    hour older), cwd its project's folder (else `/h/private`), account `acct-a` unless said:
    1. `Fix login`, group `Private/Lantern`, project Lantern, waiting (why `Bash: cargo test ⚠️`);
    2. `Rust scaffold`, `Private/Lantern`, Lantern, working (prompt `split the store`);
    3. `Release notes`, `Private/Kite`, Kite, question (why `Ship it today?`);
    4. `設計レビュー`, `Private/Kite`, Kite, done (turn ended after it was seen);
    5. `Dev server 👍🏽`, `Private/Harbor`, Harbor, idle;
    6. `Migrate DB`, `Work/Harbor`, Harbor, failed (why `API error`), account `acct-b`;
    7. `Import the whole archive of old notes into the new format`, `Private/Lantern`, quiet;
    8. `Hand started`, `Private/Harbor`, idle, `adopted: true`;
    9. `Old spike`, no group, no project, asleep (💤);
    10. `Closed thing 👨‍👩‍👧`, `Private/Kite`, ended;
    11. `Gone`, archived.
  - **Groups rows:** `Private/Ideas` (empty).
  - **Projects:** Lantern (active, last activity today), Kite (active, today), Harbor (active, 10
    Nov), Beacon (idea, no path, no activity).
  - **Tasks:** Lantern: `Fix the login loop` P0, you, due 12 Nov, today's; `Write the README`
    agent, linked to session 2, due 13 Nov; `Wait for review` waiting, blocked on `design team`,
    due 14 Nov. Kite: `Pick a cover` P1, you, due 17 Nov; `Order copies` who other, due 3 Dec;
    `Write the back-cover text for both the paperback and the e-book editions` you, no due;
    `Proofread chapter 1` done at 09:00 today. No project: `Renew the domain` who other, no due.
  - **Suggestions:** #1 open `Archive 3 ended sessions?`, #2 later `Set Kite's due date`.
  - **Activity:** eight rows over the last hour, by brain, you and session; the newest is 2 s old.
  - **Brain:** `default`, folders `[/h/private]`, accounts `acct-a`, `acct-b`, open `acct-a`,
    folded `[]`.

- [ ] **Step 1: Write the failing tests:**
  - `data.rs`:
    - `data_follows_a_snapshot_then_patches`: `replace` sessions with two rows, `upsert` one
      changed and one new, `delete` one → the expected ids and titles.
    - `an_unreadable_row_is_skipped_and_counted`: a session row with `"lifecycle": "zombie"` →
      left out, `unreadable() == 1`; a good row with that key → 0.
    - `the_fixture_decodes_whole`: `unreadable() == 0`, 11 sessions (one archived), 4 projects,
      8 tasks.
  - `text.rs`:
    - `clean_replaces_control_characters`: `"a\x1b[31mb\nc\td\u{7f}"` → `"a [31mb c d "`.
    - `clean_simplifies_emoji_sequences`: `"⚠️ ok"` → `"⚠ ok"`; `"👍🏽"` → `"👍"`;
      `"👨‍👩‍👧"` → `"👨"`; `"🇮🇱"` → `"IL"`; `"é"` written with a combining accent stays.
    - `the_chrome_glyph_set_has_one_width_each`: every `CHROME` glyph's `width` is its listed
      width (1 for `⚠ ! ? ✓ ✗ ◐ ◌ ● ○ ★ ▸ ▾ ◷ ✦ ⧗ ◆ ▤ ☀ ≡ ▦ ◉ ◫ ⌨ ⏎ ↵ · … ─ │ ├ └ ╭ ╮ ╰ ╯` and
      every `SPIN` frame; 2 for `💤`).
    - `fit_cuts_by_display_width`: `fit("💤 x", 5) == "💤 x "` (4 cells, padded to 5),
      `fit("💤 x", 3) == "💤 "` (the `x` cut), `fit("設計レビュー", 7) == "設計レ "`,
      `fit(&clean("👍🏽 ok"), 6) == "👍 ok "`, and `width(&fit(s, w)) == w` for each.
    - `fit_never_splits_a_wide_char`: proptest over cleaned strings of CJK, emoji and ASCII and
      widths 0..40: `width(&cut(s, w)) <= w`, and `cut(s, w)` is a prefix of `s` at a grapheme
      boundary.
    - `ago_words_and_their_next_change`: 30 s → `now`, next at +30 s; 5 min 10 s → `5m`, next at
      6 min; 3 h → `3h`, next at 4 h; 2 d → `2d`, next at 3 d.
    - `when_words`: from Fri 13 Nov 2026: 11 Nov → `2d overdue`; 13 → `today`; 14 → `tomorrow`;
      17 → `Tue`; 3 Dec → `3 Dec`.
    - `short_paths_and_accounts`: `/h/private/lantern` → `~/private/lantern`; `/a/b/c/d` → `c/d`;
      `claude-acct-b` → `acct-b`; `acct-a` stays.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them, and the CI step.
- [ ] **Step 4: Run them to see them pass**, then
  `nix develop --command cargo tree -p moa-view -e normal --prefix none --target all` (no IO crate).
- [ ] **Step 5: Commit:** `v1: moa-view: the data mirror, the clock and display-width text`.

## Task 5: lines, the list model, keys and the footer editor

**Files:**
- Create: `crates/moa-view/src/{line.rs, list.rs, keys.rs, editor.rs}`
- Modify: `crates/moa-view/src/lib.rs` (the four modules)
- Test: `crates/moa-view/tests/{list.rs, keys.rs}`

**Interfaces:**
- `line.rs`:
  - `pub enum Tone { Text, Dim, Head, Group, Warn, Ask, Ok, Fail, Work, Quiet, Brain, You, P0, Late, Today, Soon }`;
  - `pub struct Span { pub text: String, pub tone: Tone, pub bold: bool }`;
  - `pub enum Item { Session(SessionId), Group(String), Task(TaskId), Project(ProjectId), Suggestion(SuggestionId) }`;
  - `pub struct Line { pub spans: Vec<Span>, pub item: Option<Item>, pub more: bool, pub chip: Option<Span>, pub animated: bool, pub changes_at: Option<UtcMs> }`
    (`more`: a wrapped line of the item above; `chip`: drawn against the right edge);
  - `pub fn debug_lines(lines: &[Line]) -> String`: V19's snapshot form.
- `list.rs`: `pub struct ListState { pub sel: Option<usize>, pub top: usize, pub tracking: bool }`
  with `reselect(&mut self, was: Option<&Item>, lines: &[Line])` (V8); `follow(&mut self, lines,
  body: usize)` (the spike's `follow`); `step(&mut self, lines, down: bool)`, `ends(&mut self,
  lines, last: bool)`, `page(&mut self, lines, body, down: bool)`; `wheel(&mut self, lines, body,
  down: bool)` (±3, drags the selection, `tracking = false` until the next key); `row_at(&self,
  lines, y: usize, body: usize) -> Option<usize>` (a wrapped line is its item's; a heading is
  `None`); `selected<'a>(&self, lines: &'a [Line]) -> Option<&'a Item>`. With no selectable line,
  `step`/`page`/`ends` move `top` (V17).
- `keys.rs`:
  - `pub enum Key { Char(char), Ctrl(char), Enter, Esc, Backspace, Tab, BackTab, Up, Down, Left, Right, Home, End, PageUp, PageDown }`;
  - `pub enum Action { Down, Up, Top, Bottom, PageUp, PageDown, Activate, NextTab, PrevTab, Tab(u8), Quit, NewSession, ChooseAccount, NewGroup, Move, Rename, DeleteGroup, Archive, Close, Restart, Done, Drop, Yes, No }`;
  - `pub fn actions(view: View, tab: Option<View>, key: Key) -> &'static [Action]`: candidates
    in order; `tab` is the picture's shown tab (`None` for every other view), and the picture's
    rows follow that tab's rules, plus the picture's own tab keys. The panel takes the first that
    applies to the selected item (so `n` on a suggestion is No, elsewhere NewSession; `x` in the
    plan, or the picture's Plan tab, is Drop, never Close), per V7 and V21;
  - `pub fn hint(view: View, adopted_selected: bool) -> &'static str`: tree
    `⏎/click open · n new · + group · m move · r rename · D del group · a archive · x close`
    (plus ` · R restart under moa` when adopted is selected); plan `⏎/click jump · d done · x drop`;
    status `⏎/click jump · j/k · Ctrl-a / find`; today `j/k · ⏎ open · y/n suggestion`; projects
    `⏎ expand · n new session · N choose account`; activity `j/k scroll`; picture
    `1-6/tab · j/k · ⏎ open · n/N new · a archive · x close · y/n suggestion · q`.
- `editor.rs`: `pub struct Editor { pub label: String, pub text: String }`,
  `pub enum Edited { Typing, Done(String), Cancelled }`, `Editor::new(label, prefill)`,
  `key(&mut self, Key) -> Edited`, `paste(&mut self, &str)`, `shown(&self, width: usize) -> String`
  (`<label> <text>`, the tail kept when too long).

- [ ] **Step 1: Write the failing tests:**
  - `list.rs` (lines built by a small helper: `h` heading, `i(n)` item, `m` wrapped line):
    - `follow_shows_the_headings_above_the_first_item`: `[h, h, i1, i2, …]`, select `i1`, body 3
      → `top == 0`.
    - `follow_brings_the_key_hints_with_the_last_item`: `[…, i9, h, h]`, select `i9` → the last
      line is visible.
    - `wrapped_lines_scroll_and_click_with_their_item`: `row_at` on an `m` line gives its item's.
    - `an_item_taller_than_the_body_shows_from_its_first_line`.
    - `page_moves_by_selectable_lines_and_stops_at_the_ends`.
    - `the_wheel_moves_the_view_and_drags_the_selection_into_it`: after `wheel(down)` the
      selection is the nearest visible item and `tracking` is false; `step` sets it true.
    - `a_click_on_a_heading_is_ignored`.
    - **`the_selection_follows_its_item_when_rows_change`** (Review Focus 1): selected
      `Session(B)` at index 3; rebuilt lines put `Session(A)` above it → selection is
      `Session(B)` at 4; rebuilt without B → the nearest selectable index.
    - `a_list_with_nothing_to_select_scrolls_with_keys`.
  - `keys.rs`:
    - `every_spec_key_has_an_action`: tree `n N + m r D a x R`; plan `d x n N a`; status, today,
      projects and the picture `a x n N` on a session row; projects `n N`; today `y n`; picture
      `Tab BackTab l h Right Left 1..6 q`; every view `j k Up Down g G Home End PageUp PageDown
      Enter Char(' ')`.
    - `n_is_no_on_a_suggestion_and_new_elsewhere`.
    - `x_in_the_plan_only_drops`: `actions(Plan, None, x)` and `actions(Picture, Some(Plan), x)`
      are `[Drop]`; `actions(Picture, Some(Status), x)` holds `Close`.
    - `editor_keys`: typing, Backspace, Ctrl-u (clears), Ctrl-w (drops the last word), Esc →
      `Cancelled`, Enter → `Done` trimmed; `paste("a\nb")` → `a b`; `shown(10)` of a long text
      keeps its end.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: moa-view: lines, scrolling that keeps headings, keys, the footer editor`.

**PR p2 ends here.** Verify, review and open the PR (no try-it yet).

## Task 6: the panel and the tree

**Files:**
- Create: `crates/moa-view/src/{panel.rs, geometry.rs, views/mod.rs, views/tree.rs}`
- Modify: `crates/moa-view/src/lib.rs` (`panel`, `geometry`, `views`)
- Test: `crates/moa-view/tests/{tree.rs, panel.rs}`, snapshots under
  `crates/moa-view/tests/snapshots/`

**Interfaces:**
- `geometry.rs`: `pub struct Size { pub w: u16, pub h: u16 }`, `pub struct Rect { x, y, w, h: u16 }`,
  `pub struct Geometry { pub framed: bool, pub tabs: Option<Rect>, pub body: Rect, pub strip: Option<Rect>, pub footer: Rect }`,
  `pub fn geometry(view: View, size: Size) -> Geometry`. Framed: the spike's padding (body at
  x 2, y 1, width `w - 3`; one blank line, then the footer on the last inner line). Picture: tabs
  on line 0, a 7-line strip under a rule, the footer on the last line. Sizes too small for any of
  it give empty rects, never an underflow.
- `views/mod.rs`: `pub struct ViewUi { pub expanded: BTreeSet<ProjectId>, pub tab: usize }` and
  `pub fn build(view: View, data: &Data, ctx: &Ctx, ui: &ViewUi, width: u16) -> Vec<Line>`, which
  dispatches to one module per view, each with the same signature
  (`views::tree::build(data, ctx, ui, width)`, and so on; a view that has no use for `ui` ignores
  it). Tasks 7–10 add their module and its arm here.
- `panel.rs`:
  - `pub enum Out { Send(Op), Quit }`;
    `pub enum Mouse { Move { x: u16, y: u16 }, Click { x: u16, y: u16 }, WheelUp, WheelDown }`;
    `pub struct Handled { pub out: Vec<Out>, pub redraw: bool }`; `pub enum Mark { Plain, Hovered, Selected }`;
  - `pub struct Screen { pub geometry: Geometry, pub title: Option<String>, pub tabs: Option<(Vec<&'static str>, usize)>, pub brand: Option<Span>, pub body: Vec<(Line, Mark)>, pub strip: Vec<Line>, pub footer: Span, pub animate: bool, pub wake_at: Option<UtcMs> }`
    (`body` holds the visible lines and each one's mark; `Selected` wins over `Hovered`);
    `pub fn debug_screen(&Screen) -> String` (`debug_lines` with `>` for selected and `~` for
    hovered lines);
  - `impl Panel`: `new(view: View) -> Panel`; `view(&self) -> View`; `set_view(&mut self, View)`;
    `key(&mut self, k: Key, data: &Data, ctx: &Ctx, size: Size) -> Vec<Out>`;
    `mouse(&mut self, m: Mouse, data, ctx, size) -> Handled` (V11: `redraw` is false for a move
    that leaves the hovered row as it was); `blur(&mut self) -> bool` (focus-out: clears the
    hover, true when that changed something); `paste(&mut self, &str)`; `key` also clears the
    hover;
    `replied(&mut self, result: Result<Done, String>)` (the footer shows the last non-empty line
    of the text; an empty text shows nothing); `note(&mut self, text: &str)`;
    `screen(&mut self, data, ctx, size) -> Screen` (rebuilds, reselects, follows);
    `here(&self, data: &Data) -> Option<String>` (the selected row's folder: a session's cwd, a
    project's path, a task's session's cwd).
  - The footer shows, in order of precedence: the editor, a y/N question (`warn`), a message
    (`warn`, cleared by the next key), the unreadable-rows note (V18), the hint (`dim`).
- `views/tree.rs` (R1 §2.3 tree, V7, V16):
  - header (`dim`): `<n> sessions · <n> live · <n> asleep · <spinner> <n> working · <n> waiting on you`,
    the asleep, working and waiting parts only when non-zero; archived sessions are left out;
  - the tree: `group_path`, else the project's name, else `Loose`, split on `/`, plus every
    `groups` row; groups before sessions within a node; groups in the order of their newest
    member (`last_seen_at`), empty ones after by path; sessions newest first;
  - guides `├─ `, `└─ `, `│  ` below depth 0; a group line `▾/▸ <name>  <n>` + ` · <n> live` +
    the spinner when a member works (`Head` at depth 0, else `Group`, both bold); a node whose
    path is in `brain.folded` hides its subtree;
  - a session line, built for the width left after its guides (the spike's
    `width - len(prefix) - len(branch)`): `💤` or the state glyph and a space; the title (else the
    last prompt's first 60 characters, else the handle); the short account in 6 columns when that
    width is ≥ 34; the short folder in 23 columns when ≥ 76; the age (`last_seen_at`)
    right-aligned in 4. Tones: waiting `Warn` bold, question `Ask` bold, done `Ok` bold, failed
    `Fail` bold, other live `Ok`, the rest `Dim`; working and waiting glyphs animate
    (`SPIN[tick % 10]`, `⚠`/`!` on `(tick / 5) % 2`).

- [ ] **Step 1: Write the failing tests** (all on `fixture::lantern()`):
  - `tree.rs`:
    - `tree_model` (insta, `debug_lines` at 80).
    - `tree_header_counts`: `10 sessions · 8 live · 1 asleep · ⠸ 1 working · 1 waiting on you`
      (the fixture's `tick` is 3, and `SPIN[3]` is `⠸`; quiet doesn't count as working).
    - `groups_nest_with_guides_and_empty_groups_show`: `Private` › `Lantern`, `Kite`, `Harbor`,
      `Ideas` (empty, last); `Work` › `Harbor`; `Loose` holds the 💤 session.
    - `a_folded_node_hides_its_subtree`: `brain.folded = ["Private/Kite", "Loose"]` → neither
      subtree shows, and both read `▸`.
    - `columns_follow_the_width_left_after_the_guides`: `Fix login` sits 6 cells deep, so at a
      body width of 77 (an 80-column panel) its line has `acct-` and no folder; at 117 (120) it
      has `~/private/lantern`; at 57 (60) the account only; at 37 (40) neither.
    - `asleep_takes_the_glyph_and_its_space`: the 💤 line's title starts at the same column as a
      `●` line's at the same depth.
  - `panel.rs`:
    - `enter_on_a_session_focuses_it` → `[Send(SessionFocus { q: <full id> })]`.
    - `enter_on_a_node_folds_it` → `GroupFold { "Private/Lantern", true }`; on a folded one,
      `false`; on `Loose` → `GroupFold { "Loose", true }`.
    - `n_asks_folder_title_account_then_opens`: on group `Private/Lantern`, the prompts read
      `new session in Private/Lantern · folder: ~/private/lantern`, `title:`,
      `account (acct-a / acct-b): acct-a`; typing `Probe` as the title and Enter thrice →
      `SessionOpen { path: /h/private/lantern, account: Some("acct-a"), title: Some("Probe"), group: Some("Private/Lantern"), prompt: None, bg: false }`.
    - `n_stops_at_a_folder_it_cant_use`: `is_dir` false → `no folder ~/private/nope`; `work/x`
      → `a folder starts with ~ or /`; each right after the folder prompt, with no further prompt
      and nothing sent. `/h/work/x` (which `is_dir` allows) goes on to the title prompt: the
      core judges the brain.
    - `N_in_the_tree_asks_the_account_for_a_sessions_project`: `N` on `Fix login` → the account
      prompt, then `SessionOpen { path: /h/private/lantern, account: Some(..), group: None, .. }`;
      `N` on a group → nothing.
    - `n_on_a_group_prefills_from_its_subgroups`: on `Private` → the newest session under it
      (`Fix login`'s folder).
    - `an_empty_account_answer_cancels`.
    - `plus_m_r_D_a_send_their_ops`: `+` → `GroupAdd` (prefill `Private/Lantern/`; at the top
      level, empty); `m` → `SessionSet { group }`; `r` on a session → `SessionSet { title }`, on
      `Private/Lantern` typing `Beacon` → `GroupRename { old: "Private/Lantern", new: "Private/Beacon" }`;
      `D` on `Private/Ideas` → `GroupRm`; `a` → `SessionArchive { qs: [id] }`.
    - **`x_closes_the_session_it_named_even_after_the_list_moved`** (Review Focus 1): `x` on
      `Fix login` → footer `close Fix login? y/N`; a newer session is upserted (the list shifts);
      `y` → `SessionClose { q: <Fix login's id> }`; any other key → nothing sent.
    - `R_restarts_only_an_adopted_session`: on `Hand started` → the question, then
      `SessionRestart`; on another → nothing, and the hint lacks `R`.
    - `esc_in_a_prompt_says_cancelled`.
    - `a_reply_shows_its_last_line_and_the_next_key_clears_it`.
    - `here_is_the_selected_rows_folder`: a session → its cwd; a group → `None`.
    - Hover (V11), each through `Panel::mouse`/`key`/`blur` at 80 × 16:
      - `tree_hovered` (insta, `debug_screen`): a `Move` onto `Release notes`' row → that line is
        `~`, the selected line `>`;
      - `motion_along_one_row_doesnt_redraw`: a second `Move` on the same row (another column) →
        `redraw == false`; onto the next row → `true`; onto a heading → `true` once (no hover),
        then `false`;
      - `a_click_acts_on_the_row_under_the_pointer`: `Move` onto `Release notes`, `Click` there
        → `Send(SessionFocus { q: <its id> })` and it becomes the selection;
      - `a_key_clears_the_hover_mark`: after a hover, `j` → no line `Hovered`;
      - `focus_out_clears_the_hover_mark`: `blur()` → `true` and no line `Hovered`; again → `false`;
      - `a_hovered_item_that_leaves_the_list_clears_the_mark`: the hovered session is deleted
        from `Data` → no line `Hovered`;
      - `the_selection_wins_over_hover`: hovering the selected line marks it `Selected` only.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**; review the new snapshot by eye against the spike's
  tree (`picture.py: session_tree`, `tree_line`).
- [ ] **Step 5: Commit:** `v1: moa-view: the panel state machine and the sessions tree`.

## Task 7: the big picture (plan), and session-row keys everywhere

**Files:**
- Create: `crates/moa-view/src/views/plan.rs`
- Modify: `crates/moa-view/src/panel.rs` (Enter, `d`, `x` on tasks; V21's `a`, `x`, `n`, `N`),
  `crates/moa-view/src/views/mod.rs` (`mod plan` and its arm)
- Test: `crates/moa-view/tests/plan.rs`

**Interfaces:** `views::plan::build(data, ctx, ui, width) -> Vec<Line>` (`ui` unused), following the spike's
`plan()` exactly:
- header `Fri 13 Nov` + ` · ✓ <n> done today` (tasks `Done` whose `done_at` falls on `ctx.today`
  in `ctx.tz`), with `changes_at = start_of_next_day(today, tz)`;
- open tasks: status not done, dropped or idea; plus sessions whose display state is `Waiting`;
  grouped by project name (`-` for none);
- a task's kind: `wait` (status waiting or blocked, or who other), `agent` (who agent, or status
  active), else `you`;
- projects sorted by (no waiting session, best priority (p0 < p1 < rest), no `you` task, earliest
  due, name); each preceded by a blank line and headed `<name> ──── <n> on you · <n> with agents · <n> waiting`;
- inside: waiting sessions (`⚠` animated, `<title or project>: the session needs you`, `Warn`
  bold), then tasks by (kind you → agent → wait, priority, due, id); `P0 `/`P1 ` lead the title,
  P0 in `P0`; marks `★` (today's) / `▸` you, the spinner or `◐` agent with tail `agent working` or
  `agent`, `◷` wait with tail `blocked_on`, else `someone else` for who other;
- the due chip: `when()`; overdue ` <words> ` in `Late`, today ` today ` in `Today`, under 7 days
  `Soon`, else `Dim`. When `width - 6 - chip width < 16` the date goes into the tail and the line
  turns `Warn` if overdue or today. Text wraps at `width - 5 - (chip width + 1)`, continuation
  lines indented 4 and marked `more`;
- empty: `  nothing tracked yet: moa fills this in as sessions report` (`Dim`);
- then `no open tasks: <names>` (`Dim`, wrapped) for projects with `last_activity_on` within 7
  days, no open task, and not listed above; then a blank and
  `↵ or click: jump to its session · d done · x drop` (`Dim`).

Panel actions: Enter on a task → `SessionFocus` of its session, else `no session linked to this
task`; `d` → `TaskSet { id, status: Some("done") }`; `x` → `TaskSet { id, status: Some("dropped") }`.
V21's keys go in `Panel` for every view but the tree's own.

- [ ] **Step 1: Write the failing tests:**
  - `plan_model` (insta at 80).
  - `projects_sort_needs_you_first_then_priority_then_you_tasks_then_due_then_name`: Lantern (a
    waiting session) first, then Kite (a P1 you task); Harbor isn't listed and is in
    `no open tasks: Harbor`.
  - `task_rows_sort_and_mark`: in Lantern, `★ P0 Fix the login loop` (`P0` tone) before
    `<spinner> Write the README  (agent working)` before `◷ Wait for review  (design team)`.
  - `due_chips`: ` 1d overdue ` `Late`; ` today ` `Today`; `tomorrow`, `Tue` `Soon`; `3 Dec` `Dim`.
  - `a_narrow_plan_puts_the_date_in_the_text`: at 30 the overdue task reads
    `★ P0 Fix the login loop  (1d overdue)` wrapped, `Warn`, no chip.
  - `long_titles_wrap_under_their_mark`: continuation lines start with 4 spaces, `more == true`.
  - `done_today_counts_by_local_day`: `done_at` 23:30 local yesterday doesn't count, 00:10 today
    does (`EST5EDT`).
  - `an_empty_plan_says_so`.
  - Panel: `d_and_x_set_the_tasks_status`; `enter_jumps_to_the_tasks_session_or_says_none`;
    `session_row_keys_in_the_plan`: `a` on `Fix login`'s row → `SessionArchive`, `x` there →
    nothing; `n` on `Pick a cover` → `SessionOpen { path: /h/private/kite, account: None, .. }`;
    `N` there → the account prompt first; `n` on `Renew the domain` (the fixture's task with no
    project, under `-`) → `this row has no project`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**; compare the snapshot with the spike's `plan()` by eye.
- [ ] **Step 5: Commit:** `v1: moa-view: the big picture, with due chips and wrapping`.

**PR p3 ends here.** Verify, review and open the PR (no try-it yet).

## Task 8: status and today

**Files:**
- Create: `crates/moa-view/src/views/{status.rs, today.rs}`
- Modify: `crates/moa-view/src/views/mod.rs` (both modules and arms),
  `crates/moa-view/src/panel.rs` (`y`/`n` → `SuggestionAnswer`; Enter on their rows)
- Test: `crates/moa-view/tests/status_today.rs`

**Interfaces:**
- **status** (V16): every live session, grouped by display state (waiting, question, failed,
  done, working, quiet, idle), each group headed `<word> ──── <n>` in its tone (idle `Head`); a
  line `<glyph> <title>` with the title in `max(8, min(26, w-8-gw))` columns when `w ≥ 72`, else
  `w-8-gw`; the group's last component in `gw = 14` columns when `w ≥ 44`; the age since
  `activity_at` in 4; `  <why>` when `w ≥ 72`. Empty: `no live sessions: Ctrl-a / finds one to
  resume`. Enter → focus.
- **today**: `TODAY · Fri 13 Nov`; today's tasks (`✓`/`▸ <project>: <title>  [<status>]`, done
  last, `Dim`), else `  (nothing yet: set today's goals with moa)`; `WAITING ON YOU`: live,
  non-working sessions seen in the last 24 h, newest first, as
  `<glyph> <title or project> [<account>] <age>  <prompt>` (`Warn` for waiting), else
  `  (nothing)`; `WORKING` when any; `SUGGESTIONS  (y accept · n reject)`: open and later ones
  as `? #<id> <text>` in `Brain`, else `  (none)`. `y`/`n` → `SuggestionAnswer { id, Yes|No }`.

- [ ] **Step 1: Write the failing tests:** `status_model`, `today_model` (insta at 80);
  `status_groups_by_urgency_with_failed_and_quiet` (headings in V16's order, ✗ in `Fail`, ◌ in
  `Quiet`); `status_columns_by_width` (the why at 80, not at 60; the group at 60, not at 40);
  `an_empty_status_says_how_to_find_one`; `y_and_n_answer_a_suggestion`;
  `session_row_keys_in_status_and_today`: `x` on a status row → `close <title>? y/N`, then
  `SessionClose`; `n` on a session row in today → `SessionOpen` in its project's folder.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: moa-view: the status panel and today`.

## Task 9: projects and activity

**Files:**
- Create: `crates/moa-view/src/views/{projects.rs, activity.rs}`
- Modify: `crates/moa-view/src/views/mod.rs` (both modules and arms),
  `crates/moa-view/src/panel.rs` (Enter toggles `ViewUi.expanded`; `no folder for <name>`)
- Test: `crates/moa-view/tests/projects_activity.rs`

**Interfaces:**
- **projects**: ideas last, then `last_activity_on` newest first, then name; a line
  `▸/▾ <name>  <status> <idle>d  <n> live · <n> tasks` (`Brain` for ideas, `Dim` when idle over 60
  days); expanded (`ViewUi.expanded`, Enter toggles): the summary (`Dim`), open tasks
  `    ▸ <title>  [<status>] ⧗ <blocked_on>`, the last 6 sessions (`Dim` when ended). `n`/`N` per
  V21; a project with no path → `no folder for <name>`.
- **activity**: the newest 300 rows first, `HH:MM <text>` in local time, tone by who (brain
  `Brain`, you `You`, session `Dim`); nothing selectable (V17).

- [ ] **Step 1: Write the failing tests:** `projects_model` (Lantern expanded), `activity_model`
  (insta at 80); `enter_expands_a_project_and_N_asks_which_account`;
  `no_folder_for_a_project_without_a_path` (Beacon); `activity_newest_first_coloured_by_who`;
  `keys_scroll_the_activity_list` (j moves `top`, no selection bar).
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: moa-view: projects and activity`.

## Task 10: the picture, animation and deadlines

**Files:**
- Create: `crates/moa-view/src/views/picture.rs`
- Modify: `crates/moa-view/src/panel.rs` (tabs; `Screen.animate`, `Screen.wake_at`; views this
  build doesn't draw), `crates/moa-view/src/views/mod.rs` (`mod picture`, its arm, and the
  not-yet arm)
- Test: `crates/moa-view/tests/{picture.rs, time.rs, never_panic.rs}`

**Interfaces:**
- **picture**: tabs `Plan Today Projects Sessions Activity Status` (the current one bracketed);
  the brand (V21) in `Brain` bold at the right: `moa <spinner>` or `moa ●`; the tab's view; a rule
  `─ activity ─`; the newest 7 activity rows, oldest first, as `HH:MM ✦<text>` when under 4 s old
  (bold, and reversed on `(tick * 2 / 5) % 2 == 1`), else `HH:MM  <text>`. Tab/→/l, BackTab/←/h
  and 1–6 switch tabs (selection to the top); `q` quits. Inner views keep their own keys.
- `Screen.animate`: a visible body or strip line has `animated`, or the brand spins.
  `Screen.wake_at`: the earliest `changes_at` of the visible lines and the strip, with ages'
  `next_ago_change`, flashes' end (`at + 4 s`) and `start_of_next_day` on date headers.
- Views this build doesn't draw: one `Dim` line with V15's wording.

- [ ] **Step 1: Write the failing tests:**
  - `picture.rs`: `picture_model` (insta); `tabs_switch_with_tab_numbers_and_arrows`;
    `q_quits_only_the_picture`; `the_strip_flashes_rows_under_4_seconds`;
    `unknown_views_say_not_yet`: `View::Console` → `the console isn't in this build yet`;
    `View::Picker` → `finding a session isn't in this build yet`.
  - `time.rs` (its own small `Data`, not the fixture):
    - **`text_changes_at_exact_deadlines`** (Review Focus 5): one session last seen 5 min 10 s
      ago in the tree → `wake_at == now + 50 s`; the plan's `wake_at` is never later than
      `start_of_next_day`; a 1 s-old activity row in the picture's strip makes `wake_at` its
      `at + 4 s`.
    - **`nothing_animates_off_screen`**: in the status view, one working session among 30 idle
      ones, scrolled out of the body → `animate == false`; scrolled back → `true`; a picture with
      no working or waiting session and no fresh row → `false`, and the brand reads `moa ●`.
  - `never_panic.rs`: the proptest **`views_never_panic_or_leak_control_characters`**: random
    `Data` (sessions, groups, tasks, projects, activity with arbitrary strings including ESC,
    `\n`, CJK, emoji sequences and empty ones; random states and dates), every `View`, sizes
    0..250 × 0..60, then 20 random keys, wheel, click and mouse-move events and focus-outs (`blur`) → no panic, and no
    `char::is_control`, U+FE0F or U+200D in any span of the `Screen`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: moa-view: the picture, and when a panel wakes`.

**PR p4 ends here.** Verify, review and open the PR (no try-it yet).

## Task 11: `moa-tui`: the palette, the frame, drawing

**Files:**
- Create: `crates/moa-tui/{Cargo.toml, src/lib.rs, src/theme.rs, src/draw.rs}`
- Test: `crates/moa-tui/tests/{render.rs, theme.rs}`, snapshots under `crates/moa-tui/tests/snapshots/`

**Interfaces:**
- `moa-tui` depends on `moa-view`, `moa-core`, `jiff`, `ratatui`, `crossterm` (Task 13 adds
  `moa-client`, `moa-platform`, tokio, `futures-util`); dev: `moa-view` with `fixture`, `insta`.
- `theme.rs`: `pub enum Token { Text, Bright, Dim, Line, Surface, Sel, Dark, Blue, Cyan, Green, Yellow, Orange, Red, Magenta }`
  with `pub const fn hex(self) -> (u8, u8, u8)` (spec §6.14 exactly);
  `pub enum ColorMode { True, Ansi256 }`, `pub fn detect(in_tmux: bool, colorterm: Option<&str>) -> ColorMode`;
  `pub fn nearest_256(rgb: (u8, u8, u8)) -> u8` (the spike's `xterm256`);
  `pub struct Theme { pub mode: ColorMode }`, `pub fn style(&self, tone: Tone, bold: bool) -> Style`,
  `pub fn selected(&self) -> Style` (V12's map), `pub fn hovered(&self) -> Style` (background
  `surface` only, V11).
- `draw.rs`: `pub fn draw(buf: &mut Buffer, area: Rect, screen: &Screen, theme: &Theme)`:
  - the frame by hand, as `chrome.frame`: `╭─…─╮`, `│`, `╰─…─╯` in `Color::Reset`, the title as
    ` <label> ` bold at column 2, nothing when the area is under 4 × 2;
  - body lines at `geometry.body`, each span in its tone; the selected line is a full-width bar
    in `selected()`, keeping bold; a hovered line is a full-width `hovered()` background under its
    own tones; a chip drawn right-aligned when it fits after the text, in the
    bar's style unless it is a padded pill (late, today);
  - tabs, brand, the strip's rule and lines, the footer;
  - every string is placed by display width (`moa_view::text`), so 💤 and CJK take two cells.

- [ ] **Step 1: Write the failing tests:**
  - `render.rs`: **`every_view_at_every_width`**: for each of tree, plan, status, today,
    projects, activity, picture and each width in 40, 60, 80, 120, 200, draw `lantern()` into a
    `TestBackend` of that width × 16 and `insta::assert_snapshot!(format!("{view}@{width}"), text)`
    (35 snapshots).
  - `render.rs`, cell checks at 80:
    - `the_frame_uses_the_default_colour`: corners and edges have `fg == Color::Reset`; a body
      cell of plain text has `fg == Rgb(0xd8, 0xde, 0xe9)`;
    - `a_hovered_row_has_the_surface_background`: every body cell of a `Hovered` line has
      `bg == Rgb(0x3b, 0x42, 0x52)` and keeps its tone's `fg` (a waiting row stays yellow);
    - `the_selection_bar_spans_the_body`: every body cell of the selected line has
      `bg == Rgb(0x5e, 0x81, 0xac)`;
    - `a_waiting_row_is_yellow_and_bold`; `an_overdue_chip_is_dark_on_red`;
    - `wide_glyphs_take_two_cells`: on the 💤 line and the `Dev server 👍` line, the cell after
      the glyph is ratatui's skipped continuation cell, and the right edge `│` of every line is in
      the last column.
  - `render.rs`: **`tiny_sizes_draw_without_panicking`** (Review Focus 3): every view at widths
    0..=12 and heights 0..=4, drawn into a pane-sized `area` placed at (5, 3) inside a larger
    buffer (30 × 12) pre-filled with `#`: no panic, and every cell outside `area` is still `#` with
    its style unchanged (nothing drawn outside the pane). Then a resize from 80 × 16 to 3 × 1 with
    the editor open, then back, with the editor's text kept.
  - `theme.rs`: `nearest_256_matches_the_spike` (`text` → 254, `red` → 174, `blue` → 110);
    `detect_colour_mode` (`(true, None)` → True; `(false, Some("truecolor"))` → True;
    `(false, Some("24bit"))` → True; `(false, None)` → Ansi256); `the_palette_is_exact` (all 14
    `hex` values against spec §6.14).
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**; read every new snapshot, at 40 and 200 especially,
  against the spike's look (rounded frame, title on the edge, one column of padding).
- [ ] **Step 5: Commit:** `v1: moa-tui: the palette, the rounded frame, every view drawn`.

**PR p5 ends here.** Verify, review and open the PR. Its body shows three snapshots (tree@80,
plan@80, picture@120) as text, so the look can be judged in the PR.

## Task 12: `Live`, a connection that never blocks the panel

**Files:**
- Create: `crates/moa-client/src/live.rs`
- Modify: `crates/moa-client/src/lib.rs` (`mod live; pub use live::*;`; `Wire` gains what `Live`
  needs), `crates/moa-client/Cargo.toml` (tokio `sync`)
- Test: `crates/moa-client/tests/live.rs` (new: a scripted server, a `UnixListener` in a temp
  folder that answers `hello` and `subscribe` and then does what each test says, so timing is the
  test's), `crates/moa/tests/client.rs` (one test against a real daemon)

**Interfaces:**
- `pub enum LiveMsg { Patch(Patch), Snapshot(SnapshotMsg), Event(CoreEvent), Reply { tag: u64, result: Result<Done, String> }, Closed }`;
- `impl Client { pub async fn live(self, topics: &[Topic], first: Vec<Op>) -> Result<(Live, Vec<Result<Done, String>>, SnapshotMsg), ClientError> }`:
  sends the `first` ops in order and waits for their replies (returned in that `Vec`, untagged),
  then subscribes and returns the snapshot; from then on a spawned task owns the connection;
- `impl Live { pub fn send(&mut self, op: Op) -> u64; pub async fn recv(&mut self) -> LiveMsg }`:
  `send` never waits and returns the next tag, starting at 1 for each `Live` and counting up by
  one per `send`; `recv` is cancel-safe (an mpsc receive); `Closed` once, when the connection
  ends, after answering every reply still owed with `ClientError::Stopped`'s wording (the write
  one for a write).

- [ ] **Step 1: Write the failing tests:**
  - `live.rs` (scripted server):
    - `first_ops_are_answered_before_the_snapshot`: `live([Panels], [UiPanel, UiHere])` returns
      both replies, then the snapshot the server sent.
    - `a_slow_reply_doesnt_hold_back_pushes`: the server holds op 1's reply, sends a `patch`,
      answers op 2, then op 1 → `recv` yields `Patch`, `Reply { tag: 2 }`, `Reply { tag: 1 }`.
    - **`live_says_closed_when_the_daemon_stops`** (Review Focus 4): the server reads op 1 (a
      `TaskSet`, a write) and closes → `Reply { tag: 1, Err("moa stopped before answering; it may
      have saved that, so check before trying again") }`, then `Closed`, and nothing after.
  - `client.rs` (a real `TempBrain` daemon): `live_follows_a_real_daemon`: `live([Tasks], [])`;
    `send(TaskAdd Lantern "Probe")` → its `Reply` and a `Patch` whose `tasks` upsert holds `Probe`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass.**
- [ ] **Step 5: Commit:** `v1: moa-client: Live, a subscribed connection with tagged replies`.

## Task 13: `moa panel`, and the mouse

**Files:**
- Create: `crates/moa-tui/src/run.rs`, `crates/moa/src/panel.rs`
- Modify: `crates/moa/src/main.rs` (`Command::Panel`), `crates/moa/src/ctl.rs` (`Here::config`,
  `caller`, `options` become `pub(crate)`), `crates/moa/Cargo.toml` (`moa-tui`),
  `crates/moa-tui/Cargo.toml`
- Modify: `crates/moa-core/src/config.rs` (`"panel"` in `RESERVED_BRAIN_NAMES`; `[ui] mouse`,
  `UiConfig`), `crates/moa-tmux/src/conf.rs` (`moa_tmux(brain, mouse: bool)`),
  `crates/moa-tmux/src/moa.tmux.tpl` (`{{MOUSE}}`: `set -g  mouse on` with a comment, or nothing),
  `crates/moa-daemon/src/run.rs` (`tmux_link(paths, b)` becomes `tmux_link(paths, b, mouse: bool)`,
  its caller passing `config.ui.mouse`, and its `moa_tmux` call ~l.120 passing `mouse`),
  `crates/moa-daemon/src/tmux_tests.rs` (~l.118: `moa_tmux(B, true)`),
  `crates/moa-tui/src/lib.rs` (`pub mod run`)
- Test: `crates/moa-tui/tests/run.rs`, `crates/moa/tests/cli.rs`, `crates/moa-tmux/tests/conf.rs`
  (the `moa_tmux_default`/`moa_tmux_work` snapshots at l.15–16 gain the mouse line; the other
  calls at l.31 and l.83 pass `true`), `crates/moa-core/tests/config.rs`

`mouse = false` takes effect when moa's terminal next starts: the file then has no `mouse` line,
and a running server keeps the mouse it has (writing `mouse off` would override a user who wants
it on).

**Interfaces:**
- `moa_tui::run`:
  - `pub struct PanelOptions { pub client: ClientOptions, pub view: Option<View>, pub pane: Option<PaneId>, pub home: PathBuf, pub tz: TimeZone }`;
  - `pub async fn run(opts: PanelOptions) -> Result<(), String>`;
  - `pub const INIT: &str = "\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h"` (V11) and
    `pub const RESTORE: &str` (the reverse, and the cursor shown); raw mode on start; a panic
    hook writes `RESTORE` and leaves raw mode before the default hook runs;
  - `pub fn claim_pane(tmux: Option<&str>, tmux_pane: Option<&str>, socket: &TmuxSocket, tmux_tmpdir: Option<&Path>, uid: u32) -> Option<PaneId>` (V14);
  - `pub fn first_ops(pane: Option<PaneId>, view: Option<View>, here: Option<String>) -> Vec<Op>`:
    `[UiPanel, UiHere]` (V13);
  - `pub fn reconnect_delays() -> Vec<Duration>`: 250 ms doubling to 8 s, then 8 s, summing to
    at most 60 s;
  - `pub fn key_of(ev: crossterm::event::KeyEvent) -> Option<Key>` and
    `pub fn mouse_of(ev: MouseEvent) -> Option<Mouse>` (a move or a drag → `Move`, left press →
    `Click`, wheel → `Wheel*`, the rest `None`).
- The loop: one unbiased `tokio::select!` handling one item per turn, over crossterm's
  `EventStream` (key presses only; Ctrl-c → quit; paste → `Panel::paste`; resize → redraw; a
  mouse event → `Panel::mouse`, redrawn only when `Handled.redraw`; `FocusLost` → `Panel::blur`,
  redrawn when it returns true; `FocusGained` ignored),
  `Live::recv` (patches and snapshots into `Data`; this connection's `panels` row with another
  view → `set_view`; events → `note`; replies → `replied`), a 100 ms `interval` that exists only
  while the last `Screen.animate` was true, and one `sleep_until` at `Screen.wake_at`. After each
  it redraws (one `Terminal::draw`) and sends `ui.here` when `Panel::here` changed.
- Every connect (V13): `hello().matched` false → the mismatch line, nothing sent; else
  `live(TOPICS, first_ops(pane, current view, current here))`, the key from the `UiPanel` reply,
  `Data` replaced from the snapshot. A `Closed` → footer `moa isn't running; trying again…`, the
  delays, then `moa isn't running: press any key to try again`. Reconnects use `autostart: None`.
- `moa panel [VIEW] [--brain B]` (`crates/moa/src/panel.rs`): parses `VIEW` with `View::from_str`
  (`moa panel: no view called x; views: tree, plan, status, today, projects, activity, picture`),
  picks the brain (V15), builds `ClientOptions { client: "panel", autostart: Some(own_exe),
  on_mismatch: Accept, .. }`, the pane through `claim_pane` with `Paths::tmux_socket`,
  `TimeZone::system()`, and runs it on `ctl::runtime()`.

- [ ] **Step 1: Write the failing tests:**
  - `run.rs`:
    - `the_panel_asks_for_the_mouse_focus_and_paste`: `INIT` contains `?1003h`, `?1006h`,
      `?1004h` and `?2004h`; `RESTORE` turns each off. (Under V22: `?1000h`, `?1006h` and
      `?2004h`, and neither `?1003h` nor `?1004h`.)
    - `a_pane_from_another_server_is_not_claimed` (V14): `TMUX=<dir>/tmux-default,1,0`,
      `TMUX_PANE=%3` with `TmuxSocket::Path(<dir>/tmux-default)` → `Some(%3)`; the same socket
      named through a symlinked folder → `Some(%3)`; `TmuxSocket::Name("moa")` under another
      `TMUX_TMPDIR` → `None`; `TMUX` unset or garbage → `None`.
    - **`first_ops_register_and_restore_here`**: `first_ops(Some(%3), Some(Plan), Some("/x"))`
      → `[UiPanel { pane: "%3", view: Plan }, UiHere { folder: "/x" }]`.
    - **`reconnect_gives_up_after_about_a_minute`**: the first delay is 250 ms, none is over 8 s,
      the sum is at most 60 s and at least 45 s.
    - `keys_and_mouse_map`: Ctrl-u → `Ctrl('u')`, Shift-Tab → `BackTab`, a left press at (5, 4)
      → `Click { x: 5, y: 4 }`, a move at (5, 4) → `Move { x: 5, y: 4 }`, a release → `None`.
  - `conf.rs`: `moa_tmux_turns_the_mouse_on_unless_told_not_to`: `moa_tmux("default", true)`
    has the line `set -g  mouse on`; with `false`, no `mouse` line at all. Under V22 only, also
    `moa_tmux_binds_the_first_click_to_focus`: with `true`, the `MouseDown1Pane` line of V22's row
    exactly; with `false`, no `MouseDown1Pane` line.
  - `config.rs`: `ui_mouse_parses`: absent → true; `false` → false; `"no"` → the error line.
  - `"panel"` as a reserved brain name needs no new test: the existing
    `every_verb_and_subcommand_is_reserved` (`crates/moa/src/main.rs`) fails once `Command::Panel`
    exists and passes when `"panel"` joins `RESERVED_BRAIN_NAMES`.
  - `cli.rs`:
    - `moa_panel_names_the_views_it_knows`: `moa panel nope` → exit 1 and the views line.
    - `moa_panel_refuses_outside_the_sandbox`: `HOME` a temp folder, no `MOA_ROOT`, `PATH` an
      empty folder → the same refusal `moactl` gives, exit 1.
    - **`a_different_build_is_said_not_restarted`** (Review Focus 4): a daemon started from a copy
      of the binary at another path (another build id, P10) → `moa panel tree` on a `Terminal`
      shows the mismatch line, and `moa status --json` still reports the first daemon's pid.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement** them.
- [ ] **Step 4: Run them to see them pass**, then `cargo nextest run --workspace`, and the tmux
  3.4 pass on `moa-tmux`, `moa-daemon` and `moa`.
- [ ] **Step 5: Commit:** `v1: moa panel: a live view in any terminal; moa turns the mouse on`.

## Task 14: end to end, and the try-it

**Files:**
- Create: `crates/moa/tests/panel_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs` (`panel_tab(tb, view) -> String`: a `new-window -d -P
  -F '#{pane_id}'` running `<MOA> panel <view>` in the sandbox, through `common::tmux`;
  `screen_of(tb, pane) -> String` = `capture-pane -p -t <pane>`; `click(tb, term, pane,
  line_text)`: selects the panel's tab for the attached client, finds `line_text`'s row in
  `capture-pane`, and presses an SGR left press and release on the `Terminal` at
  `#{pane_left}`+3, `#{pane_top}`+row+1)

Earlier tasks unit-test every piece, so most of these should pass at once. The ones expected to
fail first: the `$`, fold and restart tests (they need the whole chain: key → core → tmux, and
re-registering).

- [ ] **Step 1: Write the failing tests** (front door on a `Terminal`, `came_up`;
  `moa_toml_with_fake_account`; fake sessions `moactl open --bg` with `[Hook SessionStart, Hang]`;
  every wait through `wait_for`, 10 s):
  - `a_tree_panel_shows_the_brain_and_follows_it`: two fake sessions; `panel_tab(tb, "tree")` →
    the screen shows `▤ sessions` and both titles; `moactl set <id> --title Beacon` → `Beacon`
    shows; `moactl group add Private/Ideas` → `Ideas` shows.
  - `keys_and_a_click_act`: in the tree panel, `send-keys j` until the second session's line is
    the bar (read from `capture-pane -e`'s `sel` background), `Enter` → the client's current tab
    is that session's; back to the panel's tab, `click(…, "<first title>")` → the client shows
    the first session's tab.
  - `the_terminal_key_opens_where_the_panel_points`: the bar on a session in `<root>/work/b`;
    `Ctrl-a $` on the `Terminal` → a new tab named `⌨ b` whose `#{pane_current_path}` is
    `<root>/work/b`.
  - `a_fold_is_kept_across_panels`: Enter on a group in one tree panel → a second tree panel
    started after it shows the group folded (`▸`).
  - `hover_marks_the_row_under_the_pointer_and_focus_out_clears_it` (V11; on 3.4 and 3.8): the
    panel's tab split in two (`split-window`, through `common::tmux`). The panel is active first,
    then the other half is, so the panel is **unfocused** for the rest:
    1. the `Terminal`'s output holds `\e[?1003h` (tmux turned motion reporting on);
    2. an SGR motion (`\e[<35;x;yM`) at the first title's row, positioned from the panel's own
       `#{pane_left}`/`#{pane_top}` and its row in `capture-pane` → `capture-pane -e` shows that
       row on `surface` (`48;2;59;66;82`), while the panel is still not active;
    3. a click there → that session's tab comes up;
    4. back to the split tab with the panel active, hover a row, then `select-pane` to the other
       half → the mark is gone (focus-out).
    Under V22 it is replaced by `a_first_click_on_another_pane_only_selects_it` (V22's row).
  - `panels_launch_nothing_in_the_daemon`: `status_json(tb)["launches"]` is the same before and
    after ten keys, a click, a wheel (SGR 64/65), twenty mouse moves, a footer prompt answered
    with Esc, and `Ctrl-a $`.
  - **`a_panel_comes_back_after_moa_restarts`** (Review Focus 4): with the bar on the session in
    `<root>/work/b`, `moa shutdown` → the screen shows `moa isn't running; trying again…` and
    still the titles; `moactl ls` (which starts moa) → the footer clears, a new
    `moactl set … --title Kite2` shows, a test `Client` subscribed to `panels` sees one row with
    the panel's pane and `tree`, and `Ctrl-a $` opens in `<root>/work/b`.
- [ ] **Step 2: Run them to see them fail** (or pass where earlier tasks did the work; say which
  in the report).
- [ ] **Step 3: Fix what they find.** A failure here is a bug in an earlier task: fix it there,
  with its own test.
- [ ] **Step 4: Run** the full verify three times, and `moa`'s tests on tmux 3.4.
- [ ] **Step 5: Commit:** `v1: panels end to end in the sandbox`.

**PR p6 ends here.** Verify, review and open the PR with the try-it below. Its body also says that
with moa's `mouse on`, selecting text in a tab needs Shift held, until 5c's help says it too.

## Try it (PR p6)

In your own terminal, from this checkout. Your real brains and the spike are untouched.

1. **Open the sandbox:** `nix run . -- --sandbox`. In moa's first tab, open a session or two
   (`nix run <this checkout> -- open ~/tmp/moa-try`) and ask one something.
2. **A tree panel:** back in moa's first tab (`Ctrl-a m`), `cd` to this checkout and run
   `nix run . -- panel tree`. You should see a rounded frame titled `▤ sessions`, your sessions
   nested by group with guides, a spinner while one works, and the key hints at the bottom.
3. **Point, then click a session**: the row under the pointer gets a soft grey highlight first;
   a click opens that row's tab. `Ctrl-a m` brings you back. Try `j`/`k`, the wheel,
   Enter on a group (it folds, and stays folded if you start the panel again), `r` to rename.
   To select text with the mouse, hold Shift.
4. **A terminal where you point:** select a session, then `Ctrl-a $`: a `⌨` tab opens in that
   session's folder.
5. **The other views:** Ctrl-c, then `nix run . -- panel picture` (Tab or 1–6 between views; a
   new activity line flashes ✦ for 4 seconds), `panel plan`, `panel status`. Make the terminal
   narrow and wide: columns drop and come back.
6. **Outside tmux:** in a second terminal (not inside moa),
   `MOA_ROOT=$HOME/.local/state/moa-sandbox nix run . -- panel plan` works there too.

**What this checks for real:** the exact colours in your terminal; that 💤, ⚠ and the frame line up
as tmux draws them; that clicks reach the panel with moa's `mouse on`. If you keep `mouse off` in
your own tmux config and want it kept, add `[ui]` / `mouse = false` to the sandbox's `moa.toml`; it takes effect when moa's terminal
next starts.

**If a glyph looks misaligned:** tell me which one and your terminal's name.

## Carried items: where each lands

| Item (source) | Where |
|---|---|
| The `$` key and `ui.here` (4b R16) | Tasks 1–2 (V3, V6) |
| The `n` key (4b R16) | **5c**, with the first `command-prompt` |
| The tree's restart key (4b R24) | Task 6 (`R`, V7) |
| `focus` swaps into the agent spot (4b R12, roadmap 4) | **5b** |
| Sleep parks an empty slot in the spot; `sleep Q` on the spot's session empties it today (roadmap 8, 4b ledger) | **5b** |
| `is_cockpit` refresh costs a full listing per tab rename (4b ledger, its Task 11) | **5b** (the cockpit is its subject) |
| Prompts: `\$` on 3.4 only, `command-prompt -b`, the template `Cmd` (slice 3, 4b) | **5c**: 5a sends no `command-prompt` |
| Small cleanups (slice 3): the T23 guard, `new-session` on the allow-list, `session_named`, `Sandbox::tmux`'s PATH lookup, `write_if_changed`'s mode, `XDG_CONFIG_HOME`'s trailing `/` or `,`, `MAX_BLOCK`, the parse-error text, the front door's "no tmux" line | **Carried**: 5a touches `conf.rs` only for `moa_tmux`'s mouse flag; the `XDG_CONFIG_HOME` dedupe is in `tmux_conf` and needs tmux-format escaping worked out on 3.4 and 3.8, so it waits for the next change there. `session_named` and `Sandbox::tmux` go with 5b |
| Upgrade policy and `Patch`'s base rev (2b, roadmap slice 5 note) | **Slice 9.** Meanwhile panels accept a mismatched daemon and say so (V13), and every connect starts with a full snapshot |
| Theme file and hot reload (spec §6.14) | **Slice 9** (roadmap item 3 lists the theme) |
| A panel reads its time zone once (`TimeZone::system()`), so after a zone change "today" and the midnight deadline are off until it restarts | **Slice 9**, with config hot reload and panels following the build; the daemon's `ZoneWatch` already exists |
| `$` on a panel with nothing selected uses the agent spot's folder (spike `here_dir`) | **5b**, with the spot |
| `ui.select_view` (spec §6.4) | **5b**, with `goto` |
| A panel's own errors and a dead panel in the log | **5b** (supervision); 5a shows them in the footer |
| The console's command buttons: a runtime list moa sets (and the user can override), not the spike's fixed button bar under the input (spec §6.13 console, LEARNINGS "Next steps are buttons"); the user's call, 2026-11-18 | **Slice 6** (the console). 5a builds nothing for it. 5a's views have no button bar: their footer hint lines (the picture's too) name each panel's own keys, so they stay fixed per view. Only the today view lists actions to press, and those are its suggestions, which moa already chooses |
| `moactl term --pane` stays accepted and unused (4b R16) | **Carried**: the key goes through the mailbox (V6), so nothing needs it |
| `groups.collapsed` is unused (V5) | **After the switch-over**, with the other schema tidying (the additive-only rule holds until then) |
