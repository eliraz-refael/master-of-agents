# moa v1 slice 5c: the picker, help, and new sessions

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Ctrl-a /` finds any session by typing part of its name and brings it to the screen,
as a popup (the default) or as a full tab. `Ctrl-a ?` shows every key moa has, written from
moa's own key table. `Ctrl-a n` opens a new session from anywhere by asking for a folder and a
title.
- The full-tab form is a panel parked in `_moa` and linked into your session, so opening it
  starts nothing once it exists.
- The popup form starts one short-lived `moa pick` per open. That's the user's choice, U17.
- Slice 5b's carried items land here first: the 1,350-line cockpit queue file split in three,
  one "home → ~" helper, the frame rule, the managed line that narrow panels cut off, and a
  reply that still quoted tmux.

**Architecture:**
- **`moa_core::keymap`** (pure) is the one table of `Ctrl-a` keys. The tmux file's bindings,
  its menu, and the help view are all written from it.
- **`moa_core::prompt`** sends tmux's `command-prompt -b`. Its answers come back through the
  mailbox as `moa:prompt TOKEN TEXT`, and the core runs the two questions of `Ctrl-a n` one at
  a time.
- **The picker and help are "sheets":** views shown on demand rather than placed by a layout.
  - **Popup form:** a static `display-popup` binding in `moa.tmux` runs
    `moa panel <view> --once`. moa never sends `display-popup` itself, because it waits.
  - **Full-tab form:** the core's cockpit queue links the parked sheet panel's window into the
    user's session (`link-window`). It unlinks it on a pick, on Esc, or when the user leaves
    that tab.
- **`moa-view`** gets the picker's model (rows, order, fuzzy match, highlights) and the help
  view's lines. `moa-tui` draws them; with `--once` it draws no frame of its own, because tmux
  frames the popup.

```
 Ctrl-a /  (popup)  ─► tmux: display-popup -E … moa panel picker --once ─► type, ⏎ ─► session.focus Q ─► Ok: exit (popup closes)
 Ctrl-a s  (tab)    ─► moa:key find ─► cockpit queue: Sheet{picker} ─► listing ─► [spawn once] ─► link-window -s %P -t $S:
                        picker panel: ⏎ ─► session.focus Q ─► Ok ─► ui.close ─► queue: Unsheet ─► unlink-window -t $S:@W
                        %session-window-changed $S → another tab ─► Unsheet
 Ctrl-a n           ─► moa:key new ─► command-prompt -b -t <client> -p "new session · folder:" -I "~/x/" "display-message -c <ctl> -l \"moa:prompt 7 %%%\""
                        moa:prompt 7 ~/x/kite ─► folder checks ─► second prompt "title:" ─► moa:prompt 8 … ─► session.open, told by notice
```

**Tech Stack:** Rust; the existing crates. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md`:
- §6.13: the picker [C17, U17], help, the key table, "Help says hold Shift to select text";
- §6.6: menus and prompts (`command-prompt -b … "display-message -c <ctl> -l 'moa:prompt %%'"`),
  and "never send waiting commands (`command-prompt` without `-b`, `display-popup`)";
- §6.1: "`moa pick` in a `display-popup`, when the user opens the popup picker";
- §7: rows "Ctrl-a key table, menu, prefix keys, help (2.6)" and "Picker (2.7)".

Roadmap: slice 5 item 5, and the 5c row of the 5a plan's split table. Parity sources (read
only, never run): `spike/moa-proto/picker.py` (all of it), `tmux.conf` lines 109–160,
`menu.tmux`, `help.txt`, and `docs/superpowers/research/1-inventory.md` §2.6–2.7 (R1).

**Builds on** 5b and before:
- From `moa_core`:
  - `panel::{View, PanelReg}`;
  - `cockpit::{run::{CockpitOp, queue, through_queue}, spawn::{spawn_cmds, panel_env, panel_argv}, scene::HIDDEN}`;
  - `ops::launch::{focus, focus_cmds, show}`, `ops::term::{Tell, told}`;
  - `tmux::{cmd::{Cmd, quote}, mailbox::{KeyAction, Mailbox}, link::notice}`;
  - `keys::run`, `publish::{Topic, PanelRow}`, `config::UiConfig`, `state::PanelLaunch`, `Deps.is_dir`.
- `moa_view::{panel::{Panel, Out}, keys::{actions, hint}, geometry::geometry, views::build, text::tilde}`.
- `moa_tui::run::{Loop, PanelOptions}`, `moa_tmux::conf::moa_tmux`.
- The testkit's `TempBrain`, `Terminal`, `wait_for`, `fake_claude`, and the e2e helpers in
  `crates/moa/tests/common` (`open`, `came_up`, `tmux`, `screen_of`, `select_row`, `moactl`).

## Calls for you to check

The most important come first. Each is a rule below (S-number) with its reason.

1. **The picker is a popup by default, as in the spike, and `Ctrl-a s` opens it as a full tab
   instead** (S8, S9). To swap them, put `picker = "tab"` under `[ui]` in `moa.toml`. Help
   (`Ctrl-a ?`) follows the same setting. The popup starts a small program each time it
   opens, and that's the cost you chose (U17). The full tab starts nothing after the first
   time.
2. **`Ctrl-a n` asks two questions, one at a time, in the status line:** `new session ·
   folder:` (prefilled with your brain's first folder), then `title:` (S5, S6).
   - A folder that doesn't exist is refused (`no folder ~/x`), and moa doesn't create it.
     The spike created missing folders.
   - The folder is checked right after you answer, before the title question.
   - The session opens with the brain's default account, at the top level.
3. **If a picked session can't be opened, the picker stays open and says why** (S7). The
   spike closed first and then flashed the reason for 4 seconds.
4. **A second terminal attached to moa sees the full-tab picker or help too, while it's
   open**, because both terminals share moa's tabs (S9). However you leave that tab, moa
   closes it again: a pick, Esc, or moving to another tab.
5. **Terminals (`⌨`) are listed in the picker by name only** (S6b). moa doesn't know where a
   shell has moved since, so it shows no folder for them.
6. **Help is written from moa's own key table and your layouts** (S4, S10), so it can't drift
   from what the keys do. It adds the spec's "hold Shift to select text".
7. **Two wording fixes from 5b** (S3):
   - a narrow panel shows `moa's layout · Ctrl-a r repairs` instead of a line cut off
     mid-word;
   - "moa couldn't read the terminal's state; try again" no longer quotes tmux. tmux's own
     words go to moa's log.

## PRs (roadmap P4)

Six PRs, each about 1500 lines of diff at most. Each one branches from `main` after the previous
one has merged. Each one gets:
- the full verify:
  - `cargo fmt --check`;
  - clippy, also with `--target x86_64-unknown-linux-gnu` for `moa-platform` and `moa-tmux`;
  - `cargo nextest run --workspace` three times;
  - `nix flake check`;
- the tmux 3.4 pass on the tmux-facing crates it touched;
- `/code-review medium --fix`, with the fixes pushed;
- green CI.

Bodies end with the try-it, when there is one. No "Restart needed" line.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-5c-1` | 1 | `v1 slice 5c (1): the cockpit queue in three files` |
| p2 | `v1/slice-5c-2` | 2–3 | `v1 slice 5c (2): 5b's small carries` |
| p3 | `v1/slice-5c-3` | 4–5 | `v1 slice 5c (3): one key table, and Ctrl-a n` |
| p4 | `v1/slice-5c-4` | 6–7 | `v1 slice 5c (4): terminals listed, and the picker's view` |
| p5 | `v1/slice-5c-5` | 8 | `v1 slice 5c (5): the popup picker` |
| p6 | `v1/slice-5c-6` | 9 | `v1 slice 5c (6): the full-tab picker` |
| p7 | `v1/slice-5c-7` | 10–11 | `v1 slice 5c (7): help, and 5c end to end` |

p1 is moved code only, about 1,400 diff lines because each moved line counts twice. Reviewers
read it with `git diff --color-moved=dimmed-zebra`, which shows moved blocks dimmed; `git diff
-M` pairs files, not blocks.

**Checked before this plan, on real tmux 3.4 and 3.8** (a throwaway server, scratchpad
`tmuxcheck.sh`):
- `link-window -s %P -t $S:` links at the next free index and selects it.
- Linking the same window into the same session again makes a second link, so the core
  selects instead (S9).
- `list-panes -a` lists a linked window's panes once per session that holds it.
- `unlink-window -t $S:@W` on the session's current window returns it to the window it was on
  before.
- `display-popup` accepts `-e`, `-T`, `-d`, `-w`, `-h` and `-E`, and `command-prompt` accepts
  `-b`, `-p` and `-I`.

**Checked by the plan review**, on 3.4 and 3.8 with an attached 80×24 terminal (scratchpad
`plan-5c-review.md` §5 has the commands and outputs):
- A popup wider or taller than the terminal is refused (`width too large`), and `-w`/`-h`
  take no formats. So help's popup picks its width with a nested `if -F` (S10).
- `display-popup` takes its command as several arguments, also on 3.4. It expands neither the
  command nor `-e` values, but **does** expand `-d`. A `display-menu` item's command is
  expanded before it runs. A popup gets no `TMUX_PANE`.
- `-I` splits on `,` and expands `#{…}`. Typed answers arrive verbatim, except 3.4's `\$`. An
  empty answer arrives as `moa:prompt N ` (a trailing space). Esc and Ctrl-c send nothing. A
  second `command-prompt` while one is open is silently ignored.
- `%session-window-changed` comes for a link that selects, for unlinking the current tab, and
  for `next-window` off it. Every unlink also sends `%window-close @W`, though the window lives
  on in `_moa`. Unlinking a link that's already gone fails with `can't find window`.
- `list-panes -a` lists `_moa` before `moa`, so a linked pane's last line is its `moa` one.
- `list-panes -a -f '#{@moa_term}' -F 'T #{pane_id} #{@moa_label}'` lists terminals with
  their names.
- A one-pane tab still draws the `pane-border-status` line.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| S1 | **`cockpit/run.rs` splits in three files, moved unchanged** (5b final review, Recommendation 2):<br>• `run.rs`, the queue: `LOST`, `REPLACED`, `REPLACED_GOTO`, `CockpitOp`, `Select`, `CockpitRun`, `Held`, `reset_op`, `tells_of`, `told_all`, `refused`, `current`, `title`, `in_cockpit`, `build_kind`, `refills`, `queue`, `through_queue`, `vacating`, `idle`, `apply`, `goto`, `select_view`, `pump`, `start`, `listing`, `lost_now`, `finish`, `awaits`, `drop_all`, `lost`, `reload`, `linked`, `stopped_listed`, `link_listed`, `client_gone`, `died`;<br>• `run/decide.rs`, planning: `Step`, `Then`, `Moved`, `Said`, `Next`, `wants_lazy`, `decide`, `went_to`, `build`, `act`, `layout_refused`;<br>• `run/answers.rs`, what answers do: `first_line`, `NO_ANSWER`, `plainly`, `vacate_refused`, `known_panel`, `vacate_plainly`, `chain_failed`, `scene_answered`, `spawn_answered`, `chain_answered`, `views_given`, `applied`.<br>Items gain `pub(super)` where the other files need them, and so do the fields of `Then`, `Moved` and `Said`, which `chain_answered` and `finish` read across files. `core.rs` calls `cockpit::run::{scene_answered, spawn_answered, chain_answered}`, so those three are `pub(crate)` in `answers.rs`, re-exported by `pub(crate) use answers::{…};` in `run.rs` (a `pub(super)` item can't be re-exported, E0364). Nothing else changes. `run/tests.rs` stays, with its `use`s adjusted. If the compiler forces an item into another file, the implementer moves it and says so in the report. | 5c adds two operations to the queue (S9). The file is already 1,351 lines across three jobs. The split's cut is the final review's, completed with the items it didn't place. |
| S2 | **One "home → ~" helper**, `moa_core::folders::tilde(path: &str, home: &Path) -> String`, in a new **public** module `moa_core::folders` (`ops` is private, so `moa-view` can't reach anything in it). It matches whole folders: `/h` gives `~`, `/h/` gives `~/`, `/h/x/` gives `~/x/`, `/hx` gives `/hx`, and an empty `home` changes nothing. The rest of the path is kept exactly, trailing `/` included. A `home` given with a trailing `/` is the same home. The three helpers it replaces:<br>• `ops/term.rs` `with_tilde` (tries `home`, then its canonical form, by calling `tilde` twice and taking the first that changed), and its caller in `ops/layout.rs`;<br>• `moa/src/ctl.rs` `with_tilde` (a `&Path`, so it goes through `to_string_lossy`);<br>• `moa-view/src/text.rs` `tilde` (already `(&str, &Path)`), which becomes a re-export, so `ask.rs`, `tree_keys.rs`, `tree.rs` and `short_path` compile unchanged. | p1 CR F6: the three disagreed on a trailing `/` and on home itself. The view's form wins, because `n`'s prefill needs `~/x/` to end in `/`, ready for typing. The term helper's canonical retry is kept, for macOS's `/private/var` homes. |
| S3 | **Two of 5b's wordings:**<br>• **A refusal that a listing couldn't be read** (`scene_answered`, both the plain and the vacate variant) says `moa couldn't read the terminal's state; try again`. tmux's first line goes to moa's log, as `plainly` does.<br>• **A footer message wider than the footer** is cut at a grapheme boundary and ends in `…`. The managed line has a short form, `moa's layout · Ctrl-a r repairs` (31 columns). The panel shows the long form when it fits the footer and the short one otherwise. | 5b final review #31 and #48: chain errors already speak plainly, and this listing reply was missed. A cockpit tree panel is often under 64 columns wide, and the cut-off line read `this panel is part of moa's lay`. |
| S4 | **`moa_core::keymap`, the one table of `Ctrl-a` keys:**<br>• `Binding { key: &'static str, label: &'static str, help: &'static str, does: Does }`;<br>• `Does` is `Moa(KeyAction)` (through the mailbox), `Popup(View)` (Task 8), `SplitBeside`, `SplitBelow`, `BackToMoa`, `Zoom`, `PrevTab`, `NextTab`, `SendCtrlA`, or `Move` (a greyed note in the menu).<br>• `pub fn ctrl_a(form: Form) -> Vec<Vec<Binding>>` returns the menu's groups, in the menu's order. A separator goes between groups. Task 4 adds it with no `form` (there's only one form then); Task 8 adds the parameter.<br>• `conf::menu()` and the daemon-key lines are written from it. The template's plain-tmux bindings (`\| - m z [ ] a`, the arrows and `h j k l`) and the status line's key hint stay literal in `moa.tmux.tpl`. A test loads the generated file into a throwaway tmux server and checks, from `list-keys -T moa`, that every key in the table is bound; another checks that every key the status hint names is in the table.<br>• **The menu gains `Find a session, full tab…` on `s`.** `s` is bound today but missing from the menu, and the spec wants the menu to offer both forms. That one menu item is the only change to the `moa.tmux` snapshots in Task 4.<br>• **`label`** is the menu's text, as today. **`help`** is the help view's text, from the spike's `help.txt`. For example, `/`'s is `find a session: type part of its name, Enter jumps`. | Roadmap 5.1 and research 6 ("the `Action` enum and the default keymap table"); the 5b carry "a keymap table". Help (S10) must not drift from the bindings. Keeping the template's literal lines keeps Task 4 nearly a pure refactor: the snapshots change by one menu item. |
| S5 | **Prompts** (slice 3's carries):<br>• **`Cmd::prompt(client: &ClientName, label: &str, prefill: &str, ctl: &ClientName, token: u64) -> Cmd`** is the only way to build a `command-prompt`. It builds `command-prompt -b -t <client> -p <label> -I <prefill> "display-message -c <ctl> -l \"moa:prompt <token> %%%\""`. The allow-list accepts `command-prompt` only in exactly that shape: flags `-b`, `-t`, `-p` and `-I` before one template, and the template matching `display-message -c <name> -l "moa:prompt <digits> %%%"`. A `-b` given as another flag's value doesn't count.<br>• **`\$` back to `$`** happens only when the link's tmux version is 3.4, the `#{m:3.4*}`/`next-3.4*` rule `moa.tmux.tpl` already uses. It moves out of `Mailbox::parse`, which now keeps the raw text, into `prompt::answered`. On 3.5 and later, a typed `\$` stays as typed.<br>• **`State.prompts`:** a map from token to what it waits for. At most 16 entries, and the oldest is dropped. A client's older prompt is **not** removed when it asks again, because tmux ignores a second `command-prompt` while one is open, so the answer that comes may be the older token's. The map is cleared when the link reconnects, since the templates name the old control client. An answer whose token isn't waiting is logged, as today. Esc in tmux's prompt sends nothing, so that entry just waits until it's dropped.<br>• **Answers are trimmed** of trailing spaces: an empty answer arrives as `moa:prompt N ` with a trailing space.<br>• **A prefill holding `,` or `#`** isn't used; the prompt is prefilled `~/`. | Spec §6.6. A template built by hand could miss `-b` and freeze the control client. The `$` quirk is 3.4's alone (slice 3 T17; checked again by the plan review), so unescaping it on 3.8 would change what the user typed. `-I` splits on `,` and expands `#{…}` (checked). |
| S6 | **`Ctrl-a n`** (`KeyAction::New`):<br>1. The core asks `new session · folder:`, prefilled with the brain's first folder, as `tilde` with a trailing `/`, else `~/`.<br>2. The answer is trimmed. `typed_folder` checks it: it must start with `~` or `/` (`a folder starts with ~ or /`), and it must exist (`no folder <typed>`). A refusal is told by notice, and that ends it.<br>3. Then the core asks `title:`. An empty answer means no title.<br>4. Then the core opens the session as `moactl open <folder> --title <title>` would: the same path through `ops/launch.rs`'s `open`, with the brain's default account (the `brain` row's `open_account`), at the top level, and `focus: true` (as `moactl open` without `--bg`). Its answer (`opened …`, or open's own refusal, including a tab tmux refused) is told by notice.<br>**An open's answer goes through `Tell`:** `Launching.reply` (`tmux/link.rs`) becomes `Option<Tell>`, through `opened`, `lost_opens` and the `client_gone` retain, so a key-started open (no connection, no request) is told by `Tell::Notice(client)`.<br>**`ask.rs`'s `folder(typed, ctx)`** becomes `moa_core::folders::typed_folder(typed: &str, home: &Path, is_dir: impl Fn(&Path) -> bool) -> Result<PathBuf, String>` (S2's public module), and the tree's `n` calls it from there. | Spec key table: "`n` new session (prompt folder, title; prefill the brain's first folder)". One question at a time lets the folder be checked before the title is asked (5a Call 6). tmux's `%%` can't split two answers that hold spaces. A typo never leaves a folder behind. |
| S7 | **The picker** (`moa-view`, `views/picker.rs`), from the spike's `picker.py`:<br>• **Rows, in order:**<br>&nbsp;&nbsp;1. Live sessions at home, by display state in the status view's order (waiting, question, failed, done, working, quiet, idle). Within a state, the most recent `activity_at` comes first.<br>&nbsp;&nbsp;2. Terminals, in the `terms` topic's order (S6b).<br>&nbsp;&nbsp;3. The rest: asleep, ended, and any session whose `location` is elsewhere, by `last_seen_at`, newest first, at most 300. Archived sessions never appear.<br>• **Words:** waiting `needs you`, question `asks you`, failed `failed`, done `done`, working `working`, quiet `quiet`, idle `idle`, asleep `asleep`, ended `ended`, elsewhere `elsewhere`, a terminal `terminal`. Glyphs are the status view's, animated as there, with `⌨` for a terminal and `◌` for elsewhere.<br>• **Match:** the query is lowercased and split on spaces. Each term must be a subsequence of the row's text: title, group path, word, the folder's last part, handle, and the id's first 8 characters, lowercased and joined by spaces. A row's rank is the number of terms that aren't contiguous substrings. Rows sort by rank, then by their place in the order above. An empty query shows every row in order.<br>• **Highlights:** in the title and group only, each term's substring hit if it has one, else its first subsequence positions, in the new tone `Hit` (yellow, bold, underlined).<br>• **Screen:**<br>&nbsp;&nbsp;– line 0: `› ` (head tone) and the query, with the cursor after it; right-aligned and dim, `<shown>/<rows> · <live> live`;<br>&nbsp;&nbsp;– line 1: a dim rule;<br>&nbsp;&nbsp;– then the rows: ` <glyph> <title><group><word><age>  <why>`, where `title` is `clamp(w/4, 10, 30)` wide, `group` is `clamp(w/6, 0, 24)` wide (and absent under 70 columns), `word` is 10 wide, `age` is right-aligned in 4, and `why` is the session's `why`, in display width by grapheme (V9);<br>&nbsp;&nbsp;– the title falls back to the project's name, then the folder's last part, then the id's first 8 characters; the group falls back to the project's name (the spike's `grp or project`);<br>&nbsp;&nbsp;– age is from `activity_at` for a live row and from `last_seen_at` for an old one, in the views' usual age format; a terminal row has none;<br>&nbsp;&nbsp;– a query longer than its line shows its tail;<br>&nbsp;&nbsp;– `no session matches` (dim) when nothing matches;<br>&nbsp;&nbsp;– footer hint `↵ jump · ↑↓ move · esc close`.<br>• **Keys:**<br>&nbsp;&nbsp;– Enter picks;<br>&nbsp;&nbsp;– Esc, Ctrl-c and Ctrl-g close;<br>&nbsp;&nbsp;– ↑, Ctrl-p and Shift-Tab go up; ↓, Ctrl-n and Tab go down;<br>&nbsp;&nbsp;– PgUp/PgDn go by the body's height;<br>&nbsp;&nbsp;– Backspace deletes one character, Ctrl-u clears the query, and Ctrl-w deletes the last word;<br>&nbsp;&nbsp;– any other printable character, a pasted text (newlines become spaces), `j`, `k` and `q` included, goes into the query;<br>&nbsp;&nbsp;– every edit puts the selection back on the first row, and the selection stops at the ends rather than wrapping.<br>&nbsp;&nbsp;The wheel and a click work as in every view (V11). A click picks.<br>• **A pick** sends `session.focus {q: id}`, or `term.show {pane}` for a terminal, and the footer says `opening <title>…`. An `Ok` reply makes the panel close itself (`Out::Close`). An `Err` stays in the footer, with the picker still open. Enter with no rows closes, as the spike's did.<br>• **The query** lives in the panel's own state for the picker view, beside its list state. | Spec §6.13 Picker ("same content as the spike"), R1 §2.7. v1's display states have `failed` and `quiet`, which the spike lacked. Keeping the picker open on a refusal shows the reason where you're looking, and you can pick again. |
| S6b | **Terminals become a topic, `terms`:** rows `{key: "%N", name}`, where `name` is the pane's `@moa_label` without its leading `⌨ `. `LIST_PANES` carries neither tag, and its last field is already free text (`@moa_view`), so a **fourth refresh query** lists them: `list-panes -a -f '#{@moa_term}' -F 'T #{pane_id} #{@moa_label}'` (checked on 3.4 and 3.8). The topology keeps its answer, and `term_opened`'s tag write updates it at once. The topic is published when that set or a name changes. Every existing `P` fixture stays as it is. New op **`term.show {pane}`**: `focus_cmds(pane)` when the pane is a terminal in the topology, answered `went to <name>`; otherwise `that terminal is gone`. It saves nothing, so it sends its own `Effect::Tmux`. | R1 §2.7: "open plain terminals (⌨)" are listed, and picking one selects its tab. Terminals live only in tmux tags today, and a panel never runs tmux. |
| S8 | **The popup form:**<br>• **`[ui] picker`**: `"popup"` (the default) or `"tab"`. Anything else gives `picker in [ui] must be "popup" or "tab"`. With `"popup"`, `/` is the popup and `s` the tab; with `"tab"`, they swap.<br>• **The binding is static**, in `moa.tmux`, guarded like the daemon keys: `if -F '#{@moa_ctl}' { display-popup -E -w 80% -h 70% -d <home> -T "#[fg=#88c0d0 bold] ⌕ find a session " -e …panel_env… <exe> panel picker --once --brain <b> } { display-message -d 4000 "moa isn't running: run moa" }`. `moa_tmux(brain, ui: &UiConfig, launch: &PanelLaunch)` gets the program and the environment that panels get (`MOA_NO_AUTOSTART=1` included) from one pure function, `cockpit::spawn::panel_env_of(brain: &str, launch: &PanelLaunch) -> Vec<String>`, which `spawn_cmds` uses too (lesson 5: one source for a panel's environment).<br>• **Quoting:**<br>&nbsp;&nbsp;– the program and its arguments go as separate arguments (checked on 3.4);<br>&nbsp;&nbsp;– `-d`'s folder goes through `literal_format`, because tmux expands `-d`;<br>&nbsp;&nbsp;– the menu's copy of the popup command doubles every `#` in the path, the `-e` values and `-d`, because a menu item's command is expanded before it runs.<br>• **`moa pick [--brain B]`** is `moa panel picker --once`. `pick` joins `RESERVED_BRAIN_NAMES`.<br>• **`--once`** works only for `picker` and `help` (`--once works only for the picker and help`). Such a panel:<br>&nbsp;&nbsp;– never claims a pane (`pane: None`, whatever `TMUX_PANE` says; tmux gives a popup none anyway);<br>&nbsp;&nbsp;– is never managed, whatever `MOA_NO_AUTOSTART` says: `placed()` would otherwise make it a managed sheet with no row, drawing nothing;<br>&nbsp;&nbsp;– subscribes only to what its view needs: `sessions`, `groups`, `projects`, `terms`, `brain` for the picker, and `brain`, `layouts` for help;<br>&nbsp;&nbsp;– registers with `ui.panel {once: true}` (a new field, `#[serde(default)]`);<br>&nbsp;&nbsp;– draws no frame of its own (`geometry(view, framed = false, …)`), since tmux frames the popup;<br>&nbsp;&nbsp;– exits 0 on `Out::Close`;<br>&nbsp;&nbsp;– with moa down, shows `moa isn't running` and exits on any key;<br>&nbsp;&nbsp;– writes `panel.log`'s start and end lines like any panel, with the view written `picker (popup)`.<br>• **The lost-exit nudge:** when a `once` connection goes (`ClientGone`) and the link's tmux is 3.4 or 3.5, the core sends the allow-listed `run-shell true` once. Ubuntu's builds of those versions can lose a child's exit (5b). A popup closes only when tmux reaps `moa pick`, and a pick that resumes a session opens a tab at that very moment.<br>• **The menu** has `Find a session…` on `/`, which opens the configured form, and `Find a session, full tab…` or `Find a session, popup…` on `s`. | Spec U17, C17, §6.1 ("`moa pick` in a `display-popup`"), §6.6 (never send `display-popup`: a binding isn't sent by moa, tmux runs it). The environment goes explicitly with `-e` (lesson 5). A claimed pane would take over the registration of the real pane underneath the popup. |
| S9 | **The full-tab form: sheets linked into your session.**<br>• **`KeyAction::Find`** (word `find`; `find-other` goes) queues `CockpitOp::Sheet { view: View::Picker, session: TmuxSessionId, client: ClientName }`. **`KeyAction::Help`** does the same with `View::Help`. A sheet starts from a fresh listing, as every queued operation does (L17). Then:<br>&nbsp;&nbsp;1. With no panel tagged with the view, it spawns one (`spawn_cmds`, tagged with `panel_tags`, so its frame is on and tmux draws no label pill above it) and lists again. A failed spawn tells `couldn't start the picker; see panel.log` (or `help`). A dead one is restarted in place (`respawn-pane -k`, as Repair does), never left parked beside a new one.<br>&nbsp;&nbsp;2. With the panel's window already in session `$S`, it sends `select-window -t $S:@W`; otherwise `link-window -s %P -t $S:`.<br>&nbsp;&nbsp;3. `State.sheets: BTreeMap<PaneId, i64>` records the open by **pane**. The value is the open's wall-clock milliseconds, so it never repeats across moa restarts. `panels` is published.<br>&nbsp;&nbsp;A key from `_moa`'s session tells `open moa's tab first`.<br>• **`PanelRow.sheet: Option<i64>`** (`#[serde(default)]`) is derived from `State.sheets` by the row's pane, both when the row is published and when the panel registers. A panel spawned by the first `Sheet` connects after the link, and still reads its open.<br>• **`ui.close`** (new op, no parameters) from a sheet panel's connection queues `CockpitOp::Unsheet { pane }`. From any other connection: `only the picker and help close this way`.<br>• **`Unsheet` decides from its own fresh listing:** it sends `unlink-window -t $S:@W` for each session other than `_moa` that holds the window. With none, it sends nothing and tells nothing (a second unlink would fail with `can't find window`). It removes the `State.sheets` entry and publishes. A waiting `Sheet` or `Unsheet` for the same pane is replaced by a newer one of either kind, answered with nothing (keys have no reply).<br>• **Leaving the tab:** a `%session-window-changed $S @X`, for a session that holds an open sheet's window, with `@X` not that window, queues `Unsheet`. A pick therefore causes two unsheets (this one, then `ui.close`'s), and the second finds nothing to unlink.<br>• **A detach:** when `%client-detached` leaves no user client on a session that holds an open sheet, the core queues `Unsheet`, so you never re-attach into a stale picker.<br>• **Stale links:** a listing (a scene's, or the link's) that shows a sheet's pane in a session other than `_moa` while `State.sheets` records no open for it queues `Unsheet`. That happens after a daemon restart.<br>• **Every unlink also sends `%window-close @W`,** though the window lives on in `_moa`. Nothing may read it as "that panel is gone"; a test pins it.<br>• **In the panel:**<br>&nbsp;&nbsp;– a sheet panel moa placed in a pane it claimed (`managed`, with a pane) whose row says `None` draws nothing, never animates, and sets no deadline;<br>&nbsp;&nbsp;– a picker or help you start yourself with `moa panel`, and every `--once` popup, always draws; its `Out::Close` quits it, as Ctrl-c quits any panel you started;<br>&nbsp;&nbsp;– when the number changes, the picker clears its query and selects the first row, and help scrolls to the top;<br>&nbsp;&nbsp;– a placed sheet's `Out::Close` sends `ui.close`;<br>&nbsp;&nbsp;– in a sheet, Ctrl-c closes. It never shows the managed line.<br>• **The topology knows every session a window is in:** `window_sessions` becomes `BTreeMap<WindowId, BTreeSet<TmuxSessionId>>`, with `sessions_of(window)`. `session_name_of` prefers `_moa` when it's one of them, so a scene reads a linked sheet as `Hidden`, where it lives. (`list-panes -a` lists `_moa` first, so "the last line wins", as today, would read it as `moa`.)<br>• **The engine leaves sheets alone:** a picker or help panel is never reused, retagged, moved or parked by a build.<br>• **Allow-list:** `link-window -s %N -t $N:` exactly, `unlink-window -t $N:@N` exactly, and `select-window -t $N:@N` beside the existing `-t %N`.<br>• **Cost:** each `Sheet` or `Unsheet` causes one extra full listing (`%window-add`/`%window-close` lead to a refresh). | Spec C17: "link a parked picker panel's window into the user's session and select it; … the window is unlinked afterwards. Two tmux commands, zero launches." Checked on 3.4 and 3.8 (above). The open is recorded by pane because the first open spawns the panel, which registers only after the link. A tab you left with `Ctrl-a ]`, or detached from, must not stay behind as a "picker" tab. |
| S10 | **Help** (`views/help.rs`), lines that aren't selectable and scroll as activity's do (V17). In order:<br>1. `Ctrl-a keys`: every binding in `keymap::ctrl_a(form)`, as `key  help`, with the brain's form read from the `brain` row's new `picker` field;<br>2. `tmux's prefix (Ctrl-b unless you changed it)`: `Space menu · m back to moa · C repair`;<br>3. `In every panel`: `j/k ↑↓ move · g/G top/bottom · PgUp/PgDn · ⏎ or a click acts · the wheel scrolls · Ctrl-c cancels a prompt` and `hold Shift to select text`;<br>4. one line per view, `<label>: <keys::hint(view, false)>`, for tree, plan, status, today, projects, activity and picture;<br>5. `Layouts`: each `layouts` row, `<n> <name>: <leaves joined by " · ">`, with `▸` on the current one, then `change one: moa layout set NAME JSON`;<br>6. `Agent states`: each display state's glyph and word, as the status view draws them.<br>The console's commands (R1 §2.6) join help with the console, in slice 6.<br>Keys: the moving keys, and `q`, Esc and Ctrl-c, which close. Hint `j/k scroll · q close`.<br>**The popup's width:** tmux refuses a popup wider than the terminal (`width too large`), and `-w` takes no format. So the binding nests a check: `if -F '#{@moa_ctl}' { if -F '#{e\|<:#{client_width},84}' { display-popup -E -w 100% -h 90% … } { display-popup -E -w 84 -h 90% … } } { display-message … }` (checked on 3.4 and 3.8), with `panel help --once` and title `#[fg=#88c0d0 bold] keys and help `. The menu's `?` item is the same.<br>The full tab is S9's. The panel's `TOPICS` gain `Layouts` and `Terms`. `View::label`: Picker `find a session`, Help `keys and help`. | Spec §6.13 ("Help (`Ctrl-a ?`) uses the same mechanism with the help view instead of `less`"; "Help says hold Shift to select text"), R1 §2.6. Every section is written from a table moa already has, so none can drift. The spike's 84 columns fit help's widest line. The user's tmux prefix may not be Ctrl-b. |
| S11 | **Frames:** `View::framed()` is true for picker, help and setup too. `geometry(view, framed: bool, size)` lays out the picture as before; anything with `framed == false` like the empty slot; and the rest framed. Task 3 passes `view.framed()`, and Task 8 adds `&& !once`. | 5b Task 9 M4: geometry special-cased `EmptySlot` rather than asking `framed()`. That is why picker, help and setup were drawn framed while `framed()` said no. |

## Parity rows this part delivers (spec §7)

- "Ctrl-a key table, menu, prefix keys, help (2.6)". The key table and menu already exist;
  help becomes a view, with no `less` and no `run-shell`.
- "Picker (2.7)", in both forms.
- The spike's `n` command-prompt, without the spike's bug: it put the answers into single
  quotes, so a `'` broke the command.

## Global Constraints

The roadmap's, every earlier slice plan's, and 5a's and 5b's Global Constraints apply. Also:

- **cargo only through** `nix develop --command cargo …`; tests with `cargo nextest run`.
- **Never run cargo while an e2e test runs.** Relinking `target/debug/moa` breaks the running
  tests. Run a test binary directly, or use another `CARGO_TARGET_DIR`.
- **moa never sends `display-popup`,** and never sends `command-prompt` except through
  `Cmd::prompt` (with `-b`). The popup is a static binding that tmux runs.
- **Zero launches** for opening, picking in, and closing the full-tab picker or help once its
  panel exists:
  - neither `status.launches` (`moa_platform::spawn::launches()`) nor `panel_starts` changes;
  - the sheet panel's `pane_pid` is the same before and after.
- **The popup form is one `moa panel … --once` per open,** started by tmux, never by moa's
  `Spawner`. Each one writes exactly one start line and one end line in `panel.log`.
- **tmux targets are pane ids** (`%N`), the exact sessions `=moa:` and `=_moa:`, or, for the
  sheet commands only, a session id with a window id (`$N:@N`) or a session id alone
  (`$N:`). Never a name.
- **The core stays pure:** file reads only through `Deps`, writes only through
  `Effect::WriteFile`. Ops that save nothing send their own `Effect::Tmux`.
- **Tests:**
  - only under `/tmp` roots (`TempBrain`, `Sandbox`), with `HOME` there too;
  - never a tmux server named `moa` or `moa-work`, never a real brain;
  - `fake-claude` is the only agent;
  - every wait goes through `wait_for` with a 10 s limit, never a sleep;
  - a test without a daemon that waits for a stopped pane nudges with `run-shell true` once
    `pane_dead` is 1 (5b's Linux tmux 3.4 note in `crates/moa-tmux/tests/conf.rs`).
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b`, `~/private`.
- **Plain words** in every message, prompt and help line: "tab", "folder", "panel",
  "layout", never "pane", "window" or "session window" (lesson 35). Help's
  `tmux's prefix (Ctrl-b unless you changed it)` is the one place tmux is named, because the
  user's own prefix key is tmux's.

## Review Focus

1. **A layout switch, a focus, or a Repair while the full-tab picker is linked.** tmux lists
   the picker's pane twice. Expected:
   - the build moves, retags and parks nothing of the picker;
   - the picker stays where it is, and closes normally afterwards.

   Pinned by `a_linked_sheet_is_left_alone` (Task 9, a scene with the pane listed in `moa`
   and `_moa`) and `a_layout_switch_with_the_picker_open_keeps_it` (Task 11).
2. **Leaving a full-tab sheet without Esc.** The cases: `Ctrl-a ]`, a click on another tab, a
   focus from another terminal, a detach, a daemon restart while it's linked. Expected: no
   "picker" or "help" tab is left in the user's session, and no unlink is sent twice. Pinned
   by `leaving_the_sheet_tab_unlinks_it`, `detaching_unlinks_the_sheet`,
   `a_listing_unlinks_a_stale_sheet` and `a_pick_unsheets_once` (Task 9), and
   `leaving_the_picker_tab_closes_it` (Task 11).
3. **`Ctrl-a n` answers with quotes, `$`, `#`, `;`, `%`, spaces, an empty answer, a folder that
   doesn't exist, or a prefill with a comma.** Expected:
   - exactly the typed folder and title are used;
   - a missing folder is refused in plain words, and nothing is created;
   - a `\$` stays `\$` on 3.8.

   Pinned by `prompt_answers_round_trip` (Task 5, on real tmux 3.4 and 3.8),
   `a_missing_folder_is_refused_and_not_made` (Task 5), and `ctrl_a_n_opens_a_session`
   (Task 11).
4. **The picker under odd data:** no sessions; 1,000 sessions; CJK, emoji, ZWJ and RTL titles;
   a query with `*`, `\` or `[`; a session that's gone between the selection and Enter.
   Expected:
   - no panic and no line wider than the panel;
   - at most 300 old rows;
   - a gone session's refusal in the footer, with the picker still open.

   Pinned by `picker_never_panics_or_overflows` (Task 7, proptest) and
   `a_pick_that_fails_keeps_the_picker_open` (Task 7).
5. **The popup on Ubuntu's tmux 3.4, which loses child exits (5b), and on a small
   terminal.** Expected:
   - the popup closes when `moa pick` exits, also after a pick that resumes an asleep session
     (a new tab opening at the same moment);
   - help's popup opens on an 80×24 terminal.

   Pinned by `a_once_connection_going_nudges_on_3_4` (Task 8), and by
   `the_popup_picker_finds_and_focuses` and `help_opens_on_a_small_terminal` (Task 11). The
   Task 11 tests must pass 10/10 in CI's Ubuntu job, whose tmux has the bug; this Mac's nix
   tmux doesn't.

---

## Task 1: the cockpit queue in three files

**Files:**
- Create: `crates/moa-core/src/cockpit/run/decide.rs`, `crates/moa-core/src/cockpit/run/answers.rs`
- Modify: `crates/moa-core/src/cockpit/run.rs` (remove the moved items; `mod decide; mod answers;`), `crates/moa-core/src/cockpit/run/tests.rs` (`use`s only)

**Interfaces:**
- Moves only (S1). Callers outside `cockpit::run` (`core.rs`, `keys.rs`, `ops/launch.rs`,
  `state.rs`, `sleep.rs`, `reconcile.rs`, `panel.rs`) compile unchanged.

- [ ] **Step 1: Move the items, unchanged.** The only edits are `pub(super)`, `use`, and the
  two `mod` lines. Check with `git diff -M --stat` and `git diff --color-moved=dimmed-zebra`:
  every body is a moved block.
- [ ] **Step 2: Run** `nix develop --command cargo nextest run --workspace` → all pass, with the
  same count as before.
- [ ] **Step 3: Commit:** `v1: the cockpit queue splits into run, decide and answers`.

## Task 2: one "home → ~", and a listing refusal in plain words

**Files:**
- Create: `crates/moa-core/src/folders.rs` (`pub mod folders;` in `lib.rs`)
- Modify: `crates/moa-core/src/ops/term.rs`, `crates/moa-core/src/ops/layout.rs`,
  `crates/moa/src/ctl.rs`, `crates/moa-view/src/text.rs`,
  `crates/moa-core/src/cockpit/run/answers.rs` (`scene_answered`)
- Test: in `folders.rs`; `crates/moa-core/src/cockpit/run/tests.rs`

**Interfaces:**
- Produces: `moa_core::folders::tilde(path: &str, home: &Path) -> String` (S2).
  `moa_view::text::tilde` re-exports it.

- [ ] **Step 1: Write the failing tests.**
  - `tilde_matches_whole_folders`, with home `/h`:
    - `/h` → `~`, `/h/` → `~/`, `/h/x/` → `~/x/`, `/h/x` → `~/x`;
    - `/hx` → `/hx`, `/other/h` → `/other/h`;
    - with home `""`, `/h/x` stays `/h/x`;
    - with home `/h/`, `/h/x` → `~/x`.
  - `term_tries_the_canonical_home`: `/private/h/x` with home `/h`, whose canonical form is
    `/private/h`, gives `~/x`.
  - `an_unreadable_listing_says_so_plainly`: a scene answer `Err("server exited unexpectedly")`.
    - The request gets `moa couldn't read the terminal's state; try again`.
    - The log has a line holding `server exited unexpectedly`.
    - The vacate variant is the same, with its own text and no tmux words.
- [ ] **Step 2: Run them** → they fail: `tilde` is missing, and the reply quotes tmux.
- [ ] **Step 3: Implement** S2 and S3's first bullet. Delete the two old `with_tilde`s, and
  make the view's `tilde` the re-export. Existing tests that pinned the old outputs change
  only where S2 says the output changes; say which ones in the report.
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass.
- [ ] **Step 5: Commit:** `v1: one home-to-~ helper, and a listing refusal without tmux's words`.

## Task 3: the frame rule, and footer messages that fit

**Files:**
- Modify: `crates/moa-core/src/panel.rs` (`framed`), `crates/moa-view/src/geometry.rs`,
  `crates/moa-view/src/panel/mod.rs` (`MANAGED`, `footer`), every caller of `geometry`
- Test: `crates/moa-view/src/geometry.rs` tests, `crates/moa-view/tests/` (the panel's tests), the
  `moa-tui` snapshots

**Interfaces:**
- Produces: `geometry(view: View, framed: bool, size: Size) -> Geometry` (S11).
  `const MANAGED_SHORT: &str = "moa's layout · Ctrl-a r repairs"`.

- [ ] **Step 1: Write the failing tests.**
  - `unframed_views_share_the_empty_slots_layout`: `geometry(View::Picker, false, 80×24)`
    equals `geometry(View::EmptySlot, false, 80×24)`, and is not framed.
  - `framed_follows_the_view`: `View::Picker.framed()`, `View::Help.framed()` and
    `View::Setup.framed()` are true; `View::EmptySlot.framed()` and `View::Picture.framed()`
    are false.
  - `the_managed_line_fits_a_narrow_panel`: a managed tree panel 40 columns wide; Ctrl-c
    shows `moa's layout · Ctrl-a r repairs` in the footer. At 80 columns it shows the long
    line.
  - `a_long_footer_message_ends_in_an_ellipsis`: a message of 50 CJK characters in a
    30-column panel. The footer is at most the footer's width, ends in `…`, and splits no
    wide character.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S11 and S3's second bullet.** The `moa-tui` snapshots must not change.
  Picker, help and setup were drawn framed before, and still are.
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass, with no
  snapshot changes.
- [ ] **Step 5: Commit:** `v1: frames follow View::framed, and footer messages fit`.

## Task 4: one key table

**Files:**
- Create: `crates/moa-core/src/keymap.rs` (`pub mod keymap;`)
- Modify: `crates/moa-tmux/src/conf.rs` (`menu`, the daemon-key lines)
- Test: `crates/moa-core/src/keymap.rs` tests, `crates/moa-tmux/tests/conf.rs`

**Interfaces:**
- Produces: `keymap::{Binding, Does, ctrl_a}` (S4), with `pub fn ctrl_a() -> Vec<Vec<Binding>>`
  for now. Task 8 adds the `form` parameter.

- [ ] **Step 1: Write the failing tests.**
  - `every_table_key_is_bound` (`moa-tmux/tests/conf.rs`, on a throwaway tmux server): the
    generated file is sourced, and `list-keys -T moa` shows a binding for every `Binding` with
    a key. Parse tmux's own listing, not the file's text: the template spells `[` bare, while
    `quote("[")` gives `'['`.
  - `the_status_hint_names_only_table_keys`: each key in the status-left hint (`/ c t p g $ z
    Space ?` and the arrows) is a table key, or one of `Space` and the arrows. Read the keys
    from the template text.
  - `every_binding_has_help`: no empty `help`, and no `help` with the words "pane" or
    "window".
- [ ] **Step 2: Run them** → they fail (no `keymap`).
- [ ] **Step 3: Implement S4.** The `menu()` items and the daemon-key loop read the table, in
  the table's order. `help` texts come from the spike's `help.txt`, in plain words.
- [ ] **Step 4: Run** `nix develop --command cargo nextest run -p moa-core -p moa-tmux`. Expected:
  - all pass;
  - **`moa_tmux_default` and `moa_tmux_work` change by one hunk each:** the menu's new
    `Find a session, full tab…` item on `s` (S4). Nothing else in them moves.
- [ ] **Step 5: Commit:** `v1: the Ctrl-a keys come from one table`.

## Task 5: prompts, and `Ctrl-a n`

**Files:**
- Create: `crates/moa-core/src/prompt.rs`
- Modify: `crates/moa-core/src/tmux/cmd.rs` (`Cmd::prompt`, the `command-prompt` check),
  `crates/moa-core/src/tmux/mailbox.rs` (no unescape; `New`'s `not_yet` goes), `crates/moa-core/src/keys.rs`,
  `crates/moa-core/src/core.rs` (`Mailbox::Prompt`), `crates/moa-core/src/state.rs` (`prompts`,
  cleared on reconnect), `crates/moa-core/src/folders.rs` (`typed_folder`, moved from
  `ask.rs`'s `folder`), `crates/moa-core/src/ops/launch.rs` and `crates/moa-core/src/tmux/link.rs`
  (`Launching.reply: Option<Tell>`, through `opened`, `lost_opens` and the `client_gone`
  retain), `crates/moa-view/src/panel/ask.rs` (uses `typed_folder`)
- Test: `crates/moa-core/tests/prompt.rs`, `crates/moa-core/tests/tmux_vocab.rs`,
  `crates/moa-tmux/tests/client.rs`

**Interfaces:**
- Consumes: `tilde` (Task 2), `Tell::Notice`, `notice`.
- Produces:
  - `Cmd::prompt(client: &ClientName, label: &str, prefill: &str, ctl: &ClientName, token: u64) -> Cmd`;
  - `prompt::{new_session(state: &mut State, client: ClientName) -> Vec<Effect>, answered(state: &mut State, now: &Now, token: u64, text: &str) -> Vec<Effect>}`;
  - `moa_core::folders::typed_folder(typed: &str, home: &Path, is_dir: impl Fn(&Path) -> bool) -> Result<PathBuf, String>`;
  - `Launching.reply: Option<Tell>`.

- [ ] **Step 1: Write the failing tests.**
  - `tmux_vocab.rs`:
    - `only_moas_prompt_shape_is_allowed`: `Cmd::prompt(…)` passes the check;
    - these are refused:
      - `command-prompt -p x 'display-message …'` (no `-b`);
      - `command-prompt -p -b 'x'` (`-b` as a value);
      - two templates;
      - a template other than `display-message -c <name> -l "moa:prompt <digits> %%%"`.
  - `prompt.rs` (core, fake link):
    - `ctrl_a_n_asks_the_folder_with_the_brains_first_folder`: the key `new` from client `/dev/pts/3`. One `Effect::Tmux` holds `Cmd::prompt(/dev/pts/3, "new session · folder:", "~/private/", ctl, t)`, with the brain's first folder at `$HOME/private`.
    - `a_missing_folder_is_refused_and_not_made`: the answer `~/nope`. The notice is `moa: no folder ~/nope`, no second prompt is sent, and no `open` runs.
    - `a_folder_needs_tilde_or_slash`: the answer `private`. The notice is `moa: a folder starts with ~ or /`.
    - `the_title_comes_second_and_opens`: the answers `~/private` and then `kite work`. That is one `session.open` with the folder `$HOME/private`, the title `kite work`, and account `acct-a` (the brain's `open_account`). Its answer is told by notice.
    - `an_empty_title_opens_without_one`.
    - `a_second_prompt_keeps_the_first_token`: two `Ctrl-a n` before any answer. tmux shows only the first prompt, so an answer to the first token is still acted on.
    - `seventeen_prompts_drop_the_oldest`, and `a_reconnect_clears_the_prompts`.
    - `an_empty_answer_with_tmuxs_trailing_space_is_empty`: `moa:prompt N ` as the title gives no title.
    - `a_key_started_open_that_tmux_refuses_is_told_by_notice`: the open's `new-window` answer is an error, so the client gets open's refusal as a notice, and nothing waits for a reply.
    - `dollar_unescapes_only_on_3_4`: `\$HOME` as a title answer. On link version `3.4` it becomes `$HOME`; on `3.8` it stays `\$HOME`.
    - `a_prefill_with_a_comma_falls_back`: a first folder `~/a,b`, so the prefill is `~/`.
  - `client.rs` (real tmux, 3.4 and 3.8): `prompt_answers_round_trip`. A pty client answers a `Cmd::prompt` with each of `it's "a" $HOME`, `a;b`, `100%`, `#{pane_id}`, `ü 😀`. The `Mailbox::Prompt` text equals the typed text, with 3.4's `\$` handled per `dollar_unescapes_only_on_3_4`.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S5 and S6.** The tree's `n` keeps its texts, now through
  `typed_folder`. `KeyAction::New`'s `not_yet` becomes `None`.
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass. Also run
  scratchpad `t34-all.sh` (tmux 3.4) → `client.rs` passes there.
- [ ] **Step 5: Commit:** `v1: Ctrl-a n asks for a folder and a title, one prompt at a time`.

## Task 6: terminals as a topic, and `term.show`

**Files:**
- Modify: `crates/moa-core/src/publish.rs` (`Topic::Terms`, `TermRow`), the topology's listing
  and tag paths (publish when the terminals change), `crates/moa-core/src/tmux/topology.rs`
  (the fourth refresh query, `T` lines, and the terminals it keeps),
  `crates/moa-core/src/op.rs` (`Op::TermShow { pane: String }`, `term.show`),
  `crates/moa-core/src/ops/term.rs` (`term_opened` updates the topology's terminals),
  `crates/moa-core/src/core.rs`, `crates/moa-view/src/data.rs` (`Data.terms`; `TOPICS` gains
  `Terms`), the `moa-proto` schema snapshot and `crates/moa-core/tests/skeleton.rs` (the new op)
- Test: `crates/moa-core/tests/term.rs` (or the existing terminal tests' file)

**Interfaces:**
- Produces:
  - `publish::TermRow { name: String }`, keyed by the pane id;
  - `Op::TermShow { pane: String }` (wire `term.show`), answered `went to <name>` or
    `that terminal is gone`;
  - `moa_view::data::Data.terms: BTreeMap<String, TermRow>`.

- [ ] **Step 1: Write the failing tests.**
  - `terminals_are_published_by_name`: a refresh whose terminal query answers
    `T %5 ⌨ kite work` (and whose `P` lines also list `%6`, untagged). The `terms` topic has
    one row, `{key: "%5", name: "kite work"}`.
  - `refresh_cmds_lists_terminals`: the refresh includes `list-panes -a -f '#{@moa_term}' -F
    'T #{pane_id} #{@moa_label}'`.
  - `opening_a_terminal_publishes_it_at_once`: `term_opened` for `%7` named `⌨ beacon` puts it
    in the topic before any listing.
  - `a_terminal_closing_leaves_the_topic`: `%5` gone at the next listing, so the topic is
    empty.
  - `term_show_selects_the_terminal`: `term.show {pane: "%5"}` → `Effect::Tmux` with
    `focus_cmds(%5)`, and the reply `went to kite`. Nothing is saved.
  - `term_show_of_a_non_terminal_says_gone`: `%6` → `that terminal is gone`.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S6b.**
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass.
- [ ] **Step 5: Commit:** `v1: terminals are listed as a topic, and term.show goes to one`.

## Task 7: the picker's view

**Files:**
- Create: `crates/moa-view/src/views/picker.rs`
- Modify: `crates/moa-view/src/views/mod.rs` (no placeholder for the picker),
  `crates/moa-view/src/line.rs` (`Tone::Hit`), `crates/moa-view/src/panel/mod.rs` (the picker's
  keys and query; `Out::Close`; `Panel::reset_pick`), `crates/moa-view/src/keys.rs`
  (`hint(View::Picker)`), `crates/moa-view/src/geometry.rs` (two head lines for the picker),
  the `Item` enum (`Item::Term { pane }`), `crates/moa-tui` (draw `Tone::Hit`; place the cursor
  after the query)
- Test: `crates/moa-view/tests/picker.rs`, the `moa-tui` snapshot tests

**Interfaces:**
- Consumes: `Data.terms` (Task 6), `Op::TermShow`.
- Produces:
  - `Out::Close`;
  - `Panel::reset_pick(&mut self)`;
  - `views::picker::{rows(data: &Data) -> Vec<PickRow>, rank(query: &str, hay: &str) -> Option<usize>}`.

- [ ] **Step 1: Write the failing tests** (fixture `lantern`, plus terminals `⌨ kite` and
  `⌨ harbor`):
  - `rows_follow_urgency_then_terminals_then_recency`: the order is waiting, question,
    failed, done, working, quiet, idle, then the two terminals, then asleep and ended by
    `last_seen_at`, newest first. No archived row.
  - `at_most_300_old_rows`: 400 ended sessions give 300 old rows.
  - `terms_match_as_subsequences_in_any_order`: `rank("kt wk", "kite work …")` is `Some(2)`
    (both non-contiguous); `rank("kite", …)` is `Some(0)`; `rank("zz", …)` is `None`.
  - `contiguous_matches_rank_first`: the query `kit` puts `kite` above `k-i-t` spread out.
  - `the_head_counts_shown_rows_and_live_ones`: the query `kite` → `2/14 · 7 live`, for this
    fixture's counts.
  - `typing_j_k_and_q_goes_into_the_query`, `ctrl_w_deletes_the_last_word`,
    `an_edit_selects_the_first_row`, `tab_and_ctrl_n_move_down`.
  - `enter_on_a_session_focuses_it`: `Out::Send(Op::SessionFocus { q: <id> })`, and the
    footer says `opening <title>…`. An `Ok` reply gives `Out::Close`.
  - `enter_on_a_terminal_shows_it`: `Out::Send(Op::TermShow { pane: "%5" })`.
  - `a_pick_that_fails_keeps_the_picker_open`: an `Err("its folder ~/x is gone")` reply
    leaves that text in the footer, gives no `Out::Close`, and keeps the query.
  - `esc_ctrl_c_and_ctrl_g_close`: each gives `Out::Close` and never the managed line.
  - `hits_are_highlighted_in_title_and_group_only`.
  - `picker_never_panics_or_overflows` (proptest): any rows (CJK, emoji, ZWJ, RTL, empty
    titles, 0 to 1,000 rows), any query, any width from 10 to 200 → no panic, and no line
    wider than the body.
  - The model snapshot at 80 columns; the `moa-tui` snapshots at 40, 60, 80, 120 and 200, 16
    rows high. The `Hit` cells are checked by cell assertions (yellow, bold, underlined).
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S7.** The query line and rule are fixed above the list. The list
  scrolls under them, keeping the selected row in view (lesson 20).
- [ ] **Step 4: Run** `nix develop --command cargo nextest run -p moa-view -p moa-tui` → all
  pass. Review the new snapshots.
- [ ] **Step 5: Commit:** `v1: the picker's view: urgency first, fuzzy match, highlights`.

## Task 8: the popup picker

**Files:**
- Modify: `crates/moa-core/src/config.rs` (`UiConfig.picker: Form`, `pub enum Form { Popup, Tab }`),
  `crates/moa-core/src/keymap.rs` (`ctrl_a(form)`, `Does::Popup`), `crates/moa-tmux/src/conf.rs`
  (`moa_tmux(brain, ui, launch)`, the popup binding), `crates/moa-daemon/src/run.rs` (passes
  them), `crates/moa/src/main.rs` (`pick`), `crates/moa/src/panel.rs` (`--once`),
  `crates/moa-core/src/config.rs` (`RESERVED_BRAIN_NAMES` gains `pick`),
  `crates/moa-core/src/cockpit/spawn.rs` (`pub fn panel_env_of`, used by `spawn_cmds`),
  `crates/moa-core/src/op.rs` (`ui.panel`'s `once`), `crates/moa-core/src/panel.rs` and
  `crates/moa-core/src/core.rs` (the registry keeps `once`; a `once` connection's
  `ClientGone` nudges on 3.4/3.5), `crates/moa-tui/src/run.rs` (`PanelOptions.once`: no pane
  claim, never managed, its own topics, exit on `Out::Close`), `crates/moa-view/src/geometry.rs`
  callers (`framed && !once`), `crates/moa-daemon/src/tmux_tests.rs` (`moa_tmux`'s new
  arguments)
- Test: `crates/moa-tmux/tests/conf.rs`, `crates/moa-core/tests/config.rs` (or where `[ui]` is
  tested), `crates/moa-core/tests/panel.rs` (or the registry's tests), `crates/moa/tests/panel_e2e.rs`

**Interfaces:**
- Consumes: `Out::Close` (Task 7), `PanelLaunch`.
- Produces:
  - `config::Form`;
  - `keymap::ctrl_a(form: Form)`;
  - `conf::moa_tmux(brain: &str, ui: &UiConfig, launch: &PanelLaunch) -> String`;
  - `cockpit::spawn::panel_env_of(brain: &str, launch: &PanelLaunch) -> Vec<String>`;
  - `PanelOptions.once: bool`; `Op::UiPanel`'s `once: bool`.

- [ ] **Step 1: Write the failing tests.**
  - `conf.rs`:
    - `the_picker_key_is_a_popup_by_default`: `/`'s line is the guarded `display-popup -E -w
      80% -h 70%` binding. It carries `-e MOA_BRAIN=default` and `-e MOA_NO_AUTOSTART=1` and
      the program's path, quoted. `s`'s line is the daemon key `find`.
    - `picker_tab_swaps_the_keys`: with `picker = "tab"`, `/` is the daemon key and `s` the
      popup.
    - The menu has `Find a session…` on `/` and `Find a session, full tab…` on `s` (popup
      default).
    - `a_path_with_spaces_quotes_and_hashes_survives`: a program path `/tmp/a b/it's/#1/moa`
      and a home `/tmp/h#2`. tmux 3.4 and 3.8 parse the generated file (`source-file` on a
      throwaway server). `list-keys -T moa /` shows the path intact. The menu item's command
      has every `#` doubled, and the popup's `-d` goes through `literal_format`.
    - `the_popup_env_is_the_panels_env`: the binding's `-e` flags equal
      `panel_env_of("default", &launch)`, which `spawn_cmds` also uses.
  - `config`: `picker = "drawer"` → `picker in [ui] must be "popup" or "tab"`; a missing
    `picker` → `Popup`.
  - `panel_e2e.rs`: `a_once_panel_claims_no_pane_and_exits_on_esc`. `moa panel picker --once`
    in a tab, with `TMUX_PANE` set:
    - no `panels` row names that pane;
    - Esc ends the process with status 0;
    - `panel.log` has one start line and one end line for `picker (popup)`.
  - `a_once_panel_with_moa_down_says_so_and_exits`: `moa panel picker --once` with
    `MOA_NO_AUTOSTART=1` and no daemon. The screen shows `moa isn't running`, and any key ends
    the process with status 0. It is not managed: it draws, and Ctrl-c doesn't show the
    managed line.
  - `once_refuses_other_views`: `moa panel tree --once` → `--once works only for the picker and
    help`, with a non-zero exit.
  - `a_once_connection_going_nudges_on_3_4` (core): a `ui.panel {once: true}` connection's
    `ClientGone` on link version 3.4 (and 3.5) sends one `run-shell true`. On 3.8 it sends
    nothing, and nor does a placed panel's `ClientGone`.
  - main.rs's reserved-names test passes with `pick`.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S8.** Review the new `moa.tmux` snapshots: only `/`, `s` and the
  menu's find items change.
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` and scratchpad
  `t34-all.sh` → all pass.
- [ ] **Step 5: Commit:** `v1: Ctrl-a / opens the picker as a popup`.

## Task 9: the full-tab picker

**Files:**
- Modify: `crates/moa-core/src/cockpit/run.rs` (`CockpitOp::{Sheet, Unsheet}` in the queue, and
  the same-pane replacement), `crates/moa-core/src/cockpit/run/decide.rs` (their plans),
  `crates/moa-core/src/tmux/topology.rs` (`window_sessions` as sets, `sessions_of`,
  `session_name_of` preferring `_moa`), `crates/moa-core/src/cockpit/scene.rs` (reads through
  them), `crates/moa-core/src/cockpit/assign.rs` (sheets are never candidates),
  `crates/moa-core/src/cockpit/spawn.rs` (a sheet's spawn is tagged with `panel_tags`; a dead
  sheet is respawned in place), `crates/moa-core/src/tmux/cmd.rs` (the three shapes),
  `crates/moa-core/src/keys.rs` (`Find`, `Help`), `crates/moa-core/src/tmux/mailbox.rs`
  (`FindOther` goes; `Find` and `Help` have no `not_yet`), `crates/moa-core/src/state.rs`
  (`sheets`), `crates/moa-core/src/panel.rs` and `crates/moa-core/src/publish.rs`
  (`PanelRow.sheet` from `sheets`, at publish and at registration), `crates/moa-core/src/op.rs`
  (`Op::UiClose`, `ui.close`), the link's `%session-window-changed` and `%client-detached`
  handling, the `moa-proto` schema snapshot and `crates/moa-core/tests/skeleton.rs` (the new
  op), `crates/moa-view/src/panel/mod.rs` and `crates/moa-tui/src/run.rs` (a placed sheet with
  `sheet: None` draws nothing; a new number calls `reset_pick`; `Out::Close` sends `ui.close`)
- Test: `crates/moa-core/src/cockpit/run/tests.rs`, `crates/moa-core/src/tmux/topology.rs` tests,
  `crates/moa-core/tests/tmux_vocab.rs`, `crates/moa-core/tests/sheet.rs`

**Interfaces:**
- Consumes: `spawn_cmds`, the queue (Task 1's files), `Panel::reset_pick`, `Out::Close`.
- Produces:
  - `CockpitOp::Sheet { view: View, session: TmuxSessionId, client: ClientName }`;
  - `CockpitOp::Unsheet { pane: PaneId }`;
  - `Op::UiClose` (`ui.close`);
  - `State.sheets: BTreeMap<PaneId, i64>`, `PanelRow.sheet: Option<i64>`;
  - `Topology::sessions_of(window: WindowId) -> &BTreeSet<TmuxSessionId>`.

- [ ] **Step 1: Write the failing tests.**
  - `tmux_vocab.rs`: `link-window -s %3 -t $0:`, `unlink-window -t $0:@3` and
    `select-window -t $0:@3` are allowed. These are refused:
    - `link-window -s picker -t moa:`;
    - `unlink-window -k -t $0:@3`;
    - `link-window -s %3 -t $0:5`.
  - `sheet.rs` (core, fake link):
    - `the_first_find_starts_the_picker_then_links_it`: spawn (tagged with `panel_tags`),
      listing, then `link-window -s %P -t $0:`. `State.sheets` has the pane, with the open's
      wall-clock milliseconds.
    - `a_panel_registering_after_the_link_reads_its_open`: then the spawned panel's
      `ui.panel`. Its row has `sheet == Some(<that number>)`.
    - `a_second_find_starts_nothing`: with the picker parked, one listing, then `link-window`.
      There are no spawn commands, and the number is new.
    - `a_dead_picker_is_restarted_in_place`: the picker's pane is dead, so `respawn-pane -k -t
      %P`, never a second `new-window`.
    - `find_while_linked_selects`: the picker's window already in `$0`, so `select-window -t
      $0:@W`, never a second link.
    - `ui_close_unlinks_from_every_user_session`: linked in `$0` and `$2`, so two
      `unlink-window`s, never one for `_moa`. The row's `sheet` becomes `None`.
    - `a_pick_unsheets_once`: `%session-window-changed $0 @1` and then `ui.close` for the
      same open. One `unlink-window` in all; the second `Unsheet`'s listing shows no link and
      sends nothing.
    - `repeated_finds_queue_one`: three `find` keys while a build runs leave one waiting
      `Sheet`.
    - `ui_close_from_another_panel_is_refused`: `only the picker and help close this way`.
    - `leaving_the_sheet_tab_unlinks_it`: `%session-window-changed $0 @1` while the picker's
      `@3` is linked in `$0` → `Unsheet`.
    - `detaching_unlinks_the_sheet`: `%client-detached` of the last user client on `$0`
      while the picker is open there → `Unsheet`. With another client still on `$0`,
      nothing.
    - `a_listing_unlinks_a_stale_sheet`: a fresh core whose first listing shows the picker in
      `moa` and `_moa` → `unlink-window -t $0:@3`.
    - `an_unlink_kills_nothing`: the `%window-close @3` that follows an unlink leaves the
      picker's registry entry, tags and `State.sheets` handling unchanged, and marks no pane
      gone.
    - `find_from_moas_hidden_session_says_so`: `open moa's tab first`.
  - `topology.rs`: `a_window_in_two_sessions_is_known_in_both`. `from_lists` with the same
    `%3 @3` listed under `$0 _moa` and then `$1 moa`. `sessions_of(@3)` is `{$0, $1}`, and
    `session_name_of` says `_moa`.
  - `run/tests.rs`:
    - `a_linked_sheet_is_left_alone`: the scene from that same duplicate listing (not a
      hand-built `Scene`), and a switch to `triage`. The chain names nothing of the picker's
      pane, and the build starts no panel because of it.
    - `sheets_are_never_reused_for_a_layout`: a layout needing `status`, with only a parked
      picker spare, spawns a `status` panel.
  - The panel (`moa-view`):
    - `a_closed_sheet_draws_nothing_and_never_animates`: a placed picker with `sheet: None`
      and a working session gives an empty screen and `animate == false`;
    - `a_picker_you_started_always_draws`: unmanaged, with `sheet: None`, it draws, and
      `Out::Close` quits it;
    - `a_new_sheet_number_clears_the_query`.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S9.**
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass. The 5b
  cockpit e2e (`cockpit_e2e.rs`) still passes.
- [ ] **Step 5: Commit:** `v1: Ctrl-a s opens the picker as a tab, linked and unlinked`.

## Task 10: help

**Files:**
- Create: `crates/moa-view/src/views/help.rs`
- Modify: `crates/moa-view/src/views/mod.rs`, `crates/moa-view/src/keys.rs` (help's keys and hint),
  `crates/moa-view/src/data.rs` (`TOPICS` gains `Layouts`), `crates/moa-core/src/publish.rs`
  (the `brain` row's `picker`), `crates/moa-core/src/panel.rs` (`label`),
  `crates/moa-core/src/keymap.rs` (`?`'s popup in the popup form), `crates/moa-tmux/src/conf.rs`
- Test: `crates/moa-view/tests/help.rs`, the `moa-tui` snapshots, `crates/moa-tmux/tests/conf.rs`

**Interfaces:**
- Consumes: `keymap::ctrl_a(form)`, `keys::hint`, the `layouts` rows, the sheet machinery
  (Task 9), `--once` (Task 8).
- Produces: `views::help::build(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line>`.

- [ ] **Step 1: Write the failing tests.**
  - `help_lists_every_table_key`: each `Binding` of `ctrl_a(Form::Popup)` appears as a line
    holding its key and its `help`.
  - `help_follows_the_picker_form`: with the `brain` row's `picker = "tab"`, the `/` line is
    the tab form's and `s`'s the popup's.
  - `help_says_hold_shift_to_select_text`.
  - `help_lists_the_layouts_with_the_current_one`: `▸ 1 cockpit: tree · console · plan`, with
    the current layout `cockpit`.
  - `help_has_no_tmux_words`: no line holds "pane" or "window" (lesson 35).
  - `q_esc_and_ctrl_c_close_help`: each gives `Out::Close`.
  - `the_help_key_is_a_popup_by_default`: `?`'s binding is the guarded, width-checked popup
    of S10 (`-w 100%` under 84 columns, `-w 84` otherwise, `-h 90%`), with `panel help
    --once`. The menu's `?` item is the same. With `picker = "tab"`, `?` is the daemon key
    `help`.
  - `help_names_the_prefix_plainly`: the prefix section's heading is `tmux's prefix (Ctrl-b
    unless you changed it)`.
  - The model snapshot at 80 columns; the `moa-tui` snapshots at 40–200.
- [ ] **Step 2: Run them** → they fail.
- [ ] **Step 3: Implement S10.**
- [ ] **Step 4: Run** `nix develop --command cargo nextest run --workspace` → all pass.
- [ ] **Step 5: Commit:** `v1: help is a view written from moa's own key table`.

## Task 11: 5c end to end

**Files:**
- Create: `crates/moa/tests/picker_e2e.rs`
- Modify: `crates/moa/tests/common/mod.rs` (only if a helper is missing: for example, reading
  a popup from the terminal's screen, since a popup isn't a pane)

Real `moa` daemons and panels, `fake-claude` agents, a `Terminal` (pty) attached. Run each
test 10 times under load on tmux 3.8 and 3.4 before the PR, as 5b's e2e were.

- [ ] **Step 1: Write the tests.**
  - `the_popup_picker_finds_and_focuses`: three sessions, `kite` asleep. Picking an asleep
    one on purpose: its resume opens a tab just as `moa pick` exits, the case Ubuntu's tmux
    3.4 can lose.
    1. `Ctrl-a /` shows the popup on the terminal's screen, titled `⌕ find a session`.
    2. Type `kit`, then Enter. The popup is gone, and `kite` is resumed into the agent spot.
    3. `panel.log` has exactly one more start line and one more end line for `picker
       (popup)`.
    4. A second open adds one more of each.
  - `the_full_tab_picker_starts_once_then_never`:
    1. `Ctrl-a s` starts one picker panel and shows it as a tab.
    2. Esc returns to the cockpit, and the session has no picker tab.
    3. `Ctrl-a s` again: `status.launches`, `panel_starts` and the picker's `pane_pid` are
       unchanged, and the query is empty.
    4. Type `harb`, then Enter: `harbor` is shown, and the picker tab is gone.
  - `leaving_the_picker_tab_closes_it`: `Ctrl-a s`, then `Ctrl-a ]`. Within the wait, moa's
    session has no picker tab.
  - `a_layout_switch_with_the_picker_open_keeps_it`: `Ctrl-a s`, `Ctrl-a 4`, then Esc.
    - The cockpit has `triage`'s leaves.
    - The picker panel is parked in `_moa` with the same `pane_pid`.
    - No pane died.
  - `help_shows_the_keys_in_both_forms`:
    - `Ctrl-a ?` shows the popup with `find a session` and `hold Shift to select text`, and
      `q` closes it;
    - with `picker = "tab"`, `Ctrl-a ?` shows help as a tab, and Esc closes it.
  - `help_opens_on_a_small_terminal`: an 80×24 terminal. `Ctrl-a ?` shows the popup (the
    screen holds `keys and help`), and `q` closes it.
  - `ctrl_a_n_opens_a_session`:
    1. `Ctrl-a n`: the status line shows `new session · folder: ~/private/`.
    2. Type `kite`, Enter. The second prompt `title:` appears. Type `it's "a" $HOME #1`,
       Enter.
    3. A session titled exactly that opens in `$HOME/private/kite`, made by the test
       beforehand.
    4. `Ctrl-a n` again, with folder `~/nope`: the status line shows `moa: no folder ~/nope`,
       and `~/nope` doesn't exist afterwards.
  - Each test is made to fail once on purpose, by breaking the rule it checks. The report says
    how.
- [ ] **Step 2: Run** each new test 10 times under load (the 5b recipe, scratchpad
  `hammer-par.sh`) on tmux 3.8, and then with tmux 3.4 first on `PATH` → 10/10 each.
- [ ] **Step 3: Run** `nix develop --command cargo nextest run --workspace` → all pass.
- [ ] **Step 4: Commit:** `v1: the picker, help and Ctrl-a n end to end`.

## Carried, and where each goes

| Item (from) | Where |
|---|---|
| The `cockpit/run.rs` split (5b final review, Recommendation 2) | Task 1 |
| The three "home → ~" helpers (p1 CR F6) | Task 2 |
| An unreadable listing's reply quoting tmux (5b final review #31) | Task 2 |
| `View::framed()` vs geometry's `EmptySlot` case (5b Task 9 M4) | Task 3 |
| The managed Ctrl-c line cut off in narrow panels (5b Task 12 M8) | Task 3 |
| A keymap table (5b carry list) | Task 4 |
| The `n` key, the first `command-prompt`, prompt quoting (4b R16, slice 3) | Task 5 |
| The `Find`, `FindOther`, `New` and `Help` keys' "not yet" (5b final review) | Tasks 5, 8–10 |
| Help's "hold Shift to select text" (5a V11) | Task 10 |
| Find the cockpit by a window option `@moa_cockpit` (5b final review #19) | **Slice 6** |
| True colour on tmux 3.4 for `xterm-256color` terminals; `panel.log` rotation (5b #43, #46) | **Slice 8** |
| The blocking fsync on the core (5b p2 CR F6) | **Slice 9** |
| `run-shell -b true`, so the nudge can't stall the link's queue (5b) | **Backlog** |
| A reconcile test that crosses every queued cockpit op (5b final review, Recommendation 4) | **Backlog**: 5c doesn't touch reconcile |
| No heal when the queue goes idle (5b #34); a refill judged at queue time (5b #35) | **Backlog** |
| Parked panels other than sheets animate while hidden. Sheets stop (S9); the layout panels would need moa to tell each one it's parked | **Backlog** |
